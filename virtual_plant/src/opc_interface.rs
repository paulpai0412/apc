use anyhow::{Result, anyhow};
use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, NodeId, NumericRange, 
    ReadValueId, StatusCode, TimestampsToReturn, Variant, WriteValue
};

/// Reads a list of NodeIds in a single network call (Bulk Read).
/// Returns a Vector of f64 values in the same order as the input NodeIds.
pub async fn read_bulk(session: &Session, node_ids: &[NodeId]) -> Result<Vec<f64>> {
    // 1. Prepare the request
    let nodes_to_read: Vec<ReadValueId> = node_ids.iter()
        .map(|id| ReadValueId {
            node_id: id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            data_encoding: Default::default(),
        })
        .collect();

    // 2. Send Request
    let results = session.read(&nodes_to_read, TimestampsToReturn::Both, 0.0)
        .await
        .map_err(|e| anyhow!("OPC UA Read Network Error: {}", e))?;
    anyhow::ensure!(results.len() == node_ids.len(),
        "OPC UA read cardinality mismatch: requested {}, received {}", node_ids.len(), results.len());

    // 3. Unpack Results (Variant -> f64)
    let mut values = Vec::with_capacity(node_ids.len());
    for (i, val) in results.iter().enumerate() {
        // Check Status Code (Good vs Bad)
        if let Some(ref status) = val.status {
            if !status.is_good() {
                return Err(anyhow!("Non-Good Status on Node {:?}: {:?}", node_ids[i], status));
            }
        }
        
        // Extract number (Handle Double or Float)
        let float_val = match &val.value {
            Some(Variant::Double(v)) => *v,
            Some(Variant::Float(v)) => *v as f64,
            Some(Variant::Int32(v)) => *v as f64, // Added Int32 support just in case
            Some(Variant::UInt32(v)) => *v as f64,
            Some(Variant::Boolean(v)) => if *v { 1.0 } else { 0.0 },
            _ => return Err(anyhow!("Node {:?} is not a number. Got: {:?}", node_ids[i], val.value)),
        };
        anyhow::ensure!(float_val.is_finite(), "Non-finite value on node {:?}", node_ids[i]);
        values.push(float_val);
    }

    Ok(values)
}

/// A complete batch is accepted only when every requested item has a Good result.
pub async fn write_bulk(session: &Session, values: &[(&NodeId, Variant)]) -> Result<()> {
    for (node, value) in values {
        if let Variant::Double(number) = value {
            anyhow::ensure!(number.is_finite(), "Cannot publish non-finite process value on {:?}", node);
        }
    }
    let requests: Vec<WriteValue> = values.iter().map(|(node_id, value)| WriteValue {
        node_id: (*node_id).clone(),
        attribute_id: AttributeId::Value as u32,
        index_range: NumericRange::None,
        value: DataValue::new_now(value.clone()),
    }).collect();
    let results = session.write(&requests).await
        .map_err(|e| anyhow!("OPC UA Write Network Error (outcome unknown): {}", e))?;
    validate_write_results(values.len(), &results)
}

fn validate_write_results(expected: usize, results: &[StatusCode]) -> Result<()> {
    anyhow::ensure!(results.len() == expected,
        "OPC UA write cardinality mismatch: requested {}, received {}", expected, results.len());
    for (index, code) in results.iter().enumerate() {
        anyhow::ensure!(code.is_good(), "OPC UA write item {} was not Good: {}", index, code);
    }
    Ok(())
}

const MAX_EXACT_SEQUENCE: u64 = 1_u64 << 53;

/// Keeps a high-water mark across connections, without resetting server counters.
#[derive(Default)]
pub struct AcquisitionSequence {
    high_water: u64,
}

