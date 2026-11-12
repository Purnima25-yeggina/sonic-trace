//! Evidence from orchagent's recordings and syslog.
//!
//! Formats (from sonic-swss and sonic-sairedis):
//! - `swss.rec`:     `<ts>|<TABLE:key>|<OP>|<field>:<value>|...`
//! - `sairedis.rec`, single calls: `<ts>|<c|r|s>|<SAI_OBJECT_TYPE_X:key>|<ATTR>=<value>|...`
//! - `sairedis.rec`, bulk calls:   `<ts>|<C|R|S>|<SAI_OBJECT_TYPE_X>||<key>|<ATTR>=<value>|...||<key>|...`
//!   (routeorch creates routes through the bulk API, so routes usually appear here)
//!
//! `generate_dump` stores logs gzip-compressed (`log/swss.rec.gz`); those are
//! decompressed transparently. Rotated copies (`swss.rec.1.gz`, ...) are read too, oldest
//! first, so a "no record" result covers the whole bundle, and says which window it covers.
//!
//! `sairedis.rec` does not record a status for single create calls, so it can show
//! that a SAI call was made, not that it succeeded.
use std::io::Read;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Logs {
    swss: Option<LogText>,
    sairedis: Option<LogText>,
    syslog: Option<LogText>,
}

/// One log's text, with its rotations joined oldest first, and what was read.
#[derive(Debug, Default)]
struct LogText {
    text: String,
    /// Files read, including rotations.
    files: usize,
    /// Older rotations left unread because of the size limit.
    skipped: usize,
}

impl LogText {
    fn single(text: String) -> Self {
        Self {
            text,
            files: 1,
            skipped: 0,
        }
    }

    /// What a "not found" result covers, e.g. `4 files from 2026-01-01.10:00:00`.
    /// `.rec` lines start with their timestamp; syslog's is not parsed.
    fn coverage(&self, with_ts: bool) -> String {
        let mut s = plural(self.files, "file");
        if with_ts
            && let Some(ts) = self
                .text
                .lines()
                .find_map(|l| l.split('|').next().filter(|t| !t.is_empty()))
        {
            s.push_str(&format!(" from {ts}"));
        }
        if self.skipped > 0 {
            s.push_str(&format!(
                "; {} not read (size limit, raise --max-input-mb)",
                plural(self.skipped, "older rotation")
            ));
        }
        s
    }
}

/// Where to read one log from: the current file and its rotations, newest first
/// (`.1`, `.2`, ...), as [`crate::snapshot::BundleLayout::log_rotations`] returns them.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogSource<'a> {
    pub current: Option<&'a Path>,
    pub rotations: &'a [std::path::PathBuf],
}

impl<'a> LogSource<'a> {
    pub fn file(current: Option<&'a Path>) -> Self {
        Self {
            current,
            rotations: &[],
        }
    }
}

