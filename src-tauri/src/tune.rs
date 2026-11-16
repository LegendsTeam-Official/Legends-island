//! Быстрые действия с главной: очистка кэша и оптимизация ПК.
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Serialize)]
pub struct CleanRes {
    pub freed: u64,
    pub files: u64,
}
#[derive(Serialize)]
pub struct ProcInfo {
    pub name: String,
    pub mem: u64,
    pub count: u32,
}

fn hidden(cmd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut c = std::process::Command::new(cmd);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.output()
}

/// Удаляет содержимое папки (саму папку оставляет). Ссылки и точки соединения не проходит.
fn wipe(dir: &Path, min_age: Duration, freed: &mut u64, files: &mut u64, depth: u32) {
    if depth > 14 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_symlink() {
            continue;
        }
        if md.is_dir() {
            wipe(&p, min_age, freed, files, depth + 1);
            let _ = std::fs::remove_dir(&p);
        } else {
            let old = md
                .modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .map(|a| a >= min_age)
                .unwrap_or(true);
            if old {
                let sz = md.len();
                if std::fs::remove_file(&p).is_ok() {
                    *freed += sz;
                    *files += 1;
                }
            }
        }
    }
}

fn profiles(base: PathBuf, subs: &[&str], out: &mut Vec<(PathBuf, Duration)>) {
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n == "Default" || n.starts_with("Profile ") || n == "Guest Profile" {
                for s in subs {
                    out.push((e.path().join(s), Duration::ZERO));
                }
            }
        }
    }
}

