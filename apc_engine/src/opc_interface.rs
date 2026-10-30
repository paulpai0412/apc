use anyhow::{anyhow, ensure, Result};
use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, DateTime, NodeId, NumericRange, ReadValueId, StatusCode,
    TimestampsToReturn, Variant, WriteValue,
};

#[derive(Debug, Clone)]
pub struct NumericSample {
    pub value: f64,
    pub source_timestamp: Option<DateTime>,
    pub server_timestamp: Option<DateTime>,
}

fn check_read_results(expected: usize, results: &[DataValue]) -> Result<()> {
    ensure!(results.len() == expected,
        "OPC UA read returned {} items for {} requested nodes", results.len(), expected);
    for (index, value) in results.iter().enumerate() {
        // An omitted StatusCode encodes Good; Uncertain does not authorize control.
        if let Some(status) = value.status {
            ensure!(status.is_good(), "Non-Good status at read index {index}: {status}");
        }
    }
    Ok(())
}

async fn read_values(session: &Session, node_ids: &[NodeId]) -> Result<Vec<DataValue>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let requests: Vec<ReadValueId> = node_ids.iter().map(|id| ReadValueId {
        node_id: id.clone(),
        attribute_id: AttributeId::Value as u32,
        index_range: NumericRange::None,
        data_encoding: Default::default(),
    }).collect();
    let results = session.read(&requests, TimestampsToReturn::Both, 0.0).await
        .map_err(|error| anyhow!("OPC UA read network error: {error}"))?;
    check_read_results(node_ids.len(), &results)?;
    Ok(results)
}

fn numeric_value(node_id: &NodeId, value: &DataValue) -> Result<f64> {
    let number = match &value.value {
        Some(Variant::Double(value)) => *value,
        Some(Variant::Float(value)) => f64::from(*value),
        Some(Variant::SByte(value)) => f64::from(*value),
        Some(Variant::Byte(value)) => f64::from(*value),
        Some(Variant::Int16(value)) => f64::from(*value),
        Some(Variant::UInt16(value)) => f64::from(*value),
        Some(Variant::Int32(value)) => f64::from(*value),
        Some(Variant::UInt32(value)) => f64::from(*value),
        Some(Variant::Int64(value)) if (-(1_i64 << 53)..=(1_i64 << 53)).contains(value) => *value as f64,
        Some(Variant::UInt64(value)) if *value <= (1_u64 << 53) => *value as f64,
        _ => return Err(anyhow!("Node {node_id:?} does not contain a losslessly representable number: {:?}", value.value)),
    };
    ensure!(number.is_finite(), "Node {node_id:?} contains a nonfinite number");
    Ok(number)
}

/// Numeric values in request order. Invalid quality, shape and values fail the entire read.
pub async fn read_bulk(session: &Session, node_ids: &[NodeId]) -> Result<Vec<f64>> {
    let results = read_values(session, node_ids).await?;
    node_ids.iter().zip(&results).map(|(id, value)| numeric_value(id, value)).collect()
}

/// Retain both timestamp meanings; acquisition freshness is a caller policy, not value-change age.
pub async fn read_samples(session: &Session, node_ids: &[NodeId]) -> Result<Vec<NumericSample>> {
    let results = read_values(session, node_ids).await?;
    node_ids.iter().zip(results).map(|(id, value)| Ok(NumericSample {
        value: numeric_value(id, &value)?,
        source_timestamp: value.source_timestamp,
        server_timestamp: value.server_timestamp,
    })).collect()
}

/// Reuse the request layout and numeric storage across scans. OPC responses remain owned by
/// the client API. MaxAge=0 requests current values; it does NOT prove field acquisition.
#[derive(Default)]
pub struct NumericReadPlan {
    requests: Vec<ReadValueId>,
}

impl NumericReadPlan {
    pub fn add(&mut self, nodes: &[NodeId]) -> std::ops::Range<usize> {
        let start = self.requests.len();
        self.requests.extend(nodes.iter().map(|id| ReadValueId {
            node_id: id.clone(), attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None, data_encoding: Default::default(),
        }));
        start..self.requests.len()
    }

    pub fn len(&self) -> usize { self.requests.len() }

