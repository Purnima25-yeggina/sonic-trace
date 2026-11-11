//! Evidence from orchagent's recordings and syslog.
//!
//! Formats (from sonic-swss and sonic-sairedis):
//! - `swss.rec`:     `<ts>|<TABLE:key>|<OP>|<field>:<value>|...`
//! - `sairedis.rec`, single calls: `<ts>|<c|r|s>|<SAI_OBJECT_TYPE_X:key>|<ATTR>=<value>|...`
//! - `sairedis.rec`, bulk calls:   `<ts>|<C|R|S>|<SAI_OBJECT_TYPE_X>||<key>|<ATTR>=<value>|...||<key>|...`
//!   (routeorch creates routes through the bulk API, so routes usually appear here)
//!
//! `generate_dump` stores logs gzip-compressed (`log/swss.rec.gz`); those are
//! decompressed transparently.
//!
//! `sairedis.rec` does not record a status for single create calls, so it can show
//! that a SAI call was made, not that it succeeded.
use std::io::Read;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Logs {
    swss: Option<String>,
    sairedis: Option<String>,
    syslog: Option<String>,
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

fn has_word(line: &str, token: &str) -> bool {
    line.match_indices(token).any(|(i, _)| {
        let before = line.get(..i).and_then(|s| s.chars().next_back());
        let after = line.get(i + token.len()..).and_then(|s| s.chars().next());
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        edge(before) && edge(after)
    })
}

/// Parse one `sairedis.rec` line into the per-object calls it contains.
fn parse_sairedis_line(line: &str) -> Vec<SaiRec<'_>> {
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
        let mut read = |p: Option<&Path>| p.map(|p| read_capped(p, budget)).transpose();
        Ok(Self {
            swss: read(swss)?,
            sairedis: read(sairedis)?,
            syslog: read(syslog)?,
        })
    }

    /// Build from in-memory bytes (used by fuzzing and tests); invalid UTF-8 is replaced.
    pub fn from_bytes(swss: Option<&[u8]>, sairedis: Option<&[u8]>, syslog: Option<&[u8]>) -> Self {
        let text = |b: Option<&[u8]>| b.map(|b| String::from_utf8_lossy(b).into_owned());
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

        if let Some(text) = &self.swss {
            let events: Vec<(&str, &str, &str)> = text
                .lines()
                .filter_map(|l| {
                    let mut it = l.split('|');
                    let (ts, key, op) = (it.next()?, it.next()?, it.next()?);
                    swss_keys.contains(&key).then_some((ts, key, op))
                })
                .collect();
            out.push(match events.last() {
                None => format!("swss.rec: no record of orchagent receiving {shown}"),
                Some((ts, key, op)) => format!(
                    "swss.rec: orchagent last received {op} for {key} at {ts} ({})",
                    plural(events.len(), "record")
                ),
            });
        }

        if let (Some(text), Some(sai_match)) = (&self.sairedis, sai_match) {
            let events: Vec<SaiRec> = text
                .lines()
                .flat_map(parse_sairedis_line)
                .filter(|r| sai_match(r))
                .collect();
            out.push(match events.last() {
                None => "sairedis.rec: no SAI call recorded for this object".to_string(),
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

        if let Some(text) = &self.syslog {
            let hits: Vec<&str> = text
                .lines()
                .filter(|l| {
                    [" ERR ", " ERROR ", " WARNING ", " CRIT "]
                        .iter()
                        .any(|s| l.contains(s))
                        && tokens.iter().any(|t| has_word(l, t))
                })
                .collect();
            let skip = hits.len().saturating_sub(3);
            out.extend(
                hits.iter()
                    .skip(skip)
                    .map(|l| format!("syslog: {}", l.trim())),
            );
        }
        out
    }
}
