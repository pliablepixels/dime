//! Finds removal candidates in the scanned tree and explains each one in plain language.
//! Every candidate gets a tier: `safe` (regenerated automatically), `likely` (usually fine
//! once you glance at it), `review` (big or old, your call). The rules are data: see rules.rs.
use crate::rules::{self, Item, Rule};
use crate::scan::{join, Node};
use serde::Serialize;
use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MB: u64 = 1 << 20;
const DAY: i64 = 86_400;

#[derive(Serialize, Clone)]
pub struct Candidate {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// kind id, e.g. "cache", "downloads", "large"
    pub reason: String,
    pub tier: String,
    pub what: String,
    pub note: String,
    pub age_days: i64,
    pub is_dir: bool,
    pub score: f64,
    /// Set in the idle view: `size` is then the idle bytes inside, and this is the whole item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_size: Option<u64>,
    /// What removes this properly, when a tool owns it: `ollama rm qwen3:8b`. Deleting runs this
    /// instead of unlinking. Still holding a `{name}` means the name was never resolved, so it is
    /// not runnable and is ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_cmd: Option<String>,
}

/// A removal command with every placeholder filled in, ready to run.
pub fn runnable(cmd: &str) -> bool {
    !cmd.contains('{') && !cmd.trim().is_empty()
}

#[derive(Serialize)]
pub struct Summary {
    pub total: u64,
    pub tiers: Vec<(&'static str, u64, usize)>,
    pub kinds: Vec<(String, String, String, u64, usize)>,
}

struct Walk {
    now: i64,
    rules: Vec<Rule>,
    /// Tier that anything currently open drops to, and the open paths themselves.
    open_tier: Option<String>,
    open: Vec<String>,
    /// Absolute path of the scan root, so a candidate can be checked against the real filesystem.
    base: PathBuf,
    /// Whether each folder walked can be written to, cached because siblings share one.
    writable: HashMap<String, bool>,
    out: Vec<Candidate>,
    /// (size, name) -> first path seen, for cheap duplicate detection of big files
    seen: HashMap<(u64, String), (String, i64)>,
}

/// How cautious a tier is: only ever moved further down the list, never up.
fn rank(tier: &str) -> usize {
    rules::TIERS.iter().position(|t| *t == tier).unwrap_or(0)
}
fn ext(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}
/// Sizes the way people say them, for notes that quote a total.
fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else if v < 10.0 { format!("{v:.1} {}", U[i]) } else { format!("{v:.0} {}", U[i]) }
}
fn months(days: i64) -> String {
    if days < 60 { format!("{days} days") } else if days < 730 { format!("{} months", days / 30) } else { format!("{:.1} years", days as f64 / 365.0) }
}

/// All candidates in the tree, best first. Compute once per tree version; filter by prefix per request.
/// Rules are re-read each time, so edits to ~/.dime/rules.toml show up on the next rescan.
/// Ollama stores weights by content hash, so the map shows anonymous sha256 blobs where people
/// expect model names. Its manifests hold the mapping: one file per model:tag, listing the layers
/// it is made of. Returns blob path -> (model name, size of the whole model), and separately the
/// manifests whose blobs are no longer on disk: `ollama ls` reads manifests, so a model whose
/// weights were removed behind Ollama's back keeps being listed as if it were there.
fn ollama_models(store: &Path) -> (HashMap<PathBuf, (String, u64)>, Vec<Orphan>) {
    let (manifests, blobs) = (store.join("manifests"), store.join("blobs"));
    let mut out = HashMap::new();
    let mut orphans = vec![];
    let mut stack = vec![manifests.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            // .../manifests/<registry>/<namespace>/<name>/<tag> reads back as name:tag
            let rel = p.strip_prefix(&manifests).unwrap_or(&p);
            let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if parts.len() < 2 {
                continue;
            }
            let model = format!("{}:{}", parts[parts.len() - 2], parts[parts.len() - 1]);
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            let layers = json["layers"].as_array().cloned().unwrap_or_default();
            let paths: Vec<PathBuf> = layers
                .iter()
                .filter_map(|l| l["digest"].as_str())
                .map(|d| blobs.join(d.replace(':', "-")))
                .collect();
            let total: u64 = paths.iter().filter_map(|b| std::fs::symlink_metadata(b).ok()).map(|m| m.len()).sum();
            let missing: u64 = layers
                .iter()
                .filter(|l| !blobs.join(l["digest"].as_str().unwrap_or_default().replace(':', "-")).exists())
                .filter_map(|l| l["size"].as_u64())
                .sum();
            if missing > 0 {
                let size = std::fs::symlink_metadata(&p).map(|m| m.len()).unwrap_or(0);
                orphans.push(Orphan { manifest: p.clone(), model: model.clone(), missing, size });
            }
            for b in paths {
                out.insert(b, (model.clone(), total));
            }
        }
    }
    (out, orphans)
}