    pub async fn read_into(&self, session: &Session, values: &mut Vec<f64>) -> Result<()> {
        let results = session.read(&self.requests, TimestampsToReturn::Both, 0.0).await
            .map_err(|e| anyhow!("OPC UA scan read failed: {e}"))?;
        check_read_results(self.requests.len(), &results)?;
        values.clear();
        for (request, value) in self.requests.iter().zip(&results) {
            values.push(numeric_value(&request.node_id, value)?);
        }
        Ok(())
    }
}

fn value_write(node_id: &NodeId, value: Variant) -> WriteValue {
    WriteValue {
        node_id: node_id.clone(),
        attribute_id: AttributeId::Value as u32,
        index_range: NumericRange::None,
        value: DataValue::new_now(value),
    }
}

fn check_write_results(results: &[StatusCode], expected: usize) -> Result<(), StatusCode> {
    if results.len() != expected {
        return Err(StatusCode::BadUnexpectedError);
    }
    match results.iter().find(|code| !code.is_good()) {
        Some(code) => Err(*code),
        None => Ok(()),
    }
}

async fn write_values(session: &Session, requests: &[WriteValue]) -> Result<()> {
    if requests.is_empty() {
        return Ok(());
    }
    let results = session.write(requests).await
        .map_err(|error| anyhow!("OPC UA write network error; physical outcome is unconfirmed: {error}"))?;
    check_write_results(&results, requests.len()).map_err(|error| anyhow!(
        "OPC UA write incomplete or rejected: {error}; {} requests, statuses {results:?}; partial writes may already have occurred",
        requests.len()))
}

/// Server acceptance of every item, not atomicity or proof that a PID applied its command.
pub async fn write_bulk(session: &Session, node_ids: &[NodeId], values: &[f64]) -> Result<()> {
    ensure!(node_ids.len() == values.len(), "OPC UA write node/value counts differ");
    ensure!(values.iter().all(|value| value.is_finite()), "Cannot write nonfinite OPC UA values");
    let requests: Vec<WriteValue> = node_ids.iter().zip(values)
        .map(|(id, value)| value_write(id, Variant::Double(*value))).collect();
    write_values(session, &requests).await
}

pub async fn write_single(session: &Session, node_id: &NodeId, value: f64) -> Result<()> {
    ensure!(value.is_finite(), "Cannot write a nonfinite OPC UA value");
    write_values(session, &[value_write(node_id, Variant::Double(value))]).await
}

/// Lifecycle mode/setting values retain the commissioned server's numeric wire
/// type. This reads type evidence, not mode semantics; lossy coercion is rejected.
pub async fn write_numeric_bulk(session: &Session, node_ids: &[NodeId], values: &[f64]) -> Result<()> {
    write_numeric_bulk_at(session, node_ids, values, DateTime::now()).await
}

/// A source-owned acquisition uses one timestamp across all parts of its snapshot.
pub async fn write_numeric_bulk_at(session: &Session, node_ids: &[NodeId], values: &[f64], acquired_at: DateTime) -> Result<()> {
    ensure!(node_ids.len() == values.len(), "Numeric write dimensions differ");
    let current = read_values(session, node_ids).await?;
    let mut requests = Vec::with_capacity(values.len());
    for ((node, sample), value) in node_ids.iter().zip(&current).zip(values) {
        let encoded = match sample.value.as_ref() {
            Some(Variant::Double(_)) => Variant::Double(*value),
            Some(Variant::Float(_)) => Variant::Float(*value as f32),
            Some(Variant::Int32(_)) => Variant::Int32(*value as i32),
            Some(Variant::UInt32(_)) => Variant::UInt32(*value as u32),
            Some(Variant::Int16(_)) => Variant::Int16(*value as i16),
            Some(Variant::UInt16(_)) => Variant::UInt16(*value as u16),
            Some(Variant::SByte(_)) => Variant::SByte(*value as i8),
            Some(Variant::Byte(_)) => Variant::Byte(*value as u8),
            Some(Variant::Int64(_)) => Variant::Int64(*value as i64),
            Some(Variant::UInt64(_)) => Variant::UInt64(*value as u64),
            _ => return Err(anyhow!("Commissioned setting node is not numeric")),
        };
        let data = DataValue::new_now(encoded.clone());
        ensure!(numeric_value(node, &data)? == *value, "Lifecycle value is not losslessly representable by OPC node type");
        let mut write = value_write(node, encoded);
        write.value.source_timestamp = Some(acquired_at.clone());
        requests.push(write);
    }
    write_values(session, &requests).await
}

