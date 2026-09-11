//! Di's rules for what can go, as data. Built-ins live in `rules.toml` next to this file;
//! `~/.dime/rules.toml` adds rules ahead of them and can switch built-ins off by id.
use serde::{Deserialize, Deserializer};
use std::path::PathBuf;

pub const BUILTIN: &str = include_str!("rules.toml");
/// safe/likely/review are recommendations. `note` is not: it is inventory DiMe shows without
/// suggesting anything, and it stays out of the headline totals.
pub const TIERS: &[&str] = &["safe", "likely", "review", "note"];

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub tier: String,
    pub what: String,
    pub note: String,
    #[serde(default = "one")]
    pub weight: f64,
    pub dir: Option<bool>,
    #[serde(default)]
    pub name: Vec<String>,
    #[serde(default)]
    pub ext: Vec<String>,
    /// The folder holding it ends with one of these. A list for the same reason `under` is one:
    /// the same rule usually covers several folders laid out alike, and saying so should not mean
    /// copying the rule out once per folder.
    #[serde(default, deserialize_with = "one_or_many")]
    pub parent_ends_with: Vec<String>,
    /// Any of these as an ancestor folder's name. A list, because one rule usually covers several
    /// tools laid out the same way, and teaching DiMe a new one should be adding a word.
    #[serde(default, deserialize_with = "one_or_many")]
    pub under: Vec<String>,
    #[serde(default)]
    pub has_child: Vec<String>,
    /// Names that must sit *beside* the item, in the same folder. A node_modules next to a
    /// package.json will be rebuilt by a command; one with no package.json is an orphan.
    #[serde(default)]
    pub has_sibling: Vec<String>,
    /// Names that must NOT sit beside it, the other half of the same question.
    #[serde(default)]
    pub no_sibling: Vec<String>,
    #[serde(default, deserialize_with = "size")]
    pub min_size: u64,
    #[serde(default)]
    pub min_age_days: i64,
    /// The tool that owns this, if one does. Deleting then runs this command instead of unlinking
    /// files, so the tool's own bookkeeping stays in step: remove Ollama's blobs by hand and
    /// `ollama ls` goes on listing a model whose weights are gone.
    ///
    /// Split on whitespace into argv and run without a shell, so each placeholder is one argument:
    ///   `{name}`  what the thing is called on disk, filled in for any rule.
    ///   `{model}` the name the tool knows it by, when DiMe can work that out. Only Ollama needs
    ///             this, because it addresses weights by hash; anything storing a model as a
    ///             recognisably named folder wants `{name}`.
    /// A command still holding a placeholder DiMe could not fill is never run.
    /// It runs as you, from a file only you can write, so it can run anything you can.
    pub remove_with: Option<String>,
    /// Only match when the project itself says this folder is derived, i.e. git ignores it. A
    /// `dist` in .gitignore is written again by the next build; a `dist` that is committed is what
    /// somebody ships. Outside a git repo there is no such signal and the matcher stands aside.
    /// One of the disk checks: see `DiskCheck` and `Rule::disk_checks`.
    #[serde(default)]
    pub git_ignored: bool,
    /// Only match when the folder is named for an application bundle that is no longer installed:
    /// what an uninstall left behind. The other disk check.
    #[serde(default)]
    pub app_gone: bool,
    /// This is a container, not a thing to remove: match it only to say "keep looking inside".
    /// Without it, naming a folder stops the walk there and every finer rule below is unreachable.
    #[serde(default)]
    pub descend: bool,
}
fn one() -> f64 {
    1.0
}
/// `under = ".claude"` and `under = [".claude", ".codex"]` both mean the same kind of thing.
fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V { One(String), Many(Vec<String>) }
    Ok(match V::deserialize(d)? {
        V::One(s) => vec![s],
        V::Many(v) => v,
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    disable: Vec<String>,
    /// Anything a running process holds open drops to this tier whatever its rule decided, because
    /// live files are not safe to move. Empty string switches the check off.
    open_tier: Option<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<Rule>,
}

/// The rule set plus the settings that apply across all of it.
#[derive(Debug)]
pub struct Rules {
    pub rules: Vec<Rule>,
    /// None when the user switched the open-file check off.
    pub open_tier: Option<String>,
}

/// "20 MB", "1.5 GB", "512" (bytes)
fn size<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum S { N(u64), T(String) }
    match S::deserialize(d)? {
        S::N(n) => Ok(n),
        S::T(t) => parse_size(&t).ok_or_else(|| serde::de::Error::custom(format!("bad size {t:?}: use e.g. \"20 MB\""))),
    }
}
pub fn parse_size(t: &str) -> Option<u64> {
    let t = t.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let n: f64 = t[..split].parse().ok()?;
    let mult = match t[split..].trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "KB" | "K" => 1024.0,
        "MB" | "M" => 1024.0 * 1024.0,
        "GB" | "G" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as u64)
}

