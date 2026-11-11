use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub type Fields = BTreeMap<String, String>;

/// Default cap on the total bytes read from input files.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

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

/// Redis databases keyed by name (CONFIG_DB, APPL_DB, ASIC_DB, ...),
/// each a map of key -> hash fields.
#[derive(Debug, Default)]
pub struct Snapshot {
    dbs: BTreeMap<String, BTreeMap<String, Fields>>,
}

impl Snapshot {
    /// Parse `{ "CONFIG_DB": { "<key>": {field: value} | {"type":"hash","value":{..}} } }`.
    /// The wrapped form is what `sonic-db-dump -y` emits. Never panics on bad input.
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
                let hash = match entry.get("value") {
                    Some(v) if entry.get("type").is_some() => v,
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
        let mut budget = max_bytes;
        if path.is_dir() {
            Self::from_dir_budget(path, &mut budget)
        } else {
            let text = read_limited(path, &mut budget)?;
            Self::from_json(&text)
        }
    }

    /// Load a bundle directory holding one redisdl dump per database
    /// (`CONFIG_DB.json`, `APPL_DB.json`, ...), as `generate_dump` writes them.
    /// Missing files are skipped; at least one must exist.
    pub fn from_dir(dir: &Path) -> Result<Self, String> {
        Self::from_dir_budget(dir, &mut DEFAULT_MAX_INPUT_BYTES.clone())
    }

    fn from_dir_budget(dir: &Path, budget: &mut u64) -> Result<Self, String> {
        const NAMES: [&str; 4] = ["CONFIG_DB", "APPL_DB", "STATE_DB", "ASIC_DB"];
        let mut snap = Self::default();
        let mut found = false;
        for name in NAMES {
            // `generate_dump` writes CONFIG_DB.json; sonic-utilities test data uses config_db.json.
            let path = [
                format!("{name}.json"),
                format!("{}.json", name.to_lowercase()),
            ]
            .into_iter()
            .map(|f| dir.join(f))
            .find(|p| p.is_file());
            let Some(path) = path else { continue };
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
