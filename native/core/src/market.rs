//! Rust port of market.js: commodity prices from UEX (uexcorp.space), joined
//! into slim rows and cached on disk so the trading tab works offline and UEX
//! is asked at most every MAX_AGE. Results go back to JS as JSON text with the
//! same shape, keys and key order as market.js produces.
//!
//! HTTP runs on libuv's thread pool (napi AsyncTask), so the app never waits
//! on the network; TLS uses Windows' own (SChannel) via native-tls.

#[cfg(feature = "node")]
use napi::{bindgen_prelude::AsyncTask, Env, JsString, Task};
use crate::{Error, Result};
use napi_derive::napi;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API: &str = "https://api.uexcorp.space/2.0";
const MAX_AGE: f64 = 45.0 * 60.0 * 1000.0;
const HISTORY_AGE: f64 = 3_600_000.0;
const UA: &str = "SCCaptainsLog/1.0 (personal trade tracker)";

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

// JavaScript truthiness, for the `x || 0` fallbacks in market.js.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn or0(v: Option<&Value>) -> Value {
    if truthy(v) { v.cloned().unwrap_or(json!(0)) } else { json!(0) }
}
fn or_str(v: Option<&Value>) -> Value {
    if truthy(v) { v.cloned().unwrap_or(json!("")) } else { json!("") }
}
fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}
// x * 1000, kept as an integer when x is one (as JSON.stringify would write it).
fn times_1000(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) if n.is_i64() => json!(n.as_i64().unwrap() * 1000),
        Some(Value::Number(n)) if n.is_u64() => json!(n.as_u64().unwrap() * 1000),
        _ => {
            let f = num(v) * 1000.0;
            if f.fract() == 0.0 && f.abs() < 9e15 { json!(f as i64) } else { json!(f) }
        }
    }
}
// Map key with JS Map/=== semantics for ids (1 and "1" are different keys).
fn key(v: Option<&Value>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "undefined".into())
}
// How `${x}` prints a JSON value in JS.
fn js_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn agent() -> Result<ureq::Agent> {
    let tls = native_tls::TlsConnector::new().map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .user_agent(UA)
        .timeout(Duration::from_secs(60))
        .build())
}

fn get_json(agent: &ureq::Agent, url: &str) -> std::result::Result<Vec<Value>, String> {
    let res = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("UEX returned {}", code)),
        Err(e) => return Err(e.to_string()),
    };
    let mut body = Vec::new();
    res.into_reader().read_to_end(&mut body).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if json.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return Err(format!("UEX: {}", js_str(json.get("status"))));
    }
    Ok(match json.get("data") {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    })
}

fn download(agent: &ureq::Agent) -> std::result::Result<Value, String> {
    // The three requests run side by side, as Promise.all does in market.js.
    let (prices, terminals, commodities) = std::thread::scope(|s| {
        let p = s.spawn(|| get_json(agent, &format!("{}/commodities_prices_all", API)));
        let t = s.spawn(|| get_json(agent, &format!("{}/terminals?type=commodity", API)));
        let c = s.spawn(|| get_json(agent, &format!("{}/commodities", API)).unwrap_or_default());
        (
            p.join().unwrap_or_else(|_| Err("request thread failed".into())),
            t.join().unwrap_or_else(|_| Err("request thread failed".into())),
            c.join().unwrap_or_default(),
        )
    });
    let (prices, terminals) = (prices?, terminals?);
    let empty = Map::new();
    let term: HashMap<String, &Map<String, Value>> = terminals.iter()
        .filter_map(|t| t.as_object().map(|o| (key(o.get("id")), o)))
        .collect();
    let code: HashMap<String, Value> = commodities.iter()
        .filter_map(|c| c.as_object().map(|o| (key(o.get("id")), or_str(o.get("code")))))
        .collect();
    let rows: Vec<Value> = prices.iter().filter_map(|p| {
        let p = p.as_object()?;
        let t = term.get(&key(p.get("id_terminal"))).copied().unwrap_or(&empty);
        let mut r = Map::new();
        r.insert("cid".into(), p.get("id_commodity").cloned().unwrap_or(Value::Null));
        r.insert("tid".into(), p.get("id_terminal").cloned().unwrap_or(Value::Null));
        r.insert("c".into(), p.get("commodity_name").cloned().unwrap_or(Value::Null));
        r.insert("code".into(), code.get(&key(p.get("id_commodity"))).cloned().filter(|v| truthy(Some(v))).unwrap_or(json!("")));
        r.insert("t".into(), if truthy(t.get("displayname")) { t["displayname"].clone() } else { p.get("terminal_name").cloned().unwrap_or(Value::Null) });
        r.insert("sys".into(), or_str(t.get("star_system_name")));
        r.insert("planet".into(), or_str(t.get("planet_name")));
        r.insert("buy".into(), or0(p.get("price_buy")));
        r.insert("sell".into(), or0(p.get("price_sell")));
        r.insert("buyAvg".into(), or0(p.get("price_buy_avg")));
        r.insert("sellAvg".into(), or0(p.get("price_sell_avg")));
        r.insert("scuBuy".into(), or0(p.get("scu_buy")));
        r.insert("scuSell".into(), or0(p.get("scu_sell")));
        r.insert("scuSellAvg".into(), or0(p.get("scu_sell_avg")));
        r.insert("lvlSell".into(), or0(p.get("status_sell")));
        r.insert("upd".into(), times_1000(Some(&or0(p.get("date_modified")))));
        // .filter(r => r.c && (r.buy || r.sell))
        if truthy(r.get("c")) && (truthy(r.get("buy")) || truthy(r.get("sell"))) { Some(Value::Object(r)) } else { None }
    }).collect();
    Ok(json!({ "fetched": now_ms() as i64, "rows": rows }))
}

