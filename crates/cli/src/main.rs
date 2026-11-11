use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use trace_sonic::{logs::Logs, port, route, snapshot::Snapshot, vlan};

#[derive(Parser)]
#[command(
    name = "sonic-trace",
    version,
    about = "Trace a SONiC object through CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB"
)]
struct Cli {
    /// Offline input: a multi-database JSON file, or a directory of per-database dumps
    /// (`CONFIG_DB.json`, `APPL_DB.json`, `STATE_DB.json`, `ASIC_DB.json`)
    #[arg(long, required_unless_present = "live", conflicts_with = "live")]
    snapshot: Option<PathBuf>,
    /// Read the databases from this switch by running `sonic-db-dump` for each one
    #[arg(long)]
    live: bool,
    /// Command used by --live
    #[arg(long, default_value = "sonic-db-dump")]
    db_dump_cmd: String,
    /// Multi-ASIC: use ASIC N (the `asicN` folder of a bundle, or `--netns asicN` live)
    #[arg(long)]
    asic: Option<u32>,
    /// orchagent's swss.rec, for evidence (default: swss.rec next to the snapshot)
    #[arg(long)]
    swss_rec: Option<PathBuf>,
    /// sairedis.rec, for evidence (default: sairedis.rec next to the snapshot)
    #[arg(long)]
    sairedis_rec: Option<PathBuf>,
    /// syslog, for evidence (default: syslog next to the snapshot)
    #[arg(long)]
    syslog: Option<PathBuf>,
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

/// An explicit path wins; otherwise use `<dir>/<name>` if it exists.
fn log_path(explicit: &Option<PathBuf>, dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    explicit
        .clone()
        .or_else(|| dir.map(|d| d.join(name)).filter(|p| p.is_file()))
}

fn run(cli: Cli) -> Result<bool, String> {
    let mut budget = cli.max_input_mb.saturating_mul(1024 * 1024);
    let mut dir: Option<PathBuf> = None;
    let snap = match &cli.snapshot {
        None => Snapshot::from_dump_command(&cli.db_dump_cmd, cli.asic, &mut budget)?,
        Some(path) => {
            let mut path = path.clone();
            if let Some(n) = cli.asic {
                let sub = path.join(format!("asic{n}"));
                if !sub.is_dir() {
                    return Err(format!("{}: no asic{n} folder", path.display()));
                }
                path = sub;
            }
            if path.is_dir() {
                dir = Some(path.clone());
            }
            Snapshot::from_path_budget(&path, &mut budget)?
        }
    };
    let logs = Logs::load(
        log_path(&cli.swss_rec, dir.as_deref(), "swss.rec").as_deref(),
        log_path(&cli.sairedis_rec, dir.as_deref(), "sairedis.rec").as_deref(),
        log_path(&cli.syslog, dir.as_deref(), "syslog").as_deref(),
        &mut budget,
    )?;

    let report = match &cli.cmd {
        Cmd::Vlan { name } => vlan::trace_with(&snap, &logs, name)?,
        Cmd::Port { name } => port::trace_with(&snap, &logs, name)?,
        Cmd::Route { prefix } => route::trace_with(&snap, &logs, prefix)?,
    };
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.render_text());
    }
    let ok = report.diagnosis.stopped_at.is_none();
    if !ok && cli.snapshot.is_none() {
        eprintln!(
            "note: live dumps are taken one database at a time and are not atomic; rerun to confirm"
        );
    }
    Ok(ok)
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
