use clap::{Parser, Subcommand};
use std::process::ExitCode;
use trace_sonic::{port, route, snapshot::Snapshot, vlan};

#[derive(Parser)]
#[command(
    name = "sonic-trace",
    version,
    about = "Trace a SONiC object through CONFIG_DB, APPL_DB and ASIC_DB"
)]
struct Cli {
    /// Offline input: a multi-database JSON file, or a directory of per-database
    /// dumps (`CONFIG_DB.json`, `APPL_DB.json`, `STATE_DB.json`, `ASIC_DB.json`)
    #[arg(long)]
    snapshot: std::path::PathBuf,
    /// Refuse to read more than this many MiB of input in total
    #[arg(long, default_value_t = 256)]
    max_input_mb: u64,
    /// Output JSON instead of text
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Trace a VLAN, e.g. Vlan100
    Vlan { name: String },
    /// Trace a front-panel port, e.g. Ethernet0
    Port { name: String },
    /// Trace a route by prefix, e.g. 10.1.1.0/24
    Route { prefix: String },
}

fn run(cli: Cli) -> Result<bool, String> {
    let limit = cli.max_input_mb.saturating_mul(1024 * 1024);
    let snap = Snapshot::from_path(&cli.snapshot, limit)?;
    let report = match &cli.cmd {
        Cmd::Vlan { name } => vlan::trace(&snap, name)?,
        Cmd::Port { name } => port::trace(&snap, name)?,
        Cmd::Route { prefix } => route::trace(&snap, prefix)?,
    };
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.render_text());
    }
    Ok(report.diagnosis.stopped_at.is_none())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1), // traced fine, object did not reach the ASIC
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
