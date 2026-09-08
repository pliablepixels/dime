mod assets;
mod gunk;
mod hog;
mod ru;
mod rules;
mod scan;
mod snapshot;
mod vault;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use notify::{RecursiveMode, Watcher};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use sysinfo::Disks;
use tower_http::set_header::SetResponseHeaderLayer;

enum ScanState {
    Idle,
    Scanning(PathBuf, Arc<scan::Progress>),
    Done { root: PathBuf, tree: scan::Node, as_of: Option<i64> }, // as_of: set when the tree came from a snapshot rather than a fresh scan
}

struct App {
    scan: Mutex<ScanState>,
    hog: hog::Hog,
    /// Tree version, bumped whenever the watcher patches it. The UI polls this.
    version: AtomicU64,
    /// Candidate list for the current tree version (recomputed lazily when the version moves).
    cands: Mutex<(u64, Arc<Vec<gunk::Candidate>>)>,
    pending: Mutex<HashSet<PathBuf>>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// Ru's brain, picked from the gear setting and what the machine has; swapped live from the gear menu.
    ru: Mutex<ru::Ru>,
}
type Shared = Arc<App>;
type ApiErr = (StatusCode, String);

fn bad(msg: impl Into<String>) -> ApiErr {
    (StatusCode::BAD_REQUEST, msg.into())
}

/// Running from inside DiMe.app rather than from a shell.
fn in_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

fn main() {
    if std::env::args().any(|a| a == "--rules") {
        print!("{}", rules::BUILTIN); // copy to ~/.dime/rules.toml and edit
        return;
    }
    let windowed = !std::env::args().any(|a| a == "--browser") && (in_bundle() || std::env::args().any(|a| a == "--window"));
    // The webview must own the main thread, so the server gets a runtime of its own. `rt` stays alive for the whole run.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let addr = rt.block_on(serve());
    if windowed {
        window(&addr); // returns when the window closes
    } else {
        let _ = std::process::Command::new("open").arg(format!("http://{addr}")).spawn();
        std::thread::park();
    }
}

/// Opens the app in a WKWebView window of its own. External links go to the default browser.
fn window(addr: &str) {
    use tao::{event::{Event, WindowEvent}, event_loop::{ControlFlow, EventLoopBuilder}, window::WindowBuilder};
    let event_loop = EventLoopBuilder::new().build();
    let win = WindowBuilder::new()
        .with_title("DiMe")
        .with_inner_size(tao::dpi::LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(900.0, 600.0))
        .build(&event_loop)
        .unwrap();
    let _webview = wry::WebViewBuilder::new()
        .with_url(format!("http://{addr}"))
        .with_background_color((13, 19, 33, 255)) // the app's own --bg, so there is no white flash while it loads
        .with_devtools(cfg!(debug_assertions))
        .with_new_window_req_handler(|url, _features| {
            let _ = std::process::Command::new("open").arg(url).spawn(); // links to the outside world leave the app
            wry::NewWindowResponse::Deny
        })
        .build(&win)
        .unwrap();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *flow = ControlFlow::Exit;
        }
    });
}

