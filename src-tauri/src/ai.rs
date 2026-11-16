// Химера локально: загрузка с докачкой (переживает обрыв сети и перезапуск), запуск llama-server без окон, выгрузка по простою.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const PORT: u16 = 47822;
const MANIFEST: &str = "https://island.lgscode.online/ai/manifest.json";

pub static CANCEL: AtomicBool = AtomicBool::new(false);
pub static RUNNING: AtomicBool = AtomicBool::new(false);

pub struct AiProc {
    pub child: Mutex<Option<Child>>,
    pub last_use: Mutex<Instant>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Item {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub tagline: String,
    pub name: String,
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub model: Item,
    pub runtime: Item,
    #[serde(default)]
    pub runtime_linux: Item,
    #[serde(default)]
    pub variants: Vec<Item>,
}
const SERVER_EXE: &str = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Installed {
    pub model_path: String,
    pub model_name: String,
    pub model_sha: String,
    pub runtime_dir: String,
    pub runtime_sha: String,
}

#[derive(Clone, Serialize)]
pub struct Progress {
    pub state: String, // download | verify | unpack | retry | done | error | paused
    pub stage: String, // model | runtime
    pub done: u64,
    pub total: u64,
    pub speed: f64,
    pub msg: String,
}

pub fn dir(app: &AppHandle) -> PathBuf {
    let d = app.path().local_data_dir().unwrap_or_else(|_| std::env::temp_dir()).join("LegendsIsland").join("ai");
    let _ = fs::create_dir_all(&d);
    d
}
fn installed_path(app: &AppHandle) -> PathBuf {
    dir(app).join("installed.json")
}
pub fn installed(app: &AppHandle) -> Option<Installed> {
    let i: Installed = serde_json::from_str(&fs::read_to_string(installed_path(app)).ok()?).ok()?;
    if Path::new(&i.model_path).is_file() && server_exe(Path::new(&i.runtime_dir)).is_some() {
        Some(i)
    } else {
        None
    }
}
fn read_installed_raw(app: &AppHandle) -> Installed {
    fs::read_to_string(installed_path(app)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}
fn save_installed(app: &AppHandle, i: &Installed) {
    let _ = fs::write(installed_path(app), serde_json::to_string_pretty(i).unwrap_or_default());
}
pub fn server_exe(runtime: &Path) -> Option<PathBuf> {
    let p = runtime.join(SERVER_EXE);
    if p.is_file() {
        return Some(p);
    }
    // в некоторых архивах файлы лежат во вложенной папке
    for e in fs::read_dir(runtime).ok()?.flatten() {
        let q = e.path().join(SERVER_EXE);
        if q.is_file() {
            return Some(q);
        }
    }
    None
}

fn emit(app: &AppHandle, state: &str, stage: &str, done: u64, total: u64, speed: f64, msg: &str) {
    let _ = app.emit(
        "ai_progress",
        Progress { state: state.into(), stage: stage.into(), done, total, speed, msg: msg.into() },
    );
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .user_agent("LegendsIsland/0.4")
        .build()
}

/// Качает с докачкой: продолжает .part с того места, где оборвалось. Ошибки сети — повтор с паузой, пока не отменят.
fn download(app: &AppHandle, it: &Item, dest: &Path, stage: &str) -> Result<(), String> {
    let part = dest.with_extension(format!("{}.part", dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")));
    let mut wait = 2u64;
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err("paused".into());
        }
        let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if it.size > 0 && have >= it.size {
            break;
        }
        let res = agent().get(&it.url).set("Range", &format!("bytes={}-", have)).call();
        let resp = match res {
            Ok(r) => r,
            Err(e) => {
                emit(app, "retry", stage, have, it.size, 0.0, &format!("Нет связи, повторю через {} с: {}", wait, short(&e.to_string())));
                sleep_cancellable(wait);
                wait = (wait * 2).min(30);
                continue;
            }
        };
        // сервер не поддержал докачку — начинаем заново
        let append = resp.status() == 206;
        let mut f = match fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part) {
            Ok(f) => f,
            Err(e) => return Err(format!("Не могу записать файл: {}", e)),
        };
        let mut done = if append { have } else { 0 };
        let mut r = resp.into_reader();
        let mut buf = vec![0u8; 1 << 20];
        let (mut t0, mut b0) = (Instant::now(), done);
        let mut speed = 0.0;
        let mut net_err = None;
        loop {
            if CANCEL.load(Ordering::SeqCst) {
                let _ = f.flush();
                emit(app, "paused", stage, done, it.size, 0.0, "Пауза");
                return Err("paused".into());
            }
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = f.write_all(&buf[..n]) {
                        return Err(format!("Не могу записать файл (место на диске?): {}", e));
                    }
                    done += n as u64;
                    let el = t0.elapsed().as_secs_f64();
                    if el >= 0.5 {
                        speed = (done - b0) as f64 / el;
                        t0 = Instant::now();
                        b0 = done;
                        emit(app, "download", stage, done, it.size, speed, "");
                    }
                }
                Err(e) => {
                    net_err = Some(e.to_string());
                    break;
                }
            }
        }
        let _ = f.flush();
        drop(f);
        let _ = speed;
        if let Some(e) = net_err {
            emit(app, "retry", stage, done, it.size, 0.0, &format!("Связь оборвалась, продолжу с {} — через {} с ({})", human(done), wait, short(&e)));
            sleep_cancellable(wait);
            wait = (wait * 2).min(30);
            continue;
        }
        wait = 2;
        if it.size == 0 || done >= it.size {
            break;
        }
    }
    // проверка целостности
    emit(app, "verify", stage, 0, it.size, 0.0, "Проверяю файл");
    let mut h = Sha256::new();
    let mut f = fs::File::open(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 8 << 20];
    let mut n_all = 0u64;
    let mut last = Instant::now();
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err("paused".into());
        }
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        n_all += n as u64;
        if last.elapsed() > Duration::from_millis(400) {
            last = Instant::now();
            emit(app, "verify", stage, n_all, it.size, 0.0, "Проверяю файл");
        }
    }
    let got = format!("{:x}", h.finalize());
    if !it.sha256.is_empty() && got != it.sha256 {
        let _ = fs::remove_file(&part);
        return Err("Файл скачался с ошибкой — начну заново при следующей установке".into());
    }
    let _ = fs::remove_file(dest);
    fs::rename(&part, dest).map_err(|e| e.to_string())?;
    Ok(())
}
fn sleep_cancellable(secs: u64) {
    for _ in 0..secs * 10 {
        if CANCEL.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn short(s: &str) -> String {
    s.chars().take(90).collect()
}
pub fn human(b: u64) -> String {
    let g = b as f64 / 1_073_741_824.0;
    if g >= 1.0 {
        format!("{:.2} ГБ", g).replace('.', ",")
    } else {
        format!("{:.0} МБ", b as f64 / 1_048_576.0)
    }
}

#[cfg(not(windows))]
fn untar(tgz: &Path, to: &Path) -> Result<(), String> {
    let f = fs::File::open(tgz).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(to);
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
    a.set_preserve_permissions(true);
    a.unpack(to).map_err(|e| e.to_string())
}
fn unpack(archive: &Path, to: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    if archive.to_string_lossy().ends_with(".tar.gz") {
        return untar(archive, to);
    }
    unzip(archive, to)
}
fn unzip(zip_path: &Path, to: &Path) -> Result<(), String> {
    let f = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(to);
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = e.enclosed_name() else { continue };
        let out = to.join(rel);
        if e.is_dir() {
            let _ = fs::create_dir_all(&out);
            continue;
        }
        if let Some(p) = out.parent() {
            let _ = fs::create_dir_all(p);
        }
        let mut w = fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut e, &mut w).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn fetch_manifest() -> Result<Manifest, String> {
    let mut wait = 2;
    for _ in 0..6 {
        match agent().get(MANIFEST).call() {
            Ok(r) => return r.into_string().map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Manifest>(&t).map_err(|e| e.to_string())),
            Err(_) => {
                sleep_cancellable(wait);
                wait *= 2;
            }
        }
        if CANCEL.load(Ordering::SeqCst) {
            break;
        }
    }
    Err("Нет связи с сервером Legends Island".into())
}

/// Полная установка. use_local — свой готовый .gguf (тогда качаем только движок).
pub fn install(app: AppHandle, use_local: Option<String>, variant: Option<String>) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    CANCEL.store(false, Ordering::SeqCst);
    let d = dir(&app);
    let _ = fs::write(
        d.join("installing"),
        match &variant {
            Some(v) => format!("variant:{}", v),
            None => use_local.clone().unwrap_or_default(),
        },
    );
    let r = (|| -> Result<(), String> {
        let m = fetch_manifest()?;
        let mut inst = read_installed_raw(&app);
        // движок llama.cpp
        let rt_dir = d.join("runtime");
        let rt = if cfg!(windows) { m.runtime.clone() } else { m.runtime_linux.clone() };
        if rt.url.is_empty() {
            return Err("Для этой системы движок пока не выложен".into());
        }
        if inst.runtime_sha != rt.sha256 || server_exe(&rt_dir).is_none() {
            let zp = d.join(&rt.file);
            download(&app, &rt, &zp, "runtime")?;
            emit(&app, "unpack", "runtime", 0, 0, 0.0, "Распаковываю движок");
            unpack(&zp, &rt_dir)?;
            let _ = fs::remove_file(&zp);
            inst.runtime_dir = rt_dir.to_string_lossy().to_string();
            inst.runtime_sha = rt.sha256.clone();
            save_installed(&app, &inst);
        }
        // модель
        if let Some(p) = use_local.filter(|p| !p.is_empty()) {
            if !Path::new(&p).is_file() {
                return Err("Файл модели не найден".into());
            }
            inst.model_path = p.clone();
            inst.model_name = Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            inst.model_sha = String::new();
        } else {
            let want: Item = match &variant {
                Some(v) => m.variants.iter().find(|x| &x.id == v).cloned().ok_or("Такой модели нет на сервере")?,
                None => m.model.clone(),
            };
            if inst.model_sha != want.sha256 || !Path::new(&inst.model_path).is_file() {
                let old = inst.model_path.clone();
                let mp = d.join(&want.file);
                download(&app, &want, &mp, "model")?;
                // прежнюю модель из нашей папки убираем, чтобы не копить по 5 ГБ
                if old != mp.to_string_lossy() && old.starts_with(&d.to_string_lossy().to_string()) {
                    let _ = fs::remove_file(&old);
                }
                inst.model_path = mp.to_string_lossy().to_string();
                inst.model_name = want.name.clone();
                inst.model_sha = want.sha256.clone();
            }
        }
        save_installed(&app, &inst);
        Ok(())
    })();
    RUNNING.store(false, Ordering::SeqCst);
    match r {
        Ok(()) => {
            let _ = fs::remove_file(d.join("installing"));
            emit(&app, "done", "", 0, 0, 0.0, "Химера установлена");
        }
        Err(e) if e == "paused" => emit(&app, "paused", "", 0, 0, 0.0, "Пауза — продолжится с того же места"),
        Err(e) => emit(&app, "error", "", 0, 0, 0.0, &e),
    }
}

