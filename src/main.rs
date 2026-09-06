mod gunk;
mod hog;
mod scan;

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
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};

enum ScanState {
    Idle,
    Scanning(PathBuf, Arc<scan::Progress>),
    Done { root: PathBuf, tree: scan::Node },
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
}
type Shared = Arc<App>;
type ApiErr = (StatusCode, String);

fn bad(msg: impl Into<String>) -> ApiErr {
    (StatusCode::BAD_REQUEST, msg.into())
}

#[tokio::main]
async fn main() {
    let app = Arc::new(App {
        scan: Mutex::new(ScanState::Idle),
        hog: hog::Hog::new(),
        version: AtomicU64::new(0),
        cands: Mutex::new((u64::MAX, Arc::new(vec![]))),
        pending: Mutex::new(HashSet::new()),
        watcher: Mutex::new(None),
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
        if let ScanState::Done { root, tree } = &mut *s {
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
        .route("/api/status", get(status))
        .route("/api/tree", get(tree))
        .route("/api/gunk", get(gunk_list))
        .route("/api/summary", get(summary))
        .route("/api/idle", get(idle))
        .route("/api/procs", get(procs))
        .route("/api/procfiles", get(procfiles))
        .route("/api/open", post(open_path))
        .route("/api/reveal", post(reveal))
        .fallback_service(ServeDir::new("static"))
        // static files change while developing; make every reload re-check them
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-cache")))
        .with_state(app);
    let addr = "127.0.0.1:4242";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("DuMe → http://{addr}");
    let _ = std::process::Command::new("open").arg(format!("http://{addr}")).spawn();
    axum::serve(listener, router).await.unwrap();
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
        let tree = scan::scan(&root, &progress);
        *app2.watcher.lock().unwrap() = watch(&app2, &root);
        app2.pending.lock().unwrap().clear();
        *app2.scan.lock().unwrap() = ScanState::Done { root, tree };
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
    done: bool,
}

async fn status(State(app): State<Shared>) -> Json<serde_json::Value> {
    let s = app.scan.lock().unwrap();
    let (state, root) = match &*s {
        ScanState::Idle => ("idle", None),
        ScanState::Scanning(r, _) => ("scanning", Some(r)),
        ScanState::Done { root, .. } => ("done", Some(root)),
    };
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
                    done: i.done.load(Ordering::Relaxed),
                })
                .collect();
            let files = live.iter().map(|i| i.files).sum();
            let size = live.iter().map(|i| i.size).sum();
            live.sort_unstable_by(|a, b| b.size.cmp(&a.size));
            live.truncate(60);
            (files, size, Some(live))
        }
        ScanState::Done { tree, .. } => (tree.files, tree.size, None),
        _ => (0, 0, None),
    };
    Json(serde_json::json!({
        "state": state, "root": root, "files": files, "size": size, "live": live,
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
        ScanState::Done { root, tree } => f(root, tree),
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
struct GunkQ {
    #[serde(default)]
    path: String,
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
    Ok(Json(gunk::under(&all, &q.path).take(300).cloned().collect()))
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
