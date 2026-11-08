// Persistent data store: one JSON file in userData, written atomically, with
// the previous save kept alongside and daily backups for the last 14 days.
//
//   captains-log.json        current data
//   captains-log.prev.json   the save before the current one
//   backups/captains-log-YYYY-MM-DD.json   first save of each day
//
// A file that exists but can't be parsed is renamed to *.corrupt-<time>.json
// rather than overwritten, and the store falls back to the newest good copy.
const fs = require('fs');
const path = require('path');

const KEEP_DAYS = 14;

// Rust port step 1: the same store written in Rust (native/core), loaded as
// a native module. If it's missing or won't load, the JavaScript version
// below is used instead, so saving never depends on the native build.
function createStore(dir) {
  try {
    const { Store } = require(path.join(__dirname, 'native', 'cl_native.node'));
    const s = new Store(dir);
    return {
      backend: 'rust',
      load: () => s.load(),
      save: text => s.save(text),
      writeAtomic: (p, text) => s.writeAtomic(p, text),
      info: () => ({ ...s.info(), backend: 'rust' }),
      file: s.file,
      dir: s.dir,
    };
  } catch (e) {
    const js = createJsStore(dir);
    js.backend = 'javascript';
    js.nativeError = e.message;
    const info = js.info;
    js.info = () => ({ ...info(), backend: 'javascript', nativeError: e.message });
    return js;
  }
}

function createJsStore(dir) {
  const file = path.join(dir, 'captains-log.json');
  const prev = path.join(dir, 'captains-log.prev.json');
  const backupDir = path.join(dir, 'backups');
  let lastSaved = null;

  function readJson(p) {
    if (!fs.existsSync(p)) return { missing: true };
    const text = fs.readFileSync(p, 'utf8');
    try {
      const obj = JSON.parse(text);
      if (!obj || typeof obj !== 'object' || Array.isArray(obj)) throw new Error('not an object');
      return { text };
    } catch (e) {
      const bad = p.replace(/\.json$/, '') + '.corrupt-' + Date.now() + '.json';
      try { fs.renameSync(p, bad); } catch (_) {}
      return { corrupt: bad };
    }
  }

  function backups() {
    if (!fs.existsSync(backupDir)) return [];
    return fs.readdirSync(backupDir)
      .filter(f => /^captains-log-\d{4}-\d{2}-\d{2}\.json$/.test(f))
      .sort().reverse()
      .map(f => path.join(backupDir, f));
  }

  // Returns { json, source, warnings } — json is null when there is no saved
  // data anywhere (first run, or migrating from the old localStorage store).
  function load() {
    const warnings = [];
    for (const [p, source] of [[file, 'file'], [prev, 'previous save'], ...backups().map(b => [b, 'backup ' + path.basename(b)])]) {
      const r = readJson(p);
      if (r.text) return { json: r.text, source, warnings };
      if (r.corrupt) warnings.push(path.basename(p) + ' was unreadable; kept as ' + path.basename(r.corrupt));
    }
    return { json: null, source: 'none', warnings };
  }

  function writeAtomic(p, text) {
    const tmp = p + '.tmp';
    const fd = fs.openSync(tmp, 'w');
    try { fs.writeSync(fd, text); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
    fs.renameSync(tmp, p);
  }

  function dailyBackup() {
    if (!fs.existsSync(file)) return;
    fs.mkdirSync(backupDir, { recursive: true });
    const day = new Date().toISOString().slice(0, 10);
    const target = path.join(backupDir, 'captains-log-' + day + '.json');
    if (!fs.existsSync(target)) fs.copyFileSync(file, target);
    backups().slice(KEEP_DAYS).forEach(b => { try { fs.unlinkSync(b); } catch (_) {} });
  }

  function save(text) {
    JSON.parse(text); // never write something that won't load back
    dailyBackup();
    if (fs.existsSync(file)) fs.copyFileSync(file, prev);
    writeAtomic(file, text);
    lastSaved = new Date().toISOString();
    return lastSaved;
  }

  function info() {
    return { dir, file, lastSaved, backups: backups().map(b => path.basename(b)) };
  }

  return { load, save, writeAtomic, info, file, dir };
}

module.exports = { createStore, createJsStore };
