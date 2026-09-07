//! Finds removal candidates in the scanned tree and explains each one in plain language.
//! Every candidate gets a tier: `safe` (regenerated automatically), `likely` (usually fine
//! once you glance at it), `review` (big or old, your call).
use crate::scan::{join, Node};
use serde::Serialize;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_DIRS: &[&str] = &[
    "node_modules", "target", ".venv", "venv", "__pycache__", ".gradle", ".npm", ".pnpm-store", ".m2",
    "Pods", ".next", ".turbo", ".tox", ".mypy_cache", ".pytest_cache", ".parcel-cache", ".nuxt",
    "DerivedData", ".cargo-cache", "bower_components", ".dart_tool", ".angular",
];
const BUILD_DIRS: &[&str] = &["build", "dist", "out", ".build"];
const APP_CACHE_DIRS: &[&str] = &["Caches", ".cache", "CachedData", "Code Cache", "GPUCache", "Cache", "ShaderCache"];
const SIM_DIRS: &[&str] = &["CoreSimulator", "iOS DeviceSupport", "watchOS DeviceSupport", "tvOS DeviceSupport", "visionOS DeviceSupport"];
const PROJECT_MARKERS: &[&str] = &["package.json", "Cargo.toml", "pyproject.toml", "go.mod", "Gemfile", "pom.xml", "build.gradle", "Package.swift", ".git"];
const INSTALLER_EXT: &[&str] = &["dmg", "pkg", "iso", "zip", "tgz", "gz", "xz", "bz2", "7z", "rar", "msi", "exe", "app.zip", "ipa", "apk"];
const DISK_IMAGE_EXT: &[&str] = &["qcow2", "vmdk", "vdi", "vhd", "vhdx", "img", "raw", "sparseimage", "sparsebundle"];
const MB: u64 = 1 << 20;
const DAY: i64 = 86_400;

#[derive(Serialize, Clone)]
pub struct Candidate {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// kind id, e.g. "cache", "downloads", "large"
    pub reason: &'static str,
    pub tier: &'static str,
    pub what: &'static str,
    pub note: String,
    pub age_days: i64,
    pub is_dir: bool,
    pub score: f64,
    /// Set in the idle view: `size` is then the idle bytes inside, and this is the whole item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_size: Option<u64>,
}

#[derive(Serialize)]
pub struct Summary {
    pub total: u64,
    pub tiers: Vec<(&'static str, u64, usize)>,
    pub kinds: Vec<(&'static str, &'static str, &'static str, u64, usize)>,
}

struct Walk {
    now: i64,
    out: Vec<Candidate>,
    /// (size, name) -> first path seen, for cheap duplicate detection of big files
    seen: HashMap<(u64, String), (String, i64)>,
}

fn ext(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}
fn months(days: i64) -> String {
    if days < 60 { format!("{days} days") } else if days < 730 { format!("{} months", days / 30) } else { format!("{:.1} years", days as f64 / 365.0) }
}

/// All candidates in the tree, best first. Compute once per tree version; filter by prefix per request.
pub fn find_all(root: &Node) -> Vec<Candidate> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut w = Walk { now, out: vec![], seen: HashMap::new() };
    walk(&mut w, root, "", false);
    w.out.sort_by(|a, b| b.score.total_cmp(&a.score));
    w.out
}

pub fn under<'a>(all: &'a [Candidate], prefix: &str) -> impl Iterator<Item = &'a Candidate> + 'a {
    let p = prefix.trim_matches('/').to_string();
    all.iter().filter(move |c| p.is_empty() || c.path == p || c.path.starts_with(&format!("{p}/")))
}

pub fn summary<'a>(cands: impl Iterator<Item = &'a Candidate>) -> Summary {
    let mut tiers: Vec<(&str, u64, usize)> = vec![("safe", 0, 0), ("likely", 0, 0), ("review", 0, 0)];
    let mut kinds: HashMap<&'static str, (&'static str, &'static str, u64, usize)> = HashMap::new();
    let mut total = 0;
    for c in cands {
        total += c.size;
        let t = tiers.iter_mut().find(|t| t.0 == c.tier).unwrap();
        t.1 += c.size;
        t.2 += 1;
        let k = kinds.entry(c.reason).or_insert((c.tier, c.what, 0, 0));
        k.2 += c.size;
        k.3 += 1;
    }
    let mut kinds: Vec<_> = kinds.into_iter().map(|(id, (tier, what, size, n))| (id, tier, what, size, n)).collect();
    kinds.sort_by(|a, b| b.3.cmp(&a.3));
    Summary { total, tiers, kinds }
}