/// A model Ollama still lists although its weights are gone.
struct Orphan {
    manifest: PathBuf,
    model: String,
    /// bytes the manifest still claims, which are no longer on disk
    missing: u64,
    /// what the manifest file itself takes, which is all removing it actually frees
    size: u64,
}

/// Every regular file a running process has open, as absolute paths. One lsof sweep for the whole
/// machine, cached briefly because find_all runs again on every tree change.
///
/// `-n` is not optional: without it lsof resolves the peer address of every network socket on the
/// machine, which took ten seconds on the machine this was written on and is the whole reason the
/// first Cleanup panel after a scan used to hang. Nothing here reads socket names anyway.
fn open_paths() -> Vec<String> {
    static CACHE: Mutex<Option<(SystemTime, Arc<Vec<String>>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some((at, v)) = c.as_ref() {
        if at.elapsed().is_ok_and(|e| e.as_secs() < 15) {
            return v.as_ref().clone();
        }
    }
    let mut out = vec![];
    if let Ok(o) = std::process::Command::new("lsof").args(["-Fn", "-w", "-n"]).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(p) = line.strip_prefix('n') {
                if p.starts_with('/') && !p.starts_with("/System/") && !p.starts_with("/usr/") && !p.starts_with("/dev/") {
                    out.push(p.to_string());
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    *c = Some((SystemTime::now(), Arc::new(out.clone())));
    out
}

/// Is anything open underneath this path right now?
fn in_use(w: &Walk, abs: &Path) -> bool {
    let p = abs.to_string_lossy();
    let i = w.open.partition_point(|o| o.as_str() < p.as_ref());
    w.open[i..].first().is_some_and(|o| o.as_str() == p || o.starts_with(&format!("{p}/")))
}

/// Removing something means writing to the folder that holds it, so that is what decides whether
/// DiMe may offer it. Without this it lists root-owned system assets it could never remove.
fn removable(w: &mut Walk, dir: &str) -> bool {
    if let Some(&v) = w.writable.get(dir) {
        return v;
    }
    let abs = if dir.is_empty() { w.base.clone() } else { w.base.join(dir) };
    let ok = std::ffi::CString::new(abs.as_os_str().as_bytes())
        .map(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
        .unwrap_or(false);
    w.writable.insert(dir.to_string(), ok);
    ok
}

pub fn find_all(base: &Path, root: &Node) -> Vec<Candidate> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let cfg = rules::load();
    // Me's half of the app answers a question Di cannot: what is in use right this second.
    let open = if cfg.open_tier.is_some() { open_paths() } else { vec![] };
    let mut w = Walk { now, rules: cfg.rules, open_tier: cfg.open_tier, open, base: base.to_path_buf(), writable: HashMap::new(), out: vec![], seen: HashMap::new() };
    walk(&mut w, root, "");
    name_stores(base, &mut w.out);
    w.out.sort_by(|a, b| b.score.total_cmp(&a.score));
    w.out
}

/// Every store keeps its weights differently, and most of them hide the name people know the model
/// by. This is where DiMe puts it back: Ollama addresses blobs by hash, Hugging Face mangles
/// org/name into a folder, and the rest are readable enough to leave alone. Rules decide what gets
/// flagged and at what tier; this only improves how it reads.
fn name_stores(base: &Path, out: &mut Vec<Candidate>) {
    name_ollama(base, out, ollama_store());
    name_huggingface(out);
}

/// `models--Salesforce--SFR-Embedding-Mistral` is how the hub spells `Salesforce/SFR-Embedding-Mistral`.
fn name_huggingface(out: &mut [Candidate]) {
    for c in out.iter_mut().filter(|c| c.reason == "hf-model" || c.reason == "hf-datasets") {
        let Some(rest) = c.name.strip_prefix("models--").or_else(|| c.name.strip_prefix("datasets--")) else { continue };
        let pretty = rest.replace("--", "/");
        c.note = format!("{pretty}, as the hub stores it. `huggingface-cli delete-cache` removes it tidily. {}", c.note);
        c.name = pretty;
    }
}

/// Put model names back on Ollama's content-addressed blobs, once, after the walk.
/// The one store the `ollama` command actually talks to. Its server holds this path, and it is not
/// told which folder DiMe was looking at: `ollama rm` on a store copied to an external drive would
/// remove the model from the machine's own store instead. So the command is only ever offered for
/// this one, and any other store is a folder of files like any other.
fn ollama_store() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OLLAMA_MODELS") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ollama").join("models"))
}
fn same_dir(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

fn name_ollama(base: &Path, out: &mut Vec<Candidate>, served: Option<PathBuf>) {
    let mut stores: std::collections::HashSet<PathBuf> = out
        .iter()
        .filter(|c| c.reason == "ollama-model")
        .filter_map(|c| base.join(&c.path).parent().and_then(|p| p.parent()).map(Path::to_path_buf))
        .collect();
    // a store whose blobs are all gone leaves no candidate to find it by, so the served one is
    // looked at as well whenever the scan covers it
    if let Some(s) = served.clone().filter(|s| s.starts_with(base) && s.join("manifests").is_dir()) {
        stores.insert(s);
    }
    if stores.is_empty() {
        return;
    }
    let mut map = HashMap::new();
    let mut orphans = vec![];
    for s in stores {
        let ours = served.as_deref().is_some_and(|d| same_dir(&s, d));
        let (m, o) = ollama_models(&s);
        map.extend(m.into_iter().map(|(k, (model, total))| (k, (model, total, ours))));
        orphans.extend(o.into_iter().map(|o| (o, ours)));
    }
    for (o, ours) in orphans {
        let Ok(rel) = o.manifest.strip_prefix(base) else { continue };
        let name = o.model;
        out.push(Candidate {
            path: rel.to_string_lossy().into_owned(),
            size: o.size,
            reason: "ollama-orphan".into(),
            tier: "safe".into(),
            what: "Ollama models with no weights".into(),
            note: format!(
                "Ollama still lists {name} at {}, but those weights are not on the disk any more: something removed them without telling Ollama. {} It frees only {}, because the weights are already gone.",
                human(o.missing),
                if ours { format!("Deleting this runs `ollama rm {name}`, which clears the entry.") } else { "This store is not the one the ollama command talks to, so deleting removes the entry file itself.".into() },
                human(o.size)
            ),
            remove_cmd: ours.then(|| format!("ollama rm {name}")),
            name,
            age_days: 0,
            is_dir: false,
            score: 0.0,
            full_size: None,
        });
    }
    for c in out.iter_mut().filter(|c| c.reason == "ollama-model") {
        if let Some((model, total, ours)) = map.get(&base.join(&c.path)) {
            // only the served store: elsewhere the command would remove the wrong copy
            c.remove_cmd = ours.then(|| c.remove_cmd.as_ref().map(|t| t.replace("{name}", model))).flatten();
            c.name = model.clone();
            let how = if *ours {
                format!("Deleting this hands it to Ollama as `ollama rm {model}`, so its own list stays right.")
            } else {
                "This store is not the one the ollama command talks to, so it is removed as plain files.".into()
            };
            c.note = format!("The weights of {model}, {} in all. {how} {}", human(*total), c.note);
        }
    }
}

pub fn under<'a>(all: &'a [Candidate], prefix: &str) -> impl Iterator<Item = &'a Candidate> + 'a {
    let p = prefix.trim_matches('/').to_string();
    all.iter().filter(move |c| p.is_empty() || c.path == p || c.path.starts_with(&format!("{p}/")))
}

