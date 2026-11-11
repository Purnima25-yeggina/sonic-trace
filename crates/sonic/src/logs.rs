//! Evidence from orchagent's recordings and syslog.
//!
//! Formats (from sonic-swss and sonic-sairedis):
//! - `swss.rec`:     `<ts>|<TABLE:key>|<OP>|<field>:<value>|...`
//! - `sairedis.rec`: `<ts>|<c|r|s>|<SAI_OBJECT_TYPE_X:key>|<ATTR>=<value>|...`
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

/// One parsed `sairedis.rec` line.
pub struct SaiRec<'a> {
    pub ts: &'a str,
    pub op: &'a str,
    pub key: &'a str,
    pub attrs: Vec<&'a str>,
}

fn read_capped(path: &Path, budget: &mut u64) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    let n = file
        .take(budget.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))? as u64;
    if n > *budget {
        return Err(format!(
            "{}: input exceeds the size limit (raise it with --max-input-mb)",
            path.display()
        ));
    }
    *budget -= n;
    // Logs can contain stray bytes; do not fail on them.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn has_word(line: &str, token: &str) -> bool {
    line.match_indices(token).any(|(i, _)| {
        let before = line.get(..i).and_then(|s| s.chars().next_back());
        let after = line.get(i + token.len()..).and_then(|s| s.chars().next());
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        edge(before) && edge(after)
    })
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
    /// - `swss_key`: the APPL_DB-style key orchagent consumes, e.g. `VLAN_TABLE:Vlan8`
    /// - `sai_match`: selects the `sairedis.rec` lines that belong to the object, or `None`
    ///   when the object cannot be identified there (the sairedis line is then skipped)
    /// - `tokens`: names to look for in syslog (whole words only)
    pub fn evidence(
        &self,
        swss_key: &str,
        sai_match: Option<&dyn Fn(&SaiRec) -> bool>,
        tokens: &[&str],
    ) -> Vec<String> {
        let mut out = Vec::new();

        if let Some(text) = &self.swss {
            let events: Vec<(&str, &str)> = text
                .lines()
                .filter_map(|l| {
                    let mut it = l.split('|');
                    let (ts, key, op) = (it.next()?, it.next()?, it.next()?);
                    (key == swss_key).then_some((ts, op))
                })
                .collect();
            out.push(match events.last() {
                None => format!("swss.rec: no record of orchagent receiving {swss_key}"),
                Some((ts, op)) => format!(
                    "swss.rec: orchagent last received {op} for {swss_key} at {ts} ({} records)",
                    events.len()
                ),
            });
        }

        if let (Some(text), Some(sai_match)) = (&self.sairedis, sai_match) {
            let events: Vec<SaiRec> = text
                .lines()
                .filter_map(|l| {
                    let mut it = l.split('|');
                    let (ts, op, key) = (it.next()?, it.next()?, it.next()?);
                    Some(SaiRec {
                        ts,
                        op,
                        key,
                        attrs: it.collect(),
                    })
                })
                .filter(|r| matches!(r.op, "c" | "r" | "s") && sai_match(r))
                .collect();
            out.push(match events.last() {
                None => "sairedis.rec: no SAI call recorded for this object".to_string(),
                Some(r) => {
                    let what = match r.op {
                        "c" => "create",
                        "r" => "remove",
                        _ => "set",
                    };
                    format!(
                        "sairedis.rec: last SAI {what} at {} ({} calls; the log records no result)",
                        r.ts,
                        events.len()
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
