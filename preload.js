const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('electronAPI', {
  close:    () => ipcRenderer.send('window-close'),
  minimize: () => ipcRenderer.send('window-minimize'),
  maximize: () => ipcRenderer.send('window-maximize'),
  store: {
    load:       () => ipcRenderer.sendSync('store-load'),
    save:       (text) => ipcRenderer.sendSync('store-save', text),
    info:       () => ipcRenderer.invoke('store-info'),
    openFolder: () => ipcRenderer.invoke('store-open-folder'),
    exportTo:   (text) => ipcRenderer.invoke('store-export', text),
    importPick: () => ipcRenderer.invoke('store-import-pick'),
  },
  rsiCheck: (handle) => ipcRenderer.invoke('rsi-check', handle),
  marketGet: (force) => ipcRenderer.invoke('market-get', force),
  marketHistory: (pairs, force) => ipcRenderer.invoke('market-history', pairs, force),
  gameLog: {
    start:    (file) => ipcRenderer.invoke('gamelog-start', file || null),
    stop:     () => ipcRenderer.invoke('gamelog-stop'),
    detect:   () => ipcRenderer.invoke('gamelog-detect'),
    browse:   () => ipcRenderer.invoke('gamelog-browse'),
    onEvents: (cb) => ipcRenderer.on('gamelog-events', (e, payload) => cb(payload)),
    onStatus: (cb) => ipcRenderer.on('gamelog-status', (e, status) => cb(status)),
    onRecent: (cb) => ipcRenderer.on('gamelog-recent', (e, sightings) => cb(sightings)),
  },
});
