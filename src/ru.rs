//! Ru's brains. Whatever the machine has: the Claude Code CLI, the Codex CLI, or any OpenAI-compatible
//! endpoint (Ollama included). Each adapter turns its own output into one plain event stream the page reads:
//! {"t":"provider","d":label} {"t":"text","d":delta} {"t":"tool","d":command} {"t":"denied","d":command} {"t":"done","turns":bool,"error":str?}
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// What the user chose in the gear menu, kept in ~/.dime/settings.json. Env vars fill the gaps.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    /// auto | claude | codex | api | none
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key: String,
}
fn settings_file() -> PathBuf {
    crate::vault::dir().parent().unwrap().join("settings.json")
}
pub fn load_settings() -> Settings {
    std::fs::read(settings_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}
pub fn save_settings(s: &Settings) -> std::io::Result<()> {
    let f = settings_file();
    std::fs::create_dir_all(f.parent().unwrap())?;
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(tmp, f)
}
/// What the machine offers, for the gear menu.
#[derive(Serialize)]
pub struct Available {
    pub claude: bool,
    pub codex: bool,
    pub ollama: bool,
    pub url: String,
    pub model: String,
    pub key_set: bool,
}
pub fn available(cfg: &Settings) -> Available {
    let (url, key, model) = api_params(cfg);
    Available { claude: have("claude"), codex: have("codex"), ollama: have("ollama"), url: url.unwrap_or_default(), model, key_set: key.is_some() }
}
fn api_params(cfg: &Settings) -> (Option<String>, Option<String>, String) {
    let url = Some(cfg.url.clone()).filter(|u| !u.is_empty()).or_else(|| std::env::var("DIME_RU_URL").ok()).or_else(|| have("ollama").then(|| "http://127.0.0.1:11434/v1".to_string()));
    let key = Some(cfg.key.clone()).filter(|k| !k.is_empty()).or_else(|| std::env::var("DIME_RU_KEY").ok()).or_else(|| std::env::var("OPENAI_API_KEY").ok());
    let model = Some(cfg.model.clone()).filter(|m| !m.is_empty()).or_else(|| std::env::var("DIME_RU_MODEL").ok()).or_else(|| url.as_deref().filter(|u| u.contains("11434")).and_then(|_| ollama_model())).unwrap_or_else(|| "gpt-4o-mini".into());
    (url, key, model)
}

pub enum Provider {
    Claude,
    Codex,
    Api { url: String, key: Option<String>, model: String },
    None,
}
pub struct Ru {
    pub provider: Provider,
    pub label: String,
}

fn have(tool: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file())).unwrap_or(false)
}
fn ollama_model() -> Option<String> {
    let out = std::process::Command::new("ollama").arg("list").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().nth(1)?.split_whitespace().next().map(String::from)
}

/// Pick a brain. The gear setting (or `DIME_RU`) = claude | codex | api | none; auto tries Claude, then Codex, then an endpoint.
pub fn detect(cfg: &Settings) -> Ru {
    let want = if cfg.mode.is_empty() || cfg.mode == "auto" { std::env::var("DIME_RU").unwrap_or_default().to_lowercase() } else { cfg.mode.to_lowercase() };
    let api = || {
        let (url, key, model) = api_params(cfg);
        let url = url?;
        let host = url.split("//").nth(1).unwrap_or(&url).split('/').next().unwrap_or("api").to_string();
        Some(Ru { label: format!("{model} via {host}, no tools"), provider: Provider::Api { url, key, model } })
    };
    match want.as_str() {
        "claude" => Ru { provider: Provider::Claude, label: "Claude".into() },
        "codex" => Ru { provider: Provider::Codex, label: "Codex".into() },
        "api" => api().unwrap_or(Ru { provider: Provider::None, label: String::new() }),
        "none" => Ru { provider: Provider::None, label: String::new() },
        _ if have("claude") => Ru { provider: Provider::Claude, label: "Claude".into() },
        _ if have("codex") => Ru { provider: Provider::Codex, label: "Codex".into() },
        _ => api().unwrap_or(Ru { provider: Provider::None, label: String::new() }),
    }
}

