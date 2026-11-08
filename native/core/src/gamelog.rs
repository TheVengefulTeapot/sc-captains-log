//! Rust port of gamelog.js: the Game.log line parser, the file tailer's
//! reading side, and the logbackups sighting scan. Events go back to JS as
//! JSON text with the same keys, key order and values gamelog.js produces.
//!
//! Regex notes, so the patterns match exactly what the JS ones match:
//! - Rust's \d and \w are Unicode by default; JS's are ASCII. Patterns use
//!   [0-9] and [A-Za-z0-9_] instead.
//! - JS's `.` stops at \r as well as \n; (?R) makes Rust's do the same.
//! - Rust's regex has no lookahead, so the one JS lookahead, in
//!   /^Neutralize (?!Targets?$)(\S+)$/, is done as a check on the capture.

#[cfg(feature = "node")]
use napi::{bindgen_prelude::AsyncTask, Env, JsString, Task};
use crate::Result;
use napi_derive::napi;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const NULL_ID: &str = "00000000-0000-0000-0000-000000000000";

struct Res {
    notif: Regex, em: Regex, ws: Regex, trail: Regex,
    contract: Regex, awarded: Regex, jurisdiction: Regex, ship: Regex,
    p_join: Regex, p_left: Regex, p_leader: Regex, objective: Regex, transaction: Regex,
    buy: Regex, flow: Regex, carriage: Regex, status_fx: Regex, comms: Regex, org_tag: Regex,
    end_mission: Regex, dead: Regex, corpse: Regex, shared: Regex, join_pu: Regex, region: Regex,
    login: Regex, location: Regex, neutralize: Regex, bounty_title: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| {
        let r = |p: &str| Regex::new(&format!("(?R){}", p)).expect("valid regex");
        Res {
            notif: r(r#"<SHUDEvent_OnNotification> Added notification "([\s\S]*?)" \[([0-9]+)\] to queue\..*?MissionId: \[([0-9a-f-]+)\]"#),
            em: r(r"</?EM[0-9]>"),
            ws: r(r"\s+"),
            trail: r(r"[:\s]+$"),
            contract: r(r"^Contract (Accepted|Complete|Failed|Shared|Available):\s*([\s\S]*)$"),
            awarded: r(r"^Awarded ([0-9,]+) aUEC"),
            jurisdiction: r(r"^Entered (.+?) Jurisdiction"),
            ship: r(r"^You have joined channel '(.+?) : (.+?)'"),
            p_join: r(r"^New Member Joined\s+(.+?) has joined the party"),
            p_left: r(r"^Member Left\s+(.+?) has left the party"),
            p_leader: r(r"^New Party Leader\s+(.+?) is now party leader"),
            objective: r(r"^(New Objective|Objective Complete):\s*([\s\S]*)$"),
            transaction: r(r"^Transaction (Complete|Error)"),
            buy: r(r"<CEntityComponentShop(?:UI|ping)Provider::Send(?:Shop|StandardItem)BuyRequest> .*?shopName\[([^\]]*)\].*?client_price\[([0-9.]+)\].*?itemName\[([^\]]*)\] quantity\[([0-9]+)\]"),
            flow: r(r"<CEntityComponentShop(?:UI|ping)Provider::RmShopFlowResponse> .*?result\[([A-Za-z0-9_]+)\]"),
            carriage: r(r"<Player entered carriage> \[TRANSPORT\] Player ([^\[\s]+)\["),
            status_fx: r(r"^Logged a start of a status effect! nickname: ([^,]+), status effect: (.+)$"),
            comms: r(r"<CommsNotifications> SendCommsNotification \+(\S+) - .*?Mission: \[([0-9a-f-]+)\]"),
            org_tag: r(r"^Missions\.Organization\.[^.]+$"),
            end_mission: r(r"<EndMission> .*?MissionId\[([0-9a-f-]+)\] Player\[([^\]]*)\].*?CompletionType\[([A-Za-z0-9_]+)\] Reason\[([^\]]*)\]"),
            dead: r(r"<\[ActorState\] Dead> .*?Actor '([^']+)' \[[0-9]+\] ejected from zone '([^']*)' \[[0-9]+\] to zone '([^']*)'(?:.*?due to (.+?)\.)?"),
            corpse: r(r"PopulateItemPortForItemRecoveryEntitlement\]> Item '[^']*Class\(body_"),
            shared: r(r"<MissionShared> Received share push message: ownerId\[([0-9]+)\] - missionId\[([0-9a-f-]+)\]"),
            join_pu: r(r"<Join PU> address\[([^\]]*)\] port\[[0-9]+\] shard\[([^\]]*)\]"),
            region: r(r"^pub_([a-z]+[0-9])"),
            login: r(r"<AccountLoginCharacterStatus_Character> .*? name (\S+)"),
            location: r(r"<RequestLocationInventory> Player\[([^\]]*)\] requested inventory for Location\[([^\]]*)\]"),
            neutralize: r(r"^Neutralize (\S+)$"),
            bounty_title: r(r"(?i)^(Verified|Reclusive) Bounty"),
        }
    })
}