/// Everything the HTTP server needs. Returns the address it is listening on; serving continues in the background.
async fn serve() -> String {
    // the data folder used to be ~/.dume; carry it over once so the vault, state and snapshots survive the rename
    if let Ok(h) = std::env::var("HOME") {
        let (old, new) = (PathBuf::from(&h).join(".dume"), PathBuf::from(&h).join(".dime"));
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
    // scanning is bound by APFS, not by cores: past 8 threads directory reads contend and get slower
    let _ = rayon::ThreadPoolBuilder::new().num_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8)).build_global();
    let app = Arc::new(App {
        scan: Mutex::new(ScanState::Idle),
        hog: hog::Hog::new(),
        version: AtomicU64::new(0),
        cands: Mutex::new((u64::MAX, Arc::new(vec![]))),
        pending: Mutex::new(HashSet::new()),
        watcher: Mutex::new(None),
        ru: Mutex::new(ru::detect(&ru::load_settings())),
    });
    // Apply queued filesystem changes once a second, coalesced.
    let app_bg = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let paths: Vec<PathBuf> = app_bg.pending.lock().unwrap().drain().collect();
        if paths.is_empty() {
            continue;
        }
        let mut s = app_bg.scan.lock().unwrap();
        if let ScanState::Done { root, tree, .. } = &mut *s {
            for p in &paths {
                scan::patch(tree, root, p);
            }
            app_bg.version.fetch_add(1, Ordering::Relaxed);
        }
    });
    let router = Router::new()
        .route("/api/home", get(home))
        .route("/api/drives", get(drives))
        .route("/api/ls", get(ls))
        .route("/api/scan", post(start_scan))
        .route("/api/resume", post(resume))
        .route("/api/state", get(state_get).post(state_set))
        .route("/api/status", get(status))
        .route("/api/tree", get(tree))
        .route("/api/gunk", get(gunk_list))
        .route("/api/summary", get(summary))
        .route("/api/idle", get(idle))
        .route("/api/find", get(find))
        .route("/api/procs", get(procs))
        .route("/api/procfiles", get(procfiles))
        .route("/api/open", post(open_path))
        .route("/api/reveal", post(reveal))
        .route("/api/vault", get(vault_list))
        .route("/api/vault/archive", post(vault_archive))
        .route("/api/vault/restore", post(vault_restore))
        .route("/api/vault/purge", post(vault_purge))
        .route("/api/delete", post(delete_paths))
        .route("/api/ask", post(ask))
        .route("/api/ru", get(ru_get).post(ru_set))
        .route("/api/reset", post(reset))
        .fallback(static_file)
        // static files change while developing; make every reload re-check them
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-cache")))
        .with_state(app.clone());
    let addr = format!("127.0.0.1:{}", std::env::var("DIME_PORT").unwrap_or_else(|_| "4242".into()));
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| { eprintln!("DiMe: cannot listen on {addr}: {e}. Is another DiMe running? Set DIME_PORT to use a different port."); std::process::exit(1) });
    println!("DiMe → http://{addr}");
    let ru_note = { let r = app.ru.lock().unwrap(); if matches!(r.provider, ru::Provider::None) { None } else { Some(r.label.clone()) } };
    match ru_note {
        None => println!("  note: Ru (the guru) has no AI here. Install the Claude Code CLI (https://claude.ai/code) or Codex (`npm i -g @openai/codex`) and log in once, or run Ollama, or pick an endpoint in the gear menu. Di and Me work without it."),
        Some(label) => println!("  Ru answers via {label}"),
    }
    if !have("jq") { println!("  note: `jq` not found on PATH; Ru uses it to trim Di's JSON. `brew install jq`."); }
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// The UI is built into the binary (see assets.rs). While developing, a `static/` folder in the
/// working directory wins for any file it has, so edit-and-refresh keeps working.
async fn static_file(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = match uri.path() {
        "/" => "/index.html",
        p => p,
    };
    let Some((_, mime, built_in)) = assets::FILES.iter().find(|(p, ..)| *p == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let on_disk = std::fs::read(PathBuf::from("static").join(path.trim_start_matches('/')));
    ([(axum::http::header::CONTENT_TYPE, *mime)], on_disk.unwrap_or_else(|_| built_in.to_vec())).into_response()
}

async fn home() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "path": std::env::var("HOME").unwrap_or_else(|_| "/".into()) }))
}

#[derive(Serialize)]
struct Drive {
    name: String,
    mount: String,
    total: u64,
    available: u64,
    removable: bool,
}

