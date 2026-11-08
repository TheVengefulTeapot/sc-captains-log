// SC Captain's Log, Tauri shell. Replaces Electron's main.js: the window, the
// commands the page calls (through src/tauri-bridge.js, which presents them
// as the same window.electronAPI the Electron preload did), and the game log
// watcher thread. The real work is in native/core, shared with the Electron
// build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cl_native::{gamelog, market::Market, rsi, store::Store};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct App {
    store: Mutex<Store>,
    market: Market,
    data_dir: PathBuf,
    watcher: Mutex<Option<Arc<AtomicBool>>>, // stop flag of the running watcher
}

// Same folder the Electron app used (%APPDATA%\SC Captain's Log), so the
// switch keeps every save, backup and the price cache. CL_DATA_DIR points it
// elsewhere for testing.
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CL_DATA_DIR") { return PathBuf::from(d); }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("SC Captain's Log")
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Result<T, String>> {
    let h = tauri::async_runtime::spawn_blocking(f);
    async move { h.await.map_err(|e| e.to_string()) }
}

// ---- window ----

#[tauri::command]
fn win_minimize(w: tauri::WebviewWindow) { let _ = w.minimize(); }
#[tauri::command]
fn win_maximize(w: tauri::WebviewWindow) {
    if w.is_maximized().unwrap_or(false) { let _ = w.unmaximize(); } else { let _ = w.maximize(); }
}
/// Called by the page once its last save has finished.
#[tauri::command]
fn win_destroy(w: tauri::WebviewWindow) { let _ = w.destroy(); }

// ---- data store ----

