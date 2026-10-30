//! Read-only chart history. Unlike identification averages, display reduction preserves extrema
//! and quality/setting transitions, and never invents measurements between source timestamps.
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeSet;
use crate::config::HistorianConfig;

#[derive(Debug, Serialize)]
pub struct TrendPoint<'a> {
    pub timestamp: String,
    pub node_id: &'a str,
    pub value: Option<f64>,
    pub status: Option<u32>,
    pub evidence_valid: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendHistory<'a> {
    pub points: Vec<TrendPoint<'a>>,
    pub raw_samples: usize,
    pub gap_threshold_ms: u64,
    pub warnings: Vec<String>,
    pub source_endpoint: Option<String>,
}

fn quote(value: &str) -> String { value.replace('\'', "''") }

fn quality(status: Option<u32>) -> Option<u32> { status.map(|value| value & 0xc000_0000) }

fn retain_points<'a>(points: Vec<TrendPoint<'a>>, budget: Option<usize>, preserve_changes: bool) -> Vec<TrendPoint<'a>> {
    let Some(budget) = budget.filter(|n| *n >= 4) else { return points; };
    if points.len() <= budget { return points; }
    let mut keep = BTreeSet::from([0, points.len() - 1]);
    // Each query covers one exact physical node and one engineering quantity.
    let buckets = (budget / 4).max(1);
    for bucket in 0..buckets {
        let first = bucket * points.len() / buckets;
        let last = (bucket + 1) * points.len() / buckets;
        if first == last { continue; }
        keep.insert(first); keep.insert(last - 1);
        let mut minimum: Option<(usize, f64)> = None;
        let mut maximum: Option<(usize, f64)> = None;
        for (index, point) in points.iter().enumerate().take(last).skip(first) {
            if let Some(value) = point.value {
                if minimum.map_or(true, |(_, v)| value < v) { minimum = Some((index, value)); }
                if maximum.map_or(true, |(_, v)| value > v) { maximum = Some((index, value)); }
            }
        }
        if let Some((index, _)) = minimum { keep.insert(index); }
        if let Some((index, _)) = maximum { keep.insert(index); }
    }
    for i in 1..points.len() {
        let previous = &points[i - 1]; let current = &points[i];
        if previous.value.is_none() != current.value.is_none() || quality(previous.status) != quality(current.status)
            || (preserve_changes && previous.value != current.value) {
            keep.insert(i - 1); keep.insert(i);
        }
    }
    points.into_iter().enumerate().filter_map(|(index, point)| keep.contains(&index).then_some(point)).collect()
}

pub async fn query<'a>(
    config: &HistorianConfig, start: DateTime<Utc>, end: DateTime<Utc>,
    nodes: &'a [String], setting_nodes: &[String], max_points: Option<usize>,
) -> Result<TrendHistory<'a>> {
    query_internal(config,start,end,nodes,setting_nodes,max_points,None).await
}

pub async fn query_scoped<'a>(
    config:&HistorianConfig,start:DateTime<Utc>,end:DateTime<Utc>,
    nodes:&'a [String],setting_nodes:&[String],max_points:Option<usize>,endpoint:&str,
) -> Result<TrendHistory<'a>> {
    ensure!(!endpoint.is_empty()&&endpoint.len()<=2048,"Invalid commissioned source endpoint");
    query_internal(config,start,end,nodes,setting_nodes,max_points,Some(endpoint)).await
}

