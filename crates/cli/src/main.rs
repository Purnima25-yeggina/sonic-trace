use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use trace_sonic::{
    archive, check, lag, live,
    logs::{LogSource, Logs},
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
    /// Offline input: a generate_dump archive (sonic_dump_*.tar.gz) or unpacked bundle
    /// (or its dump/ folder), a folder of per-database dumps, or one multi-database JSON file
    #[arg(long, conflicts_with = "live")]
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
    /// Trace a VLAN, e.g. Vlan100, or `all` for every VLAN in the snapshot
    Vlan { name: String },
    /// Trace a front-panel port, e.g. Ethernet0, or `all`
    Port { name: String },
    /// Trace a PortChannel, e.g. PortChannel001, or `all`
    Lag { name: String },
    /// Trace a route, e.g. 10.1.1.0/24, 10.1.0.32 (host route) or Vrf-red:10.1.1.0/24, or `all`
    Route { prefix: String },
    /// On a switch: follow changes live, from CONFIG_DB to the ASIC, as you configure.
    /// Give kinds (vlan, port, lag, route) and/or names (Vlan100, 10.1.1.0/24); none means all
    Live {
        targets: Vec<String>,
        /// Warn when a change has not reached the ASIC after this many seconds
        #[arg(long, default_value_t = 5)]
        stuck_after: u64,
        /// Command used to reach Redis
        #[arg(long, default_value = "redis-cli")]
        redis_cli: String,
    },
    /// Trace every VLAN, port, LAG and route in the snapshot; list what passed and failed
    Check {
        /// Also print the full trace of each failed object
        #[arg(long)]
        details: bool,
    },
}

fn run(cli: Cli) -> Result<bool, String> {
    if let Cmd::Live {
        targets,
        stuck_after,
        redis_cli,
    } = &cli.cmd
    {
        if cli.snapshot.is_some() {
            return Err("live follows a running switch; it does not take --snapshot".into());
        }
        let log = |p: &Option<PathBuf>, default: &str| {
            p.clone().unwrap_or_else(|| PathBuf::from(default))
        };
        let opts = live::io::Options {
            redis_cli: redis_cli.clone(),
            filter: live::Filter::parse(targets)?,
            stuck_after: std::time::Duration::from_secs(*stuck_after),
            swss_rec: log(&cli.swss_rec, "/var/log/swss/swss.rec"),
            sairedis_rec: log(&cli.sairedis_rec, "/var/log/swss/sairedis.rec"),
            syslog: log(&cli.syslog, "/var/log/syslog"),
        };
        live::io::run(opts, &mut std::io::stdout().lock())?;
        return Ok(true);
    }
    if cli.snapshot.is_none() && !cli.live {
        return Err(
            "give --snapshot <dump> (a sonic_dump_*.tar.gz or folder), or --live on a switch"
                .into(),
        );
    }
    let mut budget = cli.max_input_mb.saturating_mul(1024 * 1024);
    let mut layout: Option<BundleLayout> = None;
    // A .tar.gz bundle is unpacked (dumps and logs only) into a private temporary folder,
    // removed when `_unpacked` goes out of scope at the end of this function.
    let unpacked = match &cli.snapshot {
        Some(p) if archive::is_archive(p) => Some(archive::extract_bundle(p, budget)?),
        _ => None,
    };
    if let Some(u) = &unpacked
        && u.skipped_logs > 0
    {
        eprintln!(
            "note: {} log files in the archive were not read (size limit, raise --max-input-mb)",
            u.skipped_logs
        );
    }
    let input = unpacked
        .as_ref()
        .map(|u| u.path().to_path_buf())
        .or_else(|| cli.snapshot.clone());
    let _unpacked = unpacked;
    let snap = match &input {
        None => Snapshot::from_dump_command(&cli.db_dump_cmd, cli.asic, &mut budget)?,
        Some(path) => {
            if path.is_dir() {
                layout = Some(BundleLayout::locate(path, cli.asic)?);
            }
            Snapshot::from_path_budget(path, cli.asic, &mut budget)?
        }
    };
    // An explicit log path wins and is read alone; otherwise use the bundle's log (plain
    // or .gz) and its rotations (.1, .2, ...), if any.
    let log = |explicit: &Option<PathBuf>, name: &str| match explicit {
        Some(p) => (Some(p.clone()), Vec::new()),
        None => layout.as_ref().map_or((None, Vec::new()), |l| {
            (l.log_file(name), l.log_rotations(name))
        }),
    };
    let (swss, sairedis, syslog) = (
        log(&cli.swss_rec, "swss.rec"),
        log(&cli.sairedis_rec, "sairedis.rec"),
        log(&cli.syslog, "syslog"),
    );
    fn src((cur, rot): &(Option<PathBuf>, Vec<PathBuf>)) -> LogSource<'_> {
        LogSource {
            current: cur.as_deref(),
            rotations: rot,
        }
    }
    let logs = Logs::load_sources(src(&swss), src(&sairedis), src(&syslog), &mut budget)?;

    if let Cmd::Check { details } = cli.cmd {
        let result = check::run(&snap, &logs);
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
        } else {
            print!("{}", result.render_text(details));
        }
        return Ok(result.all_passed());
    }
    // `<kind> all`: every object of that kind, full trace each, then a count.
    let all_of = match &cli.cmd {
        Cmd::Vlan { name } if name == "all" => Some(check::Kind::Vlan),
        Cmd::Port { name } if name == "all" => Some(check::Kind::Port),
        Cmd::Lag { name } if name == "all" => Some(check::Kind::Lag),
        Cmd::Route { prefix } if prefix == "all" => Some(check::Kind::Route),
        _ => None,
    };
    if let Some(kind) = all_of {
        let result = check::run_kind(&snap, &logs, Some(kind));
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
        } else {
            print!("{}", result.render_all(kind));
        }
        return Ok(result.all_passed());
    }
    let report = match &cli.cmd {
        Cmd::Vlan { name } => vlan::trace_with(&snap, &logs, name)?,
        Cmd::Port { name } => port::trace_with(&snap, &logs, name)?,
        Cmd::Lag { name } => lag::trace_with(&snap, &logs, name)?,
        Cmd::Route { prefix } => route::trace_with(&snap, &logs, prefix)?,
        Cmd::Check { .. } | Cmd::Live { .. } => {
            return Err("unreachable: handled above".into());
        }
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
