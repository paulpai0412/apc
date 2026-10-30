//! Read-only reports from completed, local simulation campaigns.
//! Metrics remain full-resolution; display sampling preserves per-channel extrema.
use std::{collections::{BTreeSet, VecDeque}, fs, path::{Component, Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};
use axum::{extract::{Path as RoutePath, Query, State}, http::{header, StatusCode}, response::{IntoResponse, Response}, Json};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::{auth::{CurrentUser, Permission}, state::AppState};

type ReportResult<T> = Result<T, (StatusCode, String)>;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TRACE_BYTES: u64 = 1024 * 1024 * 1024;
const CACHE_ENTRIES: usize = 3;

#[derive(Debug, Deserialize)]
struct Trace {
    time_seconds: Vec<f64>,
    cv: Vec<Vec<f64>>,
    mv: Vec<Vec<f64>>,
    dv: Vec<Vec<f64>>,
    targets: Vec<Vec<f64>>,
    #[serde(default)]
    command_mv: Vec<Vec<f64>>,
}
#[derive(Deserialize)]
struct TraceFile { trace: Trace }
struct CachedTrace { path: PathBuf, len: u64, modified: SystemTime, trace: Arc<Trace> }
pub struct ReportStore { root: PathBuf, cache: Mutex<VecDeque<CachedTrace>> }

fn invalid(message: impl Into<String>) -> (StatusCode, String) { (StatusCode::BAD_REQUEST, message.into()) }
fn failure(message: impl Into<String>) -> (StatusCode, String) { (StatusCode::INTERNAL_SERVER_ERROR, message.into()) }
fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn read_json(path: &Path, max_bytes: u64) -> ReportResult<Value> {
    let metadata = fs::metadata(path).map_err(|_| (StatusCode::NOT_FOUND, "Report artifact not found".into()))?;
    if !metadata.is_file() || metadata.len() > max_bytes { return Err(failure("Report artifact exceeds size limit or is not a file")); }
    let file = fs::File::open(path).map_err(|_| failure("Cannot open report artifact"))?;
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(|_| failure("Invalid report JSON"))
}

impl ReportStore {
    pub fn new(root: PathBuf) -> Self { Self { root, cache: Mutex::new(VecDeque::new()) } }
    fn directory(&self, id: &str) -> ReportResult<PathBuf> {
        if !valid_id(id) { return Err(invalid("Invalid report identifier")); }
        let root = self.root.canonicalize().map_err(|_| (StatusCode::NOT_FOUND, "No report directory".into()))?;
        let directory = root.join(id).canonicalize().map_err(|_| (StatusCode::NOT_FOUND, "Report not found".into()))?;
        if directory.parent() != Some(root.as_path()) || !directory.is_dir() { return Err(invalid("Report path escapes configured directory")); }
        Ok(directory)
    }
    fn artifact_path(&self, directory: &Path, relative: &str) -> ReportResult<PathBuf> {
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty() || relative.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(invalid("Invalid artifact path"));
        }
        let path = directory.join(relative).canonicalize().map_err(|_| (StatusCode::NOT_FOUND, "Artifact not found".into()))?;
        if !path.starts_with(directory) || !path.is_file() { return Err(invalid("Artifact is outside report directory")); }
        Ok(path)
    }
    pub fn manifest(&self, id: &str) -> ReportResult<Value> {
        let directory = self.directory(id)?;
        let manifest = read_json(&self.artifact_path(&directory, "report.json")?, MAX_MANIFEST_BYTES)?;
        if manifest.get("schema_version").and_then(Value::as_u64) != Some(1) || manifest.get("id").and_then(Value::as_str) != Some(id) {
            return Err(failure("Unsupported or inconsistent report manifest"));
        }
        Ok(manifest)
    }
    pub fn list(&self) -> ReportResult<Value> {
        if !self.root.exists() { return Ok(json!({"reports": []})); }
        let entries = fs::read_dir(&self.root).map_err(|_| failure("Cannot read report directory"))?;
        let mut reports = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| failure("Cannot read report entry"))?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) || !entry.path().join("report.json").is_file() { continue; }
            let manifest = self.manifest(&id)?;
            reports.push(json!({"id": id, "title": manifest["title"], "created_at": manifest["created_at"],
                "status": manifest["status"], "start": manifest["period"]["start"], "end": manifest["period"]["end"],
                "sample_time": manifest["period"]["sample_time"], "samples": manifest["period"]["samples"], "synthetic": manifest["synthetic"]}));
        }
        reports.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
        Ok(json!({"reports": reports}))
    }
    fn trace(&self, path: PathBuf) -> ReportResult<Arc<Trace>> {
        let metadata = fs::metadata(&path).map_err(|_| (StatusCode::NOT_FOUND, "Trace not found".into()))?;
        let modified = metadata.modified().map_err(|_| failure("Cannot inspect trace timestamp"))?;
        if metadata.len() > MAX_TRACE_BYTES { return Err(failure("Trace exceeds configured reader capacity")); }
        {
            let cache = self.cache.lock().map_err(|_| failure("Report cache unavailable"))?;
            if let Some(entry) = cache.iter().find(|entry| entry.path == path && entry.len == metadata.len() && entry.modified == modified) {
                return Ok(Arc::clone(&entry.trace));
            }
        }
        let file = fs::File::open(&path).map_err(|_| failure("Cannot open trace"))?;
        let parsed: TraceFile = serde_json::from_reader(std::io::BufReader::new(file)).map_err(|_| failure("Invalid trace JSON"))?;
        parsed.trace.validate()?;
        let trace = Arc::new(parsed.trace);
        let mut cache = self.cache.lock().map_err(|_| failure("Report cache unavailable"))?;
        cache.retain(|entry| entry.path != path);
        if cache.len() >= CACHE_ENTRIES { cache.pop_front(); }
        cache.push_back(CachedTrace { path, len: metadata.len(), modified, trace: Arc::clone(&trace) });
        Ok(trace)
    }
    fn series(&self, id: &str, query: &SeriesQuery) -> ReportResult<Value> {
        let manifest = self.manifest(id)?;
        let run = manifest["runs"].as_array().and_then(|runs| runs.iter().find(|run| run["id"].as_str() == Some(&query.run)))
            .ok_or_else(|| (StatusCode::NOT_FOUND, "Run not found".into()))?;
        let relative = run["trace_file"].as_str().ok_or_else(|| failure("Run has no trace artifact"))?;
        let trace = self.trace(self.artifact_path(&self.directory(id)?, relative)?)?;
        let origin = parse_time(manifest["period"]["start"].as_str().ok_or_else(|| failure("Missing report period"))?)?;
        let start = query.start.as_deref().map(parse_time).transpose()?.map(|time| (time - origin).num_milliseconds() as f64 / 1000.0).unwrap_or(f64::NEG_INFINITY);
        let end = query.end.as_deref().map(parse_time).transpose()?.map(|time| (time - origin).num_milliseconds() as f64 / 1000.0).unwrap_or(f64::INFINITY);
        if start >= end { return Err(invalid("start must precede end")); }
        let max_points = query.max_points.unwrap_or(2000);
        if !(100..=10000).contains(&max_points) { return Err(invalid("max_points must be 100..10000")); }
        let first = trace.time_seconds.partition_point(|time| *time < start);
        let last = trace.time_seconds.partition_point(|time| *time < end);
        let indices = trace.display_indices(first, last, max_points)?;
        let mut rows = Vec::with_capacity(indices.len());
        for index in indices {
            let millis = trace.time_seconds[index] * 1000.0;
            if !millis.is_finite() || millis.abs() > i64::MAX as f64 { return Err(failure("Invalid trace time range")); }
            let instant = origin.checked_add_signed(chrono::Duration::milliseconds(millis.round() as i64)).ok_or_else(|| failure("Trace timestamp overflow"))?;
            rows.push(SeriesRow { timestamp: instant.to_rfc3339_opts(SecondsFormat::Millis, true), time_seconds: trace.time_seconds[index],
                cv: &trace.cv[index], mv: &trace.mv[index], dv: &trace.dv[index], targets: &trace.targets[index],
                command_mv: trace.command_mv.get(index).map(Vec::as_slice) });
        }
        Ok(json!({"run_id": query.run, "total_samples": last-first, "returned_samples": rows.len(),
            "downsampled": rows.len() < last-first, "rows": rows}))
    }
    fn download(&self, id: &str, name: &str) -> ReportResult<Response> {
        let manifest = self.manifest(id)?;
        let artifact = manifest["artifacts"].as_array().and_then(|items| items.iter().find(|item| item["name"].as_str() == Some(name)))
            .ok_or_else(|| (StatusCode::NOT_FOUND, "Artifact is not listed in manifest".into()))?;
        if name.contains(['\r', '\n', '"', '/', '\\']) { return Err(invalid("Invalid download name")); }
        let path = self.artifact_path(&self.directory(id)?, artifact["path"].as_str().ok_or_else(|| failure("Invalid artifact manifest"))?)?;
        if fs::metadata(&path).map_err(|_| failure("Cannot inspect artifact"))?.len() > MAX_TRACE_BYTES { return Err(failure("Artifact exceeds download limit")); }
        let bytes = fs::read(path).map_err(|_| failure("Cannot read artifact"))?;
        Ok(([(header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))], bytes).into_response())
    }
}

