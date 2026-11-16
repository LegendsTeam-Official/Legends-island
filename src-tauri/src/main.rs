#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod tune;
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
#[path = "lin.rs"]
mod win;

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;

const API_PORT: u16 = 47821;
const ISLAND_W: f64 = 760.0;
const ISLAND_H: f64 = 480.0;

// ---------- настройки ----------
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    style: String, // "float" | "edge"
    color: String,
    magnet: bool,
    strength: f64,
    breathe: bool,
    notify_downloads: bool,
    notify_media: bool,
    autostart: bool,
    telegram: bool,
    other_apps: bool,
    clipboard: bool,
    theme: String,
    game: bool,
    auto_hide: bool,
    jelly: bool,
    drops: bool,
    pulse: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Settings {
            style: "float".into(),
            color: "#C66B3D".into(),
            magnet: true,
            strength: 0.55,
            breathe: true,
            notify_downloads: true,
            notify_media: true,
            autostart: true,
            telegram: false,
            other_apps: false,
            clipboard: true,
            theme: "clay".into(),
            game: false,
            auto_hide: true,
            jelly: true,
            drops: true,
            pulse: true,
        }
    }
}

struct AppState {
    settings: Mutex<Settings>,
    clips: Mutex<Vec<ClipItem>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ClipItem {
    text: String,
    ts: i64,
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}
fn load_settings(app: &AppHandle) -> (Settings, bool) {
    if let Some(p) = settings_path(app) {
        if let Ok(t) = fs::read_to_string(&p) {
            if let Ok(s) = serde_json::from_str(&t) {
                return (s, false);
            }
        }
    }
    (Settings::default(), true)
}
fn write_settings(app: &AppHandle, s: &Settings) {
    if let Some(p) = settings_path(app) {
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default());
    }
}
fn apply_autostart(app: &AppHandle, on: bool) {
    let al = app.autolaunch();
    if on {
        let _ = al.enable();
    } else if al.is_enabled().unwrap_or(false) {
        let _ = al.disable();
    }
}
fn cur_settings(app: &AppHandle) -> Settings {
    app.state::<AppState>().settings.lock().unwrap().clone()
}

// ---------- команды ----------
#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}
#[tauri::command]
fn save_settings(app: AppHandle, state: tauri::State<AppState>, settings: Settings) {
    *state.settings.lock().unwrap() = settings.clone();
    write_settings(&app, &settings);
    apply_autostart(&app, settings.autostart);
    fps_flag(settings.game);
    let _ = app.emit("settings", settings);
}
#[tauri::command]
fn set_interactive(window: WebviewWindow, on: bool) {
    let _ = window.set_ignore_cursor_events(!on);
}
#[tauri::command]
fn focus_window(window: WebviewWindow) {
    let _ = window.set_focus();
}
#[tauri::command]
async fn open_settings(app: AppHandle) {
    open_settings_win(&app);
}
#[tauri::command]
fn close_window(window: WebviewWindow) {
    let _ = window.close();
}
#[tauri::command]
fn quit(app: AppHandle) {
    fps_flag(false);
    ai::stop(&app);
    app.exit(0);
}
#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[cfg(windows)]
fn spawn_explorer(arg: String) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer")
        .raw_arg(arg)
        .creation_flags(0x0800_0000)
        .spawn();
}
#[cfg(not(windows))]
fn spawn_explorer(_arg: String) {}

