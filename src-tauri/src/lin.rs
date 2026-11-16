// Linux-версия системных функций (тот же набор, что в win.rs): MPRIS, pactl, X11, /proc, D-Bus уведомления.
use base64::Engine;
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat};
use x11rb::rust_connection::RustConnection;

pub fn winrt_init() {}

fn have(cmd: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {} >/dev/null 2>&1", cmd)).status().map(|s| s.success()).unwrap_or(false)
}
fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).to_string())
}

// ---------- медиа (MPRIS) ----------
#[derive(Clone, Serialize, PartialEq, Default)]
pub struct MediaInfo {
    pub has: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub playing: bool,
    pub pos_ms: i64,
    pub dur_ms: i64,
    pub at_ms: i64,
    pub art: String,
}
pub struct Mgr {
    finder: mpris::PlayerFinder,
}
pub fn media_manager() -> Option<Mgr> {
    mpris::PlayerFinder::new().ok().map(|finder| Mgr { finder })
}
fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
fn art_data(url: &str) -> String {
    if let Some(p) = url.strip_prefix("file://") {
        let p = percent_decode(p);
        if let Ok(b) = std::fs::read(&p) {
            if b.len() < 6_000_000 {
                let mime = if b.starts_with(&[0x89, b'P']) { "image/png" } else { "image/jpeg" };
                return format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(b));
            }
        }
        return String::new();
    }
    if url.starts_with("http") {
        return url.to_string();
    }
    String::new()
}
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
pub fn media_read(m: &Mgr, prev_key: &mut String, prev_art: &mut String) -> MediaInfo {
    let Ok(p) = m.finder.find_active() else { return MediaInfo::default() };
    let mut info = MediaInfo { has: true, app: p.identity().to_string(), ..Default::default() };
    if let Ok(md) = p.get_metadata() {
        info.title = md.title().unwrap_or("").to_string();
        info.artist = md.artists().map(|a| a.join(", ")).unwrap_or_default();
        info.album = md.album_name().unwrap_or("").to_string();
        info.dur_ms = md.length().map(|d| d.as_millis() as i64).unwrap_or(0);
        let key = format!("{}|{}|{}", info.app, info.title, info.artist);
        if key != *prev_key {
            *prev_key = key;
            *prev_art = md.art_url().map(art_data).unwrap_or_default();
        }
        info.art = prev_art.clone();
    }
    info.playing = matches!(p.get_playback_status(), Ok(mpris::PlaybackStatus::Playing));
    let pos = p.get_position().map(|d| d.as_millis() as i64).unwrap_or(0);
    // играет: передаём «момент начала трека» (стабилен, пока трек не перемотали) — интерфейс сам досчитывает позицию
    if info.playing {
        info.pos_ms = 0;
        info.at_ms = (now_ms() - pos) / 2000 * 2000;
    } else {
        info.pos_ms = pos;
    }
    if info.title.is_empty() {
        info.has = false;
    }
    info
}
pub fn media_command(action: &str) {
    let Ok(f) = mpris::PlayerFinder::new() else { return };
    let Ok(p) = f.find_active() else { return };
    let _ = match action {
        "toggle" => p.play_pause(),
        "next" => p.next(),
        "prev" => p.previous(),
        _ => return,
    };
}

// ---------- громкость (PulseAudio / PipeWire) ----------
pub fn get_volume() -> f32 {
    let Some(s) = out("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]) else { return -1.0 };
    s.split('/')
        .find_map(|p| p.trim().strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()))
        .map(|v| v / 100.0)
        .unwrap_or(-1.0)
}
pub fn set_volume(x: f32) {
    let v = format!("{}%", (x.clamp(0.0, 1.0) * 100.0).round() as i32);
    let _ = Command::new("pactl").args(["set-sink-volume", "@DEFAULT_SINK@", &v]).status();
}

// ---------- буфер: «номер версии» = хеш текста ----------
pub fn clipboard_seq() -> u32 {
    use std::hash::{Hash, Hasher};
    let t = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish() as u32
}