impl Trace {
    fn validate(&self) -> ReportResult<()> {
        let n = self.time_seconds.len();
        if n == 0 || self.time_seconds.iter().any(|t| !t.is_finite()) || self.time_seconds.windows(2).any(|pair| pair[1] <= pair[0]) {
            return Err(failure("Trace times are empty, nonfinite or out of order"));
        }
        for rows in [&self.cv, &self.mv, &self.dv, &self.targets] {
            if rows.len() != n || rows.iter().any(|row| row.len() != rows[0].len() || row.iter().any(|v| !v.is_finite())) {
                return Err(failure("Trace dimensions or values are invalid"));
            }
        }
        if self.targets[0].len() != self.cv[0].len() { return Err(failure("CV target dimensions mismatch")); }
        if !self.command_mv.is_empty() && (self.command_mv.len() != n || self.command_mv.iter().any(|row| row.len() != self.mv[0].len() || row.iter().any(|v| !v.is_finite()))) {
            return Err(failure("Command trace dimensions or values are invalid"));
        }
        Ok(())
    }
    fn channels(&self, index: usize) -> impl Iterator<Item = &f64> {
        self.cv[index].iter().chain(&self.mv[index]).chain(&self.dv[index]).chain(&self.targets[index])
            .chain(self.command_mv.get(index).into_iter().flatten())
    }
    fn display_indices(&self, first: usize, last: usize, limit: usize) -> ReportResult<Vec<usize>> {
        let count = last - first;
        if count <= limit { return Ok((first..last).collect()); }
        let channels = self.channels(first).count();
        let per_bucket = 2 * channels + 2;
        if channels == 0 || limit < per_bucket { return Err(invalid("max_points too small for trace channel count")); }
        let buckets = limit / per_bucket;
        let width = count.div_ceil(buckets);
        let mut selected = BTreeSet::new();
        let mut lows = vec![first; channels];
        let mut highs = vec![first; channels];
        let mut low_values = vec![0.0; channels];
        let mut high_values = vec![0.0; channels];
        for begin in (first..last).step_by(width) {
            let stop = (begin + width).min(last);
            lows.fill(begin); highs.fill(begin);
            for (j, value) in self.channels(begin).enumerate() { low_values[j] = *value; high_values[j] = *value; }
            for index in begin + 1..stop {
                for (j, value) in self.channels(index).enumerate() {
                    if *value < low_values[j] { low_values[j] = *value; lows[j] = index; }
                    if *value > high_values[j] { high_values[j] = *value; highs[j] = index; }
                }
            }
            selected.insert(begin); selected.insert(stop-1);
            selected.extend(&lows); selected.extend(&highs);
        }
        Ok(selected.into_iter().collect())
    }
}

