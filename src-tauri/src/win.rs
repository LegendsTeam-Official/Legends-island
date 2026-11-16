// Всё, что работает напрямую с Windows: медиа, громкость, скриншоты, OCR, QR, пипетка, буфер, уведомления.
use base64::Engine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Mgr,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Props,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as St,
};

pub fn winrt_init() {
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
    }
}

// ---------- медиа ----------
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

pub fn media_manager() -> Option<Mgr> {
    Mgr::RequestAsync().ok()?.get().ok()
}

fn ticks_to_unix_ms(t: i64) -> i64 {
    (t - 116_444_736_000_000_000) / 10_000
}

fn read_thumb(p: &Props) -> Option<String> {
    use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
    let r = p.Thumbnail().ok()?;
    let s = r.OpenReadAsync().ok()?.get().ok()?;
    let size = s.Size().ok()? as u32;
    if size == 0 || size > 6_000_000 {
        return None;
    }
    let buf = Buffer::Create(size).ok()?;
    let ib = s.ReadAsync(&buf, size, InputStreamOptions::None).ok()?.get().ok()?;
    let n = ib.Length().ok()? as usize;
    let rd = DataReader::FromBuffer(&ib).ok()?;
    let mut v = vec![0u8; n];
    rd.ReadBytes(&mut v).ok()?;
    let mime = if v.starts_with(&[0x89, b'P']) { "image/png" } else { "image/jpeg" };
    Some(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(v)))
}

/// prev_key/prev_art — чтобы не перечитывать обложку, пока трек тот же
pub fn media_read(m: &Mgr, prev_key: &mut String, prev_art: &mut String) -> MediaInfo {
    let Ok(s) = m.GetCurrentSession() else { return MediaInfo::default() };
    let mut info = MediaInfo { has: true, ..Default::default() };
    info.app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
    if let Ok(p) = s.TryGetMediaPropertiesAsync().and_then(|a| a.get()) {
        info.title = p.Title().map(|h| h.to_string()).unwrap_or_default();
        info.artist = p.Artist().map(|h| h.to_string()).unwrap_or_default();
        info.album = p.AlbumTitle().map(|h| h.to_string()).unwrap_or_default();
        let key = format!("{}|{}|{}", info.app, info.title, info.artist);
        if key != *prev_key {
            *prev_key = key;
            *prev_art = read_thumb(&p).unwrap_or_default();
        }
        info.art = prev_art.clone();
    }
    if let Ok(pi) = s.GetPlaybackInfo() {
        info.playing = pi.PlaybackStatus().map(|x| x == St::Playing).unwrap_or(false);
    }
    if let Ok(t) = s.GetTimelineProperties() {
        info.pos_ms = t.Position().map(|d| d.Duration / 10_000).unwrap_or(0);
        info.dur_ms = t.EndTime().map(|d| d.Duration / 10_000).unwrap_or(0);
        info.at_ms = t.LastUpdatedTime().map(|d| ticks_to_unix_ms(d.UniversalTime)).unwrap_or(0);
    }
    if info.title.is_empty() {
        info.has = false;
    }
    info
}

pub fn media_command(action: &str) {
    winrt_init();
    let Some(m) = media_manager() else { return };
    let Ok(s) = m.GetCurrentSession() else { return };
    let r = match action {
        "toggle" => s.TryTogglePlayPauseAsync(),
        "next" => s.TrySkipNextAsync(),
        "prev" => s.TrySkipPreviousAsync(),
        _ => return,
    };
    if let Ok(op) = r {
        let _ = op.get();
    }
}

// ---------- громкость ----------
fn endpoint_volume() -> windows::core::Result<windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume> {
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        dev.Activate(CLSCTX_ALL, None)
    }
}
pub fn get_volume() -> f32 {
    endpoint_volume().and_then(|v| unsafe { v.GetMasterVolumeLevelScalar() }).unwrap_or(-1.0)
}
pub fn set_volume(x: f32) {
    if let Ok(v) = endpoint_volume() {
        unsafe {
            let _ = v.SetMasterVolumeLevelScalar(x.clamp(0.0, 1.0), std::ptr::null());
        }
    }
}

// ---------- буфер обмена ----------
pub fn clipboard_seq() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

// ---------- скриншот всего экрана ----------
pub struct Shot {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

pub fn capture_screen() -> Option<Shot> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            return None;
        }
        let hdc = GetDC(HWND::default());
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(mem, 0, 0, w, h, hdc, x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), hdc);
        if !ok || lines == 0 {
            return None;
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Some(Shot { w: w as usize, h: h as usize, rgba: buf })
    }
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