// ---------- X11 ----------
struct X {
    c: RustConnection,
    root: u32,
    w: u16,
    h: u16,
    active: u32,
    state: u32,
    fullscreen: u32,
    pid: u32,
}
fn x() -> Option<&'static Mutex<X>> {
    static X11: OnceLock<Option<Mutex<X>>> = OnceLock::new();
    X11.get_or_init(|| {
        let (c, n) = x11rb::connect(None).ok()?;
        let scr = &c.setup().roots[n];
        let (root, w, h) = (scr.root, scr.width_in_pixels, scr.height_in_pixels);
        let atom = |name: &[u8]| c.intern_atom(false, name).ok()?.reply().ok().map(|r| r.atom);
        let active = atom(b"_NET_ACTIVE_WINDOW")?;
        let state = atom(b"_NET_WM_STATE")?;
        let fullscreen = atom(b"_NET_WM_STATE_FULLSCREEN")?;
        let pid = atom(b"_NET_WM_PID")?;
        Some(Mutex::new(X { c, root, w, h, active, state, fullscreen, pid }))
    })
    .as_ref()
}
fn active_window(x: &X) -> Option<u32> {
    x.c.get_property(false, x.root, x.active, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok()?.value32()?.next().filter(|w| *w != 0)
}

pub struct Shot {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}
pub fn capture_screen() -> Option<Shot> {
    let g = x()?.lock().ok()?;
    let r = g.c.get_image(ImageFormat::Z_PIXMAP, g.root, 0, 0, g.w, g.h, !0).ok()?.reply().ok()?;
    let (w, h) = (g.w as usize, g.h as usize);
    if r.data.len() < w * h * 4 {
        return None;
    }
    let mut rgba = r.data;
    rgba.truncate(w * h * 4);
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
    Some(Shot { w, h, rgba })
}
pub fn save_png(path: &Path, s: &Shot) -> bool {
    let Ok(f) = std::fs::File::create(path) else { return false };
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), s.w as u32, s.h as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let Ok(mut wr) = e.write_header() else { return false };
    wr.write_image_data(&s.rgba).is_ok()
}
pub fn screenshots_dir(pictures: Option<PathBuf>) -> PathBuf {
    let d = pictures.unwrap_or_else(std::env::temp_dir).join("Screenshots");
    let _ = std::fs::create_dir_all(&d);
    d
}
fn load_png(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    let mut rd = dec.read_info().ok()?;
    let mut buf = vec![0; rd.output_buffer_size()];
    let info = rd.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        _ => return None,
    };
    Some((w, h, rgba))
}
/// Выделение области системным инструментом: возвращает картинку или None (отмена / нет инструмента)
pub fn snip_region() -> Result<(usize, usize, Vec<u8>, PathBuf), String> {
    let p = std::env::temp_dir().join(format!("legends-snip-{}.png", now_ms()));
    let ps = p.to_string_lossy().to_string();
    let tools: [(&str, Vec<&str>); 5] = [
        ("gnome-screenshot", vec!["-a", "-f", &ps]),
        ("xfce4-screenshooter", vec!["-r", "-s", &ps]),
        ("spectacle", vec!["-r", "-b", "-n", "-o", &ps]),
        ("maim", vec!["-s", &ps]),
        ("scrot", vec!["-s", &ps]),
    ];
    for (t, args) in tools.iter() {
        if have(t) {
            let _ = Command::new(t).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status();
            return match load_png(&p) {
                Some((w, h, rgba)) => Ok((w, h, rgba, p)),
                None => Err("cancel".into()),
            };
        }
    }
    Err("Не нашёл программу для скриншотов: установи gnome-screenshot (sudo apt install gnome-screenshot)".into())
}
pub fn region_to_clipboard() {
    if let Ok((w, h, rgba, p)) = snip_region() {
        if let Ok(mut c) = arboard::Clipboard::new() {
            let _ = c.set_image(arboard::ImageData { width: w, height: h, bytes: std::borrow::Cow::Owned(rgba) });
        }
        let _ = std::fs::remove_file(p);
    }
}

// ---------- OCR (tesseract) ----------
pub fn ocr_file(p: &Path) -> Result<String, String> {
    if !have("tesseract") {
        return Err("Нужен tesseract: sudo apt install tesseract-ocr tesseract-ocr-rus".into());
    }
    let langs = out("tesseract", &["--list-langs"]).unwrap_or_default();
    let lang = if langs.lines().any(|l| l.trim() == "rus") { "rus+eng" } else { "eng" };
    let o = Command::new("tesseract").args([&p.to_string_lossy().to_string(), "-", "-l", lang]).stderr(Stdio::null()).output().map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

// ---------- QR ----------
pub fn qr_rgba(w: usize, h: usize, rgba: &[u8]) -> Option<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        let i = (y * w + x) * 4;
        ((rgba[i] as u32 * 30 + rgba[i + 1] as u32 * 59 + rgba[i + 2] as u32 * 11) / 100) as u8
    });
    for g in img.detect_grids() {
        if let Ok((_, content)) = g.decode() {
            return Some(content);
        }
    }
    None
}

