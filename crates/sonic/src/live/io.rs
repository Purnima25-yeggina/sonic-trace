//! The I/O side of live mode: `redis-cli` for keyspace notifications and lookups, and
//! followers for log files.
//!
//! `redis-cli` is used instead of a Redis client library, as `sonic-db-dump` is for
//! `--live`: it is on every SONiC switch, and it can be replaced for tests.
use super::{AsicNames, Filter, Input, Lookup, Source, Tracker, clock};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Databases to subscribe to. COUNTERS_DB (2) is left out: it changes every second.
const WATCHED_DBS: [u8; 4] = [4, 0, 6, 1];

/// How often timers are checked and log files are polled.
const TICK: Duration = Duration::from_millis(200);

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A `redis-cli` command line, e.g. `redis-cli` or `redis-cli -s /var/run/redis/redis.sock`.
#[derive(Debug, Clone)]
pub struct RedisCli {
    program: String,
    args: Vec<String>,
}

impl RedisCli {
    pub fn new(cmd: &str) -> Result<Self, String> {
        let mut parts = cmd.split_whitespace().map(String::from);
        let program = parts.next().ok_or("--redis-cli is empty")?;
        Ok(Self {
            program,
            args: parts.collect(),
        })
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        c
    }

    /// Run a one-shot command (`--raw`), returning its output lines.
    fn run(&self, db: u8, args: &[&str]) -> Option<Vec<String>> {
        let out = self
            .command()
            .args(["--raw", "-n", &db.to_string()])
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(String::from)
                .collect(),
        )
    }

    /// `HGETALL key` as (field, value) pairs.
    pub fn hgetall(&self, db: u8, key: &str) -> Vec<(String, String)> {
        let lines = self.run(db, &["hgetall", key]).unwrap_or_default();
        lines
            .chunks(2)
            .filter_map(|c| match c {
                [k, v] => Some((k.clone(), v.clone())),
                _ => None,
            })
            .collect()
    }

    /// The server's `notify-keyspace-events` setting, if it can be read.
    pub fn notify_setting(&self) -> Option<String> {
        let lines = self.run(0, &["config", "get", "notify-keyspace-events"])?;
        lines.get(1).cloned()
    }
}

impl Lookup for RedisCli {
    fn hget(&mut self, db: u8, key: &str, field: &str) -> Option<String> {
        let lines = self.run(db, &["hget", key, field])?;
        lines.first().filter(|v| !v.is_empty()).cloned()
    }

    fn keys(&mut self, db: u8, pattern: &str) -> Vec<String> {
        self.run(db, &["keys", pattern]).unwrap_or_default()
    }
}

/// Split one line of `redis-cli --csv` output. Strings are quoted, with `\"`, `\\`, `\n`,
/// `\r`, `\t`, `\a`, `\b` and `\xHH` escapes (Redis's `sdscatrepr`); numbers are bare.
pub fn parse_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        let mut field = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => field.push('\n'),
                        Some('r') => field.push('\r'),
                        Some('t') => field.push('\t'),
                        Some('a') => field.push('\u{7}'),
                        Some('b') => field.push('\u{8}'),
                        Some('x') => {
                            let hex: String = chars.by_ref().take(2).collect();
                            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                                field.push(char::from(b));
                            }
                        }
                        Some(other) => field.push(other),
                        None => {}
                    },
                    c => field.push(c),
                }
            }
        }
        while let Some(&c) = chars.peek() {
            if c == ',' {
                break;
            }
            field.push(c);
            chars.next();
        }
        out.push(field);
        if chars.next().is_none() {
            break;
        }
    }
    out
}

/// Turns `redis-cli psubscribe` output into notifications. Accepts `--csv` lines
/// (`"pmessage","<pattern>","__keyspace@4__:VLAN|Vlan100","hset"`) and the plain
/// one-element-per-line form, in case `--csv` is not honoured.
#[derive(Debug, Default)]
pub struct PubSubParser {
    plain: Vec<String>,
}

impl PubSubParser {
    pub fn feed(&mut self, line: &str) -> Option<(u8, String, String)> {
        let fields = if line.starts_with('"') {
            parse_csv(line)
        } else {
            // Plain form: "pmessage", pattern, channel, message on four lines.
            if line == "pmessage" {
                self.plain.clear();
            }
            self.plain.push(line.to_string());
            if self.plain.len() < 4 {
                return None;
            }
            std::mem::take(&mut self.plain)
        };
        let [kind, _pattern, channel, op] = fields.as_slice() else {
            return None;
        };
        if kind != "pmessage" {
            return None;
        }
        let rest = channel.strip_prefix("__keyspace@")?;
        let (db, key) = rest.split_once("__:")?;
        Some((db.parse().ok()?, key.to_string(), op.clone()))
    }
}