/// Read-only shell for Claude: every entry must be unable to write even with its own flags
/// (no find -delete/-exec, awk, sort -o, xattr -w, plutil -convert, codesign --remove-signature, sysctl -w, git remote add).
const CLAUDE_TOOLS: &[&str] = &[
    "Read", "Glob", "Grep", "Bash(di:*)", "Bash(jq:*)",
    "Bash(ls:*)", "Bash(du:*)", "Bash(df:*)", "Bash(file:*)", "Bash(stat:*)", "Bash(mdls:*)", "Bash(mdfind:*)", "Bash(head:*)", "Bash(tail:*)", "Bash(wc:*)", "Bash(strings:*)", "Bash(xattr -l:*)", "Bash(xattr -p:*)", "Bash(plutil -p:*)", "Bash(codesign -d:*)", "Bash(codesign -dv:*)", "Bash(spctl --assess:*)",
    "Bash(ps:*)", "Bash(pgrep:*)", "Bash(lsof:*)", "Bash(top:*)", "Bash(vm_stat:*)", "Bash(sysctl -n:*)", "Bash(launchctl list:*)", "Bash(launchctl print:*)", "Bash(diskutil list:*)", "Bash(diskutil info:*)", "Bash(tmutil listlocalsnapshots:*)", "Bash(brew list:*)", "Bash(brew info:*)", "Bash(npm ls:*)", "Bash(pip list:*)", "Bash(sw_vers:*)", "Bash(uname:*)", "Bash(id:*)", "Bash(whoami:*)",
    "Bash(cat:*)", "Bash(grep:*)", "Bash(egrep:*)", "Bash(uniq:*)", "Bash(cut:*)", "Bash(tr:*)", "Bash(date:*)", "Bash(basename:*)", "Bash(dirname:*)", "Bash(realpath:*)", "Bash(readlink:*)", "Bash(defaults read:*)", "Bash(xcode-select -p:*)", "Bash(xcrun simctl list:*)", "Bash(xcrun simctl runtime list:*)", "Bash(xcrun --find:*)", "Bash(mount:*)", "Bash(hdiutil info:*)", "Bash(xcodebuild -version:*)", "Bash(git status:*)", "Bash(git log:*)", "Bash(git remote -v:*)", "Bash(otool -L:*)", "Bash(mdutil -s:*)",
];

async fn emit(w: &mut DuplexStream, v: serde_json::Value) -> std::io::Result<()> {
    w.write_all(format!("{v}\n").as_bytes()).await
}
fn cmd_of(c: &str) -> String {
    c.strip_prefix("/bin/zsh -lc ").or_else(|| c.strip_prefix("/bin/bash -lc ")).map(|s| s.trim_matches('\'')).unwrap_or(c).lines().next().unwrap_or("").to_string()
}

/// Run one question. Returns the readable half of a pipe carrying the event stream; dropping it ends the run.
pub fn ask(ru: &Ru, system: String, prompt: String, root: PathBuf, path_env: String) -> Result<DuplexStream, String> {
    let (mut w, r) = tokio::io::duplex(1 << 16);
    let label = ru.label.clone();
    match &ru.provider {
        Provider::None => return Err("Ru has no AI to think with. Install the Claude Code CLI or Codex and log in once, run Ollama, or pick an endpoint in the gear menu.".into()),
        Provider::Claude => { tokio::spawn(async move { let e = claude(&mut w, &label, system, prompt, root, path_env).await; finish(&mut w, e).await; }); }
        Provider::Codex => { tokio::spawn(async move { let e = codex(&mut w, &label, system, prompt, root, path_env).await; finish(&mut w, e).await; }); }
        Provider::Api { url, key, model } => {
            let (url, key, model) = (url.clone(), key.clone(), model.clone());
            tokio::spawn(async move { let e = api(&mut w, &label, url, key, model, system, prompt).await; finish(&mut w, e).await; });
        }
    }
    Ok(r)
}
async fn finish(w: &mut DuplexStream, r: std::io::Result<Option<String>>) {
    let _ = match r {
        Ok(None) => Ok(()),
        Ok(Some(msg)) => emit(w, serde_json::json!({ "t": "done", "turns": false, "error": msg })).await,
        Err(e) => emit(w, serde_json::json!({ "t": "done", "turns": false, "error": e.to_string() })).await,
    };
    let _ = w.shutdown().await;
}

