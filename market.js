// Commodity market prices from UEX (uexcorp.space), crowd-sourced by players.
// Prices and terminal names are joined into slim rows and cached on disk, so
// the trading tab works offline and UEX is asked at most every MAX_AGE.
const fs = require('fs');
const path = require('path');

const API = 'https://api.uexcorp.space/2.0';
const MAX_AGE = 45 * 60 * 1000;
const UA = 'SCCaptainsLog/1.0 (personal trade tracker)';

async function getJson(url) {
  const res = await fetch(url, { headers: { 'User-Agent': UA } });
  if (!res.ok) throw new Error(`UEX returned ${res.status}`);
  const json = await res.json();
  if (json.status !== 'ok') throw new Error(`UEX: ${json.status}`);
  return json.data || [];
}

async function download() {
  const [prices, terminals, commodities] = await Promise.all([
    getJson(`${API}/commodities_prices_all`),
    getJson(`${API}/terminals?type=commodity`),
    getJson(`${API}/commodities`).catch(() => []),
  ]);
  const term = new Map(terminals.map(t => [t.id, t]));
  const code = new Map(commodities.map(c => [c.id, c.code || '']));
  const rows = prices.map(p => {
    const t = term.get(p.id_terminal) || {};
    return {
      cid: p.id_commodity,
      tid: p.id_terminal,
      c: p.commodity_name,
      code: code.get(p.id_commodity) || '',   // short code, e.g. RMC

      t: t.displayname || p.terminal_name,
      sys: t.star_system_name || '',
      planet: t.planet_name || '',
      buy: p.price_buy || 0,          // you pay this at the terminal
      sell: p.price_sell || 0,        // the terminal pays you this
      buyAvg: p.price_buy_avg || 0,
      sellAvg: p.price_sell_avg || 0,
      scuBuy: p.scu_buy || 0,         // SCU it has for you to buy
      scuSell: p.scu_sell || 0,       // SCU it will take from you
      scuSellAvg: p.scu_sell_avg || 0, // its usual demand, when today's isn't reported
      lvlSell: p.status_sell || 0,    // its stock level of this, 1 empty .. 7 full
      upd: (p.date_modified || 0) * 1000,
    };
  }).filter(r => r.c && (r.buy || r.sell));
  return { fetched: Date.now(), rows };
}

// Rust port step 2: the same market code in Rust (native/core/src/market.rs).
// The JavaScript version below is used if the native module won't load, and
// for any single call the native one fails on, so prices never depend on it.
function createMarket(cacheDir) {
  let js = null;
  const jsMarket = () => js || (js = createJsMarket(cacheDir));
  try {
    const { Market } = require(path.join(__dirname, 'native', 'cl_native.node'));
    const m = new Market(cacheDir);
    return {
      backend: 'rust',
      get: force => m.get(!!force).then(JSON.parse).catch(() => jsMarket().get(force)),
      getHistory: (pairs, force) => m.getHistory(JSON.stringify(pairs || []), !!force).then(JSON.parse).catch(() => jsMarket().getHistory(pairs, force)),
    };
  } catch (e) {
    return { ...jsMarket(), backend: 'javascript', nativeError: e.message };
  }
}

function createJsMarket(cacheDir) {
  const file = path.join(cacheDir, 'market-cache.json');
  let cache = null;
  try { cache = JSON.parse(fs.readFileSync(file, 'utf8')); } catch {}
  // Caches from before rows carried IDs, codes and demand levels are refetched.
  if (cache && cache.rows.length && (cache.rows[0].tid === undefined || cache.rows[0].lvlSell === undefined)) cache = null;
  const history = new Map();
  async function get(force) {
    if (!force && cache && Date.now() - cache.fetched < MAX_AGE) return cache;
    try {
      cache = await download();
      fs.promises.writeFile(file, JSON.stringify(cache)).catch(() => {});
      return cache;
    } catch (e) {
      // Offline or UEX down: fall back to the last copy we have.
      return cache ? { ...cache, error: e.message } : { fetched: 0, rows: [], error: e.message };
    }
  }
  // Price history for terminal/commodity pairs: [{tid, cid, pts:[{t, buy, sell}]}].
  // UEX only serves history one terminal at a time; kept in memory for an hour.
  async function getHistory(pairs, force) {
    return Promise.all((pairs || []).slice(0, 8).map(async ({ tid, cid }) => {
      const key = tid + '|' + cid, hit = history.get(key);
      if (!force && hit && Date.now() - hit.at < 3600000) return hit.val;
      let pts = [];
      try {
        const data = await getJson(`${API}/commodities_prices_history?id_terminal=${encodeURIComponent(tid)}&id_commodity=${encodeURIComponent(cid)}`);
        pts = data.map(h => ({ t: h.date_added * 1000, buy: h.price_buy || 0, sell: h.price_sell || 0 })).sort((a, b) => a.t - b.t);
      } catch { return { tid, cid, pts: [], error: true }; }
      const val = { tid, cid, pts };
      history.set(key, { at: Date.now(), val });
      return val;
    }));
  }
  return { get, getHistory };
}

module.exports = { createMarket, createJsMarket };