// ---------- OCR ----------
pub fn ocr_rgba(w: usize, h: usize, rgba: &[u8]) -> Result<String, String> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;
    winrt_init();
    let mut bgra = rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let e = |x: windows::core::Error| x.message().to_string();
    let buf = CryptographicBuffer::CreateFromByteArray(&bgra).map_err(e)?;
    let sb = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32, BitmapAlphaMode::Premultiplied).map_err(e)?;
    let eng = OcrEngine::TryCreateFromUserProfileLanguages().map_err(e)?;
    let res = eng.RecognizeAsync(&sb).map_err(e)?.get().map_err(e)?;
    let mut out = Vec::new();
    if let Ok(lines) = res.Lines() {
        for l in lines {
            if let Ok(t) = l.Text() {
                out.push(t.to_string());
            }
        }
    }
    Ok(out.join("\n"))
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
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok()?;
        let hdc = GetDC(HWND::default());
        let c = GetPixel(hdc, p.x, p.y).0;
        ReleaseDC(HWND::default(), hdc);
        if c == 0xFFFF_FFFF {
            return None;
        }
        let (r, g, b) = (c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF);
        Some((p.x, p.y, format!("#{:02X}{:02X}{:02X}", r, g, b)))
    }
}
pub fn key_down(vk: u16) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) < 0 }
}

// ---------- клавиши ----------
pub fn press_combo(keys: &[u16]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
    unsafe {
        for k in keys {
            keybd_event(*k as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        }
        for k in keys.iter().rev() {
            keybd_event(*k as u8, 0, KEYEVENTF_KEYUP, 0);
        }
    }
}

// ---------- уведомления Windows (Telegram и др.) ----------
#[derive(Clone, Serialize)]
pub struct Toast {
    pub app: String,
    pub app_id: String,
    pub title: String,
    pub text: String,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&")
}

pub fn parse_toast(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<text") {
        rest = &rest[i + 5..];
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') {
            rest = &rest[gt + 1..];
            continue;
        }
        let body = &rest[gt + 1..];
        let Some(end) = body.find("</text>") else { break };
        let t = unescape(body[..end].trim());
        if !t.is_empty() {
            out.push(t);
        }
        rest = &body[end + 7..];
    }
    out
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
    conn: rusqlite::Connection,
    last: i64,
}
impl ToastDb {
    pub fn open(local: &Path) -> Option<Self> {
        let p = local.join("Microsoft").join("Windows").join("Notifications").join("wpndatabase.db");
        let conn = rusqlite::Connection::open_with_flags(
            &p,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let last: i64 = conn.query_row("SELECT IFNULL(MAX(Id),0) FROM Notification", [], |r| r.get(0)).ok()?;
        Some(ToastDb { conn, last })
    }
    pub fn poll(&mut self) -> Vec<Toast> {
        let mut out = Vec::new();
        let Ok(mut st) = self.conn.prepare(
            "SELECT n.Id, h.PrimaryId, n.Payload FROM Notification n JOIN NotificationHandler h ON n.HandlerId = h.RecordId WHERE n.Id > ?1 AND n.Type = 'toast' ORDER BY n.Id",
        ) else {
            return out;
        };
        let rows = st.query_map([self.last], |r| {
            let id: i64 = r.get(0)?;
            let app: String = r.get(1)?;
            let payload: Vec<u8> = r.get::<_, Vec<u8>>(2).or_else(|_| r.get::<_, String>(2).map(|s| s.into_bytes()))?;
            Ok((id, app, payload))
        });
        if let Ok(rows) = rows {
            for (id, app_id, payload) in rows.flatten() {
                self.last = self.last.max(id);
                let xml = String::from_utf8_lossy(&payload);
                let texts = parse_toast(&xml);
                if texts.is_empty() {
                    continue;
                }
                let title = texts[0].clone();
                let text = texts[1..].join(" · ");
                out.push(Toast { app: app_kind(&app_id), app_id, title, text });
            }
        }
        out
    }
}

// ---------- нагрузка: CPU, RAM, GPU ----------
fn ft(f: &windows::Win32::Foundation::FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}
pub struct CpuMeter {
    last: (u64, u64),
}
impl CpuMeter {
    pub fn new() -> Self {
        CpuMeter { last: Self::sample() }
    }
    fn sample() -> (u64, u64) {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::GetSystemTimes;
        let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe {
            let _ = GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u));
        }
        (ft(&i), ft(&k) + ft(&u))
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
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        if GlobalMemoryStatusEx(&mut m).is_err() {
            return (0, 0);
        }
    }
    (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys)
}

