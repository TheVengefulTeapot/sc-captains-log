const { app, BrowserWindow, ipcMain, dialog, shell } = require('electron');
const path = require('path');
const { LogTailer, detectLogPath, scanRecentSightings } = require('./gamelog');
const { checkHandle } = require('./rsi');
const { createMarket } = require('./market');
const { createStore } = require('./store');
const fs = require('fs');
let market = null;

app.setName("SC Captain's Log");

let mainWindow;
let tailer = null;
let store = null;
function getStore() { if (!store) store = createStore(app.getPath('userData')); return store; }

// Only one copy of the app may run. A second copy can't open the shared
// storage, so anything entered in it would be lost on close; instead the
// existing window is brought to the front.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (!mainWindow) return;
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.show();
    mainWindow.focus();
  });
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1200,
    height: 800,
    minWidth: 900,
    minHeight: 600,
    frame: false,
    titleBarStyle: 'hidden',
    backgroundColor: '#0a0808',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
    icon: path.join(__dirname, 'assets', 'icon.ico'),
  });

  mainWindow.loadFile(path.join(__dirname, 'src', 'index.html'));
  // RSI profile links open in the user's browser, never inside the app.
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https:\/\/(robertsspaceindustries\.com|uexcorp\.space)\//.test(url)) shell.openExternal(url);
    return { action: 'deny' };
  });
}

app.whenReady().then(() => {
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

ipcMain.on('window-close',    () => mainWindow?.close());
ipcMain.on('window-minimize', () => mainWindow?.minimize());
ipcMain.on('window-maximize', () => {
  if (mainWindow?.isMaximized()) mainWindow.unmaximize();
  else mainWindow?.maximize();
});

// GAME LOG WATCHER
function sendToRenderer(channel, payload) {
  if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send(channel, payload);
}
ipcMain.handle('gamelog-start', (e, requested) => {
  if (tailer) tailer.stop();
  const file = requested || detectLogPath();
  if (!file) {
    tailer = null;
    sendToRenderer('gamelog-status', { state: 'notfound' });
    return null;
  }
  tailer = new LogTailer(file,
    (events, backfill) => sendToRenderer('gamelog-events', { events, backfill }),
    status => sendToRenderer('gamelog-status', status));
  tailer.start();
  // Who you've crossed paths with in earlier sessions (logbackups).
  scanRecentSightings(file, 14)
    .then(sightings => sendToRenderer('gamelog-recent', sightings))
    .catch(() => {});
  return file;
});
ipcMain.handle('gamelog-stop', () => {
  if (tailer) tailer.stop();
  tailer = null;
  sendToRenderer('gamelog-status', { state: 'off' });
});
ipcMain.handle('gamelog-detect', () => detectLogPath());
ipcMain.handle('gamelog-browse', async () => {
  const r = await dialog.showOpenDialog(mainWindow, {
    title: 'Select Star Citizen Game.log',
    filters: [{ name: 'Log files', extensions: ['log'] }],
    properties: ['openFile'],
  });
  return r.canceled ? null : r.filePaths[0];
});

// DATA STORE
// Load and save are synchronous so a save has reached the disk before the
// renderer carries on, and the last save before closing can't be dropped.
ipcMain.on('store-load', e => {
  try { e.returnValue = getStore().load(); }
  catch (err) { e.returnValue = { json: null, source: 'error', warnings: ['Could not read saved data: ' + err.message], error: true }; }
});
ipcMain.on('store-save', (e, text) => {
  try { e.returnValue = { ok: true, at: getStore().save(text) }; }
  catch (err) { e.returnValue = { ok: false, error: err.message }; }
});
ipcMain.handle('store-info', () => getStore().info());
ipcMain.handle('store-open-folder', () => shell.openPath(getStore().dir));
ipcMain.handle('store-export', async (e, text) => {
  const day = new Date().toISOString().slice(0, 10);
  const r = await dialog.showSaveDialog(mainWindow, {
    title: "Export Captain's Log backup",
    defaultPath: path.join(app.getPath('documents'), 'captains-log-' + day + '.json'),
    filters: [{ name: 'JSON', extensions: ['json'] }],
  });
  if (r.canceled || !r.filePath) return null;
  getStore().writeAtomic(r.filePath, text);
  return r.filePath;
});
ipcMain.handle('store-import-pick', async () => {
  const r = await dialog.showOpenDialog(mainWindow, {
    title: "Import Captain's Log backup",
    filters: [{ name: 'JSON', extensions: ['json'] }],
    properties: ['openFile'],
  });
  if (r.canceled || !r.filePaths[0]) return null;
  return fs.readFileSync(r.filePaths[0], 'utf8');
});

// RSI HANDLE CHECK
ipcMain.handle('rsi-check', (e, handle) => checkHandle(handle));

// MARKET PRICES
ipcMain.handle('market-get', (e, force) => {
  if (!market) market = createMarket(app.getPath('userData'));
  return market.get(!!force);
});
ipcMain.handle('market-history', (e, pairs, force) => {
  if (!market) market = createMarket(app.getPath('userData'));
  return market.getHistory(pairs, !!force);
});
