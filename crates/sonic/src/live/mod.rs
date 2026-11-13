//! Live mode: follow changes as they happen, from configuration to the ASIC.
//!
//! Sources (see [`io`] for how they are read):
//! - Redis keyspace notifications for CONFIG_DB (4), APPL_DB (0), STATE_DB (6) and
//!   ASIC_DB (1). SONiC's database container enables them (`notify-keyspace-events AKE`).
//! - `swss.rec` (what orchagent received), `sairedis.rec` (SAI calls) and syslog.
//!
//! Every change is mapped to an object (VLAN, port, LAG or route) and printed as one line.
//! A change in CONFIG_DB or APPL_DB starts a timer for that object; the first ASIC_DB
//! change for it stops the timer ("reached the ASIC in 92 ms"). If nothing reaches the
//! ASIC within the limit, a warning is printed.
//!
//! This module is pure: no I/O, no clocks. Times are passed in as milliseconds since the
//! Unix epoch, and ASIC_DB lookups go through the [`Lookup`] trait, so it can be tested.
pub mod io;

use crate::check::Kind;
use crate::logs::{has_word, parse_sairedis_line, tidy_syslog};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

/// Where a change was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Config,
    Appl,
    State,
    Asic,
    SwssRec,
    SairedisRec,
    Syslog,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Config => "CONFIG_DB",
            Source::Appl => "APPL_DB",
            Source::State => "STATE_DB",
            Source::Asic => "ASIC_DB",
            Source::SwssRec => "swss.rec",
            Source::SairedisRec => "sairedis.rec",
            Source::Syslog => "syslog",
        }
    }

    /// The Redis database number SONiC uses for this source, if it is a database.
    pub fn db_id(self) -> Option<u8> {
        match self {
            Source::Appl => Some(0),
            Source::Asic => Some(1),
            Source::Config => Some(4),
            Source::State => Some(6),
            _ => None,
        }
    }

    pub fn from_db_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Source::Appl),
            1 => Some(Source::Asic),
            4 => Some(Source::Config),
            6 => Some(Source::State),
            _ => None,
        }
    }
}

/// One raw input to the tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A keyspace notification: `key` changed in a database by `op` (`hset`, `del`, ...).
    Db {
        source: Source,
        key: String,
        op: String,
    },
    /// A new line in `swss.rec`, `sairedis.rec` or syslog.
    Log { source: Source, line: String },
}

/// An object as live mode names it, e.g. (Vlan, "Vlan100") or (Route, "10.1.1.0/24").
pub type ObjectId = (Kind, String);

/// What one change says about an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub object: ObjectId,
    /// A member port, for VLAN and LAG members (`Ethernet4`), when known.
    pub member: Option<String>,
    pub source: Source,
    /// Short verb shown to the user: `set`, `removed`, `queued`, `received SET`, ...
    pub what: String,
    pub removing: bool,
}

/// What the tracker needs to read from ASIC_DB and COUNTERS_DB while running.
pub trait Lookup {
    /// `HGET key field` in database `db`.
    fn hget(&mut self, db: u8, key: &str, field: &str) -> Option<String>;
    /// `KEYS pattern` in database `db`.
    fn keys(&mut self, db: u8, pattern: &str) -> Vec<String>;
}

/// Which objects to show: by kind, and optionally by name.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kinds: BTreeSet<Kind>,
    pub names: BTreeSet<String>,
}

impl Filter {
    /// Parse `live` arguments: kinds (`vlan`, `port`, `lag`, `route`) and object names.
    /// No kinds means all kinds; a name implies its kind when the kind is not given.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut f = Filter::default();
        for a in args {
            match a.as_str() {
                "vlan" | "vlans" => f.kinds.insert(Kind::Vlan),
                "port" | "ports" => f.kinds.insert(Kind::Port),
                "lag" | "lags" => f.kinds.insert(Kind::Lag),
                "route" | "routes" => f.kinds.insert(Kind::Route),
                "all" => false,
                name => {
                    let name = if kind_of_name(name) == Some(Kind::Route) {
                        route_name(name).ok_or_else(|| format!("'{name}' is not a valid route"))?
                    } else {
                        name.to_string()
                    };
                    if kind_of_name(&name).is_none() {
                        return Err(format!(
                            "'{name}' is not a kind (vlan, port, lag, route) or an object name like Vlan100, Ethernet0, PortChannel1 or 10.1.1.0/24"
                        ));
                    }
                    f.names.insert(name)
                }
            };
        }
        Ok(f)
    }

    pub fn allows(&self, (kind, name): &ObjectId) -> bool {
        let kinds: BTreeSet<Kind> = if self.kinds.is_empty() && self.names.is_empty() {
            Kind::ALL.into_iter().collect()
        } else {
            let mut k = self.kinds.clone();
            k.extend(self.names.iter().filter_map(|n| kind_of_name(n)));
            k
        };
        if !kinds.contains(kind) {
            return false;
        }
        // Names narrow down their own kind only: `live vlan Vlan100 route` shows Vlan100
        // and every route.
        let named: Vec<&String> = self
            .names
            .iter()
            .filter(|n| kind_of_name(n) == Some(*kind))
            .collect();
        named.is_empty() || named.contains(&name)
    }

    /// A short description for the header line.
    pub fn describe(&self) -> String {
        if self.kinds.is_empty() && self.names.is_empty() {
            return "VLANs, ports, LAGs and routes".into();
        }
        let mut parts: Vec<String> = self
            .kinds
            .iter()
            .map(|k| format!("all {}s", k.name()))
            .collect();
        parts.extend(self.names.iter().cloned());
        parts.join(", ")
    }
}

