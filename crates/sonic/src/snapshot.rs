use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub type Fields = BTreeMap<String, String>;

const DB_NAMES: [&str; 4] = ["CONFIG_DB", "APPL_DB", "STATE_DB", "ASIC_DB"];

/// Default cap on the total bytes read from input files.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Redis key types that `sonic-db-dump` (redisdl) writes in its wrapped form.
const REDIS_TYPES: [&str; 5] = ["hash", "string", "list", "set", "zset"];

/// Read a file as UTF-8 without loading more than `budget` bytes, and deduct what was read.
fn read_limited(path: &Path, budget: &mut u64) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = String::new();
    // Read one byte past the budget so an oversized file is detected without loading it.
    let n = file
        .take(budget.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let n = n as u64;
    if n > *budget {
        return Err(format!(
            "{}: input exceeds the size limit (raise it with --max-input-mb)",
            path.display()
        ));
    }
    *budget -= n;
    Ok(text)
}

/// True when `entry` is redisdl's wrapper `{"type": "<redis type>", "value": ..., ["ttl", "expireat"]}`
/// rather than a plain hash that happens to have fields named `type` and `value`.
fn is_wrapped(entry: &serde_json::Map<String, Value>) -> bool {
    let typed = entry
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| REDIS_TYPES.contains(&t));
    typed
        && entry.contains_key("value")
        && entry
            .keys()
            .all(|k| matches!(k.as_str(), "type" | "value" | "ttl" | "expireat"))
        // A wrapped hash always has an object value; plain string fields never do.
        && (entry.get("type").and_then(Value::as_str) != Some("hash")
            || entry.get("value").is_some_and(Value::is_object))
}

/// Where a `generate_dump` bundle keeps its database dumps and its logs.
#[derive(Debug, Clone)]
pub struct BundleLayout {
    pub db_dir: PathBuf,
    pub log_dir: Option<PathBuf>,
}

fn has_db_files(dir: &Path, asic: Option<u32>) -> bool {
    DB_NAMES
        .iter()
        .any(|name| db_file(dir, name, asic).is_some())
}

/// The dump file for one database, trying the names used by `generate_dump`
/// (`APPL_DB.json`, or `APPL_DB.json.<N>` for ASIC N on a multi-ASIC switch), lower-case
/// names (sonic-utilities test data), and an `asic<N>/` folder.
fn db_file(dir: &Path, name: &str, asic: Option<u32>) -> Option<PathBuf> {
    let lower = name.to_lowercase();
    let candidates: Vec<PathBuf> = match asic {
        None => vec![
            dir.join(format!("{name}.json")),
            dir.join(format!("{lower}.json")),
        ],
        Some(n) => vec![
            dir.join(format!("{name}.json.{n}")),
            dir.join(format!("{lower}.json.{n}")),
            dir.join(format!("asic{n}")).join(format!("{name}.json")),
            dir.join(format!("asic{n}")).join(format!("{lower}.json")),
        ],
    };
    candidates.into_iter().find(|p| p.is_file())
}

impl BundleLayout {
    /// Accept the bundle root (with `dump/` and `log/`), the `dump/` folder itself, or
    /// any folder holding the per-database files.
    pub fn locate(path: &Path, asic: Option<u32>) -> Result<Self, String> {
        if !path.is_dir() {
            return Err(format!("{}: not a directory", path.display()));
        }
        let (db_dir, root) = if has_db_files(path, asic) {
            (path.to_path_buf(), path.parent().map(Path::to_path_buf))
        } else if has_db_files(&path.join("dump"), asic) {
            (path.join("dump"), Some(path.to_path_buf()))
        } else {
            let which = match asic {
                Some(n) => format!(" for ASIC {n} (looked for <DB>.json.{n} and asic{n}/)"),
                None => String::new(),
            };
            return Err(format!(
                "{}: no <DB>.json dump files found{which}; point --snapshot at the bundle, its dump/ folder, or a folder of dumps",
                path.display()
            ));
        };
        // Logs sit next to the dumps, or in the bundle's log/ folder.
        let log_dir = [Some(db_dir.clone()), root.map(|r| r.join("log"))]
            .into_iter()
            .flatten()
            .find(|d| {
                d.is_dir()
                    && LOG_NAMES
                        .iter()
                        .flat_map(|n| [n.to_string(), format!("{n}.gz")])
                        .any(|f| d.join(f).is_file())
            });
        Ok(Self { db_dir, log_dir })
    }

    /// `<log_dir>/<name>` or `<log_dir>/<name>.gz`, if present.
    pub fn log_file(&self, name: &str) -> Option<PathBuf> {
        let dir = self.log_dir.as_ref()?;
        [dir.join(name), dir.join(format!("{name}.gz"))]
            .into_iter()
            .find(|p| p.is_file())
    }
}

const LOG_NAMES: [&str; 3] = ["swss.rec", "sairedis.rec", "syslog"];

/// Redis databases keyed by name (CONFIG_DB, APPL_DB, ASIC_DB, ...),
/// each a map of key -> hash fields.
#[derive(Debug, Default)]
pub struct Snapshot {
    dbs: BTreeMap<String, BTreeMap<String, Fields>>,
}

