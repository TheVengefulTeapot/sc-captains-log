// Game.log watcher: tails Star Citizen's Game.log (read-only) and turns
// the lines we care about into small event objects for the renderer.
const fs = require('fs');
const path = require('path');

const NULL_ID = '00000000-0000-0000-0000-000000000000';

// Common install locations, checked in order when no path is set.
function candidatePaths() {
  const out = [];
  for (const drive of ['C', 'D', 'E', 'F', 'G']) {
    out.push(`${drive}:\\StarCitizen\\StarCitizen\\LIVE\\Game.log`);
    out.push(`${drive}:\\Program Files\\Roberts Space Industries\\StarCitizen\\LIVE\\Game.log`);
    out.push(`${drive}:\\Roberts Space Industries\\StarCitizen\\LIVE\\Game.log`);
  }
  return out;
}
function detectLogPath() {
  return candidatePaths().find(p => fs.existsSync(p)) || null;
}

// Strip the game's <EM3>..</EM3> style markup and tidy whitespace.
function cleanText(s) {
  return String(s || '').replace(/<\/?EM\d>/g, '').replace(/\s+/g, ' ').replace(/[:\s]+$/, '').trim();
}

const RE_TS = /^<(\d{4}-\d\d-\d\dT[\d:.]+Z)>\s?(.*)$/;
const RE_NOTIF = /<SHUDEvent_OnNotification> Added notification "([\s\S]*?)" \[(\d+)\] to queue\..*?MissionId: \[([0-9a-f-]+)\]/;

function parseNotification(text, idx, missionId, ts) {
  const mid = missionId && missionId !== NULL_ID ? missionId : null;
  let m;
  if ((m = text.match(/^Contract (Accepted|Complete|Failed|Shared|Available):\s*([\s\S]*)$/))) {
    return { type: 'contract', status: m[1].toLowerCase(), name: cleanText(m[2]), missionId: mid, ts };
  }
  if ((m = text.match(/^Awarded ([\d,]+) aUEC/))) {
    return { type: 'payout', amount: parseInt(m[1].replace(/,/g, ''), 10), key: `${ts}|${idx}`, ts };
  }
  if ((m = text.match(/^Entered (.+?) Jurisdiction/))) return { type: 'jurisdiction', name: m[1], ts };
  if (/^Entering Armistice Zone/.test(text)) return { type: 'zone', zone: 'armistice', ts };
  if (/^Leaving Armistice Zone/.test(text)) return { type: 'zone', zone: 'open', ts };
  if (/^Entered Monitored Space/.test(text)) return { type: 'monitored', on: true, ts };
  if (/^Exited Monitored Space/.test(text)) return { type: 'monitored', on: false, ts };
  if ((m = text.match(/^You have joined channel '(.+?) : (.+?)'/))) return { type: 'ship', ship: m[1], owner: m[2], ts };
  if ((m = text.match(/^New Member Joined\s+(.+?) has joined the party/))) return { type: 'party', action: 'joined', player: m[1], ts };
  if ((m = text.match(/^Member Left\s+(.+?) has left the party/))) return { type: 'party', action: 'left', player: m[1], ts };
  if ((m = text.match(/^New Party Leader\s+(.+?) is now party leader/))) return { type: 'party', action: 'leader', player: m[1], ts };
  if (/^Party Disbanded/.test(text)) return { type: 'party', action: 'disbanded', ts };
  if (/^You have left the party/.test(text)) return { type: 'party', action: 'self_left', ts };
  if ((m = text.match(/^(New Objective|Objective Complete):\s*([\s\S]*)$/))) {
    return { type: 'objective', status: m[1] === 'New Objective' ? 'new' : 'complete', text: cleanText(m[2]), missionId: mid, ts };
  }
  if ((m = text.match(/^Transaction (Complete|Error)/))) return { type: 'transaction', ok: m[1] === 'Complete', ts };
  return null;
}