async fn drives() -> Json<Vec<Drive>> {
    let mut out: Vec<Drive> = Disks::new_with_refreshed_list()
        .iter()
        .filter(|d| {
            let m = d.mount_point().to_string_lossy();
            d.total_space() > 0 && (m == "/" || m.starts_with("/Volumes/"))
        })
        .map(|d| Drive {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_string_lossy().into_owned(),
            total: d.total_space(),
            available: d.available_space(),
            removable: d.is_removable(),
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out.dedup_by(|a, b| a.mount == b.mount);
    Json(out)
}

#[derive(Deserialize)]
struct LsQ {
    path: String,
}

/// Subfolders of a path, for the landing-page browser. Hidden ones sort last.
async fn ls(Query(q): Query<LsQ>) -> Result<Json<serde_json::Value>, ApiErr> {
    let p = PathBuf::from(&q.path).canonicalize().map_err(|e| bad(e.to_string()))?;
    if !p.is_dir() {
        return Err(bad("not a directory"));
    }
    let mut dirs: Vec<String> = std::fs::read_dir(&p)
        .map_err(|e| bad(e.to_string()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort_by_key(|d| (d.starts_with('.'), d.to_lowercase()));
    dirs.truncate(300);
    Ok(Json(serde_json::json!({ "path": p, "dirs": dirs })))
}

#[derive(Deserialize)]
struct PathReq {
    path: String,
}

async fn start_scan(State(app): State<Shared>, Json(req): Json<PathReq>) -> Result<StatusCode, ApiErr> {
    let root = PathBuf::from(&req.path).canonicalize().map_err(|e| bad(format!("{}: {e}", req.path)))?;
    if !root.is_dir() {
        return Err(bad("not a directory"));
    }
    let progress = Arc::new(scan::Progress::new(&root));
    {
        let mut s = app.scan.lock().unwrap();
        if matches!(*s, ScanState::Scanning(..)) {
            return Err((StatusCode::CONFLICT, "scan in progress".into()));
        }
        *s = ScanState::Scanning(root.clone(), progress.clone());
    }
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        let t0 = std::time::Instant::now();
        let tree = scan::scan(&root, &progress);
        let scanned = t0.elapsed();
        let t1 = std::time::Instant::now();
        let _ = snapshot::save(&root, &tree); // next launch opens this map at once
        println!("  scanned {} files, {:.1} GB in {:.1}s; snapshot in {:.1}s", tree.files, tree.size as f64 / 1e9, scanned.as_secs_f64(), t1.elapsed().as_secs_f64());
        *app2.watcher.lock().unwrap() = watch(&app2, &root);
        app2.pending.lock().unwrap().clear();
        *app2.scan.lock().unwrap() = ScanState::Done { root, tree, as_of: None };
        app2.version.fetch_add(1, Ordering::Relaxed);
    });
    Ok(StatusCode::ACCEPTED)
}

/// Watch the scan root (FSEvents on macOS) and queue changed paths for the patch thread.
fn watch(app: &Shared, root: &Path) -> Option<notify::RecommendedWatcher> {
    let app = app.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            app.pending.lock().unwrap().extend(ev.paths);
        }
    })
    .ok()?;
    w.watch(root, RecursiveMode::Recursive).ok()?;
    Some(w)
}

#[derive(Serialize)]
struct LiveItem {
    name: String,
    is_dir: bool,
    size: u64,
    files: u64,
    types: scan::Types,
    done: bool,
}

async fn status(State(app): State<Shared>) -> Json<serde_json::Value> {
    let s = app.scan.lock().unwrap();
    let (state, root) = match &*s {
        ScanState::Idle => ("idle", None),
        ScanState::Scanning(r, _) => ("scanning", Some(r)),
        ScanState::Done { root, .. } => ("done", Some(root)),
    };
    let ru_label = { let r = app.ru.lock().unwrap(); if matches!(r.provider, ru::Provider::None) { serde_json::Value::Null } else { serde_json::Value::String(r.label.clone()) } };
    let as_of = match &*s { ScanState::Done { as_of, .. } => *as_of, _ => None };
    let snapshots = snapshot::list(); // always listed; the landing shows them only while idle, the reset dialog needs them from a live map too
    let (files, size, live) = match &*s {
        ScanState::Scanning(_, p) => {
            let mut live: Vec<LiveItem> = p
                .items
                .iter()
                .map(|i| LiveItem {
                    name: i.name.clone(),
                    is_dir: i.is_dir,
                    size: i.size.load(Ordering::Relaxed),
                    files: i.files.load(Ordering::Relaxed),
                    types: std::array::from_fn(|k| i.types[k].load(Ordering::Relaxed)),
                    done: i.done.load(Ordering::Relaxed),
                })
                .collect();
            let files = live.iter().map(|i| i.files).sum();
            let size = live.iter().map(|i| i.size).sum();
            // a fixed, name-ordered set: every entry keeps its spot on the live map for the whole scan
            live.sort_unstable_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            (files, size, Some(live))
        }
        ScanState::Done { tree, .. } => (tree.files, tree.size, None),
        _ => (0, 0, None),
    };
    Json(serde_json::json!({
        "state": state, "root": root, "files": files, "size": size, "live": live, "as_of": as_of, "snapshots": snapshots, "ru": ru_label,
        "version": app.version.load(Ordering::Relaxed),
    }))
}

#[derive(Deserialize)]
struct TreeQ {
    #[serde(default)]
    path: String,
    #[serde(default = "one")]
    depth: u32,
    /// When set, every node also reports bytes idle for at least this many days.
    idle: Option<i64>,
}
fn one() -> u32 {
    1
}

fn with_tree<T>(app: &App, f: impl FnOnce(&Path, &scan::Node) -> Result<T, ApiErr>) -> Result<T, ApiErr> {
    match &*app.scan.lock().unwrap() {
        ScanState::Done { root, tree, .. } => f(root, tree),
        _ => Err((StatusCode::PRECONDITION_FAILED, "no scan".into())),
    }
}