struct State {
    file: PathBuf,
    cache: Option<Value>,
    history: HashMap<String, (f64, Value)>,
}

fn read_cache(file: &PathBuf) -> Option<Value> {
    let text = std::fs::read(file).ok()?;
    let cache: Value = serde_json::from_slice(&text).ok()?;
    let rows = cache.get("rows")?.as_array()?;
    // Caches from before rows carried IDs, codes and demand levels are refetched.
    if let Some(first) = rows.first() {
        if first.get("tid").is_none() || first.get("lvlSell").is_none() { return None; }
    }
    Some(cache)
}

fn write_cache(file: &PathBuf, cache: &Value) {
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, cache.to_string()).is_ok() { let _ = std::fs::rename(&tmp, file); }
}

#[napi]
#[derive(Clone)]
pub struct Market {
    state: Arc<Mutex<State>>,
}

pub struct GetTask { state: Arc<Mutex<State>>, force: bool }

#[cfg(feature = "node")]
impl Task for GetTask {
    type Output = String;
    type JsValue = JsString;
    fn compute(&mut self) -> Result<String> { crate::guard(|| self.compute_inner()) }
    fn resolve(&mut self, env: Env, out: String) -> Result<JsString> { env.create_string(&out) }
}
impl GetTask {
    fn compute_inner(&mut self) -> Result<String> {
        let (cached, file) = {
            let s = self.state.lock().unwrap();
            (s.cache.clone(), s.file.clone())
        };
        if let Some(c) = &cached {
            if !self.force && now_ms() - num(c.get("fetched")) < MAX_AGE { return Ok(c.to_string()); }
        }
        match agent().map_err(|e| e.reason.clone()).and_then(|a| download(&a)) {
            Ok(fresh) => {
                write_cache(&file, &fresh);
                let out = fresh.to_string();
                self.state.lock().unwrap().cache = Some(fresh);
                Ok(out)
            }
            Err(msg) => {
                // Offline or UEX down: fall back to the last copy we have.
                let mut v = cached.unwrap_or_else(|| json!({ "fetched": 0, "rows": [] }));
                if let Some(o) = v.as_object_mut() { o.insert("error".into(), json!(msg)); }
                Ok(v.to_string())
            }
        }
    }
}

pub struct HistoryTask { state: Arc<Mutex<State>>, pairs: Vec<Value>, force: bool }

#[cfg(feature = "node")]
impl Task for HistoryTask {
    type Output = String;
    type JsValue = JsString;
    fn compute(&mut self) -> Result<String> { crate::guard(|| self.compute_inner()) }
    fn resolve(&mut self, env: Env, out: String) -> Result<JsString> { env.create_string(&out) }
}
impl HistoryTask {
    fn compute_inner(&mut self) -> Result<String> {
        let agent = agent()?;
        let state = &self.state;
        let force = self.force;
        let results: Vec<Value> = std::thread::scope(|s| {
            let handles: Vec<_> = self.pairs.iter().take(8).map(|pair| {
                let agent = &agent;
                s.spawn(move || {
                    let tid = pair.get("tid").cloned().unwrap_or(Value::Null);
                    let cid = pair.get("cid").cloned().unwrap_or(Value::Null);
                    let k = format!("{}|{}", js_str(Some(&tid)), js_str(Some(&cid)));
                    if !force {
                        if let Some((at, val)) = state.lock().unwrap().history.get(&k) {
                            if now_ms() - at < HISTORY_AGE { return val.clone(); }
                        }
                    }
                    let url = format!(
                        "{}/commodities_prices_history?id_terminal={}&id_commodity={}",
                        API, urlencode(&js_str(Some(&tid))), urlencode(&js_str(Some(&cid)))
                    );
                    match get_json(agent, &url) {
                        Ok(data) => {
                            let mut pts: Vec<Value> = data.iter().map(|h| json!({
                                "t": times_1000(h.get("date_added")),
                                "buy": or0(h.get("price_buy")),
                                "sell": or0(h.get("price_sell")),
                            })).collect();
                            pts.sort_by(|a, b| num(a.get("t")).partial_cmp(&num(b.get("t"))).unwrap_or(std::cmp::Ordering::Equal));
                            let val = json!({ "tid": tid, "cid": cid, "pts": pts });
                            state.lock().unwrap().history.insert(k, (now_ms(), val.clone()));
                            val
                        }
                        Err(_) => json!({ "tid": tid, "cid": cid, "pts": [], "error": true }),
                    }
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap_or(Value::Null)).collect()
        });
        Ok(Value::Array(results).to_string())
    }
}

// encodeURIComponent
pub(crate) fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) { o.push(b as char); } else { o.push_str(&format!("%{:02X}", b)); }
    }
    o
}

