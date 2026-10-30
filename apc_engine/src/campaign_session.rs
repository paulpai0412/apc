//! Demand-driven JSONL control of the same stateful runner used by batch campaigns.
//! Requests never synthesize time or values; snapshots borrow completed trace rows.
use super::{forecast::LatestForecast, CampaignResult, CampaignRunner, Trace};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::{BufRead, BufWriter, Write}};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    command: Command,
    expected_samples: Option<usize>,
    count: Option<usize>,
}
#[derive(Deserialize)]
#[serde(rename_all="snake_case")]
enum Command { Snapshot, Advance, Close }

#[derive(Serialize)]
struct Frame<'a> {
    time_seconds: f64,
    cv: &'a [f64], mv: &'a [f64], dv: &'a [f64], targets: &'a [f64],
    command_mv: &'a [f64], predicted_cv: &'a [f64], quality: &'a [u16],
    solver_status: &'a str,
}
#[derive(Serialize)]
struct RecentTrace<'a> {
    time_seconds: &'a [f64], cv: &'a [Vec<f64>], mv: &'a [Vec<f64>],
    dv: &'a [Vec<f64>], targets: &'a [Vec<f64>], command_mv: &'a [Vec<f64>],
    predicted_cv: &'a [Vec<f64>], quality: &'a [Vec<u16>],
    solve_ms: &'a [f64], solver_status: &'a [String],
}
impl<'a> RecentTrace<'a> {
    fn new(trace: &'a Trace)->Self {
        Self::from_sample(trace, trace.time_seconds.len().saturating_sub(256))
    }
    fn from_sample(trace: &'a Trace, first: usize)->Self {
        Self {time_seconds:&trace.time_seconds[first..],cv:&trace.cv[first..],
            mv:&trace.mv[first..],dv:&trace.dv[first..],targets:&trace.targets[first..],
            command_mv:&trace.command_mv[first..],predicted_cv:&trace.predicted_cv[first..],
            quality:&trace.quality[first..],solve_ms:&trace.solve_ms[first..],
            solver_status:&trace.solver_status[first..]}
    }
}
#[derive(Serialize)]
struct Response<'a> {
    request_id: &'a str, status: &'static str, samples: usize,
    requested_samples: usize, sample_time: f64,
    #[serde(skip_serializing_if="Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if="Option::is_none")]
    error: Option<&'a str>,
    frame: Option<Frame<'a>>, recent_trace: RecentTrace<'a>,
    latest_forecast: Option<&'a LatestForecast>,
}

struct Session<'a> {
    runner: Option<CampaignRunner<'a>>, result: Option<CampaignResult>,
    status: &'static str,
    latest_forecast: Option<LatestForecast>,
    journal: BufWriter<File>,
    persisted_samples: usize,
}
impl<'a> Session<'a> {
    fn persist_batch(&mut self, request_id: &str) -> Result<()> {
        if let Some(archive) = self.runner.as_ref().and_then(|runner|runner.archive.as_ref()) {
            archive.sync()?;
        }
        let trace = if let Some(runner) = &self.runner { &runner.trace }
            else { &self.result.as_ref().context("Session trace missing")?.trace };
        if trace.time_seconds.len() == self.persisted_samples { return Ok(()); }
        #[derive(Serialize)]
        struct Batch<'a> { schema_version:u8, request_id:&'a str, first_sample:usize, samples:usize, trace:RecentTrace<'a> }
        serde_json::to_writer(&mut self.journal, &Batch { schema_version:1, request_id,
            first_sample:self.persisted_samples, samples:trace.time_seconds.len(),
            trace:RecentTrace::from_sample(trace,self.persisted_samples) })?;
        self.journal.write_all(b"\n")?;
        self.journal.flush()?;
        self.journal.get_ref().sync_all()?;
        self.persisted_samples=trace.time_seconds.len();
        Ok(())
    }
    fn finish(&mut self,artifact:&mut BufWriter<File>,interruption:Option<&str>)->Result<()> {
        if let Some(mut runner)=self.runner.take() {
            self.latest_forecast=runner.latest_forecast.take();
            let was_terminal=runner.is_finished();
            let result=runner.finish(interruption.map(str::to_owned));
            self.status=if result.status=="valid" {"completed"}
                else if !was_terminal&&result.reason.as_deref()==interruption {"closed"} else {"failed"};
            self.result=Some(result);
            // The sole result is persisted before announcing a terminal response.
            // create_new is enforced by the CLI, and no later request rewrites it.
            let mut buffered=BufWriter::new(artifact.get_mut());
            let persisted=(||->Result<()> {
                serde_json::to_writer(&mut buffered,self.result.as_ref().unwrap())?;
                buffered.write_all(b"\n")?;
                buffered.flush()?;
                buffered.get_ref().sync_all()?;
                Ok(())
            })();
            // Discard unflushed bytes on error instead of retrying them in Drop.
            let (file,_unwritten)=buffered.into_parts();
            if let Err(error)=persisted {
                self.status="failed";
                // Invalidate even a completely serialized success when durability failed.
                let _=file.set_len(0);
                let _=file.sync_all();
                return Err(error.context("Persisting campaign session result"));
            }
        }
        Ok(())
    }

