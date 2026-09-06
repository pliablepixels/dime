use rayon::prelude::*;
use serde::Serialize;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub atime: i64,
    pub is_dir: bool,
    pub files: u64,
    #[serde(skip)]
    pub children: Vec<Node>,
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Live counters for each top-level entry, so the UI can draw the map while the scan runs.
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub size: AtomicU64,
    pub files: AtomicU64,
    pub done: AtomicBool,
}

pub struct Progress {
    pub items: Vec<Item>,
}

impl Progress {
    pub fn new(root: &Path) -> Progress {
        let items = fs::read_dir(root)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter_map(|e| {
                        let ft = e.file_type().ok()?;
                        if ft.is_symlink() {
                            return None;
                        }
                        Some(Item {
                            name: name_of(&e.path()),
                            is_dir: ft.is_dir(),
                            size: AtomicU64::new(0),
                            files: AtomicU64::new(0),
                            done: AtomicBool::new(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Progress { items }
    }
}

pub fn scan(root: &Path, progress: &Progress) -> Node {
    let skip: Vec<PathBuf> = if root == Path::new("/") {
        ["/dev", "/Volumes", "/System/Volumes", "/private/var/vm", "/proc"]
            .iter()
            .map(PathBuf::from)
            .collect()
    } else {
        vec![]
    };
    let own = fs::symlink_metadata(root).ok();
    let (mut mtime, mut atime) = own.as_ref().map(|m| (m.mtime(), m.atime())).unwrap_or((0, 0));
    let mut children: Vec<Node> = progress
        .items
        .par_iter()
        .filter_map(|it| {
            let p = root.join(&it.name);
            let node = if it.is_dir {
                if skip.iter().any(|s| s == &p) {
                    return None;
                }
                scan_dir(&p, &it.size, &it.files, &skip)
            } else {
                let md = fs::symlink_metadata(&p).ok()?;
                let n = file_node(&p, &md);
                it.size.store(n.size, Ordering::Relaxed);
                it.files.store(1, Ordering::Relaxed);
                n
            };
            it.done.store(true, Ordering::Relaxed);
            Some(node)
        })
        .collect();
    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    let mut size = 0;
    let mut files = 0;
    for c in &children {
        size += c.size;
        files += c.files;
        mtime = mtime.max(c.mtime);
        atime = atime.max(c.atime);
    }
    Node { name: name_of(root), size, mtime, atime, is_dir: true, files, children }
}

fn file_node(p: &Path, md: &fs::Metadata) -> Node {
    Node {
        name: name_of(p),
        size: md.blocks() * 512,
        mtime: md.mtime(),
        atime: md.atime(),
        is_dir: false,
        files: 1,
        children: vec![],
    }
}

fn scan_dir(path: &Path, bytes: &AtomicU64, counter: &AtomicU64, skip: &[PathBuf]) -> Node {
    let own = fs::symlink_metadata(path).ok();
    let (mut mtime, mut atime) = own.as_ref().map(|m| (m.mtime(), m.atime())).unwrap_or((0, 0));
    let entries: Vec<fs::DirEntry> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(_) => vec![],
    };
    let mut children: Vec<Node> = entries
        .into_par_iter()
        .filter_map(|e| {
            let p = e.path();
            let md = fs::symlink_metadata(&p).ok()?;
            let ft = md.file_type();
            if ft.is_symlink() {
                return None;
            }
            if ft.is_dir() {
                if skip.iter().any(|s| s == &p) {
                    return None;
                }
                return Some(scan_dir(&p, bytes, counter, skip));
            }
            let n = file_node(&p, &md);
            counter.fetch_add(1, Ordering::Relaxed);
            bytes.fetch_add(n.size, Ordering::Relaxed);
            Some(n)
        })
        .collect();
    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    let mut size = 0;
    let mut files = 0;
    for c in &children {
        size += c.size;
        files += c.files;
        mtime = mtime.max(c.mtime);
        atime = atime.max(c.atime);
    }
    Node { name: name_of(path), size, mtime, atime, is_dir: true, files, children }
}

pub fn get<'a>(root: &'a Node, rel: &str) -> Option<&'a Node> {
    let mut cur = root;
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        cur = cur.children.iter().find(|c| c.name == part)?;
    }
    Some(cur)
}

/// Apply one filesystem change at `abs` to the tree: re-stat the path, then replace, insert, or
/// remove its node and roll the size/file delta up through every ancestor.
pub fn patch(root_node: &mut Node, root: &Path, abs: &Path) {
    let Ok(rel) = abs.strip_prefix(root) else { return };
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if parts.is_empty() {
        return;
    }
    fn go(n: &mut Node, parts: &[String], abs: &Path) -> (i64, i64) {
        let idx = n.children.iter().position(|c| c.name == parts[0]);
        let delta = match (idx, parts.len()) {
            (Some(i), 1) => {
                let old = &n.children[i];
                match fs::symlink_metadata(abs) {
                    Err(_) => {
                        let c = n.children.remove(i);
                        (-(c.size as i64), -(c.files as i64))
                    }
                    Ok(md) if md.is_dir() => {
                        // the entries inside get their own events; only refresh the dir's own times
                        let c = &mut n.children[i];
                        c.mtime = c.mtime.max(md.mtime());
                        c.atime = c.atime.max(md.atime());
                        (0, 0)
                    }
                    Ok(md) if md.file_type().is_symlink() => (0, 0),
                    Ok(md) => {
                        let new = file_node(abs, &md);
                        let d = (new.size as i64 - old.size as i64, new.files as i64 - old.files as i64);
                        n.children[i] = new;
                        d
                    }
                }
            }
            (Some(i), _) => go(&mut n.children[i], &parts[1..], abs),
            (None, _) => {
                // first missing component: stat what exists at that depth and insert it whole
                let mut p = abs.to_path_buf();
                for _ in 1..parts.len() {
                    p.pop();
                }
                let Ok(md) = fs::symlink_metadata(&p) else { return (0, 0) };
                if md.file_type().is_symlink() {
                    return (0, 0);
                }
                let new = if md.is_dir() { scan_dir(&p, &AtomicU64::new(0), &AtomicU64::new(0), &[]) } else { file_node(&p, &md) };
                let d = (new.size as i64, new.files as i64);
                n.children.push(new);
                d
            }
        };
        if delta != (0, 0) {
            n.size = (n.size as i64 + delta.0).max(0) as u64;
            n.files = (n.files as i64 + delta.1).max(0) as u64;
            n.children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        }
        delta
    }
    go(root_node, &parts, abs);
}

/// Bytes per file type: code, images, video, audio, documents, archives, data, models, apps, other.
pub type Types = [u64; 10];

pub fn type_of(name: &str) -> usize {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "py" | "go" | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "rb" | "php" | "sh" | "zsh"
        | "html" | "css" | "scss" | "vue" | "svelte" | "sql" | "lua" | "dart" | "m" | "mm" | "pl" | "r" | "scala" | "toml" | "yaml" | "yml" | "lock" | "map" | "wasm" => 0,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tiff" | "tif" | "bmp" | "svg" | "psd" | "raw" | "cr2" | "arw" | "dng" | "ico" | "icns" => 1,
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" => 2,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "aiff" | "aif" | "wma" | "opus" => 3,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "key" | "pages" | "numbers" | "txt" | "md" | "rtf" | "epub" | "mobi" | "odt" | "tex" => 4,
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "rar" | "dmg" | "pkg" | "iso" | "jar" | "whl" | "deb" | "rpm" | "ipa" | "apk" | "xip" => 5,
        "json" | "jsonl" | "csv" | "tsv" | "xml" | "plist" | "db" | "sqlite" | "sqlite3" | "ibd" | "frm" | "log" | "parquet" | "arrow" | "avro" | "ndjson" | "pb" | "cache" | "idx" | "pack" => 6,
        "safetensors" | "gguf" | "ggml" | "pt" | "pth" | "ckpt" | "onnx" | "h5" | "npy" | "npz" | "tflite" | "mlmodel" | "mlmodelc" | "bin" => 7,
        "app" | "dylib" | "so" | "dll" | "exe" | "framework" | "bundle" | "a" | "o" | "rlib" | "rmeta" | "class" | "pyc" | "node" | "kext" | "xcarchive" => 8,
        _ if name.starts_with("sha256") || name.starts_with("blobs") => 7,
        _ => 9,
    }
}

