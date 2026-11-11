use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use trace_sonic::{
    lag,
    logs::Logs,
    port, route,
    snapshot::{BundleLayout, Snapshot},
    vlan,
};

#[derive(Parser)]
#[command(
    name = "sonic-trace",
    version,
    about = "Trace a SONiC object through CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB"
)]
struct Cli {
    /// Offline input: a generate_dump bundle (or its dump/ folder), a folder of
    /// per-database dumps, or one multi-database JSON file
    #[arg(long, required_unless_present = "live", conflicts_with = "live")]
    snapshot: Option<PathBuf>,
    /// Read the databases from this switch by running `sonic-db-dump` for each one
    #[arg(long)]
    live: bool,
    /// Command used by --live
    #[arg(long, default_value = "sonic-db-dump")]
    db_dump_cmd: String,
    /// Multi-ASIC: use ASIC N (`<DB>.json.N` in a bundle's dump/ folder, or `--netns asicN` live)
    #[arg(long)]
    asic: Option<u32>,
    /// orchagent's swss.rec, plain or .gz, for evidence (default: found in the bundle)
    #[arg(long)]
    swss_rec: Option<PathBuf>,
    /// sairedis.rec, plain or .gz, for evidence (default: found in the bundle)
    #[arg(long)]
    sairedis_rec: Option<PathBuf>,
    /// syslog, plain or .gz, for evidence (default: found in the bundle)
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
    /// Trace a PortChannel, e.g. PortChannel001
    Lag { name: String },
    /// Trace a route, e.g. 10.1.1.0/24, 10.1.0.32 (host route) or Vrf-red:10.1.1.0/24
    Route { prefix: String },
}

fn run(cli: Cli) -> Result<bool, String> {
    let mut budget = cli.max_input_mb.saturating_mul(1024 * 1024);
    let mut layout: Option<BundleLayout> = None;
    let snap = match &cli.snapshot {
        None => Snapshot::from_dump_command(&cli.db_dump_cmd, cli.asic, &mut budget)?,
        Some(path) => {
            if path.is_dir() {
                layout = Some(BundleLayout::locate(path, cli.asic)?);
            }
            Snapshot::from_path_budget(path, cli.asic, &mut budget)?
        }
    };
    // An explicit log path wins; otherwise use the bundle's log (plain or .gz), if any.
    let log = |explicit: &Option<PathBuf>, name: &str| {
        explicit
            .clone()
            .or_else(|| layout.as_ref().and_then(|l| l.log_file(name)))
    };
    let logs = Logs::load(
        log(&cli.swss_rec, "swss.rec").as_deref(),
        log(&cli.sairedis_rec, "sairedis.rec").as_deref(),
        log(&cli.syslog, "syslog").as_deref(),
        &mut budget,
    )?;

    let report = match &cli.cmd {
        Cmd::Vlan { name } => vlan::trace_with(&snap, &logs, name)?,
        Cmd::Port { name } => port::trace_with(&snap, &logs, name)?,
        Cmd::Lag { name } => lag::trace_with(&snap, &logs, name)?,
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
