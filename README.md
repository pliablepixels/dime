# DiMe

Di digs through your disk. Me watches your memory. Ru says what can go. Three explorers, one map.

## Install

macOS only (it leans on FSEvents, nettop, lsof and IOKit). Apple silicon or Intel.

1. **Rust** 1.85 or newer, if you do not have it:
   ```
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
2. **Build**:
   ```
   git clone <this repo> dime && cd dime
   cargo build --release
   ```
   Everything Rust needs is fetched by cargo. The page loads Three.js from a CDN, so the first open needs internet.
3. **Optional, for Ru** (the guru who validates what can go), any one of:
   - the [Claude Code CLI](https://claude.ai/code): install, run `claude` once to log in. Ru gets a read-only tool allowlist.
   - the [Codex CLI](https://github.com/openai/codex): `npm i -g @openai/codex`, run `codex` once to log in. Ru runs in Codex's sandbox: reads everywhere, writes only to a scratch folder.
   - any OpenAI-compatible endpoint, including a local [Ollama](https://ollama.com): Ru then judges from what Di shows it, without shell tools.

   `jq` helps Ru trim Di's JSON and ships with recent macOS (`brew install jq` on older ones). With none of these, Di and Me work as usual and Ru is greyed out with a note.

## Run

```
./target/release/dime
```

Opens http://127.0.0.1:4242 in your browser. `DIME_PORT=5000 ./target/release/dime` to use another port. The startup line lists anything optional it could not find.

**Full Disk Access.** To scan folders macOS protects (Mail, Messages, Safari data, other users), give your terminal app Full Disk Access in System Settings → Privacy & Security. Without it those folders simply show smaller.

DiMe keeps its own files in `~/.dime`: the vault (`vault/`), remembered per-root state (`state.json`), the last map of each root (`snapshots/`) and Ru's read-only helper (`bin/di`). Delete the folder to reset everything except what is in the vault, which you should restore or purge from the app first.

## What it does

**Di (disk)**: pick a drive or folder, Go. The map forms live while the scan runs (millions of files in seconds on a warm disk). Blocks are folders, area and height follow size, every folder carries an inset preview of its children. Colour by file type or by age. Click to explode into a folder, Esc to come back, right-click for Finder, Copy path, Hide, Send to Ru. The Cleanup tab leads with what is safe to free, in tiers (safe / probably safe / worth a look) with a plain-language reason for each item; rows unfold in place so you can tick things inside them. The idle slider narrows everything to files untouched for that long, and acting on a folder then moves only those files. Ticking adds to **your list**, which is only a list; nothing moves. Review the list to move items into the **vault** (`~/.dime/vault`, reversible, restorable in a later session) or, behind a second confirmation, delete them. The scan root is watched, so changes on disk show within a couple of seconds. The last map of each root reopens instantly from a snapshot; Rescan refreshes it.

**Me (memory)**: a live dashboard, updated every 2 s. Totals for memory, CPU, GPU, network and disk IO. An odd-behaviour list that flags runaway or bursty CPU, heavy or bursty network, uploads, memory growth, disk hammering, and large idle processes, each with a plain sentence and how long it has been going on. Ranked lanes for CPU, GPU, Network and Disk IO with 60 s sparklines, plus the top memory holders. Click any process for a drawer with six sparklines, the app path, and its open files grouped by folder. Click a folder or file to land there on the disk map. Per-process GPU comes from Metal's accumulated GPU time in IOKit, no root needed.

**Ru (guru)**: Di finds, Ru validates. The gear at the top right picks what Ru thinks with: Auto (first of Claude, Codex, an endpoint), or one of them, or none. The endpoint form takes a URL, model, and key; `DIME_RU`, `DIME_RU_URL`, `DIME_RU_MODEL`, `DIME_RU_KEY` (or `OPENAI_API_KEY`) do the same from the environment. The choice is saved in `~/.dime/settings.json` and switches without a restart. The drawer header says which one is answering. Ask about the selection, the folder in view, or the vault; Ru gets what Di knows about it, can query Di's index (`di tree`, `di flagged`, `di find`, `di idle`) and inspect the disk with read-only commands only, then answers with a verdict per item. Verdicts become a button that adds Ru's picks to your list; the move or delete still happens only from the list. Drag rows or hold a block on the map to hand Ru something. Everything Ru is given is shown under "show what Ru sees".

Backend: Rust (axum, rayon, sysinfo, notify). Frontend: one HTML + one JS file, Three.js from CDN, no build step.