/// The kind an object name belongs to, from its shape.
fn kind_of_name(name: &str) -> Option<Kind> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if name.strip_prefix("Vlan").is_some_and(digits) {
        Some(Kind::Vlan)
    } else if name.strip_prefix("Ethernet").is_some_and(digits) {
        Some(Kind::Port)
    } else if name.strip_prefix("PortChannel").is_some_and(digits) {
        Some(Kind::Lag)
    } else if route_name(name).is_some() {
        Some(Kind::Route)
    } else {
        None
    }
}

/// Canonical route name: `[<vrf>:]<address>/<mask>`, with host routes given their mask
/// (APPL_DB writes `10.1.0.32`, ASIC_DB `10.1.0.32/32`). `None` when not a route.
pub fn route_name(s: &str) -> Option<String> {
    let (vrf, prefix) = match s.strip_prefix("Vrf") {
        Some(_) => {
            let (v, p) = s.split_once(':')?;
            (Some(v), p)
        }
        None => (None, s),
    };
    let (addr, mask) = match prefix.split_once('/') {
        Some((a, m)) => (a, Some(m)),
        None => (prefix, None),
    };
    let ip: IpAddr = addr.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let mask: u8 = match mask {
        Some(m) => m.parse().ok().filter(|m| *m <= max)?,
        None => max,
    };
    Some(match vrf {
        Some(v) => format!("{v}:{ip}/{mask}"),
        None => format!("{ip}/{mask}"),
    })
}

/// Map a CONFIG_DB, APPL_DB or STATE_DB key to an object. `sep` is `|` for CONFIG_DB and
/// STATE_DB, `:` for APPL_DB. Returns the object and an optional member port.
fn classify_table_key(key: &str, sep: char) -> Option<(ObjectId, Option<String>)> {
    let (table, rest) = key.split_once(sep)?;
    let two = || {
        rest.split_once(sep)
            .map(|(a, b)| (a.to_string(), b.to_string()))
    };
    let obj = |k: Kind, n: &str| (k, n.to_string());
    match table {
        "VLAN" | "VLAN_TABLE" => Some((obj(Kind::Vlan, rest), None)),
        "VLAN_MEMBER" | "VLAN_MEMBER_TABLE" => {
            let (v, m) = two()?;
            Some(((Kind::Vlan, v), Some(m)))
        }
        "PORT" | "PORT_TABLE" => Some((obj(Kind::Port, rest), None)),
        "PORTCHANNEL" | "LAG_TABLE" => Some((obj(Kind::Lag, rest), None)),
        "PORTCHANNEL_MEMBER" | "LAG_MEMBER_TABLE" => {
            let (l, m) = two()?;
            Some(((Kind::Lag, l), Some(m)))
        }
        // CONFIG_DB: STATIC_ROUTE|<prefix> or STATIC_ROUTE|<vrf>|<prefix>.
        "STATIC_ROUTE" => {
            let name = match rest.split_once('|') {
                Some(("default", p)) => p.to_string(),
                Some((v, p)) => format!("{v}:{p}"),
                None => rest.to_string(),
            };
            Some(((Kind::Route, route_name(&name)?), None))
        }
        // APPL_DB: ROUTE_TABLE:[<vrf>:]<prefix>, host routes without the mask.
        "ROUTE_TABLE" => Some(((Kind::Route, route_name(rest)?), None)),
        _ => None,
    }
    .filter(|((k, n), _)| *k == Kind::Route || kind_of_name(n) == Some(*k))
}

/// IDs learned from ASIC_DB, so that removals (whose data is already gone) can still be
/// named, and so that lookups are not repeated.
#[derive(Debug, Default)]
pub struct AsicNames {
    /// VLAN OID -> `Vlan<id>`.
    vlans: BTreeMap<String, String>,
    /// Port OID -> `Ethernet<n>`, and LAG OID -> `PortChannel<n>`.
    ports: BTreeMap<String, String>,
    lags: BTreeMap<String, String>,
    /// Member OID -> (owner object, member name).
    members: BTreeMap<String, (ObjectId, Option<String>)>,
}

impl AsicNames {
    /// Add `name -> oid` pairs from COUNTERS_PORT_NAME_MAP or COUNTERS_LAG_NAME_MAP.
    pub fn insert_map(&mut self, lag: bool, pairs: impl IntoIterator<Item = (String, String)>) {
        let into = if lag { &mut self.lags } else { &mut self.ports };
        for (name, oid) in pairs {
            into.insert(oid, name);
        }
    }