async fn claude(w: &mut DuplexStream, label: &str, system: String, prompt: String, root: PathBuf, path_env: String) -> std::io::Result<Option<String>> {
    emit(w, serde_json::json!({ "t": "provider", "d": label })).await?;
    let mut cmd = tokio::process::Command::new("claude");
    cmd.env("PATH", path_env)
        // --bare would also drop the login, so isolate piecemeal: no user settings or hooks, no MCP servers
        .args(["-p", "--setting-sources", "", "--strict-mcp-config", "--mcp-config", r#"{"mcpServers":{}}"#, "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--no-session-persistence", "--max-turns", "24"])
        .args(["--tools", "Read,Glob,Grep,Bash"]).arg("--allowedTools").args(CLAUDE_TOOLS)
        .arg("--append-system-prompt").arg(&system)
        .arg("--add-dir").arg(&root).arg("--add-dir").arg(crate::vault::dir())
        .current_dir(if root.is_dir() { root.clone() } else { PathBuf::from("/") })
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| std::io::Error::other(format!("could not start claude: {e}")))?;
    let mut stdin = child.stdin.take().unwrap();
    tokio::spawn(async move { let _ = stdin.write_all(prompt.as_bytes()).await; });
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let (mut saw_text, mut last_tool) = (false, false);
    while let Some(line) = lines.next_line().await? {
        let Ok(d) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match d["type"].as_str() {
            Some("stream_event") => {
                let ev = &d["event"];
                if ev["type"] == "content_block_start" {
                    let kind = ev["content_block"]["type"].as_str().unwrap_or("");
                    if kind == "text" && last_tool && saw_text { emit(w, serde_json::json!({ "t": "text", "d": "\n\n" })).await?; }
                    last_tool = kind == "tool_use";
                }
                if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                    if let Some(t) = ev["delta"]["text"].as_str() { saw_text = true; emit(w, serde_json::json!({ "t": "text", "d": t })).await?; }
                }
            }
            Some("assistant") => {
                for c in d["message"]["content"].as_array().cloned().unwrap_or_default() {
                    if c["type"] == "tool_use" {
                        let i = &c["input"];
                        let what = i["command"].as_str().or(i["file_path"].as_str()).or(i["pattern"].as_str()).or(i["path"].as_str()).unwrap_or(c["name"].as_str().unwrap_or("tool"));
                        emit(w, serde_json::json!({ "t": "tool", "d": what.lines().next().unwrap_or("") })).await?;
                    }
                }
            }
            Some("result") => {
                for x in d["permission_denials"].as_array().cloned().unwrap_or_default() {
                    let c = x["tool_input"]["command"].as_str().or(x["tool_name"].as_str()).unwrap_or("");
                    emit(w, serde_json::json!({ "t": "denied", "d": c.lines().next().unwrap_or("") })).await?;
                }
                if !saw_text { if let Some(t) = d["result"].as_str() { emit(w, serde_json::json!({ "t": "text", "d": t })).await?; } }
                emit(w, serde_json::json!({ "t": "done", "turns": d["subtype"] == "error_max_turns", "error": if d["is_error"] == true && !saw_text { serde_json::Value::String("Ru could not answer. Is `claude` logged in? Run `claude` once.".into()) } else { serde_json::Value::Null } })).await?;
                return Ok(None);
            }
            _ => {}
        }
    }
    Ok(Some("claude ended without an answer".into()))
}

async fn codex(w: &mut DuplexStream, label: &str, system: String, prompt: String, _root: PathBuf, path_env: String) -> std::io::Result<Option<String>> {
    emit(w, serde_json::json!({ "t": "provider", "d": label })).await?;
    // Seatbelt sandbox: writes only inside an empty scratch folder, reads everywhere, network on so `di` can reach Di's index
    let scratch = crate::vault::dir().parent().unwrap().join("ru-scratch");
    std::fs::create_dir_all(&scratch)?;
    let mut cmd = tokio::process::Command::new("codex");
    cmd.env("PATH", path_env)
        .args(["exec", "--json", "--sandbox", "workspace-write", "-c", "sandbox_workspace_write.network_access=true", "--skip-git-repo-check", "-C"]).arg(&scratch).arg("-")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| std::io::Error::other(format!("could not start codex: {e}")))?;
    let mut stdin = child.stdin.take().unwrap();
    let input = format!("{system}\n\nEverything below is what the user is looking at and what they asked.\n\n{prompt}");
    tokio::spawn(async move { let _ = stdin.write_all(input.as_bytes()).await; });
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut saw_text = false;
    while let Some(line) = lines.next_line().await? {
        let Ok(d) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let it = &d["item"];
        match (d["type"].as_str(), it["type"].as_str()) {
            (Some("item.completed"), Some("agent_message")) => {
                if let Some(t) = it["text"].as_str() { emit(w, serde_json::json!({ "t": "text", "d": format!("{}{t}", if saw_text { "\n\n" } else { "" }) })).await?; saw_text = true; }
            }
            (Some("item.started"), Some("command_execution")) => { emit(w, serde_json::json!({ "t": "tool", "d": cmd_of(it["command"].as_str().unwrap_or("")) })).await?; }
            (Some("item.completed"), Some("command_execution")) => {
                if it["exit_code"].as_i64().unwrap_or(0) != 0 && it["aggregated_output"].as_str().unwrap_or("").contains("Operation not permitted") {
                    emit(w, serde_json::json!({ "t": "denied", "d": cmd_of(it["command"].as_str().unwrap_or("")) })).await?;
                }
            }
            (Some("turn.completed"), _) => { emit(w, serde_json::json!({ "t": "done", "turns": false, "error": serde_json::Value::Null })).await?; return Ok(None); }
            (Some("turn.failed"), _) | (Some("error"), _) => { return Ok(Some(d["error"]["message"].as_str().or(d["message"].as_str()).unwrap_or("codex failed").to_string())); }
            _ => {}
        }
    }
    Ok(if saw_text { None } else { Some("codex ended without an answer. Run `codex` once to log in.".into()) })
}