enum Msg {
    Input(Input),
    SubscriberEnded(String),
}

/// Follows a growing log file, like `tail -F`: starts at the current end, survives the
/// file being rotated or truncated, and waits for it to appear.
struct Follower {
    path: PathBuf,
    source: Source,
    file: Option<(std::fs::File, u64, Option<u64>)>,
    partial: String,
}

#[cfg(unix)]
fn inode(m: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(m.ino())
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> Option<u64> {
    None
}

impl Follower {
    /// Open at the end of the file, so only new lines are reported.
    fn new(path: PathBuf, source: Source) -> Self {
        let mut f = Self {
            path,
            source,
            file: None,
            partial: String::new(),
        };
        f.open(true);
        f
    }

    fn open(&mut self, at_end: bool) {
        self.file = std::fs::File::open(&self.path).ok().and_then(|mut file| {
            let meta = file.metadata().ok()?;
            let pos = if at_end { meta.len() } else { 0 };
            file.seek(SeekFrom::Start(pos)).ok()?;
            Some((file, pos, inode(&meta)))
        });
    }

    /// New complete lines since the last poll.
    fn poll(&mut self) -> Vec<String> {
        // Reopen after rotation (new inode) or truncation (shorter than what was read).
        let current = std::fs::metadata(&self.path).ok();
        let rotated = match (&self.file, &current) {
            (Some((_, pos, ino)), Some(m)) => inode(m) != *ino || m.len() < *pos,
            (None, Some(_)) => true,
            _ => false,
        };
        if rotated {
            self.open(false);
            self.partial.clear();
        }
        let Some((file, pos, _)) = self.file.as_mut() else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        // Cap one poll's read so a burst cannot exhaust memory.
        let n = file.take(16 << 20).read_to_end(&mut buf).unwrap_or(0);
        *pos += n as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = Vec::new();
        while let Some(i) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=i).collect();
            lines.push(line.trim_end_matches(['\n', '\r']).to_string());
        }
        lines
    }
}

/// Options for [`run`].
pub struct Options {
    pub redis_cli: String,
    pub filter: Filter,
    pub stuck_after: Duration,
    pub swss_rec: PathBuf,
    pub sairedis_rec: PathBuf,
    pub syslog: PathBuf,
}

fn note_log(out: &mut dyn Write, path: &Path) -> std::io::Result<()> {
    match std::fs::File::open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => writeln!(
            out,
            "note: cannot read {} (permission denied; run with sudo for log evidence)",
            path.display()
        ),
        Err(_) => writeln!(
            out,
            "note: {} does not exist yet; it will be read once it appears",
            path.display()
        ),
    }
}

