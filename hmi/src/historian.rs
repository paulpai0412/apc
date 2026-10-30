use anyhow::{bail, ensure, Context, Result};
use questdb::ingress::{Sender, Buffer, TimestampNanos, ProtocolVersion};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Duration};
use crate::opc_worker::{OpcUpdate, OpcCommand};
use crate::config::HistorianConfig;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone)]
enum LoggedValue {
    Number(f64),
    Bool(bool),
    Text(String),
    Null,
}

#[derive(Debug, Clone)]
struct LoggedSample {
    value: LoggedValue,
    status: u32,
    logged_at: Instant,
}

// Never evict an identity and accidentally accept an old snapshot as new evidence.
const MAX_SOURCE_IDENTITIES: usize = 16_384;

struct SourceSamples {
    timestamp: i64,
    value: serde_json::Value,
    status: u32,
    conflicted: bool,
    logged: Option<LoggedSample>,
}

#[derive(Default)]
struct Recorder {
    sources: HashMap<String, HashMap<String, SourceSamples>>,
    identity_count: usize,
}

impl Recorder {
    /// Only new source acquisitions enter deadband evaluation. ReadAll is not an acquisition.
    /// Equal-time mutations are retained as Bad evidence; older arrivals are rejected visibly.
    /// Memory is bounded by identities, not acquisition count; a full registry fails closed.
    fn record(&mut self, buffer: &mut Buffer, config: &HistorianConfig, update: &OpcUpdate, now: Instant) -> Result<bool> {
        if update.internal { return Ok(false); }
        let timestamp = source_timestamp(update)?;
        let previous = self.sources.get(&update.source_endpoint).and_then(|nodes| nodes.get(&update.node_id));
        let mut conflicted = false;
        if let Some(previous) = previous {
            ensure!(timestamp >= previous.timestamp, "Out-of-order OPC source timestamp rejected; no acquisition invented");
            if timestamp == previous.timestamp {
                if update.value == previous.value && update.status == previous.status {
                    return Ok(false);
                }
                conflicted = true;
            }
        } else {
            ensure!(self.identity_count < MAX_SOURCE_IDENTITIES, "Historian source identity limit reached");
        }
        let current = logged_value_from_json(&update.value);
        let should_log = conflicted || previous.is_some_and(|sample| sample.conflicted) || should_log_update(
            config, &update.node_id, &current, update.status,
            previous.and_then(|sample| sample.logged.as_ref()), now,
        );
        if should_log {
            write_to_buffer(buffer, &config.table_name, update, timestamp, !conflicted)?;
        }
        let new_identity = previous.is_none();
        if new_identity {
            self.sources.entry(update.source_endpoint.clone()).or_default().insert(update.node_id.clone(), SourceSamples {
                timestamp, value: serde_json::Value::Null, status: update.status,
                conflicted: false, logged: None,
            });
            self.identity_count += 1;
        }
        let source = self.sources.get_mut(&update.source_endpoint).unwrap().get_mut(&update.node_id).unwrap();
        source.timestamp = timestamp;
        source.value.clone_from(&update.value);
        source.status = update.status;
        source.conflicted = conflicted;
        if should_log {
            source.logged = Some(LoggedSample { value: current, status: update.status, logged_at: now });
        }
        Ok(should_log)
    }
}

fn get_field(node_id: &str) -> Option<&str> {
    node_id.split_once(':').map(|(_, field)| field)
}

fn logged_value_from_json(value: &serde_json::Value) -> LoggedValue {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                LoggedValue::Number(f)
            } else if let Some(i) = n.as_i64() {
                LoggedValue::Number(i as f64)
            } else {
                LoggedValue::Text(n.to_string())
            }
        }
        serde_json::Value::Bool(b) => LoggedValue::Bool(*b),
        serde_json::Value::String(s) => LoggedValue::Text(s.clone()),
        serde_json::Value::Null => LoggedValue::Null,
        _ => LoggedValue::Text(value.to_string()),
    }
}