async fn tree(State(app): State<Shared>, Query(q): Query<TreeQ>) -> Result<Json<scan::Out>, ApiErr> {
    with_tree(&app, |_, t| {
        let n = scan::get(t, &q.path).ok_or_else(|| (StatusCode::NOT_FOUND, "no such path".into()))?;
        let cutoff = q.idle.map(|d| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - d.max(0) * 86_400
        });
        Ok(Json(scan::subtree(n, &q.path, q.depth.min(2), cutoff)))
    })
}

#[derive(Deserialize)]
struct FindQ {
    q: String,
    #[serde(default)]
    path: String,
    #[serde(default = "two_hundred")]
    limit: usize,
}
fn two_hundred() -> usize {
    200
}
#[derive(Serialize)]
struct Hit {
    path: String,
    size: u64,
    is_dir: bool,
    files: u64,
}
/// Name search over the in-memory tree (case-insensitive substring), biggest first. Ru uses this instead of `find`.
async fn find(State(app): State<Shared>, Query(q): Query<FindQ>) -> Result<Json<Vec<Hit>>, ApiErr> {
    let needle = q.q.to_lowercase();
    if needle.is_empty() {
        return Err(bad("q is empty"));
    }
    with_tree(&app, |_, t| {
        let start = scan::get(t, &q.path).ok_or_else(|| (StatusCode::NOT_FOUND, "no such path".into()))?;
        let mut out = vec![];
        fn walk(n: &scan::Node, path: &str, needle: &str, out: &mut Vec<Hit>) {
            for c in &n.children {
                let p = scan::join(path, &c.name);
                if c.name.to_lowercase().contains(needle) {
                    out.push(Hit { path: p.clone(), size: c.size, is_dir: c.is_dir, files: c.files });
                }
                if c.is_dir {
                    walk(c, &p, needle, out);
                }
            }
        }
        walk(start, q.path.trim_matches('/'), &needle, &mut out);
        out.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        out.truncate(q.limit.min(1000));
        Ok(Json(out))
    })
}

#[derive(Deserialize)]
struct GunkQ {
    #[serde(default)]
    path: String,
    /// Idle view: size each candidate by the bytes inside it untouched for this many days, and drop the ones with none.
    idle: Option<i64>,
}

/// Candidates for the current tree version, computed once and shared.
fn candidates(app: &App) -> Result<Arc<Vec<gunk::Candidate>>, ApiErr> {
    let v = app.version.load(Ordering::Relaxed);
    let mut c = app.cands.lock().unwrap();
    if c.0 != v {
        let list = with_tree(app, |_, t| Ok(gunk::find_all(t)))?;
        *c = (v, Arc::new(list));
    }
    Ok(c.1.clone())
}

#[derive(Deserialize)]
struct IdleQ {
    #[serde(default)]
    path: String,
    days: i64,
}

async fn idle(State(app): State<Shared>, Query(q): Query<IdleQ>) -> Result<Json<Vec<scan::IdleFile>>, ApiErr> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    with_tree(&app, |_, t| {
        let n = scan::get(t, &q.path).ok_or_else(|| (StatusCode::NOT_FOUND, "no such path".into()))?;
        Ok(Json(scan::idle_files(n, q.path.trim_matches('/'), now - q.days.max(0) * 86_400, now, 300)))
    })
}

async fn gunk_list(State(app): State<Shared>, Query(q): Query<GunkQ>) -> Result<Json<Vec<gunk::Candidate>>, ApiErr> {
    let all = candidates(&app)?;
    if let Some(days) = q.idle {
        let cutoff = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - days.max(0) * 86_400;
        return with_tree(&app, |_, t| {
            let mut out: Vec<gunk::Candidate> = gunk::under(&all, &q.path)
                .filter_map(|c| {
                    let idle = scan::get(t, &c.path).map(|n| gunk::idle_bytes(n, cutoff)).unwrap_or(0);
                    (idle > 0).then(|| gunk::Candidate { size: idle, full_size: Some(c.size), ..c.clone() })
                })
                .collect();
            out.sort_by(|a, b| b.size.cmp(&a.size));
            Ok(Json(out))
        });
    }
    // every group gets its full list (capped per kind so one huge kind cannot swamp the payload), best first
    let mut per: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    Ok(Json(gunk::under(&all, &q.path).filter(|c| { let n = per.entry(c.reason.as_str()).or_default(); *n += 1; *n <= 500 }).cloned().collect()))
}

async fn summary(State(app): State<Shared>, Query(q): Query<GunkQ>) -> Result<Json<gunk::Summary>, ApiErr> {
    let all = candidates(&app)?;
    Ok(Json(gunk::summary(gunk::under(&all, &q.path))))
}