fn targets() -> Vec<(PathBuf, Duration)> {
    let hour = Duration::from_secs(3600);
    let mut v = vec![(std::env::temp_dir(), hour)];
    let br = ["Cache", "Code Cache", "GPUCache"];
    #[cfg(windows)]
    {
        let env = |k: &str| std::env::var(k).map(PathBuf::from).ok();
        v.push((PathBuf::from(r"C:\Windows\Temp"), hour));
        if let Some(l) = env("LOCALAPPDATA") {
            profiles(l.join(r"Google\Chrome\User Data"), &br, &mut v);
            profiles(l.join(r"Microsoft\Edge\User Data"), &br, &mut v);
            profiles(l.join(r"Yandex\YandexBrowser\User Data"), &br, &mut v);
        }
        if let Some(r) = env("APPDATA") {
            for s in br {
                v.push((r.join("discord").join(s), Duration::ZERO));
            }
            v.push((r.join(r"Telegram Desktop\tdata\user_data\cache"), Duration::ZERO));
            v.push((r.join(r"Telegram Desktop\tdata\user_data\media_cache"), Duration::ZERO));
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(h) = std::env::var("HOME").map(PathBuf::from) {
            v.push((h.join(".cache/thumbnails"), Duration::ZERO));
            for b in ["google-chrome", "chromium", "yandex-browser", "microsoft-edge"] {
                profiles(h.join(".cache").join(b), &["Cache", "Code Cache", "GPUCache"], &mut v);
            }
            for s in br {
                v.push((h.join(".config/discord").join(s), Duration::ZERO));
            }
            v.push((h.join(".local/share/TelegramDesktop/tdata/user_data/cache"), Duration::ZERO));
            v.push((h.join(".local/share/TelegramDesktop/tdata/user_data/media_cache"), Duration::ZERO));
        }
    }
    v
}

#[tauri::command]
pub async fn clean_cache() -> CleanRes {
    tauri::async_runtime::spawn_blocking(|| {
        let (mut freed, mut files) = (0u64, 0u64);
        for (d, age) in targets() {
            wipe(&d, age, &mut freed, &mut files, 0);
        }
        CleanRes { freed, files }
    })
    .await
    .unwrap_or(CleanRes { freed: 0, files: 0 })
}

/// Сжимает рабочие наборы фоновых процессов; возвращает, сколько памяти освободилось.
#[tauri::command]
pub async fn ram_trim() -> Result<u64, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(|| {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
            use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA};
            let (u0, _) = crate::win::ram();
            let mut sys = sysinfo::System::new();
            sys.refresh_processes();
            for pid in sys.processes().keys() {
                unsafe {
                    if let Ok(h) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, false, pid.as_u32()) {
                        let _ = EmptyWorkingSet(h);
                        let _ = CloseHandle(h);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(600));
            let (u1, _) = crate::win::ram();
            Ok(u0.saturating_sub(u1))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        Err("На Linux система сама отдаёт память — тут это не нужно".into())
    }
}

const HIGH: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
const BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";

/// Схема питания: None — только узнать. Возвращает, включена ли «Высокая производительность».
#[tauri::command]
pub async fn power_mode(high: Option<bool>) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        {
            if let Some(h) = high {
                let id = if h { HIGH } else { BALANCED };
                let ok = hidden("powercfg", &["/setactive", id]).map(|o| o.status.success()).unwrap_or(false);
                if !ok && h {
                    // схема скрыта (часто на ноутбуках) — восстановим её копию и включим
                    let out = hidden("powercfg", &["-duplicatescheme", HIGH]).map_err(|e| e.to_string())?;
                    let txt = String::from_utf8_lossy(&out.stdout).to_string();
                    let guid = txt
                        .split_whitespace()
                        .find(|w| w.len() == 36 && w.matches('-').count() == 4)
                        .ok_or("Windows не дала включить этот режим")?
                        .to_string();
                    hidden("powercfg", &["/setactive", &guid]).map_err(|e| e.to_string())?;
                    let _ = hidden("powercfg", &["/changename", &guid, "Legends: Высокая производительность"]);
                }
            }
            let out = hidden("powercfg", &["/getactivescheme"]).map_err(|e| e.to_string())?;
            let t = String::from_utf8_lossy(&out.stdout).to_lowercase();
            Ok(t.contains(HIGH) || t.contains("legends:") || t.contains("высокая") || t.contains("high performance") || t.contains("ultimate"))
        }
        #[cfg(not(windows))]
        {
            if let Some(h) = high {
                hidden("powerprofilesctl", &["set", if h { "performance" } else { "balanced" }])
                    .map_err(|_| "Нет power-profiles-daemon".to_string())?;
            }
            let out = hidden("powerprofilesctl", &["get"]).map_err(|_| "Нет power-profiles-daemon".to_string())?;
            Ok(String::from_utf8_lossy(&out.stdout).trim() == "performance")
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

const SKIP: &[&str] = &[
    "system", "registry", "idle", "smss", "csrss", "wininit", "winlogon", "services", "lsass", "svchost", "dwm",
    "explorer", "memory compression", "fontdrvhost", "sihost", "ctfmon", "searchhost", "startmenuexperiencehost",
    "shellexperiencehost", "runtimebroker", "msmpeng", "textinputhost", "audiodg", "spoolsv", "wudfhost",
    "securityhealthservice", "llama-server", "legends island", "legends-island", "systemd", "xorg", "xwayland",
    "cinnamon", "gnome-shell", "plasmashell", "kwin_x11", "kwin_wayland", "xfwm4", "pipewire", "pulseaudio",
    "wireplumber", "dbus-daemon", "init", "kthreadd", "lightdm", "gdm", "sddm",
];
fn clean_name(n: &str) -> String {
    n.strip_suffix(".exe").unwrap_or(n).to_string()
}
fn skipped(n: &str) -> bool {
    let l = clean_name(&n.to_lowercase());
    SKIP.contains(&l.as_str())
}

/// Топ-5 программ по памяти (процессы одного имени складываются).
#[tauri::command]
pub async fn top_procs() -> Vec<ProcInfo> {
    tauri::async_runtime::spawn_blocking(|| {
        let me = std::process::id();
        let mut sys = sysinfo::System::new();
        sys.refresh_processes();
        let mut m: std::collections::HashMap<String, (u64, u32)> = Default::default();
        for (pid, p) in sys.processes() {
            if pid.as_u32() == me || skipped(p.name()) {
                continue;
            }
            let e = m.entry(clean_name(p.name())).or_default();
            e.0 += p.memory();
            e.1 += 1;
        }
        let mut v: Vec<ProcInfo> = m.into_iter().map(|(name, (mem, count))| ProcInfo { name, mem, count }).collect();
        v.sort_by(|a, b| b.mem.cmp(&a.mem));
        v.truncate(5);
        v
    })
    .await
    .unwrap_or_default()
}

/// Закрыть программу по имени (все её процессы).
#[tauri::command]
pub async fn kill_proc(name: String) -> Result<u32, String> {
    if skipped(&name) {
        return Err("Системный процесс — не трогаю".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let me = std::process::id();
        let mut sys = sysinfo::System::new();
        sys.refresh_processes();
        let mut n = 0;
        for (pid, p) in sys.processes() {
            if pid.as_u32() != me && clean_name(p.name()).eq_ignore_ascii_case(&name) && p.kill() {
                n += 1;
            }
        }
        if n == 0 { Err("Не получилось закрыть — возможно, нужны права администратора".into()) } else { Ok(n) }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn flush_dns() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        let r = hidden("ipconfig", &["/flushdns"]);
        #[cfg(not(windows))]
        let r = hidden("resolvectl", &["flush-caches"]);
        match r {
            Ok(o) if o.status.success() => Ok(()),
            _ => Err("Не получилось сбросить DNS".into()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
