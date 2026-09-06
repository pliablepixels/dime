# DuMe

Du digs through your disk. Me watches your memory. Two explorers, one map.

```
cargo run --release
```

Opens http://127.0.0.1:4242. Two explorers share one 3D map.

**Du (disk)**: pick a drive or folder, Go. The map forms live while the scan runs (2.6M files in ~18s). Blocks are folders, area and height follow size, every folder carries an inset preview of its children. Colour by file type or by age. Click to explode into a folder, Esc to come back, right-click for Finder, Copy path, Explore. The Cleanup tab leads with what is safe to free, in tiers (safe / probably safe / worth a look) with a plain-language reason for each item. Filters and an idle slider re-lay the map to only what matches. The scan root is watched, so deleting in Finder updates the map within ~2s. No delete actions in the app.

**Me (memory)**: a live dashboard, updated every 2 s. Totals for memory, CPU, GPU, network and disk IO. An odd-behaviour list that flags runaway or bursty CPU, heavy or bursty network, uploads, memory growth, disk hammering, and large idle processes, each with a plain sentence and how long it has been going on. Ranked lanes for CPU, GPU, Network and Disk IO with 60 s sparklines, plus the top memory holders. Click any process for a drawer with six sparklines, the app path, and its open files grouped by folder, with "writing now" tags from file mtimes. Click a folder or file to land there on the disk map, scanning it first if needed. Per-process GPU comes from Metal's accumulated GPU time in IOKit, no root needed.

Backend: Rust (axum, rayon, sysinfo, notify). Frontend: one HTML + one JS file, Three.js from CDN, no build step. macOS only (FSEvents, nettop, lsof, IOKit).