async fn procs(State(app): State<Shared>) -> Json<hog::Snapshot> {
    Json(app.hog.snapshot())
}

#[derive(Deserialize)]
struct PidQ {
    pid: u32,
}

#[derive(Serialize)]
struct ProcFile {
    path: String,
    /// Path relative to the scan root when the file lives inside it.
    rel: Option<String>,
    size: u64,
    /// File type index, same order as the tree's `types`.
    ty: usize,
    /// Seconds since the file was last written, if known.
    written_ago: Option<u64>,
}

async fn procfiles(State(app): State<Shared>, Query(q): Query<PidQ>) -> Json<Vec<ProcFile>> {
    let files = tokio::task::spawn_blocking(move || {
        hog::open_files(q.pid)
            .into_iter()
            .map(|f| {
                let md = std::fs::metadata(&f.path).ok();
                let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
                let ago = md.and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
                let name = f.path.rsplit('/').next().unwrap_or("").to_string();
                (f.path, size, scan::type_of(&name), ago)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let root = match &*app.scan.lock().unwrap() {
        ScanState::Done { root, .. } => Some(root.clone()),
        _ => None,
    };
    Json(files.into_iter().map(|(path, size, ty, written_ago)| {
        let rel = root.as_ref().and_then(|r| Path::new(&path).strip_prefix(r).ok()).map(|p| p.to_string_lossy().into_owned());
        ProcFile { path, rel, size, ty, written_ago }
    }).collect())
}

/// Resolve a relative path inside the scan root, refusing anything that escapes it.
fn resolve(app: &App, rel: &str) -> Result<(String, PathBuf), ApiErr> {
    let rel = rel.trim_matches('/').to_string();
    if rel.split('/').any(|p| p == "..") {
        return Err(bad("refusing"));
    }
    let abs = with_tree(app, |root, _| {
        let abs = root.join(&rel).canonicalize().map_err(|e| bad(e.to_string()))?;
        if !abs.starts_with(root) {
            return Err(bad("outside scan root"));
        }
        Ok(abs)
    })?;
    Ok((rel, abs))
}

/// Reveal any absolute path in Finder (used for process executables, which live outside the scan root).
async fn reveal(Json(req): Json<PathReq>) -> Result<StatusCode, ApiErr> {
    let p = PathBuf::from(&req.path);
    if !p.is_absolute() || !p.exists() {
        return Err(bad("no such path"));
    }
    std::process::Command::new("open").arg("-R").arg(&p).spawn().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Open a folder in Finder, or reveal a file in its folder.
async fn open_path(State(app): State<Shared>, Json(req): Json<PathReq>) -> Result<StatusCode, ApiErr> {
    let (_, abs) = resolve(&app, &req.path)?;
    let mut cmd = std::process::Command::new("open");
    if !abs.is_dir() {
        cmd.arg("-R");
    }
    cmd.arg(&abs).spawn().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- cleanup actions: archive to the vault, delete, restore ----------

#[derive(Deserialize)]
struct PathsReq {
    paths: Vec<String>,
    /// Idle view: act only on the files inside each path untouched for this many days; the rest stays.
    idle: Option<i64>,
}
/// Files under `abs` (relative to it) untouched since `cutoff`, from Di's tree. A file path returns itself if old enough.
fn idle_files(app: &App, rel: &str, cutoff: i64) -> Result<(Vec<PathBuf>, u64), ApiErr> {
    with_tree(app, |_, t| {
        let n = scan::get(t, rel).ok_or_else(|| (StatusCode::NOT_FOUND, "no such path".into()))?;
        let mut out = vec![];
        let mut bytes = 0;
        fn go(n: &scan::Node, p: PathBuf, cutoff: i64, out: &mut Vec<PathBuf>, bytes: &mut u64) {
            if !n.is_dir {
                if n.atime.max(n.mtime) <= cutoff { *bytes += n.size; out.push(p); }
                return;
            }
            for c in &n.children { go(c, p.join(&c.name), cutoff, out, bytes); }
        }
        if n.is_dir { for c in &n.children { go(c, PathBuf::from(&c.name), cutoff, &mut out, &mut bytes); } } else if n.atime.max(n.mtime) <= cutoff { out.push(PathBuf::new()); bytes = n.size; }
        Ok((out, bytes))
    })
}
fn cutoff_for(days: i64) -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - days.max(0) * 86_400
}
#[derive(Deserialize)]
struct IdsReq {
    ids: Vec<String>,
}
/// One line per item so the UI can say exactly what happened and what did not.
#[derive(Serialize)]
struct Outcome {
    key: String,
    ok: bool,
    error: Option<String>,
}

/// Resolve a path for a destructive action: inside the root, not the root itself, not holding the vault.
fn target(app: &App, rel: &str) -> Result<(String, PathBuf, u64, String), ApiErr> {
    let (rel, abs) = resolve(app, rel)?;
    let is_root = with_tree(app, |root, _| Ok(abs == root))?;
    if rel.is_empty() || is_root {
        return Err(bad("refusing to touch the scan root"));
    }
    if vault::dir().starts_with(&abs) {
        return Err(bad("that holds the vault"));
    }
    let size = with_tree(app, |_, t| Ok(scan::get(t, &rel).map(|n| n.size).unwrap_or(0)))?;
    let note = candidates(app)?.iter().find(|c| c.path == rel).map(|c| format!("{} · {}", c.what, c.note)).unwrap_or_default();
    Ok((rel, abs, size, note))
}

/// Re-stat changed paths so the map and candidate list catch up at once instead of waiting on the watcher.
fn touched(app: &App, paths: &[PathBuf]) {
    let mut s = app.scan.lock().unwrap();
    if let ScanState::Done { root, tree, .. } = &mut *s {
        for p in paths {
            scan::patch(tree, root, p);
        }
    }
    app.version.fetch_add(1, Ordering::Relaxed);
}

async fn vault_list() -> Json<Vec<vault::Entry>> {
    Json(vault::list())
}

async fn vault_archive(State(app): State<Shared>, Json(req): Json<PathsReq>) -> Json<Vec<Outcome>> {
    tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        let mut moved = vec![];
        let idle = req.idle.map(cutoff_for);
        for key in req.paths {
            let r = target(&app, &key).map_err(|e| e.1).and_then(|(rel, abs, size, note)| match idle {
                Some(cut) if abs.is_dir() => idle_files(&app, &rel, cut).map_err(|e| e.1).and_then(|(files, bytes)| vault::archive_partial(&abs, &files, bytes, format!("{note} · only files idle at the time")).map(|_| files.iter().map(|f| abs.join(f)).collect::<Vec<_>>()).map_err(|e| e.to_string())),
                Some(cut) => idle_files(&app, &rel, cut).map_err(|e| e.1).and_then(|(files, _)| if files.is_empty() { Err("not idle that long".into()) } else { vault::archive(&abs, size, note).map(|_| vec![abs]).map_err(|e| e.to_string()) }),
                None => vault::archive(&abs, size, note).map(|_| vec![abs]).map_err(|e| e.to_string()),
            });
            match r {
                Ok(paths) => { moved.extend(paths); out.push(Outcome { key, ok: true, error: None }) }
                Err(e) => out.push(Outcome { key, ok: false, error: Some(e) }),
            }
        }
        touched(&app, &moved);
        Json(out)
    })
    .await
    .unwrap()
}

async fn delete_paths(State(app): State<Shared>, Json(req): Json<PathsReq>) -> Json<Vec<Outcome>> {
    tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        let mut gone = vec![];
        let idle = req.idle.map(cutoff_for);
        for key in req.paths {
            let r = target(&app, &key).map_err(|e| e.1).and_then(|(rel, abs, ..)| match idle {
                Some(cut) => idle_files(&app, &rel, cut).map_err(|e| e.1).and_then(|(files, _)| {
                    if files.is_empty() { return Err("not idle that long".into()); }
                    let mut done = vec![];
                    for f in &files { let p = if f.as_os_str().is_empty() { abs.clone() } else { abs.join(f) }; vault::delete(&p).map_err(|e| e.to_string())?; done.push(p); } // a file candidate is its own only entry
                    Ok(done)
                }),
                None => vault::delete(&abs).map(|_| vec![abs]).map_err(|e| e.to_string()),
            });
            match r {
                Ok(paths) => { gone.extend(paths); out.push(Outcome { key, ok: true, error: None }) }
                Err(e) => out.push(Outcome { key, ok: false, error: Some(e) }),
            }
        }
        touched(&app, &gone);
        Json(out)
    })
    .await
    .unwrap()
}

async fn vault_restore(State(app): State<Shared>, Json(req): Json<IdsReq>) -> Json<Vec<Outcome>> {
    tokio::task::spawn_blocking(move || {
        let mut out = vec![];
        let mut back = vec![];
        for key in req.ids {
            match vault::restore(&key) {
                Ok((_, paths)) => { back.extend(paths); out.push(Outcome { key, ok: true, error: None }) }
                Err(e) => out.push(Outcome { key, ok: false, error: Some(e.to_string()) }),
            }
        }
        touched(&app, &back);
        Json(out)
    })
    .await
    .unwrap()
}

async fn vault_purge(Json(req): Json<IdsReq>) -> Json<Vec<Outcome>> {
    tokio::task::spawn_blocking(move || {
        Json(req.ids.into_iter().map(|key| match vault::purge(&key) {
            Ok(_) => Outcome { key, ok: true, error: None },
            Err(e) => Outcome { key, ok: false, error: Some(e.to_string()) },
        }).collect())
    })
    .await
    .unwrap()
}

// ---------- Ru: the wise one. Runs `claude -p` with what you are looking at and streams the answer ----------

const RU_SYSTEM: &str = "You are Ru, the wise one in DiMe, a Mac disk and memory explorer. Di maps the disk and flags candidates for removal, Me watches memory, you give judgment. \
Di has already scanned the whole tree and keeps it in memory. Ask Di before touching the disk; it answers instantly and its sizes are the ones the user sees. The `di` command queries that index; paths are RELATIVE to the scan root ROOT, and an empty path means the root: \
`di tree <rel> [depth]` a folder and its children (name, size, files, is_dir, mtime, atime, types), sizes on disk, biggest first, depth 1 or 2; \
`di flagged <rel>` everything Di flagged under it, with tier, reason, note; \
`di find <name-substring> [rel] [limit]` name search across the tree, biggest first; \
`di idle <rel> <days>` the biggest files untouched that long. Output is JSON; pipe through jq to trim it. Only walk the disk yourself (ls, du, find) for what Di does not hold: file contents, permissions, ownership, symlink targets, files under 1 MB Di skipped, or whether a file is open. \
Di finds; you validate. Di's flags are guesses from names, sizes and ages. Your job is to check each candidate against reality before it goes: is it open or in use right now (lsof, ps), does a running or installed app still depend on it, is it referenced by a config or a project next to it, was it touched recently, would it be regenerated, and what breaks without it. Then give a verdict per item: delete, move to the vault (reversible, in ~/.dime/vault), or keep, with the reason in one line. When you could not verify, say so and prefer archive. \
You may inspect with read-only tools: files and metadata (ls, du, df, file, stat, mdls, mdfind, head, tail, wc, xattr -l, plutil -p, codesign -d, Read, Glob, Grep; no find, sort or awk: use di find and jq instead) and processes and the system (ps, pgrep, lsof, top -l 1, vm_stat, sysctl, launchctl list, diskutil info, brew list). Use them when the context cannot tell you something, like what an unknown folder holds, whether a file is still open, or what a process is doing. Anything that writes, and sudo, is not available to you: say so plainly if a question needs it, and suggest what the user could run themselves. Never modify, move, or delete anything; the user does that from DiMe. \
Be economical with checks: batch related commands into one call, and stop checking once you can judge; always finish with the verdict even if a check was refused or you ran short of turns. \
Mounted volumes under a folder (for example simulator runtime images under CoreSimulator/Volumes) are not counted by Di and cannot be moved; the space lives in the .dmg they are mounted from. \
Answer in short paragraphs and bullets, plain language, no headings, sizes in human units. Do not restate the context back. End with a one-line verdict when a decision was asked for. \
When your answer recommends what to do with specific items, finish with a fenced code block tagged dime holding JSON of the form {\"archive\": [...], \"delete\": [...], \"keep\": [...]} with absolute paths only, taken from the context or from what you inspected. archive means move to the vault (reversible), delete means remove for good; the vault is where things wait before a final delete. DiMe turns that block into buttons; leave it out when nothing should change.";

#[derive(Deserialize)]
struct AskReq {
    prompt: String,
}

/// One question for Ru. The provider's own output is normalised into a plain event stream by ru.rs; the page reads that.
async fn ask(State(app): State<Shared>, Json(req): Json<AskReq>) -> Result<axum::response::Response, ApiErr> {
    let root = match &*app.scan.lock().unwrap() {
        ScanState::Done { root, .. } | ScanState::Scanning(root, _) => root.clone(),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())),
    };
    let port = std::env::var("DIME_PORT").unwrap_or_else(|_| "4242".into());
    let system = RU_SYSTEM.replace("ROOT", &root.to_string_lossy());
    let bin = ru::di_helper(&port).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stream = ru::ask(&app.ru.lock().unwrap(), system, req.prompt, root, ru::path_with(&bin)).map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(stream));
    Ok(axum::response::Response::builder().header("content-type", "application/x-ndjson").header("cache-control", "no-cache").body(body).unwrap())
}

