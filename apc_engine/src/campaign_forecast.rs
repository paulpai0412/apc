//! Streamed campaign forecasts. An empty/missing index means the archive is incomplete.
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{fs::{File, OpenOptions}, io::{BufWriter, Write}, path::Path};
use crate::config::UnifiedModel;

#[derive(Serialize)]
struct Header {
    schema_version: u8,
    sample_time: f64,
    cv_count: usize,
    mv_count: usize,
    prediction_horizon: usize,
    control_horizon: usize,
    layout: &'static str,
    cv_first_offset_steps: usize,
    mv_first_offset_steps: usize,
}

impl Header {
    fn new(model: &UnifiedModel) -> Self {
        Self {
            schema_version: 1, sample_time: model.tuning.sample_time,
            cv_count: model.variables.cvs.len(), mv_count: model.variables.mvs.len(),
            prediction_horizon: model.tuning.prediction_horizon,
            control_horizon: model.tuning.control_horizon,
            layout: "step-major", cv_first_offset_steps: 1, mv_first_offset_steps: 0,
        }
    }

    fn validate(&self, issued_at_seconds: f64, cv: &[f64], mv_base: &[f64], mv_delta: &[f64]) -> Result<()> {
        ensure!(cv.len() == self.cv_count * self.prediction_horizon
            && mv_base.len() == self.mv_count && mv_delta.len() == self.mv_count * self.control_horizon,
            "Forecast horizon dimensions do not match header");
        ensure!(issued_at_seconds.is_finite() && cv.iter().chain(mv_base).chain(mv_delta).all(|v| v.is_finite()),
            "Nonfinite forecast cannot be retained");
        Ok(())
    }
}

/// One completed native horizon, never part of the persisted campaign result.
/// Solver vectors transfer ownership; only the small pre-solve MV base is copied
/// into reusable storage. Snapshot serialization neither solves nor copies arrays.
#[derive(Serialize)]
pub(super) struct LatestForecast {
    #[serde(flatten)]
    header: Header,
    sample: usize,
    issued_at_seconds: f64,
    cv: Vec<f64>,
    mv_base: Vec<f64>,
    mv_delta: Vec<f64>,
    #[serde(skip)]
    valid: bool,
}

impl LatestForecast {
    pub(super) fn new(model: &UnifiedModel) -> Self {
        Self {header: Header::new(model), sample: 0, issued_at_seconds: 0.0,
            cv: Vec::new(), mv_base: vec![0.0; model.variables.mvs.len()], mv_delta: Vec::new(), valid: false}
    }

    pub(super) fn invalidate(&mut self) {
        self.valid = false;
        self.cv.clear();
        self.mv_delta.clear();
    }

    pub(super) fn capture(&mut self, sample: usize, issued_at_seconds: f64,
        cv: Vec<f64>, mv_base: &[f64], mv_delta: Vec<f64>) -> Result<()> {
        self.invalidate();
        self.header.validate(issued_at_seconds, &cv, mv_base, &mv_delta)?;
        self.sample = sample;
        self.issued_at_seconds = issued_at_seconds;
        self.cv = cv;
        self.mv_base.copy_from_slice(mv_base);
        self.mv_delta = mv_delta;
        self.valid = true;
        Ok(())
    }

    pub(super) fn for_trace(&self, trace: &super::Trace) -> Option<&Self> {
        (self.valid && self.sample.checked_add(1) == Some(trace.time_seconds.len())
            && trace.time_seconds.last() == Some(&self.issued_at_seconds)).then_some(self)
    }
}

#[derive(Serialize)]
struct Record<'a> {
    sample: usize,
    issued_at_seconds: f64,
    cv: &'a [f64],
    mv_base: &'a [f64],
    mv_delta: &'a [f64],
}

#[derive(Serialize)]
struct Index<'a> {
    schema_version: u8,
    header: &'a Header,
    offsets: &'a [u64],
    samples: usize,
}

pub(super) struct ForecastArchive {
    data: File,
    index: File,
    header: Header,
    offsets: Vec<u64>,
    buffer: Vec<u8>,
    position: u64,
    failed: bool,
}

