//! Direct reference and real stdin/stdout COM DA bridge transports.
use anyhow::{bail,ensure,Context,Result};
use serde_json::{json,Value};
use std::{io::{BufRead,BufReader,BufWriter,Write},process::{Child,ChildStdin,ChildStdout,Command,Stdio}};
use crate::campaign_plant::Fault;

// A forced native-session termination bypasses Drop. On Linux the bridge must
// die with its native owner too, including the race between fork and prctl.
#[cfg(target_os="linux")]
fn bind_bridge_lifetime(command:&mut Command) {
    use std::os::unix::process::CommandExt;
    let parent=std::process::id() as libc::pid_t;
    // SAFETY: the child hook uses only async-signal-safe syscalls and constructs
    // OS errors without allocation; it does not access parent locks or memory.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG,libc::SIGKILL as libc::c_ulong,
                0 as libc::c_ulong,0 as libc::c_ulong,0 as libc::c_ulong)!=0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid()!=parent {return Err(std::io::Error::from_raw_os_error(libc::ECHILD));}
            Ok(())
        });
    }
}

struct Bridge {child:Child,input:BufWriter<ChildStdin>,output:BufReader<ChildStdout>,line:String}
impl Bridge {
    fn request(&mut self,request:Value)->Result<Value> {
        serde_json::to_writer(&mut self.input,&request)?;self.input.write_all(b"\n")?;self.input.flush()?;
        self.line.clear();ensure!(self.output.read_line(&mut self.line)?>0,"DA bridge closed stdout");
        let reply:Value=serde_json::from_str(&self.line).context("Malformed DA bridge response")?;
        ensure!(reply["ok"].as_bool()==Some(true),"DA bridge error: {reply}");Ok(reply)
    }
}
impl Drop for Bridge {fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();}}