/// Если прошлую установку прервали закрытием приложения — продолжаем сами.
pub fn resume_on_start(app: AppHandle) {
    let marker = dir(&app).join("installing");
    if let Ok(local) = fs::read_to_string(&marker) {
        let t = local.trim().to_string();
        let variant = t.strip_prefix("variant:").map(|v| v.to_string());
        let local = if t.is_empty() || variant.is_some() { None } else { Some(local) };
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(8));
            install(app, local, variant);
        });
    }
}

pub fn part_progress(app: &AppHandle) -> (u64, u64) {
    let d = dir(app);
    let mut have = 0;
    if let Ok(rd) = fs::read_dir(&d) {
        for e in rd.flatten() {
            if e.path().to_string_lossy().ends_with(".part") {
                have += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (have, 0)
}

pub fn delete(app: &AppHandle) {
    stop(app);
    let d = dir(app);
    let inst = read_installed_raw(app);
    // свой файл пользователя не трогаем — удаляем только то, что качали сами
    if inst.model_path.starts_with(&d.to_string_lossy().to_string()) {
        let _ = fs::remove_file(&inst.model_path);
    }
    let _ = fs::remove_dir_all(d.join("runtime"));
    if let Ok(rd) = fs::read_dir(&d) {
        for e in rd.flatten() {
            let p = e.path();
            let ps = p.to_string_lossy().to_string();
            if ps.ends_with(".part") || ps.ends_with(".zip") || ps.ends_with(".tar.gz") {
                let _ = fs::remove_file(p);
            }
        }
    }
    let _ = fs::remove_file(d.join("installing"));
    let _ = fs::remove_file(installed_path(app));
}

// ---------- сервер модели ----------
fn health() -> bool {
    ureq::get(&format!("http://127.0.0.1:{}/health", PORT))
        .timeout(Duration::from_secs(2))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

pub fn start(app: &AppHandle) -> Result<u16, String> {
    let st = app.state::<AiProc>();
    *st.last_use.lock().unwrap() = Instant::now();
    {
        let mut c = st.child.lock().unwrap();
        if let Some(ch) = c.as_mut() {
            if ch.try_wait().ok().flatten().is_none() {
                drop(c);
                // уже запущен — ждём готовности (загрузка модели)
                for _ in 0..240 {
                    if health() {
                        return Ok(PORT);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                return Err("Модель долго загружается".into());
            }
        }
        *c = None;
    }
    let inst = installed(app).ok_or("Химера не установлена")?;
    let exe = server_exe(Path::new(&inst.runtime_dir)).ok_or("Нет движка llama.cpp")?;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(2).max(2);
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([
        "-m", &inst.model_path,
        "-c", "4096",
        "--host", "127.0.0.1",
        "--port", &PORT.to_string(),
        "-t", &threads.to_string(),
        "--no-webui",
    ]);
    if let Some(p) = exe.parent() {
        cmd.current_dir(p);
        #[cfg(not(windows))]
        cmd.env("LD_LIBRARY_PATH", p);
    }
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_4000); // без окна, пониженный приоритет (BELOW_NORMAL)
    }
    let child = cmd.spawn().map_err(|e| format!("Не запустился llama-server: {}", e))?;
    *st.child.lock().unwrap() = Some(child);
    for _ in 0..360 {
        if health() {
            return Ok(PORT);
        }
        {
            let mut c = st.child.lock().unwrap();
            if let Some(ch) = c.as_mut() {
                if let Ok(Some(code)) = ch.try_wait() {
                    *c = None;
                    return Err(format!("Модель не запустилась (код {}). Возможно, не хватает памяти", code));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Модель не успела загрузиться".into())
}

pub fn stop(app: &AppHandle) {
    let st = app.state::<AiProc>();
    let mut c = st.child.lock().unwrap();
    if let Some(mut ch) = c.take() {
        let _ = ch.kill();
        let _ = ch.wait();
    }
}

pub fn is_loaded(app: &AppHandle) -> bool {
    let st = app.state::<AiProc>();
    let mut c = st.child.lock().unwrap();
    match c.as_mut() {
        Some(ch) => ch.try_wait().ok().flatten().is_none(),
        None => false,
    }
}

/// Выгружаем модель из памяти после простоя.
pub fn idle_watchdog(app: AppHandle, minutes: u64) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let idle = app.state::<AiProc>().last_use.lock().unwrap().elapsed();
        if idle > Duration::from_secs(minutes * 60) && is_loaded(&app) {
            stop(&app);
            let _ = app.emit("ai_unloaded", ());
        }
    });
}
