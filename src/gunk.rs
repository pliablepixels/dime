//! Finds removal candidates in the scanned tree and explains each one in plain language.
//! Every candidate gets a tier: `safe` (regenerated automatically), `likely` (usually fine
//! once you glance at it), `review` (big or old, your call). The rules are data: see rules.rs.
use crate::rules::{self, DiskCheck, Item, Rule};
use rayon::prelude::*;
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
    /// Ignore patterns in force in each folder, gathered up to the repo root. `None` means the
    /// folder is not inside a git repo at all, so there is nothing to ask. Cached per folder.
    ignored: HashMap<String, Option<Vec<String>>>,
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
/// In the idle view a row's size is the bytes inside it that are old enough, but its note was
/// written about the item itself. A folder written to yesterday can hold gigabytes untouched for
/// months, and "Idle 1 days" under a heading reading "untouched 30 days+" looks like a
/// contradiction. So for a folder the sentence is rewritten to say which of the two ages it is.
/// A file is its own age and only shows up when it is old enough, so its note is left alone.
pub fn note_for_idle(note: &str, is_dir: bool, age_days: i64, days: i64) -> String {
    if !is_dir {
        return note.to_string();
    }
    let own = format!("Idle {}.", months(age_days));
    let said = format!("Touched {} ago itself; what is counted here is the files inside it untouched {days}+ days.", months(age_days));
    if note.contains(&own) {
        return note.replace(&own, &said);
    }
    note.replace("Age unknown.", &format!("What is counted here is the files inside it untouched {days}+ days."))
}

fn months(days: i64) -> String {
    if days < 60 { format!("{days} days") } else if days < 730 { format!("{} months", days / 30) } else { format!("{:.1} years", days as f64 / 365.0) }
}

/// All candidates in the tree, best first. Compute once per tree version; filter by prefix per request.
/// Rules are re-read each time, so edits to ~/.dime/rules.toml show up on the next rescan.
/// Ollama stores weights by content hash, so the map shows anonymous sha256 blobs where people
/// expect model names. Its manifests hold the mapping: one file per model:tag, listing the layers
/// it is made of. Returns blob path -> every (model, whole-model size) that claims it, and separately the
/// manifests whose blobs are no longer on disk: `ollama ls` reads manifests, so a model whose
/// weights were removed behind Ollama's back keeps being listed as if it were there.
fn ollama_models(store: &Path) -> (Blobs, Vec<Orphan>) {
    let (manifests, blobs) = (store.join("manifests"), store.join("blobs"));
    let mut out: HashMap<PathBuf, Vec<(String, u64)>> = HashMap::new();
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
            // one blob, every model built on it: models share a base layer far more often than
            // they look like they do, and the last manifest read used to silently win
            for b in paths {
                out.entry(b).or_default().push((model.clone(), total));
            }
        }
    }
    (out, orphans)
}

/// One blob, and every (model, whole-model size) that names it as a layer.
type Blobs = HashMap<PathBuf, Vec<(String, u64)>>;

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
///
/// The list is sorted, so the files under `p` are the run starting at `p/`. Searching for `p`
/// itself and looking at one entry is not the same thing: `.` sorts below `/`, so every sibling
/// spelled `p.something` lands between the two and hides the run behind it. `com.apple.Safari`
/// and `com.apple.Safari.SafeBrowsing` share a Caches folder, and that pair alone was enough to
/// let Safari's cache read as idle while Safari held it open.
fn in_use(w: &Walk, abs: &Path) -> bool {
    let p = abs.to_string_lossy();
    if w.open.binary_search(&p.to_string()).is_ok() {
        return true;
    }
    let under = format!("{p}/");
    let i = w.open.partition_point(|o| o.as_str() < under.as_str());
    w.open.get(i).is_some_and(|o| o.starts_with(&under))
}