fn deadband_settings_for_node(config: &HistorianConfig, node_id: &str) -> (f64, f64, u64) {
    let mut absolute = config.deadband.absolute_default.max(0.0);
    let mut relative_percent = config.deadband.relative_percent_default.max(0.0);
    let mut max_silence_sec = config.deadband.max_silence_sec;

    if let Some(field) = get_field(node_id) {
        if let Some(rule) = config.deadband.field_overrides.get(field) {
            if let Some(v) = rule.absolute {
                absolute = v.max(0.0);
            }
            if let Some(v) = rule.relative_percent {
                relative_percent = v.max(0.0);
            }
            if let Some(v) = rule.max_silence_sec {
                max_silence_sec = v;
            }
        }
    }

    if let Some(rule) = config.deadband.node_overrides.get(node_id) {
        if let Some(v) = rule.absolute {
            absolute = v.max(0.0);
        }
        if let Some(v) = rule.relative_percent {
            relative_percent = v.max(0.0);
        }
        if let Some(v) = rule.max_silence_sec {
            max_silence_sec = v;
        }
    }

    (absolute, relative_percent, max_silence_sec)
}

fn should_log_update(
    config: &HistorianConfig,
    node_id: &str,
    current: &LoggedValue,
    status: u32,
    previous: Option<&LoggedSample>,
    now: Instant,
) -> bool {
    if !config.deadband.enabled {
        return true;
    }

    let Some(previous) = previous else {
        return true;
    };
    if status != previous.status {
        return true;
    }

    let (absolute, relative_percent, max_silence_sec) = deadband_settings_for_node(config, node_id);
    if max_silence_sec > 0 && now.duration_since(previous.logged_at).as_secs() >= max_silence_sec {
        return true;
    }

    match (&previous.value, current) {
        (LoggedValue::Number(prev), LoggedValue::Number(curr)) => {
            let delta = (curr - prev).abs();
            let scale = prev.abs().max(curr.abs());
            let relative_threshold = (relative_percent / 100.0) * scale;
            let threshold = absolute.max(relative_threshold);
            delta >= threshold
        }
        (LoggedValue::Bool(prev), LoggedValue::Bool(curr)) => prev != curr,
        (LoggedValue::Text(prev), LoggedValue::Text(curr)) => prev != curr,
        (LoggedValue::Null, LoggedValue::Null) => false,
        _ => true,
    }
}