// ---------- пипетка ----------
pub fn cursor_pixel() -> Option<(i32, i32, String)> {
    let g = x()?.lock().ok()?;
    let p = g.c.query_pointer(g.root).ok()?.reply().ok()?;
    let r = g.c.get_image(ImageFormat::Z_PIXMAP, g.root, p.root_x, p.root_y, 1, 1, !0).ok()?.reply().ok()?;
    if r.data.len() < 3 {
        return None;
    }
    Some((p.root_x as i32, p.root_y as i32, format!("#{:02X}{:02X}{:02X}", r.data[2], r.data[1], r.data[0])))
}
/// vk: 0x11 — Ctrl, 0x1B — Esc (коды как в Windows, чтобы общий код не менялся)
pub fn key_down(vk: u16) -> bool {
    let Some(m) = x() else { return false };
    let Ok(g) = m.lock() else { return false };
    let Some(km) = g.c.query_keymap().ok().and_then(|r| r.reply().ok()) else { return false };
    let codes: &[u8] = match vk {
        0x11 => &[37, 105],
        0x1B => &[9],
        _ => &[],
    };
    codes.iter().any(|&kc| km.keys[(kc / 8) as usize] & (1 << (kc % 8)) != 0)
}
pub fn press_combo(_keys: &[u16]) {}

// ---------- уведомления (D-Bus: org.freedesktop.Notifications.Notify) ----------
#[derive(Clone, Serialize)]
pub struct Toast {
    pub app: String,
    pub app_id: String,
    pub title: String,
    pub text: String,
}
pub fn app_kind(id: &str) -> String {
    let l = id.to_lowercase();
    if l.contains("telegram") {
        "telegram".into()
    } else if l.contains("discord") {
        "discord".into()
    } else if l.contains("whatsapp") {
        "whatsapp".into()
    } else {
        "other".into()
    }
}
pub struct ToastDb {
    rx: mpsc::Receiver<Toast>,
}
fn unquote(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in s.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
}
impl ToastDb {
    pub fn open(_local: &Path) -> Option<Self> {
        let mut child = Command::new("dbus-monitor")
            .args(["--session", "interface='org.freedesktop.Notifications',member='Notify'"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let so = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut strings: Vec<String> = Vec::new();
            let mut cur: Option<String> = None;
            let mut in_call = false;
            for line in BufReader::new(so).lines().map_while(Result::ok) {
                if let Some(acc) = cur.as_mut() {
                    acc.push('\n');
                    acc.push_str(&line);
                    if line.ends_with('"') && !line.ends_with("\\\"") {
                        let s = acc.trim_end_matches('"').to_string();
                        strings.push(unquote(&s));
                        cur = None;
                    }
                    continue;
                }
                if line.starts_with("method call") {
                    in_call = line.contains("member=Notify");
                    strings.clear();
                    continue;
                }
                if !in_call {
                    continue;
                }
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("string \"") {
                    if rest.ends_with('"') && !rest.ends_with("\\\"") || rest == "\"" {
                        strings.push(unquote(rest.strip_suffix('"').unwrap_or(rest)));
                    } else {
                        cur = Some(rest.to_string());
                    }
                    // app_name, app_icon, summary, body
                    if strings.len() == 4 {
                        let app_id = strings[0].clone();
                        let title = strip_tags(&strings[2]);
                        let text = strip_tags(&strings[3]).replace('\n', " · ");
                        in_call = false;
                        if !title.is_empty() || !text.is_empty() {
                            let _ = tx.send(Toast { app: app_kind(&app_id), app_id, title, text });
                        }
                    }
                }
            }
            let _ = child.wait();
        });
        Some(ToastDb { rx })
    }
    pub fn poll(&mut self) -> Vec<Toast> {
        self.rx.try_iter().collect()
    }
}

// ---------- нагрузка ----------
pub struct CpuMeter {
    last: (u64, u64),
}
impl CpuMeter {
    pub fn new() -> Self {
        CpuMeter { last: Self::sample() }
    }
    fn sample() -> (u64, u64) {
        let s = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let v: Vec<u64> = s.lines().next().unwrap_or("").split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
        let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
        (idle, v.iter().sum())
    }
    pub fn percent(&mut self) -> f64 {
        let now = Self::sample();
        let (di, dt) = (now.0.saturating_sub(self.last.0), now.1.saturating_sub(self.last.1));
        self.last = now;
        if dt == 0 {
            return 0.0;
        }
        ((dt - di.min(dt)) as f64 / dt as f64 * 100.0).clamp(0.0, 100.0)
    }
}
pub fn ram() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |k: &str| {
        s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
    };
    let (t, a) = (get("MemTotal:"), get("MemAvailable:"));
    (t.saturating_sub(a), t)
}
pub struct GpuMeter {
    nvidia: bool,
    amd: Option<PathBuf>,
}
impl GpuMeter {
    pub fn new() -> Self {
        let amd = std::fs::read_dir("/sys/class/drm").ok().and_then(|rd| {
            rd.flatten().map(|e| e.path().join("device").join("gpu_busy_percent")).find(|p| p.exists())
        });
        GpuMeter { nvidia: have("nvidia-smi"), amd }
    }
    pub fn percent(&mut self) -> Option<f64> {
        if self.nvidia {
            return out("nvidia-smi", &["--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"])
                .and_then(|s| s.lines().next().and_then(|l| l.trim().parse().ok()));
        }
        self.amd.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok())
    }
}
pub fn gpu_name() -> String {
    if have("nvidia-smi") {
        if let Some(s) = out("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"]) {
            return s.lines().next().unwrap_or("").trim().to_string();
        }
    }
    out("sh", &["-c", "lspci 2>/dev/null | grep -iE 'vga|3d' | head -1 | sed 's/.*: //'"]).map(|s| s.trim().to_string()).unwrap_or_default()
}

