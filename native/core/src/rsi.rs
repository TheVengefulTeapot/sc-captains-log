//! Rust port of rsi.js: checks a handle against its public RSI citizen page
//! and, when it doesn't exist, asks Spectrum's member search for close
//! matches. Results go back to JS as JSON text with the same keys, key order
//! and values as rsi.js returns.
//!
//! Handles are limited to [A-Za-z0-9_-], so the string slicing in suggest()
//! is plain ASCII and matches JS index for index. Names coming back from
//! Spectrum may not be ASCII; their lengths are measured in UTF-16 units like
//! JS's .length, so ties sort the same way.

#[cfg(feature = "node")]
use napi::{bindgen_prelude::AsyncTask, Env, JsString, Task};
#[cfg(feature = "node")]
use crate::Result;
#[cfg(feature = "node")]
use napi_derive::napi;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::market::urlencode;

const RSI: &str = "https://robertsspaceindustries.com";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) SCCaptainsLog";

static RESULTS: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
static SEARCHES: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
fn results() -> &'static Mutex<HashMap<String, Value>> { RESULTS.get_or_init(Default::default) }
fn searches() -> &'static Mutex<HashMap<String, Vec<String>>> { SEARCHES.get_or_init(Default::default) }

fn re(pat: &str) -> Regex { Regex::new(pat).expect("valid regex") }

fn agent() -> std::result::Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .user_agent(UA)
        .timeout(Duration::from_secs(30))
        .build())
}

// decode() in rsi.js: a few HTML entities, whitespace collapsed, trimmed.
fn decode(s: Option<&str>) -> String {
    let s = s.unwrap_or("")
        .replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'");
    static WS: OnceLock<Regex> = OnceLock::new();
    WS.get_or_init(|| re(r"\s+")).replace_all(&s, " ").trim().to_string()
}

fn cap1<'a>(r: &Regex, s: &'a str) -> Option<&'a str> {
    r.captures(s).and_then(|c| c.get(1)).map(|m| m.as_str())
}