fn parse_time(value: &str) -> ReportResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).map(|time| time.with_timezone(&Utc)).map_err(|_| invalid("Times must be RFC3339 with timezone"))
}
#[derive(Deserialize)]
pub struct SeriesQuery { run: String, start: Option<String>, end: Option<String>, max_points: Option<usize> }
#[derive(Serialize)]
struct SeriesRow<'a> {
    timestamp: String, time_seconds: f64, cv: &'a [f64], mv: &'a [f64], dv: &'a [f64], targets: &'a [f64],
    #[serde(skip_serializing_if = "Option::is_none")] command_mv: Option<&'a [f64]>,
}
async fn read_response<T: IntoResponse + Send + 'static>(work: impl FnOnce() -> ReportResult<T> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => value.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => failure("Report reader failed").into_response(),
    }
}
pub async fn list(State(state): State<AppState>, user: CurrentUser) -> Response {
    if let Err(error) = user.require(Permission::ReadView) { return error.into_response(); }
    read_response(move || state.reports.list().map(Json)).await
}
pub async fn manifest(State(state): State<AppState>, user: CurrentUser, RoutePath(id): RoutePath<String>) -> Response {
    if let Err(error) = user.require(Permission::ReadView) { return error.into_response(); }
    read_response(move || state.reports.manifest(&id).map(Json)).await
}
pub async fn series(State(state): State<AppState>, user: CurrentUser, RoutePath(id): RoutePath<String>, Query(query): Query<SeriesQuery>) -> Response {
    if let Err(error) = user.require(Permission::ReadView) { return error.into_response(); }
    read_response(move || state.reports.series(&id, &query).map(Json)).await
}
pub async fn artifact(State(state): State<AppState>, user: CurrentUser, RoutePath((id, name)): RoutePath<(String, String)>) -> Response {
    if let Err(error) = user.require(Permission::ReadView) { return error.into_response(); }
    read_response(move || state.reports.download(&id, &name)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sampling_preserves_spikes_endpoints_and_limit() {
        let mut trace = Trace { time_seconds: (0..1000).map(|i| i as f64).collect(), cv: vec![vec![0.0, 1.0]; 1000],
            mv: vec![vec![0.0]; 1000], dv: vec![vec![]; 1000], targets: vec![vec![0.0, 1.0]; 1000], command_mv: Vec::new() };
        trace.cv[487][0] = 200.0; trace.cv[489][1] = -100.0;
        trace.validate().unwrap();
        let indices = trace.display_indices(0, 1000, 100).unwrap();
        assert!(indices.len() <= 100);
        for expected in [0, 487, 489, 999] { assert!(indices.contains(&expected)); }
        assert_eq!(trace.display_indices(480, 490, 100).unwrap(), (480..490).collect::<Vec<_>>());
    }
    #[test]
    fn path_identifiers_reject_traversal_and_url_encodings() {
        for bad in ["", "..", "../data", "run/id", "run\\id", "%2e%2e", "run.json"] { assert!(!valid_id(bad)); }
        assert!(valid_id("cement-2026_01"));
    }
}
