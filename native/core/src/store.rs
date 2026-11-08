//! Rust port of store.js: the Captain's Log data file, loaded into Electron as
//! a native module. Behaviour matches store.js exactly, file for file:
//!
//!   captains-log.json        current data
//!   captains-log.prev.json   the save before the current one
//!   backups/captains-log-YYYY-MM-DD.json   first save of each day (14 kept)
//!
//! A file that exists but isn't a JSON object is renamed to
//! *.corrupt-<ms>.json instead of being overwritten.

use crate::{Error, Result};
use napi_derive::napi;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const KEEP_DAYS: usize = 14;

#[napi(object)]
pub struct LoadResult {
    pub json: Option<String>,
    pub source: String,
    pub warnings: Vec<String>,
}

#[napi(object)]
pub struct StoreInfo {
    pub dir: String,
    pub file: String,
    pub last_saved: Option<String>,
    pub backups: Vec<String>,
}

#[napi]
pub struct Store {
    dir: PathBuf,
    file: PathBuf,
    prev: PathBuf,
    backup_dir: PathBuf,
    last_saved: Option<String>,
}

fn err<E: std::fmt::Display>(e: E) -> Error {
    Error::from_reason(e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// Same as JavaScript's new Date(ms).toISOString() (UTC).
pub fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86400) as i64;
    let sod = secs % 86400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, sod / 3600, (sod / 60) % 60, sod % 60, ms % 1000
    )
}

// JSON check with the same rules as JavaScript's JSON.parse, plus "the top
// level is an object". Written by hand rather than using serde_json because
// serde rejects lone surrogate escapes (\ud800) that JSON.stringify can write
// and JSON.parse accepts; a stricter check here would wrongly set a good save
// aside as corrupt.
struct Json<'a> { b: &'a [u8], i: usize }
impl<'a> Json<'a> {
    fn ws(&mut self) { while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') { self.i += 1; } }
    fn peek(&self) -> Option<u8> { self.b.get(self.i).copied() }
    fn eat(&mut self, c: u8) -> bool { if self.peek() == Some(c) { self.i += 1; true } else { false } }
    fn lit(&mut self, s: &[u8]) -> bool { if self.b[self.i..].starts_with(s) { self.i += s.len(); true } else { false } }
    fn digits(&mut self) -> bool { let s = self.i; while matches!(self.peek(), Some(b'0'..=b'9')) { self.i += 1; } self.i > s }
    fn string(&mut self) -> bool {
        if !self.eat(b'"') { return false; }
        loop {
            match self.peek() {
                None => return false,
                Some(b'"') => { self.i += 1; return true; }
                Some(b'\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            self.i += 1;
                            for _ in 0..4 { if !matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) { return false; } self.i += 1; }
                        }
                        _ => return false,
                    }
                }
                Some(c) if c < 0x20 => return false,
                Some(_) => self.i += 1,
            }
        }
    }
    fn number(&mut self) -> bool {
        self.eat(b'-');
        if self.eat(b'0') {} else if matches!(self.peek(), Some(b'1'..=b'9')) { self.digits(); } else { return false; }
        if self.eat(b'.') && !self.digits() { return false; }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if !self.eat(b'+') { self.eat(b'-'); }
            if !self.digits() { return false; }
        }
        true
    }
    fn value(&mut self, depth: u32) -> bool {
        if depth > 10_000 { return false; }
        self.ws();
        match self.peek() {
            Some(b'{') => {
                self.i += 1; self.ws();
                if self.eat(b'}') { return true; }
                loop {
                    self.ws();
                    if !self.string() { return false; }
                    self.ws();
                    if !self.eat(b':') || !self.value(depth + 1) { return false; }
                    self.ws();
                    if self.eat(b'}') { return true; }
                    if !self.eat(b',') { return false; }
                }
            }
            Some(b'[') => {
                self.i += 1; self.ws();
                if self.eat(b']') { return true; }
                loop {
                    if !self.value(depth + 1) { return false; }
                    self.ws();
                    if self.eat(b']') { return true; }
                    if !self.eat(b',') { return false; }
                }
            }
            Some(b'"') => self.string(),
            Some(b't') => self.lit(b"true"),
            Some(b'f') => self.lit(b"false"),
            Some(b'n') => self.lit(b"null"),
            _ => self.number(),
        }
    }
}
fn is_json_object(text: &str) -> bool {
    let mut p = Json { b: text.as_bytes(), i: 0 };
    p.ws();
    if p.peek() != Some(b'{') { return false; }
    if !p.value(0) { return false; }
    p.ws();
    p.i == p.b.len()
}
fn is_json(text: &str) -> bool {
    let mut p = Json { b: text.as_bytes(), i: 0 };
    if !p.value(0) { return false; }
    p.ws();
    p.i == p.b.len()
}