#[tauri::command]
fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "linux" }
}
#[tauri::command]
fn reveal_file(path: String) {
    #[cfg(windows)]
    spawn_explorer(format!("/select,\"{}\"", path));
    #[cfg(not(windows))]
    {
        let dir = Path::new(&path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or(path);
        win::spawn("xdg-open", &[&dir]);
    }
}
#[tauri::command]
fn open_file(path: String) {
    #[cfg(windows)]
    spawn_explorer(format!("\"{}\"", path));
    #[cfg(not(windows))]
    win::spawn("xdg-open", &[&path]);
}

// ---------- музыка ----------
#[tauri::command]
fn media_cmd(action: String) {
    std::thread::spawn(move || win::media_command(&action));
}
#[tauri::command]
async fn get_volume() -> f32 {
    tauri::async_runtime::spawn_blocking(win::get_volume).await.unwrap_or(-1.0)
}
#[tauri::command]
async fn set_volume(v: f32) {
    let _ = tauri::async_runtime::spawn_blocking(move || win::set_volume(v)).await;
}

static PLAYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// пульс под музыку: уровень системного звука ~25 раз в секунду, только когда что-то играет
fn watch_level(app: AppHandle) {
    std::thread::spawn(move || {
        let mut meter: Option<win::Peak> = None;
        let mut made = Instant::now();
        let mut last = 0.0f32;
        loop {
            std::thread::sleep(Duration::from_millis(40));
            if !cur_settings(&app).pulse || !PLAYING.load(std::sync::atomic::Ordering::Relaxed) {
                if last > 0.0 {
                    last = 0.0;
                    let _ = app.emit_to("island", "level", 0.0f32);
                }
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }
            if meter.is_none() || made.elapsed() > Duration::from_secs(10) {
                meter = win::peak_meter();
                made = Instant::now();
            }
            let lv = meter.as_ref().map(|m| m.level()).unwrap_or(0.0);
            if (lv - last).abs() > 0.02 {
                last = lv;
                let _ = app.emit_to("island", "level", lv);
            }
        }
    });
}

fn watch_media(app: AppHandle) {
    std::thread::spawn(move || {
        win::winrt_init();
        let mut mgr = None;
        let mut last = win::MediaInfo::default();
        let (mut key, mut art) = (String::new(), String::new());
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            if mgr.is_none() {
                mgr = win::media_manager();
            }
            let Some(m) = mgr.as_ref() else { continue };
            let info = win::media_read(m, &mut key, &mut art);
            PLAYING.store(info.playing, std::sync::atomic::Ordering::Relaxed);
            if info != last {
                last = info.clone();
                let _ = app.emit_to("island", "media", info);
            }
        }
    });
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ---------- хранилище данных интерфейса (заметки, напоминания, погода…) ----------
fn data_file(app: &AppHandle, key: &str) -> Option<PathBuf> {
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let d = app.path().app_config_dir().ok()?.join("data");
    let _ = fs::create_dir_all(&d);
    Some(d.join(format!("{}.json", key)))
}
#[tauri::command]
fn data_get(app: AppHandle, key: String) -> String {
    data_file(&app, &key).and_then(|p| fs::read_to_string(p).ok()).unwrap_or_else(|| "null".into())
}
#[tauri::command]
fn data_set(app: AppHandle, key: String, value: String) -> Result<(), String> {
    let p = data_file(&app, &key).ok_or("плохой ключ")?;
    fs::write(p, value).map_err(|e| e.to_string())
}

// ---------- сеть (погода, тексты песен, календари) ----------
#[tauri::command]
async fn http_get(url: String) -> Result<String, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("только http(s)".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        ureq::get(&url)
            .set("User-Agent", "LegendsIsland/0.2 (+https://island.lgscode.online)")
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- приложения и запуск ----------
#[derive(Serialize)]
struct AppEntry {
    name: String,
    path: String,
}
fn walk_lnk(dir: &Path, depth: u32, out: &mut Vec<AppEntry>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                walk_lnk(&p, depth + 1, out);
            }
            continue;
        }
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
        if ext != "lnk" && ext != "url" && ext != "appref-ms" {
            continue;
        }
        let name = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let l = name.to_lowercase();
        if l.contains("uninstall") || l.contains("удал") || l.contains("readme") {
            continue;
        }
        out.push(AppEntry { name, path: p.to_string_lossy().to_string() });
    }
}
#[tauri::command]
async fn list_apps() -> Vec<AppEntry> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut out = Vec::new();
        #[cfg(windows)]
        for var in ["ProgramData", "APPDATA"] {
            if let Ok(base) = std::env::var(var) {
                walk_lnk(&Path::new(&base).join("Microsoft").join("Windows").join("Start Menu").join("Programs"), 0, &mut out);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = walk_lnk;
            let home = std::env::var("HOME").unwrap_or_default();
            for d in [
                "/usr/share/applications".to_string(),
                "/usr/local/share/applications".into(),
                format!("{}/.local/share/applications", home),
                "/var/lib/flatpak/exports/share/applications".into(),
                format!("{}/.local/share/flatpak/exports/share/applications", home),
                "/var/lib/snapd/desktop/applications".into(),
            ] {
                let Ok(rd) = fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().map(|x| x != "desktop").unwrap_or(true) {
                        continue;
                    }
                    let Ok(t) = fs::read_to_string(&p) else { continue };
                    if win::desktop_field(&t, "NoDisplay").as_deref() == Some("true") || win::desktop_field(&t, "Type").as_deref() != Some("Application") {
                        continue;
                    }
                    let name = win::desktop_field(&t, "Name[ru]").or_else(|| win::desktop_field(&t, "Name")).unwrap_or_default();
                    if !name.is_empty() {
                        out.push(AppEntry { name, path: p.to_string_lossy().to_string() });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
        out
    })
    .await
    .unwrap_or_default()
}
#[tauri::command]
fn open_target(target: String) {
    #[cfg(windows)]
    spawn_explorer(format!("\"{}\"", target));
    #[cfg(not(windows))]
    {
        if target.ends_with(".desktop") {
            let id = Path::new(&target).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if std::process::Command::new("gtk-launch").arg(&id).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_err() {
                win::spawn("gio", &["launch", &target]);
            }
        } else {
            win::spawn("xdg-open", &[&target]);
        }
    }
}

// ---------- буфер обмена: история ----------
fn clips_save(app: &AppHandle, v: &[ClipItem]) {
    if let Some(p) = data_file(app, "clips") {
        let _ = fs::write(p, serde_json::to_string(v).unwrap_or_default());
    }
}
fn watch_clipboard(app: AppHandle) {
    std::thread::spawn(move || {
        let mut seq = win::clipboard_seq();
        loop {
            std::thread::sleep(Duration::from_millis(700));
            let s = win::clipboard_seq();
            if s == seq {
                continue;
            }
            seq = s;
            if !cur_settings(&app).clipboard {
                continue;
            }
            let Ok(mut cb) = arboard::Clipboard::new() else { continue };
            let Ok(t) = cb.get_text() else { continue };
            let t = t.trim_end().to_string();
            if t.trim().is_empty() || t.len() > 20_000 {
                continue;
            }
            let list = {
                let st = app.state::<AppState>();
                let mut v = st.clips.lock().unwrap();
                v.retain(|c| c.text != t);
                v.insert(0, ClipItem { text: t, ts: now_ms() });
                v.truncate(40);
                v.clone()
            };
            clips_save(&app, &list);
            let _ = app.emit_to("island", "clips", list);
        }
    });
}
#[tauri::command]
fn clip_list(state: tauri::State<AppState>) -> Vec<ClipItem> {
    state.clips.lock().unwrap().clone()
}
#[tauri::command]
fn clip_copy(text: String) -> Result<(), String> {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).map_err(|e| e.to_string())
}
#[tauri::command]
fn clip_remove(app: AppHandle, state: tauri::State<AppState>, text: String) -> Vec<ClipItem> {
    let v = {
        let mut v = state.clips.lock().unwrap();
        v.retain(|c| c.text != text);
        v.clone()
    };
    clips_save(&app, &v);
    v
}
#[tauri::command]
fn clip_clear(app: AppHandle, state: tauri::State<AppState>) {
    state.clips.lock().unwrap().clear();
    clips_save(&app, &[]);
}

