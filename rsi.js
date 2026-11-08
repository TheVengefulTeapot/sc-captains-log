// RSI handle lookups: checks a handle against its public citizen page and,
// when it doesn't exist, asks Spectrum's member search for close matches.
const RSI = 'https://robertsspaceindustries.com';
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) SCCaptainsLog';
const HANDLE_RE = /^[A-Za-z0-9_-]{3,60}$/;
const cache = new Map();

function decode(s) {
  return String(s || '').replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"').replace(/&#0?39;/g, "'").replace(/\s+/g, ' ').trim();
}

// Returns { found: true, handle, moniker, enlisted, orgName, orgSid, orgRank, avatar, url }
// or { found: false } for a handle with no citizen page.
async function lookupCitizen(handle) {
  const res = await fetch(`${RSI}/citizens/${encodeURIComponent(handle)}`, { headers: { 'User-Agent': UA } });
  if (res.status === 404) return { found: false };
  if (!res.ok) throw new Error(`RSI returned ${res.status}`);
  const html = (await res.text()).replace(/\s+/g, ' ');
  const field = label => decode((html.match(new RegExp(`<span class="label">${label}</span> ?<strong class="value[^"]*">([^<]*)`)) || [])[1]);
  const out = {
    found: true,
    handle: field('Handle name') || handle,
    moniker: decode((html.match(/<div class="info"> ?<p class="entry"> ?<strong class="value">([^<]*)/) || [])[1]),
    enlisted: field('Enlisted'),
    avatar: (html.match(/<div class="thumb"> ?<img src="([^"]+)"/) || [])[1] || '',
    orgName: '', orgSid: '', orgRank: '',
  };
  if (out.avatar.startsWith('/')) out.avatar = RSI + out.avatar;
  out.url = `${RSI}/citizens/${encodeURIComponent(out.handle)}`;
  // Main org block; absent (or redacted) when the player hides it. Its
  // "dataN" class names are shuffled per request, so match on labels.
  const org = (html.match(/main-org(.{0,2000})/) || [])[1] || '';
  if (org) {
    const m = org.match(/<a href="\/orgs\/([^"]*)" class="value[^"]*"[^>]*>([^<]*)/);
    if (m) { out.orgSid = decode(m[1]); out.orgName = decode(m[2]); }
    out.orgRank = decode((org.match(/Organization rank<\/span> ?<strong class="value[^"]*">([^<]*)/) || [])[1]);
  }
  return out;
}

const searchCache = new Map();
async function searchMembers(text, page = 1) {
  const key = text.toLowerCase() + '|' + page;
  if (searchCache.has(key)) return searchCache.get(key);
  const res = await fetch(`${RSI}/api/spectrum/search/member/autocomplete`, {
    method: 'POST',
    headers: { 'User-Agent': UA, 'Content-Type': 'application/json' },
    body: JSON.stringify({ community_id: null, text, page }),
  });
  if (!res.ok) throw new Error(`Spectrum returned ${res.status}`);
  const json = await res.json();
  const names = ((json.data && json.data.members) || []).map(m => m.nickname).filter(Boolean);
  searchCache.set(key, names);
  return names;
}

function distance(a, b) {
  a = a.toLowerCase(); b = b.toLowerCase();
  const row = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    let prev = row[0]; row[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const tmp = row[j];
      row[j] = Math.min(row[j] + 1, row[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1));
      prev = tmp;
    }
  }
  return row[b.length];
}

// Spectrum's search only matches from the start of a name, so a typo in the
// middle finds nothing. Find where the matches run out (that's roughly where
// the typo is), then try the usual slips there: a missed, extra or swapped
// letter. Everything found is ranked by how close it is to what was typed.
async function suggest(handle) {
  const seen = new Set();
  const add = names => names.forEach(n => seen.add(n));
  // Binary search for the longest prefix that still starts someone's handle.
  // The search also matches display names, which would hide the typo, so
  // only handle matches count here.
  let lo = 3, hi = handle.length, best = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1, prefix = handle.slice(0, mid).toLowerCase();
    const names = await searchMembers(prefix);
    add(names);
    if (names.some(n => n.toLowerCase().startsWith(prefix))) { best = mid; lo = mid + 1; } else hi = mid - 1;
  }
  if (best === handle.length) return rank(seen, handle);
  if (best) add(await searchMembers(handle.slice(0, best), 2));
  const p = Math.max(best, 1), tail = Math.min(handle.length + 1, p + 5);
  const variants = new Set([
    handle.slice(0, p) + handle.slice(p + 1),                           // extra letter
    handle.slice(0, p - 1) + handle.slice(p),                           // extra letter just before
    handle.slice(0, p) + handle[p + 1] + handle[p] + handle.slice(p + 2), // swapped pair
    handle.slice(0, p - 1) + handle[p] + handle[p - 1] + handle.slice(p + 1),
  ]);
  for (const ch of 'aeiouy' + handle[p - 1]) variants.add(handle.slice(0, p) + ch + handle.slice(p)); // missed letter
  const probes = [...variants].filter(v => v && !v.includes('undefined') && v.length >= 3).map(v => v.slice(0, tail));
  (await Promise.all([...new Set(probes)].map(v => searchMembers(v).catch(() => [])))).forEach(add);
  return rank(seen, handle);
}
function rank(seen, handle) {
  return [...seen]
    .map(n => ({ n, d: distance(n, handle) }))
    .filter(x => x.d <= Math.max(3, Math.floor(handle.length / 3)))
    .sort((a, b) => a.d - b.d || a.n.length - b.n.length)
    .slice(0, 5)
    .map(x => x.n);
}

async function jsCheckHandle(raw) {
  const handle = String(raw || '').trim();
  if (!HANDLE_RE.test(handle)) return { status: 'invalid', handle };
  const key = handle.toLowerCase();
  if (cache.has(key)) return cache.get(key);
  let result;
  try {
    const c = await lookupCitizen(handle);
    result = c.found ? { status: 'found', ...c } : { status: 'notfound', handle, suggestions: await suggest(handle) };
  } catch (e) {
    return { status: 'error', handle, message: e.message };
  }
  cache.set(key, result);
  return result;
}

// Rust port step 3: the same lookups in Rust (native/core/src/rsi.rs). The
// JavaScript version above is used if the native module won't load, or for
// any single check the native one fails on.
let native = null, nativeError = null;
try { native = require(require('path').join(__dirname, 'native', 'cl_native.node')); }
catch (e) { nativeError = e.message; }
async function checkHandle(raw) {
  if (!native) return jsCheckHandle(raw);
  try { return JSON.parse(await native.checkHandle(String(raw || ''))); }
  catch { return jsCheckHandle(raw); }
}
const backend = () => (native ? 'rust' : 'javascript');

module.exports = { checkHandle, jsCheckHandle, distance, backend, nativeError };