/// Folders whose contents are a replica of something in the cloud. Moving a file out of one of
/// these is not archiving it: the sync client reads the removal and takes the file off every other
/// device and out of the account, and the shelf cannot put that back. Di still measures them, and
/// they still show on the map. It simply never suggests anything inside one.
fn synced(path: &str) -> bool {
    path.split('/').any(|s| {
        // named exactly, never by a fragment: a folder called Sync or Box is somebody's project
        matches!(s, "Mobile Documents" | "CloudStorage" | "Dropbox" | "Creative Cloud Files" | "pCloud Drive" | "Box Sync" | "Resilio Sync" | "Sync.com" | "MEGA" | "Nextcloud" | "Seafile")
            || s.starts_with("Google Drive")
            || s.starts_with("OneDrive")
            || s.starts_with("iCloud")
            || s.ends_with(".photoslibrary")
    })
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

/// The patterns one .gitignore lays down, normalised to bare names. Enough for the question being
/// asked, which is whether a folder called `build` or `dist` is derived: those are written as a
/// plain name, `/name`, `name/` or `**/name`, and never as anything cleverer.
fn ignore_names(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(".gitignore"))
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
                .map(|l| l.trim_start_matches("**/").trim_matches('/').to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Does the project itself say this folder is derived? A `dist` in .gitignore is output that the
/// next build writes again. A `dist` that is committed is what somebody ships, and the two want
/// opposite advice from a cleaner. Read from the .gitignore files rather than by running git,
/// because git would be a process per candidate.
///
/// Outside a repo there is no such signal, so the answer is yes and the rule behaves as it always
/// did: this narrows what gets offered where there is evidence, and changes nothing where there
/// is none.
fn git_ignored(w: &mut Walk, dir: &str, name: &str) -> bool {
    if !w.ignored.contains_key(dir) {
        let (mut pats, mut cur, mut in_repo) = (vec![], dir.to_string(), false);
        loop {
            let abs = if cur.is_empty() { w.base.clone() } else { w.base.join(&cur) };
            pats.extend(ignore_names(&abs));
            if abs.join(".git").exists() {
                in_repo = true;
                break;
            }
            match cur.rfind('/') {
                Some(i) => cur.truncate(i),
                None if !cur.is_empty() => cur.clear(),
                None => break,
            }
        }
        w.ignored.insert(dir.to_string(), in_repo.then_some(pats));
    }
    w.ignored[dir].as_ref().is_none_or(|pats| pats.iter().any(|p| p == name))
}

/// Every application bundle identifier installed on this machine, with the ids of the helpers and
/// daemons that ship beside them. Built once, in parallel, because a cold `plutil` per app is a
/// tenth of a second all told and the answer cannot change inside one run of DiMe.
///
/// Empty means the question could not be answered, not that nothing is installed, so `app_gone`
/// reads an empty set as "say nothing" rather than "everything is orphaned".
fn installed_bundles() -> &'static std::collections::HashSet<String> {
    static IDS: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        let roots = ["/Applications", "/System/Applications", "/Applications/Setapp", &format!("{home}/Applications")].map(PathBuf::from);
        let mut apps = vec![];
        // three levels deep: /Applications/Utilities/Foo.app and the folders vendors like to make
        fn find_apps(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "app") {
                    out.push(p);
                } else if depth > 0 && p.is_dir() {
                    find_apps(&p, depth - 1, out);
                }
            }
        }
        for r in &roots {
            find_apps(r, 2, &mut apps);
        }
        let mut ids: std::collections::HashSet<String> = apps
            .par_iter()
            .filter_map(|a| {
                let out = std::process::Command::new("plutil")
                    .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
                    .arg(a.join("Contents/Info.plist"))
                    .output()
                    .ok()?;
                out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
            })
            .collect();
        // Launch agents, daemons and privileged helpers are named by bundle id on disk, and they
        // are exactly the things that keep a folder alive with no .app anywhere to prove it.
        for d in ["/Library/LaunchAgents", "/Library/LaunchDaemons", "/Library/PrivilegedHelperTools", "/Library/Application Support", &format!("{home}/Library/LaunchAgents")] {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            for e in rd.flatten() {
                ids.insert(e.file_name().to_string_lossy().trim_end_matches(".plist").to_string());
            }
        }
        ids
    })
}