// ---------- быстрые действия ----------
#[derive(Clone, Serialize)]
struct ActionResult {
    kind: String,
    state: String, // wait | ok | fail | live
    title: String,
    text: String,
    extra: String,
}
fn act(app: &AppHandle, kind: &str, state: &str, title: &str, text: &str, extra: &str) {
    let _ = app.emit_to(
        "island",
        "action",
        ActionResult { kind: kind.into(), state: state.into(), title: title.into(), text: text.into(), extra: extra.into() },
    );
}
fn set_clip_image(rgba: Vec<u8>, w: usize, h: usize) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_image(arboard::ImageData { width: w, height: h, bytes: std::borrow::Cow::Owned(rgba) });
    }
}
fn wait_clip_image(old_seq: u32, secs: u64) -> Option<(usize, usize, Vec<u8>)> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(300));
        if win::clipboard_seq() != old_seq {
            std::thread::sleep(Duration::from_millis(250));
            let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
            return Some((img.width, img.height, img.bytes.into_owned()));
        }
    }
    None
}
fn clean_text(t: &str) -> String {
    let t = t.replace("\r\n", "\n").replace('\u{a0}', " ").replace('\u{200b}', "");
    let lines: Vec<String> = t.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    let mut out = String::new();
    let mut blank = 0;
    for l in lines {
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&l);
        out.push('\n');
    }
    out.trim().to_string()
}
fn run_action(app: AppHandle, kind: String) {
    let k = kind.as_str();
    match k {
        #[cfg(windows)]
        "region" => spawn_explorer("ms-screenclip:".into()),
        #[cfg(not(windows))]
        "region" => {
            win::region_to_clipboard();
            act(&app, k, "ok", "Область скопирована", "Картинка в буфере обмена", "");
        }
        "screen" => {
            let w = app.get_webview_window("island");
            if let Some(w) = &w {
                let _ = w.hide();
            }
            std::thread::sleep(Duration::from_millis(250));
            let shot = win::capture_screen();
            if let Some(w) = &w {
                let _ = w.show();
            }
            match shot {
                Some(s) => {
                    let p = win::screenshots_dir(app.path().picture_dir().ok()).join(format!("Legends_{}.png", now_ms()));
                    let saved = win::save_png(&p, &s);
                    set_clip_image(s.rgba, s.w, s.h);
                    act(&app, k, "ok", "Скриншот готов", if saved { "Сохранён в «Изображения\\Screenshots» и скопирован" } else { "Скопирован в буфер" }, &p.to_string_lossy());
                }
                None => act(&app, k, "fail", "Не удалось снять экран", "", ""),
            }
        }
        #[cfg(not(windows))]
        "ocr" | "qr" => {
            act(&app, k, "wait", "Выдели область", if k == "ocr" { "Распознаю текст" } else { "Найду QR-код" }, "");
            let (w, h, rgba, file) = match win::snip_region() {
                Ok(x) => x,
                Err(e) => {
                    act(&app, k, "fail", if e == "cancel" { "Отменено" } else { "Не получилось" }, if e == "cancel" { "" } else { &e }, "");
                    return;
                }
            };
            if k == "ocr" {
                match win::ocr_file(&file) {
                    Ok(t) if !t.trim().is_empty() => {
                        let _ = clip_copy(t.clone());
                        act(&app, k, "ok", "Текст скопирован", &t, "");
                    }
                    Ok(_) => act(&app, k, "fail", "Текст не найден", "Попробуй выделить крупнее", ""),
                    Err(e) => act(&app, k, "fail", "Не удалось распознать", &e, ""),
                }
            } else {
                match win::qr_rgba(w, h, &rgba) {
                    Some(t) => {
                        let _ = clip_copy(t.clone());
                        act(&app, k, "ok", "QR-код прочитан", &t, if t.starts_with("http") { "url" } else { "" });
                    }
                    None => act(&app, k, "fail", "QR-код не найден", "Выдели код целиком", ""),
                }
            }
            let _ = fs::remove_file(file);
        }
        #[cfg(windows)]
        "ocr" | "qr" => {
            let seq = win::clipboard_seq();
            spawn_explorer("ms-screenclip:".into());
            act(&app, k, "wait", "Выдели область", if k == "ocr" { "Распознаю текст" } else { "Найду QR-код" }, "");
            let Some((w, h, rgba)) = wait_clip_image(seq, 90) else {
                act(&app, k, "fail", "Отменено", "", "");
                return;
            };
            if k == "ocr" {
                match win::ocr_rgba(w, h, &rgba) {
                    Ok(t) if !t.trim().is_empty() => {
                        let _ = clip_copy(t.clone());
                        act(&app, k, "ok", "Текст скопирован", &t, "");
                    }
                    Ok(_) => act(&app, k, "fail", "Текст не найден", "Попробуй выделить крупнее", ""),
                    Err(e) => act(&app, k, "fail", "Не удалось распознать", &e, ""),
                }
            } else {
                match win::qr_rgba(w, h, &rgba) {
                    Some(t) => {
                        let _ = clip_copy(t.clone());
                        act(&app, k, "ok", "QR-код прочитан", &t, if t.starts_with("http") { "url" } else { "" });
                    }
                    None => act(&app, k, "fail", "QR-код не найден", "Выдели код целиком", ""),
                }
            }
        }
        "picker" => {
            let start = Instant::now();
            let mut last = String::new();
            // ждём, пока отпустят Ctrl, если он был зажат
            while win::key_down(0x11) && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(30));
            }
            while start.elapsed() < Duration::from_secs(40) {
                std::thread::sleep(Duration::from_millis(40));
                let Some((_, _, hex)) = win::cursor_pixel() else { continue };
                if win::key_down(0x1B) {
                    act(&app, k, "fail", "Пипетка закрыта", "", "");
                    return;
                }
                if win::key_down(0x11) {
                    let _ = clip_copy(hex.clone());
                    act(&app, k, "ok", "Цвет скопирован", &hex, &hex);
                    return;
                }
                if hex != last {
                    last = hex.clone();
                    act(&app, k, "live", "Наведи и нажми Ctrl", &hex, &hex);
                }
            }
            act(&app, k, "fail", "Пипетка закрыта", "", "");
        }
        "clean" => match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => {
                let c = clean_text(&t);
                let _ = clip_copy(c.clone());
                act(&app, k, "ok", "Текст очищен", &c, "");
            }
            Err(_) => act(&app, k, "fail", "В буфере нет текста", "", ""),
        },
        "record" => {
            if cfg!(windows) {
                win::press_combo(&[0x5B, 0x12, 0x52]); // Win+Alt+R
                act(&app, k, "ok", "Запись экрана", "Win+Alt+R — Xbox Game Bar. Повтори, чтобы остановить", "");
            } else {
                act(&app, k, "ok", "Запись экрана", "В Cinnamon: Ctrl+Alt+Shift+R — старт и стоп, видео в папке «Видео»", "");
            }
        }
        _ => {}
    }
}
#[tauri::command]
fn action(app: AppHandle, kind: String) {
    std::thread::spawn(move || run_action(app, kind));
}