impl AcquisitionSequence {
    pub fn reserve(&mut self, observed: &[f64]) -> Result<(f64, f64)> {
        anyhow::ensure!(!observed.is_empty(), "No acquisition counters configured");
        let mut high_water = self.high_water;
        for &value in observed {
            anyhow::ensure!(value.is_finite() && value >= 0.0
                && value <= MAX_EXACT_SEQUENCE as f64 && value.fract() == 0.0,
                "AcquisitionSequence must be an exact nonnegative integer <= 2^53, got {}", value);
            high_water = high_water.max(value as u64);
        }
        let odd = if high_water % 2 == 0 { high_water + 1 } else { high_water + 2 };
        let even = odd + 1;
        // Leave an exact odd available to invalidate an uncertain commit.
        anyhow::ensure!(even < MAX_EXACT_SEQUENCE, "AcquisitionSequence exhausted; publication stopped");
        self.high_water = even;
        Ok((odd as f64, even as f64))
    }
}

pub async fn write_sequence(session: &Session, nodes: &[NodeId], value: f64) -> Result<()> {
    let values: Vec<_> = nodes.iter().map(|node| (node, Variant::Double(value))).collect();
    write_bulk(session, &values).await
}

/// Only called after the entire process frame has received checked Good results.
pub async fn commit_sequence(session: &Session, nodes: &[NodeId], even: f64) -> Result<()> {
    if let Err(error) = write_sequence(session, nodes, even).await {
        // A lost reply can mean the even write reached the server. Invalidate it
        // where possible, then stop: never retry/declare that uncertain frame good.
        let invalidation = write_sequence(session, nodes, even + 1.0).await;
        return Err(anyhow!("Acquisition commit failed: {}; odd invalidation: {:?}. \
            Publication stopped; if invalidation failed, server state is unknown", error, invalidation));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_good_results_are_required() {
        assert!(validate_write_results(2, &[StatusCode::Good, StatusCode::Good]).is_ok());
        assert!(validate_write_results(1, &[]).is_err());
        assert!(validate_write_results(1, &[StatusCode::Good, StatusCode::Good]).is_err());
        assert!(validate_write_results(2, &[StatusCode::Good, StatusCode::BadUnexpectedError]).is_err());
        assert!(validate_write_results(1, &[StatusCode::Uncertain]).is_err());
    }

    #[test]
    fn reservations_never_reuse_initial_or_abandoned_frames() {
        let mut sequence = AcquisitionSequence::default();
        assert_eq!(sequence.reserve(&[0.0, 0.0, 0.0]).unwrap(), (1.0, 2.0));
        // Restarted server/failed writes do not roll back this process's counter.
        assert_eq!(sequence.reserve(&[0.0, 1.0, 2.0]).unwrap(), (3.0, 4.0));
        assert_eq!(sequence.reserve(&[9.0, 4.0, 8.0]).unwrap(), (11.0, 12.0));
        // A new producer resumes beyond the last visible (even or odd) value.
        assert_eq!(AcquisitionSequence::default().reserve(&[12.0]).unwrap(), (13.0, 14.0));
        assert_eq!(AcquisitionSequence::default().reserve(&[13.0]).unwrap(), (15.0, 16.0));
    }

    #[test]
    fn invalid_and_exhausted_counters_fail_closed() {
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.5,
            MAX_EXACT_SEQUENCE as f64, (MAX_EXACT_SEQUENCE + 2) as f64] {
            assert!(AcquisitionSequence::default().reserve(&[value]).is_err());
        }
        assert!(AcquisitionSequence::default().reserve(&[]).is_err());
        let mut sequence = AcquisitionSequence::default();
        let (odd, even) = sequence.reserve(&[(MAX_EXACT_SEQUENCE - 4) as f64]).unwrap();
        assert_eq!(odd as u64 % 2, 1);
        assert_eq!(even as u64 % 2, 0);
        assert_eq!((even + 1.0) as u64, MAX_EXACT_SEQUENCE - 1);
        assert!(sequence.reserve(&[even]).is_err());
    }
}

