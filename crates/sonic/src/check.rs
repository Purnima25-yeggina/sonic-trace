//! Trace every object in a snapshot and sort the results into passed and failed.
//!
//! Objects come from the databases themselves:
//! - VLANs, ports and LAGs: `VLAN|`, `PORT|` and `PORTCHANNEL|` keys in CONFIG_DB
//! - routes: `ROUTE_TABLE:` keys in APPL_DB and `STATIC_ROUTE|` keys in CONFIG_DB
//!
//! The same route can be found under two names (a static route and its APPL_DB entry, a
//! host route with and without its mask); it is traced once.
use crate::logs::Logs;
use crate::snapshot::Snapshot;
use crate::{lag, port, route, vlan};
use serde::Serialize;
use std::collections::BTreeSet;
use trace_core::Report;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Vlan,
    Port,
    Lag,
    Route,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Vlan, Kind::Port, Kind::Lag, Kind::Route];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Vlan => "vlan",
            Kind::Port => "port",
            Kind::Lag => "lag",
            Kind::Route => "route",
        }
    }
}

/// The result for one object.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub kind: Kind,
    /// The object as traced, e.g. `Vlan10` or `10.1.0.32/32`.
    pub object: String,
    pub status: Status,
    /// The full trace; absent when the object could not be traced at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    /// Why the object could not be traced (only for [`Status::Error`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    /// The name in the database could not be traced (e.g. an unexpected key format).
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub entries: Vec<Entry>,
}

impl CheckReport {
    pub fn count(&self, kind: Option<Kind>, status: Status) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == status && kind.is_none_or(|k| e.kind == k))
            .count()
    }

    pub fn all_passed(&self) -> bool {
        self.entries.iter().all(|e| e.status == Status::Passed)
    }

    /// Text report: failed objects (with where they stopped and why), then passed
    /// objects, then a table of counts. One object per line, so it can be grepped.
    pub fn render_text(&self, details: bool) -> String {
        let width = self
            .entries
            .iter()
            .map(|e| e.object.chars().count())
            .max()
            .unwrap_or(0)
            .max(6);
        let line = |e: &Entry, tag: &str| {
            let why = match (&e.report, &e.error) {
                (Some(r), _) => match &r.diagnosis.stopped_at {
                    Some(stage) => format!("stopped at {stage}: {}", r.diagnosis.message),
                    None => r.diagnosis.message.clone(),
                },
                (None, Some(err)) => err.clone(),
                (None, None) => String::new(),
            };
            format!("{tag} {:<5} {:<width$}  {why}\n", e.kind.name(), e.object)
        };
        let mut out = String::new();
        for (status, title, tag) in [
            (Status::Failed, "FAILED", "FAIL"),
            (Status::Error, "COULD NOT TRACE", "ERR "),
            (Status::Passed, "PASSED", "PASS"),
        ] {
            let list: Vec<&Entry> = self.entries.iter().filter(|e| e.status == status).collect();
            if list.is_empty() {
                continue;
            }
            out.push_str(&format!("{title} ({})\n", list.len()));
            for e in list {
                out.push_str(&line(e, tag));
                if details
                    && status == Status::Failed
                    && let Some(r) = &e.report
                {
                    for l in r.render_text().lines() {
                        out.push_str(&format!("       | {l}\n"));
                    }
                }
            }
            out.push('\n');
        }
        out.push_str("SUMMARY\n");
        out.push_str("  type    total  passed  failed  error\n");
        for kind in Kind::ALL {
            let total = self.entries.iter().filter(|e| e.kind == kind).count();
            out.push_str(&format!(
                "  {:<6} {:>6} {:>7} {:>7} {:>6}\n",
                kind.name(),
                total,
                self.count(Some(kind), Status::Passed),
                self.count(Some(kind), Status::Failed),
                self.count(Some(kind), Status::Error),
            ));
        }
        out.push_str(&format!(
            "  {:<6} {:>6} {:>7} {:>7} {:>6}\n",
            "all",
            self.entries.len(),
            self.count(None, Status::Passed),
            self.count(None, Status::Failed),
            self.count(None, Status::Error),
        ));
        out.push_str(if self.all_passed() {
            "=> all objects passed\n"
        } else {
            "=> some objects did not pass; trace one for details, e.g. sonic-trace --snapshot <bundle> route <prefix>\n"
        });
        out
    }
}

/// Keys of `db` that start with `prefix`, with the prefix removed.
fn names(snap: &Snapshot, db: &str, prefix: &str) -> Vec<String> {
    snap.iter(db)
        .filter_map(|(k, _)| k.strip_prefix(prefix).map(String::from))
        .collect()
}

/// The objects to check, in a stable order.
pub fn objects(snap: &Snapshot) -> Vec<(Kind, String)> {
    let mut out: BTreeSet<(Kind, String)> = BTreeSet::new();
    let plain = |n: &String| !n.contains('|') && !n.contains(':');
    out.extend(
        names(snap, "CONFIG_DB", "VLAN|")
            .into_iter()
            .filter(plain)
            .map(|n| (Kind::Vlan, n)),
    );
    out.extend(
        names(snap, "CONFIG_DB", "PORT|")
            .into_iter()
            .filter(plain)
            .map(|n| (Kind::Port, n)),
    );
    out.extend(
        names(snap, "CONFIG_DB", "PORTCHANNEL|")
            .into_iter()
            .filter(plain)
            .map(|n| (Kind::Lag, n)),
    );
    out.extend(
        names(snap, "APPL_DB", "ROUTE_TABLE:")
            .into_iter()
            .map(|n| (Kind::Route, n)),
    );
    // STATIC_ROUTE|<prefix> or STATIC_ROUTE|<vrf>|<prefix>; "default" is the default VRF.
    out.extend(
        names(snap, "CONFIG_DB", "STATIC_ROUTE|")
            .into_iter()
            .map(|n| match n.split_once('|') {
                Some(("default", p)) => p.to_string(),
                Some((vrf, p)) => format!("{vrf}:{p}"),
                None => n,
            })
            .map(|n| (Kind::Route, n)),
    );
    out.into_iter().collect()
}

fn trace_one(snap: &Snapshot, logs: &Logs, kind: Kind, name: &str) -> Result<Report, String> {
    match kind {
        Kind::Vlan => vlan::trace_with(snap, logs, name),
        Kind::Port => port::trace_with(snap, logs, name),
        Kind::Lag => lag::trace_with(snap, logs, name),
        Kind::Route => route::trace_with(snap, logs, name),
    }
}

/// Trace every object in the snapshot. Log evidence is only gathered for objects that
/// stopped at a stage, as in a single trace.
pub fn run(snap: &Snapshot, logs: &Logs) -> CheckReport {
    let mut seen: BTreeSet<(Kind, String)> = BTreeSet::new();
    let mut entries = Vec::new();
    for (kind, name) in objects(snap) {
        let entry = match trace_one(snap, logs, kind, &name) {
            Ok(r) => Entry {
                kind,
                object: r.object.clone(),
                status: if r.diagnosis.stopped_at.is_none() {
                    Status::Passed
                } else {
                    Status::Failed
                },
                report: Some(r),
                error: None,
            },
            Err(e) => Entry {
                kind,
                object: name,
                status: Status::Error,
                report: None,
                error: Some(e),
            },
        };
        if seen.insert((kind, entry.object.clone())) {
            entries.push(entry);
        }
    }
    CheckReport { entries }
}