/// Is this folder what an uninstalled application left behind?
///
/// Only ever says yes about a reverse-DNS folder name, because that is the one name on disk that
/// maps to an application without guessing: `com.acme.Widget` is a bundle id, `Widget` is a word.
/// Apple's own ids are never claimed: plenty of them belong to system services that ship no .app.
/// A helper counts as installed when its app is, so `com.acme.Widget.Updater` survives as long as
/// `com.acme.Widget` does.
fn app_gone(name: &str) -> bool {
    let ids = installed_bundles();
    if ids.is_empty() || name.contains(' ') {
        return false;
    }
    // A group container wears a team id or a `group.` in front of the identifier it belongs to,
    // and the Apple test has to come after those come off: `group.com.apple.chronod` is Siri's,
    // not some app's, and reading it before stripping was enough to offer four system services.
    let base = name.trim_end_matches(".savedState").trim_end_matches(".binarycookies");
    let base = base.strip_prefix("group.").unwrap_or(base);
    let base = match base.split_once('.') {
        Some((team, rest)) if team.len() == 10 && team.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) => rest,
        _ => base,
    };
    if base.starts_with("com.apple.") || base.matches('.').count() < 2 {
        return false;
    }
    // Matched at the vendor, not at the exact identifier. An app renames itself between versions
    // (`com.microsoft.teams` became `com.microsoft.teams2`) and ships helpers under identifiers no
    // bundle on disk carries, so demanding an exact match invents leftovers for software that is
    // plainly still installed. An uninstall usually takes the whole vendor with it, and claiming
    // less than we could is the right way to be wrong here.
    let vendor = |id: &str| id.split('.').take(2).collect::<Vec<_>>().join(".");
    let mine = vendor(base);
    !ids.iter().any(|id| vendor(id) == mine)
}

/// Bytes under `n` that another path on the disk holds as well. One store hard-linked into every
/// project is how pnpm, uv, bun and Homebrew all work, and the same blocks are then counted at
/// every link. Removing one of them frees nothing until the last one goes.
fn shared_bytes(n: &Node) -> u64 {
    if !n.is_dir {
        return if n.shared { n.size } else { 0 };
    }
    n.children.iter().map(shared_bytes).sum()
}