// ---------- игровой режим: нагрузка, FPS, полноэкранные приложения ----------
fn fps_state_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into())).join("LegendsIsland").join("state")
}
fn fps_flag(on: bool) {
    let f = fps_state_dir().join("fps.on");
    if on {
        if fps_state_dir().join("setup.ok").exists() {
            let _ = fs::write(f, "1");
        }
    } else {
        let _ = fs::remove_file(f);
    }
}
#[derive(Clone, Serialize)]
struct Stats {
    cpu: f64,
    gpu: Option<f64>,
    gpu_name: String,
    ram_used: u64,
    ram_total: u64,
    fps: Option<u32>,
    app: String,
    fps_ready: bool,
}
fn read_fps(pid: u32, name: &str) -> Option<u32> {
    let p = fps_state_dir().join("fps.json");
    let meta = fs::metadata(&p).ok()?;
    if meta.modified().ok()?.elapsed().ok()? > Duration::from_secs(4) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
    let obj = v.as_object()?;
    if let Some(e) = obj.get(&pid.to_string()) {
        return e.get(1)?.as_u64().map(|x| x as u32);
    }
    // рабочий стол и проводник рисует dwm.exe
    let l = name.to_lowercase();
    if l.is_empty() || l == "explorer.exe" || l == "legends-island.exe" {
        for (_, e) in obj {
            if e.get(0).and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case("dwm.exe")).unwrap_or(false) {
                return e.get(1)?.as_u64().map(|x| x as u32);
            }
        }
    }
    None
}
fn watch_stats(app: AppHandle) {
    std::thread::spawn(move || {
        let mut cpu = win::CpuMeter::new();
        let mut gpu = win::GpuMeter::new();
        let gname = win::gpu_name();
        let mut last_fs = false;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let fs_now = win::fullscreen_busy();
            if fs_now != last_fs {
                last_fs = fs_now;
                let _ = app.emit_to("island", "fullscreen", fs_now);
            }
            let (pid, name) = win::foreground();
            let (ru, rt) = win::ram();
            let st = Stats {
                cpu: cpu.percent(),
                gpu: gpu.percent(),
                gpu_name: gname.clone(),
                ram_used: ru,
                ram_total: rt,
                fps: if cur_settings(&app).game { read_fps(pid, &name) } else { None },
                app: name,
                fps_ready: fps_state_dir().join("setup.ok").exists(),
            };
            let _ = app.emit_to("island", "stats", st);
        }
    });
}
#[tauri::command]
fn fps_status() -> String {
    let d = fps_state_dir();
    if d.join("setup.ok").exists() {
        "ok".into()
    } else if let Ok(e) = fs::read_to_string(d.join("setup.err")) {
        format!("err:{}", e.trim())
    } else {
        "none".into()
    }
}
#[tauri::command]
fn fps_setup(app: AppHandle) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Счётчик FPS пока есть только в Windows-версии".into());
    }
    let res = app.path().resource_dir().map_err(|e| e.to_string())?.join("resources");
    let script = res.join("fps-setup.ps1");
    if !script.exists() {
        return Err("нет файлов помощника FPS".into());
    }
    let args = format!(
        "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" -src \"{}\"",
        script.to_string_lossy(),
        res.to_string_lossy()
    );
    if win::run_elevated_hidden("powershell.exe", &args) {
        Ok(())
    } else {
        Err("Разрешение не дано".into())
    }
}
#[tauri::command]
async fn app_icon(path: String) -> String {
    tauri::async_runtime::spawn_blocking(move || win::file_icon_png(&path).unwrap_or_default()).await.unwrap_or_default()
}
#[cfg(windows)]
fn spawn_hidden(exe: &str, args: &[&str]) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new(exe).args(args).creation_flags(0x0800_0000).spawn();
}
#[cfg(windows)]
#[tauri::command]
fn power(action: String) {
    match action.as_str() {
        "lock" => win::lock_pc(),
        "sleep" => spawn_hidden("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        "restart" => spawn_hidden("shutdown.exe", &["/r", "/t", "0"]),
        "shutdown" => spawn_hidden("shutdown.exe", &["/s", "/t", "0"]),
        _ => {}
    }
}
#[cfg(not(windows))]
#[tauri::command]
fn power(action: String) {
    match action.as_str() {
        "lock" => win::lock_pc(),
        "sleep" => win::spawn("systemctl", &["suspend"]),
        "restart" => win::spawn("systemctl", &["reboot"]),
        "shutdown" => win::spawn("systemctl", &["poweroff"]),
        _ => {}
    }
}

// ---------- ИИ: Химера ----------
#[derive(Serialize)]
struct AiStatus {
    installed: bool,
    installing: bool,
    loaded: bool,
    model_name: String,
    model_path: String,
    part_bytes: u64,
    ram_used: u64,
    ram_total: u64,
    dir: String,
}
#[tauri::command]
fn ai_status(app: AppHandle) -> AiStatus {
    let i = ai::installed(&app);
    let (ru, rt) = win::ram();
    AiStatus {
        installed: i.is_some(),
        installing: ai::RUNNING.load(std::sync::atomic::Ordering::SeqCst),
        loaded: ai::is_loaded(&app),
        model_name: i.as_ref().map(|x| x.model_name.clone()).unwrap_or_default(),
        model_path: i.as_ref().map(|x| x.model_path.clone()).unwrap_or_default(),
        part_bytes: ai::part_progress(&app).0,
        ram_used: ru,
        ram_total: rt,
        dir: ai::dir(&app).to_string_lossy().to_string(),
    }
}
/// Горячая клавиша «сон»: пустая строка — снять. Ошибка — сочетание не распознано или занято.
#[tauri::command]
fn hotkey_set(app: AppHandle, accel: String) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let a = accel.trim();
    if a.is_empty() {
        return Ok(());
    }
    let sc: Shortcut = a.parse().map_err(|e| format!("Не понял сочетание: {e}"))?;
    gs.register(sc).map_err(|_| "Это сочетание уже занято другой программой".to_string())
}
#[tauri::command]
fn ai_install(app: AppHandle, local: Option<String>, variant: Option<String>) {
    std::thread::spawn(move || ai::install(app, local, variant));
}
#[tauri::command]
fn ai_cancel() {
    ai::CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}
