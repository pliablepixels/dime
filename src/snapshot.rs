//! Snapshots: the finished tree written to ~/.dime/snapshots so the next launch opens the last map
//! at once instead of rescanning. Plain length-prefixed binary, no dependencies, ~30 bytes a node.
use crate::scan::{join, Node};
use serde::Serialize;
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 5] = b"DIME1";

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".dime").join("snapshots")
}
fn file_for(root: &Path) -> PathBuf {
    // FNV-1a of the root path: stable, short, and one file per root
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    dir().join(format!("{h:016x}.dime"))
}
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[derive(Serialize, Clone)]
pub struct Meta {
    pub root: String,
    pub at: i64,
    pub size: u64,
    pub files: u64,
}

fn w_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    w.write_all(&(s.len() as u32).to_le_bytes())?;
    w.write_all(s.as_bytes())
}
fn r_str(r: &mut impl Read) -> io::Result<String> {
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_le_bytes(n) as usize;
    if n > 1 << 20 {
        return Err(io::Error::other("corrupt snapshot"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    String::from_utf8(b).map_err(io::Error::other)
}
fn r_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn w_node(w: &mut impl Write, n: &Node) -> io::Result<()> {
    w_str(w, &n.name)?;
    w.write_all(&n.size.to_le_bytes())?;
    w.write_all(&(n.mtime as u64).to_le_bytes())?;
    w.write_all(&(n.atime as u64).to_le_bytes())?;
    w.write_all(&n.files.to_le_bytes())?;
    // bit 0 is_dir, bit 1 shared. Older snapshots wrote only bit 0, so they read back as not
    // shared, and an older DiMe reading a newer snapshot still sees bit 0 exactly as before.
    w.write_all(&[n.is_dir as u8 | (n.shared as u8) << 1])?;
    w.write_all(&(n.children.len() as u32).to_le_bytes())?;
    for c in &n.children {
        w_node(w, c)?;
    }
    Ok(())
}
fn r_node(r: &mut impl Read) -> io::Result<Node> {
    let name = r_str(r)?;
    let size = r_u64(r)?;
    let mtime = r_u64(r)? as i64;
    let atime = r_u64(r)? as i64;
    let files = r_u64(r)?;
    let mut flag = [0u8; 1];
    r.read_exact(&mut flag)?;
    let mut cnt = [0u8; 4];
    r.read_exact(&mut cnt)?;
    let cnt = u32::from_le_bytes(cnt) as usize;
    let mut children = Vec::with_capacity(cnt.min(1 << 16));
    for _ in 0..cnt {
        children.push(r_node(r)?);
    }
    Ok(Node { name, size, mtime, atime, is_dir: flag[0] & 1 == 1, shared: flag[0] & 2 != 0, files, children })
}

pub fn save(root: &Path, tree: &Node) -> io::Result<Meta> {
    fs::create_dir_all(dir())?;
    let f = file_for(root);
    let tmp = f.with_extension("tmp");
    let at = now();
    {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w_str(&mut w, &root.to_string_lossy())?;
        w.write_all(&(at as u64).to_le_bytes())?;
        w_node(&mut w, tree)?;
        w.flush()?;
    }
    fs::rename(&tmp, &f)?;
    Ok(Meta { root: root.to_string_lossy().into_owned(), at, size: tree.size, files: tree.files })
}

fn open(f: &Path) -> io::Result<(BufReader<fs::File>, Meta)> {
    let mut r = BufReader::new(fs::File::open(f)?);
    let mut magic = [0u8; 5];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::other("not a snapshot"));
    }
    let root = r_str(&mut r)?;
    let at = r_u64(&mut r)? as i64;
    // read just the root node's header for size and files; the tree stays on disk
    let _name = r_str(&mut r)?;
    let size = r_u64(&mut r)?;
    let _mtime = r_u64(&mut r)?;
    let _atime = r_u64(&mut r)?;
    let files = r_u64(&mut r)?;
    Ok((r, Meta { root, at, size, files }))
}

/// Every snapshot on disk, newest first.
pub fn list() -> Vec<Meta> {
    let mut out: Vec<Meta> = fs::read_dir(dir()).map(|rd| rd.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "dime")).filter_map(|e| open(&e.path()).ok().map(|(_, m)| m)).collect()).unwrap_or_default();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