/// QuestDB Historian - Continuous Time-Series Recorder
/// 
/// Subscribes to OPC UA updates and writes to QuestDB using ILP (InfluxDB Line Protocol).
/// Uses batching for performance - flushes every N records or every X milliseconds.
/// Periodic reads recover missed new samples, never fabricate fresh constant-value acquisitions.
pub async fn run_historian(
    config: HistorianConfig,
    mut rx: broadcast::Receiver<OpcUpdate>,
    cmd_tx: mpsc::Sender<OpcCommand>,
) -> Result<()> {
    
    if !config.enabled {
        println!("📊 Historian: Disabled in config");
        return Ok(());
    }

    println!("📊 Historian: Connecting to QuestDB at {}:{}", config.host, config.ilp_port);

    // Connect to QuestDB ILP endpoint
    let mut sender = Sender::from_conf(&format!("tcp::addr={}:{};", config.host, config.ilp_port))?;
    let mut buffer = Buffer::new(ProtocolVersion::V1);
    let mut recorder = Recorder::default();
    
    let mut batch_count = 0;
    let mut flush_timer = interval(Duration::from_millis(config.flush_interval_ms));
    flush_timer.tick().await; // First tick completes immediately

    let mut snapshot_timer = interval(Duration::from_secs(config.snapshot_interval_sec));
    snapshot_timer.tick().await; // First tick completes immediately

    println!("✅ Historian: Connected. Recording to table '{}'", config.table_name);
    println!("📸 Historian: Snapshot mode enabled (every {}s)", config.snapshot_interval_sec);

    loop {
        tokio::select! {
            // Receive OPC UA updates
            Ok(update) = rx.recv() => {
                match recorder.record(&mut buffer, &config, &update, Instant::now()) {
                    Ok(true) => {
                        batch_count += 1;
                        
                        // Flush if batch size reached
                        if batch_count >= config.batch_size {
                            if let Err(e) = sender.flush(&mut buffer) {
                                eprintln!("❌ Historian flush error: {}", e);
                            }
                            buffer.clear();
                            batch_count = 0;
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("⚠️ Historian write error for {}: {}", update.node_id, e);
                    }
                }
            }
            
            // Periodic flush timer
            _ = flush_timer.tick() => {
                if batch_count > 0 {
                    if let Err(e) = sender.flush(&mut buffer) {
                        eprintln!("❌ Historian flush error: {}", e);
                    }
                    buffer.clear();
                    batch_count = 0;
                }
            }

            // Periodic snapshot timer - request all nodes to be read and logged
            _ = snapshot_timer.tick() => {
                if let Err(e) = cmd_tx.send(OpcCommand::ReadAll).await {
                    eprintln!("⚠️ Historian: Failed to request snapshot: {}", e);
                }
            }
            
            else => {
                println!("📊 Historian: Channel closed, shutting down");
                break;
            }
        }
    }

    // Final flush on shutdown
    if batch_count > 0 {
        let _ = sender.flush(&mut buffer);
    }

    Ok(())
}

/// Source clocks must be explicit, RFC3339 and representable by QuestDB ILP.
/// OPC's unset 1601 epoch and pre-Unix values are not acquisition evidence.
fn source_timestamp(update: &OpcUpdate) -> Result<i64> {
    ensure!(!update.source_endpoint.trim().is_empty(), "Missing OPC source endpoint");
    let timestamp = DateTime::parse_from_rfc3339(&update.timestamp)
        .context("Invalid or missing OPC source timestamp")?
        .timestamp_nanos_opt().context("OPC source timestamp outside ILP nanosecond range")?;
    ensure!(timestamp > 0, "OPC source timestamp must be after the Unix epoch");
    Ok(timestamp)
}

/// Writes an OPC UA update to QuestDB buffer using ILP format
fn write_to_buffer(buffer: &mut Buffer, table_name: &str, update: &OpcUpdate, timestamp: i64, evidence_valid: bool) -> Result<()> {
    // Endpoint and physical identifier together identify a source; neither is inferred.
    buffer.table(table_name)?.symbol("node_id", &update.node_id)?
        .symbol("source_endpoint", &update.source_endpoint)?;

    // Write value based on type
    match &update.value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                buffer.column_f64("value", f)?;
            } else if let Some(i) = n.as_i64() {
                buffer.column_i64("value", i)?;
            }
        }
        serde_json::Value::Bool(b) => {
            buffer.column_f64("value", if *b { 1.0 } else { 0.0 })?;
        }
        serde_json::Value::String(s) => {
            buffer.column_str("value_str", s)?;
        }
        serde_json::Value::Array(arr) => {
            // Store arrays as JSON string for now
            buffer.column_str("value_array", &serde_json::to_string(arr)?)?;
        }
        _ => {
            buffer.column_str("value_str", "null")?;
        }
    }

    // Preserve original quality; timestamp collisions are never eligible Good evidence.
    buffer.column_i64("status", if evidence_valid { update.status } else { update.status | 0x8000_0000 } as i64)?;
    buffer.column_i64("source_status", update.status as i64)?;
    buffer.column_bool("evidence_valid", evidence_valid)?;
    buffer.at(TimestampNanos::new(timestamp))?;

    Ok(())
}

/// Time-series data point for step response analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesPoint {
    pub timestamp: String,  // ISO 8601 format
    pub value: f64,
}

/// Historical data for a single node/tag
#[derive(Debug, Serialize, Deserialize)]
pub struct NodeHistory {
    pub node_id: String,
    pub data: Vec<TimeSeriesPoint>,
}

#[derive(Debug, Clone, Default)]
pub struct HistoricalQueryOptions {
    pub bucket_ms: Option<u64>,
}


fn build_raw_query(
    table_name: &str,
    node_id: &str,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
) -> String {
    format!(
        "SELECT timestamp, value, status, source_endpoint, evidence_valid FROM {} WHERE node_id = '{}' AND timestamp >= '{}' AND timestamp < '{}' ORDER BY timestamp LIMIT 500001",
        table_name,
        node_id.replace('\'', "''"),
        start_time.to_rfc3339(),
        end_time.to_rfc3339()
    )
}