/// Read one log. The current file must fit in `budget`; rotations are read newest first
/// while they fit in `rotation_budget` (and `budget`), and the rest are counted as skipped.
fn read_source(
    src: LogSource,
    budget: &mut u64,
    rotation_budget: u64,
) -> Result<Option<LogText>, String> {
    let mut parts = Vec::new();
    if let Some(p) = src.current {
        parts.push(read_capped(p, budget)?);
    }
    let mut share = rotation_budget.min(*budget);
    let mut skipped = 0;
    for (i, p) in src.rotations.iter().enumerate() {
        let before = share;
        match read_capped(p, &mut share) {
            Ok(t) => {
                *budget -= before - share;
                parts.push(t);
            }
            Err(_) => {
                skipped = src.rotations.len() - i;
                break;
            }
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let files = parts.len();
    // Current first, then .1, .2, ...: reverse to get chronological order.
    parts.reverse();
    let mut text = String::new();
    for t in parts {
        text.push_str(&t);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(Some(LogText {
        text,
        files,
        skipped,
    }))
}

/// Longest syslog message shown in evidence, in characters.
const SYSLOG_MAX: usize = 200;

/// Make a syslog line readable: rsyslog escapes newlines as `#012`, and some daemons log
/// whole JSON documents on one line. Returns (key for de-duplication, display text). The
/// header (`<time> <host> <LEVEL> proc[pid]: `) is kept; a long message is shortened,
/// starting at its beginning when the first matching token is near it, else around it.
/// The key is the shortened message without the header, so repeats differing only in
/// time or pid collapse into one.
pub(crate) fn tidy_syslog(line: &str, tokens: &[&str]) -> (String, String) {
    let flat = line.replace("#012", " ").replace("#011", " ");
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let (head, msg) = match flat
        .find("]: ")
        .map(|i| i + 3)
        .or_else(|| flat.find(": ").map(|i| i + 2))
    {
        Some(i) => flat.split_at(i),
        None => ("", flat.as_str()),
    };
    let chars: Vec<char> = msg.chars().collect();
    let shown = if chars.len() <= SYSLOG_MAX {
        msg.to_string()
    } else {
        // Character position of the first token, so the window shows why it matched.
        let at = tokens
            .iter()
            .filter_map(|t| msg.find(t))
            .min()
            .map_or(0, |b| msg.get(..b).map_or(0, |s| s.chars().count()));
        let start = if at + 40 <= SYSLOG_MAX {
            0
        } else {
            at.saturating_sub(SYSLOG_MAX / 3)
        };
        let end = (start + SYSLOG_MAX).min(chars.len());
        let start = end.saturating_sub(SYSLOG_MAX);
        let mut s: String = chars.get(start..end).unwrap_or_default().iter().collect();
        if start > 0 {
            s.insert(0, '…');
        }
        if end < chars.len() {
            s.push('…');
        }
        s
    };
    let shown_head = format!("{head}{shown}");
    (shown, shown_head)
}

/// One SAI call for one object, from a single or a bulk `sairedis.rec` line.
pub struct SaiRec<'a> {
    pub ts: &'a str,
    /// `c`, `r` or `s` (create, remove, set); bulk calls are normalized to these.
    pub op: char,
    /// `SAI_OBJECT_TYPE_X:<key>`, the same shape for single and bulk calls.
    pub key: String,
    pub attrs: Vec<&'a str>,
    pub bulk: bool,
}

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Read a log file, decompressing gzip, without letting more than `budget` bytes
/// (compressed read plus decompressed output) into memory.
fn read_capped(path: &Path, budget: &mut u64) -> Result<String, String> {
    let too_big = || {
        format!(
            "{}: input exceeds the size limit (raise it with --max-input-mb)",
            path.display()
        )
    };
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut raw = Vec::new();
    let n = file
        .take(budget.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|e| format!("{}: {e}", path.display()))? as u64;
    if n > *budget {
        return Err(too_big());
    }
    *budget -= n;
    let bytes = if raw.starts_with(&GZIP_MAGIC) {
        let mut out = Vec::new();
        let m = flate2::read::MultiGzDecoder::new(raw.as_slice())
            .take(budget.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(|e| format!("{}: not a valid gzip file: {e}", path.display()))?
            as u64;
        if m > *budget {
            return Err(too_big());
        }
        *budget -= m;
        out
    } else {
        raw
    };
    // Logs can contain stray bytes; do not fail on them.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

pub(crate) fn has_word(line: &str, token: &str) -> bool {
    line.match_indices(token).any(|(i, _)| {
        let before = line.get(..i).and_then(|s| s.chars().next_back());
        let after = line.get(i + token.len()..).and_then(|s| s.chars().next());
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        edge(before) && edge(after)
    })
}

/// Parse one `sairedis.rec` line into the per-object calls it contains.
pub(crate) fn parse_sairedis_line(line: &str) -> Vec<SaiRec<'_>> {
    let mut head = line.splitn(3, '|');
    let (Some(ts), Some(op), Some(rest)) = (head.next(), head.next(), head.next()) else {
        return Vec::new();
    };
    match op {
        "c" | "r" | "s" => {
            let mut it = rest.split('|');
            let Some(key) = it.next() else {
                return Vec::new();
            };
            let op = op.chars().next().unwrap_or('c');
            vec![SaiRec {
                ts,
                op,
                key: key.to_string(),
                attrs: it.collect(),
                bulk: false,
            }]
        }
        "C" | "R" | "S" => {
            // <OBJECT_TYPE>||<key>|<attrs...>||<key>|<attrs...>
            let mut entries = rest.split("||");
            let Some(object_type) = entries.next() else {
                return Vec::new();
            };
            let op = op.to_ascii_lowercase().chars().next().unwrap_or('c');
            entries
                .filter_map(|entry| {
                    let mut it = entry.split('|');
                    let key = it.next().filter(|k| !k.is_empty())?;
                    Some(SaiRec {
                        ts,
                        op,
                        key: format!("{object_type}:{key}"),
                        attrs: it.collect(),
                        bulk: true,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

impl Logs {
    pub fn load(
        swss: Option<&Path>,
        sairedis: Option<&Path>,
        syslog: Option<&Path>,
        budget: &mut u64,
    ) -> Result<Self, String> {
        Self::load_sources(
            LogSource::file(swss),
            LogSource::file(sairedis),
            LogSource::file(syslog),
            budget,
        )
    }

    /// Like [`Logs::load`], also reading rotated logs. The current files must fit in
    /// `budget`; each log's rotations get an equal share of what is left after them.
    pub fn load_sources(
        swss: LogSource,
        sairedis: LogSource,
        syslog: LogSource,
        budget: &mut u64,
    ) -> Result<Self, String> {
        fn current_only(s: LogSource<'_>) -> LogSource<'_> {
            LogSource::file(s.current)
        }
        let mut logs = Self {
            swss: read_source(current_only(swss), budget, 0)?,
            sairedis: read_source(current_only(sairedis), budget, 0)?,
            syslog: read_source(current_only(syslog), budget, 0)?,
        };
        let share = *budget / 3;
        for (slot, src) in [
            (&mut logs.swss, swss),
            (&mut logs.sairedis, sairedis),
            (&mut logs.syslog, syslog),
        ] {
            if src.rotations.is_empty() {
                continue;
            }
            let older = LogSource {
                current: None,
                rotations: src.rotations,
            };
            if let Some(old) = read_source(older, budget, share)? {
                let cur = slot.take();
                let mut text = old.text;
                if let Some(c) = &cur {
                    text.push_str(&c.text);
                }
                *slot = Some(LogText {
                    text,
                    files: old.files + cur.as_ref().map_or(0, |c| c.files),
                    skipped: old.skipped,
                });
            } else if let Some(c) = slot.as_mut() {
                c.skipped = src.rotations.len();
            }
        }
        Ok(logs)
    }

    /// Build from in-memory bytes (used by fuzzing and tests); invalid UTF-8 is replaced.
    pub fn from_bytes(swss: Option<&[u8]>, sairedis: Option<&[u8]>, syslog: Option<&[u8]>) -> Self {
        let text =
            |b: Option<&[u8]>| b.map(|b| LogText::single(String::from_utf8_lossy(b).into_owned()));
        Self {
            swss: text(swss),
            sairedis: text(sairedis),
            syslog: text(syslog),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.swss.is_none() && self.sairedis.is_none() && self.syslog.is_none()
    }

    /// Evidence lines for one object.
    /// - `swss_keys`: the APPL_DB-style keys orchagent consumes, e.g. `VLAN_TABLE:Vlan8`
    ///   (several when the same object can be written in more than one form)
    /// - `sai_match`: selects the `sairedis.rec` calls that belong to the object, or `None`
    ///   when the object cannot be identified there (the sairedis line is then skipped)
    /// - `tokens`: names to look for in syslog (whole words only)
    pub fn evidence(
        &self,
        swss_keys: &[&str],
        sai_match: Option<&dyn Fn(&SaiRec) -> bool>,
        tokens: &[&str],
    ) -> Vec<String> {
        let mut out = Vec::new();
        let shown = swss_keys.first().copied().unwrap_or("");

        if let Some(log) = &self.swss {
            let events: Vec<(&str, &str, &str)> = log
                .text
                .lines()
                .filter_map(|l| {
                    let mut it = l.split('|');
                    let (ts, key, op) = (it.next()?, it.next()?, it.next()?);
                    swss_keys.contains(&key).then_some((ts, key, op))
                })
                .collect();
            out.push(match events.last() {
                None => format!(
                    "swss.rec: no record of orchagent receiving {shown} (checked {})",
                    log.coverage(true)
                ),
                Some((ts, key, op)) => format!(
                    "swss.rec: orchagent last received {op} for {key} at {ts} ({})",
                    plural(events.len(), "record")
                ),
            });
        }

        if let (Some(log), Some(sai_match)) = (&self.sairedis, sai_match) {
            let events: Vec<SaiRec> = log
                .text
                .lines()
                .flat_map(parse_sairedis_line)
                .filter(|r| sai_match(r))
                .collect();
            out.push(match events.last() {
                None => format!(
                    "sairedis.rec: no SAI call recorded for this object (checked {})",
                    log.coverage(true)
                ),
                Some(r) => {
                    let what = match r.op {
                        'c' => "create",
                        'r' => "remove",
                        _ => "set",
                    };
                    let kind = if r.bulk { " (bulk)" } else { "" };
                    format!(
                        "sairedis.rec: last SAI {what}{kind} at {} ({}; the log records no result)",
                        r.ts,
                        plural(events.len(), "call")
                    )
                }
            });
        }

        if let Some(log) = &self.syslog {
            // Distinct messages, in order of their last appearance, with a repeat count.
            let mut seen: Vec<(String, String, usize)> = Vec::new();
            for l in log.text.lines().filter(|l| {
                [" ERR ", " ERROR ", " WARNING ", " CRIT "]
                    .iter()
                    .any(|s| l.contains(s))
                    && tokens.iter().any(|t| has_word(l, t))
            }) {
                let (msg, shown) = tidy_syslog(l.trim(), tokens);
                let count = match seen.iter().position(|(m, _, _)| *m == msg) {
                    Some(i) => seen.remove(i).2 + 1,
                    None => 1,
                };
                seen.push((msg, shown, count));
            }
            let skip = seen.len().saturating_sub(3);
            out.extend(seen.iter().skip(skip).map(|(_, shown, n)| {
                if *n > 1 {
                    format!("syslog: {shown} (seen {n} times; last shown)")
                } else {
                    format!("syslog: {shown}")
                }
            }));
        }
        out
    }
}