async fn query_internal<'a>(
    config:&HistorianConfig,start:DateTime<Utc>,end:DateTime<Utc>,
    nodes:&'a [String],setting_nodes:&[String],max_points:Option<usize>,endpoint:Option<&str>,
) -> Result<TrendHistory<'a>> {
    ensure!(start < end, "End must be later than start");
    ensure!(!nodes.is_empty() && nodes.len() <= 256, "Select between 1 and 256 signals");
    ensure!(nodes.iter().all(|node| !node.is_empty() && node.len() <= 512), "Invalid signal identifier");
    ensure!(setting_nodes.iter().all(|node| nodes.contains(node)), "Setting signals must be included in nodes");
    ensure!(max_points.map_or(true, |n| (4..=10000).contains(&n)), "max_points must be between 4 and 10000");
    let silence = config.deadband.node_overrides.values().chain(config.deadband.field_overrides.values())
        .filter_map(|rule| rule.max_silence_sec).fold(config.deadband.max_silence_sec, u64::max)
        .max(config.snapshot_interval_sec).max(1);
    let gap_threshold_ms = silence.saturating_mul(3000);
    let mut result = TrendHistory { points: Vec::new(), raw_samples: 0, gap_threshold_ms, source_endpoint: None, warnings: vec![
        "Historian timestamps are producer source timestamps, not solver issue times. Rows without source provenance are unavailable.".into(),
        "No last-value resampling or averages are used; quality gaps and setting changes are retained.".into(),
        "Legacy rows without an exact node_id are excluded; ambiguous tag/field identities are never reconstructed.".into(),
    ] };
    if !config.enabled { result.warnings.push("Historian is disabled in this HMI configuration.".into()); return Ok(result); }
    ensure!(!config.table_name.is_empty() && config.table_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "Invalid historian table identifier");
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let scope=endpoint.map(|endpoint|format!(" AND source_endpoint = '{}'",quote(endpoint))).unwrap_or_default();
    for node in nodes {
        let query = format!("SELECT timestamp, value, status, source_endpoint, evidence_valid FROM {} WHERE node_id = '{}' AND timestamp >= '{}' AND timestamp < '{}'{} ORDER BY timestamp LIMIT 500001",
            config.table_name, quote(node), start.to_rfc3339(), end.to_rfc3339(),scope);
        let response = client.get(format!("http://{}:{}/exec", config.host, config.rest_port))
            .query(&[("query", query)]).send().await?.error_for_status()?;
        let data: serde_json::Value = response.json().await?;
        if let Some(error) = data.get("error") { bail!("Historian query rejected: {error}"); }
        let rows = data.get("dataset").and_then(serde_json::Value::as_array).context("Historian response has no dataset")?;
        ensure!(rows.len() <= 500000, "Selected history exceeds 500000 raw samples per signal; narrow the time window");
        result.raw_samples += rows.len();
        let mut points = Vec::with_capacity(rows.len());
        let mut previous: Option<DateTime<chrono::FixedOffset>> = None;
        for row in rows {
            let values = row.as_array().context("Invalid historian row")?;
            let timestamp = values.first().and_then(serde_json::Value::as_str).context("Historian row has no timestamp")?;
            let time = DateTime::parse_from_rfc3339(timestamp).context("Invalid historian timestamp")?;
            let source=values.get(3).and_then(serde_json::Value::as_str).filter(|value|!value.is_empty()).context("Historian source provenance is absent")?;
            if let Some(expected)=&result.source_endpoint {ensure!(expected==source,"History spans multiple physical endpoints; select an exact commissioning scope");}
            else {result.source_endpoint=Some(source.to_owned());}
            ensure!(endpoint.is_none_or(|expected|expected==source),"Historian endpoint does not match commissioning profile");
            let evidence_valid=values.get(4).and_then(serde_json::Value::as_bool).context("Historian evidence provenance is absent")?;
            // Detect absence before reducing display points: sparse extrema are not lost telemetry.
            if let Some(last) = previous {
                ensure!(time>last,"Duplicate or regressing historian source timestamp; evidence is ambiguous");
                if max_points.is_some() && (time - last).num_milliseconds() > gap_threshold_ms as i64 {
                    points.push(TrendPoint { timestamp: (last + chrono::Duration::milliseconds(gap_threshold_ms as i64)).to_rfc3339(),
                        node_id: node, value: None, status: None, evidence_valid:false });
                }
            }
            previous = Some(time);
            let value = values.get(1).and_then(|v| v.as_f64().or_else(|| v.as_bool().map(|b| if b {1.0} else {0.0}))).filter(|v| v.is_finite());
            let status = values.get(2).and_then(serde_json::Value::as_u64).and_then(|v| u32::try_from(v).ok())
                .map(|status|if evidence_valid{status}else{status|0x80000000});
            points.push(TrendPoint { timestamp: timestamp.to_owned(), node_id: node, value, status, evidence_valid });
        }
        result.points.extend(retain_points(points, max_points, setting_nodes.contains(node)));
        ensure!(result.points.len() <= 200000, "Too many extrema or events to display safely; narrow the time window or select fewer signals");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(i: usize, value: Option<f64>, status: Option<u32>, node_id: &str) -> TrendPoint<'_> {
        TrendPoint { timestamp: format!("2026-01-01T00:{:02}:{:02}Z", i/60, i%60), node_id, value, status, evidence_valid:true }
    }
    #[test]
    fn extrema_and_bad_quality_are_not_hidden_by_reduction() {
        let rows = (0..100).map(|i| point(i, Some(if i==17 {999.0} else if i==71 {-20.0} else {2.0}), Some(if i==43 {0x80000000} else {0}), "PV")).collect();
        let reduced = retain_points(rows, Some(8), false);
        assert!(reduced.iter().any(|p| p.value == Some(999.0)));
        assert!(reduced.iter().any(|p| p.value == Some(-20.0)));
        assert!(reduced.iter().any(|p| p.status == Some(0x80000000)));
        assert!(reduced.iter().any(|p| p.timestamp == "2026-01-01T00:00:42Z"));
        assert!(reduced.iter().any(|p| p.timestamp == "2026-01-01T00:00:44Z"));
    }
    #[test]
    fn settings_transitions_survive_display_budget() {
        let reduced = retain_points((0..100).map(|i| point(i, Some(if i==37 {4.0} else {3.0}), Some(0), "plant.quality.maximum")).collect(), Some(4), true);
        for second in [36,37,38] { assert!(reduced.iter().any(|p| p.timestamp == format!("2026-01-01T00:00:{second:02}Z"))); }
    }
    #[test]
    fn missing_intervals_remain_visible() {
        let reduced = retain_points((0..100).map(|i| point(i, (i!=37).then_some(4.0), Some(0), "PV")).collect(), Some(4), false);
        assert!(reduced.iter().any(|p| p.value.is_none()));
        assert!(reduced.iter().any(|p| p.timestamp == "2026-01-01T00:00:36Z"));
        assert!(reduced.iter().any(|p| p.timestamp == "2026-01-01T00:00:38Z"));
    }
}