// Stateful line parser. Some notifications span several lines, so a
// notification is buffered until its "to queue." tail arrives.
function createParser() {
  let pending = null;
  // A shop buy is only logged as a request; the following response says
  // whether it went through.
  let pendingBuy = null;

  function parseFull(ts, body) {
    let m;
    if ((m = body.match(/<CEntityComponentShop(?:UI|ping)Provider::Send(?:Shop|StandardItem)BuyRequest> .*?shopName\[([^\]]*)\].*?client_price\[([\d.]+)\].*?itemName\[([^\]]*)\] quantity\[(\d+)\]/))) {
      pendingBuy = { type: 'purchase', shop: m[1], price: Math.round(parseFloat(m[2])), item: m[3], qty: parseInt(m[4], 10), key: ts, ts };
      return null;
    }
    if ((m = body.match(/<CEntityComponentShop(?:UI|ping)Provider::RmShopFlowResponse> .*?result\[(\w+)\]/))) {
      const buy = pendingBuy; pendingBuy = null;
      return buy && m[1] === 'Success' ? buy : null;
    }
    if ((m = body.match(/<Player entered carriage> \[TRANSPORT\] Player ([^\[\s]+)\[/))) {
      return { type: 'sighting', player: m[1], source: 'transit', ts };
    }
    if ((m = body.match(/^Logged a start of a status effect! nickname: ([^,]+), status effect: (.+)$/))) {
      return { type: 'sighting', player: m[1].trim(), source: 'nearby', detail: m[2].trim(), ts };
    }
    if ((m = body.match(/<CommsNotifications> SendCommsNotification \+(\S+) - .*?Mission: \[([0-9a-f-]+)\]/))) {
      const tags = m[1].split(',');
      const types = tags.filter(t => t.startsWith('Missions.MissionType.')).map(t => t.slice(21)).filter(t => t !== 'Generic');
      const org = (tags.find(t => /^Missions\.Organization\.[^.]+$/.test(t)) || '').slice(22);
      return types.length || org ? { type: 'mission_info', missionId: m[2], types, org, ts } : null;
    }
    if (body.includes('<SHUDEvent_OnNotification> Added notification')) {
      m = body.match(RE_NOTIF);
      return m ? parseNotification(m[1], m[2], m[3], ts) : null;
    }
    if ((m = body.match(/<EndMission> .*?MissionId\[([0-9a-f-]+)\] Player\[([^\]]*)\].*?CompletionType\[(\w+)\] Reason\[([^\]]*)\]/))) {
      return { type: 'mission_end', missionId: m[1], player: m[2], result: m[3].toLowerCase(), reason: m[4], ts };
    }
    // Your death, two ways: dying in a destroyed ship logs an ActorState Dead
    // line (with the ship), and every death logs recovery of your body item.
    if ((m = body.match(/<\[ActorState\] Dead> .*?Actor '([^']+)' \[\d+\] ejected from zone '([^']*)' \[\d+\] to zone '([^']*)'(?:.*?due to (.+?)\.)?/))) {
      return { type: 'actor_dead', actor: m[1], fromZone: m[2], toZone: m[3], reason: m[4] || '', ts };
    }
    if (/PopulateItemPortForItemRecoveryEntitlement\]> Item '[^']*Class\(body_/.test(body)) return { type: 'corpse', ts };
    if ((m = body.match(/<MissionShared> Received share push message: ownerId\[(\d+)\] - missionId\[([0-9a-f-]+)\]/))) {
      return { type: 'mission_shared', ownerId: m[1], missionId: m[2], ts };
    }
    if ((m = body.match(/<Join PU> address\[([^\]]*)\] port\[\d+\] shard\[([^\]]*)\]/))) {
      const region = (m[2].match(/^pub_([a-z]+\d)/) || [])[1] || '';
      return { type: 'session', shard: m[2], region, ts };
    }
    if ((m = body.match(/<AccountLoginCharacterStatus_Character> .*? name (\S+)/))) return { type: 'login', handle: m[1], ts };
    if ((m = body.match(/<RequestLocationInventory> Player\[([^\]]*)\] requested inventory for Location\[([^\]]*)\]/))) {
      return { type: 'location', code: m[2], ts };
    }
    return null;
  }

  return {
    reset() { pending = null; pendingBuy = null; },
    feed(line) {
      const tm = line.match(RE_TS);
      if (pending) {
        // Continuation lines carry their own timestamp prefix; drop it.
        pending.body += '\n' + (tm ? tm[2] : line);
        if (/to queue\./.test(pending.body) || pending.body.length > 4000) {
          const p = pending; pending = null;
          return parseFull(p.ts, p.body);
        }
        return null;
      }
      if (!tm) return null;
      const [, ts, body] = tm;
      if (body.includes('<SHUDEvent_OnNotification> Added notification') && !/to queue\./.test(body)) {
        pending = { ts, body };
        return null;
      }
      return parseFull(ts, body);
    },
  };
}