#[derive(Serialize)]
pub struct Out {
    #[serde(flatten)]
    pub node: Node,
    pub path: String,
    /// Bytes per file type, indexed like `TYPES`.
    pub types: Types,
    /// Bytes of files last touched on or before the requested cutoff (only when asked for).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_size: Option<u64>,
    pub children: Vec<Out>,
}

/// One walk of the subtree collecting both type bytes and idle bytes.
fn tally(n: &Node, cutoff: Option<i64>) -> (Types, u64) {
    let mut t = [0u64; 10];
    let mut idle = 0;
    fn go(n: &Node, cutoff: Option<i64>, t: &mut Types, idle: &mut u64) {
        for c in &n.children {
            if c.is_dir {
                go(c, cutoff, t, idle);
            } else {
                t[type_of(&c.name)] += c.size;
                if cutoff.is_some_and(|k| c.atime.max(c.mtime) <= k) {
                    *idle += c.size;
                }
            }
        }
    }
    if n.is_dir {
        go(n, cutoff, &mut t, &mut idle);
    } else {
        t[type_of(&n.name)] = n.size;
        if cutoff.is_some_and(|k| n.atime.max(n.mtime) <= k) {
            idle = n.size;
        }
    }
    (t, idle)
}

const MAX_CHILDREN: usize = 60;