/// Follow changes until `redis-cli` stops (normally: until Ctrl+C).
pub fn run(opts: Options, out: &mut dyn Write) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut redis = RedisCli::new(&opts.redis_cli)?;

    match redis.notify_setting() {
        None => {
            return Err(format!(
                "cannot reach Redis with `{}` (is this a SONiC switch? use --redis-cli to change the command)",
                opts.redis_cli
            ));
        }
        Some(s) if !s.contains('K') || !(s.contains('A') || s.contains('h')) => writeln!(
            out,
            "warning: Redis keyspace notifications are not fully enabled (notify-keyspace-events = \"{s}\"); changes may be missed"
        )
        .map_err(io)?,
        Some(_) => {}
    }

    let mut names = AsicNames::default();
    names.insert_map(false, redis.hgetall(2, "COUNTERS_PORT_NAME_MAP"));
    names.insert_map(true, redis.hgetall(2, "COUNTERS_LAG_NAME_MAP"));
    let mut tracker = Tracker::new(
        opts.filter.clone(),
        opts.stuck_after.as_millis() as u64,
        names,
    );

    // Followers record the current end of each log before anything else starts.
    let mut followers: Vec<Follower> = [
        (opts.swss_rec.clone(), Source::SwssRec),
        (opts.sairedis_rec.clone(), Source::SairedisRec),
        (opts.syslog.clone(), Source::Syslog),
    ]
    .into_iter()
    .map(|(p, s)| Follower::new(p, s))
    .collect();
    for f in &followers {
        note_log(out, &f.path).map_err(io)?;
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let mut child = redis
        .command()
        .arg("--csv")
        .arg("psubscribe")
        .args(WATCHED_DBS.iter().map(|d| format!("__keyspace@{d}__:*")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", opts.redis_cli))?;
    let stdout = child.stdout.take().ok_or("redis-cli: no output")?;
    let sub_tx = tx.clone();
    std::thread::spawn(move || {
        let mut parser = PubSubParser::default();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some((db, key, op)) = parser.feed(&line)
                && let Some(source) = Source::from_db_id(db)
                && sub_tx
                    .send(Msg::Input(Input::Db { source, key, op }))
                    .is_err()
            {
                return;
            }
        }
        let _ = sub_tx.send(Msg::SubscriberEnded("its output ended".into()));
    });
    drop(tx);

    writeln!(
        out,
        "following {} — CONFIG_DB, APPL_DB, STATE_DB, ASIC_DB, swss.rec, sairedis.rec, syslog (times UTC; Ctrl+C to stop)",
        opts.filter.describe()
    )
    .map_err(io)?;
    out.flush().map_err(io)?;

    let mut ended: Option<(String, u64)> = None;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Msg::Input(input)) => {
                for l in tracker.on_input(now_ms(), &input, &mut redis) {
                    writeln!(out, "{l}").map_err(io)?;
                }
            }
            Ok(Msg::SubscriberEnded(why)) => ended = Some((why, now_ms())),
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        for f in &mut followers {
            for line in f.poll() {
                let input = Input::Log {
                    source: f.source,
                    line,
                };
                for l in tracker.on_input(now_ms(), &input, &mut redis) {
                    writeln!(out, "{l}").map_err(io)?;
                }
            }
        }
        for l in tracker.on_tick(now_ms()) {
            writeln!(out, "{l}").map_err(io)?;
        }
        out.flush().map_err(io)?;
        // After the subscriber ends, give log lines a moment to arrive, then stop.
        if let Some((why, at)) = &ended
            && now_ms().saturating_sub(*at) > 500
        {
            let status = child.wait().map_err(io)?;
            writeln!(
                out,
                "{}  redis-cli stopped ({why}, {status})",
                clock(now_ms())
            )
            .map_err(io)?;
            return if status.success() {
                Ok(())
            } else {
                Err(format!("redis-cli stopped: {status}"))
            };
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn csv_lines_are_split_with_escapes() {
        let line = r#""pmessage","__keyspace@*__:*","__keyspace@1__:ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"10.0.0.0/8\"}","hset""#;
        let f = parse_csv(line);
        assert_eq!(f.len(), 4);
        assert_eq!(
            f.get(2).unwrap(),
            r#"__keyspace@1__:ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{"dest":"10.0.0.0/8"}"#
        );
        assert_eq!(parse_csv(r#""psubscribe","x",1"#).get(2).unwrap(), "1");
        assert_eq!(parse_csv(r#""a\x41\\b""#).first().unwrap(), "aA\\b");
    }

    #[test]
    fn pubsub_csv_and_plain_forms() {
        let mut p = PubSubParser::default();
        assert_eq!(p.feed(r#""psubscribe","__keyspace@4__:*",1"#), None);
        assert_eq!(
            p.feed(r#""pmessage","__keyspace@4__:*","__keyspace@4__:VLAN|Vlan100","hset""#),
            Some((4, "VLAN|Vlan100".into(), "hset".into()))
        );
        let mut p = PubSubParser::default();
        let mut got = None;
        for l in [
            "pmessage",
            "__keyspace@0__:*",
            "__keyspace@0__:VLAN_TABLE:Vlan100",
            "hset",
        ] {
            got = p.feed(l);
        }
        assert_eq!(got, Some((0, "VLAN_TABLE:Vlan100".into(), "hset".into())));
        // Garbage never panics.
        for l in [
            "",
            "\"",
            "\"pmessage\"",
            "pmessage",
            "x",
            "\"pmessage\",\"a\",\"__keyspace@x__:k\",\"del\"",
        ] {
            let _ = p.feed(l);
        }
    }

    #[test]
    fn follower_reads_new_lines_and_survives_rotation() {
        let d = std::env::temp_dir().join(format!("sonic-trace-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("syslog");
        std::fs::write(&p, "old line\n").unwrap();
        let mut f = Follower::new(p.clone(), Source::Syslog);
        assert!(f.poll().is_empty(), "existing content is skipped");
        let mut fh = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        fh.write_all(b"new 1\npart").unwrap();
        assert_eq!(f.poll(), vec!["new 1".to_string()]);
        fh.write_all(b"ial\n").unwrap();
        assert_eq!(f.poll(), vec!["partial".to_string()]);
        // Rotate: move away and create a fresh file.
        std::fs::rename(&p, d.join("syslog.1")).unwrap();
        std::fs::write(&p, "after rotate\n").unwrap();
        assert_eq!(f.poll(), vec!["after rotate".to_string()]);
        std::fs::remove_dir_all(&d).ok();
    }
}