enum Read { Missing, Text(String), Corrupt(PathBuf) }

fn read_json(p: &Path) -> Result<Read> {
    if !p.exists() { return Ok(Read::Missing); }
    let bytes = fs::read(p).map_err(err)?;
    let text = String::from_utf8_lossy(&bytes).into_owned(); // as readFileSync(p, 'utf8')
    if is_json_object(&text) { return Ok(Read::Text(text)); }
    let stem = p.to_string_lossy();
    let stem = stem.strip_suffix(".json").unwrap_or(&stem);
    let bad = PathBuf::from(format!("{}.corrupt-{}.json", stem, now_ms()));
    let _ = fs::rename(p, &bad);
    Ok(Read::Corrupt(bad))
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn is_backup_name(n: &str) -> bool {
    // ^captains-log-\d{4}-\d{2}-\d{2}\.json$
    let Some(mid) = n.strip_prefix("captains-log-").and_then(|r| r.strip_suffix(".json")) else { return false };
    let b = mid.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn write_atomic(p: &Path, text: &str) -> Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", p.to_string_lossy()));
    {
        let mut f = File::create(&tmp).map_err(err)?;
        f.write_all(text.as_bytes()).map_err(err)?;
        f.sync_all().map_err(err)?;
    }
    fs::rename(&tmp, p).map_err(err)
}

impl Store {
    fn backups(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.backup_dir) else { return vec![] };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| is_backup_name(n))
            .collect();
        names.sort();
        names.reverse();
        names.into_iter().map(|n| self.backup_dir.join(n)).collect()
    }

    fn daily_backup(&self) -> Result<()> {
        if !self.file.exists() { return Ok(()); }
        fs::create_dir_all(&self.backup_dir).map_err(err)?;
        let day = &iso(now_ms())[..10];
        let target = self.backup_dir.join(format!("captains-log-{}.json", day));
        if !target.exists() { fs::copy(&self.file, &target).map_err(err)?; }
        for old in self.backups().into_iter().skip(KEEP_DAYS) { let _ = fs::remove_file(old); }
        Ok(())
    }
}

#[napi]
impl Store {
    #[napi(constructor)]
    pub fn new(dir: String) -> Self {
        let dir = PathBuf::from(dir);
        Store {
            file: dir.join("captains-log.json"),
            prev: dir.join("captains-log.prev.json"),
            backup_dir: dir.join("backups"),
            dir,
            last_saved: None,
        }
    }

    #[napi(getter)]
    pub fn dir(&self) -> String { self.dir.to_string_lossy().into_owned() }

    #[napi(getter)]
    pub fn file(&self) -> String { self.file.to_string_lossy().into_owned() }

    /// The newest readable copy: current file, then previous save, then the
    /// daily backups newest first. json is null when there is no data at all.
    #[napi]
    pub fn load(&self) -> Result<LoadResult> { crate::guard(|| self.load_inner()) }
    fn load_inner(&self) -> Result<LoadResult> {
        let mut warnings = vec![];
        let mut order = vec![(self.file.clone(), "file".to_string()), (self.prev.clone(), "previous save".to_string())];
        for b in self.backups() { let n = name_of(&b); order.push((b, format!("backup {}", n))); }
        for (p, source) in order {
            match read_json(&p)? {
                Read::Text(t) => return Ok(LoadResult { json: Some(t), source, warnings }),
                Read::Corrupt(bad) => warnings.push(format!("{} was unreadable; kept as {}", name_of(&p), name_of(&bad))),
                Read::Missing => {}
            }
        }
        Ok(LoadResult { json: None, source: "none".into(), warnings })
    }

    /// Writes the data; refuses anything that wouldn't load back. Returns the
    /// save time as an ISO string.
    #[napi]
    pub fn save(&mut self, text: String) -> Result<String> { crate::guard(|| self.save_inner(text)) }
    fn save_inner(&mut self, text: String) -> Result<String> {
        if !is_json(&text) { return Err(Error::from_reason("refusing to save data that is not valid JSON")); }
        self.daily_backup()?;
        if self.file.exists() { fs::copy(&self.file, &self.prev).map_err(err)?; }
        write_atomic(&self.file, &text)?;
        let at = iso(now_ms());
        self.last_saved = Some(at.clone());
        Ok(at)
    }

    #[napi]
    pub fn write_atomic(&self, path: String, text: String) -> Result<()> {
        write_atomic(Path::new(&path), &text)
    }

    #[napi]
    pub fn info(&self) -> StoreInfo {
        StoreInfo {
            dir: self.dir(),
            file: self.file(),
            last_saved: self.last_saved.clone(),
            backups: self.backups().iter().map(|b| name_of(b)).collect(),
        }
    }
}