/// The gear menu: what is installed, what is chosen, and switching it live.
fn ru_view(app: &App) -> serde_json::Value {
    let cfg = ru::load_settings();
    let r = app.ru.lock().unwrap();
    serde_json::json!({ "mode": if cfg.mode.is_empty() { "auto" } else { cfg.mode.as_str() }, "current": if matches!(r.provider, ru::Provider::None) { serde_json::Value::Null } else { serde_json::Value::String(r.label.clone()) }, "have": ru::available(&cfg) })
}
async fn ru_get(State(app): State<Shared>) -> Json<serde_json::Value> {
    Json(ru_view(&app))
}
async fn ru_set(State(app): State<Shared>, Json(cfg): Json<ru::Settings>) -> Result<Json<serde_json::Value>, ApiErr> {
    if !["auto", "claude", "codex", "api", "none"].contains(&cfg.mode.as_str()) {
        return Err(bad("mode must be auto, claude, codex, api or none"));
    }
    ru::save_settings(&cfg).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *app.ru.lock().unwrap() = ru::detect(&cfg);
    Ok(Json(ru_view(&app)))
}

// ---------- resume from a snapshot, and the per-root state file ----------

/// Open the last map of a root from its snapshot: no rescan, the watcher picks up from here.
async fn resume(State(app): State<Shared>, Json(req): Json<PathReq>) -> Result<Json<snapshot::Meta>, ApiErr> {
    let root = PathBuf::from(&req.path);
    if matches!(*app.scan.lock().unwrap(), ScanState::Scanning(..)) {
        return Err((StatusCode::CONFLICT, "scan in progress".into()));
    }
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        let (meta, tree) = snapshot::load(&root).map_err(|e| bad(format!("no snapshot for {}: {e}", root.display())))?;
        *app2.watcher.lock().unwrap() = watch(&app2, &root);
        app2.pending.lock().unwrap().clear();
        *app2.scan.lock().unwrap() = ScanState::Done { root, tree, as_of: Some(meta.at) };
        app2.version.fetch_add(1, Ordering::Relaxed);
        Ok(Json(meta))
    })
    .await
    .unwrap()
}