pub fn summary<'a>(cands: impl Iterator<Item = &'a Candidate>) -> Summary {
    let mut tiers: Vec<(&'static str, u64, usize)> = rules::TIERS.iter().map(|t| (*t, 0, 0)).collect();
    // keyed by kind *and* tier: one open file can demote a single item out of an otherwise safe
    // group, and reporting the group under that one item's tier misrepresents the other hundred
    let mut kinds: HashMap<(&str, &str), (&str, u64, usize)> = HashMap::new();
    let mut total = 0;
    for c in cands {
        total += c.size;
        let t = tiers.iter_mut().find(|t| t.0 == c.tier).unwrap();
        t.1 += c.size;
        t.2 += 1;
        let k = kinds.entry((&c.reason, &c.tier)).or_insert((&c.what, 0, 0));
        k.1 += c.size;
        k.2 += 1;
    }
    let mut kinds: Vec<_> = kinds.into_iter().map(|((id, tier), (what, size, n))| (id.into(), tier.into(), what.into(), size, n)).collect();
    kinds.sort_by(|a, b| b.3.cmp(&a.3));
    Summary { total, tiers, kinds }
}

fn walk(w: &mut Walk, n: &Node, path: &str) {
    let sibs: Vec<String> = n.children.iter().map(|k| k.name.clone()).collect();
    for c in &n.children {
        if c.size < MB {
            continue;
        }
        let p = join(path, &c.name);
        let stamp = c.atime.max(c.mtime);
        let age_known = stamp > 0; // some system files sit at the epoch; that is unknown, not ancient
        let age = if age_known { ((w.now - stamp) / DAY).max(0) } else { 0 };
        let mk = |rule: &Rule, note: String| Candidate {
            path: p.clone(),
            name: c.name.clone(),
            size: c.size,
            reason: rule.id.clone(),
            tier: rule.tier.clone(),
            what: rule.what.clone(),
            note,
            age_days: age,
            is_dir: c.is_dir,
            score: c.size as f64 * rule.weight * (1.0 + age as f64 / 365.0),
            full_size: None,
            remove_cmd: rule.remove_with.clone(),
        };
        if !c.is_dir && c.size >= 5 * MB {
            // duplicates need state across the walk, so this one stays built in
            let key = (c.size, c.name.clone());
            if let Some((other, other_m)) = w.seen.get(&key).cloned() {
                if other_m >= c.mtime {
                    let dup = Rule { id: "duplicate".into(), tier: "review".into(), what: "Possible duplicates".into(), note: String::new(), weight: 1.5, dir: None, name: vec![], ext: vec![], parent_ends_with: None, under: None, has_child: vec![], has_sibling: vec![], no_sibling: vec![], min_size: 0, min_age_days: 0, descend: false, remove_with: None };
                    w.out.push(mk(&dup, format!("Same name and size as {other}.")));
                    continue;
                }
            }
            w.seen.insert(key, (p.clone(), c.mtime));
        }
        let e = ext(&c.name);
        let kids: Vec<String> = if c.is_dir { c.children.iter().map(|k| k.name.clone()).collect() } else { vec![] };
        let it = Item { name: &c.name, is_dir: c.is_dir, ext: &e, size: c.size, age_days: age, age_known, parent: path, children: &kids, siblings: &sibs };
        // checked before the rule lookup so the two borrows of `w` never overlap; an item we cannot
        // remove still gets walked into, since something deeper may sit in a folder we do own
        let can_remove = removable(w, path);
        let hit = w.rules.iter().filter(|_| can_remove).find(|r| r.matches(&it));
        if hit.is_some_and(|r| r.descend) {
            // a container: named only so the walk knows to carry on past it
            if c.is_dir {
                walk(w, c, &p);
            }
            continue;
        }
        if let Some(r) = hit {
            let idle = if age_known { format!("Idle {}.", months(age)) } else { "Age unknown.".into() };
            let note = r.note.replace("{idle}", &idle).replace("{age}", &if age_known { months(age) } else { "an unknown time".into() });
            let mut cand = mk(r, note);
            // Me knows what Di cannot: something being written to right now is not safe to move,
            // whatever its name suggests. This is the one check that uses live evidence.
            if let Some(t) = w.open_tier.clone() {
                if rank(&t) > rank(&cand.tier) && in_use(w, &w.base.join(&p)) {
                    cand.tier = t;
                    cand.note = format!("A running program has this open right now. {}", cand.note);
                }
            }
            w.out.push(cand);
        } else if c.is_dir {
            walk(w, c, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan;

    /// A model whose weights were removed behind Ollama's back: the manifest is still there, so
    /// `ollama ls` still lists it, and DiMe has to offer the removal Ollama itself understands.
    #[test]
    fn ollama_entry_left_without_weights() {
        let dir = std::env::temp_dir().join(format!("gunk-ollama-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = dir.join(".ollama/models");
        std::fs::create_dir_all(store.join("blobs")).unwrap();
        let m = store.join("manifests/registry.ollama.ai/library/qwen3");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(store.join("blobs/sha256-here"), b"weights").unwrap();
        std::fs::write(
            m.join("8b"),
            br#"{"layers":[{"digest":"sha256:here","size":7},{"digest":"sha256:gone","size":5000000000}]}"#,
        )
        .unwrap();

        let mut out = vec![];
        name_ollama(&dir, &mut out, Some(store.clone()));
        assert_eq!(out.len(), 1, "one entry, for the model whose blob is missing");
        let c = &out[0];
        assert_eq!(c.name, "qwen3:8b");
        assert_eq!(c.reason, "ollama-orphan");
        assert_eq!(c.remove_cmd.as_deref(), Some("ollama rm qwen3:8b"));

        // the same store on a drive, found through a blob the walk flagged rather than through the
        // served path: `ollama rm` would remove the model from this machine instead, so it is not
        // offered, and the blob is removed as a plain file
        let blob = Candidate {
            path: ".ollama/models/blobs/sha256-here".into(),
            name: "sha256-here".into(),
            size: 7,
            reason: "ollama-model".into(),
            tier: "review".into(),
            what: "Ollama models".into(),
            note: String::new(),
            age_days: 0,
            is_dir: false,
            score: 0.0,
            full_size: None,
            remove_cmd: Some("ollama rm {name}".into()),
        };
        let mut out = vec![blob.clone()];
        name_ollama(&dir, &mut out, None);
        assert_eq!(out.len(), 2, "the blob it was found by, and the entry with no weights");
        assert!(out.iter().all(|c| c.remove_cmd.is_none()), "no command for a store ollama does not serve");
        assert!(out.iter().all(|c| c.note.contains("not the one the ollama command talks to")));

        // and served, that same blob is handed to ollama by name
        let mut out = vec![blob];
        name_ollama(&dir, &mut out, Some(store.clone()));
        let b = out.iter().find(|c| c.reason == "ollama-model").unwrap();
        assert_eq!(b.name, "qwen3:8b");
        assert_eq!(b.remove_cmd.as_deref(), Some("ollama rm qwen3:8b"));
        assert!(runnable(c.remove_cmd.as_deref().unwrap()), "no placeholder left to fill");
        assert!(c.note.contains("4.7 GB"), "says what is missing, not what it frees: {}", c.note);
        assert!(c.size < 1000, "removing the entry frees only the manifest itself");

        // every blob present: nothing to report
        std::fs::write(store.join("blobs/sha256-gone"), b"x").unwrap();
        let mut out = vec![];
        name_ollama(&dir, &mut out, Some(store));
        assert!(out.is_empty());

        // a rule's command is not runnable until the name it asks for is known
        assert!(!runnable("ollama rm {name}"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_flags_and_patches() {
        let dir = std::env::temp_dir().join(format!("gunk-test-{}", std::process::id()));
        let nm = dir.join("proj/node_modules/pkg");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        std::fs::write(nm.join("big.js"), vec![0u8; 2 << 20]).unwrap();
        std::fs::write(dir.join("proj/small.txt"), b"hi").unwrap();
        std::fs::write(dir.join("proj/package.json"), b"{}").unwrap(); // project still present, so its cache is rebuildable
        std::fs::write(dir.join("Downloads/tool.dmg"), vec![1u8; 6 << 20]).unwrap();
        std::fs::write(dir.join("proj/copy.dmg"), vec![1u8; 6 << 20]).unwrap();
        let mut tree = scan::scan(&dir, &scan::Progress::new(&dir));
        assert_eq!(tree.files, 5);
        let all = find_all(&dir, &tree);
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
        assert_eq!(tree.files, 6);
        std::fs::remove_file(dir.join("proj/small.txt")).unwrap();
        scan::patch(&mut tree, &dir, &dir.join("proj/small.txt"));
        assert_eq!(tree.files, 5);
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
