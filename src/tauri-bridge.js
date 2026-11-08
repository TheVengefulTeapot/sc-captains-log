// Tauri bridge: in the Tauri app, presents the Rust commands as the same
// window.electronAPI that Electron's preload.js provides, so app.js runs
// unchanged in both. Does nothing in Electron (the preload has already set
// electronAPI, and there is no window.__TAURI__).
(function () {
  const T = window.__TAURI__;
  if (!T || window.electronAPI) return;
  const invoke = T.core.invoke;
  const listen = T.event.listen;

  // Saved data is read before app.js's load() runs (it awaits __clReady).
  window.__clReady = invoke('store_load')
    .then(r => { window.__clStoreLoad = r; })
    .catch(e => { window.__clStoreLoad = { json: null, source: 'error', warnings: ['Could not read saved data: ' + e], error: true }; });

  // Saves go to disk one after another in the order they were made. app.js
  // expects an answer straight away, so a save reports success at once and a
  // failure, if the write then fails, brings up the save-problem banner.
  let saveChain = Promise.resolve();
  const problem = msg => { if (typeof showSaveProblem === 'function') showSaveProblem(msg); };
  function save(text) {
    saveChain = saveChain
      .then(() => invoke('store_save', { text }))
      .then(r => { if (!r.ok) problem('NOT SAVED: ' + r.error); else if (typeof hideSaveProblem === 'function') hideSaveProblem(); })
      .catch(e => problem('NOT SAVED: ' + e));
    return { ok: true, at: new Date().toISOString() };
  }
  // Closing waits for the last save to reach the disk.
  const closeAfterSaves = () => saveChain.finally(() => invoke('win_destroy'));
  listen('cl-close-requested', closeAfterSaves);

  window.electronAPI = {
    close: closeAfterSaves,
    minimize: () => invoke('win_minimize'),
    maximize: () => invoke('win_maximize'),
    rsiCheck: handle => invoke('rsi_check', { handle: String(handle || '') }),
    marketGet: force => invoke('market_get', { force: !!force }),
    marketHistory: (pairs, force) => invoke('market_history', { pairs: pairs || [], force: !!force }),
    shipsGet: () => invoke('ships_get'),
    store: {
      load: () => window.__clStoreLoad,
      save,
      info: () => invoke('store_info'),
      openFolder: () => invoke('store_open_folder'),
      exportTo: text => invoke('store_export', { text }),
      importPick: () => invoke('store_import_pick'),
    },
    gameLog: {
      start: file => invoke('gamelog_start', { file: file || null }),
      stop: () => invoke('gamelog_stop'),
      detect: () => invoke('gamelog_detect'),
      browse: () => invoke('gamelog_browse'),
      onEvents: cb => listen('gamelog-events', e => cb(e.payload)),
      onStatus: cb => listen('gamelog-status', e => cb(e.payload)),
      onRecent: cb => listen('gamelog-recent', e => cb(e.payload)),
    },
  };

  // RSI profile and UEX links open in the browser (Electron did this with
  // setWindowOpenHandler); the Rust side only opens those two sites.
  document.addEventListener('click', e => {
    const a = e.target.closest && e.target.closest('a[target="_blank"]');
    if (!a) return;
    e.preventDefault();
    invoke('open_url', { url: a.href });
  }, true);

  // The frameless window is dragged by its title bar. Electron used the
  // -webkit-app-region CSS; Tauri uses this attribute on the bar and its
  // plain text parts (buttons and inputs stay clickable).
  document.addEventListener('DOMContentLoaded', () => {
    document.querySelectorAll('.titlebar, .titlebar-title, .titlebar > div:not(.titlebar-controls)').forEach(el => {
      if (!el.querySelector('button, input')) el.setAttribute('data-tauri-drag-region', '');
    });
    const bar = document.querySelector('.titlebar');
    if (bar) bar.setAttribute('data-tauri-drag-region', '');
  });
})();