#[derive(Deserialize)]
struct StateQ {
    root: String,
}
#[derive(Deserialize)]
struct StateReq {
    root: String,
    state: serde_json::Value,
}
fn state_file() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".dime").join("state.json")
}
fn state_all() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(state_file()).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
/// What the page remembers per scan root: hidden folders, tiers, filter, ticks, Ru's conversation. Small JSON, one key per root.
async fn state_get(Query(q): Query<StateQ>) -> Json<serde_json::Value> {
    Json(state_all().get(&q.root).cloned().unwrap_or(serde_json::Value::Null))
}
static STATE_LOCK: Mutex<()> = Mutex::new(());
async fn state_set(Json(req): Json<StateReq>) -> Result<StatusCode, ApiErr> {
    let _g = STATE_LOCK.lock().unwrap();
    let mut all = state_all();
    all.insert(req.root, req.state);
    let f = state_file();
    let tmp = f.with_extension("json.tmp");
    std::fs::create_dir_all(f.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec_pretty(&serde_json::Value::Object(all)).unwrap()))
        .and_then(|_| std::fs::rename(&tmp, &f))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

fn have(tool: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file())).unwrap_or(false)
}

#[derive(Deserialize)]
struct ResetReq {
    /// Delete everything in the vault for good.
    #[serde(default)]
    vault: bool,
    /// Forget the last maps (snapshots).
    #[serde(default)]
    snapshots: bool,
}
/// Start over: forget every root's remembered state; optionally purge the vault and the snapshots. Settings (the AI choice) stay.
async fn reset(State(app): State<Shared>, Json(req): Json<ResetReq>) -> Result<Json<serde_json::Value>, ApiErr> {
    let _g = STATE_LOCK.lock().unwrap();
    let _ = std::fs::remove_file(state_file());
    let mut purged = 0;
    if req.vault {
        for e in vault::list() { if vault::purge(&e.id).is_ok() { purged += 1; } }
        if let Ok(rd) = std::fs::read_dir(vault::dir()) { for e in rd.flatten() { if e.path().is_dir() { let _ = std::fs::remove_dir_all(e.path()); } } } // orphans from interrupted moves
    }
    if req.snapshots {
        let _ = std::fs::remove_dir_all(vault::dir().parent().unwrap().join("snapshots"));
    }
    drop(_g);
    let _ = &app;
    Ok(Json(serde_json::json!({ "purged": purged })))
}
