# gunk — disk + memory offender explorer

## Goal
Point at a path (default: home). Scan fast. Explore in 3D. Surface files/dirs worth deleting and processes hogging RAM. Trash safely.

## Architecture
- `src/main.rs` — axum server bound to 127.0.0.1:4242, serves `static/` and JSON API.
- `src/scan.rs` — parallel recursive scan (rayon). Builds in-memory tree: name, size (disk blocks), mtime, atime, children sorted by size desc. Symlinks not followed. Errors skipped. `/dev`, `/Volumes`, `/System/Volumes` skipped when root is `/`.
- `src/gunk.rs` — candidate finder over tree: known junk dirs (node_modules, target, Caches, DerivedData, .cache, …), large files (>100MB), stale big files (>10MB, atime >180d). Score = size × (1 + stale_days/365).
- `static/index.html`, `static/app.js` — Three.js (CDN importmap). Squarified treemap extruded to 3D. Click dir = explode into children. Breadcrumb / Esc = back. Right panel: Gunk list (checkbox, trash), Memory list (auto-refresh).

## API
- `GET /api/home` → `{path}`
- `POST /api/scan {path}` → starts scan (spawn_blocking). `GET /api/status` → `{state: idle|scanning|done|error, files, root}`
- `GET /api/tree?path=<rel>&depth=1` → subtree, children capped at 60 + aggregated `…other`.
- `GET /api/gunk` → top 80 candidates `{path, size, reason, age_days}`
- `GET /api/memory` → `{total, used, procs:[{pid,name,rss,cpu}]}` top 30
- `POST /api/trash {path}` → moves to Trash; path must canonicalize under scan root and not equal it. Tree updated in place.

## Non-goals
Auth (localhost only). Multi-user. Persisting scans. Windows.

## Testing
Unit test for squarified treemap? No — visual. Rust: one test for gunk scoring + tree size aggregation on a temp dir. Manual: curl endpoints, open browser.