#[tauri::command]
async fn ai_delete(app: AppHandle) {
    ai::CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(600));
        ai::delete(&app)
    })
    .await;
}
#[tauri::command]
async fn ai_start(app: AppHandle) -> Result<u16, String> {
    tauri::async_runtime::spawn_blocking(move || ai::start(&app)).await.map_err(|e| e.to_string())?
}
#[tauri::command]
fn ai_stop(app: AppHandle) {
    ai::stop(&app);
}
#[tauri::command]
fn ai_touch(app: AppHandle) {
    *app.state::<ai::AiProc>().last_use.lock().unwrap() = Instant::now();
}

// ---------- уведомления Windows: Telegram и другие ----------
fn watch_toasts(app: AppHandle) {
    std::thread::spawn(move || {
        let Ok(local) = app.path().local_data_dir() else { return };
        let mut db: Option<win::ToastDb> = None;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let s = cur_settings(&app);
            if !(s.telegram || s.other_apps) {
                db = None;
                continue;
            }
            if db.is_none() {
                db = win::ToastDb::open(&local);
                if db.is_none() {
                    std::thread::sleep(Duration::from_secs(10));
                    continue;
                }
            }
            for t in db.as_mut().unwrap().poll() {
                let want = if t.app == "telegram" { s.telegram } else { s.other_apps };
                if want {
                    let _ = app.emit_to("island", "message", t);
                }
            }
        }
    });
}
#[tauri::command]
fn toasts_available(app: AppHandle) -> bool {
    #[cfg(windows)]
    return app.path().local_data_dir().ok().and_then(|l| win::ToastDb::open(&l)).is_some();
    #[cfg(not(windows))]
    {
        let _ = app;
        win::dbus_monitor_available()
    }
}