pub struct Transport {bridge:Option<Bridge>,input_ids:Vec<String>,command_ids:Vec<String>,direct_inputs:Vec<f64>,direct_commands:Vec<f64>,pub quality:Vec<u16>,pub stats:Value}
impl Transport {
    pub fn new(executable:Option<&str>,args:&[String],cv:&[f64],mv:&[f64],dv:&[f64],targets:&[f64],timestamp:i64)->Result<Self> {
        let input_ids:Vec<_>=[("CV",cv.len()),("MV",mv.len()),("DV",dv.len()),("Target",targets.len())].into_iter().flat_map(|(role,n)|(0..n).map(move|i|format!("{role}:{i}"))).collect();
        let command_ids:Vec<_>=(0..mv.len()).map(|i|format!("Command:{i}")).collect();
        let inputs:Vec<f64>=cv.iter().chain(mv).chain(dv).chain(targets).copied().collect();
        let quality=vec![192;inputs.len()];
        let mut transport=Self{bridge:None,input_ids,command_ids,direct_inputs:inputs,direct_commands:mv.to_vec(),quality,stats:Value::Null};
        if let Some(exe)=executable {
            let mut command=Command::new(exe);
            command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
            #[cfg(target_os="linux")]
            bind_bridge_lifetime(&mut command);
            let mut child=command.spawn().context("Starting DA bridge")?;
            let input=BufWriter::new(child.stdin.take().context("DA stdin unavailable")?);
            let output=BufReader::new(child.stdout.take().context("DA stdout unavailable")?);
            let mut bridge=Bridge{child,input,output,line:String::new()};
            let mut items:Vec<_>=transport.input_ids.iter().zip(&transport.direct_inputs).map(|(id,value)|json!({"id":id,"value":value,"writable":false})).collect();
            items.extend(transport.command_ids.iter().zip(mv).map(|(id,value)|json!({"id":id,"value":value,"writable":true})));
            bridge.request(json!({"op":"init","items":items,"timestamp_ms":timestamp}))?;transport.bridge=Some(bridge);
        }Ok(transport)
    }
    pub fn fault(&mut self,f:&Fault)->Result<()> {
        let bridge=self.bridge.as_mut().context("Fault injection requires actual opc-da transport")?;
        let mut request=json!({"op":"fault","kind":f.kind,"enabled":true});
        if !f.items.is_empty(){request["items"]=json!(f.items);}
        bridge.request(request)?;Ok(())
    }
    pub fn inputs(&mut self,values:&[f64],timestamp:i64,out:&mut[f64])->Result<()> {
        ensure!(values.len()==self.input_ids.len()&&out.len()==values.len(),"Transport input dimension mismatch");
        if let Some(bridge)=&mut self.bridge {
            let items:Vec<_>=self.input_ids.iter().zip(values).map(|(id,value)|json!({"id":id,"value":value})).collect();
            bridge.request(json!({"op":"update","items":items,"timestamp_ms":timestamp}))?;
            let reply=bridge.request(json!({"op":"read","items":self.input_ids}))?;
            decode_read(&reply,timestamp,out,Some(&mut self.quality))?;
        } else {self.direct_inputs.copy_from_slice(values);out.copy_from_slice(&self.direct_inputs);} Ok(())
    }
    pub fn apply(&mut self,values:&[f64],timestamp:i64,out:&mut[f64])->Result<()> {
        ensure!(values.len()==self.command_ids.len()&&out.len()==values.len(),"Transport command dimension mismatch");
        if let Some(bridge)=&mut self.bridge {
            let items:Vec<_>=self.command_ids.iter().zip(values).map(|(id,value)|json!({"id":id,"value":value})).collect();
            let written=bridge.request(json!({"op":"write","items":items,"timestamp_ms":timestamp}))?;
            let results=written["hresult"].as_array().context("DA write missing HRESULTs")?;
            ensure!(results.len()==values.len()&&results.iter().all(|v|v.as_i64().map_or(false,|h|h>=0&&h<=i32::MAX as i64)),"DA per-item write failure: {written}");
            let reply=bridge.request(json!({"op":"read","items":self.command_ids}))?;
            decode_read(&reply,timestamp,out,None)?;
            ensure!(out.iter().zip(values).all(|(a,b)|(a-b).abs()<=1e-10*(1.0+b.abs())),"DA write readback mismatch");
        }else{self.direct_commands.copy_from_slice(values);out.copy_from_slice(&self.direct_commands);}Ok(())
    }
    pub fn close(&mut self)->Result<()> {
        if let Some(mut bridge)=self.bridge.take(){let response=bridge.request(json!({"op":"close"}))?;self.stats=response.get("stats").cloned().unwrap_or(Value::Null);let status=bridge.child.wait()?;ensure!(status.success(),"DA bridge exit status {status}");}Ok(())
    }
}
fn decode_read(reply:&Value,timestamp:i64,out:&mut[f64],mut quality_out:Option<&mut[u16]>)->Result<()> {
    let values=reply["values"].as_array().context("DA values missing")?;
    let quality=reply["quality"].as_array().context("DA quality missing")?;
    let times=reply["timestamp_ms"].as_array().context("DA timestamps missing")?;
    let results=reply["hresult"].as_array().context("DA HRESULTs missing")?;
    ensure!([values.len(),quality.len(),times.len(),results.len()].iter().all(|n|*n==out.len()),"DA response dimensions mismatch");
    for i in 0..out.len(){
        let h=results[i].as_i64().context("Malformed DA HRESULT")?;
        ensure!(h>=0&&h<=i32::MAX as i64,"DA read item {i} failed HRESULT {h}");
        let q=quality[i].as_u64().context("Malformed DA quality")?;ensure!(q<=u16::MAX as u64&&q&192==192,"DA bad quality item {i}: {q}");
        if let Some(output)=&mut quality_out {output[i]=q as u16;}
        ensure!(times[i].as_i64()==Some(timestamp),"DA stale timestamp item {i}");
        let v=values[i].as_f64().context("DA nonnumeric value")?;if !v.is_finite(){bail!("DA nonfinite item {i}");}out[i]=v;
    }Ok(())
}