    fn vlan_of(&mut self, lookup: &mut dyn Lookup, oid: &str) -> Option<String> {
        if let Some(v) = self.vlans.get(oid) {
            return Some(v.clone());
        }
        let id = lookup.hget(
            1,
            &format!("ASIC_STATE:SAI_OBJECT_TYPE_VLAN:{oid}"),
            "SAI_VLAN_ATTR_VLAN_ID",
        )?;
        let name = format!("Vlan{id}");
        self.vlans.insert(oid.to_string(), name.clone());
        Some(name)
    }

    /// Port name of a bridge port, or `tunnel` for a VXLAN tunnel bridge port.
    fn bridge_port_name(&mut self, lookup: &mut dyn Lookup, bp: &str) -> Option<String> {
        let key = format!("ASIC_STATE:SAI_OBJECT_TYPE_BRIDGE_PORT:{bp}");
        if let Some(port) = lookup.hget(1, &key, "SAI_BRIDGE_PORT_ATTR_PORT_ID") {
            return self
                .ports
                .get(&port)
                .or_else(|| self.lags.get(&port))
                .cloned();
        }
        lookup
            .hget(1, &key, "SAI_BRIDGE_PORT_ATTR_TUNNEL_ID")
            .map(|_| "VXLAN tunnel".to_string())
    }
}