    fn response<'b>(&'b self,id:&'b str,error:Option<&'b str>)->Response<'b> {
        let (trace,sample_time,requested_samples,reason)=if let Some(runner)=&self.runner {
            (&runner.trace,runner.spec.sample_time,runner.scenario.time_seconds.len(),runner.reason.as_deref())
        } else {
            let result=self.result.as_ref().expect("terminal session retains result");
            (&result.trace,result.sample_time,result.requested_samples,result.reason.as_deref())
        };
        let samples=trace.time_seconds.len();
        let frame=samples.checked_sub(1).map(|i|Frame {time_seconds:trace.time_seconds[i],
            cv:&trace.cv[i],mv:&trace.mv[i],dv:&trace.dv[i],targets:&trace.targets[i],
            command_mv:&trace.command_mv[i],predicted_cv:&trace.predicted_cv[i],
            quality:&trace.quality[i],solver_status:&trace.solver_status[i]});
        let latest_forecast=self.runner.as_ref().and_then(|runner|runner.latest_forecast.as_ref())
            .or(self.latest_forecast.as_ref()).and_then(|latest|latest.for_trace(trace));
        Response {request_id:id,status:self.status,samples,requested_samples,sample_time,
            reason,error,frame,recent_trace:RecentTrace::new(trace),latest_forecast}
    }

    fn handle(&mut self,request:&Request,artifact:&mut BufWriter<File>)->Result<Option<String>> {
        match request.command {
            Command::Snapshot=>Ok(None),
            Command::Close=>{self.finish(artifact,Some("Session closed before completion"))?;Ok(None)},
            Command::Advance=>{
                let attempted=(||->Result<()> {
                    let expected=request.expected_samples.context("advance requires expected_samples")?;
                    let count=request.count.context("advance requires count")?;
                    self.runner.as_mut().context("Session is already terminal")?.advance(expected,count)
                })();
                if let Err(error)=attempted {return Ok(Some(format!("{error:#}")));}
                if self.runner.as_ref().is_some_and(CampaignRunner::is_finished) {
                    self.finish(artifact,None)?;
                }
                Ok(None)
            }
        }
    }
}

/// Serve one response per JSONL request. EOF or a broken pipe finalizes an
/// incomplete run as invalid; dropping a transport also kills its bridge child.
/// There is deliberately no unsolicited startup response and no wall-clock loop.
pub fn run<R:BufRead,W:Write>(mut runner:CampaignRunner<'_>,artifact:&mut BufWriter<File>,journal:File,mut input:R,mut output:W)->Result<()> {
    runner.latest_forecast=Some(LatestForecast::new(runner.model));
    let mut session=Session {runner:Some(runner),result:None,status:"paused",latest_forecast:None,
        journal:BufWriter::new(journal),persisted_samples:0};
    let served=(||->Result<()> {
        let mut line=String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)?==0 {break;}
            let parsed=serde_json::from_str::<Request>(&line);
            let (id,error,close)=match parsed {
                Ok(request)=>{
                    let close=matches!(request.command,Command::Close);
                    let error=session.handle(&request,artifact)?;
                    session.persist_batch(&request.request_id)?;
                    (request.request_id,error,close)
                },
                Err(error)=>{
                    let id=serde_json::from_str::<serde_json::Value>(&line).ok()
                        .and_then(|value|value.get("request_id").and_then(|id|id.as_str()).map(str::to_owned))
                        .unwrap_or_default();
                    (id,Some(format!("Invalid session request: {error}")),false)
                }
            };
            serde_json::to_writer(&mut output,&session.response(&id,error.as_deref()))?;
            output.write_all(b"\n")?;output.flush()?;
            if close {break;}
        }
        Ok(())
    })();
    let interruption=if served.is_err() {"Session IO interrupted before completion"} else {"Session input ended before completion"};
    let finalized=session.finish(artifact,Some(interruption));
    served.and(finalized)
}