fn clean_text(s: &str) -> String {
    let r = res();
    let s = r.em.replace_all(s, "");
    let s = r.ws.replace_all(&s, " ");
    r.trail.replace_all(&s, "").trim().to_string()
}

// JS parseInt(s, 10) for a string of digits; NaN (no digits) becomes null,
// as it does once the event crosses IPC as JSON.
fn parse_int(s: &str) -> Value {
    let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if d.is_empty() { return Value::Null; }
    d.parse::<u64>().map(|n| json!(n)).unwrap_or_else(|_| json!(d.parse::<f64>().unwrap_or(0.0)))
}
// Math.round(parseFloat(s)) for [0-9.]+ text.
fn round_float(s: &str) -> Value {
    let mut end = 0;
    let mut dot = false;
    let mut digits = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() { digits = true; end = i + 1; }
        else if c == '.' && !dot { dot = true; end = i + 1; }
        else { break; }
    }
    if !digits { return Value::Null; }
    let f: f64 = s[..end].trim_end_matches('.').parse().unwrap_or(0.0);
    let r = (f + 0.5).floor();
    if r.abs() < 9e15 { json!(r as i64) } else { json!(r) }
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs { m.insert(k.to_string(), v); }
    Value::Object(m)
}

fn g<'a>(c: &'a regex::Captures, i: usize) -> &'a str { c.get(i).map(|m| m.as_str()).unwrap_or("") }

fn parse_notification(text: &str, idx: &str, mission_id: &str, ts: &str) -> Option<Value> {
    let r = res();
    let mid = if !mission_id.is_empty() && mission_id != NULL_ID { json!(mission_id) } else { Value::Null };
    if let Some(m) = r.contract.captures(text) {
        return Some(obj(vec![("type", json!("contract")), ("status", json!(g(&m, 1).to_lowercase())), ("name", json!(clean_text(g(&m, 2)))), ("missionId", mid), ("ts", json!(ts))]));
    }
    if let Some(m) = r.awarded.captures(text) {
        return Some(obj(vec![("type", json!("payout")), ("amount", parse_int(&g(&m, 1).replace(',', ""))), ("key", json!(format!("{}|{}", ts, idx))), ("ts", json!(ts))]));
    }
    if let Some(m) = r.jurisdiction.captures(text) { return Some(obj(vec![("type", json!("jurisdiction")), ("name", json!(g(&m, 1))), ("ts", json!(ts))])); }
    if text.starts_with("Entering Armistice Zone") { return Some(obj(vec![("type", json!("zone")), ("zone", json!("armistice")), ("ts", json!(ts))])); }
    if text.starts_with("Leaving Armistice Zone") { return Some(obj(vec![("type", json!("zone")), ("zone", json!("open")), ("ts", json!(ts))])); }
    if text.starts_with("Entered Monitored Space") { return Some(obj(vec![("type", json!("monitored")), ("on", json!(true)), ("ts", json!(ts))])); }
    if text.starts_with("Exited Monitored Space") { return Some(obj(vec![("type", json!("monitored")), ("on", json!(false)), ("ts", json!(ts))])); }
    if let Some(m) = r.ship.captures(text) { return Some(obj(vec![("type", json!("ship")), ("ship", json!(g(&m, 1))), ("owner", json!(g(&m, 2))), ("ts", json!(ts))])); }
    for (re, action) in [(&r.p_join, "joined"), (&r.p_left, "left"), (&r.p_leader, "leader")] {
        if let Some(m) = re.captures(text) { return Some(obj(vec![("type", json!("party")), ("action", json!(action)), ("player", json!(g(&m, 1))), ("ts", json!(ts))])); }
    }
    if text.starts_with("Party Disbanded") { return Some(obj(vec![("type", json!("party")), ("action", json!("disbanded")), ("ts", json!(ts))])); }
    if text.starts_with("You have left the party") { return Some(obj(vec![("type", json!("party")), ("action", json!("self_left")), ("ts", json!(ts))])); }
    if let Some(m) = r.objective.captures(text) {
        let st = if g(&m, 1) == "New Objective" { "new" } else { "complete" };
        return Some(obj(vec![("type", json!("objective")), ("status", json!(st)), ("text", json!(clean_text(g(&m, 2)))), ("missionId", mid), ("ts", json!(ts))]));
    }
    if let Some(m) = r.transaction.captures(text) { return Some(obj(vec![("type", json!("transaction")), ("ok", json!(g(&m, 1) == "Complete")), ("ts", json!(ts))])); }
    None
}