/// `SAI_OBJECT_TYPE_<T>:<rest>` (from ASIC_DB or sairedis.rec) to an object.
/// `attrs` are the attributes from a sairedis.rec line, used before asking ASIC_DB.
fn classify_sai(
    names: &mut AsicNames,
    lookup: &mut dyn Lookup,
    key: &str,
    attrs: &[&str],
    removing: bool,
) -> Option<(ObjectId, Option<String>)> {
    let rest = key.strip_prefix("SAI_OBJECT_TYPE_")?;
    let (ty, id) = rest.split_once(':')?;
    let attr = |name: &str| {
        attrs.iter().find_map(|a| {
            a.split_once('=')
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    };
    let asic_key = format!("ASIC_STATE:{key}");
    match ty {
        "ROUTE_ENTRY" => {
            let v: serde_json::Value = serde_json::from_str(id).ok()?;
            let dest = v.get("dest")?.as_str()?;
            Some(((Kind::Route, route_name(dest)?), None))
        }
        "VLAN" => {
            if let Some(vid) = attr("SAI_VLAN_ATTR_VLAN_ID") {
                let name = format!("Vlan{vid}");
                names.vlans.insert(id.to_string(), name.clone());
                return Some(((Kind::Vlan, name), None));
            }
            if removing {
                return names.vlans.get(id).map(|n| ((Kind::Vlan, n.clone()), None));
            }
            Some(((Kind::Vlan, names.vlan_of(lookup, id)?), None))
        }
        "VLAN_MEMBER" => {
            if let Some(known) = names.members.get(id) {
                return Some(known.clone());
            }
            if removing {
                return None;
            }
            let vlan_oid = attr("SAI_VLAN_MEMBER_ATTR_VLAN_ID")
                .or_else(|| lookup.hget(1, &asic_key, "SAI_VLAN_MEMBER_ATTR_VLAN_ID"))?;
            let vlan = names.vlan_of(lookup, &vlan_oid)?;
            let bp = attr("SAI_VLAN_MEMBER_ATTR_BRIDGE_PORT_ID")
                .or_else(|| lookup.hget(1, &asic_key, "SAI_VLAN_MEMBER_ATTR_BRIDGE_PORT_ID"));
            let member = bp.and_then(|bp| names.bridge_port_name(lookup, &bp));
            let found = ((Kind::Vlan, vlan), member);
            names.members.insert(id.to_string(), found.clone());
            Some(found)
        }
        "PORT" => names.ports.get(id).map(|n| ((Kind::Port, n.clone()), None)),
        "LAG" => names.lags.get(id).map(|n| ((Kind::Lag, n.clone()), None)),
        "LAG_MEMBER" => {
            if let Some(known) = names.members.get(id) {
                return Some(known.clone());
            }
            if removing {
                return None;
            }
            let lag_oid = attr("SAI_LAG_MEMBER_ATTR_LAG_ID")
                .or_else(|| lookup.hget(1, &asic_key, "SAI_LAG_MEMBER_ATTR_LAG_ID"))?;
            let lag = names.lags.get(&lag_oid)?.clone();
            let port = attr("SAI_LAG_MEMBER_ATTR_PORT_ID")
                .or_else(|| lookup.hget(1, &asic_key, "SAI_LAG_MEMBER_ATTR_PORT_ID"))
                .and_then(|p| names.ports.get(&p).cloned());
            let found = ((Kind::Lag, lag), port);
            names.members.insert(id.to_string(), found.clone());
            Some(found)
        }
        _ => None,
    }
}

/// Turn one input into the changes it describes (usually zero or one).
pub fn classify(names: &mut AsicNames, lookup: &mut dyn Lookup, input: &Input) -> Vec<Change> {
    let change =
        |(object, member): (ObjectId, Option<String>), source, what: &str, removing| Change {
            object,
            member,
            source,
            what: what.to_string(),
            removing,
        };
    match input {
        Input::Db { source, key, op } => {
            let removing = op == "del";
            // Producer tables (APPL_DB) write `_TABLE:key` and a `TABLE_KEY_SET`; the
            // consumer moves the entry to `TABLE:key`. The queue's own deletes are noise.
            if key.contains("KEY_SET") || key.contains("DEL_SET") {
                return Vec::new();
            }
            let (queued, key) = match key.strip_prefix('_') {
                Some(k) => (true, k),
                None => (false, key.as_str()),
            };
            if queued && removing {
                return Vec::new();
            }
            let what = match (queued, removing) {
                (true, _) => "queued",
                (false, true) => "removed",
                (false, false) => "set",
            };
            let found = match source {
                Source::Config | Source::State => classify_table_key(key, '|'),
                Source::Appl => classify_table_key(key, ':'),
                Source::Asic => key
                    .strip_prefix("ASIC_STATE:")
                    .and_then(|k| classify_sai(names, lookup, k, &[], removing)),
                _ => None,
            };
            found
                .map(|f| change(f, *source, what, removing))
                .into_iter()
                .collect()
        }
        Input::Log {
            source: Source::SwssRec,
            line,
        } => {
            let mut it = line.split('|');
            let (Some(_ts), Some(key), Some(op)) = (it.next(), it.next(), it.next()) else {
                return Vec::new();
            };
            let removing = op == "DEL";
            classify_table_key(key, ':')
                .map(|f| {
                    change(
                        f,
                        Source::SwssRec,
                        &format!("orchagent received {op}"),
                        removing,
                    )
                })
                .into_iter()
                .collect()
        }
        Input::Log {
            source: Source::SairedisRec,
            line,
        } => parse_sairedis_line(line)
            .into_iter()
            .filter_map(|r| {
                let removing = r.op == 'r';
                let what = match (r.op, r.bulk) {
                    ('c', false) => "SAI create",
                    ('r', false) => "SAI remove",
                    ('s', false) => "SAI set",
                    ('c', true) => "SAI create (bulk)",
                    ('r', true) => "SAI remove (bulk)",
                    _ => "SAI set (bulk)",
                };
                classify_sai(names, lookup, &r.key, &r.attrs, removing)
                    .map(|f| change(f, Source::SairedisRec, what, removing))
            })
            .collect(),
        Input::Log { .. } => Vec::new(),
    }
}

/// A change that has not reached the ASIC yet.
#[derive(Debug, Clone)]
struct Pending {
    since_ms: u64,
    from: Source,
    removing: bool,
    warned: bool,
    sai_seen: bool,
}

/// Follows objects through the stages and produces the lines to print.
pub struct Tracker {
    pub filter: Filter,
    pub stuck_after_ms: u64,
    names: AsicNames,
    pending: BTreeMap<(ObjectId, Option<String>), Pending>,
    /// Objects seen during this session, to match syslog lines against.
    seen: BTreeSet<ObjectId>,
    /// Recent serious syslog lines, replayed when an object first appears: a log line can
    /// arrive just before the database change that introduces its object.
    recent_syslog: Vec<(u64, String)>,
    /// Last time each identical change was printed, to collapse the burst of notifications
    /// that one logical change produces (a VLAN create writes several fields to its key,
    /// and Redis notifies on each one).
    last_emitted: BTreeMap<(ObjectId, Option<String>, Source, String, bool), u64>,
}

/// Window within which an identical change (same object, member, source, op) is treated as
/// a repeat of the same logical change and shown only once.
const DEDUP_WINDOW_MS: u64 = 1000;

/// How long and how many serious syslog lines are kept for replay.
const SYSLOG_KEEP_MS: u64 = 10_000;
const SYSLOG_KEEP_LINES: usize = 200;

/// `HH:MM:SS.mmm` (UTC) for a time in milliseconds since the Unix epoch.
pub fn clock(ms: u64) -> String {
    let day_ms = ms % 86_400_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        day_ms / 3_600_000,
        day_ms / 60_000 % 60,
        day_ms / 1000 % 60,
        day_ms % 1000
    )
}

fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

impl Tracker {
    pub fn new(filter: Filter, stuck_after_ms: u64, names: AsicNames) -> Self {
        Self {
            filter,
            stuck_after_ms,
            names,
            pending: BTreeMap::new(),
            seen: BTreeSet::new(),
            recent_syslog: Vec::new(),
            last_emitted: BTreeMap::new(),
        }
    }

    fn line(at_ms: u64, (kind, name): &ObjectId, source: &str, text: &str) -> String {
        format!(
            "{}  {:<5} {:<20} {:<12} {}",
            clock(at_ms),
            kind.name(),
            name,
            source,
            text
        )
    }

    /// Handle one input at `now_ms`; returns the lines to print.
    pub fn on_input(&mut self, now_ms: u64, input: &Input, lookup: &mut dyn Lookup) -> Vec<String> {
        if let Input::Log {
            source: Source::Syslog,
            line,
        } = input
        {
            return self.on_syslog(now_ms, line);
        }
        let mut out = Vec::new();
        for c in classify(&mut self.names, lookup, input) {
            if !self.filter.allows(&c.object) {
                continue;
            }
            // Collapse the burst of identical notifications one logical change produces.
            let dedup_key = (
                c.object.clone(),
                c.member.clone(),
                c.source,
                c.what.clone(),
                c.removing,
            );
            if let Some(prev) = self.last_emitted.get(&dedup_key)
                && now_ms.saturating_sub(*prev) < DEDUP_WINDOW_MS
            {
                self.last_emitted.insert(dedup_key, now_ms);
                continue;
            }
            self.last_emitted.insert(dedup_key, now_ms);
            let first_time = self.seen.insert(c.object.clone());
            let mut text = c.what.clone();
            if let Some(m) = &c.member {
                text.push_str(&format!("  member {m}"));
            }
            let slot = (c.object.clone(), c.member.clone());
            match c.source {
                // A user-side change starts (or restarts) the timer. For ports only a
                // CONFIG_DB change counts: orchagent itself writes link state
                // (oper_status) to APPL_DB PORT_TABLE, which never touches the ASIC.
                Source::Config | Source::Appl
                    if c.source == Source::Config || c.object.0 != Kind::Port =>
                {
                    let restart = self
                        .pending
                        .get(&slot)
                        .is_none_or(|p| p.removing != c.removing);
                    if restart {
                        self.pending.insert(
                            slot,
                            Pending {
                                since_ms: now_ms,
                                from: c.source,
                                removing: c.removing,
                                warned: false,
                                sai_seen: false,
                            },
                        );
                    }
                }
                Source::SairedisRec => {
                    if let Some(p) = self.pending.get_mut(&slot) {
                        p.sai_seen = true;
                    }
                }
                Source::Asic => {
                    // A member's ASIC change completes that member; a whole-object change
                    // completes the object.
                    if let Some(p) = self.pending.remove(&slot) {
                        let took = now_ms.saturating_sub(p.since_ms);
                        let verb = if c.removing {
                            "removed from"
                        } else {
                            "reached"
                        };
                        let late = if p.warned { " (late)" } else { "" };
                        text.push_str(&format!(
                            "   ✅ {verb} the ASIC {} after the {} change{late}",
                            duration(took),
                            p.from.label()
                        ));
                    }
                }
                _ => {}
            }
            out.push(Self::line(now_ms, &c.object, c.source.label(), &text));
            if first_time {
                let recent: Vec<String> =
                    self.recent_syslog.iter().map(|(_, l)| l.clone()).collect();
                for l in recent {
                    out.extend(self.syslog_lines_for(now_ms, &c.object, &l));
                }
            }
        }
        out
    }

    /// The syslog line, shown under `obj`, if it mentions it.
    fn syslog_lines_for(&self, now_ms: u64, obj: &ObjectId, line: &str) -> Option<String> {
        let (_, name) = obj;
        // Routes appear in logs with or without the mask, and without the VRF.
        let bare = name.rsplit_once(':').map_or(name.as_str(), |(_, p)| p);
        let host = bare
            .strip_suffix("/32")
            .or_else(|| bare.strip_suffix("/128"));
        let tokens: Vec<&str> = [Some(bare), host].into_iter().flatten().collect();
        tokens.iter().any(|t| has_word(line, t)).then(|| {
            let (_, shown) = tidy_syslog(line.trim(), &tokens);
            Self::line(now_ms, obj, "syslog", &shown)
        })
    }

    /// Error and warning lines that mention an object being followed.
    fn on_syslog(&mut self, now_ms: u64, line: &str) -> Vec<String> {
        let serious = [" ERR ", " ERROR ", " WARNING ", " CRIT "]
            .iter()
            .any(|s| line.contains(s));
        if !serious {
            return Vec::new();
        }
        self.recent_syslog
            .retain(|(t, _)| now_ms.saturating_sub(*t) <= SYSLOG_KEEP_MS);
        if self.recent_syslog.len() >= SYSLOG_KEEP_LINES {
            self.recent_syslog.remove(0);
        }
        self.recent_syslog.push((now_ms, line.to_string()));
        let mut candidates: BTreeSet<ObjectId> = self.seen.clone();
        candidates.extend(
            self.filter
                .names
                .iter()
                .filter_map(|n| kind_of_name(n).map(|k| (k, n.clone()))),
        );
        candidates
            .iter()
            .filter_map(|obj| self.syslog_lines_for(now_ms, obj, line))
            .collect()
    }

    /// Check timers at `now_ms`; returns warnings for changes that have not reached the
    /// ASIC in time. Each change is warned about once.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<String> {
        let mut out = Vec::new();
        for ((obj, member), p) in self.pending.iter_mut() {
            if p.warned || now_ms.saturating_sub(p.since_ms) < self.stuck_after_ms {
                continue;
            }
            p.warned = true;
            let what = match member {
                Some(m) => format!("member {m}"),
                None => "it".to_string(),
            };
            let action = if p.removing { "removal" } else { "change" };
            let mut text = format!(
                "⚠ no ASIC update for {what} {} after the {} {action}",
                duration(now_ms.saturating_sub(p.since_ms)),
                p.from.label()
            );
            if p.sai_seen {
                text.push_str("; the SAI call was made (sairedis.rec) but ASIC_DB did not change");
            } else if obj.0 == Kind::Port {
                text.push_str(
                    " (normal if the change does not touch the chip, e.g. a description)",
                );
            } else {
                text.push_str(&format!(
                    " (check syslog above, or trace it: sonic-trace --live {} {})",
                    obj.0.name(),
                    obj.1
                ));
            }
            out.push(Self::line(now_ms, obj, "", &text));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[derive(Default)]
    struct FakeLookup(BTreeMap<(u8, String, String), String>);

    impl Lookup for FakeLookup {
        fn hget(&mut self, db: u8, key: &str, field: &str) -> Option<String> {
            self.0
                .get(&(db, key.to_string(), field.to_string()))
                .cloned()
        }
        fn keys(&mut self, _: u8, _: &str) -> Vec<String> {
            Vec::new()
        }
    }

    fn db(source: Source, key: &str, op: &str) -> Input {
        Input::Db {
            source,
            key: key.into(),
            op: op.into(),
        }
    }

    #[test]
    fn route_names_are_canonical() {
        assert_eq!(route_name("10.1.0.32").unwrap(), "10.1.0.32/32");
        assert_eq!(route_name("FE80:0::1/64").unwrap(), "fe80::1/64");
        assert_eq!(
            route_name("Vrf-red:20.0.0.0/24").unwrap(),
            "Vrf-red:20.0.0.0/24"
        );
        assert_eq!(route_name("10.0.0.0/33"), None);
        assert_eq!(route_name("Vlan10"), None);
    }

    #[test]
    fn table_keys_map_to_objects() {
        let c = |k, s| classify_table_key(k, s);
        assert_eq!(
            c("VLAN|Vlan100", '|').unwrap().0,
            (Kind::Vlan, "Vlan100".into())
        );
        assert_eq!(
            c("VLAN_MEMBER_TABLE:Vlan100:Ethernet4", ':').unwrap(),
            ((Kind::Vlan, "Vlan100".into()), Some("Ethernet4".into()))
        );
        assert_eq!(
            c("ROUTE_TABLE:10.1.0.32", ':').unwrap().0,
            (Kind::Route, "10.1.0.32/32".into())
        );
        assert_eq!(
            c("ROUTE_TABLE:Vrf-red:20.0.0.0/24", ':').unwrap().0,
            (Kind::Route, "Vrf-red:20.0.0.0/24".into())
        );
        assert_eq!(
            c("STATIC_ROUTE|default|10.9.0.0/24", '|').unwrap().0,
            (Kind::Route, "10.9.0.0/24".into())
        );
        assert_eq!(
            c("PORTCHANNEL|PortChannel01", '|').unwrap().0,
            (Kind::Lag, "PortChannel01".into())
        );
        assert!(c("VLAN|notavlan", '|').is_none());
        assert!(c("NEIGH_TABLE:Vlan10:10.0.0.1", ':').is_none());
    }

    #[test]
    fn filter_by_kind_and_name() {
        let f = Filter::parse(&["vlan".into()]).unwrap();
        assert!(f.allows(&(Kind::Vlan, "Vlan5".into())));
        assert!(!f.allows(&(Kind::Route, "10.0.0.0/8".into())));
        let f = Filter::parse(&["Vlan100".into(), "route".into()]).unwrap();
        assert!(f.allows(&(Kind::Vlan, "Vlan100".into())));
        assert!(!f.allows(&(Kind::Vlan, "Vlan200".into())));
        assert!(f.allows(&(Kind::Route, "10.0.0.0/8".into())));
        assert!(!f.allows(&(Kind::Port, "Ethernet0".into())));
        let f = Filter::parse(&[]).unwrap();
        assert!(f.allows(&(Kind::Lag, "PortChannel1".into())));
        assert!(Filter::parse(&["bogus".into()]).is_err());
        let f = Filter::parse(&["10.1.0.32".into()]).unwrap();
        assert!(f.allows(&(Kind::Route, "10.1.0.32/32".into())));
    }

    #[test]
    fn vlan_create_flows_to_the_asic_and_reports_the_time() {
        let mut look = FakeLookup::default();
        look.0.insert(
            (
                1,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26".into(),
                "SAI_VLAN_ATTR_VLAN_ID".into(),
            ),
            "100".into(),
        );
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        let mut out = Vec::new();
        out.extend(t.on_input(1000, &db(Source::Config, "VLAN|Vlan100", "hset"), &mut look));
        out.extend(t.on_input(
            1050,
            &db(Source::Appl, "_VLAN_TABLE:Vlan100", "hset"),
            &mut look,
        ));
        out.extend(t.on_input(
            1051,
            &db(Source::Appl, "VLAN_TABLE_KEY_SET", "sadd"),
            &mut look,
        ));
        out.extend(t.on_input(
            1060,
            &db(Source::Appl, "VLAN_TABLE:Vlan100", "hset"),
            &mut look,
        ));
        out.extend(t.on_input(
            1061,
            &db(Source::Appl, "_VLAN_TABLE:Vlan100", "del"),
            &mut look,
        ));
        out.extend(t.on_input(
            1070,
            &db(Source::State, "VLAN_TABLE|Vlan100", "hset"),
            &mut look,
        ));
        out.extend(t.on_input(
            1092,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26",
                "hset",
            ),
            &mut look,
        ));
        let text = out.join("\n");
        assert_eq!(out.len(), 5, "{text}");
        assert!(out[1].contains("APPL_DB      queued"), "{text}");
        assert!(
            out[4].contains(
                "ASIC_DB      set   ✅ reached the ASIC 92 ms after the CONFIG_DB change"
            ),
            "{text}"
        );
        assert!(
            t.on_tick(10_000).is_empty(),
            "completed changes do not warn"
        );
    }

    #[test]
    fn stuck_change_warns_once_and_late_arrival_is_marked() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        t.on_input(0, &db(Source::Config, "VLAN|Vlan200", "hset"), &mut look);
        assert!(t.on_tick(4999).is_empty());
        let w = t.on_tick(5000);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("⚠ no ASIC update for it 5.0 s after the CONFIG_DB change"),
            "{w:?}"
        );
        assert!(t.on_tick(9000).is_empty(), "warned once");
        let sai = Input::Log {
            source: Source::SairedisRec,
            line: "2000-01-01.00:00:07.000000|c|SAI_OBJECT_TYPE_VLAN:oid:0x99|SAI_VLAN_ATTR_VLAN_ID=200".into(),
        };
        let out = t.on_input(7000, &sai, &mut look);
        assert!(out[0].contains("sairedis.rec SAI create"), "{out:?}");
        let out = t.on_input(
            7100,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x99",
                "hset",
            ),
            &mut look,
        );
        assert!(
            out[0].contains("reached the ASIC 7.1 s after the CONFIG_DB change (late)"),
            "{out:?}"
        );
    }

    #[test]
    fn sai_call_without_asic_db_update_is_called_out() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(Filter::default(), 1000, AsicNames::default());
        t.on_input(
            0,
            &db(Source::Appl, "ROUTE_TABLE:10.9.0.0/24", "hset"),
            &mut look,
        );
        let sai = Input::Log {
            source: Source::SairedisRec,
            line: r#"2000-01-01.00:00:00.100000|C|SAI_OBJECT_TYPE_ROUTE_ENTRY||{"dest":"10.9.0.0/24","switch_id":"oid:0x21","vr":"oid:0x30"}|SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID=oid:0x40"#.into(),
        };
        let out = t.on_input(100, &sai, &mut look);
        assert!(
            out[0].contains("route 10.9.0.0/24") && out[0].contains("SAI create (bulk)"),
            "{out:?}"
        );
        let w = t.on_tick(1000);
        assert!(
            w[0].contains("the SAI call was made (sairedis.rec) but ASIC_DB did not change"),
            "{w:?}"
        );
    }

    #[test]
    fn removal_is_tracked_with_cached_names() {
        let mut look = FakeLookup::default();
        look.0.insert(
            (
                1,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26".into(),
                "SAI_VLAN_ATTR_VLAN_ID".into(),
            ),
            "100".into(),
        );
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        // Learn the OID from an update, then remove: ASIC_DB no longer has the data.
        t.on_input(
            0,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26",
                "hset",
            ),
            &mut look,
        );
        look.0.clear();
        t.on_input(10, &db(Source::Config, "VLAN|Vlan100", "del"), &mut look);
        let out = t.on_input(
            30,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26",
                "del",
            ),
            &mut look,
        );
        assert!(
            out[0].contains(
                "ASIC_DB      removed   ✅ removed from the ASIC 20 ms after the CONFIG_DB change"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn vlan_member_resolves_port_through_bridge_port() {
        let mut look = FakeLookup::default();
        let k = |s: &str| s.to_string();
        look.0.insert(
            (
                1,
                k("ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x27"),
                k("SAI_VLAN_MEMBER_ATTR_VLAN_ID"),
            ),
            k("oid:0x26"),
        );
        look.0.insert(
            (
                1,
                k("ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x27"),
                k("SAI_VLAN_MEMBER_ATTR_BRIDGE_PORT_ID"),
            ),
            k("oid:0x3a"),
        );
        look.0.insert(
            (
                1,
                k("ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26"),
                k("SAI_VLAN_ATTR_VLAN_ID"),
            ),
            k("100"),
        );
        look.0.insert(
            (
                1,
                k("ASIC_STATE:SAI_OBJECT_TYPE_BRIDGE_PORT:oid:0x3a"),
                k("SAI_BRIDGE_PORT_ATTR_PORT_ID"),
            ),
            k("oid:0x1001"),
        );
        let mut names = AsicNames::default();
        names.insert_map(false, [(k("Ethernet4"), k("oid:0x1001"))]);
        let mut t = Tracker::new(Filter::default(), 5000, names);
        t.on_input(
            0,
            &db(Source::Config, "VLAN_MEMBER|Vlan100|Ethernet4", "hset"),
            &mut look,
        );
        let out = t.on_input(
            50,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x27",
                "hset",
            ),
            &mut look,
        );
        assert!(
            out[0].contains("vlan  Vlan100")
                && out[0].contains("member Ethernet4")
                && out[0].contains("reached the ASIC 50 ms"),
            "{out:?}"
        );
    }

    #[test]
    fn syslog_errors_for_followed_objects_are_shown() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        t.on_input(0, &db(Source::Config, "VLAN|Vlan200", "hset"), &mut look);
        let log = |l: &str| Input::Log {
            source: Source::Syslog,
            line: l.into(),
        };
        let out = t.on_input(10, &log("2000 Jan 01 00:00:00.1 host ERR swss#orchagent: :- addVlan: Failed to create VLAN Vlan200"), &mut look);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains("syslog") && out[0].contains("Failed to create VLAN Vlan200"),
            "{out:?}"
        );
        assert!(
            t.on_input(
                10,
                &log("2000 Jan 01 00:00:00.1 host NOTICE swss#orchagent: Vlan200 ok"),
                &mut look
            )
            .is_empty()
        );
        assert!(
            t.on_input(
                10,
                &log("2000 Jan 01 00:00:00.1 host ERR swss#orchagent: Vlan2000 failed"),
                &mut look
            )
            .is_empty()
        );
    }

    #[test]
    fn syslog_line_before_its_object_is_replayed() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        let log = Input::Log {
            source: Source::Syslog,
            line:
                "2000 Jan 01 00:00:00.1 host ERR swss#orchagent: Failed to create route 10.9.0.0/24"
                    .into(),
        };
        assert!(
            t.on_input(0, &log, &mut look).is_empty(),
            "object not seen yet"
        );
        let out = t.on_input(
            100,
            &db(Source::Appl, "ROUTE_TABLE:10.9.0.0/24", "hset"),
            &mut look,
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(
            out[1].contains("syslog") && out[1].contains("Failed to create route"),
            "{out:?}"
        );
        // Too old to replay.
        let out = t.on_input(
            20_000,
            &db(Source::Appl, "ROUTE_TABLE:10.9.0.0/24", "hset"),
            &mut look,
        );
        assert_eq!(
            out.len(),
            1,
            "seen before, and the log line is old: {out:?}"
        );
    }

    #[test]
    fn repeated_field_notifications_collapse_to_one_line() {
        // A VLAN create fires several APPL_DB notifications for the same key (one per
        // field); only one line should be shown per stage. Seen on real hardware.
        let mut look = FakeLookup::default();
        look.0.insert(
            (
                1,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26".into(),
                "SAI_VLAN_ATTR_VLAN_ID".into(),
            ),
            "999".into(),
        );
        let mut t = Tracker::new(Filter::default(), 5000, AsicNames::default());
        let mut out = Vec::new();
        out.extend(t.on_input(100, &db(Source::Config, "VLAN|Vlan999", "hset"), &mut look));
        for ms in [110, 110, 110, 110] {
            out.extend(t.on_input(
                ms,
                &db(Source::Appl, "VLAN_TABLE:Vlan999", "hset"),
                &mut look,
            ));
        }
        out.extend(t.on_input(
            300,
            &db(
                Source::Asic,
                "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26",
                "hset",
            ),
            &mut look,
        ));
        assert_eq!(
            out.iter()
                .filter(|l| l.contains("APPL_DB      set"))
                .count(),
            1,
            "{out:#?}"
        );
        assert_eq!(out.len(), 3, "{out:#?}");
        // A genuine later change of the same kind is still shown (outside the window).
        let later = t.on_input(
            2000,
            &db(Source::Appl, "VLAN_TABLE:Vlan999", "hset"),
            &mut look,
        );
        assert_eq!(later.len(), 1, "{later:?}");
    }

    #[test]
    fn port_link_state_updates_do_not_start_a_timer() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(Filter::default(), 1000, AsicNames::default());
        // orchagent reports a link going down: APPL_DB and STATE_DB change, the ASIC does not.
        let out = t.on_input(
            0,
            &db(Source::Appl, "PORT_TABLE:Ethernet4", "hset"),
            &mut look,
        );
        assert_eq!(out.len(), 1, "still shown: {out:?}");
        t.on_input(
            0,
            &db(Source::State, "PORT_TABLE|Ethernet4", "hset"),
            &mut look,
        );
        assert!(t.on_tick(5000).is_empty(), "no false stuck warning");
        // A configured change still does.
        t.on_input(
            6000,
            &db(Source::Config, "PORT|Ethernet4", "hset"),
            &mut look,
        );
        assert_eq!(t.on_tick(7000).len(), 1);
    }

    #[test]
    fn filtered_out_objects_print_nothing() {
        let mut look = FakeLookup::default();
        let mut t = Tracker::new(
            Filter::parse(&["route".into()]).unwrap(),
            5000,
            AsicNames::default(),
        );
        assert!(
            t.on_input(0, &db(Source::Config, "VLAN|Vlan100", "hset"), &mut look)
                .is_empty()
        );
        assert!(t.on_tick(10_000).is_empty());
    }

    #[test]
    fn clock_formats_utc() {
        assert_eq!(clock(86_400_000 + 3_723_045), "01:02:03.045");
    }
}