pub fn find_all(base: &Path, root: &Node) -> Vec<Candidate> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let cfg = rules::load();
    // Me's half of the app answers a question Di cannot: what is in use right this second.
    let open = if cfg.open_tier.is_some() { open_paths() } else { vec![] };
    let mut w = Walk { now, rules: cfg.rules, open_tier: cfg.open_tier, open, base: base.to_path_buf(), writable: HashMap::new(), ignored: HashMap::new(), out: vec![], seen: HashMap::new() };
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
        map.extend(m.into_iter().map(|(k, models)| (k, (models, ours))));
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
        let Some((models, ours)) = map.get(&base.join(&c.path)) else { continue };
        let Some((model, total)) = models.first() else { continue };
        if models.len() > 1 {
            // A layer several models are built on. `ollama rm` on any one of them leaves the blob
            // exactly where it is, so offering the command here would promise space it cannot give.
            let names: Vec<&str> = models.iter().map(|(m, _)| m.as_str()).collect();
            c.name = format!("layer of {}", names.join(" + "));
            c.remove_cmd = None;
            c.note = format!(
                "One layer that {} models share: {}. It goes when the last of them goes and not before, so removing any one of them frees none of this. Remove them from Ollama by name rather than from here. {}",
                names.len(),
                names.join(", "),
                c.note
            );
            continue;
        }
        // only the served store: elsewhere the command would remove the wrong copy
        c.remove_cmd = ours.then(|| c.remove_cmd.as_ref().map(|t| t.replace("{model}", model))).flatten();
        c.name = model.clone();
        let how = if *ours {
            format!("Deleting this hands it to Ollama as `ollama rm {model}`, so its own list stays right. That is final: a pulled model downloads again, one you built with `ollama create` does not.")
        } else {
            "This store is not the one the ollama command talks to, so it is removed as plain files.".into()
        };
        c.note = format!("The weights of {model}, {} in all. {how} {}", human(*total), c.note);
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
            remove_cmd: rule.remove_with.as_ref().map(|t| t.replace("{name}", &c.name)),
        };
        if !c.is_dir && c.size >= 5 * MB {
            // duplicates need state across the walk, so this one stays built in
            let key = (c.size, c.name.clone());
            if let Some((other, other_m)) = w.seen.get(&key).cloned() {
                if other_m >= c.mtime {
                    let dup = Rule { id: "duplicate".into(), tier: "review".into(), what: "Possible duplicates".into(), note: String::new(), weight: 1.5, dir: None, name: vec![], ext: vec![], parent_ends_with: vec![], under: vec![], has_child: vec![], has_sibling: vec![], no_sibling: vec![], min_size: 0, min_age_days: 0, descend: false, remove_with: None, git_ignored: false, app_gone: false };
                    // same name, same size and only one copy of the bytes: these are two names
                    // for one file, and removing either frees nothing at all
                    let same_inode = if c.shared { " They are hard links to one another, so removing either frees nothing: the bytes go when the last name for them does." } else { "" };
                    w.out.push(mk(&dup, format!("Same name and size as {other}.{same_inode}")));
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
        // ...and a file that syncs is never ours to remove, however ordinary its name looks
        let can_remove = removable(w, path) && !synced(&p);
        let hit = w.rules.iter().filter(|_| can_remove).find(|r| r.matches(&it)).cloned();
        if hit.as_ref().is_some_and(|r| r.descend) {
            // a container: named only so the walk knows to carry on past it
            if c.is_dir {
                walk(w, c, &p);
            }
            continue;
        }
        // The half of the matcher contract that reads the disk rather than the tree, declared on
        // the rule as `DiskCheck` and answered here. Failing one means the item was never a
        // candidate at all: the walk carries on underneath it as if no rule had named it.
        let hit = hit.filter(|r| {
            r.disk_checks().all(|check| match check {
                DiskCheck::GitIgnored => git_ignored(w, path, &c.name),
                DiskCheck::AppGone => app_gone(&c.name),
            })
        });
        if let Some(r) = &hit {
            let idle = if age_known { format!("Idle {}.", months(age)) } else { "Age unknown.".into() };
            let note = r.note.replace("{idle}", &idle).replace("{age}", &if age_known { months(age) } else { "an unknown time".into() });
            let mut cand = mk(r, note);
            // Hard-linked bytes are counted at every path that holds them, so a row can promise
            // space that removing it will not give back. Say so on the row rather than after.
            let shared = shared_bytes(c);
            if shared >= cand.size && shared > 0 {
                cand.note = format!("Every byte here is hard-linked from somewhere else on the disk, so removing this frees nothing until the last link to it goes too. {}", cand.note);
            } else if human(cand.size - shared) != human(cand.size) {
                // only worth a sentence when it changes the figure the row is already showing:
                // a few linked megabytes inside three gigabytes reads as a contradiction, not a warning
                cand.note = format!("{} of this is hard-linked from somewhere else on the disk, so removing it frees about {}, not {}. {}", human(shared), human(cand.size - shared), human(cand.size), cand.note);
            }
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

/// Bytes under `n` belonging to files untouched since `cutoff` (files only; a folder counts what is inside it).
pub fn idle_bytes(n: &Node, cutoff: i64) -> u64 {
    if !n.is_dir {
        return if n.atime.max(n.mtime) <= cutoff { n.size } else { 0 };
    }
    n.children.iter().map(|c| idle_bytes(c, cutoff)).sum()
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
            remove_cmd: Some("ollama rm {model}".into()),
        };
        let mut out = vec![blob.clone()];
        name_ollama(&dir, &mut out, None);
        assert_eq!(out.len(), 2, "the blob it was found by, and the entry with no weights");
        assert!(out.iter().all(|c| c.remove_cmd.is_none()), "no command for a store ollama does not serve");
        assert!(out.iter().all(|c| c.note.contains("not the one the ollama command talks to")));

        // and served, that same blob is handed to ollama by name
        let mut out = vec![blob.clone()];
        name_ollama(&dir, &mut out, Some(store.clone()));
        let b = out.iter().find(|c| c.reason == "ollama-model").unwrap();
        assert_eq!(b.name, "qwen3:8b");
        assert_eq!(b.remove_cmd.as_deref(), Some("ollama rm qwen3:8b"));
        assert!(runnable(c.remove_cmd.as_deref().unwrap()), "no placeholder left to fill");
        assert!(c.note.contains("4.7 GB"), "says what is missing, not what it frees: {}", c.note);
        assert!(c.size < 1000, "removing the entry frees only the manifest itself");

        // A layer two models are built on. `ollama rm` on either leaves it exactly where it is,
        // so the row must not offer a command, and must not read as that model's weights.
        let m2 = store.join("manifests/registry.ollama.ai/library/qwen3-coder");
        std::fs::create_dir_all(&m2).unwrap();
        std::fs::write(m2.join("8b"), br#"{"layers":[{"digest":"sha256:here","size":7}]}"#).unwrap();
        let mut out = vec![blob.clone()];
        name_ollama(&dir, &mut out, Some(store.clone()));
        let b = out.iter().find(|c| c.reason == "ollama-model").unwrap();
        assert!(b.remove_cmd.is_none(), "removing one of the two would free none of it");
        assert!(b.note.contains("qwen3:8b") && b.note.contains("qwen3-coder:8b"), "names both: {}", b.note);
        assert!(b.note.contains("frees none of this"), "{}", b.note);
        std::fs::remove_dir_all(&m2).unwrap();

        // every blob present: nothing to report
        std::fs::write(store.join("blobs/sha256-gone"), b"x").unwrap();
        let mut out = vec![];
        name_ollama(&dir, &mut out, Some(store));
        assert!(out.is_empty());

        // in the idle view a folder's own timestamp is not what picked the files inside it
        let n = "Debug symbols for one OS version. Idle 9 days.";
        let out = note_for_idle(n, true, 9, 30);
        assert!(!out.contains("Idle 9 days"), "the bare count reads as a contradiction: {out}");
        assert!(out.contains("Touched 9 days ago itself"), "{out}");
        assert!(out.contains("untouched 30+ days"), "{out}");
        assert_eq!(note_for_idle(n, false, 40, 30), n, "a file is its own age, so leave it be");
        assert!(note_for_idle("Age unknown.", true, 0, 30).contains("untouched 30+ days"));

        // a rule's command is not runnable until the name it asks for is known
        assert!(!runnable("ollama rm {model}"));

        // the plain case needs no code at all: a store that keeps one folder per model gets its
        // command filled in from the folder's own name, so a new tool is a rule and nothing else
        let r = Rule {
            id: "newtool-model".into(), tier: "review".into(), what: "NewTool models".into(), note: String::new(),
            weight: 1.0, dir: Some(true), name: vec![], ext: vec![], parent_ends_with: vec![".newtool/models".into()],
            under: vec![], has_child: vec![], has_sibling: vec![], no_sibling: vec![], min_size: 0, min_age_days: 0,
            descend: false, remove_with: Some("newtool remove {name}".into()), git_ignored: false, app_gone: false,
        };
        let filled = r.remove_with.as_ref().map(|t| t.replace("{name}", "llama-3-8b"));
        assert_eq!(filled.as_deref(), Some("newtool remove llama-3-8b"));
        assert!(runnable(filled.as_deref().unwrap()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The three checks that ask the machine rather than the tree: what is open right now, what
    /// belongs to the cloud, and what an uninstall left behind.
    #[test]
    fn live_checks() {
        let w = Walk {
            now: 0,
            rules: vec![],
            open_tier: None,
            // exactly as open_paths() leaves it: sorted, and `.` sorts below `/`
            open: vec!["/c/com.apple.Safari.SafeBrowsing/db".into(), "/c/com.apple.Safari/Cache.db".into(), "/c/z".into()],
            base: PathBuf::from("/"),
            writable: HashMap::new(),
            ignored: HashMap::new(),
            out: vec![],
            seen: HashMap::new(),
        };
        assert!(w.open.windows(2).all(|p| p[0] <= p[1]), "the check below relies on the order");
        // the sibling sorting in between is what used to hide the whole run behind it
        assert!(in_use(&w, Path::new("/c/com.apple.Safari")), "Safari holds this open");
        assert!(in_use(&w, Path::new("/c/com.apple.Safari.SafeBrowsing")));
        assert!(in_use(&w, Path::new("/c/com.apple.Safari/Cache.db")), "the file itself");
        assert!(!in_use(&w, Path::new("/c/com.apple.Saf")), "a prefix is not a parent");
        assert!(!in_use(&w, Path::new("/c/nothing")));

        // a synced folder is a replica: moving a file out of one takes it off every other device,
        // and the shelf cannot undo that
        assert!(synced("Library/Mobile Documents/com~apple~CloudDocs/Downloads/old.zip"));
        assert!(synced("Library/CloudStorage/GoogleDrive-me/My Drive/old.zip"));
        assert!(synced("Dropbox/archive"));
        assert!(synced("Pictures/My Photos.photoslibrary/resources/derivatives"));
        assert!(!synced("Downloads/old.zip"), "the ordinary one still gets offered");

        // leftovers: only ever claimed for a reverse-DNS name, and never for Apple's own
        assert!(!app_gone("com.apple.Safari"), "plenty of Apple ids ship no .app at all");
        assert!(!app_gone("Google"), "a word is not a bundle id");
        assert!(!app_gone("com.foo"), "nor is one dot");
        if let Some(id) = installed_bundles().iter().next().cloned() {
            assert!(!app_gone(&id), "{id} is installed");
            assert!(!app_gone(&format!("{id}.Helper")), "a helper lives as long as its app");
            assert!(!app_gone(&format!("ABCDE12345.{id}")), "a group container wears a team id");
            assert!(app_gone("com.example.nothing.installed"));
            assert!(app_gone("ABCDE12345.com.example.nothing.installed"));
        }
    }

    /// Two rows that used to promise space they could not give: a folder hard-linked from a shared
    /// store, and a build folder the project itself tracks rather than ignores.
    #[test]
    fn promises_only_what_it_can_free() {
        let dir = std::env::temp_dir().join(format!("gunk-promise-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("dist")).unwrap();
        std::fs::create_dir_all(repo.join("out")).unwrap();
        std::fs::write(repo.join(".gitignore"), b"node_modules\n/dist/\n").unwrap();
        std::fs::write(repo.join("package.json"), b"{}").unwrap();
        std::fs::write(repo.join("dist/a.bin"), vec![0u8; 25 << 20]).unwrap();
        std::fs::write(repo.join("out/b.bin"), vec![0u8; 26 << 20]).unwrap();
        // a package store hard-linked into the project, which is how pnpm, uv and bun all work
        let store = dir.join("store");
        std::fs::create_dir_all(store.join("v1")).unwrap();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        std::fs::write(store.join("v1/lib.js"), vec![0u8; 4 << 20]).unwrap();
        std::fs::hard_link(store.join("v1/lib.js"), repo.join("node_modules/pkg/lib.js")).unwrap();

        let tree = scan::scan(&dir, &scan::Progress::new(&dir));
        let all = find_all(&dir, &tree);
        let by = |p: &str| all.iter().find(|c| c.path == p);

        // the project says dist is derived, so it is output; out is tracked, so it is a deliverable
        assert_eq!(by("repo/dist").map(|c| c.reason.as_str()), Some("build"));
        assert!(by("repo/out").is_none(), "a build folder the repo tracks is what somebody ships");

        // and the cache whose bytes belong to a store somewhere else says what removing it frees
        let nm = by("repo/node_modules").expect("still a cache");
        assert_eq!(nm.reason, "cache");
        assert!(nm.note.contains("hard-linked"), "must not promise bytes another path holds: {}", nm.note);
        assert!(nm.note.contains("frees nothing"), "{}", nm.note);
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