impl ForecastArchive {
    pub(super) fn create(path: &Path, model: &UnifiedModel, samples: usize) -> Result<Self> {
        let data = OpenOptions::new().write(true).create_new(true).open(path)
            .with_context(|| format!("Forecast output must be a new file: {}", path.display()))?;
        let mut index_path = path.as_os_str().to_os_string();
        index_path.push(".index.json");
        // Reserve both paths before any scan. Never replace an older artifact, even its index.
        let index = OpenOptions::new().write(true).create_new(true).open(&index_path)
            .context("Forecast index must be a new file")?;
        let header = Header::new(model);
        let buffer_capacity = 256 + 25 * (header.cv_count * header.prediction_horizon
            + header.mv_count * (header.control_horizon + 1));
        let mut archive = Self {data, index, header, offsets: Vec::with_capacity(samples), buffer: Vec::with_capacity(buffer_capacity), position: 0, failed: true};
        serde_json::to_writer(&mut archive.buffer, &archive.header)?;
        archive.buffer.push(b'\n');
        archive.data.write_all(&archive.buffer).context("Writing forecast header")?;
        archive.position = archive.buffer.len() as u64;
        archive.failed = false;
        Ok(archive)
    }

    pub(super) fn record(&mut self, sample: usize, issued_at_seconds: f64, cv: &[f64], mv_base: &[f64], mv_delta: &[f64]) -> Result<()> {
        ensure!(!self.failed, "Forecast archive already failed");
        self.failed = true;
        self.header.validate(issued_at_seconds, cv, mv_base, mv_delta)?;
        self.buffer.clear();
        serde_json::to_writer(&mut self.buffer, &Record {sample, issued_at_seconds, cv, mv_base, mv_delta})?;
        self.buffer.push(b'\n');
        // One reusable serialized horizon buffer, no retained prediction/plan vectors.
        self.data.write_all(&self.buffer).context("Writing forecast scan")?;
        self.offsets.push(self.position);
        self.position += self.buffer.len() as u64;
        self.failed = false;
        Ok(())
    }
    pub(super) fn sync(&self) -> Result<()> {
        ensure!(!self.failed, "Cannot acknowledge an incomplete forecast archive");
        self.data.sync_all().context("Persisting forecast batch")
    }

    pub(super) fn finish(mut self) -> Result<()> {
        ensure!(!self.failed, "Forecast archive is incomplete after recording failure");
        self.data.sync_all().context("Finalizing forecast data")?;
        let result = (|| -> Result<()> {
            let mut writer = BufWriter::new(&mut self.index);
            serde_json::to_writer(&mut writer, &Index {schema_version: 1, header: &self.header, offsets: &self.offsets, samples: self.offsets.len()})?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // A partial index must not advertise an apparently completed archive.
            let _ = self.index.set_len(0);
        }
        result.context("Finalizing forecast index")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_or_failed_writes_never_publish_a_readable_index() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("authrity-forecast-io-{}-{timestamp}-{sequence}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        for failure in ["interrupted", "data", "index", "nonfinite"] {
            let path = dir.join(failure);
            let index_path = dir.join(format!("{failure}.index.json"));
            let mut archive = ForecastArchive {
                data: File::create(&path).unwrap(), index: File::create(&index_path).unwrap(),
                header: Header {schema_version: 1, sample_time: 2.0, cv_count: 1, mv_count: 1,
                    prediction_horizon: 3, control_horizon: 1, layout: "step-major",
                    cv_first_offset_steps: 1, mv_first_offset_steps: 0},
                offsets: Vec::new(), buffer: Vec::new(), position: 0, failed: false,
            };
            match failure {
                "data" => {
                    // A real read-only OS file deterministically rejects writes.
                    archive.data = File::open(&path).unwrap();
                    assert!(archive.record(0, 10.0, &[1.0; 3], &[50.0], &[1.0]).is_err());
                    assert!(archive.finish().is_err());
                }
                "index" => {
                    archive.record(0, 10.0, &[1.0; 3], &[50.0], &[1.0]).unwrap();
                    archive.index = File::open(&index_path).unwrap();
                    assert!(archive.finish().is_err());
                }
                "nonfinite" => {
                    assert!(archive.record(0, 10.0, &[f64::NAN; 3], &[50.0], &[1.0]).is_err());
                    assert!(archive.finish().is_err());
                    assert!(std::fs::read(&path).unwrap().is_empty());
                }
                _ => {
                    archive.record(0, 10.0, &[1.0; 3], &[50.0], &[1.0]).unwrap();
                    drop(archive);
                }
            }
            assert!(std::fs::read(&index_path).unwrap().is_empty(), "{failure}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