fn walk(w: &mut Walk, n: &Node, path: &str, in_downloads: bool) {
    for c in &n.children {
        let p = join(path, &c.name);
        let age = ((w.now - c.atime.max(c.mtime)) / DAY).max(0);
        let idle = format!("Idle {}.", months(age));
        let mk = |reason, tier, what, note: String, weight: f64| Candidate {
            path: p.clone(),
            name: c.name.clone(),
            size: c.size,
            reason,
            tier,
            what,
            note,
            age_days: age,
            is_dir: c.is_dir,
            score: c.size as f64 * weight * (1.0 + age as f64 / 365.0),
            full_size: None,
        };
        let name = c.name.as_str();
        if c.is_dir {
            if c.size < MB {
                continue;
            }
            let hit = if CACHE_DIRS.contains(&name) {
                Some(mk("cache", "safe", "Build & dependency caches", "Recreated the next time you build or install.".into(), 3.0))
            } else if APP_CACHE_DIRS.contains(&name) {
                Some(mk("appcache", "safe", "App caches", "Apps rebuild this as needed. Some may start slower once.".into(), 2.5))
            } else if SIM_DIRS.contains(&name) {
                Some(mk("simulator", "safe", "Xcode simulators & device support", "Xcode downloads again only for devices you actually use.".into(), 2.5))
            } else if name == ".Trash" {
                Some(mk("trash", "safe", "Already in the Trash", "Empty the Trash to get this space back.".into(), 3.0))
            } else if name == ".ollama" || name == ".lmstudio" || (name == "hub" && path.ends_with("huggingface")) {
                Some(mk("models", "review", "AI models", "Downloaded model weights. Pulled again automatically if you use them.".into(), 1.2))
            } else if name == "Backup" && path.ends_with("MobileSync") {
                Some(mk("backups", "review", "iPhone & iPad backups", "Device backups. Remove old ones in Finder > device > Manage Backups.".into(), 1.2))
            } else if name == "Archives" && path.ends_with("Developer/Xcode") {
                Some(mk("archives", "review", "Xcode archives", "Old app builds. Keep only ones you still ship.".into(), 1.2))
            } else if BUILD_DIRS.contains(&name) && c.size >= 20 * MB {
                Some(mk("build", "likely", "Build output", "Regenerated by the next build. Check it is not deployed from here.".into(), 1.5))
            } else if name == "Logs" && c.size >= 50 * MB {
                Some(mk("logs", "likely", "Logs", "Apps write fresh logs. Old ones are rarely needed.".into(), 1.5))
            } else if age > 180 && c.size >= 50 * MB && c.children.iter().any(|k| PROJECT_MARKERS.contains(&k.name.as_str())) {
                Some(mk("project", "review", "Untouched projects", format!("No changes in {}. Archive it or delete it.", months(age)), 1.0))
            } else {
                None
            };
            if let Some(h) = hit {
                w.out.push(h);
            } else {
                walk(w, c, &p, in_downloads || name == "Downloads");
            }
            continue;
        }
        if c.size < MB {
            continue;
        }
        let e = ext(name);
        if c.size >= 5 * MB {
            let key = (c.size, c.name.clone());
            if let Some((other, other_m)) = w.seen.get(&key).cloned() {
                if other_m >= c.mtime {
                    w.out.push(mk("duplicate", "review", "Possible duplicates", format!("Same name and size as {other}."), 1.5));
                    continue;
                }
            }
            w.seen.insert(key, (p.clone(), c.mtime));
        }
        let hit = if name == "Docker.raw" {
            Some(mk("diskimage", "review", "VM & disk images", "Docker Desktop's disk. Shrink it from Docker's settings, not by deleting.".into(), 1.0))
        } else if DISK_IMAGE_EXT.contains(&e.as_str()) && c.size >= 100 * MB {
            Some(mk("diskimage", "review", "VM & disk images", format!("Virtual disk. Delete only if the VM is gone. {idle}"), 1.0))
        } else if INSTALLER_EXT.contains(&e.as_str()) && c.size >= 5 * MB && age > 7 {
            Some(mk("installer", "likely", "Installers & archives", format!("Usually done with once installed or extracted. {idle}"), 1.8))
        } else if in_downloads && age > 30 {
            Some(mk("downloads", "likely", "Old downloads", format!("Sitting in Downloads. {idle}"), 1.8))
        } else if (e == "log" || e == "crash") && c.size >= 10 * MB {
            Some(mk("logs", "likely", "Logs", format!("Apps write fresh logs. {idle}"), 1.5))
        } else if c.size >= 100 * MB {
            Some(mk("large", "review", "Large files", if age > 180 { idle.clone() } else { "Used recently. Worth knowing it is here.".into() }, 1.0))
        } else if c.size >= 10 * MB && age > 180 {
            Some(mk("stale", "review", "Big and idle", idle.clone(), 1.0))
        } else {
            None
        };
        if let Some(h) = hit {
            w.out.push(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan;

    #[test]
    fn scan_flags_and_patches() {
        let dir = std::env::temp_dir().join(format!("gunk-test-{}", std::process::id()));
        let nm = dir.join("proj/node_modules/pkg");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        std::fs::write(nm.join("big.js"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(dir.join("proj/small.txt"), b"hi").unwrap();
        std::fs::write(dir.join("Downloads/tool.dmg"), vec![1u8; 6 << 20]).unwrap();
        std::fs::write(dir.join("proj/copy.dmg"), vec![1u8; 6 << 20]).unwrap();
        let mut tree = scan::scan(&dir, &scan::Progress::new(&dir));
        assert_eq!(tree.files, 4);
        let all = find_all(&tree);
        let by = |r: &str| all.iter().find(|c| c.reason == r).map(|c| c.path.clone());
        assert_eq!(by("cache"), Some("proj/node_modules".into()));
        assert_eq!(all.iter().find(|c| c.reason == "cache").unwrap().tier, "safe");
        assert_eq!(under(&all, "proj").filter(|c| c.reason == "cache").count(), 1);
        assert!(under(&all, "nope").next().is_none());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        assert_eq!(scan::subtree(&tree, "", 1, Some(now - 30 * DAY)).idle_size, Some(0));
        let out = scan::subtree(&tree, "", 2, Some(now + 1));
        assert_eq!(out.idle_size, Some(tree.size));
        assert_eq!(out.types.iter().sum::<u64>(), tree.size);
        assert_eq!(out.types[5], 12 << 20); // two .dmg archives
        let s = summary(all.iter());
        assert_eq!(s.tiers[0].0, "safe");
        assert!(s.total >= 2 << 20);
        // live patch: a new file appears, an existing one is deleted, a folder is removed
        std::fs::write(dir.join("proj/new.bin"), vec![0u8; 3 << 20]).unwrap();
        scan::patch(&mut tree, &dir, &dir.join("proj/new.bin"));
        assert_eq!(tree.files, 5);
        std::fs::remove_file(dir.join("proj/small.txt")).unwrap();
        scan::patch(&mut tree, &dir, &dir.join("proj/small.txt"));
        assert_eq!(tree.files, 4);
        std::fs::remove_dir_all(dir.join("proj/node_modules")).unwrap();
        scan::patch(&mut tree, &dir, &dir.join("proj/node_modules"));
        assert!(scan::get(&tree, "proj/node_modules").is_none());
        assert_eq!(scan::get(&tree, "proj").unwrap().size + scan::get(&tree, "Downloads").unwrap().size, tree.size);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Bytes under `n` belonging to files untouched since `cutoff` (files only; a folder counts what is inside it).
pub fn idle_bytes(n: &Node, cutoff: i64) -> u64 {
    if !n.is_dir {
        return if n.atime.max(n.mtime) <= cutoff { n.size } else { 0 };
    }
    n.children.iter().map(|c| idle_bytes(c, cutoff)).sum()
}