/// What a rule is matched against: one entry in the tree.
pub struct Item<'a> {
    pub name: &'a str,
    pub is_dir: bool,
    /// lower-case extension, no dot
    pub ext: &'a str,
    pub size: u64,
    pub age_days: i64,
    /// False when the file carries no usable timestamp, which macOS leaves at the epoch on some
    /// system files. Unknown is not the same as ancient, so it must never satisfy an age floor.
    pub age_known: bool,
    /// path of the folder holding it, relative to the scan root ("" at the top)
    pub parent: &'a str,
    pub children: &'a [String],
    /// what else sits in the same folder
    pub siblings: &'a [String],
}

/// A matcher that reads the filesystem rather than the scanned tree, and so cannot be answered by
/// `Rule::matches`: `Item` is built from the tree, and these need to open files beside the item or
/// look at what is installed on the machine. `gunk::walk` asks them after a rule matches, and a
/// rule that fails one was never a candidate at all.
///
/// Teaching DiMe a third is three edits, all named here: a `bool` field above, a variant here, and
/// the arm that answers it in `gunk::walk`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DiskCheck {
    /// git considers this folder derived output rather than something the project tracks
    GitIgnored,
    /// no application installed on this machine publishes this bundle identifier
    AppGone,
}

impl Rule {
    /// What this rule wants asked of the disk once `matches` has said yes. Every one must hold.
    pub fn disk_checks(&self) -> impl Iterator<Item = DiskCheck> + '_ {
        [(self.git_ignored, DiskCheck::GitIgnored), (self.app_gone, DiskCheck::AppGone)]
            .into_iter()
            .filter_map(|(want, c)| want.then_some(c))
    }

    /// Everything that can be decided from the scanned tree alone. The disk-backed half of the
    /// contract lives in `disk_checks`, so a rule is matched only when both agree.
    pub fn matches(&self, it: &Item) -> bool {
        self.dir.is_none_or(|d| d == it.is_dir)
            && (self.name.is_empty() || self.name.iter().any(|n| n == it.name))
            && (self.ext.is_empty() || self.ext.iter().any(|e| e == it.ext))
            && (self.parent_ends_with.is_empty() || self.parent_ends_with.iter().any(|p| it.parent.ends_with(p)))
            && (self.under.is_empty() || it.parent.split('/').any(|s| self.under.iter().any(|u| u == s)))
            && (self.has_child.is_empty() || it.children.iter().any(|c| self.has_child.contains(c)))
            && (self.has_sibling.is_empty() || it.siblings.iter().any(|c| self.has_sibling.contains(c)))
            && !it.siblings.iter().any(|c| self.no_sibling.contains(c))
            && it.size >= self.min_size
            && (self.min_age_days == 0 || (it.age_known && it.age_days >= self.min_age_days))
    }
}

pub fn user_file() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".dime").join("rules.toml")
}

fn parse(text: &str) -> Result<File, String> {
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    for r in &f.rules {
        if !TIERS.contains(&r.tier.as_str()) {
            return Err(format!("rule {:?}: tier must be one of {}", r.id, TIERS.join(", ")));
        }
    }
    Ok(f)
}

/// User rules first, then the built-ins minus the disabled ids.
pub fn merge(user: &str) -> Result<Rules, String> {
    let builtin = parse(BUILTIN).expect("built-in rules.toml is valid");
    let u = parse(user)?;
    let open_tier = u.open_tier.or(builtin.open_tier).filter(|t| !t.is_empty());
    if let Some(t) = &open_tier {
        if !TIERS.contains(&t.as_str()) {
            return Err(format!("open_tier must be one of {}, or \"\" to switch it off", TIERS.join(", ")));
        }
    }
    let mut rules = u.rules;
    rules.extend(builtin.rules.into_iter().filter(|r| !u.disable.contains(&r.id)));
    Ok(Rules { rules, open_tier })
}