// Every phrase the parser checks for, found in one pass over the line.
// Bit i is set when LITERALS[i] occurs anywhere in the text.
const LITERALS: [&str; 14] = ["BuyRequest> ", "::RmShopFlowResponse> ", "<Player entered carriage> ", "Logged a start of a status effect! nickname: ", "<CommsNotifications> SendCommsNotification +", "<SHUDEvent_OnNotification> Added notification", "<EndMission> ", "<[ActorState] Dead> ", "PopulateItemPortForItemRecoveryEntitlement]> Item '", "<MissionShared> Received share push message: ", "<Join PU> address[", "<AccountLoginCharacterStatus_Character> ", "<RequestLocationInventory> Player[", "to queue."];
fn phrases(text: &str) -> u32 {
    static AC: OnceLock<aho_corasick::AhoCorasick> = OnceLock::new();
    let ac = AC.get_or_init(|| aho_corasick::AhoCorasick::new(LITERALS).expect("valid phrases"));
    let mut mask = 0u32;
    for m in ac.find_overlapping_iter(text) { mask |= 1 << m.pattern().as_u32(); }
    mask
}
fn has(mask: u32, i: u32) -> bool { mask & (1 << i) != 0 }

struct Pending { ts: String, body: String, len16: usize }

/// Stateful line parser. Some notifications span several lines, so a
/// notification is buffered until its "to queue." tail arrives.
#[derive(Default)]
pub struct Parser { pending: Option<Pending>, pending_buy: Option<Value> }

impl Parser {
    pub fn reset(&mut self) { self.pending = None; self.pending_buy = None; }

