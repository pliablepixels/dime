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
    /// Bytes per file type so far, so the live map can wear real colours.
    pub types: [AtomicU64; 10],
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
                            types: Default::default(),
                            done: AtomicBool::new(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Progress { items }
    }
}

/// Directories macOS itself refused, which is what Full Disk Access fixes. Reset at the start of
/// every scan and read once it finishes. One scan at a time, so a single counter is enough.
static DENIED: AtomicU64 = AtomicU64::new(0);
pub fn denied() -> u64 {
    DENIED.load(Ordering::Relaxed)
}

/// Does this process already hold Full Disk Access? The privacy database is the canonical probe:
/// nothing but that grant opens it.
///
/// Worth asking because a refusal alone does not mean the grant is missing. macOS also seals system
/// data vaults (`/private/var/db`, the sandbox caches under `/private/var/folders`, Apple's model
/// assets) that stay shut for everyone, granted or not. Suggesting Full Disk Access for those would
/// send the user to a settings pane that cannot help them.
pub fn full_disk_access() -> bool {
    let Ok(home) = std::env::var("HOME") else { return true };
    match fs::File::open(Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db")) {
        Ok(_) => true,
        // missing or unreadable for any other reason: assume the grant is fine rather than nag
        Err(e) => e.raw_os_error() != Some(libc::EPERM),
    }
}

/// Did macOS refuse this, or is it just an ordinary directory we have no rights to?
///
/// Rust reports both as `PermissionDenied`, but they need different advice: privacy refusals come
/// back as EPERM and Full Disk Access opens them, while EACCES means the mode bits or the owner say
/// no and no permission DiMe can be granted will change that. Only EPERM is worth mentioning.
fn privacy_refusal(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPERM)
}

/// Folders a scan never enters: system mounts under "/", and DiMe's own vault (moving something there must not just move it on the map).
pub fn skip_list(root: &Path) -> Vec<PathBuf> {
    let mut skip: Vec<PathBuf> = if root == Path::new("/") { ["/dev", "/Volumes", "/System/Volumes", "/private/var/vm", "/proc"].iter().map(PathBuf::from).collect() } else { vec![] };
    if let Ok(h) = std::env::var("HOME") {
        skip.push(PathBuf::from(h).join(".dime"));
    }
    skip
}
pub fn scan(root: &Path, progress: &Progress) -> Node {
    DENIED.store(0, Ordering::Relaxed);
    let skip = skip_list(root);
    let own = fs::symlink_metadata(root).ok();
    let (mut mtime, mut atime) = own.as_ref().map(|m| (m.mtime(), m.atime())).unwrap_or((0, 0));
    let dev = own.as_ref().map(|m| m.dev()).unwrap_or(0);
    let mut children: Vec<Node> = progress
        .items
        .par_iter()
        .filter_map(|it| {
            let p = root.join(&it.name);
            let node = if it.is_dir {
                if skip.iter().any(|s| s == &p) || fs::symlink_metadata(&p).map(|m| m.dev() != dev).unwrap_or(true) {
                    it.done.store(true, Ordering::Relaxed); // skipped, but finished as far as the live map is concerned
                    return None;
                }
                scan_dir(&p, &it.size, &it.files, &it.types, dev, &skip)
            } else {
                let md = fs::symlink_metadata(&p).ok()?;
                let n = file_node(&p, &md);
                it.size.store(n.size, Ordering::Relaxed);
                it.files.store(1, Ordering::Relaxed);
                it.types[type_of(&n.name)].store(n.size, Ordering::Relaxed);
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

/// One directory entry as `getattrlistbulk` hands it back: no per-file stat, no path building.
struct Ent {
    name: String,
    kind: u32, // VREG 1, VDIR 2, VLNK 5, else other
    size: u64,
    mtime: i64,
    atime: i64,
    dev: u64,
}
/// Read a whole directory with macOS's bulk attribute call: name, type, times, allocated size and device for every entry,
/// a few hundred entries per syscall. Errors fall back to the readdir + lstat walk.
fn bulk_entries(path: &Path) -> std::io::Result<Vec<Ent>> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::Error::other("nul in path"))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME | libc::ATTR_CMN_DEVID | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_MODTIME | libc::ATTR_CMN_ACCTIME;
    al.fileattr = libc::ATTR_FILE_ALLOCSIZE;
    thread_local! { static BUF: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(vec![0u8; 128 * 1024]); } // one buffer per rayon thread, never re-zeroed
    let mut out = Vec::new();
    BUF.with(|b| -> std::io::Result<()> {
    let mut buf = b.borrow_mut();
    let rd32 = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let rd64 = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let fd_guard = fd;
    loop {
        let n = unsafe { libc::getattrlistbulk(fd, &mut al as *mut libc::attrlist as *mut libc::c_void, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), libc::FSOPT_PACK_INVAL_ATTRS as u64) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd_guard) };
            return Err(e);
        }
        if n == 0 {
            break;
        }
        let buf: &[u8] = &buf;
        let mut off = 0usize;
        for _ in 0..n {
            let start = off;
            let len = rd32(&buf, off) as usize;
            let mut p = off + 4;
            // returned attribute set: which of the requested attributes are actually present (5 x u32)
            let ret_common = rd32(&buf, p);
            let ret_file = rd32(&buf, p + 12); // attribute_set_t: common, vol, dir, file, fork
            p += 20;
            let mut name = String::new();
            if ret_common & libc::ATTR_CMN_NAME != 0 {
                let (ro, rl) = (rd32(&buf, p) as i32 as isize, rd32(&buf, p + 4) as usize);
                let s = (p as isize + ro) as usize;
                name = String::from_utf8_lossy(&buf[s..s + rl.saturating_sub(1)]).into_owned();
                p += 8;
            }
            let mut dev = 0;
            if ret_common & libc::ATTR_CMN_DEVID != 0 { dev = rd32(&buf, p) as u64; p += 4; }
            let mut kind = 0;
            if ret_common & libc::ATTR_CMN_OBJTYPE != 0 { kind = rd32(&buf, p); p += 4; }
            let mut mtime = 0;
            if ret_common & libc::ATTR_CMN_MODTIME != 0 { mtime = rd64(&buf, p) as i64; p += 16; }
            let mut atime = 0;
            if ret_common & libc::ATTR_CMN_ACCTIME != 0 { atime = rd64(&buf, p) as i64; p += 16; }
            let mut size = 0;
            if ret_file & libc::ATTR_FILE_ALLOCSIZE != 0 { size = rd64(&buf, p); }
            out.push(Ent { name, kind, size, mtime, atime, dev });
            off = start + len;
        }
    }
    Ok(())
    })?;
    unsafe { libc::close(fd) };
    Ok(out)
}

