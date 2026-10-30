use anyhow::{ensure, Context, Result};
use authrity_apc_engine::{config::UnifiedModel, twin::History, validation};
use clap::{Parser, Subcommand};
use std::{fs::OpenOptions, io::{BufWriter, Write}, path::Path};

#[derive(Parser)]
#[command(about = "Isolated historical model validation and closed-loop MPC experiments")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Validate {
        #[arg(long)] plant: String,
        #[arg(long)] history: String,
        #[arg(long)] output: String,
    },
    ClosedLoop {
        #[arg(long)] plant: String,
        #[arg(long)] controller: String,
        #[arg(long)] history: String,
        #[arg(long)] output: String,
    },
}

fn write_result(path: &str, result: &impl serde::Serialize) -> Result<()> {
    // Never overwrite a model/history or leave a stale successful result for a failed trial.
    let file = OpenOptions::new().write(true).create_new(true).open(path)
        .with_context(|| format!("Output must be a new file: {path}"))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, result).context("Writing result JSON")?;
    writer.flush().context("Flushing result JSON")
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Validate { plant, history, output } => {
            ensure!(!Path::new(&output).exists(), "Output already exists; choose a new result path");
            let result = validation::validate(&UnifiedModel::load(&plant)?, &History::load(&history)?)?;
            write_result(&output, &result)
        }
        Command::ClosedLoop { plant, controller, history, output } => {
            ensure!(!Path::new(&output).exists(), "Output already exists; choose a new result path");
            let result = validation::closed_loop(&UnifiedModel::load(&plant)?, &UnifiedModel::load(&controller)?, &History::load(&history)?)?;
            write_result(&output, &result)
        }
    }
}