// Polls the file for growth. Game.log is rewritten from scratch each launch,
// so a shrinking file means a new session: start again from the top.
class JsLogTailer {
  constructor(file, onEvents, onStatus) {
    this.file = file;
    this.onEvents = onEvents;
    this.onStatus = onStatus;
    this.parser = createParser();
    this.offset = 0;
    this.rest = '';
    this.timer = null;
    this.state = null;
  }
  start() {
    this.offset = 0; this.rest = ''; this.parser.reset();
    this.tick(true);
    this.timer = setInterval(() => this.tick(false), 1000);
  }
  stop() { clearInterval(this.timer); this.timer = null; }
  status(state, extra) {
    if (state === this.state && !extra) return;
    this.state = state;
    this.onStatus({ state, file: this.file, ...extra });
  }
  tick(first) {
    let st;
    try { st = fs.statSync(this.file); } catch { return this.status('missing'); }
    if (st.size < this.offset) {
      this.offset = 0; this.rest = ''; this.parser.reset();
      this.onEvents([{ type: 'log_reset', ts: new Date().toISOString() }], false);
    }
    this.status('watching');
    if (st.size === this.offset) return;
    const len = st.size - this.offset;
    const buf = Buffer.alloc(len);
    let fd;
    try {
      fd = fs.openSync(this.file, 'r');
      fs.readSync(fd, buf, 0, len, this.offset);
    } catch (e) {
      return this.status('error', { message: e.message });
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
    this.offset = st.size;
    const lines = (this.rest + buf.toString('utf8')).split(/\r?\n/);
    this.rest = lines.pop();
    const events = [];
    for (const line of lines) {
      const ev = this.parser.feed(line);
      if (ev) events.push(ev);
    }
    // The first read is a catch-up of the session so far.
    if (events.length) this.onEvents(events, first);
  }
}

// Reads the game's older session logs (logbackups next to Game.log) from the
// last `days` days and returns every player sighting in them, so the app
// knows who you've crossed paths with even when it wasn't running.
async function jsScanRecentSightings(logFile, days) {
  const dir = path.join(path.dirname(logFile), 'logbackups');
  const cutoff = Date.now() - days * 86400000;
  let files;
  try {
    files = (await fs.promises.readdir(dir)).filter(f => f.toLowerCase().endsWith('.log'));
  } catch { return []; }
  const out = [];
  for (const f of files) {
    const full = path.join(dir, f);
    let st;
    try { st = await fs.promises.stat(full); } catch { continue; }
    if (st.mtimeMs < cutoff) continue;
    let text;
    try { text = await fs.promises.readFile(full, 'utf8'); } catch { continue; }
    const parser = createParser();
    let shard = '', contracts = {};
    for (const line of text.split(/\r?\n/)) {
      const ev = parser.feed(line);
      if (!ev) continue;
      if (ev.type === 'session') shard = ev.shard;
      else if (ev.type === 'contract' && ev.missionId) contracts[ev.missionId] = ev.name;
      else if (ev.type === 'sighting') out.push({ player: ev.player, ts: ev.ts, source: ev.source, shard });
      else if (ev.type === 'party' && ev.player && ev.action !== 'left') out.push({ player: ev.player, ts: ev.ts, source: 'party', shard });
      else if (ev.type === 'objective' && ev.status === 'new') {
        const m = ev.text.match(/^Neutralize (?!Targets?$)(\S+)$/);
        if (m && !/^(Verified|Reclusive) Bounty/i.test(contracts[ev.missionId] || '')) out.push({ player: m[1], ts: ev.ts, source: 'bounty', shard });
      }
    }
    // Let other work run between files.
    await new Promise(r => setImmediate(r));
  }
  return out;
}

// Rust port step 4: the parser, file reading and backup scan in Rust
// (native/core/src/gamelog.rs). JS keeps the 1-second timer and hands the
// results on. If the native module won't load, or a native call fails, the
// JavaScript versions above take over (a failed tailer restarts in JS, which
// re-reads the log as a catch-up, exactly like restarting the watcher).
let native = null, nativeError = null;
try { native = require(path.join(__dirname, 'native', 'cl_native.node')); }
catch (e) { nativeError = e.message; }

class LogTailer {
  constructor(file, onEvents, onStatus) {
    this.file = file; this.onEvents = onEvents; this.onStatus = onStatus;
    this.timer = null; this.nt = null; this.js = null;
    this.backend = native ? 'rust' : 'javascript';
  }
  start() {
    if (!native) return this.startJs();
    try {
      this.nt = new native.Tailer(this.file);
      this.apply(this.nt.start());
      this.timer = setInterval(() => {
        try { this.apply(this.nt.tick(false)); }
        catch (e) { this.fallBack(e); }
      }, 1000);
    } catch (e) { this.fallBack(e); }
  }
  apply(json) {
    for (const item of JSON.parse(json)) {
      if (item.kind === 'events') this.onEvents(item.events, item.backfill);
      else { const { kind, ...status } = item; this.onStatus(status); }
    }
  }
  fallBack(e) {
    console.error('Rust game log reader failed, using JavaScript:', e && e.message);
    clearInterval(this.timer); this.timer = null; this.nt = null;
    this.backend = 'javascript';
    this.startJs();
  }
  startJs() {
    this.js = new JsLogTailer(this.file, this.onEvents, this.onStatus);
    this.js.start();
  }
  stop() {
    clearInterval(this.timer); this.timer = null; this.nt = null;
    if (this.js) this.js.stop();
  }
}

async function scanRecentSightings(logFile, days) {
  if (!native) return jsScanRecentSightings(logFile, days);
  try { return JSON.parse(await native.scanRecentSightings(logFile, days)); }
  catch { return jsScanRecentSightings(logFile, days); }
}

module.exports = { createParser, LogTailer, JsLogTailer, detectLogPath, cleanText, scanRecentSightings, jsScanRecentSightings, nativeError };
