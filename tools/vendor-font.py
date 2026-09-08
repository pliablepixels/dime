#!/usr/bin/env python3
"""Re-vendor the Manrope webfont into static/vendor/font (then run tools/gen-assets.py)."""
import urllib.request, re, os, shutil

URL = "https://fonts.googleapis.com/css2?family=Manrope:wght@400;600;800&display=swap"
OUT = "static/vendor/font"
UA = {"User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
                    "(KHTML, like Gecko) Chrome/120 Safari/537.36"}  # decides which formats Google serves

shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)
css = urllib.request.urlopen(urllib.request.Request(URL, headers=UA)).read().decode()
seen = {}
def grab(m):
    u = m.group(1)
    if u not in seen:
        seen[u] = f"manrope-{len(seen)}.woff2"
        open(f"{OUT}/{seen[u]}", "wb").write(urllib.request.urlopen(urllib.request.Request(u, headers=UA)).read())
    return f"url(/vendor/font/{seen[u]})"
css = re.sub(r"url\((https://fonts\.gstatic\.com/[^)]+)\)", grab, css)
open(f"{OUT}/manrope.css", "w").write("/* Manrope, vendored from Google Fonts by tools/vendor-font.py */\n" + css)
print(f"manrope: {len(seen)} woff2 files")