impl Snapshot {
    /// Parse `{ "CONFIG_DB": { "<key>": {field: value} | {"type":"hash","value":{..}} } }`.
    /// The wrapped form is what `sonic-db-dump` emits. Never panics on bad input.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
        let root = root
            .as_object()
            .ok_or("top level must be an object of databases")?;
        let mut dbs = BTreeMap::new();
        for (db, keys) in root {
            let keys = keys
                .as_object()
                .ok_or_else(|| format!("{db}: expected an object"))?;
            let mut table = BTreeMap::new();
            for (key, entry) in keys {
                let hash = match entry.as_object() {
                    Some(obj) if is_wrapped(obj) => obj.get("value").unwrap_or(entry),
                    _ => entry,
                };
                let mut fields = Fields::new();
                if let Some(obj) = hash.as_object() {
                    for (f, v) in obj {
                        let s = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        fields.insert(f.clone(), s);
                    }
                }
                table.insert(key.clone(), fields);
            }
            dbs.insert(db.clone(), table);
        }
        Ok(Self { dbs })
    }

    /// Load a multi-database JSON file, or a bundle directory (see [`Self::from_dir`]),
    /// reading at most `max_bytes` in total.
    pub fn from_path(path: &Path, max_bytes: u64) -> Result<Self, String> {
        Self::from_path_budget(path, None, &mut { max_bytes })
    }

    /// Like [`Self::from_path`], for ASIC `asic` of a multi-ASIC bundle, deducting what
    /// it reads from a shared `budget`.
    pub fn from_path_budget(
        path: &Path,
        asic: Option<u32>,
        budget: &mut u64,
    ) -> Result<Self, String> {
        if path.is_dir() {
            let layout = BundleLayout::locate(path, asic)?;
            Self::from_dir_budget(&layout.db_dir, asic, budget)
        } else if asic.is_some() {
            Err(format!(
                "{}: --asic needs a bundle directory, not a single file",
                path.display()
            ))
        } else {
            let text = read_limited(path, budget)?;
            Self::from_json(&text)
        }
    }

    /// Load a directory holding one dump per database (`CONFIG_DB.json`, `APPL_DB.json`,
    /// ...), as `generate_dump` writes them in its `dump/` folder. Missing files are
    /// skipped; at least one must exist.
    pub fn from_dir(dir: &Path) -> Result<Self, String> {
        Self::from_dir_budget(dir, None, &mut { DEFAULT_MAX_INPUT_BYTES })
    }

    fn from_dir_budget(dir: &Path, asic: Option<u32>, budget: &mut u64) -> Result<Self, String> {
        let mut snap = Self::default();
        let mut found = false;
        for name in DB_NAMES {
            let Some(path) = db_file(dir, name, asic) else {
                continue;
            };
            let text = read_limited(&path, budget)?;
            let wrapped = format!("{{\"{name}\":{text}}}");
            let one = Self::from_json(&wrapped).map_err(|e| format!("{}: {e}", path.display()))?;
            snap.dbs.extend(one.dbs);
            found = true;
        }
        if found {
            Ok(snap)
        } else {
            Err(format!("{}: no <DB>.json dump files found", dir.display()))
        }
    }

    /// Live mode: run `<cmd> -n <DB> [--netns asicN]` for each database and parse its
    /// JSON output (this is how `sonic-db-dump` is called by sonic-utilities). The
    /// databases are dumped one after another, so the snapshot is not atomic.
    pub fn from_dump_command(
        cmd: &str,
        asic: Option<u32>,
        budget: &mut u64,
    ) -> Result<Self, String> {
        use std::process::{Command, Stdio};
        let mut snap = Self::default();
        for name in DB_NAMES {
            let mut c = Command::new(cmd);
            c.args(["-n", name]);
            if let Some(n) = asic {
                c.args(["--netns", &format!("asic{n}")]);
            }
            // stderr is inherited so the tool's own error messages reach the user.
            let mut child = c
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("{cmd}: {e}"))?;
            let mut text = String::new();
            let n = match child.stdout.take() {
                Some(out) => {
                    out.take(budget.saturating_add(1))
                        .read_to_string(&mut text)
                        .map_err(|e| format!("{cmd} -n {name}: {e}"))? as u64
                }
                None => 0,
            };
            if n > *budget {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{cmd} -n {name}: output exceeds the size limit (raise it with --max-input-mb)"
                ));
            }
            *budget -= n;
            let status = child.wait().map_err(|e| format!("{cmd}: {e}"))?;
            if !status.success() {
                return Err(format!("{cmd} -n {name} failed ({status})"));
            }
            let wrapped = format!("{{\"{name}\":{text}}}");
            let one = Self::from_json(&wrapped).map_err(|e| format!("{cmd} -n {name}: {e}"))?;
            snap.dbs.extend(one.dbs);
        }
        Ok(snap)
    }

    pub fn get(&self, db: &str, key: &str) -> Option<&Fields> {
        self.dbs.get(db)?.get(key)
    }

    pub fn iter<'a>(&'a self, db: &str) -> Box<dyn Iterator<Item = (&'a String, &'a Fields)> + 'a> {
        match self.dbs.get(db) {
            Some(t) => Box::new(t.iter()),
            None => Box::new(std::iter::empty()),
        }
    }
}