pub async fn write_int32(session: &Session, node_id: &NodeId, value: i32) -> Result<()> {
    write_values(session, &[value_write(node_id, Variant::Int32(value))]).await
}

pub async fn write_array(session: &Session, node_id: &NodeId, values: Vec<f64>) -> Result<(), StatusCode> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err(StatusCode::BadInvalidArgument);
    }
    let results = session.write(&[value_write(node_id, Variant::from(values))]).await
        .map_err(|_| StatusCode::BadUnexpectedError)?;
    check_write_results(&results, 1)
}

pub async fn read_string(session: &Session, node_id: &NodeId) -> Result<String> {
    let values = read_values(session, std::slice::from_ref(node_id)).await?;
    match &values[0].value {
        Some(Variant::String(value)) => Ok(value.to_string()),
        _ => Err(anyhow!("Node {node_id:?} does not contain a string")),
    }
}

pub async fn write_string(session: &Session, node_id: &NodeId, value: &str) -> Result<()> {
    write_values(session, &[value_write(node_id, Variant::String(value.into()))]).await
}

pub async fn write_string_array(session: &Session, node_id: &NodeId, values: Vec<String>) -> Result<()> {
    write_values(session, &[value_write(node_id, Variant::from(values))]).await
}

pub async fn read_bool(session: &Session, node_id: &NodeId) -> Result<bool> {
    let values = read_values(session, std::slice::from_ref(node_id)).await?;
    match &values[0].value {
        Some(Variant::Boolean(value)) => Ok(*value),
        _ => Err(anyhow!("Node {node_id:?} does not contain a Boolean")),
    }
}

pub async fn write_bool(session: &Session, node_id: &NodeId, value: bool) -> Result<()> {
    write_values(session, &[value_write(node_id, Variant::Boolean(value))]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_reads_and_uncertain_values_cannot_authorize_control() {
        let good = DataValue::new_now(Variant::Double(100.0));
        assert!(check_read_results(2, std::slice::from_ref(&good)).is_err());
        assert!(check_read_results(1, &[good.clone(), good.clone()]).is_err());
        for status in [StatusCode::Uncertain, StatusCode::BadNoCommunication] {
            let mut invalid = good.clone();
            invalid.status = Some(status);
            assert!(check_read_results(2, &[good.clone(), invalid]).is_err());
        }
        let mut implicit_good = good.clone();
        implicit_good.status = None;
        assert!(check_read_results(2, &[good, implicit_good]).is_ok());
    }

    #[test]
    fn numeric_decoding_preserves_units_and_rejects_nonfinite_or_lossy_values() {
        let node = NodeId::new(2, "Feed:SP");
        for (variant, expected) in [(Variant::Float(12.5), 12.5), (Variant::Int32(-8), -8.0),
                                    (Variant::UInt16(3), 3.0), (Variant::Double(103.25), 103.25)] {
            assert_eq!(numeric_value(&node, &DataValue::new_now(variant)).unwrap(), expected);
        }
        for variant in [Variant::Double(f64::NAN), Variant::Double(f64::INFINITY),
                        Variant::Float(f32::NEG_INFINITY), Variant::UInt64((1_u64 << 53) + 1),
                        Variant::Boolean(true), Variant::String("100".into())] {
            assert!(numeric_value(&node, &DataValue::new_now(variant)).is_err());
        }
    }

    #[test]
    fn write_responses_require_every_good_acknowledgement() {
        assert!(check_write_results(&[], 1).is_err());
        assert!(check_write_results(&[StatusCode::Good, StatusCode::Good], 1).is_err());
        assert_eq!(check_write_results(&[StatusCode::Good, StatusCode::BadUserAccessDenied], 2), Err(StatusCode::BadUserAccessDenied));
        assert_eq!(check_write_results(&[StatusCode::Uncertain], 1), Err(StatusCode::Uncertain));
        assert_eq!(check_write_results(&[StatusCode::Good, StatusCode::Good], 2), Ok(()));
    }
}