#[tauri::command]
fn store_load(app: State<App>) -> Value {
    match app.store.lock().unwrap().load() {
        Ok(r) => json!({ "json": r.json, "source": r.source, "warnings": r.warnings }),
        Err(e) => json!({ "json": null, "source": "error", "warnings": [format!("Could not read saved data: {}", e)], "error": true }),
    }
}
#[tauri::command]
fn store_save(app: State<App>, text: String) -> Value {
    match app.store.lock().unwrap().save(text) {
        Ok(at) => json!({ "ok": true, "at": at }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}
#[tauri::command]
fn store_info(app: State<App>) -> Value {
    let i = app.store.lock().unwrap().info();
    json!({ "dir": i.dir, "file": i.file, "lastSaved": i.last_saved, "backups": i.backups, "backend": "rust" })
}
#[tauri::command]
fn store_open_folder(handle: AppHandle, app: State<App>) {
    let _ = handle.opener().open_path(app.data_dir.to_string_lossy(), None::<&str>);
}
#[tauri::command]
async fn store_export(handle: AppHandle, text: String) -> Result<Option<String>, String> {
    let day: String = cl_native::store::iso(now_ms())[..10].to_string();
    let docs = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Documents")).unwrap_or_default();
    let picked = handle.dialog().file()
        .set_title("Export Captain's Log backup")
        .set_directory(docs)
        .set_file_name(format!("captains-log-{}.json", day))
        .add_filter("JSON", &["json"])
        .blocking_save_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    let state = handle.state::<App>();
    let r = state.store.lock().unwrap().write_atomic(path.to_string_lossy().into_owned(), text);
    r.map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}
#[tauri::command]
async fn store_import_pick(handle: AppHandle) -> Result<Option<String>, String> {
    let picked = handle.dialog().file()
        .set_title("Import Captain's Log backup")
        .add_filter("JSON", &["json"])
        .blocking_pick_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    std::fs::read_to_string(path).map(Some).map_err(|e| e.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- RSI, market ----

#[tauri::command]
async fn rsi_check(handle: String) -> Result<Value, String> {
    blocking(move || rsi::check(&handle)).await
}
#[tauri::command]
async fn market_get(app: State<'_, App>, force: bool) -> Result<Value, String> {
    let m = app.market.clone();
    let text = blocking(move || m.get_now(force)).await?.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}
#[tauri::command]
async fn market_history(app: State<'_, App>, pairs: Value, force: bool) -> Result<Value, String> {
    let m = app.market.clone();
    let pairs = pairs.to_string();
    let text = blocking(move || m.history_now(&pairs, force)).await?.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[tauri::command]
async fn ships_get(app: State<'_, App>) -> Result<Value, String> {
    let m = app.market.clone();
    let text = blocking(move || m.ships_now(false)).await?.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// RSI profile and UEX links open in the user's browser, nothing else.
#[tauri::command]
fn open_url(handle: AppHandle, url: String) {
    let ok = url.starts_with("https://robertsspaceindustries.com/") || url.starts_with("https://uexcorp.space/") || url == "https://uexcorp.space";
    if ok { let _ = handle.opener().open_url(url, None::<&str>); }
}

// ---- game log ----

// Common install locations, checked in order when no path is set (as gamelog.js).
fn detect_log_path() -> Option<String> {
    for drive in ['C', 'D', 'E', 'F', 'G'] {
        for p in [
            format!("{}:\\StarCitizen\\StarCitizen\\LIVE\\Game.log", drive),
            format!("{}:\\Program Files\\Roberts Space Industries\\StarCitizen\\LIVE\\Game.log", drive),
            format!("{}:\\Roberts Space Industries\\StarCitizen\\LIVE\\Game.log", drive),
        ] {
            if PathBuf::from(&p).exists() { return Some(p); }
        }
    }
    None
}

fn send_tick(handle: &AppHandle, json_text: &str) {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json_text) else { return };
    for mut item in items {
        let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
        if kind == "events" {
            let _ = handle.emit("gamelog-events", json!({ "events": item["events"], "backfill": item["backfill"] }));
        } else {
            if let Some(o) = item.as_object_mut() { o.remove("kind"); }
            let _ = handle.emit("gamelog-status", item);
        }
    }
}

fn stop_watcher(app: &App) {
    if let Some(flag) = app.watcher.lock().unwrap().take() { flag.store(true, Ordering::SeqCst); }
}

#[tauri::command]
fn gamelog_start(handle: AppHandle, app: State<App>, file: Option<String>) -> Option<String> {
    stop_watcher(&app);
    let file = file.filter(|f| !f.is_empty()).or_else(detect_log_path);
    let Some(file) = file else {
        let _ = handle.emit("gamelog-status", json!({ "state": "notfound" }));
        return None;
    };
    let stop = Arc::new(AtomicBool::new(false));
    *app.watcher.lock().unwrap() = Some(stop.clone());
    // Reads once a second, like the Electron watcher. The first read is the
    // catch-up of the session so far.
    let (h, f) = (handle.clone(), file.clone());
    std::thread::spawn(move || {
        let mut tailer = gamelog::Tailer::new(f);
        match tailer.start() { Ok(t) => send_tick(&h, &t), Err(e) => eprintln!("game log: {}", e) }
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(1));
            if stop.load(Ordering::SeqCst) { break; }
            match tailer.tick(false) { Ok(t) => send_tick(&h, &t), Err(e) => eprintln!("game log: {}", e) }
        }
    });
    // Who you've crossed paths with in earlier sessions (logbackups).
    let (h, f) = (handle.clone(), file.clone());
    std::thread::spawn(move || {
        let found = std::panic::catch_unwind(|| gamelog::scan(&f, 14.0)).unwrap_or_default();
        let _ = h.emit("gamelog-recent", Value::Array(found));
    });
    Some(file)
}
#[tauri::command]
fn gamelog_stop(handle: AppHandle, app: State<App>) {
    stop_watcher(&app);
    let _ = handle.emit("gamelog-status", json!({ "state": "off" }));
}
#[tauri::command]
fn gamelog_detect() -> Option<String> { detect_log_path() }
#[tauri::command]
async fn gamelog_browse(handle: AppHandle) -> Option<String> {
    handle.dialog().file()
        .set_title("Select Star Citizen Game.log")
        .add_filter("Log files", &["log"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

/// The taskbar and Alt-Tab use a window's large icon, which Tauri leaves
/// unset (it only sets the small title-bar one). Give the window the large
/// version of the program's own icon, the ship's wheel from icons/icon.ico.
#[cfg(windows)]
fn set_taskbar_icon(hwnd: windows_sys::Win32::Foundation::HWND) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ExtractIconExW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, HICON, ICON_BIG, WM_SETICON};
    let Ok(exe) = std::env::current_exe() else { return };
    let path: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut large: HICON = std::ptr::null_mut();
    unsafe {
        if ExtractIconExW(path.as_ptr(), 0, &mut large, std::ptr::null_mut(), 1) > 0 && !large.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, large as isize);
        }
    }
}

fn main() {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let dir_s = dir.to_string_lossy().into_owned();
    tauri::Builder::default()
        // Only one copy may run: a second launch brings this window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(App {
            store: Mutex::new(Store::new(dir_s.clone())),
            market: Market::new(dir_s),
            data_dir: dir,
            watcher: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            win_minimize, win_maximize, win_destroy,
            store_load, store_save, store_info, store_open_folder, store_export, store_import_pick,
            rsi_check, market_get, market_history, ships_get, open_url,
            gamelog_start, gamelog_stop, gamelog_detect, gamelog_browse,
        ])
        .setup(|app| {
            // The app icon (icons/icon.ico) for the taskbar and Alt-Tab: a
            // window made here doesn't get it unless it's set explicitly.
            let icon = app.default_window_icon().cloned();
            let mut w = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()));
            if let Some(icon) = icon { w = w.icon(icon)?; }
            let w = w
                .title("SC Captain's Log")
                .inner_size(1200.0, 800.0)
                .min_inner_size(900.0, 600.0)
                .decorations(false)
                .background_color(tauri::window::Color(10, 8, 8, 255))
                // Draw on the CPU, never the graphics card: the app then has
                // no GPU process sharing the driver with Star Citizen (the
                // game crashed in NVIDIA's driver as this app started up
                // mid-session). The first three flags are Tauri's defaults,
                // which setting this replaces.
                .additional_browser_args(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                     --disable-gpu --disable-gpu-compositing",
                )
                .build()?;
            #[cfg(windows)]
            if let Ok(hwnd) = w.hwnd() { set_taskbar_icon(hwnd.0 as _); }
            // Closing waits for the page's last save: the page is told, then
            // calls win_destroy. If it doesn't answer in 3s, close anyway.
            let w2 = w.clone();
            w.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    let _ = w2.emit("cl-close-requested", ());
                    let w3 = w2.clone();
                    std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(3)); let _ = w3.destroy(); });
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running SC Captain's Log");
}