pub struct GpuMeter {
    q: isize,
    c: isize,
    ok: bool,
}
unsafe impl Send for GpuMeter {}
impl GpuMeter {
    pub fn new() -> Self {
        use windows::Win32::System::Performance::*;
        unsafe {
            let mut q: isize = 0;
            let mut c: isize = 0;
            let mut ok = PdhOpenQueryW(windows::core::PCWSTR::null(), 0, &mut q) == 0;
            if ok {
                ok = PdhAddEnglishCounterW(q, windows::core::w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage"), 0, &mut c) == 0;
            }
            if ok {
                let _ = PdhCollectQueryData(q);
            }
            GpuMeter { q, c, ok }
        }
    }
    pub fn percent(&mut self) -> Option<f64> {
        use windows::Win32::System::Performance::*;
        if !self.ok {
            return None;
        }
        unsafe {
            if PdhCollectQueryData(self.q) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let _ = PdhGetFormattedCounterArrayW(self.c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return Some(0.0);
            }
            let mut buf = vec![0u8; size as usize];
            let r = PdhGetFormattedCounterArrayW(self.c, PDH_FMT_DOUBLE, &mut size, &mut count, Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W));
            if r != 0 {
                return None;
            }
            let items = std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize);
            let sum: f64 = items.iter().map(|it| it.FmtValue.Anonymous.doubleValue).sum();
            Some(sum.clamp(0.0, 100.0))
        }
    }
}

pub fn gpu_name() -> String {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    for i in 0..6 {
        let key = windows::core::HSTRING::from(format!(
            "SYSTEM\\CurrentControlSet\\Control\\Class\\{{4d36e968-e325-11ce-bfc1-08002be10318}}\\{:04}",
            i
        ));
        let mut buf = vec![0u16; 256];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, &key, windows::core::w!("DriverDesc"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
        };
        if r.is_ok() {
            let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
            let l = s.to_lowercase();
            if !s.is_empty() && !l.contains("basic") && !l.contains("parsec") && !l.contains("virtual") {
                return s;
            }
        }
    }
    String::new()
}

// ---------- активное окно и полноэкранный режим ----------
pub fn foreground() -> (u32, String) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let h = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 {
            return (0, String::new());
        }
        let mut name = String::new();
        if let Ok(ph) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(ph, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                let full = String::from_utf16_lossy(&buf[..len as usize]);
                name = full.rsplit('\\').next().unwrap_or("").to_string();
            }
            let _ = CloseHandle(ph);
        }
        (pid, name)
    }
}
pub fn fullscreen_busy() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
    unsafe {
        match SHQueryUserNotificationState() {
            Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
            Err(_) => false,
        }
    }
}

// ---------- иконка приложения (48px PNG) ----------
pub fn file_icon_png(path: &str) -> Option<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_EXTRALARGE};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let w = windows::core::HSTRING::from(path);
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(&w, FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX);
        if r == 0 {
            return None;
        }
        let il: IImageList = SHGetImageList(SHIL_EXTRALARGE as i32).ok()?;
        let hicon = il.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0 as u32).ok()?;
        let mut ii = ICONINFO::default();
        if GetIconInfo(hicon, &mut ii).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }
        let mut bm = BITMAP::default();
        GetObjectW(ii.hbmColor, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (w, h) = (bm.bmWidth.max(1), bm.bmHeight.max(1));
        let dc = CreateCompatibleDC(None);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut px = vec![0u8; (w * h * 4) as usize];
        let n = GetDIBits(dc, ii.hbmColor, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(ii.hbmColor);
        let _ = DeleteObject(ii.hbmMask);
        let _ = DestroyIcon(hicon);
        let _ = HWND::default();
        if n == 0 {
            return None;
        }
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            }
        }
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w as u32, h as u32);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut wr = e.write_header().ok()?;
            wr.write_image_data(&px).ok()?;
        }
        Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(out)))
    }
}

// ---------- запуск с правами администратора (одно окно UAC, без консоли) ----------
pub fn run_elevated_hidden(exe: &str, args: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let e = windows::core::HSTRING::from(exe);
    let a = windows::core::HSTRING::from(args);
    let r = unsafe { ShellExecuteW(None, windows::core::w!("runas"), &e, &a, None, SW_HIDE) };
    r.0 as isize > 32
}
pub fn lock_pc() {
    unsafe {
        let _ = windows::Win32::System::Shutdown::LockWorkStation();
    }
}

// ---------- уровень звука (для «пульса под музыку») ----------
pub struct Peak {
    m: windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation,
}
pub fn peak_meter() -> Option<Peak> {
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let m = dev.Activate(CLSCTX_ALL, None).ok()?;
        Some(Peak { m })
    }
}
impl Peak {
    pub fn level(&self) -> f32 {
        unsafe { self.m.GetPeakValue().unwrap_or(0.0) }
    }
}