/// Any OpenAI-compatible chat endpoint, streamed through curl (present on every Mac). No tools: Ru judges from the context alone.
async fn api(w: &mut DuplexStream, label: &str, url: String, key: Option<String>, model: String, system: String, prompt: String) -> std::io::Result<Option<String>> {
    emit(w, serde_json::json!({ "t": "provider", "d": label })).await?;
    let system = format!("{system}\nNOTE: in this setup you have no shell and cannot inspect the disk or ask Di anything beyond the context below. Judge from that, and say plainly what you could not verify.");
    let body = serde_json::json!({ "model": model, "stream": true, "messages": [{ "role": "system", "content": system }, { "role": "user", "content": prompt }] });
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-sN", "-X", "POST", "-H", "content-type: application/json", "-d", "@-"]);
    if let Some(k) = &key { cmd.arg("-H").arg(format!("authorization: Bearer {k}")); }
    cmd.arg(format!("{}/chat/completions", url.trim_end_matches('/'))).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| std::io::Error::other(format!("could not start curl: {e}")))?;
    let mut stdin = child.stdin.take().unwrap();
    tokio::spawn(async move { let _ = stdin.write_all(body.to_string().as_bytes()).await; });
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let (mut saw_text, mut raw) = (false, String::new());
    while let Some(line) = lines.next_line().await? {
        let Some(data) = line.strip_prefix("data:") else { if !line.trim().is_empty() { raw.push_str(&line); } continue };
        let data = data.trim();
        if data == "[DONE]" { break; }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        if let Some(t) = d["choices"][0]["delta"]["content"].as_str() { if !t.is_empty() { saw_text = true; emit(w, serde_json::json!({ "t": "text", "d": t })).await?; } }
        if let Some(e) = d["error"]["message"].as_str() { return Ok(Some(e.to_string())); }
    }
    if saw_text { emit(w, serde_json::json!({ "t": "done", "turns": false, "error": serde_json::Value::Null })).await?; return Ok(None); }
    let err = serde_json::from_str::<serde_json::Value>(&raw).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| if raw.is_empty() { format!("no answer from {url}") } else { raw.chars().take(200).collect() });
    Ok(Some(err))
}

/// Ru's helper `di`, a GET-only shell wrapper over Di's index. Written to ~/.dime/bin; returns the folder to put on PATH.
pub fn di_helper(port: &str) -> std::io::Result<PathBuf> {
    let dir = crate::vault::dir().parent().unwrap().join("bin");
    std::fs::create_dir_all(&dir)?;
    let script = format!(r#"#!/bin/sh
# di: query DiMe's in-memory index (read-only). Written by DiMe on each Ru question.
B='http://127.0.0.1:{port}/api'
case "$1" in
  tree)    curl -sG "$B/tree" --data-urlencode "path=$2" --data-urlencode "depth=${{3:-1}}" ;;
  flagged) curl -sG "$B/gunk" --data-urlencode "path=$2" ;;
  find)    curl -sG "$B/find" --data-urlencode "q=$2" --data-urlencode "path=$3" --data-urlencode "limit=${{4:-100}}" ;;
  idle)    curl -sG "$B/idle" --data-urlencode "path=$2" --data-urlencode "days=${{3:-180}}" ;;
  *) echo "usage: di tree <rel> [depth] | di flagged <rel> | di find <q> [rel] [limit] | di idle <rel> <days>" >&2; exit 2 ;;
esac
echo
"#);
    let f = dir.join("di");
    if std::fs::read_to_string(&f).ok().as_deref() != Some(script.as_str()) {
        let tmp = dir.join(format!("di.{}", std::process::id()));
        std::fs::write(&tmp, &script)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &f)?;
    }
    Ok(dir)
}

pub fn path_with(dir: &Path) -> String {
    format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default())
}
