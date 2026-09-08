#!/usr/bin/env python3
"""Re-vendor Three.js into static/vendor (run after bumping VERSION, then `python3 tools/gen-assets.py`)."""
import os, re, urllib.request, posixpath, shutil

VERSION = "0.170.0"
BASE = f"https://cdn.jsdelivr.net/npm/three@{VERSION}/"
OUT = "static/vendor"
ENTRIES = ["build/three.module.js"] + ["examples/jsm/" + p for p in [
    "controls/OrbitControls.js", "postprocessing/EffectComposer.js", "postprocessing/RenderPass.js",
    "postprocessing/UnrealBloomPass.js", "postprocessing/OutputPass.js", "renderers/CSS2DRenderer.js"]]
IMPORT = re.compile(r"""(?:from|import)\s+['"]([^'"]+)['"]""")

def local(rest):
    if rest == "build/three.module.js":
        return "three.module.js"
    return "addons/" + rest[len("examples/jsm/"):]

shutil.rmtree(OUT, ignore_errors=True)
seen, queue = set(), list(ENTRIES)
while queue:
    rest = queue.pop()
    if rest in seen:
        continue
    seen.add(rest)
    src = urllib.request.urlopen(BASE + rest).read().decode()
    p = os.path.join(OUT, local(rest))
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write(src)
    for spec in IMPORT.findall(src):
        if spec.startswith("."):
            queue.append(posixpath.normpath(posixpath.join(posixpath.dirname(rest), spec)))
        elif spec != "three":
            raise SystemExit(f"unresolved import {spec!r} in {rest}")
print(f"three {VERSION}: {len(seen)} files")