fn scan_dir(path: &Path, bytes: &AtomicU64, counter: &AtomicU64, types: &[AtomicU64; 10], dev: u64, skip: &[PathBuf]) -> Node {
    if std::env::var_os("DIME_SLOW_SCAN").is_some() { return scan_dir_slow(path, bytes, counter, types, dev, skip); }
    let Ok(ents) = bulk_entries(path) else { return scan_dir_slow(path, bytes, counter, types, dev, skip) };
    let own = fs::symlink_metadata(path).ok();
    let (mut mtime, mut atime) = own.as_ref().map(|m| (m.mtime(), m.atime())).unwrap_or((0, 0));
    let mut children: Vec<Node> = Vec::with_capacity(ents.len());
    let mut dirs: Vec<String> = Vec::new();
    for e in ents {
        match e.kind {
            2 => { if e.dev == dev && !skip.iter().any(|s| s == &path.join(&e.name)) { dirs.push(e.name); } } // another volume mounted here: not this disk's bytes
            1 => {
                counter.fetch_add(1, Ordering::Relaxed);
                bytes.fetch_add(e.size, Ordering::Relaxed);
                types[type_of(&e.name)].fetch_add(e.size, Ordering::Relaxed);
                children.push(Node { name: e.name, size: e.size, mtime: e.mtime, atime: e.atime, is_dir: false, files: 1, children: vec![] });
            }
            _ => {} // symlinks, sockets, devices
        }
    }
    let sub: Vec<Node> = if dirs.len() > 1 {
        dirs.into_par_iter().map(|d| scan_dir(&path.join(d), bytes, counter, types, dev, skip)).collect()
    } else {
        dirs.into_iter().map(|d| scan_dir(&path.join(d), bytes, counter, types, dev, skip)).collect()
    };
    children.extend(sub);
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

/// The portable walk: readdir plus one lstat per entry. Used when the bulk call is refused (some network and FUSE volumes).
fn scan_dir_slow(path: &Path, bytes: &AtomicU64, counter: &AtomicU64, types: &[AtomicU64; 10], dev: u64, skip: &[PathBuf]) -> Node {
    let own = fs::symlink_metadata(path).ok();
    let (mut mtime, mut atime) = own.as_ref().map(|m| (m.mtime(), m.atime())).unwrap_or((0, 0));
    let entries: Vec<fs::DirEntry> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(e) => {
            // the bulk walk falls back to this one, so every refused directory passes through here
            if privacy_refusal(&e) {
                DENIED.fetch_add(1, Ordering::Relaxed);
            }
            vec![]
        }
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
                if skip.iter().any(|s| s == &p) || md.dev() != dev {
                    return None; // another volume mounted here (a simulator runtime image, a network share): not this disk's bytes
                }
                return Some(scan_dir_slow(&p, bytes, counter, types, dev, skip));
            }
            let n = file_node(&p, &md);
            counter.fetch_add(1, Ordering::Relaxed);
            bytes.fetch_add(n.size, Ordering::Relaxed);
            types[type_of(&n.name)].fetch_add(n.size, Ordering::Relaxed);
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
    fn go(n: &mut Node, parts: &[String], abs: &Path, root_dev: u64, skip: &[PathBuf]) -> (i64, i64) {
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
            (Some(i), _) => go(&mut n.children[i], &parts[1..], abs, root_dev, skip),
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
                if md.is_dir() && (md.dev() != root_dev || skip.iter().any(|s| s == &p)) { return (0, 0); } // a mount or a skipped folder appearing: not this disk's bytes
                let new = if md.is_dir() { scan_dir(&p, &AtomicU64::new(0), &AtomicU64::new(0), &Default::default(), root_dev, skip) } else { file_node(&p, &md) };
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
    let root_dev = fs::symlink_metadata(root).map(|m| m.dev()).unwrap_or(0);
    go(root_node, &parts, abs, root_dev, &skip_list(root));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the Full Disk Access banner: only macOS's own refusals are worth showing,
    /// because those are the ones the user can do something about.
    #[test]
    fn only_privacy_refusals_count() {
        let perm = std::io::Error::from_raw_os_error(libc::EPERM); // macOS privacy: "Operation not permitted"
        let acces = std::io::Error::from_raw_os_error(libc::EACCES); // mode bits: "Permission denied"
        assert_eq!(perm.kind(), acces.kind(), "Rust flattens both to PermissionDenied, hence this check");
        assert!(privacy_refusal(&perm));
        assert!(!privacy_refusal(&acces));
        assert!(!privacy_refusal(&std::io::Error::from_raw_os_error(libc::ENOENT)));
    }
}