// ---------- загрузки ----------
#[derive(Clone, Serialize)]
struct Download {
    name: String,
    path: String,
}

fn is_final(p: &Path) -> bool {
    let name = match p.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_lowercase(),
        None => return false,
    };
    if name.starts_with('.') || name.starts_with("~$") || name == "desktop.ini" {
        return false;
    }
    let bad = [".crdownload", ".part", ".partial", ".tmp", ".download", ".opdownload", ".!ut", ".temp", ".!qb"];
    !bad.iter().any(|b| name.ends_with(b))
}
fn part_sibling(p: &Path) -> bool {
    let s = p.to_string_lossy();
    [".part", ".crdownload", ".partial"].iter().any(|e| Path::new(&format!("{}{}", s, e)).exists())
}

fn watch_downloads(app: AppHandle) {
    let Ok(dir) = app.path().download_dir() else { return };
    std::thread::spawn(move || {
        use notify::{event::ModifyKind, EventKind, RecursiveMode, Watcher};
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut w) = notify::recommended_watcher(tx) else { return };
        if w.watch(&dir, RecursiveMode::NonRecursive).is_err() {
            return;
        }
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let mut seen: HashMap<PathBuf, Instant> = HashMap::new();
        loop {
            if let Ok(Ok(ev)) = rx.recv_timeout(Duration::from_millis(400)) {
                match ev.kind {
                    EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_)) => {
                        for p in ev.paths {
                            if is_final(&p) {
                                pending.insert(p, Instant::now());
                            }
                        }
                    }
                    EventKind::Modify(_) => {
                        for p in ev.paths {
                            if let Some(t) = pending.get_mut(&p) {
                                *t = Instant::now();
                            }
                        }
                    }
                    _ => {}
                }
            }
            let now = Instant::now();
            let ready: Vec<PathBuf> = pending
                .iter()
                .filter(|(_, t)| now.duration_since(**t) > Duration::from_millis(1500))
                .map(|(p, _)| p.clone())
                .collect();
            for p in ready {
                pending.remove(&p);
                let ok = p.is_file()
                    && fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false)
                    && !part_sibling(&p);
                if !ok {
                    continue;
                }
                if seen.get(&p).map(|t| now.duration_since(*t) < Duration::from_secs(30)).unwrap_or(false) {
                    continue;
                }
                seen.insert(p.clone(), now);
                if !cur_settings(&app).notify_downloads {
                    continue;
                }
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let _ = app.emit_to(
                    "island",
                    "download",
                    Download { name, path: p.to_string_lossy().to_string() },
                );
            }
            seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        }
    });
}

