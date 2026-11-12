//! Read a `generate_dump` archive (`sonic_dump_<host>_<time>.tar.gz`) directly.
//!
//! Only the files the tool uses are taken out of the archive: the database dumps
//! (`<bundle>/dump/<DB>.json[.N]`) and the logs (`<bundle>/log/{swss.rec,sairedis.rec,
//! syslog}[.N][.gz]`). They are written to a private temporary folder laid out like an
//! unpacked bundle, which is removed when the [`Extracted`] value is dropped.
//!
//! The archive is untrusted: entry paths are never used as output paths. Each file is
//! written under a name rebuilt from a fixed pattern, and the total written is capped.
use std::io::Read;
use std::path::{Path, PathBuf};

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const DB_PREFIXES: [&str; 4] = ["CONFIG_DB", "APPL_DB", "STATE_DB", "ASIC_DB"];
const LOG_NAMES: [&str; 3] = ["swss.rec", "sairedis.rec", "syslog"];

/// True when `path` is a file that looks like a tar archive, plain or gzip-compressed:
/// a `.tar`, `.tar.gz` or `.tgz` name, or gzip content (a JSON dump never starts with it).
pub fn is_archive(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return true;
    }
    let mut head = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == GZIP_MAGIC)
}

/// A bundle unpacked into a private temporary folder; removed on drop.
#[derive(Debug)]
pub struct Extracted {
    root: PathBuf,
    /// Log files left out because the size cap was reached.
    pub skipped_logs: usize,
}

impl Extracted {
    /// The folder to pass to [`crate::snapshot::BundleLayout::locate`]: it holds `dump/`
    /// and `log/`.
    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Extracted {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `APPL_DB.json` or `APPL_DB.json.<N>` (generate_dump's names), else `None`.
fn db_name(file: &str) -> Option<String> {
    let (stem, rest) = file.split_once(".json")?;
    let ok_rest = rest.is_empty()
        || rest
            .strip_prefix('.')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (DB_PREFIXES.contains(&stem) && ok_rest).then(|| file.to_string())
}

/// `<log>`, `<log>.gz`, `<log>.<N>` or `<log>.<N>.gz` for the logs the tool reads.
fn log_name(file: &str) -> Option<String> {
    LOG_NAMES.iter().find_map(|log| {
        let rest = file.strip_prefix(log)?;
        let rest = rest.strip_suffix(".gz").unwrap_or(rest);
        let ok = rest.is_empty()
            || rest
                .strip_prefix('.')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        ok.then(|| file.to_string())
    })
}

/// A new, empty, private folder under the system temporary directory.
fn private_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for i in 0..100u32 {
        let dir = base.join(format!("sonic-trace-{}-{nanos}-{i}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create a temporary folder: {e}")),
        }
    }
    Err("cannot create a temporary folder".into())
}

/// Copy at most `cap` bytes of `entry` to `dest`; `Ok(None)` when it is larger.
fn copy_capped(entry: impl Read, dest: &Path, cap: u64) -> Result<Option<u64>, String> {
    let mut buf = Vec::new();
    let n = entry
        .take(cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading the archive: {e}"))? as u64;
    if n > cap {
        return Ok(None);
    }
    std::fs::write(dest, &buf).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(Some(n))
}

/// Unpack the database dumps and logs of a `generate_dump` archive, writing at most
/// `max_bytes` in total. A database dump that does not fit is an error; logs that do not
/// fit are left out and counted in [`Extracted::skipped_logs`].
pub fn extract_bundle(archive: &Path, max_bytes: u64) -> Result<Extracted, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut head = [0u8; 2];
    let gz = std::fs::File::open(archive)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == GZIP_MAGIC);
    let reader: Box<dyn Read> = if gz {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };

    let root = private_dir()?;
    // Created now so that a failure below still cleans up through Drop.
    let mut out = Extracted {
        root,
        skipped_logs: 0,
    };
    let (dump_dir, log_dir) = (out.root.join("dump"), out.root.join("log"));
    for d in [&dump_dir, &log_dir] {
        std::fs::create_dir(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }

    let bad = |e: std::io::Error| format!("{}: not a readable tar archive: {e}", archive.display());
    let mut tar = tar::Archive::new(reader);
    let mut left = max_bytes;
    let mut dbs = 0;
    for entry in tar.entries().map_err(bad)? {
        let entry = entry.map_err(bad)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(bad)?.into_owned();
        let mut parts = path.iter().rev().filter_map(|c| c.to_str());
        let (Some(file), Some(parent)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (dest, is_db) = match parent {
            "dump" => match db_name(file) {
                Some(n) => (dump_dir.join(n), true),
                None => continue,
            },
            "log" => match log_name(file) {
                Some(n) => (log_dir.join(n), false),
                None => continue,
            },
            _ => continue,
        };
        match copy_capped(entry, &dest, left)? {
            Some(n) => {
                left -= n;
                dbs += usize::from(is_db);
            }
            None if is_db => {
                return Err(format!(
                    "{}: {file} exceeds the size limit (raise it with --max-input-mb)",
                    archive.display()
                ));
            }
            None => out.skipped_logs += 1,
        }
    }
    if dbs == 0 {
        return Err(format!(
            "{}: no dump/<DB>.json files in this archive; is it a generate_dump bundle?",
            archive.display()
        ));
    }
    Ok(out)
}