    fn parse_full(&mut self, ts: &str, body: &str, mask: u32) -> Option<Value> {
        let r = res();
        if let Some(m) = has(mask, 0).then(|| r.buy.captures(body)).flatten() {
            self.pending_buy = Some(obj(vec![
                ("type", json!("purchase")), ("shop", json!(g(&m, 1))), ("price", round_float(g(&m, 2))),
                ("item", json!(g(&m, 3))), ("qty", parse_int(g(&m, 4))), ("key", json!(ts)), ("ts", json!(ts)),
            ]));
            return None;
        }
        if let Some(m) = has(mask, 1).then(|| r.flow.captures(body)).flatten() {
            let buy = self.pending_buy.take();
            return if g(&m, 1) == "Success" { buy } else { None };
        }
        if let Some(m) = has(mask, 2).then(|| r.carriage.captures(body)).flatten() {
            return Some(obj(vec![("type", json!("sighting")), ("player", json!(g(&m, 1))), ("source", json!("transit")), ("ts", json!(ts))]));
        }
        if let Some(m) = has(mask, 3).then(|| r.status_fx.captures(body)).flatten() {
            return Some(obj(vec![("type", json!("sighting")), ("player", json!(g(&m, 1).trim())), ("source", json!("nearby")), ("detail", json!(g(&m, 2).trim())), ("ts", json!(ts))]));
        }
        if let Some(m) = has(mask, 4).then(|| r.comms.captures(body)).flatten() {
            let tags: Vec<&str> = g(&m, 1).split(',').collect();
            let types: Vec<&str> = tags.iter().filter_map(|t| t.strip_prefix("Missions.MissionType.")).filter(|t| *t != "Generic").collect();
            let org = tags.iter().find(|t| r.org_tag.is_match(t)).map(|t| &t[22..]).unwrap_or("");
            return if !types.is_empty() || !org.is_empty() {
                Some(obj(vec![("type", json!("mission_info")), ("missionId", json!(g(&m, 2))), ("types", json!(types)), ("org", json!(org)), ("ts", json!(ts))]))
            } else { None };
        }
        if has(mask, 5) {
            return r.notif.captures(body).and_then(|m| parse_notification(g(&m, 1), g(&m, 2), g(&m, 3), ts));
        }
        if let Some(m) = has(mask, 6).then(|| r.end_mission.captures(body)).flatten() {
            return Some(obj(vec![("type", json!("mission_end")), ("missionId", json!(g(&m, 1))), ("player", json!(g(&m, 2))), ("result", json!(g(&m, 3).to_lowercase())), ("reason", json!(g(&m, 4))), ("ts", json!(ts))]));
        }
        // Your death, two ways: dying in a destroyed ship logs an ActorState
        // Dead line (with the ship), and every death logs recovery of your body item.
        if let Some(m) = has(mask, 7).then(|| r.dead.captures(body)).flatten() {
            return Some(obj(vec![("type", json!("actor_dead")), ("actor", json!(g(&m, 1))), ("fromZone", json!(g(&m, 2))), ("toZone", json!(g(&m, 3))), ("reason", json!(g(&m, 4))), ("ts", json!(ts))]));
        }
        if has(mask, 8) && r.corpse.is_match(body) { return Some(obj(vec![("type", json!("corpse")), ("ts", json!(ts))])); }
        if let Some(m) = has(mask, 9).then(|| r.shared.captures(body)).flatten() {
            return Some(obj(vec![("type", json!("mission_shared")), ("ownerId", json!(g(&m, 1))), ("missionId", json!(g(&m, 2))), ("ts", json!(ts))]));
        }
        if let Some(m) = has(mask, 10).then(|| r.join_pu.captures(body)).flatten() {
            let shard = g(&m, 2);
            let region = r.region.captures(shard).map(|c| g(&c, 1).to_string()).unwrap_or_default();
            return Some(obj(vec![("type", json!("session")), ("shard", json!(shard)), ("region", json!(region)), ("ts", json!(ts))]));
        }
        if let Some(m) = has(mask, 11).then(|| r.login.captures(body)).flatten() { return Some(obj(vec![("type", json!("login")), ("handle", json!(g(&m, 1))), ("ts", json!(ts))])); }
        if let Some(m) = has(mask, 12).then(|| r.location.captures(body)).flatten() { return Some(obj(vec![("type", json!("location")), ("code", json!(g(&m, 2))), ("ts", json!(ts))])); }
        None
    }

    pub fn feed(&mut self, line: &str) -> Option<Value> {
        let tm = split_ts(line);
        if let Some(p) = self.pending.as_mut() {
            // Continuation lines carry their own timestamp prefix; drop it.
            let add = match &tm { Some((_, b)) => *b, None => line };
            p.body.push('\n');
            p.body.push_str(add);
            p.len16 += 1 + add.encode_utf16().count();
            if p.body.contains("to queue.") || p.len16 > 4000 {
                let p = self.pending.take().unwrap();
                let mask = phrases(&p.body);
                return self.parse_full(&p.ts, &p.body, mask);
            }
            return None;
        }
        let (ts, body) = tm?;
        let mask = phrases(body);
        if has(mask, 5) && !has(mask, 13) {
            let len16 = body.encode_utf16().count();
            self.pending = Some(Pending { ts: ts.to_string(), body: body.to_string(), len16 });
            return None;
        }
        self.parse_full(ts, body, mask)
    }
}