// ---------- локальный API: уведомления от Claude Code и своих скриптов ----------
#[derive(Clone, Serialize)]
struct Notice {
    src: String,
    title: String,
    text: String,
}

fn spawn_api(app: AppHandle) {
    std::thread::spawn(move || {
        let Ok(server) = tiny_http::Server::http(("127.0.0.1", API_PORT)) else { return };
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().take(16_384).read_to_string(&mut body);
            let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
            let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
            let resp = match path {
                "/ping" => "legends-island".to_string(),
                "/notify" => {
                    let src = q.get("src").cloned().unwrap_or_default();
                    let event = q.get("event").cloned().unwrap_or_default();
                    let body_msg = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from));
                    let (dt, dx) = if src == "claude" {
                        match event.as_str() {
                            "stop" => ("Claude Code", "Закончил задачу".to_string()),
                            "notification" => ("Claude Code", body_msg.clone().unwrap_or("Ждёт твоего ответа".into())),
                            _ => ("Claude Code", body_msg.clone().unwrap_or_default()),
                        }
                    } else {
                        ("Уведомление", body_msg.clone().unwrap_or_else(|| body.trim().to_string()))
                    };
                    let title = q.get("title").cloned().unwrap_or(dt.to_string());
                    let text = q.get("text").cloned().unwrap_or(dx);
                    let _ = app.emit_to("island", "notify", Notice { src, title, text });
                    "ok".to_string()
                }
                _ => "not found".to_string(),
            };
            let _ = req.respond(tiny_http::Response::from_string(resp));
        }
    });
}

fn claude_settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().home_dir().ok().map(|h| h.join(".claude").join("settings.json"))
}
fn hook_cmd(event: &str) -> String {
    format!(
        "curl -s -m 2 -X POST \"http://127.0.0.1:{}/notify?src=claude&event={}\" --data-binary @- >/dev/null 2>&1 || true",
        API_PORT, event
    )
}

#[tauri::command]
fn claude_hook_status(app: AppHandle) -> bool {
    claude_settings_path(&app)
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| t.contains(&format!("127.0.0.1:{}", API_PORT)))
        .unwrap_or(false)
}

#[tauri::command]
fn install_claude_hook(app: AppHandle) -> Result<(), String> {
    let p = claude_settings_path(&app).ok_or("Не нашёл папку пользователя")?;
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let text = fs::read_to_string(&p).unwrap_or_else(|_| "{}".into());
    let mut v: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&text).map_err(|_| "settings.json Claude Code повреждён — не трогаю его".to_string())?
    };
    if p.exists() {
        let _ = fs::write(p.with_extension("json.bak-island"), &text);
    }
    let obj = v.as_object_mut().ok_or("settings.json должен быть объектом")?;
    let hooks = obj.entry("hooks").or_insert(serde_json::json!({}));
    let hooks = hooks.as_object_mut().ok_or("поле hooks имеет неожиданный формат")?;
    for (ev, key) in [("Stop", "stop"), ("Notification", "notification")] {
        let arr = hooks.entry(ev).or_insert(serde_json::json!([]));
        let arr = arr.as_array_mut().ok_or("hooks имеет неожиданный формат")?;
        let has = arr.iter().any(|x| x.to_string().contains(&format!("127.0.0.1:{}", API_PORT)));
        if !has {
            arr.push(serde_json::json!({ "hooks": [ { "type": "command", "command": hook_cmd(key) } ] }));
        }
    }
    fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- обновления ----------
#[derive(Clone, Serialize)]
struct UpdInfo {
    state: String,
    version: String,
}

async fn check_update(app: AppHandle, manual: bool) {
    let r = match app.updater() {
        Ok(u) => u.check().await,
        Err(e) => Err(e),
    };
    let info = match r {
        Ok(Some(u)) => UpdInfo { state: "available".into(), version: u.version.clone() },
        Ok(None) => UpdInfo { state: "none".into(), version: String::new() },
        Err(e) => UpdInfo { state: "error".into(), version: e.to_string() },
    };
    if manual || info.state == "available" {
        let _ = app.emit("update", info);
    }
}
#[tauri::command]
async fn check_updates(app: AppHandle) {
    check_update(app, true).await;
}
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let u = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    if let Some(u) = u {
        let _ = app.emit("update", UpdInfo { state: "installing".into(), version: u.version.clone() });
        u.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
        app.restart();
    }
    Ok(())
}

