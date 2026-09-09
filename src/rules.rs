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
    pub parent_ends_with: Option<String>,
    pub under: Option<String>,
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
    /// The tool that owns this, if one does. `ollama rm {name}` means DiMe must not unlink the
    /// files itself: it runs that command, so the tool's own bookkeeping stays in step. Without it
    /// Ollama keeps listing a model whose weights DiMe removed behind its back.
    ///
    /// Split on whitespace into argv and run without a shell; `{name}` becomes one argument.
    /// It runs as you, from a file only you can write, so it can run anything you can.
    pub remove_with: Option<String>,
    /// This is a container, not a thing to remove: match it only to say "keep looking inside".
    /// Without it, naming a folder stops the walk there and every finer rule below is unreachable.
    #[serde(default)]
    pub descend: bool,
}
fn one() -> f64 {
    1.0
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

impl Rule {
    pub fn matches(&self, it: &Item) -> bool {
        self.dir.is_none_or(|d| d == it.is_dir)
            && (self.name.is_empty() || self.name.iter().any(|n| n == it.name))
            && (self.ext.is_empty() || self.ext.iter().any(|e| e == it.ext))
            && self.parent_ends_with.as_deref().is_none_or(|p| it.parent.ends_with(p))
            && self.under.as_deref().is_none_or(|u| it.parent.split('/').any(|s| s == u))
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
        assert_eq!(b[0].id, "orphan-cache"); // the more specific cache rule is tried first
        assert!(b.iter().any(|r| r.id == "downloads"));
        let nm = Item { siblings: &["package.json".into()], ..item("node_modules", true, 5 << 20, 0, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&nm)).unwrap().id, "cache");
        let dl = item("x.bin", false, 5 << 20, 40, "Downloads/sub");
        assert_eq!(b.iter().find(|r| r.matches(&dl)).unwrap().id, "downloads");
        let recent = item("x.bin", false, 5 << 20, 3, "Downloads");
        assert!(b.iter().find(|r| r.matches(&recent)).is_none());

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
        assert!(b.iter().find(|r| r.matches(&holder)).is_none(), "the folder itself must stay unflagged");
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

        // a cache beside its project is rebuilt by a command; the same folder orphaned is dead weight
        let live = Item { siblings: &["package.json".into()], ..item("node_modules", true, 5 << 20, 400, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&live)).unwrap().id, "cache");
        let orphan = Item { siblings: &["README.md".into()], ..item("node_modules", true, 5 << 20, 400, "proj") };
        assert_eq!(b.iter().find(|r| r.matches(&orphan)).unwrap().id, "orphan-cache");

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
}
