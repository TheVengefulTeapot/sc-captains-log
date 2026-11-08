// cargo run --release --example ships --no-default-features --features plain -- <dir>
// Fetches UEX's vehicle list into <dir>/ships-cache.json, then reads it back from the cache.
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let m = cl_native::market::Market::new(dir);
    let t = std::time::Instant::now();
    let a = m.ships_now(false).expect("fetch");
    let v: serde_json::Value = serde_json::from_str(&a).unwrap();
    println!("fetched: {} vehicles in {:?}, error: {}", v["vehicles"].as_array().map_or(0, |x| x.len()), t.elapsed(), v.get("error").map_or("none".into(), |e| e.to_string()));
    let t = std::time::Instant::now();
    let b = m.ships_now(false).expect("cache");
    println!("second call served from cache: {} ({:?})", a == b, t.elapsed());
}