/// Validate the complete raw response before averaging can hide ambiguity or bad quality.
/// One unscoped multi-signal request must have exactly one proven physical endpoint.
fn decode_history_rows(rows: &[serde_json::Value], endpoint: &mut Option<String>, bucket_ms: Option<u64>) -> Result<Vec<TimeSeriesPoint>> {
    ensure!(rows.len() <= 500_000, "Selected history exceeds 500000 raw samples per signal; narrow the time window");
    let bucket_ms = bucket_ms.filter(|value| *value > 0)
        .map(|value| value.div_ceil(1000).checked_mul(1000).and_then(|value| i64::try_from(value).ok())
            .context("Historian bucket interval is out of range")).transpose()?;
    let mut data = Vec::with_capacity(rows.len());
    let mut previous = None;
    let mut bucket: Option<(i64, f64, usize)> = None;
    for row in rows {
        let values = row.as_array().context("Invalid historian row")?;
        let timestamp = values.first().and_then(serde_json::Value::as_str).context("Historian row has no timestamp")?;
        let time = DateTime::parse_from_rfc3339(timestamp).context("Invalid historian source timestamp")?;
        let nanos = time.timestamp_nanos_opt().filter(|value| *value > 0).context("Invalid historian source clock range")?;
        ensure!(previous.is_none_or(|previous| nanos > previous), "Duplicate or unordered historian source time; evidence is ambiguous");
        previous = Some(nanos);
        let source = values.get(3).and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty()).context("Historian row lacks source endpoint provenance")?;
        if let Some(endpoint) = endpoint.as_ref() {
            ensure!(endpoint == source, "Mixed OPC endpoints cannot form one historian evidence window");
        } else {
            *endpoint = Some(source.to_owned());
        }
        ensure!(values.get(4).and_then(serde_json::Value::as_bool) == Some(true), "Historian source-time conflict or missing evidence validity");
        let status = values.get(2).and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()).context("Historian row lacks valid source quality")?;
        ensure!(status & 0xc000_0000 == 0, "Historian evidence contains non-Good source quality");
        let value = values.get(1).and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite()).context("Historian evidence contains missing or nonfinite numeric value")?;
        if let Some(interval) = bucket_ms {
            let start = time.timestamp_millis().div_euclid(interval) * interval;
            if bucket.is_some_and(|(previous, _, _)| previous != start) {
                let (start, mean, _) = bucket.take().unwrap();
                data.push(TimeSeriesPoint { timestamp: DateTime::<Utc>::from_timestamp_millis(start).context("Invalid bucket timestamp")?.to_rfc3339(), value: mean });
            }
            let (_, mean, count) = bucket.get_or_insert((start, 0.0, 0));
            *count += 1;
            *mean = *mean * ((*count - 1) as f64 / *count as f64) + value / *count as f64;
        } else {
            data.push(TimeSeriesPoint { timestamp: timestamp.to_owned(), value });
        }
    }
    if let Some((start, mean, _)) = bucket {
        data.push(TimeSeriesPoint { timestamp: DateTime::<Utc>::from_timestamp_millis(start).context("Invalid bucket timestamp")?.to_rfc3339(), value: mean });
    }
    Ok(data)
}


/// Query historical data from QuestDB for step response analysis
/// 
/// Fetches time-series data for specified nodes within a time range.
/// Uses QuestDB's REST API for querying.
pub async fn query_historical_data(
    config: &HistorianConfig,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    node_ids: Vec<String>,
    options: HistoricalQueryOptions,
) -> Result<Vec<NodeHistory>> {
    ensure!(start_time < end_time, "End must be later than start");
    ensure!(!node_ids.is_empty() && node_ids.len() <= 256, "Select between 1 and 256 signals");
    ensure!(node_ids.iter().all(|node| !node.is_empty() && node.len() <= 512), "Invalid signal identifier");
    if !config.enabled {
        return Ok(vec![]);
    }
    ensure!(!config.table_name.is_empty() && config.table_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "Invalid historian table identifier");
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let url = format!("http://{}:{}/exec", config.host, config.rest_port);
    let mut results = Vec::with_capacity(node_ids.len());
    let mut endpoint = None;
    for node_id in node_ids {
        let query = build_raw_query(&config.table_name, &node_id, start_time, end_time);
        let response = client.get(&url).query(&[("query", query)]).send().await?
            .error_for_status().context("Historian query failed; exact node_id storage is required")?;
        let json: serde_json::Value = response.json().await?;
        if let Some(error) = json.get("error") {
            bail!("Historian query rejected: {error}");
        }
        let rows = json.get("dataset").and_then(serde_json::Value::as_array)
            .context("Historian response has no dataset")?;
        let data = decode_history_rows(rows, &mut endpoint, options.bucket_ms)?;
        results.push(NodeHistory { node_id, data });
    }
    Ok(results)
}