// The /^<(\d{4}-\d\d-\d\dT[\d:.]+Z)>\s?(.*)$/ split, done by hand because it
// runs on every line: (timestamp, rest). JS's `.` doesn't match \r, \u2028
// or \u2029, so a line with one of those after the timestamp doesn't match.
fn split_ts(line: &str) -> Option<(&str, &str)> {
    let b = line.as_bytes();
    let d = |i: usize| b.get(i).map_or(false, |c| c.is_ascii_digit());
    if b.first() != Some(&b'<') || !(1..5).all(d) || b.get(5) != Some(&b'-') || !d(6) || !d(7)
        || b.get(8) != Some(&b'-') || !d(9) || !d(10) || b.get(11) != Some(&b'T') { return None; }
    let mut i = 12;
    while b.get(i).map_or(false, |c| c.is_ascii_digit() || *c == b':' || *c == b'.') { i += 1; }
    if i == 12 || b.get(i) != Some(&b'Z') || b.get(i + 1) != Some(&b'>') { return None; }
    let ts = &line[1..i + 1];
    let mut rest = &line[i + 2..];
    if let Some(c) = rest.chars().next() { if c.is_whitespace() { rest = &rest[c.len_utf8()..]; } }
    if rest.contains(['\r', '\u{2028}', '\u{2029}']) { return None; }
    Some((ts, rest))
}

// Lines as JS's text.split(/\r?\n/) gives them.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

use crate::guard;

/// The parser on its own, for comparing against gamelog.js.
#[cfg(feature = "node")]
#[napi]
pub struct GameLogParser { p: Parser }

#[cfg(feature = "node")]
#[napi]
impl GameLogParser {
    #[napi(constructor)]
    pub fn new() -> Self { GameLogParser { p: Parser::default() } }
    /// Feeds a block of text (whole lines); returns a JSON array of events.
    #[napi]
    pub fn feed_text(&mut self, text: String) -> Result<String> {
        guard(|| Ok(Value::Array(split_lines(&text).filter_map(|l| self.p.feed(l)).collect()).to_string()))
    }
}

fn now_iso() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    crate::store::iso(ms)
}

/// Reading side of LogTailer. JS calls tick() once a second; it reads what
/// the game has added since and returns, in order, what happened:
/// [{kind:'events', events, backfill}, {kind:'status', state, file, message?}].
/// Bytes are kept until a full line arrives, so a character split between
/// two reads isn't garbled.
#[napi]
pub struct Tailer { file: PathBuf, file_str: String, parser: Parser, offset: u64, rest: Vec<u8>, state: Option<String> }

impl Tailer {
    fn status(&mut self, out: &mut Vec<Value>, state: &str, message: Option<String>) {
        if self.state.as_deref() == Some(state) && message.is_none() { return; }
        self.state = Some(state.to_string());
        let mut s = obj(vec![("kind", json!("status")), ("state", json!(state)), ("file", json!(self.file_str))]);
        if let Some(m) = message { s["message"] = json!(m); }
        out.push(s);
    }
    fn tick_inner(&mut self, first: bool) -> Vec<Value> {
        let mut out = vec![];
        let size = match std::fs::metadata(&self.file) { Ok(m) => m.len(), Err(_) => { self.status(&mut out, "missing", None); return out; } };
        if size < self.offset {
            self.offset = 0; self.rest.clear(); self.parser.reset();
            out.push(obj(vec![("kind", json!("events")), ("events", json!([{ "type": "log_reset", "ts": now_iso() }])), ("backfill", json!(false))]));
        }
        self.status(&mut out, "watching", None);
        if size == self.offset { return out; }
        let mut buf = vec![0u8; (size - self.offset) as usize];
        let read = File::open(&self.file).and_then(|mut f| {
            f.seek(SeekFrom::Start(self.offset))?;
            let mut n = 0;
            while n < buf.len() { let k = f.read(&mut buf[n..])?; if k == 0 { break; } n += k; }
            Ok(n)
        });
        let n = match read { Ok(n) => n, Err(e) => { self.status(&mut out, "error", Some(e.to_string())); return out; } };
        buf.truncate(n);
        self.offset += n as u64;
        self.rest.extend_from_slice(&buf);
        let cut = match self.rest.iter().rposition(|&b| b == b'\n') { Some(i) => i, None => return out };
        let complete: Vec<u8> = self.rest.drain(..=cut).collect();
        let text = String::from_utf8_lossy(&complete[..complete.len() - 1]).into_owned();
        let events: Vec<Value> = split_lines(&text).filter_map(|l| self.parser.feed(l)).collect();
        // The first read is a catch-up of the session so far.
        if !events.is_empty() { out.push(obj(vec![("kind", json!("events")), ("events", json!(events)), ("backfill", json!(first))])); }
        out
    }
}