// ---------- окна ----------
fn open_settings_win(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Legends Island — настройки")
        .inner_size(480.0, 780.0)
        .resizable(false)
        .decorations(false)
        .center()
        .build();
}

fn place_island(w: &WebviewWindow) {
    if let Ok(Some(m)) = w.primary_monitor() {
        let s = m.scale_factor();
        let x = m.position().x + ((m.size().width as f64 - ISLAND_W * s) / 2.0) as i32;
        let _ = w.set_position(PhysicalPosition::new(x, m.position().y));
    }
}

// курсор опрашивается глобально — окно острова прозрачно для кликов, пока мышь не над ним
fn spawn_cursor(w: WebviewWindow) {
    std::thread::spawn(move || {
        let mut last = (f64::NAN, f64::NAN);
        let mut tick: u32 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(16));
            tick = tick.wrapping_add(1);
            if tick % 300 == 0 {
                place_island(&w); // разрешение экрана могло смениться
            }
            let Ok(c) = w.cursor_position() else { continue };
            let Ok(p) = w.outer_position() else { continue };
            let s = w.scale_factor().unwrap_or(1.0);
            let x = (c.x - p.x as f64) / s;
            let y = (c.y - p.y as f64) / s;
            if (x - last.0).abs() < 0.5 && (y - last.1).abs() < 0.5 {
                continue;
            }
            last = (x, y);
            let _ = w.emit_to("island", "cursor", (x, y));
        }
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_i = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let toggle_i = MenuItem::with_id(app, "toggle", "Скрыть / показать островок", true, None::<&str>)?;
    let upd_i = MenuItem::with_id(app, "update", "Проверить обновления", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_i, &toggle_i, &upd_i, &quit_i])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Legends Island")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "settings" => open_settings_win(app),
            "toggle" => {
                if let Some(w) = app.get_webview_window("island") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                    }
                }
            }
            "update" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move { check_update(h, true).await });
            }
            "quit" => {
                fps_flag(false);
                ai::stop(app);
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                open_settings_win(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| open_settings_win(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--autostart"])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _sc, ev| {
                    if ev.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        let _ = app.emit_to("island", "hotkey", "sleep");
                    }
                })
                .build(),
        )
        .setup(|app| {
            let h = app.handle().clone();
            let (s, first) = load_settings(&h);
            write_settings(&h, &s);
            apply_autostart(&h, s.autostart);
            fps_flag(s.game);
            let clips: Vec<ClipItem> = data_file(&h, "clips")
                .and_then(|p| fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            app.manage(AppState { settings: Mutex::new(s), clips: Mutex::new(clips) });
            app.manage(ai::AiProc { child: Mutex::new(None), last_use: Mutex::new(Instant::now()) });
            ai::resume_on_start(h.clone());
            ai::idle_watchdog(h.clone(), 10);
            if let Some(w) = app.get_webview_window("island") {
                let _ = w.set_size(tauri::LogicalSize::new(ISLAND_W, ISLAND_H));
                place_island(&w);
                let _ = w.set_ignore_cursor_events(true);
                spawn_cursor(w.clone());
            }
            build_tray(&h)?;
            watch_downloads(h.clone());
            watch_media(h.clone());
            watch_level(h.clone());
            watch_clipboard(h.clone());
            watch_toasts(h.clone());
            watch_stats(h.clone());
            spawn_api(h.clone());
            let h2 = h.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(15));
                loop {
                    tauri::async_runtime::block_on(check_update(h2.clone(), false));
                    std::thread::sleep(Duration::from_secs(30 * 60));
                }
            });
            if first && !std::env::args().any(|a| a == "--autostart") {
                open_settings_win(&h);
            }
            Ok(())
        })
        .on_window_event(|w, e| {
            if w.label() == "island" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings, save_settings, set_interactive, open_settings, close_window, quit, app_version,
            reveal_file, open_file, media_cmd, install_claude_hook, claude_hook_status, check_updates, install_update,
            get_volume, set_volume, ai_status, hotkey_set, tune::clean_cache, tune::ram_trim, tune::power_mode, tune::top_procs, tune::kill_proc, tune::flush_dns, ai_install, ai_cancel, ai_delete, ai_start, ai_stop, ai_touch, data_get, data_set, http_get, list_apps, open_target, clip_list, clip_copy, clip_remove,
            clip_clear, action, toasts_available, focus_window, platform, app_icon, fps_setup, fps_status, power
        ])
        .build(tauri::generate_context!())
        .expect("Legends Island не запустился")
        .run(|app, ev| {
            if let tauri::RunEvent::Exit = ev {
                ai::stop(app);
                fps_flag(false);
            }
        });
}