fn lookup_citizen(agent: &ureq::Agent, handle: &str) -> std::result::Result<Option<Map<String, Value>>, String> {
    let res = match agent.get(&format!("{}/citizens/{}", RSI, urlencode(handle))).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(ureq::Error::Status(code, _)) => return Err(format!("RSI returned {}", code)),
        Err(e) => return Err(e.to_string()),
    };
    let mut body = Vec::new();
    res.into_reader().read_to_end(&mut body).map_err(|e| e.to_string())?;
    let html = re(r"\s+").replace_all(&String::from_utf8_lossy(&body), " ").into_owned();
    let field = |label: &str| {
        let r = re(&format!(r#"<span class="label">{}</span> ?<strong class="value[^"]*">([^<]*)"#, regex::escape(label)));
        decode(cap1(&r, &html))
    };
    let handle_name = field("Handle name");
    let moniker = decode(cap1(&re(r#"<div class="info"> ?<p class="entry"> ?<strong class="value">([^<]*)"#), &html));
    let enlisted = field("Enlisted");
    let mut avatar = cap1(&re(r#"<div class="thumb"> ?<img src="([^"]+)""#), &html).unwrap_or("").to_string();
    if avatar.starts_with('/') { avatar = format!("{}{}", RSI, avatar); }
    let out_handle = if handle_name.is_empty() { handle.to_string() } else { handle_name };
    let (mut org_name, mut org_sid, mut org_rank) = (String::new(), String::new(), String::new());
    // Main org block; absent (or redacted) when the player hides it. Its
    // "dataN" class names are shuffled per request, so match on labels.
    // .{0,2000} counts characters here and UTF-16 units in JS; the org block
    // is ASCII markup, so both take the same span.
    let org = cap1(&re(r"main-org(.{0,2000})"), &html).unwrap_or("");
    if !org.is_empty() {
        if let Some(m) = re(r#"<a href="/orgs/([^"]*)" class="value[^"]*"[^>]*>([^<]*)"#).captures(org) {
            org_sid = decode(m.get(1).map(|x| x.as_str()));
            org_name = decode(m.get(2).map(|x| x.as_str()));
        }
        org_rank = decode(cap1(&re(r#"Organization rank</span> ?<strong class="value[^"]*">([^<]*)"#), org));
    }
    let mut o = Map::new();
    o.insert("found".into(), json!(true));
    o.insert("handle".into(), json!(out_handle));
    o.insert("moniker".into(), json!(moniker));
    o.insert("enlisted".into(), json!(enlisted));
    o.insert("avatar".into(), json!(avatar));
    o.insert("orgName".into(), json!(org_name));
    o.insert("orgSid".into(), json!(org_sid));
    o.insert("orgRank".into(), json!(org_rank));
    o.insert("url".into(), json!(format!("{}/citizens/{}", RSI, urlencode(&out_handle))));
    Ok(Some(o))
}

fn search_members(agent: &ureq::Agent, text: &str, page: u32) -> std::result::Result<Vec<String>, String> {
    let key = format!("{}|{}", text.to_lowercase(), page);
    if let Some(hit) = searches().lock().unwrap().get(&key) { return Ok(hit.clone()); }
    let body = json!({ "community_id": null, "text": text, "page": page }).to_string();
    let res = match agent.post(&format!("{}/api/spectrum/search/member/autocomplete", RSI))
        .set("Content-Type", "application/json")
        .send_string(&body)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("Spectrum returned {}", code)),
        Err(e) => return Err(e.to_string()),
    };
    let mut raw = Vec::new();
    res.into_reader().read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let names: Vec<String> = json.get("data").and_then(|d| d.get("members")).and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|m| m.get("nickname").and_then(|n| n.as_str()))
            .filter(|n| !n.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    searches().lock().unwrap().insert(key, names.clone());
    Ok(names)
}

pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<u16> = a.to_lowercase().encode_utf16().collect();
    let b: Vec<u16> = b.to_lowercase().encode_utf16().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let tmp = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + if a[i - 1] == b[j - 1] { 0 } else { 1 });
            prev = tmp;
        }
    }
    row[b.len()]
}

// Names in first-seen order, as a JS Set keeps them.
#[derive(Default)]
struct Seen { list: Vec<String>, set: HashSet<String> }
impl Seen {
    fn add(&mut self, names: &[String]) {
        for n in names { if self.set.insert(n.clone()) { self.list.push(n.clone()); } }
    }
}

// String.prototype.slice for ASCII text: indexes clamp, start > end is "".
fn slice(s: &str, a: usize, b: usize) -> &str {
    let b = b.min(s.len());
    let a = a.min(b);
    &s[a..b]
}
// s[i] in JS: one character, or the text "undefined" past the end.
fn at(s: &str, i: usize) -> String {
    s.get(i..i + 1).map(String::from).unwrap_or_else(|| "undefined".into())
}

fn suggest(agent: &ureq::Agent, handle: &str) -> std::result::Result<Vec<String>, String> {
    let mut seen = Seen::default();
    // Binary search for the longest prefix that still starts someone's
    // handle. The search also matches display names, which would hide the
    // typo, so only handle matches count here.
    let (mut lo, mut hi, mut best) = (3i64, handle.len() as i64, 0usize);
    while lo <= hi {
        let mid = ((lo + hi) >> 1) as usize;
        let prefix = slice(handle, 0, mid).to_lowercase();
        let names = search_members(agent, &prefix, 1)?;
        seen.add(&names);
        if names.iter().any(|n| n.to_lowercase().starts_with(&prefix)) { best = mid; lo = mid as i64 + 1; } else { hi = mid as i64 - 1; }
    }
    if best == handle.len() { return Ok(rank(&seen, handle)); }
    if best > 0 { seen.add(&search_members(agent, slice(handle, 0, best), 2)?); }
    let p = best.max(1);
    let tail = (handle.len() + 1).min(p + 5);
    let h = handle;
    let mut variants: Vec<String> = vec![];
    let mut vset = HashSet::new();
    let mut push = |v: String| { if vset.insert(v.clone()) { variants.push(v); } };
    push(format!("{}{}", slice(h, 0, p), slice(h, p + 1, usize::MAX)));                         // extra letter
    push(format!("{}{}", slice(h, 0, p - 1), slice(h, p, usize::MAX)));                         // extra letter just before
    push(format!("{}{}{}{}", slice(h, 0, p), at(h, p + 1), at(h, p), slice(h, p + 2, usize::MAX))); // swapped pair
    push(format!("{}{}{}{}", slice(h, 0, p - 1), at(h, p), at(h, p - 1), slice(h, p + 1, usize::MAX)));
    for ch in format!("aeiouy{}", at(h, p - 1)).chars() {                                         // missed letter
        push(format!("{}{}{}", slice(h, 0, p), ch, slice(h, p, usize::MAX)));
    }
    let mut probes: Vec<String> = vec![];
    let mut pset = HashSet::new();
    for v in variants.iter().filter(|v| !v.is_empty() && !v.contains("undefined") && v.len() >= 3) {
        let cut = slice(v, 0, tail).to_string();
        if pset.insert(cut.clone()) { probes.push(cut); }
    }
    // Probes run side by side (Promise.all) and are added in order.
    let found: Vec<Vec<String>> = std::thread::scope(|s| {
        let hs: Vec<_> = probes.iter().map(|v| s.spawn(move || search_members(agent, v, 1).unwrap_or_default())).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    for names in &found { seen.add(names); }
    Ok(rank(&seen, handle))
}

fn rank(seen: &Seen, handle: &str) -> Vec<String> {
    let limit = 3usize.max(handle.len() / 3);
    let mut v: Vec<(&String, usize)> = seen.list.iter().map(|n| (n, distance(n, handle))).filter(|x| x.1 <= limit).collect();
    v.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.encode_utf16().count().cmp(&b.0.encode_utf16().count())));
    v.into_iter().take(5).map(|x| x.0.clone()).collect()
}

fn valid_handle(h: &str) -> bool {
    (3..=60).contains(&h.len()) && h.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

pub fn check(raw: &str) -> Value {
    let handle = raw.trim();
    if !valid_handle(handle) { return json!({ "status": "invalid", "handle": handle }); }
    let key = handle.to_lowercase();
    if let Some(hit) = results().lock().unwrap().get(&key) { return hit.clone(); }
    let attempt = agent().and_then(|a| match lookup_citizen(&a, handle)? {
        Some(c) => {
            let mut o = Map::new();
            o.insert("status".into(), json!("found"));
            o.extend(c);
            Ok(Value::Object(o))
        }
        None => Ok(json!({ "status": "notfound", "handle": handle, "suggestions": suggest(&a, handle)? })),
    });
    match attempt {
        Ok(result) => {
            results().lock().unwrap().insert(key, result.clone());
            result
        }
        Err(message) => json!({ "status": "error", "handle": handle, "message": message }),
    }
}

#[cfg(feature = "node")]
pub struct CheckTask { raw: String }

#[cfg(feature = "node")]
impl Task for CheckTask {
    type Output = String;
    type JsValue = JsString;
    fn compute(&mut self) -> Result<String> { crate::guard(|| Ok(check(&self.raw).to_string())) }
    fn resolve(&mut self, env: Env, out: String) -> Result<JsString> { env.create_string(&out) }
}

/// Resolves to the result as JSON text, same shape as rsi.js checkHandle().
#[cfg(feature = "node")]
#[napi]
pub fn check_handle(raw: String) -> AsyncTask<CheckTask> {
    AsyncTask::new(CheckTask { raw })
}

/// Edit distance, exposed so the JS and Rust versions can be compared.
#[cfg(feature = "node")]
#[napi(js_name = "rsiDistance")]
pub fn rsi_distance(a: String, b: String) -> u32 {
    distance(&a, &b) as u32
}