#[napi]
impl Tailer {
    #[napi(constructor)]
    pub fn new(file: String) -> Self {
        Tailer { file: PathBuf::from(&file), file_str: file, parser: Parser::default(), offset: 0, rest: vec![], state: None }
    }
    #[napi]
    pub fn start(&mut self) -> Result<String> {
        self.offset = 0; self.rest.clear(); self.parser.reset();
        self.tick(true)
    }
    #[napi]
    pub fn tick(&mut self, first: bool) -> Result<String> {
        guard(|| Ok(Value::Array(self.tick_inner(first)).to_string()))
    }
}

pub fn scan(log_file: &str, days: f64) -> Vec<Value> {
    let r = res();
    let dir = Path::new(log_file).parent().unwrap_or(Path::new(".")).join("logbackups");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
    let cutoff = now - days * 86_400_000.0;
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out = vec![];
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().ends_with(".log") { continue; }
        let full = e.path();
        let Ok(meta) = std::fs::metadata(&full) else { continue };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        if mtime < cutoff { continue; }
        let Ok(bytes) = std::fs::read(&full) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let mut parser = Parser::default();
        let mut shard = String::new();
        let mut contracts: std::collections::HashMap<String, String> = Default::default();
        for line in split_lines(&text) {
            let Some(ev) = parser.feed(line) else { continue };
            let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let push = |out: &mut Vec<Value>, player: String, source: String, shard: &str| {
                out.push(obj(vec![("player", json!(player)), ("ts", ev["ts"].clone()), ("source", json!(source)), ("shard", json!(shard))]));
            };
            match s("type").as_str() {
                "session" => shard = s("shard"),
                "contract" if !s("missionId").is_empty() => { contracts.insert(s("missionId"), s("name")); }
                "sighting" => push(&mut out, s("player"), s("source"), &shard),
                "party" if !s("player").is_empty() && s("action") != "left" => push(&mut out, s("player"), "party".into(), &shard),
                "objective" if s("status") == "new" => {
                    if let Some(m) = r.neutralize.captures(&s("text")) {
                        let target = g(&m, 1);
                        let title = contracts.get(&s("missionId")).cloned().unwrap_or_default();
                        if target != "Target" && target != "Targets" && !r.bounty_title.is_match(&title) {
                            push(&mut out, target.to_string(), "bounty".into(), &shard);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(feature = "node")]
pub struct ScanTask { file: String, days: f64 }
#[cfg(feature = "node")]
impl Task for ScanTask {
    type Output = String;
    type JsValue = JsString;
    fn compute(&mut self) -> Result<String> {
        let (f, d) = (self.file.clone(), self.days);
        guard(move || Ok(Value::Array(scan(&f, d)).to_string()))
    }
    fn resolve(&mut self, env: Env, out: String) -> Result<JsString> { env.create_string(&out) }
}

/// Player sightings from logbackups in the last `days` days, as JSON text.
#[cfg(feature = "node")]
#[napi]
pub fn scan_recent_sightings(log_file: String, days: f64) -> AsyncTask<ScanTask> {
    AsyncTask::new(ScanTask { file: log_file, days })
}