#[napi]
impl Market {
    #[napi(constructor)]
    pub fn new(cache_dir: String) -> Self {
        let file = PathBuf::from(cache_dir).join("market-cache.json");
        let cache = read_cache(&file);
        Market { state: Arc::new(Mutex::new(State { file, cache, history: HashMap::new() })) }
    }

    /// Resolves to the cache as JSON text: { fetched, rows, error? }.
    #[cfg(feature = "node")]
    #[napi]
    pub fn get(&self, force: bool) -> AsyncTask<GetTask> {
        AsyncTask::new(GetTask { state: self.state.clone(), force })
    }

    /// pairs_json: [{tid, cid}, ...]. Resolves to JSON text:
    /// [{tid, cid, pts:[{t, buy, sell}]}] (max 8 pairs, cached for an hour).
    #[cfg(feature = "node")]
    #[napi]
    pub fn get_history(&self, pairs_json: String, force: bool) -> Result<AsyncTask<HistoryTask>> {
        let pairs: Vec<Value> = match serde_json::from_str::<Value>(&pairs_json) {
            Ok(Value::Array(a)) => a,
            _ => vec![],
        };
        Ok(AsyncTask::new(HistoryTask { state: self.state.clone(), pairs, force }))
    }
}

// Blocking versions for callers that run their own threads (the Tauri app).
impl Market {
    pub fn get_now(&self, force: bool) -> Result<String> {
        crate::guard(|| GetTask { state: self.state.clone(), force }.compute_inner())
    }
    pub fn history_now(&self, pairs_json: &str, force: bool) -> Result<String> {
        let pairs: Vec<Value> = match serde_json::from_str::<Value>(pairs_json) { Ok(Value::Array(a)) => a, _ => vec![] };
        crate::guard(|| HistoryTask { state: self.state.clone(), pairs, force }.compute_inner())
    }
}

// ---- ship list ----
// UEX's vehicle list, for the ship autocomplete and ship classes. Cached in
// ships-cache.json next to the price cache and fetched again after a week;
// the page turns it into [name, maker, class] rows (shipsFromUex in app.js).
const SHIPS_MAX_AGE: f64 = 7.0 * 24.0 * 3_600_000.0;
const SHIP_KEYS: [&str; 24] = [
    "name", "name_full", "company_name", "is_addon", "is_concept", "is_ground_vehicle", "is_mining", "is_salvage",
    "is_refinery", "is_construction", "is_exploration", "is_medical", "is_military", "is_bomber",
    "is_cargo", "is_industrial", "is_repair", "is_refuel", "is_passenger", "is_carrier", "is_science",
    "is_scanning", "is_research", "is_datarunner",
];

impl Market {
    /// { fetched, vehicles: [...], error? } as JSON text.
    pub fn ships_now(&self, force: bool) -> Result<String> {
        crate::guard(|| {
            let file = self.state.lock().unwrap().file.with_file_name("ships-cache.json");
            let cached: Option<Value> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok())
                // A copy saved before full names were kept is fetched again.
                .filter(|c: &Value| c["vehicles"].get(0).map_or(false, |v| v.get("name_full").is_some()));
            if let Some(c) = &cached {
                if !force && now_ms() - num(c.get("fetched")) < SHIPS_MAX_AGE { return Ok(c.to_string()); }
            }
            let fresh = agent().map_err(|e| e.reason.clone()).and_then(|a| get_json(&a, &format!("{}/vehicles", API)));
            match fresh {
                Ok(list) if list.len() >= 100 => {
                    let slim: Vec<Value> = list.iter().filter_map(|v| v.as_object()).map(|o| {
                        let mut m = Map::new();
                        for k in SHIP_KEYS { if let Some(x) = o.get(k) { m.insert(k.into(), x.clone()); } }
                        Value::Object(m)
                    }).collect();
                    let out = json!({ "fetched": now_ms() as i64, "vehicles": slim });
                    write_cache(&file, &out);
                    Ok(out.to_string())
                }
                Ok(_) | Err(_) => {
                    let msg = match fresh { Err(e) => e, Ok(l) => format!("UEX returned only {} vehicles", l.len()) };
                    let mut v = cached.unwrap_or_else(|| json!({ "fetched": 0, "vehicles": [] }));
                    if let Some(o) = v.as_object_mut() { o.insert("error".into(), json!(msg)); }
                    Ok(v.to_string())
                }
            }
        })
    }
}