#[derive(Serialize)]
pub struct Change {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// Size now. Zero means it is gone.
    pub size: u64,
    /// Size when the snapshot was taken. Zero means it is new.
    pub was: u64,
    /// now minus was, so growth is positive.
    pub delta: i64,
}

/// What changed between the saved map and the one in memory, biggest movers first. Compared at the
/// shallowest level that tells the story: a folder that grew is reported once, not as every file
/// inside it, unless the change is spread across several of its children.
pub fn changes(old: &Node, new: &Node, limit: usize) -> Vec<Change> {
    let mut out = vec![];
    fn walk(old: Option<&Node>, new: Option<&Node>, path: &str, depth: usize, out: &mut Vec<Change>) {
        let (was, size) = (old.map_or(0, |n| n.size), new.map_or(0, |n| n.size));
        let delta = size as i64 - was as i64;
        if delta.unsigned_abs() < (16 << 20) {
            return; // below 16 MB is noise on any real disk
        }
        let node = new.or(old).unwrap();
        // keep descending while one child accounts for nearly all of the change, so the answer is
        // "Xcode's DerivedData grew" rather than "your home grew"
        if depth < 6 {
            if let Some(n) = new {
                let mut best: Option<(&Node, Option<&Node>, i64)> = None;
                for c in &n.children {
                    let o = old.and_then(|x| x.children.iter().find(|k| k.name == c.name));
                    let d = c.size as i64 - o.map_or(0, |k| k.size) as i64;
                    if best.is_none_or(|(_, _, bd)| d.abs() > bd.abs()) {
                        best = Some((c, o, d));
                    }
                }
                if let Some((c, o, d)) = best {
                    if d.abs() as f64 > delta.abs() as f64 * 0.8 {
                        walk(o, Some(c), &join(path, &c.name), depth + 1, out);
                        return;
                    }
                }
            }
        }
        out.push(Change { path: path.to_string(), name: node.name.clone(), is_dir: node.is_dir, size, was, delta });
    }
    for c in &new.children {
        let o = old.children.iter().find(|k| k.name == c.name);
        walk(o, Some(c), &c.name, 1, &mut out);
    }
    for c in &old.children {
        if !new.children.iter().any(|k| k.name == c.name) {
            walk(Some(c), None, &c.name, 1, &mut out);
        }
    }
    out.sort_by_key(|c| -c.delta.abs());
    out.truncate(limit);
    out
}

pub fn load(root: &Path) -> io::Result<(Meta, Node)> {
    let f = file_for(root);
    let mut r = BufReader::new(fs::File::open(&f)?);
    let mut magic = [0u8; 5];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::other("not a snapshot"));
    }
    let root_s = r_str(&mut r)?;
    let at = r_u64(&mut r)? as i64;
    let tree = r_node(&mut r)?;
    Ok((Meta { root: root_s, at, size: tree.size, files: tree.files }, tree))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let leaf = |n: &str, s: u64| Node { name: n.into(), size: s, mtime: 1, atime: 2, is_dir: false, files: 1, shared: false, children: vec![] };
        let tree = Node { name: "r".into(), size: 30, mtime: 5, atime: 6, is_dir: true, files: 2, shared: false, children: vec![Node { name: "d".into(), size: 30, mtime: 3, atime: 4, is_dir: true, files: 2, shared: false, children: vec![leaf("a", 10), leaf("bé", 20)] }] };
        let root = std::env::temp_dir().join(format!("dime-snap-{}", std::process::id()));
        let m = save(&root, &tree).unwrap();
        assert_eq!((m.size, m.files), (30, 2));
        let (m2, t2) = load(&root).unwrap();
        assert_eq!(m2.root, root.to_string_lossy());
        assert_eq!(t2.children[0].children[1].name, "bé");
        assert_eq!(t2.children[0].children[1].size, 20);
        assert!(list().iter().any(|x| x.root == m2.root && x.size == 30 && x.files == 2));
        fs::remove_file(file_for(&root)).unwrap();
    }
}