/// The active rule set. A broken user file is reported once per load and ignored.
pub fn load() -> Rules {
    let user = std::fs::read_to_string(user_file()).unwrap_or_default();
    merge(&user).unwrap_or_else(|e| {
        eprintln!("DiMe: ignoring {}: {e}", user_file().display());
        merge("").unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item<'a>(name: &'a str, is_dir: bool, size: u64, age: i64, parent: &'a str) -> Item<'a> {
        Item { name, is_dir, ext: name.rsplit('.').next().unwrap_or(""), size, age_days: age, age_known: true, parent, children: &[], siblings: &[] }
    }

    #[test]
    fn builtin_and_user_rules() {
        let b = merge("").unwrap().rules;
        assert_eq!(b[0].id, "venv-nolock"); // the most specific cache rule is tried first
        assert!(b.iter().any(|r| r.id == "downloads"));
        let nm = Item { siblings: &["package.json".into()], ..item("node_modules", true, 5 << 20, 0, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&nm)).unwrap().id, "cache");
        let dl = item("x.bin", false, 5 << 20, 40, "Downloads/sub");
        assert_eq!(b.iter().find(|r| r.matches(&dl)).unwrap().id, "downloads");
        let recent = item("x.bin", false, 5 << 20, 3, "Downloads");
        assert!(!b.iter().any(|r| r.matches(&recent)));

        let user = r#"
disable = ["downloads"]
[[rule]]
id = "docker"
tier = "review"
what = "Docker"
note = "n"
dir = true
name = ["node_modules"]
min_size = "1 MB"
"#;
        let m = merge(user).unwrap().rules;
        assert_eq!(m[0].id, "docker");
        assert_eq!(m.iter().find(|r| r.matches(&nm)).unwrap().id, "docker"); // user rule wins over the built-in
        assert!(m.iter().all(|r| r.id != "downloads"));
        assert!(m.iter().find(|r| r.matches(&dl)).is_none_or(|r| r.id != "downloads"));

        // a rule with parent_ends_with and no name flags each child, and leaves the folder holding
        // them unmatched so the walk keeps going: that is what stops a 34 GB all-or-nothing row
        let holder = item("iOS DeviceSupport", true, 34 << 30, 400, "Library/Developer/Xcode");
        assert!(!b.iter().any(|r| r.matches(&holder)), "the folder itself must stay unflagged");
        let version = item("iPhone15,2 17.3 (21D50)", true, 6 << 30, 400, "Library/Developer/Xcode/iOS DeviceSupport");
        assert_eq!(b.iter().find(|r| r.matches(&version)).unwrap().id, "devicesupport");
        let sim = item("AAAA-1111", true, 3 << 30, 400, "Library/Developer/CoreSimulator/Devices");
        assert_eq!(b.iter().find(|r| r.matches(&sim)).unwrap().id, "simulator-device");
        // matched even when it looks freshly used, so the walk stops here instead of wandering inside
        let fresh = item("iPhone18,2 26.6 (23G71)", true, 6 << 30, 0, "Library/Developer/Xcode/iOS DeviceSupport");
        assert_eq!(b.iter().find(|r| r.matches(&fresh)).unwrap().id, "devicesupport");
        // a runtime image is claimed before the generic disk-image rule, which would give the wrong advice
        let dmg = item("A088375A.dmg", false, 7 << 30, 400, "Library/Developer/CoreSimulator/Images");
        assert_eq!(b.iter().find(|r| r.matches(&dmg)).unwrap().id, "simruntime");

        assert!(merge("[[rule]]\nid=\"x\"\ntier=\"nope\"\nwhat=\"\"\nnote=\"\"").unwrap_err().contains("tier"));
        assert!(merge("[[rule]]\nid=\"x\"\ntier=\"safe\"\nwhat=\"\"\nnote=\"\"\nbogus=1").is_err());
        // no usable timestamp is unknown, not ancient: an age floor must not treat it as old
        let mut undated = item("thing.dmg", false, 8 << 30, 20_000, "Library/Developer/CoreDevice");
        undated.age_known = false;
        assert!(b.iter().find(|r| r.matches(&undated)).is_none_or(|r| r.min_age_days == 0));
        let dated = item("thing.dmg", false, 8 << 30, 20_000, "Library/Developer/CoreDevice");
        assert_eq!(b.iter().find(|r| r.matches(&dated)).unwrap().id, "installer");

        // A virtualenv is the one cache that can hold work nothing recorded. With a manifest beside
        // it, one command puts it back and it is safe; without one, nothing on disk says what is
        // installed, so it must not read as safe however much it looks like every other cache.
        let venv = |sibs: &'static [String]| Item { siblings: sibs, ..item(".venv", true, 200 << 20, 100, "proj") };
        let locked = venv(Box::leak(Box::new(["pyproject.toml".to_string()])));
        assert_eq!(b.iter().find(|r| r.matches(&locked)).unwrap().id, "cache");
        assert_eq!(b.iter().find(|r| r.matches(&locked)).unwrap().tier, "safe");
        let loose = venv(Box::leak(Box::new(["README.md".to_string()])));
        assert_eq!(b.iter().find(|r| r.matches(&loose)).unwrap().id, "venv-nolock");
        assert_eq!(b.iter().find(|r| r.matches(&loose)).unwrap().tier, "review");

        // a cache beside its project is rebuilt by a command; the same folder orphaned is dead weight
        let live = Item { siblings: &["package.json".into()], ..item("node_modules", true, 5 << 20, 400, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&live)).unwrap().id, "cache");
        let orphan = Item { siblings: &["README.md".into()], ..item("node_modules", true, 5 << 20, 400, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&orphan)).unwrap().id, "orphan-cache");

        // Naming a folder in the container list with no finer rule underneath is not a container,
        // it is a blind spot: the walk passes through and nothing ever claims the bytes.
        let containers: Vec<&str> = b.iter().filter(|r| r.descend).flat_map(|r| r.name.iter().map(String::as_str)).collect();
        for blind in ["uv", "pip", "torch", "xet"] {
            assert!(!containers.contains(&blind), "{blind} is walked through and nothing below it matches");
        }
        // and each of those now lands somewhere
        assert_eq!(b.iter().find(|r| r.matches(&item("uv", true, 2 << 30, 10, "Users/x/.cache"))).unwrap().tier, "safe");
        assert_eq!(b.iter().find(|r| r.matches(&item("torch", true, 2 << 30, 10, "Users/x/.cache"))).unwrap().id, "torch-cache");
        assert_eq!(b.iter().find(|r| r.matches(&item("xet", true, 2 << 30, 10, "Users/x/.cache/huggingface"))).unwrap().id, "hf-xet");
        // weights a library cached are not the output of a run on this machine, whatever the folder
        // is called: the checkpoints under torch/hub come down again on demand
        let cached = item("checkpoints", true, 2 << 30, 200, "Users/x/.cache/torch/hub");
        assert_eq!(b.iter().find(|r| r.matches(&cached)).unwrap().id, "torch-cache");
        let produced = item("checkpoints", true, 2 << 30, 200, "Users/x/proj/train");
        assert_eq!(b.iter().find(|r| r.matches(&produced)).unwrap().id, "run-output");

        // inventory stays out of the recommendation tiers
        let big = item("movie.mov", false, 3 << 30, 10, "Movies");
        assert_eq!(b.iter().find(|r| r.matches(&big)).unwrap().tier, "note");
        assert!(merge("").unwrap().open_tier.as_deref() == Some("review"));
        assert!(merge("open_tier = \"\"").unwrap().open_tier.is_none());
        assert!(merge("open_tier = \"nope\"").unwrap_err().contains("open_tier"));

        assert_eq!(parse_size("20 MB"), Some(20 << 20));
        assert_eq!(parse_size("1.5gb"), Some(3 << 29));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("2 lightyears"), None);
    }

    /// Things a machine used for AI work holds that no download brings back. None of them may ever
    /// be presented as safe or probably safe, whatever else changes in the rule file.
    #[test]
    fn nothing_irreplaceable_is_ever_suggested() {
        let b = merge("").unwrap().rules;
        let hit = |it: &Item| b.iter().find(|r| r.matches(it)).unwrap_or_else(|| panic!("nothing matched {}", it.name));
        let cases = [
            // a coding agent's own memory: past sessions, and what resuming one reads back
            ("agent-sessions", item("projects", true, 3 << 30, 40, "Users/x/.claude")),
            ("agent-sessions", item("sessions", true, 3 << 30, 40, "Users/x/.codex")),
            ("agent-sessions", item("todos", true, 100 << 20, 40, "Users/x/.claude")),
            // embeddings: rebuilt only if you still have every document, and only by paying again
            ("vector-store", item("chroma", true, 200 << 20, 90, "Users/x/.cache")),
            ("vector-store", item("lancedb", true, 200 << 20, 90, "Users/x/proj/data")),
            ("vector-store", item("vector_store", true, 50 << 20, 400, "Users/x/proj")),
            // what a training run produced on this machine
            ("run-output", item("checkpoints", true, 8 << 30, 200, "Users/x/proj")),
            ("run-output", item("lora", true, 2 << 30, 200, "Users/x/proj/train")),
            ("run-output", item("wandb", true, 500 << 20, 200, "Users/x/proj")),
        ];
        for (id, it) in &cases {
            let r = hit(it);
            assert_eq!(r.id, *id, "{} matched {} instead", it.name, r.id);
            assert_eq!(r.tier, "note", "{} is irreplaceable and must stay out of the recommendations", it.name);
        }

        // the disposable half of the same folders is still worth offering
        let scratch = item("shell-snapshots", true, 100 << 20, 40, "Users/x/.claude");
        assert_eq!(hit(&scratch).id, "agent-scratch");
        assert_eq!(hit(&scratch).tier, "likely");

        // and a model store is a recommendation, never a silent one: it says what removal costs
        let blob = Item { ..item("sha256-abc", false, 5 << 30, 40, "Users/x/.ollama/models/blobs") };
        let r = hit(&blob);
        assert_eq!(r.id, "ollama-model");
        assert_eq!(r.remove_with.as_deref(), Some("ollama rm {model}"), "Ollama removes its own models");
        assert!(r.note.contains("ollama create"), "says when nothing will bring it back");
    }
}