/// Get exact physical node identifiers recorded by the historian.
pub async fn get_available_tags(config: &HistorianConfig) -> Result<Vec<String>> {
    if !config.enabled {
        return Ok(vec![]);
    }
    ensure!(!config.table_name.is_empty() && config.table_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "Invalid historian table identifier");

    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let query = format!(
        "SELECT DISTINCT node_id FROM {} WHERE node_id IS NOT NULL ORDER BY node_id",
        config.table_name
    );

    let url = format!("http://{}:{}/exec", config.host, config.rest_port);
    let response = client
        .get(&url)
        .query(&[("query", &query)])
        .send()
        .await?.error_for_status().context("Historian catalog requires exact node_id storage")?;

    let json: serde_json::Value = response.json().await?;
    if let Some(error) = json.get("error") {
        bail!("Historian catalog rejected: {error}");
    }
    let rows = json.get("dataset").and_then(serde_json::Value::as_array)
        .context("Historian catalog has no dataset")?;
    let tags = rows.iter().map(|row| {
        row.get(0).and_then(serde_json::Value::as_str).map(str::to_owned)
            .context("Historian catalog row has no node identifier")
    }).collect::<Result<Vec<_>>>()?;

    Ok(tags)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> HistorianConfig {
        let settings: crate::config::AppSettings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        let mut config = settings.historian;
        config.deadband.enabled = false;
        config
    }

    fn update(node: &str, timestamp: &str) -> OpcUpdate {
        OpcUpdate {
            node_id: node.to_owned(), value: serde_json::json!(42.5),
            timestamp: timestamp.to_owned(), status: 0,
            source_endpoint: "opc.tcp://plant-a:4840".to_owned(), internal: false,
        }
    }

    #[test]
    fn serialized_timestamp_is_source_time_not_receive_time() {
        let mut buffer = Buffer::new(ProtocolVersion::V1);
        Recorder::default().record(&mut buffer, &config(), &update("CV:PV", "2026-01-01T00:00:00.123456789Z"), Instant::now()).unwrap();
        let line = std::str::from_utf8(buffer.as_bytes()).unwrap();
        assert!(line.ends_with(" 1767225600123456789\n"), "{line}");
        assert!(line.contains("source_endpoint=opc.tcp://plant-a:4840"), "{line}");
    }

    #[test]
    fn invalid_source_clocks_never_serialize_as_current_good_samples() {
        for timestamp in ["", "NaN", "Infinity", "not-a-clock", "9999-01-01T00:00:00Z", "1601-01-01T00:00:00Z"] {
            let mut buffer = Buffer::new(ProtocolVersion::V1);
            assert!(Recorder::default().record(&mut buffer, &config(), &update("CV:PV", timestamp), Instant::now()).is_err(), "{timestamp}");
            assert!(buffer.as_bytes().is_empty());
        }
    }

    #[test]
    fn distinct_physical_nodes_never_alias_one_historian_series() {
        let series = ["PV", "unknown:PV"].map(|node_id| {
            let mut buffer = Buffer::new(ProtocolVersion::V1);
            Recorder::default().record(&mut buffer, &config(), &update(node_id, "2026-01-01T00:00:00Z"), Instant::now()).unwrap();
            buffer.as_bytes().split(|byte| *byte == b' ').next().unwrap().to_vec()
        });
        assert_ne!(series[0], series[1], "Different OPC nodes must remain independently queryable");
    }

    #[test]
    fn repeated_snapshot_never_creates_a_new_acquisition_even_after_max_silence() {
        let mut recorder = Recorder::default();
        let mut buffer = Buffer::new(ProtocolVersion::V1);
        let mut config = config();
        config.deadband.enabled = true;
        config.deadband.max_silence_sec = 1;
        let now = Instant::now();
        let sample = update("CV:PV", "2026-01-01T00:00:00Z");
        assert!(recorder.record(&mut buffer, &config, &sample, now).unwrap());
        let original = buffer.as_bytes().to_vec();
        assert!(!recorder.record(&mut buffer, &config, &sample, now + Duration::from_secs(60)).unwrap());
        assert_eq!(buffer.as_bytes(), original.as_slice());
    }

    #[test]
    fn same_value_new_source_sample_obeys_deadband_without_forward_fill() {
        let now = Instant::now();
        for enabled in [false, true] {
            let mut config = config();
            config.deadband.enabled = enabled;
            config.deadband.absolute_default = 1.0;
            config.deadband.relative_percent_default = 0.0;
            config.deadband.max_silence_sec = 10;
            config.deadband.node_overrides.clear();
            config.deadband.field_overrides.clear();
            let mut recorder = Recorder::default();
            let mut buffer = Buffer::new(ProtocolVersion::V1);
            assert!(recorder.record(&mut buffer, &config, &update("CV:PV", "2026-01-01T00:00:00Z"), now).unwrap());
            assert_eq!(recorder.record(&mut buffer, &config, &update("CV:PV", "2026-01-01T00:00:01Z"), now + Duration::from_secs(1)).unwrap(), !enabled);
            assert!(recorder.record(&mut buffer, &config, &update("CV:PV", "2026-01-01T00:00:10Z"), now + Duration::from_secs(11)).unwrap());
            let lines = std::str::from_utf8(buffer.as_bytes()).unwrap();
            assert_eq!(lines.lines().count(), if enabled { 2 } else { 3 });
            assert!(lines.ends_with(" 1767225610000000000\n"));
        }
    }

    #[test]
    fn equal_source_time_value_or_quality_changes_poison_evidence_and_recovery_is_new() {
        for quality_change in [false, true] {
            let mut recorder = Recorder::default();
            let mut buffer = Buffer::new(ProtocolVersion::V1);
            let mut config = config();
            config.deadband.enabled = true;
            config.deadband.absolute_default = 100.0;
            config.deadband.max_silence_sec = 0;
            let now = Instant::now();
            let mut sample = update("CV:PV", "2026-01-01T00:00:00Z");
            assert!(recorder.record(&mut buffer, &config, &sample, now).unwrap());
            if quality_change { sample.status = 0x8000_0000; } else { sample.value = serde_json::json!(42.6); }
            assert!(recorder.record(&mut buffer, &config, &sample, now).unwrap());
            let lines = std::str::from_utf8(buffer.as_bytes()).unwrap();
            let collision = lines.lines().nth(1).unwrap();
            assert!(collision.contains("evidence_valid=f"), "{collision}");
            assert!(collision.contains("status=2147483648i"), "{collision}");
            assert!(collision.ends_with(" 1767225600000000000"), "{collision}");
            assert!(!recorder.record(&mut buffer, &config, &sample, now).unwrap());
            sample.timestamp = "2026-01-01T00:00:01Z".to_owned();
            sample.status = 0;
            assert!(recorder.record(&mut buffer, &config, &sample, now).unwrap());
            assert_eq!(std::str::from_utf8(buffer.as_bytes()).unwrap().lines().count(), 3);
        }
    }

    #[test]
    fn endpoint_is_part_of_serialized_identity_and_missing_endpoint_is_rejected() {
        let mut recorder = Recorder::default();
        let mut buffer = Buffer::new(ProtocolVersion::V1);
        let now = Instant::now();
        let mut sample = update("CV:PV", "2026-01-01T00:00:00Z");
        assert!(recorder.record(&mut buffer, &config(), &sample, now).unwrap());
        sample.source_endpoint = "opc.tcp://plant-b:4840".to_owned();
        assert!(recorder.record(&mut buffer, &config(), &sample, now).unwrap());
        let lines: Vec<_> = std::str::from_utf8(buffer.as_bytes()).unwrap().lines().collect();
        assert_ne!(lines[0].split(' ').next(), lines[1].split(' ').next());
        sample.source_endpoint.clear();
        assert!(recorder.record(&mut buffer, &config(), &sample, now).is_err());
        assert_eq!(std::str::from_utf8(buffer.as_bytes()).unwrap().lines().count(), 2);
    }

    #[test]
    fn history_rejects_mixed_sources_legacy_quality_and_collisions_before_averaging() {
        let good = serde_json::json!(["2026-01-01T00:00:00Z", 42.5, 0, "opc.tcp://plant-a:4840", true]);
        let next = serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 0, "opc.tcp://plant-a:4840", true]);
        let averaged = decode_history_rows(&[good.clone(), next.clone()], &mut None, Some(2000)).unwrap();
        assert_eq!(averaged.len(), 1);
        assert_eq!(averaged[0].value, 43.5);
        for invalid in [
            serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 0, "opc.tcp://plant-b:4840", true]),
            serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 0, null, true]),
            serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 0, "opc.tcp://plant-a:4840", null]),
            serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 0, "opc.tcp://plant-a:4840", false]),
            serde_json::json!(["2026-01-01T00:00:01Z", 44.5, 2147483648u32, "opc.tcp://plant-a:4840", true]),
            serde_json::json!(["2026-01-01T00:00:00Z", 44.5, 2147483648u32, "opc.tcp://plant-a:4840", false]),
            good.clone(),
        ] {
            assert!(decode_history_rows(&[good.clone(), invalid], &mut None, Some(2000)).is_err());
        }
        let mut endpoint = None;
        decode_history_rows(&[good], &mut endpoint, None).unwrap();
        let other_signal = serde_json::json!(["2026-01-01T00:00:00Z", 42.5, 0, "opc.tcp://plant-b:4840", true]);
        assert!(decode_history_rows(&[other_signal], &mut endpoint, None).is_err());
    }

    #[test]
    fn older_source_arrivals_are_rejected_without_relabeling_or_duplicate_rows() {
        let mut recorder = Recorder::default();
        let mut buffer = Buffer::new(ProtocolVersion::V1);
        let config = config();
        let now = Instant::now();
        let original = update("CV:PV", "2026-01-01T00:00:00Z");
        assert!(recorder.record(&mut buffer, &config, &original, now).unwrap());
        assert!(recorder.record(&mut buffer, &config, &update("CV:PV", "2026-01-01T00:00:01Z"), now).unwrap());
        let recorded = buffer.as_bytes().to_vec();
        assert!(recorder.record(&mut buffer, &config, &original, now).is_err());
        assert_eq!(buffer.as_bytes(), recorded.as_slice());
    }

    #[test]
    fn full_identity_registry_refuses_new_sources_without_forgetting_existing_samples() {
        let mut recorder = Recorder::default();
        let mut buffer = Buffer::new(ProtocolVersion::V1);
        let config = config();
        let now = Instant::now();
        for index in 0..MAX_SOURCE_IDENTITIES {
            assert!(recorder.record(&mut buffer, &config, &update(&format!("CV{index}:PV"), "2026-01-01T00:00:00Z"), now).unwrap());
            buffer.clear();
        }
        assert!(recorder.record(&mut buffer, &config, &update("overflow:PV", "2026-01-01T00:00:00Z"), now).is_err());
        assert!(!recorder.record(&mut buffer, &config, &update("CV0:PV", "2026-01-01T00:00:00Z"), now).unwrap());
        assert!(buffer.as_bytes().is_empty());
        assert!(recorder.record(&mut buffer, &config, &update("CV0:PV", "2026-01-01T00:00:01Z"), now).unwrap());
        assert_eq!(std::str::from_utf8(buffer.as_bytes()).unwrap().lines().count(), 1);
    }

    #[test]
    fn deadband_keeps_quality_failure_and_recovery_without_numeric_change() {
        let settings: crate::config::AppSettings = toml::from_str(include_str!("../config/settings.toml")).unwrap();
        let mut config = settings.historian;
        config.deadband.enabled = true;
        config.deadband.absolute_default = 10.0;
        let now = Instant::now();
        let value = LoggedValue::Number(50.0);
        let mut recorded = LoggedSample { value: value.clone(), status: 0, logged_at: now };
        assert!(!should_log_update(&config, "CV:PV", &value, 0, Some(&recorded), now));
        assert!(should_log_update(&config, "CV:PV", &value, 0x8000_0000, Some(&recorded), now));
        recorded.status = 0x8000_0000;
        assert!(should_log_update(&config, "CV:PV", &value, 0, Some(&recorded), now));
    }
}