// ---------- активное окно, полный экран ----------
pub fn foreground() -> (u32, String) {
    let Some(m) = x() else { return (0, String::new()) };
    let Ok(g) = m.lock() else { return (0, String::new()) };
    let Some(w) = active_window(&g) else { return (0, String::new()) };
    let pid = g
        .c
        .get_property(false, w, g.pid, AtomEnum::CARDINAL, 0, 1)
        .ok()
        .and_then(|r| r.reply().ok())
        .and_then(|r| r.value32().and_then(|mut v| v.next()))
        .unwrap_or(0);
    let name = std::fs::read_to_string(format!("/proc/{}/comm", pid)).map(|s| s.trim().to_string()).unwrap_or_default();
    (pid, name)
}
pub fn fullscreen_busy() -> bool {
    let Some(m) = x() else { return false };
    let Ok(g) = m.lock() else { return false };
    let Some(w) = active_window(&g) else { return false };
    g.c.get_property(false, w, g.state, AtomEnum::ATOM, 0, 32)
        .ok()
        .and_then(|r| r.reply().ok())
        .and_then(|r| r.value32().map(|mut v| v.any(|a| a == g.fullscreen)))
        .unwrap_or(false)
}

// ---------- приложения (.desktop) и их иконки ----------
pub fn desktop_field(text: &str, key: &str) -> Option<String> {
    let mut in_main = false;
    let mut val: Option<String> = None;
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_main = l == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some(v) = l.strip_prefix(&format!("{}=", key)) {
            val = Some(v.to_string());
        }
    }
    val
}
fn icon_file(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Some(PathBuf::from(name)).filter(|p| p.exists());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let bases = [
        format!("{}/.local/share/icons", home),
        "/usr/share/icons/hicolor".into(),
        "/usr/share/icons/Mint-Y".into(),
        "/usr/share/icons/Papirus".into(),
        "/usr/share/icons/Adwaita".into(),
        "/var/lib/flatpak/exports/share/icons/hicolor".into(),
        format!("{}/.local/share/flatpak/exports/share/icons/hicolor", home),
    ];
    let sizes = ["48x48", "64x64", "scalable", "128x128", "256x256", "32x32", "48", "64"];
    for b in bases.iter() {
        for s in sizes {
            for ext in ["png", "svg"] {
                let p = PathBuf::from(format!("{}/{}/apps/{}.{}", b, s, name, ext));
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    for ext in ["png", "svg"] {
        let p = PathBuf::from(format!("/usr/share/pixmaps/{}.{}", name, ext));
        if p.exists() {
            return Some(p);
        }
    }
    None
}
pub fn file_icon_png(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let icon = desktop_field(&text, "Icon")?;
    let f = icon_file(&icon)?;
    let b = std::fs::read(&f).ok()?;
    if b.len() > 800_000 {
        return None;
    }
    let mime = if f.extension().map(|e| e == "svg").unwrap_or(false) { "image/svg+xml" } else { "image/png" };
    Some(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(b)))
}

pub fn run_elevated_hidden(_exe: &str, _args: &str) -> bool {
    false
}
pub fn lock_pc() {
    for (c, a) in [("cinnamon-screensaver-command", vec!["-l"]), ("xdg-screensaver", vec!["lock"]), ("loginctl", vec!["lock-session"])] {
        if have(c) && Command::new(c).args(&a).status().map(|s| s.success()).unwrap_or(false) {
            return;
        }
    }
}
pub fn spawn(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}
pub fn dbus_monitor_available() -> bool {
    have("dbus-monitor")
}

// ---------- уровень звука: на Linux пока нет ----------
pub struct Peak;
pub fn peak_meter() -> Option<Peak> {
    None
}
impl Peak {
    pub fn level(&self) -> f32 {
        0.0
    }
}
