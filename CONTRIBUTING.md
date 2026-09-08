# Contributing

## Build and run

```sh
cargo run --release          # serves on http://localhost:4242 and opens your browser
cargo run --release -- --window   # the same, in the app's own window
make app                     # dist/DiMe.app, universal, with its icon
cargo test                   # unit tests, no external services
```

The frontend is `static/index.html` and `static/app.js`, served from disk. Edit, hard-refresh the browser, no rebuild. Rust changes need a restart.

## Where things live

| Path | Role |
|------|------|
| `src/scan.rs` | Walks the disk into a `Node` tree, patches it on file-system events |
| `src/rules.toml`, `src/rules.rs` | What Di flags for removal, as data. Matching and loading |
| `src/gunk.rs` | Runs the rules over the tree, scores and groups candidates |
| `src/hog.rs` | Process sampling for Me |
| `src/ru.rs` | Ru's providers: Claude Code CLI, Codex CLI, OpenAI-compatible API |
| `src/shelf.rs` | The shelf: move items aside, put them back, delete them for good |
| `src/snapshot.rs` | Saves the finished tree so the next launch opens instantly |
| `src/main.rs` | HTTP routes, shared state, reset |
| `static/app.js` | The whole UI. Sections marked with `// ----` comments |
| `static/vendor/` | Three.js and the Manrope webfont, vendored so nothing loads from a CDN |
| `src/assets.rs` | Generated: every file under `static/` baked into the binary |
| `packaging/`, `tools/` | Icon, `Info.plist`, and the scripts that regenerate them |

The UI is embedded in the binary, but a `static/` folder in the working directory wins for any file it has, so running from the repo picks up your edits with a refresh.

## Regenerating what is generated

```sh
python3 tools/vendor-three.py   # re-fetch Three.js (bump VERSION in the script first)
python3 tools/vendor-font.py    # re-fetch the Manrope webfont
python3 tools/gen-assets.py     # rebuild src/assets.rs; run after adding any file under static/
make icon                       # redraw packaging/DiMe.icns from tools/make-icon.py
```

Server owns all state; the UI polls `/api/status` and re-fetches when `version` changes.

## Adding a rule for Di

Most contributions are rules. Edit `src/rules.toml`, no Rust needed. Order matters: first match wins, and a matched folder is not searched inside. Put specific rules above general ones. Every field is documented at the top of that file. Test it locally first as `~/.dime/rules.toml`, then move it into the built-in file.

Something the matchers cannot express (needs state across the walk, like duplicates): add a matcher field to `Rule` in `src/rules.rs` and honour it in `Rule::matches`, or add a built-in pass in `gunk::walk`. Add a case to the test in `rules.rs`.

## Adding a Ru provider

`src/ru.rs`: add a variant to `Provider`, pick it in `detect()`, and spawn it in `ask` like `claude()` and `codex()` do. Anything that speaks the OpenAI chat-completions API already works through the endpoint setting, so a new provider is only for a CLI with its own protocol. Providers must be read-only on the disk: Ru inspects, the user acts from the list.

## Before a PR

- `cargo test` passes.
- No new dependencies unless a few lines will not do.
- Keep the plain-language tone in anything the user reads.
