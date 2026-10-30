use anyhow::{bail,ensure,Context,Result};
use authrity_apc_engine::{campaign,config::UnifiedModel};
use clap::{Parser,Subcommand,ValueEnum};
use serde_json::json;
use std::{fs::{File,OpenOptions},io::{BufWriter,Write}};

#[derive(Parser)]
#[command(about="Full-resolution synthetic plant replay and real DMC/COM DA campaign evaluation")]
struct Args {#[command(subcommand)]command:Command}
#[derive(Clone,ValueEnum)]enum Transport {Direct,OpcDa}
#[derive(clap::Args)]
struct ClosedLoopArgs {
    #[arg(long)]plant:String,#[arg(long)]controller:String,#[arg(long)]scenario:String,#[arg(long)]output:String,
    /// Stream complete solver horizons; also creates PATH.index.json.
    #[arg(long)]forecast_output:Option<std::path::PathBuf>,
    #[arg(long,value_enum,default_value="direct")]transport:Transport,
    #[arg(long)]bridge:Option<String>,#[arg(long,allow_hyphen_values=true)]bridge_arg:Vec<String>,
}
#[derive(Subcommand)]
enum Command {
    Replay {#[arg(long)]plant:String,#[arg(long)]scenario:String,#[arg(long)]output:String},
    ClosedLoop(ClosedLoopArgs),
    /// Demand-driven JSONL snapshots/advances; never advances on wall-clock time.
    Session(ClosedLoopArgs),
}

fn write_result(writer:&mut BufWriter<File>,value:&impl serde::Serialize)->Result<()> {
    serde_json::to_writer(&mut *writer,value)?;writer.write_all(b"\n")?;writer.flush()?;Ok(())
}

fn evaluate(args:ClosedLoopArgs,session:bool,writer:&mut BufWriter<File>,session_started:&mut bool)->Result<bool> {
    let executable=match args.transport {
        Transport::Direct=>{ensure!(args.bridge.is_none()&&args.bridge_arg.is_empty(),"Direct transport cannot accept bridge arguments");None},
        Transport::OpcDa=>Some(args.bridge.as_deref().context("opc-da requires --bridge executable")?),
    };
    let plant=campaign::load_json(&args.plant)?;
    let model=UnifiedModel::load(&args.controller)?;
    let scenario=campaign::load_json(&args.scenario)?;
    if session {
        let runner=campaign::CampaignRunner::new(&plant,&model,&scenario,executable,&args.bridge_arg,args.forecast_output.as_deref())?;
        let journal_path=format!("{}.trace.ndjson",args.output);
        let journal=OpenOptions::new().create_new(true).write(true).open(&journal_path)
            .with_context(||format!("Session trace journal must be a new file: {journal_path}"))?;
        *session_started=true;
        campaign::session::run(runner,writer,journal,std::io::stdin().lock(),BufWriter::new(std::io::stdout().lock()))?;
        Ok(true)
    } else {
        let result=campaign::closed_loop(&plant,&model,&scenario,executable,&args.bridge_arg,args.forecast_output.as_deref())?;
        let valid=result.status=="valid";write_result(writer,&result)?;Ok(valid)
    }
}

fn main()->Result<()> {
    let args=Args::parse();
    let output=match &args.command {
        Command::Replay{output,..}=>output,
        Command::ClosedLoop(args)|Command::Session(args)=>&args.output,
    };
    // Reserve a new artifact before evaluation: failures cannot leave a stale success.
    let file=OpenOptions::new().create_new(true).write(true).open(output).with_context(||format!("Output must be a new file: {output}"))?;
    let mut writer=BufWriter::new(file);
    let mut session_started=false;
    let evaluated:Result<bool>=(||match args.command {
        Command::Replay{plant,scenario,..}=>{
            let value=campaign::replay(&campaign::load_json(&plant)?,&campaign::load_json(&scenario)?)?;
            write_result(&mut writer,&value)?;Ok(true)
        },
        Command::ClosedLoop(args)=>evaluate(args,false,&mut writer,&mut session_started),
        Command::Session(args)=>evaluate(args,true,&mut writer,&mut session_started),
    })();
    let valid=match evaluated {
        Ok(valid)=>valid,
        Err(error)=>{
            // Session IO errors may already have persisted an invalid partial result.
            // Never append a second document or overwrite an existing terminal result.
            if !session_started&&writer.get_ref().metadata()?.len()==0 {
                write_result(&mut writer,&json!({"status":"invalid","reason":format!("{error:#}"),"samples":0,"fault_assertions":[],"trace":{"time_seconds":[],"cv":[],"mv":[],"dv":[],"targets":[],"command_mv":[]}}))?;
            }
            return Err(error);
        }
    };
    if !valid {bail!("Campaign invalid; failure artifact written");}Ok(())
}