#[derive(Serialize)]
pub struct IdleFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub age_days: i64,
}

/// Largest files under `n` not touched since `cutoff`, biggest first, capped at `limit`.
pub fn idle_files(n: &Node, path: &str, cutoff: i64, now: i64, limit: usize) -> Vec<IdleFile> {
    fn go(n: &Node, path: &str, cutoff: i64, now: i64, out: &mut Vec<IdleFile>) {
        for c in &n.children {
            let p = join(path, &c.name);
            if c.is_dir {
                go(c, &p, cutoff, now, out);
            } else if c.size >= 1 << 20 && c.atime.max(c.mtime) <= cutoff {
                out.push(IdleFile { path: p, name: c.name.clone(), size: c.size, age_days: (now - c.atime.max(c.mtime)) / 86_400 });
            }
        }
    }
    let mut out = vec![];
    go(n, path, cutoff, now, &mut out);
    out.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    out.truncate(limit);
    out
}

pub fn subtree(n: &Node, path: &str, depth: u32, cutoff: Option<i64>) -> Out {
    if depth == 0 || !n.is_dir {
        let (types, idle) = tally(n, cutoff);
        return Out { node: n.clone_shallow(), path: path.to_string(), types, idle_size: cutoff.map(|_| idle), children: vec![] };
    }
    let mut types = [0u64; 10];
    let mut idle = 0;
    let mut children: Vec<Out> = n
        .children
        .iter()
        .take(MAX_CHILDREN)
        .map(|c| {
            let o = subtree(c, &join(path, &c.name), depth - 1, cutoff);
            for i in 0..10 {
                types[i] += o.types[i];
            }
            idle += o.idle_size.unwrap_or(0);
            o
        })
        .collect();
    let rest = &n.children[n.children.len().min(MAX_CHILDREN)..];
    if !rest.is_empty() {
        let mut rt = [0u64; 10];
        let mut ri = 0;
        for c in rest {
            let (t, i) = tally(c, cutoff);
            for k in 0..10 {
                rt[k] += t[k];
            }
            ri += i;
        }
        for k in 0..10 {
            types[k] += rt[k];
        }
        idle += ri;
        children.push(Out {
            node: Node { name: format!("… {} more", rest.len()), size: rest.iter().map(|c| c.size).sum(), mtime: 0, atime: 0, is_dir: false, files: rest.iter().map(|c| c.files).sum(), children: vec![] },
            path: String::new(),
            types: rt,
            idle_size: cutoff.map(|_| ri),
            children: vec![],
        });
    }
    Out { node: n.clone_shallow(), path: path.to_string(), types, idle_size: cutoff.map(|_| idle), children }
}

impl Node {
    fn clone_shallow(&self) -> Node {
        Node { children: vec![], name: self.name.clone(), ..*self }
    }
}

pub fn join(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{a}/{b}") }
}
