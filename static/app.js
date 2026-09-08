import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { CSS2DRenderer, CSS2DObject } from 'three/addons/renderers/CSS2DRenderer.js';

const $ = (s) => document.querySelector(s);
addEventListener('unhandledrejection', (e) => console.error('unhandled', e.reason?.stack || e.reason));
const api = async (url, body) => {
  const r = await fetch(url, body ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : undefined);
  if (!r.ok) throw new Error(await r.text());
  return r.headers.get('content-type')?.includes('json') ? r.json() : null;
};
const fmt = (b) => { const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0; while (b >= 1024 && i < 4) { b /= 1024; i++; } return (i ? b.toFixed(b < 10 ? 1 : 0) : b) + ' ' + u[i]; };
const fmtN = (n) => n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(0) + 'K' : String(n);
// A folder still being counted shows whole megabytes, because gigabytes tick over far too slowly to
// read as progress. The moment it is done, fmt takes over and picks the unit that fits.
const fmtScan = (b) => b < (1 << 20) ? fmt(b) : Math.round(b / (1 << 20)).toLocaleString() + ' MB';
const ageDays = (n) => { const t = Math.max(n.atime, n.mtime); return t > 0 ? Math.max(0, Math.floor((Date.now() / 1000 - t) / 86400)) : -1; }; // -1: no usable timestamp, which is unknown rather than ancient
const fmtAge = (d) => d < 0 ? 'age unknown' : d === 0 ? 'used today' : d < 30 ? `${d}d idle` : d < 365 ? `${Math.round(d / 30)}mo idle` : `${(d / 365).toFixed(1)}y idle`;
const REDUCED = matchMedia('(prefers-reduced-motion: reduce)').matches;
const toast = (msg, who) => { const t = $('#toast'); t.textContent = msg; t.className = who || ''; t.hidden = false; clearTimeout(toast.h); toast.h = setTimeout(() => (t.hidden = true), 2600); };
// the two characters: Di runs the disk search party, Me flies the rocket over live processes
const DU_GLYPH = '<svg viewBox="0 0 48 32" aria-hidden="true"><path d="M6 6h30M21 6v6M14 12h18l6 6v4H10l-4-4v-6zM8 26h14M4 22l6 4M36 22l-6-2"/></svg>';
const ME_GLYPH = '<svg viewBox="0 0 32 48" aria-hidden="true"><path d="M16 4c5 4 7 10 7 18v10H9V22c0-8 2-14 7-18zM9 26l-5 8h5M23 26l5 8h-5M13 40l3 6 3-6"/></svg>';
const whoBadge = (who) => { const b = document.createElement('span'); b.className = `who ${who}`; b.innerHTML = (who === 'du' ? DU_GLYPH : ME_GLYPH) + (who === 'du' ? 'Di' : 'Me'); b.title = who === 'du' ? 'Di · disk explorer' : 'Me · memory explorer'; return b; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------- squarified treemap ----------
function squarify(items, x, y, w, h) {
  const total = items.reduce((s, i) => s + i.size, 0) || 1;
  const min = total * 0.0025;
  const areas = items.map((i) => Math.max(i.size, min) / total);
  const norm = areas.reduce((s, a) => s + a, 0);
  const scaled = areas.map((a) => (a / norm) * w * h);
  const out = [];
  let i = 0;
  while (i < scaled.length) {
    const horiz = w >= h;
    const side = horiz ? h : w;
    let row = [], sum = 0, worst = Infinity;
    for (;;) {
      const a = scaled[i + row.length];
      if (a === undefined) break;
      const s2 = sum + a, len = s2 / side;
      const mx = Math.max(...row, a), mn = Math.min(...row, a);
      const ratio = Math.max((len * len) / mn, mx / (len * len));
      if (row.length && ratio > worst) break;
      row.push(a); sum = s2; worst = ratio;
    }
    const len = sum / side;
    let off = 0;
    row.forEach((a, k) => {
      const t = a / len;
      out.push(horiz ? { item: items[i + k], x, y: y + off, w: len, h: t } : { item: items[i + k], x: x + off, y, w: t, h: len });
      off += t;
    });
    if (horiz) { x += len; w -= len; } else { y += len; h -= len; }
    i += row.length;
  }
  return out;
}

// ---------- scene ----------
const STAGE_W = 120, STAGE_D = 80, GAP = 0.7;
const FULL = { x: -STAGE_W / 2, z: -STAGE_D / 2, w: STAGE_W, h: STAGE_D };
const COL_BLUE = new THREE.Color('#3B6FB6'), COL_DIM = new THREE.Color('#2A3142'), COL_FORM = new THREE.Color('#2E5C9E');
// age scale in six distinct bands, t = days/365: this week, this month, 3 months, 6 months, this year, older
const AGE_BANDS = [[7, '#3DDC97'], [30, '#4FD1C5'], [90, '#3B6FB6'], [180, '#8A5BC7'], [365, '#B24BB3'], [Infinity, '#6B6F8A']];
const AGE_COLORS = AGE_BANDS.map(([, c]) => new THREE.Color(c));
const heatColor = (t) => { const d = t >= 0 ? t * 365 : 0; return AGE_COLORS[Math.max(0, AGE_BANDS.findIndex(([max]) => d < max))].clone(); }; // NaN or negative age lands in the freshest band
const TYPE_NAMES = ['code', 'images', 'video', 'audio', 'documents', 'archives', 'data', 'models', 'apps', 'other'];
const TYPE_HEX = ['#4FD1C5', '#E07BB5', '#9B6BD6', '#5B8DEF', '#E8E6DF', '#F5C26B', '#7FB77E', '#FF9B5C', '#A0A8B8', '#46557A'];
const TYPE_COLOR = TYPE_HEX.map((c) => new THREE.Color(c));
const dominant = (types) => { let i = 9, m = -1; types?.forEach((v, k) => { if (v > m) { m = v; i = k; } }); return i; };
const typeShare = (n) => { const t = n.types; if (!t) return null; const tot = t.reduce((s, v) => s + v, 0) || 1; const i = dominant(t); return { i, share: t[i] / tot }; };
let colorMode = new URLSearchParams(location.search).get('c') || (() => { try { return localStorage.getItem('dime.color') || 'type'; } catch { return 'type'; } })();
if (colorMode !== 'age') colorMode = 'type';
const TIER_COLOR = { safe: new THREE.Color('#FF7A3D'), likely: new THREE.Color('#F5C26B'), review: new THREE.Color('#E8457A') };
const TIER_LABEL = { safe: 'Safe to remove', likely: 'Probably safe', review: 'Worth a look' };
const TIER_HEX = { safe: '#FF7A3D', likely: '#F5C26B', review: '#E8457A' };
const HOME_CAM = new THREE.Vector3(-14, 150, 140);

const renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'high-performance' });
renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
renderer.setSize(innerWidth, innerHeight);
$('#stage').appendChild(renderer.domElement);
const labelR = new CSS2DRenderer({ element: $('#labels') });
labelR.setSize(innerWidth, innerHeight);

const scene = new THREE.Scene();
scene.background = new THREE.Color('#0D1321');
scene.fog = new THREE.Fog('#0D1321', 260, 900);
const camera = new THREE.PerspectiveCamera(42, innerWidth / innerHeight, 1, 1600);
camera.position.copy(HOME_CAM);
const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true; controls.dampingFactor = 0.08;
controls.maxPolarAngle = Math.PI * 0.47; controls.minDistance = 40; controls.maxDistance = 320;
controls.target.set(27, 0, 6);
controls.autoRotate = !REDUCED; controls.autoRotateSpeed = 0.35;

scene.add(new THREE.HemisphereLight('#B9C7E8', '#0D1321', 1.1));
const sun = new THREE.DirectionalLight('#FFFFFF', 1.6); sun.position.set(-40, 80, 50); scene.add(sun);
const ground = new THREE.Mesh(new THREE.PlaneGeometry(800, 800), new THREE.MeshStandardMaterial({ color: '#10141F', roughness: 1 }));
ground.rotation.x = -Math.PI / 2; ground.position.y = -0.05; scene.add(ground);
const grid = new THREE.GridHelper(800, 100, '#1B2439', '#1B2439'); grid.material.transparent = true; grid.material.opacity = 0.5; scene.add(grid);

const composer = new EffectComposer(renderer);
composer.addPass(new RenderPass(scene, camera));
composer.addPass(new UnrealBloomPass(new THREE.Vector2(innerWidth, innerHeight), 0.5, 0.5, 0.7));
composer.addPass(new OutputPass());
addEventListener('resize', () => {
  camera.aspect = innerWidth / innerHeight; camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight); composer.setSize(innerWidth, innerHeight); labelR.setSize(innerWidth, innerHeight);
});

// ---------- blocks: every mesh eases from `cur` toward `target` each frame ----------
const boxGeo = new THREE.BoxGeometry(1, 1, 1);
const blocks = new THREE.Group(); scene.add(blocks);
const byKey = new Map();
const within = (r, box) => ({ x: box.x + ((r.x + STAGE_W / 2) / STAGE_W) * box.w, z: box.z + ((r.z + STAGE_D / 2) / STAGE_D) * box.h, w: (r.w / STAGE_W) * box.w, h: (r.h / STAGE_D) * box.h });
// scan-time layout: equal cells in name order; only the height moves as bytes are counted
const gridOf = (items) => {
  const n = Math.max(1, items.length), cols = Math.ceil(Math.sqrt(n * STAGE_W / STAGE_D)), rows = Math.ceil(n / cols), w = STAGE_W / cols, h = STAGE_D / rows;
  return items.map((item, i) => ({ item, x: -STAGE_W / 2 + (i % cols) * w, z: -STAGE_D / 2 + Math.floor(i / cols) * h, w, h }));
};
const layoutOf = (items) => squarify(items, 0, 0, STAGE_W, STAGE_D).map((r) => ({ item: r.item, x: r.x - STAGE_W / 2, z: r.y - STAGE_D / 2, w: r.w, h: r.h }));
const heightFor = (size, max) => 1.5 + 26 * Math.pow(size / max, 0.4);

function place(m, s) {
  m.scale.set(Math.max(0.2, s.w - GAP), Math.max(0.01, s.y), Math.max(0.2, s.h - GAP));
  m.position.set(s.x + s.w / 2, m.scale.y / 2, s.z + s.h / 2);
}
const edgeGeo = new THREE.EdgesGeometry(boxGeo);
function setInner(m, inner) {
  for (const c of m.userData.inner ?? []) { m.remove(c); c.material.dispose(); }
  m.userData.inner = [];
  if (!inner?.length) return;
  const max = Math.max(1, ...inner.map((k) => k.size));
  for (const r of squarify(inner, 0, 0, 1, 1)) {
    const n = r.item;
    const col = FILTER_COLOR[filter] ? FILTER_COLOR[filter].clone().lerp(new THREE.Color('#FFFFFF'), 0.25) : colorMode === 'type' && n.types ? TYPE_COLOR[dominant(n.types)] : gunkSet.has(n.path) ? TIER_COLOR[gunkSet.get(n.path).tier] : heatColor(Math.min(1, ageDays(n) / 365));
    const c = new THREE.Mesh(boxGeo, new THREE.MeshStandardMaterial({ color: col, roughness: 0.5, emissive: col, emissiveIntensity: 0.35 }));
    const lift = 0.04 + 0.1 * Math.pow(n.size / max, 0.5);
    c.scale.set(r.w * 0.86, lift, r.h * 0.86);
    c.position.set(r.x + r.w / 2 - 0.5, 0.5 + lift / 2, r.y + r.h / 2 - 0.5);
    m.add(c); m.userData.inner.push(c);
  }
}
function setLabel(m, entry, big) {
  if (big && !m.userData.label) {
    const el = document.createElement('div'); el.className = 'lbl';
    const lbl = new CSS2DObject(el); lbl.position.set(0, 0.6, 0); lbl.center.set(0.5, 1);
    m.add(lbl); m.userData.label = lbl;
  } else if (!big && m.userData.label) {
    m.userData.label.element.remove(); m.remove(m.userData.label); m.userData.label = null;
  }
  if (m.userData.label) m.userData.label.element.innerHTML = `${entry.name}<small>${entry.sub}</small>`;
}
/**
 * entries: [{key, name, sub, node, rect:{x,z,w,h}, y, color, em, pulse}]
 * from: rect new blocks grow out of (default: their own footprint, from the floor)
 * to:   rect vanishing blocks collapse into (default: sink in place)
 */
function setBlocks(entries, { from, to, stagger } = {}) {
  const seen = new Set();
  for (const e of entries) {
    seen.add(e.key);
    let m = byKey.get(e.key);
    if (!m) {
      const mat = new THREE.MeshStandardMaterial({ color: e.color, roughness: 0.55, metalness: 0.15, emissive: e.color, emissiveIntensity: e.em, transparent: true });
      m = new THREE.Mesh(boxGeo, mat);
      const edge = new THREE.LineSegments(edgeGeo, new THREE.LineBasicMaterial({ color: e.color, transparent: true, opacity: 0.7 }));
      m.add(edge);
      const start = from ? within(e.rect, from) : e.rect;
      const cx = e.rect.x + e.rect.w / 2, cz = e.rect.z + e.rect.h / 2;
      m.userData = { cur: { ...start, y: 0 }, target: null, label: null, dying: false, edge, drop: null, live: !!e.fly, done: !!e.node.done,
        delay: stagger ? Math.hypot(cx, cz) / 70 * 0.9 + Math.random() * 0.08 : 0, popT: from ? 0 : -1, lastSize: 0 };
      if (e.fly) crewJob(e.key, e.rect);
      place(m, m.userData.cur);
      byKey.set(e.key, m); blocks.add(m);
    }
    const u = m.userData;
    u.lastSize = e.node.size; u.live = !!e.fly; u.done = !!e.node.done;
    u.dying = false; u.entry = e; u.node = e.node;
    u.target = { ...e.rect, y: e.y };
    u.colorTarget = e.color; u.edgeColor = e.edge; u.em = e.em; u.pulse = e.pulse;
    m.material.opacity = 1;
    setLabel(m, e, e.rect.w * e.rect.h > STAGE_W * STAGE_D * 0.012);
    setInner(m, e.rect.w * e.rect.h > STAGE_W * STAGE_D * 0.006 ? e.inner : null);
  }
  for (const [key, m] of byKey) {
    if (seen.has(key) || m.userData.dying) continue;
    m.userData.dying = true;
    const r = to ? within(m.userData.cur, to) : m.userData.cur;
    m.userData.target = { ...r, y: 0 };
    setLabel(m, m.userData.entry, false); setInner(m, null);
  }
}
function dispose(m) {
  byKey.delete(m.userData.entry.key); blocks.remove(m); m.material.dispose(); m.userData.edge.material.dispose();
  if (m.userData.label) m.userData.label.element.remove();
}
const smooth = (a, b, f) => a + (b - a) * f;
function updateBlocks(dt, now) {
  const f = REDUCED ? 1 : 1 - Math.exp(-dt * 7);
  for (const m of [...byKey.values()]) {
    const u = m.userData, c = u.cur, t = u.target;
    if (!t) continue;
    for (const k of ['x', 'z', 'w', 'h', 'y']) c[k] = smooth(c[k], t[k], f);
    place(m, c);
    if (u.drop) { // live blocks fall from the choppers and bounce into place
      const d = u.drop; d.v -= 120 * dt; d.y += d.v * dt;
      if (d.y <= 0) { d.y = 0; if (d.v < -8) { d.squash = Math.min(0.45, -d.v / 60); d.v = -d.v * 0.35; } else { d.v = 0; if (d.squash < 0.02) u.drop = null; } }
      d.squash = smooth(d.squash, 0, 1 - Math.exp(-dt * 9));
      m.position.y += d.y; m.scale.y *= 1 - d.squash; m.scale.x *= 1 + d.squash * 0.5; m.scale.z *= 1 + d.squash * 0.5;
    }
    if (u.jig > 0.01 && !REDUCED) { u.jig = smooth(u.jig, 0, 1 - Math.exp(-dt * 4)); const w = 1 + 0.16 * u.jig * Math.sin(now / 40); m.scale.y *= w; m.position.y = m.scale.y / 2 + (u.drop?.y ?? 0); }
    crewLift(m, u, dt);
    if (u.entry.jitter > 0.05 && !REDUCED) { const j = u.entry.jitter * 0.35; m.position.x += (Math.random() - 0.5) * j; m.position.z += (Math.random() - 0.5) * j; }
    if (u.dying) {
      m.material.opacity = Math.max(0, m.material.opacity - dt * 2.5); u.edge.material.opacity = m.material.opacity * 0.7;
      if (m.material.opacity <= 0 || Math.abs(c.y - t.y) < 0.05) dispose(m);
      continue;
    }
    m.material.color.lerp(u.colorTarget, f); m.material.emissive.lerp(u.colorTarget, f);
    u.edge.material.color.copy(u.edgeColor ?? m.material.color).multiplyScalar(m === hovered || m === selected || u.entry.key === rowHover ? 2.2 : u.edgeColor ? 1.9 : 1.5);
    let boost = m === selected ? 0.8 : m === hovered || u.entry.key === rowHover ? 0.45 : u.em;
    if (u.flashUntil > now) boost = Math.floor(now / 150) % 2 ? 1.4 : 0.2;
    const pulse = u.pulse === 'slow' ? 0.14 * (0.5 + 0.5 * Math.sin(now / 900 + c.x * 0.3)) : u.pulse ? 0.08 * (0.5 + 0.5 * Math.sin(now / 400 + c.x)) : 0;
    m.material.emissiveIntensity = smooth(m.material.emissiveIntensity, boost + pulse, f);
  }
}

// ---------- the search party: choppers with spotlights hover over whatever is still being counted; detail ripples up under the beam ----------
const crew = { units: [], jobs: [], group: new THREE.Group() };
scene.add(crew.group);
let _poolTex = null;
function poolTex() { // radial spotlight landing: hot centre, soft edge
  if (_poolTex) return _poolTex;
  const c = document.createElement('canvas'); c.width = c.height = 128; const g = c.getContext('2d');
  const r = g.createRadialGradient(64, 64, 4, 64, 64, 64); r.addColorStop(0, 'rgba(255,246,220,1)'); r.addColorStop(0.35, 'rgba(255,214,140,0.55)'); r.addColorStop(1, 'rgba(255,200,110,0)');
  g.fillStyle = r; g.fillRect(0, 0, 128, 128);
  _poolTex = new THREE.CanvasTexture(c); return _poolTex;
}
function makeChopper(i) {
  const g = new THREE.Group();
  const body = new THREE.Mesh(new THREE.BoxGeometry(4.2, 2.2, 2.4), new THREE.MeshStandardMaterial({ color: ['#F5C26B', '#4FD1C5', '#E8457A'][i % 3], roughness: 0.4, metalness: 0.3, emissive: ['#F5C26B', '#4FD1C5', '#E8457A'][i % 3], emissiveIntensity: 0.15 }));
  body.position.y = 0.4; g.add(body);
  const nose = new THREE.Mesh(new THREE.SphereGeometry(1.25, 16, 12), new THREE.MeshStandardMaterial({ color: '#0D1321', roughness: 0.2, metalness: 0.6 })); nose.position.set(2.1, 0.5, 0); g.add(nose);
  const tail = new THREE.Mesh(new THREE.BoxGeometry(4.5, 0.6, 0.6), body.material); tail.position.set(-3.6, 0.9, 0); g.add(tail);
  const fin = new THREE.Mesh(new THREE.BoxGeometry(0.4, 1.8, 0.4), body.material); fin.position.set(-5.6, 1.6, 0); g.add(fin);
  const rotor = new THREE.Mesh(new THREE.BoxGeometry(9, 0.12, 0.5), new THREE.MeshStandardMaterial({ color: '#E8E6DF', transparent: true, opacity: 0.55 })); rotor.position.y = 2.1; g.add(rotor);
  const rotor2 = rotor.clone(); rotor2.rotation.y = Math.PI / 2; g.add(rotor2);
  const skids = new THREE.Mesh(new THREE.BoxGeometry(4, 0.15, 3), new THREE.MeshStandardMaterial({ color: '#46557A' })); skids.position.y = -1; g.add(skids);
  // the beam: a soft wide cone, a brighter narrow core, and a pool of light on whatever it lands on
  const cone = new THREE.Mesh(new THREE.ConeGeometry(6, 26, 32, 1, true), new THREE.MeshBasicMaterial({ color: '#FFD98A', transparent: true, opacity: 0.16, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  cone.position.y = -14; g.add(cone);
  const core = new THREE.Mesh(new THREE.ConeGeometry(2.6, 26, 24, 1, true), new THREE.MeshBasicMaterial({ color: '#FFF6E0', transparent: true, opacity: 0.22, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  core.position.y = -14; g.add(core);
  const pool = new THREE.Mesh(new THREE.PlaneGeometry(16, 16), new THREE.MeshBasicMaterial({ map: poolTex(), transparent: true, opacity: 0.55, blending: THREE.AdditiveBlending, depthWrite: false }));
  pool.rotation.x = -Math.PI / 2; scene.add(pool);
  const beam = new THREE.Group(); beam.add(cone, core); g.add(beam);
  const light = new THREE.SpotLight('#FFE9B8', 34, 80, 0.4, 0.7, 1.6); light.position.set(0, -1, 0); g.add(light); g.add(light.target); light.target.position.set(0, -30, 0);
  g.scale.setScalar(1.5);
  crew.group.add(g);
  return { g, rotor, rotor2, cone, core, beam, pool, light, x: (Math.random() - 0.5) * 80, z: (Math.random() - 0.5) * 50, y: 30, vx: 0, vz: 0, phase: Math.random() * 10, job: null, hold: 0, rip: 0, seed: i };
}
function crewJob() {} // choppers are assigned from scan progress, see crewAssign
crew.busy = new Set(); // keys of top-level folders still being counted
function crewAssign(live) { crew.busy = new Set(live.filter((i) => !i.done && i.is_dir).map((i) => i.name)); }
function floorAt(x, z) { // top of whatever block is under this point
  for (const m of byKey.values()) { const c = m.userData.cur; if (x >= c.x && x <= c.x + c.w && z >= c.z && z <= c.z + c.h) return m.position.y + m.scale.y / 2; }
  return 0;
}
function updateCrew(dt, now) {
  // one chopper per folder still being counted (up to 6); extras fly off, missing ones arrive
  const want = scanning ? Math.min(6, Math.max(1, crew.busy.size)) : 0;
  while (crew.units.length < want) crew.units.push(makeChopper(crew.units.length));
  crew.group.visible = crew.units.length > 0 || !!crew.escort || home.on;
  const taken = new Set(crew.units.map((c) => c.job?.key).filter(Boolean));
  crew.units.forEach((c, i) => {
    // leave when the scan is done or there is nothing left for this unit: climb out and vanish
    if (!scanning || i >= want) { // leaving: beam off, rotor still turning, climb out
      c.y += 40 * dt; c.g.position.y = c.y; c.x += 30 * dt; c.g.position.x = c.x; c.rotor.rotation.y = now / 22; c.rotor2.rotation.y = now / 22 + Math.PI / 2;
      const f = 1 - Math.exp(-dt * 6); c.cone.material.opacity *= 1 - f; c.core.material.opacity *= 1 - f; c.light.intensity *= 1 - f; c.pool.material.opacity = Math.max(0, c.pool.material.opacity - dt * 3); if (c.y > 140) { crew.group.remove(c.g); scene.remove(c.pool); crew.units.splice(i, 1); } return; }
    // drop a finished folder; after a while, rotate to another untaken one so the party roams
    if (c.job && (!crew.busy.has(c.job.key) || !byKey.has(c.job.key))) { taken.delete(c.job.key); c.job = null; }
    if (c.job && c.hold > 7) { const other = [...crew.busy].find((k) => !taken.has(k) && byKey.has(k)); if (other) { taken.delete(c.job.key); c.job = null; } }
    if (!c.job) {
      const count = (k) => crew.units.filter((u) => u.job?.key === k).length;
      const k = [...crew.busy].filter((k) => byKey.has(k)).sort((x, y) => count(x) - count(y))[0];
      if (k) { c.job = { key: k }; c.hold = 0; c.rip = 0; taken.add(k); }
    }
    let tx, tz, ty = 30;
    if (c.job) { const r = byKey.get(c.job.key).userData.cur; const off = Math.min(r.w, r.h) * 0.22; tx = r.x + r.w / 2 + Math.cos(c.phase + now / 4000) * off; tz = r.z + r.h / 2 + Math.sin(c.phase + now / 4000) * off; ty = 20 + Math.min(10, byKey.get(c.job.key).scale.y * 0.4); }
    else { const t = now / 1000 * 0.35 + c.phase; tx = Math.sin(t * 0.9 + c.seed) * 52; tz = Math.cos(t * 0.6 + c.seed * 2) * 34; }
    const ax = (tx - c.x) * 2.2 - c.vx * 1.6, az = (tz - c.z) * 2.2 - c.vz * 1.6;
    c.vx += ax * dt; c.vz += az * dt; c.x += c.vx * dt; c.z += c.vz * dt;
    c.y = smooth(c.y, ty + Math.sin(now / 700 + c.phase) * 0.8, 1 - Math.exp(-dt * 2));
    const near = c.job && Math.hypot(tx - c.x, tz - c.z) < 6;
    if (near) c.hold += dt;
    c.g.position.set(c.x, c.y, c.z);
    const speed = Math.hypot(c.vx, c.vz);
    if (speed > 0.5) c.g.rotation.y = smooth(c.g.rotation.y, Math.atan2(-c.vz, c.vx), 1 - Math.exp(-dt * 4));
    c.g.rotation.z = smooth(c.g.rotation.z, -Math.min(0.5, speed * 0.02), 1 - Math.exp(-dt * 3));
    c.rotor.rotation.y = now / 22; c.rotor2.rotation.y = now / 22 + Math.PI / 2;
    // beam sways a little while hovering, snaps straight down while travelling
    c.beam.rotation.x = smooth(c.beam.rotation.x, near ? Math.sin(now / 900 + c.phase) * 0.12 : 0, 1 - Math.exp(-dt * 3));
    c.beam.rotation.z = smooth(c.beam.rotation.z, near ? Math.cos(now / 1100 + c.phase) * 0.12 : 0, 1 - Math.exp(-dt * 3));
    c.cone.material.opacity = near ? 0.22 : 0.12; c.core.material.opacity = near ? 0.32 : 0.18; c.light.intensity = near ? 28 : 12; // a torch, not a floodlight: the stacks keep their own colour
    c.light.target.position.set(0, -c.y, 0);
    // pool of light where the beam lands, sized by height, sitting on the block top
    const px = c.x + Math.sin(c.beam.rotation.z) * c.y * 0.6, pz = c.z - Math.sin(c.beam.rotation.x) * c.y * 0.6;
    c.pool.position.set(px, floorAt(px, pz) + 0.15, pz); const ps = 0.55 + c.y / 40; c.pool.scale.set(ps, ps, 1);
    c.pool.material.opacity = near ? 0.75 : 0.35;
  });
}
// ---- reveal ripple: under each spotlight, small cubes ripple outward across the folder being counted, like detail surfacing under the light
const RIP_MAX = 3000, RIP_CELL = 3;
const rip = new THREE.InstancedMesh(new THREE.BoxGeometry(1, 1, 1), new THREE.MeshStandardMaterial({ roughness: 0.45, metalness: 0.15, emissive: '#FFFFFF', emissiveIntensity: 0.08 }), RIP_MAX);
rip.instanceMatrix.setUsage(THREE.DynamicDrawUsage); rip.count = 0; scene.add(rip);
const _rm = new THREE.Matrix4(), _rq = new THREE.Quaternion(), _rp = new THREE.Vector3(), _rs = new THREE.Vector3(), _rc = new THREE.Color(), _white = new THREE.Color('#FFF6E0');
function updateRipples(dt, now) {
  let n = 0;
  if (scanning) for (const c of crew.units) {
    const m = c.job && byKey.get(c.job.key); if (!m || m.userData.dying) continue;
    const r = m.userData.cur, top = m.position.y + m.scale.y / 2;
    const near = c.hold > 0; if (!near) continue;
    c.rip += dt;
    const px = Math.min(r.x + r.w - 1, Math.max(r.x + 1, c.pool.position.x)), pz = Math.min(r.z + r.h - 1, Math.max(r.z + 1, c.pool.position.z));
    const R = Math.hypot(r.w, r.h) * 0.5, period = 2.4, t = (c.rip % period) / period, ring = t * R;
    const cell = Math.max(1.2, Math.min(RIP_CELL, Math.min(r.w, r.h) / 7)); // small live cells still get a fine grain
    const nx = Math.max(2, Math.round(r.w / cell)), nz = Math.max(2, Math.round(r.h / cell)), cw = r.w / nx, cd = r.h / nz;
    const col = m.material.color;
    for (let i = 0; i < nx && n < RIP_MAX; i++) for (let j = 0; j < nz && n < RIP_MAX; j++) {
      const cx = r.x + (i + 0.5) * cw, cz = r.z + (j + 0.5) * cd, d = Math.hypot(cx - px, cz - pz);
      const wave = Math.exp(-((d - ring) ** 2) / 10) * (1 - t * 0.55); // one ring travelling out from the light, fading as it spreads
      const glow = Math.max(0, 1 - d / (R * 0.45)) * 0.35 * (0.6 + 0.4 * Math.sin(now / 260 + i * 0.9 + j * 1.3)); // shimmer right under the beam
      const h = wave * 2.4 + glow;
      if (h < 0.15) continue;
      _rp.set(cx, top + h / 2, cz); _rs.set(cw * 0.66, h, cd * 0.66);
      _rm.compose(_rp, _rq, _rs); rip.setMatrixAt(n, _rm);
      _rc.copy(col).lerp(_white, Math.min(0.45, wave * 0.4 + glow * 0.3)); rip.setColorAt(n, _rc); n++;
    }
  }
  rip.count = n; rip.instanceMatrix.needsUpdate = true; if (rip.instanceColor) rip.instanceColor.needsUpdate = true;
}
// ---- construction sites: the treemap grows like a skyline, so Di's ground crew builds it
// A site rides the top of a folder that is still being counted: a climbing crane whose hook tracks
// the height, a dozer working the deck, and two figures in hard hats. When the count lands, they
// pack up and the site moves to the next tower.
const sites = { pool: [], group: new THREE.Group() };
scene.add(sites.group);
const SITE_FIT = 6.5; // a deck narrower than this has no room for a crew, so it stays bare
const siteScale = (w, h) => Math.max(0.62, Math.min(1.15, Math.min(w, h) / 14)); // the crew shrinks to fit its deck, but never past legibility
// one minifigure: stubby legs, a blocky torso, swinging arms and a big head under a hard hat
function makeFig(role, mats) {
  const use = (m) => { mats.push(m); return m; };
  const paint = (c, rough = 0.55) => use(new THREE.MeshStandardMaterial({ color: c, roughness: rough, transparent: true }));
  const boss = role === 'boss';
  const skin = paint('#F5C26B', 0.5), navy = paint('#46557A', 0.7);
  const vest = paint(boss ? '#4FD1C5' : '#FF7A3D', 0.5);
  const g = new THREE.Group();
  const hips = new THREE.Group(); hips.position.y = 0.78; g.add(hips);
  const legs = [];
  for (const sx of [-1, 1]) {
    const l = new THREE.Group(); l.position.set(sx * 0.17, 0, 0);
    const m = new THREE.Mesh(new THREE.BoxGeometry(0.3, 0.78, 0.34), navy); m.position.y = -0.39;
    l.add(m); hips.add(l); legs.push(l);
  }
  const torso = new THREE.Mesh(new THREE.BoxGeometry(0.74, 0.86, 0.44), vest); torso.position.y = 1.21; g.add(torso);
  const arms = [];
  for (const sx of [-1, 1]) {
    const a = new THREE.Group(); a.position.set(sx * 0.47, 1.56, 0);
    const m = new THREE.Mesh(new THREE.BoxGeometry(0.21, 0.62, 0.26), vest); m.position.y = -0.31;
    const hand = new THREE.Mesh(new THREE.CylinderGeometry(0.11, 0.11, 0.16, 8), skin); hand.position.y = -0.66;
    a.add(m, hand); g.add(a); arms.push(a);
  }
  const head = new THREE.Mesh(new THREE.CylinderGeometry(0.31, 0.31, 0.56, 12), skin); head.position.y = 1.92; g.add(head);
  const hat = new THREE.Mesh(new THREE.SphereGeometry(0.35, 12, 8, 0, Math.PI * 2, 0, Math.PI / 2), paint(boss ? '#E8E6DF' : '#FFD98A', 0.4)); hat.position.y = 2.18; g.add(hat);
  const brim = new THREE.Mesh(new THREE.CylinderGeometry(0.44, 0.44, 0.07, 14), hat.material); brim.position.y = 2.18; g.add(brim);
  // the one running the job carries a clipboard; one of the others hauls a beam about
  let prop = null;
  if (boss) { prop = new THREE.Mesh(new THREE.BoxGeometry(0.42, 0.5, 0.06), paint('#E8E6DF', 0.8)); prop.position.set(0.4, 1.15, 0.34); prop.rotation.x = -0.5; g.add(prop); }
  if (role === 'carry') { prop = new THREE.Mesh(new THREE.BoxGeometry(0.22, 0.22, 3.4), paint('#F5C26B', 0.6)); prop.position.set(0, 0.95, 0.55); g.add(prop); }
  g.scale.setScalar(1.35);
  return { g, legs, arms, role, prop, phase: Math.random() * 6.3, speed: 0.55 + Math.random() * 0.35 };
}
function makeSite() {
  const g = new THREE.Group();
  const mats = [];
  const use = (m) => { mats.push(m); return m; };
  const gold = () => use(new THREE.MeshStandardMaterial({ color: '#F5C26B', roughness: 0.5, metalness: 0.3, transparent: true }));
  const dark = () => use(new THREE.MeshStandardMaterial({ color: '#46557A', roughness: 0.75, transparent: true }));

  // A tower crane reads by its silhouette: a latticed mast, an A-frame apex with tie bars out to
  // both arms, and a counterweight at the back. Solid bars alone just look like an L.
  const crane = new THREE.Group();
  const H = 12.4, POST = 0.3; // mast height, and how far the corner posts sit from its centre
  const steel = gold();
  const foot = new THREE.Mesh(new THREE.BoxGeometry(1.5, 0.3, 1.5), dark()); foot.position.y = 0.15; crane.add(foot);
  for (const [px, pz] of [[-1, -1], [1, -1], [-1, 1], [1, 1]]) {
    const post = new THREE.Mesh(new THREE.BoxGeometry(0.1, H, 0.1), steel);
    post.position.set(px * POST, H / 2, pz * POST); crane.add(post);
  }
  for (let y = 1.4; y < H; y += 1.9) { // the ties that make it read as lattice rather than a pole
    for (const [w, d, ox, oz] of [[POST * 2, 0.07, 0, -POST], [POST * 2, 0.07, 0, POST], [0.07, POST * 2, -POST, 0], [0.07, POST * 2, POST, 0]]) {
      const tie = new THREE.Mesh(new THREE.BoxGeometry(w, 0.07, d), steel); tie.position.set(ox, y, oz); crane.add(tie);
    }
  }
  const slew = new THREE.Mesh(new THREE.BoxGeometry(1.1, 0.5, 1.1), dark()); slew.position.y = H + 0.25; crane.add(slew);
  const opcab = new THREE.Mesh(new THREE.BoxGeometry(0.8, 0.8, 0.9), dark()); opcab.position.set(0.9, H + 0.7, 0); crane.add(opcab);
  // the A-frame above the slew, and the tie bars from its top out to each arm
  const apexY = H + 3.2;
  for (const sx of [-1, 1]) {
    const leg = new THREE.Mesh(new THREE.BoxGeometry(0.12, 3.3, 0.12), steel);
    leg.position.set(sx * 0.55, H + 1.9, 0); leg.rotation.z = sx * 0.33; crane.add(leg);
  }
  const jib = new THREE.Group(); jib.position.y = H + 0.6; crane.add(jib);
  const chord = new THREE.Mesh(new THREE.BoxGeometry(11.5, 0.16, 0.16), steel); chord.position.x = 5.2; jib.add(chord);
  const lower = new THREE.Mesh(new THREE.BoxGeometry(11.5, 0.13, 0.13), steel); lower.position.set(5.2, -0.55, 0); jib.add(lower);
  for (let x = 0.8; x < 11; x += 1.6) { const st = new THREE.Mesh(new THREE.BoxGeometry(0.09, 0.6, 0.09), steel); st.position.set(x, -0.28, 0); jib.add(st); }
  const back = new THREE.Mesh(new THREE.BoxGeometry(3.4, 0.16, 0.5), steel); back.position.x = -1.7; jib.add(back);
  const cw = new THREE.Mesh(new THREE.BoxGeometry(1.3, 1.1, 1.2), dark()); cw.position.set(-3, -0.3, 0); jib.add(cw);
  for (const [x, len, tilt] of [[3.4, 7.4, 0.42], [-1.9, 4.0, -0.75]]) { // apex ties, fore and aft
    const bar = new THREE.Mesh(new THREE.BoxGeometry(len, 0.09, 0.09), steel);
    bar.position.set(x, (apexY - (H + 0.6)) / 2, 0); bar.rotation.z = tilt; jib.add(bar);
  }
  const cable = new THREE.Mesh(new THREE.BoxGeometry(0.07, 6, 0.07), dark()); cable.position.set(8, -3, 0); jib.add(cable);
  const trolley = new THREE.Mesh(new THREE.BoxGeometry(0.6, 0.3, 0.5), dark()); trolley.position.set(8, -0.3, 0); jib.add(trolley);
  const hook = new THREE.Mesh(new THREE.BoxGeometry(0.75, 0.5, 0.75), steel); hook.position.set(8, -6, 0); jib.add(hook);
  jib.add(cable, trolley, hook);
  g.add(crane);

  const doz = new THREE.Group(); doz.scale.setScalar(1.5);
  const body = new THREE.Mesh(new THREE.BoxGeometry(2.4, 1, 1.5), gold()); body.position.y = 0.75;
  const blade = new THREE.Mesh(new THREE.BoxGeometry(0.3, 1.2, 2.1), dark()); blade.position.set(1.5, 0.65, 0);
  const cab = new THREE.Mesh(new THREE.BoxGeometry(1, 0.85, 1.1), dark()); cab.position.set(-0.5, 1.6, 0);
  doz.add(body, blade, cab); g.add(doz);

  // the site itself: cones, a stack of materials, and a ladder against the mast
  const props = new THREE.Group(); g.add(props);
  for (const [cx, cz] of [[-1.6, 2.4], [1.9, 2.9], [3.1, -1.4]]) {
    const cone = new THREE.Mesh(new THREE.ConeGeometry(0.26, 0.8, 10), use(new THREE.MeshStandardMaterial({ color: '#FF7A3D', roughness: 0.6, transparent: true })));
    cone.position.set(cx, 0.4, cz); props.add(cone);
  }
  for (let i = 0; i < 3; i++) {
    const crate = new THREE.Mesh(new THREE.BoxGeometry(1.1, 0.42, 0.9), gold());
    crate.position.set(-3.1 + (i % 2) * 0.2, 0.21 + i * 0.42, 2.0); crate.rotation.y = i * 0.2; props.add(crate);
  }
  const ladder = new THREE.Group(); ladder.position.set(-1.1, 0, -0.6); ladder.rotation.z = 0.22;
  for (const sx of [-0.24, 0.24]) { const rail = new THREE.Mesh(new THREE.BoxGeometry(0.08, 3.4, 0.08), gold()); rail.position.set(sx, 1.7, 0); ladder.add(rail); }
  for (let i = 0; i < 5; i++) { const rung = new THREE.Mesh(new THREE.BoxGeometry(0.5, 0.07, 0.07), gold()); rung.position.y = 0.5 + i * 0.65; ladder.add(rung); }
  props.add(ladder);

  const figs = [makeFig('boss', mats), makeFig('walk', mats), makeFig('walk', mats), makeFig('carry', mats)];
  for (const f of figs) g.add(f.g);
  sites.group.add(g);
  return { g, crane, jib, cable, hook, doz, figs, mats, key: null, out: 0, phase: Math.random() * 6.3 };
}
function updateSites(dt, now) {
  sites.group.visible = scanning || sites.pool.some((s) => s.key || s.out > 0);
  if (!sites.group.visible) return;
  // the biggest decks still being counted, so the crew is always somewhere worth watching
  const open = scanning ? [...crew.busy].filter((k) => { const m = byKey.get(k); if (!m) return false; const r = m.userData.cur; return Math.min(r.w, r.h) >= SITE_FIT; })
    .sort((a, b) => byKey.get(b).scale.y - byKey.get(a).scale.y).slice(0, 6) : [];
  const taken = new Set(sites.pool.map((s) => s.key).filter(Boolean));
  for (const k of open) {
    if (taken.has(k)) continue;
    let free = sites.pool.find((s) => !s.key && s.out <= 0);
    if (!free && sites.pool.length < 6) { free = makeSite(); sites.pool.push(free); }
    if (!free) break;
    free.key = k; free.out = 0; taken.add(k);
  }
  for (const s of sites.pool) {
    const m = s.key && byKey.get(s.key);
    if (s.key && (!m || !open.includes(s.key))) { s.key = null; s.out = 0.0001; } // count landed: pack up
    if (!s.key && s.out > 0) { s.out = Math.min(1, s.out + dt / 0.9); if (s.out >= 1) { s.g.visible = false; s.out = 0; } }
    if (!s.key && !s.out) { s.g.visible = false; continue; }
    const live = m ? m : null;
    if (!live && !s.out) continue;
    s.g.visible = true;
    if (live) {
      const r = live.userData.cur;
      s.g.position.set(r.x + r.w / 2, live.position.y + live.scale.y / 2, r.z + r.h / 2);
      s.rw = Math.min(r.w, 34); s.rh = Math.min(r.h, 34);
    }
    const fade = s.out ? 1 - s.out : 1;
    for (const mat of s.mats) mat.opacity = fade;
    s.g.position.y -= s.out * 6 * dt; // the site sinks away as it clears
    // the whole crew scales to the deck, so offsets below are in the site's own units
    const k = siteScale(s.rw ?? 12, s.rh ?? 12);
    s.g.scale.setScalar(k);
    const rw = (s.rw ?? 12) / k, rh = (s.rh ?? 12) / k;
    // the crane stands at a corner of the deck and slews slowly, hook riding the tower up
    s.crane.position.set(-rw * 0.3, 0, -rh * 0.3);
    if (!REDUCED) s.crane.rotation.y = Math.sin(now / 5200 + s.phase) * 0.9;
    const drop = REDUCED ? 5.5 : 5.5 + Math.sin(now / 1800 + s.phase) * 2.6; // how far below the jib the hook hangs
    s.hook.position.y = -drop;
    s.cable.position.y = -drop / 2; s.cable.scale.y = Math.max(0.05, drop / 6);
    // the dozer works a slow loop of the deck, nose pointing the way it is going
    const a = REDUCED ? 0 : now / 2600 + s.phase;
    const dx = Math.cos(a) * rw * 0.26, dz = Math.sin(a) * rh * 0.26;
    s.doz.position.set(dx, 0, dz);
    s.doz.rotation.y = -a + Math.PI / 2;
    s.figs.forEach((f, i) => {
      if (f.role === 'boss') { // stands back and watches, turning to take the site in
        const t = REDUCED ? 0 : now / 3400 + f.phase;
        f.g.position.set(-rw * 0.34, 0, rh * 0.3);
        f.g.rotation.y = Math.sin(t) * 1.3 - 0.6;
        for (const l of f.legs) l.rotation.x = 0;
        f.arms[0].rotation.x = -1.1; f.arms[1].rotation.x = -0.9 + (REDUCED ? 0 : Math.sin(now / 900 + f.phase) * 0.18);
        return;
      }
      // the rest walk laps of the deck, legs and arms swinging opposite each other
      const a = REDUCED ? f.phase : now / 1000 * f.speed * 0.6 + f.phase;
      const ax = rw * (f.role === 'carry' ? 0.2 : 0.3), az = rh * (f.role === 'carry' ? 0.2 : 0.3) * (i % 2 ? -1 : 1);
      f.g.position.set(Math.cos(a) * ax, REDUCED ? 0 : Math.abs(Math.sin(a * 9)) * 0.07, Math.sin(a) * az);
      f.g.rotation.y = -Math.atan2(Math.cos(a) * az, -Math.sin(a) * ax) + Math.PI / 2;
      const step = REDUCED ? 0 : Math.sin(a * 9) * 0.72;
      f.legs[0].rotation.x = step; f.legs[1].rotation.x = -step;
      if (f.role === 'carry') { f.arms[0].rotation.x = -1.35; f.arms[1].rotation.x = -1.35; }
      else { f.arms[0].rotation.x = -step * 0.55; f.arms[1].rotation.x = step * 0.55; }
    });
  }
}

// ---- escort: one chopper that flies in to shine on whatever you pick, and stays until you let go
crew.escort = null;
function escortTo(key) {
  if (!crew.escort) { const c = makeChopper(1); c.x = 90; c.z = -80; c.y = 60; crew.escort = c; }
  if (REDUCED) { const m = byKey.get(key); if (m) { const r = m.userData.cur; crew.escort.x = r.x + r.w / 2; crew.escort.z = r.z + r.h / 2; crew.escort.y = m.scale.y + 16; } } // no fly-in, just appear
  crew.escort.key = key; crew.escort.leaving = false; crew.group.visible = true;
}
function escortRelease() { if (crew.escort) crew.escort.leaving = true; }
function updateEscort(dt, now) {
  const c = crew.escort; if (!c) return;
  const m = c.key && byKey.get(c.key);
  if (!m || m.userData.dying || mode !== 'disk') c.leaving = true;
  let tx, tz, ty;
  if (c.leaving) { tx = c.x + 40; tz = c.z - 30; ty = 120; }
  else { const r = m.userData.cur; tx = r.x + r.w / 2 + Math.cos(now / 3000) * Math.min(r.w, 8) * 0.3; tz = r.z + r.h / 2 + Math.sin(now / 3000) * Math.min(r.h, 8) * 0.3; ty = m.scale.y + 16; }
  if (REDUCED) { c.x = tx; c.z = tz; c.y = ty; } else {
    const ax = (tx - c.x) * 2.4 - c.vx * 1.7, az = (tz - c.z) * 2.4 - c.vz * 1.7;
    c.vx += ax * dt; c.vz += az * dt; c.x += c.vx * dt; c.z += c.vz * dt;
    c.y = smooth(c.y, ty + Math.sin(now / 700) * 0.6, 1 - Math.exp(-dt * 2.2));
  }
  const near = !c.leaving && Math.hypot(tx - c.x, tz - c.z) < 5;
  c.g.position.set(c.x, c.y, c.z);
  const speed = Math.hypot(c.vx, c.vz);
  if (speed > 0.5) c.g.rotation.y = smooth(c.g.rotation.y, Math.atan2(-c.vz, c.vx), 1 - Math.exp(-dt * 4));
  c.g.rotation.z = smooth(c.g.rotation.z, -Math.min(0.5, speed * 0.02), 1 - Math.exp(-dt * 3));
  c.rotor.rotation.y = now / 22; c.rotor2.rotation.y = now / 22 + Math.PI / 2;
  c.beam.rotation.x = smooth(c.beam.rotation.x, near ? Math.sin(now / 900) * 0.08 : 0, 1 - Math.exp(-dt * 3));
  c.cone.material.opacity = near ? 0.4 : 0.15; c.core.material.opacity = near ? 0.6 : 0.2; c.light.intensity = near ? 130 : 30;
  c.light.target.position.set(0, -c.y, 0);
  c.pool.position.set(c.x, floorAt(c.x, c.z) + 0.15, c.z); const ps = 0.5 + c.y / 45; c.pool.scale.set(ps, ps, 1);
  c.pool.material.opacity = smooth(c.pool.material.opacity, near ? 0.8 : 0, REDUCED ? 1 : 1 - Math.exp(-dt * 4));
  if (c.leaving && c.y > 110) { crew.group.remove(c.g); scene.remove(c.pool); crew.escort = null; }
}

// ---------- Ru in the world: the sage from the landing page, lifted into the scene ----------
// Same character people already meet on the way in: a seated figure with a spark over its head and
// two crossed orbits circling it. Di darts and Me cruises, so Ru barely moves; the contrast in
// motion is the characterisation.
const guru = { g: null, x: 40, z: 40, y: 40, vx: 0, vz: 0, key: null, want: false, glow: 0, busy: false, rings: [] };
const RU_TILT = 32 * Math.PI / 180; // the ±32° of the two orbit paths on the landing button
function makeSage() {
  const g = new THREE.Group();
  const skin = (o) => new THREE.MeshStandardMaterial({ color: '#C9B3FF', roughness: 0.4, metalness: 0.2, transparent: true, opacity: o, flatShading: true });
  const robe = new THREE.Mesh(new THREE.ConeGeometry(2.5, 4.4, 6), skin(0.42)); robe.position.y = 0.4; g.add(robe);
  const hem = new THREE.Mesh(new THREE.TorusGeometry(2.4, 0.16, 6, 24), skin(0.7)); hem.rotation.x = Math.PI / 2; hem.position.y = -1.7; g.add(hem);
  const head = new THREE.Mesh(new THREE.SphereGeometry(1.05, 16, 12), skin(0.75)); head.position.y = 3.4; g.add(head);
  const arms = new THREE.Mesh(new THREE.TorusGeometry(1.75, 0.14, 6, 20, Math.PI), skin(0.7)); // the sweep of the sleeves
  arms.rotation.set(Math.PI / 2, 0, Math.PI); arms.position.y = 0.5; g.add(arms);
  const spark = new THREE.Mesh(new THREE.OctahedronGeometry(0.62, 0), new THREE.MeshBasicMaterial({ color: '#F1E9FF' })); spark.position.y = 5.4; g.add(spark);
  const core = new THREE.Mesh(new THREE.SphereGeometry(0.9, 12, 10), new THREE.MeshBasicMaterial({ color: '#F1E9FF', transparent: true, opacity: 0.9 })); core.position.y = 0.6; g.add(core);
  const light = new THREE.PointLight('#C9B3FF', 40, 90, 2); light.position.y = 1; g.add(light);
  // two crossed orbits with a bead on each, the periods taken from the landing animation
  const rings = [];
  for (const [tilt, period, phase] of [[-RU_TILT, 7, 0], [RU_TILT, 9.5, 4]]) {
    const rg = new THREE.Group(); rg.rotation.set(Math.PI / 2 - 0.42, 0, tilt);
    const path = new THREE.Mesh(new THREE.TorusGeometry(5.4, 0.05, 6, 96), new THREE.MeshBasicMaterial({ color: '#C9B3FF', transparent: true, opacity: 0.3 }));
    const bead = new THREE.Mesh(new THREE.SphereGeometry(0.32, 10, 8), new THREE.MeshBasicMaterial({ color: '#C9B3FF' }));
    rg.add(path, bead); g.add(rg);
    rings.push({ bead, period, phase });
  }
  const halo = new THREE.Mesh(new THREE.PlaneGeometry(26, 26), new THREE.MeshBasicMaterial({ map: poolTex(), color: '#C9B3FF', transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false }));
  halo.rotation.x = -Math.PI / 2; scene.add(halo);
  scene.add(g);
  Object.assign(guru, { g, robe, head, spark, core, light, halo, rings });
}
/// Send Ru to hover over one item on the map. Null lets it drift back out.
function ruLookAt(key) { guru.key = key ?? null; if (key) ruShow(true); }
function ruShow(on) { guru.want = on; if (on && !guru.g) makeSage(); }
function updateRuBody(dt, now) {
  if (!guru.g) return;
  const onMap = mode === 'disk';
  const m = guru.key && onMap ? byKey.get(guru.key) : null;
  const showing = guru.want && (onMap || !$('#landing').hidden);
  let tx, tz, ty;
  if (m && !m.userData.dying) {
    const r = m.userData.cur;
    tx = r.x + r.w / 2; tz = r.z + r.h / 2; ty = m.scale.y + 24 + Math.sin(now / 1600) * 1.2; // it sits above what it is reading
  } else if (showing) {
    const t = now / 1000 * 0.11; // a wide, unhurried loop
    tx = Math.cos(t) * 58 - 10; tz = Math.sin(t * 0.8) * 40 + 10; ty = 40 + Math.sin(t * 1.7) * 2.5;
  } else { tx = guru.x; tz = guru.z; ty = 150; }
  if (REDUCED) { guru.x = tx; guru.z = tz; guru.y = ty; } else {
    const ax = (tx - guru.x) * 0.9 - guru.vx * 1.9, az = (tz - guru.z) * 0.9 - guru.vz * 1.9; // gentler than the choppers on purpose
    guru.vx += ax * dt; guru.vz += az * dt; guru.x += guru.vx * dt; guru.z += guru.vz * dt;
    guru.y = smooth(guru.y, ty, 1 - Math.exp(-dt * 1.4));
  }
  guru.g.position.set(guru.x, guru.y, guru.z);
  guru.g.rotation.y += dt * (REDUCED ? 0 : 0.16); // it turns slowly to face the room
  // the light breathes, and quickens while Ru is working
  const beat = guru.busy ? 0.55 + 0.45 * Math.sin(now / 260) : 0.6 + 0.25 * Math.sin(now / 1500);
  guru.glow = smooth(guru.glow, beat, REDUCED ? 1 : 1 - Math.exp(-dt * 6));
  guru.core.scale.setScalar(0.8 + guru.glow * 0.4);
  guru.light.intensity = 25 + guru.glow * (guru.busy ? 130 : 55);
  guru.robe.material.opacity = 0.34 + guru.glow * 0.14;
  guru.spark.rotation.y += dt * 1.6; guru.spark.rotation.x += dt * 1.1;
  guru.spark.scale.setScalar(0.8 + guru.glow * 0.5);
  for (const r of guru.rings) {
    const a = REDUCED ? 0 : ((now / 1000 + r.phase) / r.period) * Math.PI * 2;
    r.bead.position.set(Math.cos(a) * 5.4, Math.sin(a) * 5.4, 0);
    r.bead.visible = !REDUCED;
  }
  const near = m && Math.hypot(tx - guru.x, tz - guru.z) < 6;
  guru.halo.position.set(guru.x, floorAt(guru.x, guru.z) + 0.16, guru.z);
  const hs = 0.45 + guru.y / 70; guru.halo.scale.set(hs, hs, 1);
  guru.halo.material.opacity = smooth(guru.halo.material.opacity, near ? 0.5 : showing ? 0.18 : 0, REDUCED ? 1 : 1 - Math.exp(-dt * 3));
  if (!showing && guru.y > 140) { scene.remove(guru.g, guru.halo); guru.g = null; }
}

// ---- verdict marks: what Ru cleared pulses, what it wants kept holds a steadier ring
const marks = [];
function ruMark(key, kind) {
  const m = byKey.get(key); if (!m) return;
  const r = m.userData.cur;
  const el = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), new THREE.MeshBasicMaterial({ map: poolTex(), color: kind === 'keep' ? '#4FD1C5' : '#C9B3FF', transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false }));
  el.rotation.x = -Math.PI / 2;
  el.position.set(r.x + r.w / 2, m.scale.y + 0.4, r.z + r.h / 2);
  el.scale.set(Math.max(r.w, 6), Math.max(r.h, 6), 1);
  scene.add(el);
  marks.push({ el, t: 0, life: kind === 'keep' ? 2.6 : 1.5, kind });
}
function updateMarks(dt) {
  for (let i = marks.length - 1; i >= 0; i--) {
    const k = marks[i]; k.t += dt;
    const u = k.t / k.life;
    if (u >= 1) { scene.remove(k.el); k.el.material.dispose(); marks.splice(i, 1); continue; }
    k.el.material.opacity = k.kind === 'keep' ? 0.5 * (1 - u) : 0.7 * Math.sin(u * Math.PI) * (0.6 + 0.4 * Math.sin(u * 18));
  }
}

// ---- the handoff: hand a block to Ru and a mote carries it over
const parcels = [];
function ruParcel(key) {
  const m = byKey.get(key); if (!m || !guru.g || REDUCED) return;
  const r = m.userData.cur;
  const el = new THREE.Mesh(new THREE.IcosahedronGeometry(1.1, 0), new THREE.MeshBasicMaterial({ color: '#C9B3FF', transparent: true, opacity: 0.9 }));
  el.position.set(r.x + r.w / 2, m.scale.y + 3, r.z + r.h / 2);
  scene.add(el);
  parcels.push({ el, from: el.position.clone(), t: 0 });
}
function updateParcels(dt) {
  for (let i = parcels.length - 1; i >= 0; i--) {
    const p = parcels[i]; p.t += dt / 0.85;
    if (p.t >= 1 || !guru.g) { scene.remove(p.el); p.el.material.dispose(); parcels.splice(i, 1); guru.glow = 1.4; continue; }
    const u = p.t * p.t * (3 - 2 * p.t); // ease so it sets off slowly and arrives quickly
    p.el.position.lerpVectors(p.from, guru.g.position, u);
    p.el.position.y += Math.sin(u * Math.PI) * 10; // a lob rather than a straight line
    p.el.rotation.x += dt * 4; p.el.rotation.y += dt * 3;
    p.el.material.opacity = 0.9 * (1 - u * 0.4);
  }
}

// blocks under a hovering chopper glow and lift a little, like they are being inspected
function crewLift(m, u, dt) {
  let k = 0;
  if (scanning) for (const c of crew.units) { if (!c.job) continue; const d = Math.hypot(u.cur.x + u.cur.w / 2 - c.x, u.cur.z + u.cur.h / 2 - c.z); k = Math.max(k, Math.exp(-(d * d) / 120)); }
  u.kick = smooth(u.kick ?? 0, k, 1 - Math.exp(-dt * 8));
  if (u.kick < 0.002) return;
  m.position.y += u.kick * 1.2;
}

// ---------- sparks: a burst when you explode into a folder ----------
const SPARK_N = 90;
const sparkGeo = new THREE.BufferGeometry(); sparkGeo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(SPARK_N * 3), 3));
const sparks = new THREE.Points(sparkGeo, new THREE.PointsMaterial({ color: '#9FE8DF', size: 1.1, transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false }));
scene.add(sparks);
const sparkV = new Float32Array(SPARK_N * 3); let sparkLife = 0;
function burst(rect) {
  if (REDUCED) return;
  const p = sparkGeo.attributes.position, cx = rect.x + rect.w / 2, cz = rect.z + rect.h / 2;
  for (let i = 0; i < SPARK_N; i++) {
    p.setXYZ(i, cx + (Math.random() - 0.5) * rect.w * 0.6, 2 + Math.random() * 3, cz + (Math.random() - 0.5) * rect.h * 0.6);
    const a = Math.random() * Math.PI * 2, s = 14 + Math.random() * 26;
    sparkV[i * 3] = Math.cos(a) * s; sparkV[i * 3 + 1] = 18 + Math.random() * 30; sparkV[i * 3 + 2] = Math.sin(a) * s;
  }
  p.needsUpdate = true; sparkLife = 1; sparks.material.opacity = 1;
}
function updateSparks(dt) {
  if (sparkLife <= 0) return;
  sparkLife -= dt * 1.1; const p = sparkGeo.attributes.position;
  for (let i = 0; i < SPARK_N; i++) {
    sparkV[i * 3 + 1] -= 70 * dt;
    p.setXYZ(i, p.getX(i) + sparkV[i * 3] * dt, Math.max(0.2, p.getY(i) + sparkV[i * 3 + 1] * dt), p.getZ(i) + sparkV[i * 3 + 2] * dt);
  }
  p.needsUpdate = true; sparks.material.opacity = Math.max(0, sparkLife);
}

// ---------- dust: slow drifting motes for depth ----------
const DUST_N = 320;
const dustGeo = new THREE.BufferGeometry(); const dustPos = new Float32Array(DUST_N * 3);
for (let i = 0; i < DUST_N; i++) { dustPos[i * 3] = (Math.random() - 0.5) * 240; dustPos[i * 3 + 1] = 2 + Math.random() * 48; dustPos[i * 3 + 2] = (Math.random() - 0.5) * 180; }
dustGeo.setAttribute('position', new THREE.BufferAttribute(dustPos, 3));
const dust = new THREE.Points(dustGeo, new THREE.PointsMaterial({ color: '#8FC7FF', size: 0.55, transparent: true, opacity: 0.32, blending: THREE.AdditiveBlending, depthWrite: false, sizeAttenuation: true }));
scene.add(dust);
function updateDust(dt, now) {
  if (REDUCED) return;
  const p = dustGeo.attributes.position;
  for (let i = 0; i < DUST_N; i++) {
    let y = p.getY(i) + dt * (0.6 + (i % 5) * 0.25); if (y > 50) y = 2;
    p.setXYZ(i, p.getX(i) + Math.sin(now / 3000 + i) * dt * 0.8, y, p.getZ(i) + Math.cos(now / 3700 + i * 0.7) * dt * 0.6);
  }
  p.needsUpdate = true;
}

// ---------- home flyers: Di's choppers and Me's rocket cruise the idle field while you decide ----------
const home = { chops: [], rocketOn: false, on: false };
function updateHomeFlyers(dt, now) {
  const want = !$('#landing').hidden && !$('#landing').classList.contains('away');
  if (want && !home.on) {
    home.on = true;
    while (home.chops.length < 2) { const c = makeChopper(home.chops.length); c.x = -60 + home.chops.length * 120; c.z = 40; c.y = 26; c.phase = home.chops.length * 3; home.chops.push(c); }
    if (!ship.g) makeShip();
    ship.g.visible = true; ship.beam.visible = false; home.rocketOn = true; ship.x = 0; ship.z = -80; ship.y = 45;
    crew.group.visible = true;
  }
  if (!home.on) return;
  home.chops.forEach((c, i) => {
    let tx, tz, ty;
    if (want) { const t = now / 1000 * 0.3 + c.phase; tx = Math.sin(t * 0.8 + i * 2) * 62 + 20; tz = Math.cos(t * 0.55 + i) * 40 + 8; ty = 24 + Math.sin(t * 1.3) * 3; }
    else { tx = c.x + 60; tz = c.z - 60; ty = 140; }
    if (REDUCED) { c.x = tx; c.z = tz; c.y = ty; } else {
      const ax = (tx - c.x) * 1.6 - c.vx * 1.5, az = (tz - c.z) * 1.6 - c.vz * 1.5;
      c.vx += ax * dt; c.vz += az * dt; c.x += c.vx * dt; c.z += c.vz * dt; c.y = smooth(c.y, ty, 1 - Math.exp(-dt * 2));
    }
    c.g.position.set(c.x, c.y, c.z);
    const speed = Math.hypot(c.vx, c.vz);
    if (speed > 0.5) c.g.rotation.y = smooth(c.g.rotation.y, Math.atan2(-c.vz, c.vx), 1 - Math.exp(-dt * 4));
    c.g.rotation.z = smooth(c.g.rotation.z, -Math.min(0.5, speed * 0.02), 1 - Math.exp(-dt * 3));
    c.rotor.rotation.y = now / 22; c.rotor2.rotation.y = now / 22 + Math.PI / 2;
    c.beam.rotation.x = Math.sin(now / 1300 + c.phase) * 0.25; c.beam.rotation.z = Math.cos(now / 1700 + c.phase) * 0.25; // sweeping the ground
    c.cone.material.opacity = 0.14; c.core.material.opacity = 0.2; c.light.intensity = 40; c.light.target.position.set(0, -c.y, 0);
    const px = c.x + Math.sin(c.beam.rotation.z) * c.y * 0.6, pz = c.z - Math.sin(c.beam.rotation.x) * c.y * 0.6;
    c.pool.position.set(px, 0.15, pz); const ps = 0.5 + c.y / 45; c.pool.scale.set(ps, ps, 1); c.pool.material.opacity = want ? 0.35 : 0;
  });
  if (home.rocketOn) {
    let tx, tz, ty;
    if (want) { const t = now / 1000 * 0.25; tx = Math.cos(t) * 75 + 20; tz = Math.sin(t * 1.4) * 45; ty = 42 + Math.sin(t * 2.2) * 6; }
    else { tx = ship.x; tz = ship.z; ty = 170; }
    if (REDUCED) { ship.x = tx; ship.z = tz; ship.y = ty; } else {
      const ax = (tx - ship.x) * 2 - ship.vx * 1.4, az = (tz - ship.z) * 2 - ship.vz * 1.4;
      ship.vx += ax * dt; ship.vz += az * dt; ship.x += ship.vx * dt; ship.z += ship.vz * dt; ship.y = smooth(ship.y, ty, 1 - Math.exp(-dt * 2));
    }
    ship.g.position.set(ship.x, ship.y, ship.z);
    const speed = Math.hypot(ship.vx, ship.vz);
    ship.g.rotation.z = smooth(ship.g.rotation.z, -Math.max(-0.5, Math.min(0.5, ship.vx * 0.025)), 1 - Math.exp(-dt * 3));
    ship.g.rotation.x = smooth(ship.g.rotation.x, Math.max(-0.5, Math.min(0.5, ship.vz * 0.025)), 1 - Math.exp(-dt * 3));
    ship.g.rotation.y += dt * 0.2;
    const thrust = Math.min(1, speed / 20);
    for (const f of ship.flames) { const flick = 0.75 + 0.25 * Math.sin(now / 37 + f.seed) * Math.sin(now / 53); const len = (0.6 + 1.4 * thrust) * flick; f.flame.scale.set(0.8 + 0.4 * thrust, len, 0.8 + 0.4 * thrust); f.core.scale.set(1, len, 1); }
    ship.glow.intensity = 20 + 50 * thrust; ship.light.intensity = 0; ship.pool.material.opacity = 0;
  }
  if (!want) {
    home.chops = home.chops.filter((c) => { if (c.y > 130) { crew.group.remove(c.g); scene.remove(c.pool); return false; } return true; });
    if (home.rocketOn && ship.y > 160) { home.rocketOn = false; ship.g.visible = false; ship.on = false; }
    if (!home.chops.length && !home.rocketOn) home.on = false;
  }
}

// ---------- earth: Di works over a city at dusk, Me flies in space ----------
const sky = new THREE.Mesh(new THREE.SphereGeometry(1100, 32, 16), new THREE.ShaderMaterial({
  side: THREE.BackSide, depthWrite: false, fog: false,
  uniforms: { top: { value: new THREE.Color('#0A0F22') }, mid: { value: new THREE.Color('#2A2246') }, horizon: { value: new THREE.Color('#5E3A38') } },
  vertexShader: 'varying vec3 vP; void main(){ vP = position; gl_Position = projectionMatrix * modelViewMatrix * vec4(position,1.0); }',
  fragmentShader: 'uniform vec3 top; uniform vec3 mid; uniform vec3 horizon; varying vec3 vP; void main(){ float h = clamp(vP.y / 1100.0, -0.05, 1.0); vec3 c = h < 0.12 ? mix(horizon, mid, smoothstep(-0.05, 0.12, h)) : mix(mid, top, smoothstep(0.12, 0.7, h)); gl_FragColor = vec4(c, 1.0); }',
}));
sky.position.y = -40; scene.add(sky);
// distant skyline: a ring of towers with lit windows
function windowTex() {
  const c = document.createElement('canvas'); c.width = 64; c.height = 256; const g = c.getContext('2d');
  g.fillStyle = '#000'; g.fillRect(0, 0, 64, 256);
  for (let y = 6; y < 250; y += 10) for (let x = 6; x < 60; x += 12) { if (Math.random() < 0.55) { g.fillStyle = Math.random() < 0.8 ? '#FFD9A0' : '#9FD4FF'; g.fillRect(x, y, 6, 6); } }
  const t = new THREE.CanvasTexture(c); t.wrapS = t.wrapT = THREE.RepeatWrapping; return t;
}
const skyline = new THREE.Group(); scene.add(skyline);
{
  const tex = windowTex();
  const mat = new THREE.MeshStandardMaterial({ color: '#0E1424', roughness: 0.9, emissive: '#FFFFFF', emissiveMap: tex, emissiveIntensity: 0.9 });
  const N = 170;
  const mesh = new THREE.InstancedMesh(new THREE.BoxGeometry(1, 1, 1), mat, N);
  const m4 = new THREE.Matrix4(), p = new THREE.Vector3(), q = new THREE.Quaternion(), s = new THREE.Vector3();
  for (let i = 0; i < N; i++) {
    const a = (i / N) * Math.PI * 2 + (Math.random() - 0.5) * 0.04, r = 440 + Math.random() * 120;
    const w = 12 + Math.random() * 18, h = 10 + Math.pow(Math.random(), 2.2) * 42, d = 12 + Math.random() * 18;
    p.set(Math.cos(a) * r, h / 2 - 0.5, Math.sin(a) * r); q.setFromAxisAngle(new THREE.Vector3(0, 1, 0), -a); s.set(w, h, d);
    m4.compose(p, q, s); mesh.setMatrixAt(i, m4);
    mesh.setColorAt(i, new THREE.Color().setHSL(0.62, 0.25, 0.08 + Math.random() * 0.06));
  }
  skyline.add(mesh);
  // a warm haze along the horizon behind the towers
  const haze = new THREE.Mesh(new THREE.CylinderGeometry(600, 600, 90, 64, 1, true), new THREE.MeshBasicMaterial({ color: '#B0603F', transparent: true, opacity: 0.1, side: THREE.BackSide, blending: THREE.AdditiveBlending, depthWrite: false, fog: false }));
  haze.position.y = 20; skyline.add(haze);
  // beacon lights on the tallest towers
  for (let i = 0; i < 14; i++) { const a = Math.random() * Math.PI * 2, r = 440 + Math.random() * 120; const b = new THREE.Mesh(new THREE.SphereGeometry(0.9, 8, 8), new THREE.MeshBasicMaterial({ color: '#FF5A4E' })); b.position.set(Math.cos(a) * r, 30 + Math.random() * 24, Math.sin(a) * r); b.userData.phase = Math.random() * 6; skyline.add(b); }
}
const SPACE_BG = new THREE.Color('#0D1321'), EARTH_FOG = new THREE.Color('#1C1A33');
function updateWorld(dt, now) {
  const earth = mode !== 'mem';
  sky.visible = earth; skyline.visible = earth; dust.visible = !earth;
  scene.fog.color.copy(earth ? EARTH_FOG : SPACE_BG); scene.background = earth ? null : SPACE_BG;
  if (earth) for (const b of skyline.children) if (b.userData.phase != null) b.visible = Math.sin(now / 600 + b.userData.phase) > 0.3;
}

// ---------- idle field (landing) ----------
const idle = new THREE.Group(); scene.add(idle);
const idleMat = new THREE.MeshStandardMaterial({ color: COL_DIM, roughness: 0.8, emissive: COL_BLUE, emissiveIntensity: 0.08, transparent: true });
for (let i = 0; i < 14; i++) for (let j = 0; j < 9; j++) {
  const m = new THREE.Mesh(boxGeo, idleMat);
  m.userData.p = [i - 6.5, j - 4]; idle.add(m);
}
let idleLevel = 1, started = false; // idle field eases out once a scan starts and never returns
function updateIdle(dt, now) {
  if (!idle.visible) return;
  idleLevel = REDUCED ? (started ? 0 : 1) : smooth(idleLevel, started ? 0 : 1, 1 - Math.exp(-dt * 3));
  idleMat.opacity = idleLevel;
  for (const m of idle.children) {
    const [i, j] = m.userData.p;
    const h = (1.5 + 3 * (0.5 + 0.5 * Math.sin(now / 900 + i * 0.7 + j * 0.9))) * idleLevel;
    m.scale.set(7.6, Math.max(0.01, h), 7.6); m.position.set(i * 8.6 - 4, h / 2, j * 8.6 + 6);
  }
  if (idleLevel < 0.01) idle.visible = false;
}


// ---------- state ----------
const params = new URLSearchParams(location.search);
let filter = params.get('f') || ''; // '', flagged, junk, large, stale, idle
let idleDays = Math.min(365, Math.max(0, +params.get('d') || 0)); // idle slider threshold; 0 = off
if (filter === 'idle' && !idleDays) filter = '';
const FILTER_COLOR = TIER_COLOR; // 'flagged' and 'idle' keep per-item colors
const hiddenPaths = new Map(); // path -> node, things you told Di to keep off the map for this scan
const hiddenUnder = (p) => [...hiddenPaths.keys()].some((h) => p === h || p.startsWith(h + '/'));
function matches(n) {
  if (hiddenPaths.has(n.path)) return false;
  if (!filter || !n.path) return true;
  if (filter === 'idle') return n.is_dir ? (n.idle_size ?? 0) > 0 : ageDays(n) >= idleDays;
  const ok = (c) => filter === 'flagged' || c.tier === filter;
  const own = gunkSet.get(n.path);
  if (own) return ok(own);
  return n.is_dir && gunkList.some((c) => c.path.startsWith(n.path + '/') && ok(c));
}
const matchedSize = (n) => filter === 'idle' ? (n.is_dir ? n.idle_size ?? 0 : ageDays(n) >= idleDays ? n.size : 0) : gunkSet.get(n.path)?.size ?? gunkList.filter((c) => c.path.startsWith(n.path + '/') && (filter === 'flagged' || c.tier === filter)).reduce((s, c) => s + c.size, 0);
let scanDone = false;
let current = null, gunkSet = new Map(), hovered = null, selected = null, rootName = '', rootPath = '', rootSize = 1, scanning = false, flyHome = false;
const HOME_TARGET = new THREE.Vector3(27, 0, 6);
let camGoal = null; // {pos, target, until}
function dive(rect, out) {
  if (REDUCED) return;
  const c = new THREE.Vector3(rect.x + rect.w / 2, 4, rect.z + rect.h / 2);
  const pos = out ? camera.position.clone().add(new THREE.Vector3(0, 30, 30)) : camera.position.clone().lerp(c, 0.45);
  camGoal = { pos, target: out ? controls.target.clone() : c, until: performance.now() + 260 };
}
function updateCamera(dt, now) {
  if (camGoal) {
    if (now > camGoal.until && !camGoal.hold) camGoal = { pos: HOME_CAM, target: HOME_TARGET, until: Infinity, home: true };
    const f = 1 - Math.exp(-dt * (camGoal.home ? 3.5 : 9));
    camera.position.lerp(camGoal.pos, f); controls.target.lerp(camGoal.target, f);
    if ((camGoal.home && camera.position.distanceTo(HOME_CAM) < 0.3) || (camGoal.hold && camera.position.distanceTo(camGoal.pos) < 0.3)) camGoal = null;
  }
}
const parentOf = (p) => p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '';

function entriesFor(kids, { live } = {}) {
  const max = Math.max(1, ...kids.map((k) => k.size));
  return (live ? gridOf(kids) : layoutOf(kids)).map((r) => {
    const n = r.item, key = live ? n.name : n.path;
    const gunk = !live && gunkSet.has(n.path);
    const base = colorMode === 'type' && n.types ? TYPE_COLOR[dominant(n.types)] : heatColor(Math.min(1, ageDays(n) / 365)); // idle view keeps the usual colours; the threshold shows up top instead
    const color = live ? (colorMode === 'type' && n.types?.some(Boolean) ? TYPE_COLOR[dominant(n.types)] : n.done ? COL_BLUE : COL_FORM) : n.path === '' ? COL_DIM : FILTER_COLOR[filter] ?? (gunk && filter !== 'idle' && colorMode !== 'type' ? TIER_COLOR[gunkSet.get(n.path).tier] : base);
    const sub = live ? (n.done ? fmt(n.size) : `${fmtScan(n.size)} so far`) : filter === 'idle' ? `${fmt(n.size)} idle` : filter ? `${fmt(n.size)} flagged` : n.is_dir ? `${fmt(n.size)} · ${fmtN(n.files)} files` : fmt(n.size);
    const inner = !live && n.children ? n.children.filter((c) => c.size > 0 && c.path && matches(c)).map((c) => filter ? { ...c, size: matchedSize(c) } : c).slice(0, 16) : null;
    const edge = gunk && !live && filter !== 'idle' ? TIER_COLOR[gunkSet.get(n.path).tier] : null; // flagged items keep a tier-coloured rim even in type mode
    return { key, name: n.name, sub, node: live ? { ...n, path: n.name } : n, rect: { x: r.x, z: r.z, w: r.w, h: r.h }, y: heightFor(n.size, max), color, edge, em: live ? 0.03 : gunk || filter ? 0.45 : 0.12, pulse: gunk && !live ? 'slow' : false, inner, fly: live };
  });
}

// view cache: hovering a folder prefetches it so exploding into it is instant
const views = new Map();
function fetchView(path) {
  const idle = filter === 'idle' ? `&idle=${idleDays}` : '', key = path + idle;
  if (!views.has(key)) {
    const q = encodeURIComponent(path);
    views.set(key, Promise.all([api(`/api/tree?path=${q}&depth=2${idle}`), api(`/api/gunk?path=${q}${idle}`), api(`/api/summary?path=${q}`), idle ? api(`/api/idle?path=${q}&days=${idleDays}`) : null]).catch((e) => { views.delete(key); throw e; }));
  }
  return views.get(key);
}
let summaryData = null, idleFiles = null;
let navToken = 0, navBusy = 0, lastInput = 0;
const navHist = []; // folders you came from, newest last
function goBack() { const p = navHist.pop(); if (p !== undefined) navigate(p, { back: true }); }
addEventListener('pointerdown', () => (lastInput = performance.now()), true);
addEventListener('keydown', () => (lastInput = performance.now()), true);
async function navigate(path, opts = {}) {
  const token = ++navToken;
  // a slow open shows as a busy cursor and a line in the hint, so a click never looks ignored
  const slow = opts.quiet ? null : setTimeout(() => { document.body.classList.add('busy'); $('#hint').textContent = `Di: opening ${path ? path.split('/').pop() : rootName}…`; }, 150);
  navBusy++;
  let view; try { view = await fetchView(path); } catch (e) { if ($('#hint').textContent.startsWith('Di: opening')) $('#hint').textContent = `Di: could not open that · ${e.message}`; throw e; } finally { navBusy--; clearTimeout(slow); if (!navBusy) document.body.classList.remove('busy'); }
  const [node, gunk, sum, idleList] = view;
  if (token !== navToken || mode !== 'disk') return; // a newer click or a switch to Memory beat this one
  if (current?.path !== undefined && current.path !== path && !opts.back) navHist.push(current.path); // every real move is one Back away
  current = node; gunkList = gunk; summaryData = sum; idleFiles = idleList; gunkSet = new Map(gunk.map((c) => [c.path, c]));
  selected = null; hovered = null;
  let kids = node.children.filter((c) => (c.size > 0 || (c.is_dir && !filter)) && matches(c)); // empty folders are noise under a filter
  if (filter) kids = kids.map((c) => ({ ...c, size: matchedSize(c), full_size: c.size })).sort((x, y) => y.size - x.size); // block area = matched bytes, not whole folder; full_size keeps the whole
  const entries = entriesFor(kids);
  const to = opts.toPath ? entries.find((e) => e.key === opts.toPath)?.rect : opts.to;
  setBlocks(entries, { from: opts.from, to, stagger: opts.stagger });
  renderCrumbs(); renderStats(); renderSidebar();
  if (opts.highlight) flash(opts.highlight);
  if ($('#hint').textContent.startsWith('Di: opening')) $('#hint').textContent = 'Click a folder to open it. Right-click for more. Esc goes back.';
  history.replaceState(null, '', (filter ? `?f=${filter}${filter === 'idle' ? `&d=${idleDays}` : ''}` : location.pathname) + '#' + path);
}
function flash(path) {
  const m = byKey.get(path);
  if (!m) return;
  setSelected(m);
  m.userData.flashUntil = performance.now() + 900;
}
function goUp() {
  if (!current || current.path === '' || scanning) return;
  dive(FULL, true);
  navigate(parentOf(current.path), { from: FULL, toPath: current.path, highlight: current.path });
}
function enter(m) {
  const n = m.userData.node;
  if (!n.is_dir || !n.path || scanning) return;
  const r = m.userData.target;
  dive(r, false); burst(r); escortTo(m.userData.entry.key); // the chopper heads for the folder you opened, then peels off as it explodes
  navigate(n.path, { from: r, to: r });
}

// ---------- picking ----------
const ray = new THREE.Raycaster(), mouse = new THREE.Vector2(-2, -2); mouse.px = 0; mouse.py = 0; // px/py: last pointer position in pixels
let downAt = null;
const tip = $('#tip');
let tipX = 0, tipY = 0;
function placeTip() { const w = tip.offsetWidth, h = tip.offsetHeight; tip.style.left = `${Math.min(tipX + 16, innerWidth - w - 8)}px`; tip.style.top = `${tipY + 18 + h > innerHeight - 8 ? tipY - h - 12 : tipY + 18}px`; }
function showTip(n) {
  if (!n) { tip.hidden = true; return; }
  const cand = gunkSet.get(n.path), share = current?.size ? n.size / current.size : 0;
  tip.innerHTML = `<div class="n"></div><div class="m"></div><div class="m"></div><div class="f"></div><div class="h"></div>`;
  const [name, m1, m2, f, h] = tip.children;
  name.textContent = n.name;
  m1.textContent = `${fmt(n.size)} · ${(share * 100).toFixed(share < 0.1 ? 1 : 0)}% of ${current.name}`;
  const ts = typeShare(n);
  m2.textContent = `${n.is_dir ? fmtN(n.files) + ' files · ' : ''}${fmtAge(ageDays(n))}${ts ? ` · ${n.is_dir ? 'mostly ' : ''}${TYPE_NAMES[ts.i]}${n.is_dir ? ` (${Math.round(ts.share * 100)}%)` : ''}` : ''}`;
  f.textContent = cand ? `${TIER_LABEL[cand.tier]} · ${cand.what}. ${cand.note}` : n.is_dir ? (flaggedUnder(n.path) ? `${fmt(flaggedUnder(n.path))} flagged inside` : '') : '';
  if (cand) f.style.color = TIER_HEX[cand.tier];
  h.textContent = n.is_dir ? 'Click to open · right-click for more' : 'Click to select · right-click for more';
  tip.hidden = false; placeTip();
}
renderer.domElement.addEventListener('pointermove', (e) => { mouse.set((e.clientX / innerWidth) * 2 - 1, -(e.clientY / innerHeight) * 2 + 1); tipX = e.clientX; tipY = e.clientY; if (!tip.hidden) placeTip(); });
renderer.domElement.addEventListener('pointerdown', (e) => { downAt = [e.clientX, e.clientY]; if (camGoal?.home || camGoal?.hold) camGoal = null; });
renderer.domElement.addEventListener('pointerup', (e) => {
  if (e.button !== 0 || !downAt || Math.hypot(e.clientX - downAt[0], e.clientY - downAt[1]) > 4) return;
  if (mode === 'mem') { if (orbit.on && orbit.hover) openDrawer(orbit.hover); return; }
  if (!hovered) { setSelected(null); return; }
  if (hovered.userData.node.is_dir) enter(hovered); else setSelected(hovered);
});
renderer.domElement.addEventListener('pointerleave', () => { mouse.set(-2, -2); tip.hidden = true; });
let rotHold = false; // space during a scan: hold the slow lap still
const typing = () => { const a = document.activeElement; return a?.tagName === 'INPUT' || a?.tagName === 'TEXTAREA' || a?.isContentEditable; }; // keyboard shortcuts stay out of text fields
addEventListener('keydown', (e) => { if (e.key === ' ' && mode === 'disk' && scanning && !typing()) { e.preventDefault(); rotHold = !rotHold; toast(rotHold ? 'Di: holding the camera · space to resume' : 'Di: circling again', 'du'); return; }
  if (e.key === ' ' && mode === 'mem' && memView === 'orbit' && !typing()) { e.preventDefault(); orbit.frozen = !orbit.frozen; toast(orbit.frozen ? 'Me: holding still · space to resume' : 'Me: live again', 'me'); $('#pause').setAttribute('aria-pressed', String(orbit.frozen)); $('#pause').textContent = orbit.frozen ? 'Paused' : 'Pause'; return; }
  if (e.key === '/' && mode === 'mem' && !typing()) { e.preventDefault(); $('#memq').focus(); return; }
  if (e.key === 'Escape' && document.activeElement === $('#memq')) { $('#memq').value = ''; memFilter.q = ''; refilter(); $('#memq').blur(); return; }
  if (e.key === 'm' && mode === 'disk' && hog.returnTo && !typing()) { const to = hog.returnTo; hog.returnTo = null; hog.pendingSel = to.pid; enterHog(); return; }
  if (dlg.open || gearEl.open || resetEl.open) return;
  if (e.key === 'Escape' && ru.open) { ruClose(); return; }
  if ((e.key === 'Escape' || e.key === 'Backspace') && !typing()) { e.preventDefault(); if (mode === 'mem') closeDrawer(); else selected ? setSelected(null) : goUp(); } });

function setSelected(m) {
  selected = m;
  if (m) escortTo(m.userData.entry.key); else escortRelease();
  if (!current) return; // nothing to render outside the disk map
  renderFocus(); renderDisk();
}
function updateHover() {
  ray.setFromCamera(mouse, camera);
  const hit = ray.intersectObjects(blocks.children, false)[0]?.object ?? null;
  const h = hit && !hit.userData.dying && hit.userData.node?.path ? hit : null;
  if (h === hovered) return;
  hovered = h;
  if (hovered?.userData.node.is_dir && !scanning) fetchView(hovered.userData.node.path).catch(() => {});
  renderer.domElement.style.cursor = hovered ? 'pointer' : '';
  showTip(hovered && !scanning ? hovered.userData.node : null);
  if (scanning) return;
  $('#hint').textContent = hovered ? `${hovered.userData.node.name} · ${fmt(hovered.userData.node.size)} · ${fmtAge(ageDays(hovered.userData.node))}` : 'Click a folder to open it. Right-click for more. Esc goes back.';
}
let rowHover = null; // key of the sidebar row under the pointer, lights up its block

// ---------- context menu ----------
const menu = $('#menu');
function showMenu(x, y, n) {
  tip.hidden = true;
  menu.innerHTML = ''; menu.hidden = false;
  const t = document.createElement('div'); t.className = 't'; t.textContent = n.path || n.name; menu.appendChild(t);
  const add = (label, fn, cls = '') => { const b = document.createElement('button'); b.className = cls; b.textContent = label; b.onclick = () => { hideMenu(); fn(); }; menu.appendChild(b); };
  add(n.is_dir ? 'Open in Finder' : 'Reveal in Finder', () => api('/api/open', { path: n.path }).catch((e) => toast(e.message)));
  if (n.is_dir && n.path !== current.path) add('Explore here', () => { const m = byKey.get(n.path); m ? enter(m) : navigate(n.path); });
  add('Copy path', () => navigator.clipboard?.writeText(rootPath + '/' + n.path).then(() => toast('Path copied')));
  if (n.path) add('Send to Ru', () => ruTake(n.path, 'menu'));
  if (n.path && n.path !== current.path) add('Hide from map', () => { hiddenPaths.set(n.path, n); for (const [p] of picked) if (p === n.path || p.startsWith(n.path + '/')) picked.delete(p); saveState(); navigate(current.path); toast(`Di: ${n.name} hidden · ${fmt(n.size)}`, 'du'); });
  const w = menu.offsetWidth, h = menu.offsetHeight;
  menu.style.left = `${Math.min(x, innerWidth - w - 8)}px`; menu.style.top = `${Math.min(y, innerHeight - h - 8)}px`;
}
function hideMenu() { menu.hidden = true; }
function renderHidden() { // pill on the stats line; the line is rebuilt on every render so the button is too
  const n = hiddenPaths.size; if (!n) return;
  const b = document.createElement('button'); b.id = 'hid'; b.type = 'button'; b.title = 'Things you hid from the map';
  b.textContent = `${n} hidden · ${fmt([...hiddenPaths.values()].reduce((s, x) => s + x.size, 0))}`; b.onclick = showHiddenList;
  $('#stats').appendChild(b);
}
function showHiddenList(e) {
  tip.hidden = true; menu.innerHTML = ''; menu.hidden = false;
  const add = (label, fn, cls = '') => { const b = document.createElement('button'); b.className = cls; b.textContent = label; b.onclick = () => { hideMenu(); fn(); }; menu.appendChild(b); };
  const t = document.createElement('div'); t.className = 't'; t.textContent = 'Hidden from the map · click to bring back'; menu.appendChild(t);
  for (const [p, x] of hiddenPaths) add(`${x.name} · ${fmt(x.size)}`, () => { hiddenPaths.delete(p); saveState(); navigate(current.path); });
  add('Bring all back', () => { hiddenPaths.clear(); saveState(); navigate(current.path); }, 'hot');
  const r = e.currentTarget.getBoundingClientRect(); menu.style.left = `${Math.min(r.left, innerWidth - menu.offsetWidth - 8)}px`; menu.style.top = `${r.bottom + 6}px`;
};
addEventListener('pointerdown', (e) => { if (!menu.contains(e.target)) hideMenu(); }, true);
addEventListener('keydown', (e) => { if (e.key === 'Escape') hideMenu(); }, true);
renderer.domElement.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  if (scanning) return;
  showMenu(e.clientX, e.clientY, hovered ? hovered.userData.node : current);
});
$('#panel').addEventListener('contextmenu', (e) => {
  const row = e.target.closest('.row[data-key]'); if (!row) return;
  e.preventDefault();
  const n = current.children.find((c) => c.path === row.dataset.key) ?? gunkSet.get(row.dataset.key);
  if (n) showMenu(e.clientX, e.clientY, n);
});

// ---------- chrome ----------
function renderCrumbs() {
  const parts = current?.path ? current.path.split('/') : [];
  const im = $('#idlemark'); im.hidden = !(filter === 'idle' && mode === 'disk' && !scanning);
  if (!im.hidden) im.innerHTML = `untouched ${idleDays >= 365 ? 'a year' : idleDays + ' days'}+<small>inside ${current?.path ? current.name : rootName}</small>`;
  const el = $('#crumbs'); el.innerHTML = '';
  const home = document.createElement('button'); home.className = 'home'; home.title = 'Scan a different drive or folder'; home.setAttribute('aria-label', home.title);
  home.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 11 12 4l8.5 7M5.5 9.5V20h13V9.5"/></svg>';
  home.onclick = showLanding; el.appendChild(home); el.appendChild(whoBadge('du'));
  const add = (label, path, cur) => {
    const b = document.createElement('button'); b.textContent = label; if (cur) b.className = 'cur'; else b.onclick = () => navigate(path);
    el.appendChild(b);
  };
  add(rootName, '', parts.length === 0);
  parts.forEach((p, i) => { const s = document.createElement('span'); s.className = 'sep'; s.textContent = '/'; el.appendChild(s); add(p, parts.slice(0, i + 1).join('/'), i === parts.length - 1); });
  if (scanning) return;
  if (hog.returnTo) {
    const b = document.createElement('button'); b.className = 'rescan back'; b.title = 'Back to Memory with this process open';
    b.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M15 5l-7 7 7 7"/></svg>' + `Back to Me · ${hog.returnTo.name}`;
    b.onclick = () => { const to = hog.returnTo; hog.returnTo = null; hog.pendingSel = to.pid; enterHog(); };
    el.appendChild(b);
  }
  const r = document.createElement('button'); r.className = 'rescan';
  r.title = parts.length ? 'Scan this folder as the new root' : 'Scan again';
  r.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/></svg>' + (parts.length ? 'Rescan here' : 'Rescan');
  r.onclick = () => startScan(rootPath + '/' + current.path); // rootPath is '' for a root scan, so always join with '/'
  el.appendChild(r);
}
let drive = null; // drive holding the scan root
async function loadDrive() {
  const list = await api('/api/drives');
  const rp = rootPath || '/';
  drive = list.filter((d) => rp === d.mount || rp.startsWith(d.mount === '/' ? '/' : d.mount + '/')).sort((x, y) => y.mount.length - x.mount.length)[0] ?? null;
}
function renderDisk() {
  const el = $('#disk');
  if (!drive || !current) { el.hidden = true; return; }
  const n = selected ? selected.userData.node : current;
  const sel = Math.min(n.full_size ?? n.size, drive.total), free = drive.available, oth = Math.max(0, drive.total - free - sel);
  const pct = (v) => `${(v / drive.total * 100).toFixed(v / drive.total < 0.1 ? 1 : 0)}%`;
  el.hidden = false;
  el.innerHTML = `<div class="cap"><i class="sel${free / drive.total < 0.1 ? ' lo' : ''}"></i><i class="oth"></i></div><div></div>`;
  el.querySelector('.sel').style.width = pct(sel); el.querySelector('.oth').style.width = pct(oth);
  el.lastChild.innerHTML = `<b>${fmt(free)} free</b> of ${fmt(drive.total)} on ${drive.name || drive.mount} · <b>${n.name}</b> is ${pct(sel)}`;
}
function renderStats() {
  renderDisk();
  $('#stats').className = '';
  let t = `${fmt(current.size)} in ${fmtN(current.files)} files · ${fmtAge(ageDays(current))}`;
  $('#stats').title = 'Live: the map updates when files change on disk';
  if (filter) { const shown = current.children.filter((c) => c.path && matches(c)); t += ` · ${filter === 'idle' ? `idle ${idleDays}+ days: ` : 'showing '}${shown.length} of ${current.children.filter((c) => c.path).length} (${fmt(shown.reduce((s, c) => s + matchedSize(c), 0))})`; }
  $('#stats').textContent = t; $('#stats').insertAdjacentHTML('beforeend', '<span class="dot" aria-label="live"></span>');
  if (mapAsOf) { const a = document.createElement('span'); a.className = 'asof'; a.textContent = `map from ${ago(mapAsOf)} · Rescan for fresh sizes`; a.title = 'Opened from the last snapshot. Changes since then are picked up live, but sizes are as of that scan.'; $('#stats').appendChild(a); }
  renderHidden();
}
$('#legend').onclick = (e) => {
  const b = e.target.closest('button[data-f]'); if (!b || scanning || !current) return;
  filter = b.dataset.f; idleDays = 0; saveState();
  syncFilterButtons();
  navigate(current.path);
};
$('#types').innerHTML = TYPE_NAMES.map((t, i) => `<span style="--c:${TYPE_HEX[i]}"><i></i>${t}</span>`).join('');
function syncColorMode() {
  document.body.dataset.color = colorMode;
  for (const b of $('#colors').querySelectorAll('button')) b.setAttribute('aria-pressed', String(b.dataset.m === colorMode));
  $('#types').hidden = colorMode !== 'type';
}
$('#colors').onclick = (e) => {
  const b = e.target.closest('button[data-m]'); if (!b) return;
  colorMode = b.dataset.m; try { localStorage.setItem('dime.color', colorMode); } catch {} saveState();
  syncColorMode(); if (current && !scanning) navigate(current.path);
};
syncColorMode();
function syncFilterButtons() {
  for (const x of $('#legend').querySelectorAll('button')) x.setAttribute('aria-pressed', String(x.dataset.f === filter));
  $('#idle').value = idleDays;
  $('#idle-out').innerHTML = idleDays ? `idle <b>${idleDays >= 365 ? 'a year' : idleDays + ' days'}</b>+` : 'used today';
}
let idleTimer;
$('#idle').oninput = (e) => {
  idleDays = +e.target.value; filter = idleDays ? 'idle' : ''; saveState();
  syncFilterButtons();
  clearTimeout(idleTimer); idleTimer = setTimeout(() => { if (current && !scanning) navigate(current.path); }, 120);
};
syncFilterButtons();

let gunkList = [];
const under = (p, dir) => dir === '' || p === dir || p.startsWith(dir + '/');
const flaggedUnder = (dir) => gunkList.filter((c) => under(c.path, dir)).reduce((s, c) => s + c.size, 0);
const nodeInfo = (n) => `${fmt(n.size)}${n.full_size != null && n.full_size !== n.size ? ` ${filter === 'idle' ? 'idle' : 'flagged'} of ${fmt(n.full_size)}` : ''}${n.is_dir ? ` · ${fmtN(n.files)} files` : ''} · ${fmtAge(ageDays(n))}`;

const filterLabel = () => ({ safe: 'safe to remove', likely: 'probably safe', review: 'worth a look', flagged: 'flagged' })[filter] ?? '';
function renderSidebar() { $('#back').hidden = !navHist.length; renderFocus(); renderCleanup(); }
$('#back').onclick = goBack;
function showTab(v) { for (const t of $('#tabs').children) t.setAttribute('aria-selected', String(t.dataset.v === v)); $('#v-clean').hidden = v !== 'clean'; $('#v-browse').hidden = v !== 'browse'; }
$('#tabs').onclick = (e) => { const b = e.target.closest('button[data-v]'); if (b) showTab(b.dataset.v); };
// drag the panel's left edge to widen it; the map chrome follows via --pw
{ const setW = (w) => document.documentElement.style.setProperty('--pw', `${w}px`);
  try { const w = +localStorage.getItem('dime.panelw'); if (w) setW(Math.min(w, innerWidth - 300)); } catch {}
  const g = $('#grip');
  g.onpointerdown = (e) => {
    e.preventDefault(); g.setPointerCapture(e.pointerId); g.classList.add('on'); document.body.style.cursor = 'col-resize';
    let w = $('#panel').offsetWidth;
    g.onpointermove = (ev) => { w = Math.round(Math.min(Math.max(300, innerWidth - 16 - ev.clientX), innerWidth - 300)); setW(w); };
    g.onpointerup = () => { g.onpointermove = g.onpointerup = null; g.classList.remove('on'); document.body.style.cursor = ''; try { localStorage.setItem('dime.panelw', w); } catch {} };
  }; }

const tiersOn = new Set(['safe', 'likely', 'review']);
const TIER_ORDER = { safe: 0, likely: 1, review: 2 };
const KIND_HINT = {
  cache: 'Recreated on the next build or install', appcache: 'Apps rebuild these as needed', simulator: 'Xcode re-downloads what you use', trash: 'Empty the Trash to reclaim',
  build: 'Regenerated by the next build', logs: 'Old logs are rarely needed', downloads: 'Untouched for over a month', installer: 'Done with once installed or extracted',
  duplicate: 'Same name and size elsewhere', diskimage: 'Virtual disks, often oversized', project: 'No changes for six months or more', large: 'Over 100 MB each', stale: 'Over 10 MB and idle six months',
  archives: 'Old app builds from Xcode', models: 'Re-downloaded on demand', backups: 'Manage from Finder', idlefile: 'Not opened or changed for that long', ru: 'Nominated by Ru',
};
const specific = (note) => /Idle|Same name|No changes/.test(note);
const openKinds = new Set(); let kindsTouched = false; // once you fold or unfold a group yourself, the default 'first two open' stops applying

// the candidate list the panel works from: Di's flags, and in the idle view only those untouched long enough plus Di's untouched files
function cleanList() {
  if (filter !== 'idle') return gunkList;
  const old = gunkList; // already sized by idle bytes and filtered by the server
  const inside = (p) => old.some((c) => p === c.path || p.startsWith(c.path + '/')); // a file under a flagged folder is already counted there
  const files = (idleFiles ?? []).filter((f) => !inside(f.path)).map((f) => ({ path: f.path, name: f.name, size: f.size, is_dir: false, tier: 'review', reason: 'idlefile', what: 'Untouched files', note: `Idle ${f.age_days} days.`, age_days: f.age_days }));
  return [...old, ...files];
}
function renderCleanup() {
  const s = summaryData; if (!s) return;
  const head = $('#clean-head'), list = cleanList(), idle = filter === 'idle';
  for (const [p] of picked) { const f = list.find((c) => c.path === p); if (f) picked.set(p, f); } // sizes and idle flags match the view you act from
  // hidden folders drop out, then the headline and cards count only the tiers that are switched on, so they always agree with the list below
  const kindsAll = (idle
    ? [...new Set(list.map((c) => c.reason))].map((id) => { const its = list.filter((c) => c.reason === id); return [id, its[0].tier, its[0].what, sumOf(its), its.length]; })
    : s.kinds).map(([id, tier, what, size, n]) => { const gone = list.filter((c) => c.reason === id && hiddenUnder(c.path)); return [id, tier, what, size - sumOf(gone), n - gone.length]; }).filter((k) => k[4] > 0);
  const tierOf = (t) => kindsAll.filter((k) => k[1] === t).reduce((a, k) => [a[0] + k[3], a[1] + k[4]], [0, 0]);
  const tierSums = ['safe', 'likely', 'review'].map((t) => [t, ...tierOf(t)]);
  const sum = (pred) => tierSums.filter((t) => pred(t) && tiersOn.has(t[0])).reduce((a, t) => a + t[1], 0);
  const easy = sum((t) => t[0] !== 'review'), review = sum((t) => t[0] === 'review');
  head.innerHTML = `<div class="big"></div><div class="scope"></div>`;
  head.querySelector('.big').innerHTML = easy ? `${fmt(easy)}<small>safe to free</small>` : review ? `${fmt(review)}<small>worth a look</small>` : `Nothing to clean<small>here</small>`;
  if (easy && review) head.querySelector('.big').insertAdjacentHTML('afterend', `<div class="plus">plus ${fmt(review)} worth a look</div>`);
  const vb = document.createElement('button'); vb.id = 'shelf'; vb.className = 'btn sm quiet'; vb.textContent = 'Shelf'; vb.title = 'Shelved items, still on your disk, put back any time'; vb.style.marginTop = '10px'; vb.onclick = openShelf; head.appendChild(vb);
  api('/api/shelf').then((v) => { shelfList = v; if (v.length) vb.textContent = `Shelf · ${v.length} · ${fmt(sumOf(v))}`; }).catch(() => {});
  if (picked.size) { const lb = document.createElement('button'); lb.className = 'btn sm quiet'; lb.textContent = `Shortlist · ${picked.size}`; lb.title = 'Review the shortlist; shelve or delete from there'; lb.style.marginTop = '10px'; lb.style.marginLeft = '8px'; lb.onclick = () => listDialog([...picked.values()].map(candRow)); head.appendChild(lb); }
  const xb = document.createElement('button'); xb.className = 'btn sm quiet'; xb.textContent = 'Export'; xb.title = 'Save this cleanup view as a Markdown report'; xb.style.marginTop = '10px'; xb.style.marginLeft = '8px'; xb.onclick = exportReport; head.appendChild(xb);
  const scope = head.querySelector('.scope');
  scope.textContent = `${idle ? `untouched ${idleDays >= 365 ? 'a year' : idleDays + ' days'}+ · ` : ''}${current.path ? `in ${current.name} · ` : `in ${rootName}, ${fmtN(current.files)} files`}`;
  if (idle) { const b = document.createElement('button'); b.textContent = 'show everything'; b.onclick = () => { idleDays = 0; filter = ''; syncFilterButtons(); navigate(current.path); }; scope.appendChild(b); scope.append(' · '); }
  if (current.path) { const b = document.createElement('button'); b.textContent = `see whole ${rootName}`; b.onclick = () => navigate(''); scope.appendChild(b); }
  const tiers = $('#clean-tiers'); tiers.innerHTML = '';
  for (const [tier, size, n] of tierSums) {
    const b = document.createElement('button'); b.style.setProperty('--c', TIER_HEX[tier]); b.setAttribute('aria-pressed', String(tiersOn.has(tier)));
    b.innerHTML = `<b></b><span class="sw"></span><span></span><div class="n"></div>`;
    b.querySelector('b').textContent = n ? fmt(size) : '—'; b.children[2].textContent = TIER_LABEL[tier]; b.lastChild.textContent = n ? `${n} item${n === 1 ? '' : 's'}` : 'none';
    b.title = { safe: 'Caches, Trash, simulators. Apps recreate these.', likely: 'Old downloads, installers, logs, build output. Glance, then remove.', review: 'Large, idle, duplicates, untouched projects. Your call.' }[tier];
    b.onclick = () => { tiersOn.has(tier) && tiersOn.size > 1 ? tiersOn.delete(tier) : tiersOn.add(tier); saveState(); renderCleanup(); };
    tiers.appendChild(b);
  }
  const groups = $('#clean-groups'); groups.innerHTML = '';
  const kinds = kindsAll.filter((k) => tiersOn.has(k[1])).sort((x, y) => TIER_ORDER[x[1]] - TIER_ORDER[y[1]] || y[3] - x[3]);
  if (!kinds.length) { groups.innerHTML = '<div class="empty">Nothing flagged in this folder. Explore the map or scan somewhere else.</div>'; return; }
  const base = current.path ? current.path + '/' : '';
  kinds.forEach(([id, tier, what, size, n], i) => {
    const items = list.filter((c) => c.reason === id && !hiddenUnder(c.path));
    const d = document.createElement('details'); d.className = 'grp'; d.style.setProperty('--c', TIER_HEX[tier]);
    d.open = kindsTouched ? openKinds.has(id) : i < 2;
    d.ontoggle = () => { kindsTouched = true; d.open ? openKinds.add(id) : openKinds.delete(id); };
    d.innerHTML = `<summary><span class="tbar"></span><div><div class="what"></div><div class="why"></div></div><div><div class="tot"></div><div class="cnt"></div></div></summary>`;
    d.querySelector('.what').textContent = what;
    d.querySelector('.why').textContent = `${TIER_LABEL[tier]} · ${KIND_HINT[id] ?? ''}`;
    d.querySelector('.tot').textContent = fmt(size); d.querySelector('.cnt').textContent = items.length < n ? `biggest ${items.length} of ${n}` : `${n} item${n === 1 ? '' : 's'}`;
    const allIn = items.every((c) => picked.has(c.path));
    const pall = document.createElement('button'); pall.className = 'pall'; pall.type = 'button';
    pall.textContent = allIn ? 'remove all from list' : 'add all to list';
    pall.onclick = (e) => { e.preventDefault(); e.stopPropagation(); for (const c of items) allIn ? picked.delete(c.path) : picked.set(c.path, c); renderCleanup(); };
    d.querySelector('summary').lastElementChild.appendChild(pall);
    for (const c of items) d.appendChild(candRowEl(c, base, 0));
    groups.appendChild(d);
  });
  if (!groups.children.length) groups.innerHTML = '<div class="empty">Everything flagged here is hidden from the map.</div>';
  renderPickBar(); ruSync();
}

// ---------- acting on the shortlist: shelve or delete, always after a look at what is on it ----------
const picked = new Map(); // path -> candidate, kept while you browse so you can gather from several folders
let shelfList = [];
const sumOf = (rows) => rows.reduce((s, r) => s + r.size, 0);
function renderPickBar() {
  saveState();
  const bar = $('#clean-bar'); bar.hidden = !picked.size; if (!picked.size) return;
  const n = picked.size;
  bar.innerHTML = `<div><b></b><button class="clr" type="button">clear it</button></div><button class="btn sm quiet" type="button">Review shortlist</button>`;
  bar.querySelector('b').textContent = `${n} on your shortlist · ${fmt(sumOf([...picked.values()]))}`;
  bar.querySelector('.clr').onclick = () => { picked.clear(); saveState(); renderCleanup(); };
  bar.querySelector('.btn').onclick = () => listDialog([...picked.values()].map(candRow));
}
const dlg = $('#dlg');
// One dialog for every destructive step: a title, the exact list, optional checkboxes, an optional "I understand" gate, then the buttons.
function openDialog({ title, sub, rows, select, gate, actions }) {
  dlg.innerHTML = `<div class="head"><div class="title"></div><div class="sub"></div></div><div class="list"></div><label class="gate" hidden><input type="checkbox"><span></span></label><div class="foot"></div>`;
  dlg.querySelector('.title').textContent = title; dlg.querySelector('.sub').textContent = sub;
  const list = dlg.querySelector('.list');
  if (!rows.length) list.innerHTML = '<div class="empty">Nothing here yet.</div>';
  for (const r of rows) {
    const el = document.createElement('label'); el.className = 'drow';
    el.innerHTML = `<input type="checkbox"><div><div class="name"></div><div class="sub"></div></div><div class="sz"></div>`;
    const cb = el.querySelector('input'); cb.value = r.key; cb.checked = select ? r.checked ?? true : true; cb.hidden = !select; cb.onchange = sync;
    el.querySelector('.name').textContent = r.name; el.querySelector('.name').classList.toggle('dir', !!r.is_dir);
    el.querySelector('.sub').textContent = r.sub; el.querySelector('.sz').textContent = fmt(r.size);
    if (r.files) { const pk = document.createElement('button'); pk.type = 'button'; pk.className = 'peek'; pk.textContent = 'see which files'; pk.onclick = (e) => { e.preventDefault(); e.stopPropagation(); dlg.close(); idleFilesDialog(r.files, r.name, true); }; el.querySelector('.sub').after(pk); }
    list.appendChild(el);
  }
  const gateEl = dlg.querySelector('.gate');
  if (gate) { gateEl.hidden = false; gateEl.querySelector('span').textContent = gate; gateEl.querySelector('input').onchange = sync; }
  const foot = dlg.querySelector('.foot');
  const cancel = document.createElement('button'); cancel.className = 'btn sm quiet'; cancel.textContent = 'Cancel'; cancel.onclick = () => dlg.close(); foot.appendChild(cancel);
  const chosen = () => [...list.querySelectorAll('input:checked')].map((c) => rows.find((r) => r.key === c.value));
  const btns = actions.map((a) => { const b = document.createElement('button'); b.className = `btn sm ${a.cls ?? ''}`; b.onclick = () => { dlg.close(); a.fn(chosen()); }; foot.appendChild(b); return [b, a]; });
  function sync() { const c = chosen(), ok = c.length && (!gate || gateEl.querySelector('input').checked); for (const [b, a] of btns) { b.disabled = !ok; b.textContent = a.label(c); } }
  sync(); dlg.showModal();
}
const candRow = (c) => ({ key: c.path, name: c.name, is_dir: c.is_dir, size: c.size, files: c.full_size != null && c.full_size !== c.size ? c.path : null, sub: [parentOf(c.path) || rootName, c.what, c.full_size != null && c.full_size !== c.size ? `the idle ${fmt(c.size)} of ${fmt(c.full_size)}` : ''].filter(Boolean).join(' · ') });
// ---- a candidate row: tick it, unfold it in place to tick things inside it, or jump to it on the map. The map's scope never moves on its own.
const openRows = new Set(), kidCache = new Map(); // expanded paths, and each one's children (keyed by path plus the idle threshold)
function candRowEl(c, base, depth) {
  const wrap = document.createDocumentFragment();
  const rel = c.path.startsWith(base) ? c.path.slice(base.length) : c.path;
  const row = document.createElement('div'); row.className = 'row pick'; row.dataset.key = c.path; row.tabIndex = 0; row.style.paddingLeft = `${28 + depth * 18}px`;
  row.innerHTML = `<input class="pk" type="checkbox" aria-label="Add to shortlist" title="Add to your shortlist. Nothing moves until you act from there."><div><div class="name"></div><div class="sub"></div></div><div class="sz"></div><span class="acts"><button class="mp" type="button" title="Show on the map">Map</button><button class="fb" type="button">Finder</button></span>`;
  const pk = row.querySelector('.pk'); pk.checked = picked.has(c.path); row.classList.toggle('picked', pk.checked);
  pk.onclick = (e) => e.stopPropagation(); pk.onchange = () => { pk.checked ? picked.set(c.path, c) : picked.delete(c.path); renderCleanup(); };
  dragSource(row, c.path);
  const name = row.querySelector('.name'); name.textContent = c.name; name.classList.toggle('dir', c.is_dir);
  if (c.is_dir) { row.classList.add('fold'); row.classList.toggle('open', openRows.has(c.path)); }
  row.querySelector('.sub').textContent = [depth ? '' : parentOf(rel), c.full_size != null && c.full_size !== c.size ? `${fmt(c.size)} idle of ${fmt(c.full_size)} · only those files move` : '', specific(c.note) ? c.note : ''].filter(Boolean).join(' · ');
  row.querySelector('.sz').textContent = fmt(c.size);
  row.querySelector('.fb').onclick = (e) => { e.stopPropagation(); api('/api/open', { path: c.path }).catch((err) => toast(err.message)); };
  row.querySelector('.mp').onclick = (e) => { e.stopPropagation(); navigate(parentOf(c.path), { highlight: c.path }); };
  if (c.full_size != null && c.full_size !== c.size) { const pk = document.createElement('button'); pk.type = 'button'; pk.textContent = 'Files'; pk.title = 'See which files would actually move'; pk.onclick = (e) => { e.stopPropagation(); idleFilesDialog(c.path, c.name, false); }; row.querySelector('.acts').prepend(pk); }
  row.onmouseenter = () => (rowHover = base + rel.split('/')[0]); row.onmouseleave = () => (rowHover = null);
  row.onclick = () => { if (!c.is_dir) { pk.checked = !pk.checked; pk.onchange(); return; } openRows.has(c.path) ? openRows.delete(c.path) : openRows.add(c.path); renderCleanup(); };
  row.onkeydown = (e) => { if (e.key === 'Enter') row.onclick(); };
  wrap.appendChild(row);
  if (c.is_dir && openRows.has(c.path)) {
    const key = c.path + (filter === 'idle' ? `@${idleDays}` : ''), kids = kidCache.get(key);
    if (!kids) {
      const p = document.createElement('div'); p.className = 'empty'; p.style.paddingLeft = `${46 + depth * 18}px`; p.textContent = 'looking inside…'; wrap.appendChild(p);
      api(`/api/tree?path=${encodeURIComponent(c.path)}&depth=1${filter === 'idle' ? `&idle=${idleDays}` : ''}`).then((t) => { kidCache.set(key, t.children ?? []); }).catch(() => { kidCache.set(key, []); }).finally(() => { if (openRows.has(c.path)) renderCleanup(); });
    } else {
      const idle = filter === 'idle';
      const list = kids.map((k) => ({ path: k.path, name: k.name, size: idle ? k.idle_size ?? 0 : k.size, full_size: idle ? k.size : undefined, is_dir: k.is_dir, files: k.files, age_days: ageDays(k), tier: c.tier, reason: c.reason, what: c.what, note: `Inside ${c.name}.` })).filter((k) => k.size > 0).sort((x, y) => y.size - x.size);
      if (!list.length) { const p = document.createElement('div'); p.className = 'empty'; p.style.paddingLeft = `${46 + depth * 18}px`; p.textContent = idle ? 'nothing inside is idle that long' : 'empty'; wrap.appendChild(p); }
      const show = list.slice(0, 40);
      for (const k of show) wrap.appendChild(candRowEl(k, base, depth + 1));
      if (list.length > show.length) { const p = document.createElement('div'); p.className = 'empty'; p.style.paddingLeft = `${46 + depth * 18}px`; p.textContent = `and ${list.length - show.length} smaller, ${fmt(sumOf(list.slice(40)))}`; wrap.appendChild(p); }
    }
  }
  return wrap;
}
// the shortlist: the only place a destructive action starts. Ticks only gather here; Shelve (reversible) or Delete (gated) act on what is checked.
const idleOpt = () => filter === 'idle' ? { idle: idleDays } : {}; // in the idle view only the files that old move or go
function listDialog(rows, sub) {
  const idle = filter === 'idle', days = idleDays >= 365 ? 'a year' : `${idleDays} days`;
  sub ??= `Nothing has moved yet. Checked items are what the buttons act on. Shelving moves them to ~/.dime/shelf, where they stay on your disk until you delete them, so nothing is freed yet and everything can go back${idle ? `. In this idle view only the files inside each item untouched for ${days}+ move` : ''}. Delete asks again.`;
  openDialog({ title: 'Shortlist', sub, rows, select: true, actions: [
    { cls: 'go', label: (c) => `Shelve ${c.length} · ${fmt(sumOf(c))}`, fn: (c) => act('/api/shelf/add', { paths: c.map((r) => r.key), ...idleOpt() }, 'shelved') },
    { cls: 'danger quiet', label: (c) => `Delete ${c.length}…`, fn: (c) => openDialog({ title: 'Delete for good', sub: idle ? `Only the files untouched for ${days}+ are removed, right away. Not the Trash, not the shelf.` : 'Removed from disk right away. Not the Trash, not the shelf.', rows: c, select: false, gate: 'I understand this cannot be undone. Shelve it instead if unsure.',
      actions: [{ cls: 'danger', label: (x) => `Delete ${x.length} · ${fmt(sumOf(x))}`, fn: (x) => act('/api/delete', { paths: x.map((r) => r.key), ...idleOpt() }, 'deleted') }] }) },
  ] });
  if (rows.length) { // emptying the shortlist moves nothing, so it needs no confirmation
    const clr = document.createElement('button'); clr.type = 'button'; clr.className = 'btn sm quiet'; clr.textContent = 'Clear shortlist'; clr.style.marginRight = 'auto';
    clr.onclick = () => { picked.clear(); dlg.close(); if (current) renderCleanup(); else renderPickBar(); toast('Di: shortlist cleared', 'du'); };
    dlg.querySelector('.foot').prepend(clr);
  }
}
// "27 GB idle of 33 GB" says how much moves but nothing about what. This lists it, grouped by the
// folder the files sit in, because the leaves are often content hashes that tell you nothing.
async function idleFilesDialog(path, name, back) {
  let files;
  try { files = await api(`/api/idle?path=${encodeURIComponent(path)}&days=${idleDays}`); } catch (e) { toast(e.message); return; }
  const groups = new Map();
  for (const f of files) {
    const rel = f.path.startsWith(path + '/') ? f.path.slice(path.length + 1) : f.path, parts = rel.split('/');
    const key = parts.slice(0, Math.min(2, parts.length - 1)).join('/') || rel; // one file at the top level groups under itself
    const g = groups.get(key) ?? { size: 0, n: 0 };
    g.size += f.size; g.n++; groups.set(key, g);
  }
  const rows = [...groups].sort((a, b) => b[1].size - a[1].size).map(([k, g]) => ({ key: k, name: k, is_dir: g.n > 1, size: g.size, sub: `${g.n} file${g.n === 1 ? '' : 's'} untouched for ${idleDays}+ days` }));
  const shown = files.reduce((t, f) => t + f.size, 0);
  openDialog({
    title: `What would move out of ${name}`,
    sub: rows.length ? `${fmt(shown)} of it sits in the ${files.length} largest file${files.length === 1 ? '' : 's'}, grouped below. The rest is in files under 1 MB, which move too.`
      : 'Nothing of 1 MB or more inside this has been idle that long.',
    rows, select: false,
    actions: back ? [{ label: () => 'Back to the shortlist', fn: () => listDialog([...picked.values()].map(candRow)) }] : [],
  });
}
async function openShelf() {
  try { shelfList = await api('/api/shelf'); } catch (e) { toast(e.message); return; }
  const rows = shelfList.map((e) => ({ key: e.id, name: e.name, is_dir: e.is_dir, size: e.size, checked: false, sub: [tilde(parentOf(e.from)), e.partial ? `${e.count} idle file${e.count === 1 ? '' : 's'} out of it, the rest stayed` : '', new Date(e.shelved_at * 1000).toLocaleDateString(), e.note].filter(Boolean).join(' · ') }));
  openDialog({ title: 'Shelf', sub: rows.length ? `${rows.length} item${rows.length === 1 ? '' : 's'} · ${fmt(sumOf(rows))} on the shelf in ~/.dime/shelf. Still on your disk: deleting from here is what frees the space. Put back returns things exactly where they were.` : 'Nothing on the shelf yet. Tick items in Cleanup, then shelve them from the shortlist.',
    rows, select: true,
    actions: [
      { label: (c) => c.length ? `Put back ${c.length}` : 'Put back', fn: (c) => act('/api/shelf/restore', { ids: c.map((r) => r.key) }, 'put back') },
      { cls: 'danger', label: (c) => c.length ? `Delete ${c.length} for good…` : 'Delete ticked…', fn: purgeShelf },
    ] });
  if (rows.length) {
    const ru = document.createElement('button'); ru.type = 'button'; ru.className = 'btn sm ru'; ru.textContent = '✦ Ask Ru'; ru.style.marginRight = 'auto';
    ru.onclick = () => { dlg.close(); ruOpen('shelf'); };
    // Emptying the shelf and deleting a selection are the same act on different rows, so never show
    // both: Empty stands in until you tick something, then the row buttons take over.
    const all = document.createElement('button'); all.type = 'button'; all.className = 'btn sm danger quiet'; all.textContent = `Empty the shelf · ${fmt(sumOf(rows))}`;
    all.onclick = () => purgeShelf(rows);
    dlg.querySelector('.foot').prepend(ru, all);
    const list = dlg.querySelector('.list'), sync = () => (all.hidden = !!list.querySelector('input:checked'));
    list.addEventListener('change', sync); sync();
  }
}
/// Second look before anything leaves the disk for good, shared by the ticked rows and Empty the shelf.
const purgeShelf = (rows) => openDialog({
  title: rows.length === shelfList.length ? 'Empty the shelf' : 'Delete for good',
  sub: 'Removed from the shelf and from your disk. This is the step that frees the space.',
  rows, select: false, gate: 'I understand this cannot be undone.',
  actions: [{ cls: 'danger', label: (x) => `Delete ${x.length} · ${fmt(sumOf(x))}`, fn: (x) => act('/api/shelf/delete', { ids: x.map((r) => r.key) }, 'deleted') }],
});
const nestedIn = (p, keys) => keys.some((k) => k !== p && (k === '' || p.startsWith(k + '/')));
// One move at a time, with something on screen while it runs: without it a slow shelve looks like
// nothing happened, and the obvious response is to click it again.
let acting = false;
function progress(text) {
  const el = $('#prog');
  if (text === null) { el.hidden = true; el.classList.remove('wait'); return; }
  el.querySelector('.pl').textContent = text;
  el.hidden = false;
}
async function withProgress(verb, run) {
  const el = $('#prog'), fill = el.querySelector('.track i');
  progress(`${verb}…`); el.classList.add('wait');
  const poll = setInterval(async () => {
    let s; try { s = await api('/api/status'); } catch { return; }
    const j = s.job;
    if (!j || !j.total) return;
    el.classList.remove('wait');
    fill.style.width = `${Math.round((j.done / j.total) * 100)}%`;
    progress(`${verb} ${j.done} of ${j.total}`);
  }, 250);
  try { return await run(); } finally { clearInterval(poll); fill.style.width = ''; progress(null); }
}
async function act(url, body, verb) {
  if (acting) { toast('Di: still working on the last one', 'du'); return; }
  if (body.paths) { const keys = [...new Set(body.paths)]; body.paths = keys.filter((p) => !nestedIn(p, keys)); } // a folder takes what is inside it; sending the child too would only fail
  acting = true;
  let res; try { res = await withProgress(verb === 'deleted' ? 'Deleting' : verb === 'put back' ? 'Putting back' : 'Shelving', () => api(url, body)); } catch (e) { toast(`Di: ${e.message}`, 'du'); return; } finally { acting = false; }
  const ok = res.filter((r) => r.ok), bad = res.filter((r) => !r.ok);
  for (const [p] of picked) if (ok.some((r) => p === r.key || p.startsWith(r.key + '/'))) picked.delete(p); // the item and anything ticked inside it
  for (const p of [...ru.sel.keys()]) if (ok.some((r) => p === r.key || p.startsWith(r.key + '/'))) ru.sel.delete(p);
  toast(bad.length ? `Di: ${ok.length} ${verb}, ${bad.length} failed · ${bad[0].error}` : `Di: ${ok.length} item${ok.length === 1 ? '' : 's'} ${verb}`, 'du');
  if (!current) { renderPickBar(); return; }
  if (url !== '/api/shelf/delete' && ok.some((r) => current.path === r.key || current.path.startsWith(r.key + '/'))) { const gone = ok.find((r) => current.path === r.key || current.path.startsWith(r.key + '/')); navHist.length = 0; await navigate(parentOf(gone.key)); return; } // the folder we were in is gone: step out of it
  await refresh({ force: true });
}

let allKids = false; // "show all" toggle for the folders-inside list
function renderFocus() {
  if (!current) return;
  const el = $('#focus'); el.innerHTML = '';
  const parts = current.path ? current.path.split('/') : [];
  // where you are: Up button + clickable trail, then the folder itself
  const trail = document.createElement('div'); trail.className = 'trail';
  const up = document.createElement('button'); up.className = 'up'; up.title = 'Up one level (Esc)'; up.setAttribute('aria-label', 'Up one level'); up.textContent = '↑'; up.disabled = !current.path; up.onclick = goUp; trail.appendChild(up);
  const crumb = (label, path) => { const b = document.createElement('button'); b.className = 'crumb'; b.textContent = label; b.onclick = () => navigate(path); trail.appendChild(b); const s = document.createElement('span'); s.textContent = '›'; trail.appendChild(s); };
  if (parts.length) { crumb(rootName, ''); parts.slice(0, -1).forEach((p, i) => crumb(p, parts.slice(0, i + 1).join('/'))); } else trail.append('Scan root');
  el.appendChild(trail);
  const name = document.createElement('div'); name.className = 'name'; name.textContent = current.name + (current.path ? ' /' : ''); el.appendChild(name);
  const meta = document.createElement('div'); meta.className = 'meta';
  const share = current.path ? current.size / rootSize : 1;
  meta.textContent = `${nodeInfo(current)}${current.path ? ` · ${(share * 100).toFixed(share < 0.1 ? 1 : 0)}% of ${rootName}` : ''}`; el.appendChild(meta);
  const flagged = flaggedUnder(current.path);
  if (flagged) { const f = document.createElement('div'); f.className = 'flag'; f.textContent = `${fmt(flagged)} flagged inside`; el.appendChild(f); }
  const acts = document.createElement('div'); acts.className = 'acts';
  const mk = (label, cls, fn, n) => { const b = document.createElement('button'); b.className = `btn sm ${cls}`; b.textContent = label; b.onclick = fn; (n ?? acts).appendChild(b); };
  const finder = (n) => () => api('/api/open', { path: n.path }).catch((e) => toast(e.message));
  mk('Finder', 'quiet', finder(current)); el.appendChild(acts);
  // what you clicked on the map
  if (selected) {
    const n = selected.userData.node, cand = gunkSet.get(n.path);
    const sel = document.createElement('div'); sel.className = 'sel';
    sel.innerHTML = `<div class="k">Selected on the map</div><div class="r"><div><div class="n"></div><div class="m"></div></div><div class="a"></div></div>`;
    sel.querySelector('.n').textContent = n.name + (n.is_dir ? ' /' : ''); sel.querySelector('.m').textContent = `${nodeInfo(n)} · ${((n.size / current.size) * 100).toFixed(n.size / current.size < 0.1 ? 1 : 0)}% of ${current.name}${cand ? ` · ${TIER_LABEL[cand.tier]}` : ''}`;
    if (cand) sel.querySelector('.m').style.color = TIER_HEX[cand.tier];
    const a = sel.querySelector('.a');
    if (n.is_dir) mk('Open', '', () => enter(selected), a);
    mk('Finder', 'quiet', finder(n), a); mk('✦ Ru', 'ru', () => ruOpen('selection'), a); mk('×', 'quiet', () => setSelected(null), a);
    dragSource(sel, n.path);
    el.appendChild(sel);
  }
  // what is inside: folders by size, click to go down
  const dirs = current.children.filter((c) => c.path && c.is_dir && c.size > 0 && !hiddenPaths.has(c.path)), files = current.children.filter((c) => c.path && !c.is_dir && !hiddenPaths.has(c.path));
  const hiddenHere = current.children.filter((c) => c.path && hiddenPaths.has(c.path));
  const head = document.createElement('div'); head.className = 'kh'; head.textContent = dirs.length ? `Folders inside · ${dirs.length}` : 'No folders inside'; el.appendChild(head);
  const show = allKids ? dirs : dirs.slice(0, 8), max = dirs[0]?.size || 1;
  for (const c of show) {
    const row = document.createElement('div'); row.className = 'row kid'; row.tabIndex = 0; row.dataset.key = c.path;
    row.innerHTML = `<div><div class="name dir"></div><div class="sub"></div></div><div class="sz"></div><span class="go">›</span><div class="bar"><i></i></div>`;
    row.querySelector('.name').textContent = c.name; row.querySelector('.sub').textContent = `${fmtN(c.files)} files · ${fmtAge(ageDays(c))}${flaggedUnder(c.path) ? ` · ${fmt(flaggedUnder(c.path))} flagged` : ''}`;
    row.querySelector('.sz').textContent = fmt(c.size); row.querySelector('.bar i').style.width = `${Math.max(1, (c.size / max) * 100)}%`;
    row.onmouseenter = () => (rowHover = c.path); row.onmouseleave = () => (rowHover = null); dragSource(row, c.path);
    row.onclick = () => { const m = byKey.get(c.path); m ? enter(m) : navigate(c.path); }; row.onkeydown = (e) => { if (e.key === 'Enter') row.onclick(); };
    el.appendChild(row);
  }
  const foot = document.createElement('div'); foot.className = 'kf';
  if (dirs.length > 8) { const b = document.createElement('button'); b.textContent = allKids ? 'show fewer' : `show all ${dirs.length}`; b.onclick = () => { allKids = !allKids; renderFocus(); }; foot.appendChild(b); }
  if (files.length) foot.append(`${foot.children.length ? ' · ' : ''}${fmtN(files.length)} file${files.length === 1 ? '' : 's'} here · ${fmt(files.reduce((s, c) => s + c.size, 0))}`);
  if (hiddenHere.length) { const b = document.createElement('button'); b.textContent = `${foot.childNodes.length ? ' · ' : ''}${hiddenHere.length} hidden · show`; b.onclick = showHiddenList; foot.appendChild(b); }
  if (foot.childNodes.length) el.appendChild(foot);
  ruSync();
}

// ---------- remembered per scan root: hidden folders, tiers, colour, filter, ticks. Lives in ~/.dime/state.json ----------
let stateTimer = null, stateLoaded = false;
function saveState() {
  if (!stateLoaded) return;
  clearTimeout(stateTimer);
  stateTimer = setTimeout(() => api('/api/state', { root: rootPath || '/', state: {
    hidden: [...hiddenPaths.values()].map((n) => ({ path: n.path, name: n.name, size: n.size })), tiers: [...tiersOn], color: colorMode, filter, idleDays,
    picked: [...picked.values()].map((c) => ({ path: c.path, name: c.name, size: c.size, is_dir: c.is_dir, tier: c.tier, reason: c.reason, what: c.what, note: c.note, age_days: c.age_days })),
    // Ru's chat is deliberately not saved: a verdict is about the disk as it was, and stale advice reads as current
  } }).catch(() => {}), 400);
}
async function loadState() {
  stateLoaded = false;
  let st = null; try { st = await api(`/api/state?root=${encodeURIComponent(rootPath || '/')}`); } catch {}
  hiddenPaths.clear(); picked.clear(); ru.turns.length = 0; ruMsgs.innerHTML = ''; ruEl.querySelector('.clear').hidden = true;
  if (st) {
    for (const h of st.hidden ?? []) hiddenPaths.set(h.path, h);
    if (st.tiers?.length) { tiersOn.clear(); for (const t of st.tiers) tiersOn.add(t); }
    if (st.color) { colorMode = st.color; syncColorMode(); }
    if (st.filter !== undefined && !params.get('f')) { filter = st.filter; idleDays = st.idleDays ?? 0; if (filter === 'idle' && !idleDays) filter = ''; syncFilterButtons(); }
    for (const c of st.picked ?? []) picked.set(c.path, c);
  }
  stateLoaded = true;
}
// ---------- live updates: the server watches the scan root and bumps `version` on every change ----------
let version = null;
let lastRefresh = 0;
async function refresh({ force = false } = {}) {
  if (!force && (navBusy || performance.now() - lastInput < 3000)) return; // never yank the map out from under a click; the next tick retries
  lastRefresh = performance.now(); views.clear(); kidCache.clear();
  const keep = selected?.userData.entry.key, path = current.path;
  await Promise.all([loadDrive(), navigate(path, { quiet: true })]);
  if (keep && byKey.has(keep)) setSelected(byKey.get(keep));
}
setInterval(async () => {
  if (document.hidden || scanning || !current) return;
  try {
    const s = await api('/api/status');
    if (s.state !== 'done') return;
    if (version !== null && s.version !== version) { if (performance.now() - lastRefresh < 15000) return; await refresh(); if (navBusy || performance.now() - lastInput < 3000) return; } // a busy disk bumps the version every second; the map is redrawn at most every 15 s // leave `version` stale so we retry once things are quiet
    version = s.version;
  } catch {}
}, 2000);

// ---------- landing / scan ----------
function showLanding() {
  if (mode === 'mem') leaveHog();
  hog.returnTo = null;
  hideMenu(); setSelected(null); navHist.length = 0;
  setBlocks([]); current = null; $('#disk').hidden = true;
  $('#app').hidden = true;
  const l = $('#landing'); l.hidden = false;
  requestAnimationFrame(() => l.classList.remove('away'));
  if (!scanning) browse(rootPath || '/');
  history.replaceState(null, '', location.pathname);
  // main menu: just the two cards; Disk goes back to the map if one exists, else expands its setup below
  for (const x of $('#modes').children) x.setAttribute('aria-pressed', 'false');
  $('#disk-setup').hidden = true; $('#denied').hidden = true;
  $('.mode .alt').hidden = !scanDone;
  $('.mode.du b').textContent = scanDone ? `Back to ${rootName}` : 'Dig through the drive';
  if (!scanDone) api('/api/status').then((s) => renderSnaps(s.snapshots)).catch(() => {});
}
const ringSvg = (frac, i) => {
  const r = 20, c = 2 * Math.PI * r, hot = frac > 0.85;
  return `<svg viewBox="0 0 52 52" aria-hidden="true"><defs><linearGradient id="g${i}" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="${hot ? '#F5C26B' : '#4FD1C5'}"/><stop offset="1" stop-color="${hot ? '#FF7A3D' : '#8A5BC7'}"/></linearGradient></defs>
    <circle class="track" cx="26" cy="26" r="${r}" fill="none" stroke-width="5"/>
    <circle class="used" cx="26" cy="26" r="${r}" fill="none" stroke="url(#g${i})" stroke-width="5" stroke-dasharray="${c}" stroke-dashoffset="${c * (1 - frac)}"/>
    <text class="pct" x="26" y="30" text-anchor="middle">${Math.round(frac * 100)}%</text></svg>`;
};
async function loadDrives() {
  const list = await api('/api/drives');
  const el = $('#drives'); el.innerHTML = '';
  list.forEach((d, i) => {
    const used = d.total - d.available, b = document.createElement('button');
    b.type = 'button'; b.className = 'drive'; b.dataset.mount = d.mount; b.setAttribute('aria-pressed', 'false');
    b.innerHTML = `${ringSvg(used / d.total, i)}<div><div class="dn"></div><div class="du"></div></div>`;
    b.querySelector('.dn').textContent = d.name || d.mount;
    b.querySelector('.du').textContent = `${fmt(used)} used of ${fmt(d.total)}${d.removable ? ' · removable' : ''}`;
    b.onclick = () => browse(d.mount);
    el.appendChild(b);
  });
}
async function browse(path) {
  let r;
  try { r = await api(`/api/ls?path=${encodeURIComponent(path)}`); } catch (e) { $('#err').textContent = e.message; return; }
  $('#err').textContent = '';
  $('#path').value = r.path;
  for (const b of $('#drives').children) b.setAttribute('aria-pressed', String(r.path === b.dataset.mount || (b.dataset.mount !== '/' && r.path.startsWith(b.dataset.mount + '/')) || (b.dataset.mount === '/' && !r.path.startsWith('/Volumes/'))));
  const parts = r.path.split('/').filter(Boolean);
  const bc = $('#bcrumb'); bc.innerHTML = '';
  const add = (label, p) => { const b = document.createElement('button'); b.type = 'button'; b.textContent = label; b.onclick = () => browse(p); bc.appendChild(b); };
  add('/', '/');
  parts.forEach((p, i) => { const s = document.createElement('span'); s.className = 'sep'; s.textContent = '/'; bc.appendChild(s); add(p, '/' + parts.slice(0, i + 1).join('/')); });
  lastLs = r; renderDirs();
}
let lastLs = null, showHidden = false;
const FOLDER_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2h9A1.5 1.5 0 0 1 21 9.5v8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z"/></svg>';
function renderDirs() {
  const dirs = $('#dirs'); dirs.innerHTML = '';
  const r = lastLs, list = r.dirs.filter((d) => showHidden || !d.startsWith('.'));
  const hiddenCount = r.dirs.length - r.dirs.filter((d) => !d.startsWith('.')).length;
  $('#hidden-toggle').textContent = showHidden ? 'Hide hidden' : hiddenCount ? `Show ${hiddenCount} hidden` : '';
  $('#hidden-toggle').setAttribute('aria-pressed', String(showHidden));
  if (!list.length) { dirs.innerHTML = '<div class="empty">No subfolders here. Scan this one.</div>'; return; }
  for (const d of list) {
    const b = document.createElement('button'); b.type = 'button'; b.className = d.startsWith('.') ? 'dot' : '';
    b.innerHTML = `${FOLDER_SVG}<span></span><span class="go">Open</span>`;
    b.querySelector('span').textContent = d;
    b.onclick = () => browse(r.path + (r.path.endsWith('/') ? '' : '/') + d);
    dirs.appendChild(b);
  }
}
$('#hidden-toggle').onclick = () => { showHidden = !showHidden; renderDirs(); };
$('#path').addEventListener('change', () => browse($('#path').value));
loadDrives();
let homePath = '';
api('/api/home').then((h) => { homePath = h.path.replace(/\/$/, ''); browse('/'); }); // Di starts at the root of the drive
const tilde = (p) => homePath && (p === homePath || p.startsWith(homePath + '/')) ? '~' + p.slice(homePath.length) : p;
function beginScanUi(root) {
  if (mode === 'mem') leaveHog();
  rootName = root.split('/').filter(Boolean).pop() || '/'; rootPath = root.replace(/\/$/, '');
  scanning = true; document.body.dataset.scanning = ''; started = true; clearTimeout(stateTimer); stateLoaded = false; hiddenPaths.clear(); picked.clear(); openRows.clear(); kidCache.clear(); navHist.length = 0; rotHold = false; flyHome = false; camGoal = null;
  for (const m of [...byKey.values()]) dispose(m); // a rescan starts from a bare stage; the stacks rise fresh rather than the old map morphing in place
  setSelected(null); hideMenu(); $('#reveal').classList.remove('on');
  camera.position.set(-60, 95, 170); controls.target.set(14, 6, 0);
  controls.autoRotate = !REDUCED; controls.autoRotateSpeed = 1.1; // slow lap around the map while the search party works
  $('#landing').classList.add('away'); setTimeout(() => ($('#landing').hidden = true), 600);
  $('#app').hidden = false; $('#panel').classList.add('away');
  current = { path: '' }; renderCrumbs();
  $('#hint').textContent = 'Di: search party out. My choppers hover over every folder still being counted and drop in what they find. Space holds the camera.';
}
async function watchScan() {
  for (;;) {
    const s = await api('/api/status');
    if (s.state === 'done') return s;
    if (s.state !== 'scanning') throw new Error('scan stopped');
    $('#stats').className = 'live'; $('#stats').textContent = `Scanning · ${fmtN(s.files)} files · ${fmt(s.size)}`;
    if (mode === 'disk') { setBlocks(entriesFor(s.live, { live: true })); crewAssign(s.live); }
    await sleep(120);
  }
}
async function finishScan() {
  scanning = false; delete document.body.dataset.scanning; scanDone = true; version = null; views.clear(); mapAsOf = null; deniedShown = false;
  await loadState();
  if (mode !== 'disk') { rootSize = (await api('/api/status')).size || 1; await loadDrive(); return; } // finished while Me was up; the map waits until you come back $('#hint').textContent = 'Click a folder to open it. Right-click for more. Esc goes back.';
  const st = await api('/api/status');
  rootSize = st.size || 1;
  denyBanner(st.denied, st.fda);
  await loadDrive();
  await navigate('', pendingHighlight ? { highlight: pendingHighlight } : {}); // same keys as the stacks: they slide into their treemap places
  pendingHighlight = null;
  reveal(st);
  $('#panel').classList.remove('away');
}
/** The moment the scan lands: headline counts up while the camera takes one quick turn around the rising map, then stops. */
function reveal(st) {
  const el = $('#reveal'), s = summaryData;
  const easy = s ? s.tiers.filter((t) => t[0] !== 'review').reduce((a, t) => a + t[1], 0) : 0;
  el.classList.add('on');
  const t0 = performance.now(), dur = REDUCED ? 0 : 1400;
  const tick = (now) => {
    const k = dur ? Math.min(1, (now - t0) / dur) : 1, e = 1 - Math.pow(1 - k, 3);
    el.querySelector('.big').innerHTML = `${fmt(st.size * e)} <span>·</span> ${fmtN(Math.round(st.files * e))} files`;
    el.querySelector('.sub').innerHTML = easy ? `Di found <b>${fmt(easy * e)}</b> safe to free` : `Di mapped ${rootName}`;
    if (k < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  camGoal = null;
  if (!REDUCED) { spinUntil = performance.now() + 2600; controls.autoRotateSpeed = 6; setTimeout(() => { flyHome = true; }, 2600); } else { spinUntil = 0; flyHome = true; }
  setTimeout(() => el.classList.remove('on'), 3400);
}
async function startScan(path) {
  if (scanning) return;
  $('#err').textContent = ''; $('#scanbtn').disabled = true;
  hideMenu(); tip.hidden = true;
  try {
    await api('/api/scan', { path });
    beginScanUi((await api('/api/status')).root);
    await watchScan();
    await finishScan();
  } catch (err) {
    $('#err').textContent = err.message; $('#landing').hidden = false; $('#landing').classList.remove('away'); scanning = false; delete document.body.dataset.scanning;
  } finally { $('#scanbtn').disabled = false; }
}
$('#scanform').onsubmit = (e) => { e.preventDefault(); startScan($('#path').value); };
// page reload: pick up a scan already running or finished on the server
let mapAsOf = null; // epoch seconds when the map came from a snapshot, null after a fresh scan
// macOS itself refused some folders, so the map is smaller than the disk. Say so once per scan, but only
// when Full Disk Access would actually help: without that grant, nothing DiMe can be given opens them.
// Ordinary Unix permission denials are not counted either, for the same reason.
let deniedShown = false;
function denyBanner(n, fda) {
  const el = $('#denied');
  if (!n || fda || deniedShown) { el.hidden = true; return; }
  el.innerHTML = `<span><b>macOS held back ${fmtN(n)} folder${n === 1 ? '' : 's'}.</b> That much of this drive is missing from the map.</span><button type="button">Give DiMe access</button><button class="x" type="button" title="Dismiss">Dismiss</button>`;
  el.querySelector('button').onclick = () => { api('/api/fda', {}).catch((e) => toast(e.message)); toast('Di: add DiMe to the list, then start it again', 'du'); };
  el.querySelector('.x').onclick = () => { deniedShown = true; el.hidden = true; };
  el.hidden = false;
}
const ago = (t) => { const s = Date.now() / 1000 - t; return s < 90 ? 'just now' : s < 5400 ? `${Math.round(s / 60)} min ago` : s < 129600 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} d ago`; };
/** The page picked up a map the server already holds: a scan in progress, a finished one, or one just resumed from a snapshot. */
async function enterMap(s) {
  beginScanUi(s.root);
  if (s.state === 'scanning') await watchScan();
  scanning = false; delete document.body.dataset.scanning; scanDone = true; flyHome = false; $('#hint').textContent = 'Click a folder to open it. Right-click for more. Esc goes back.';
  const st = await api('/api/status'); rootSize = st.size || 1; mapAsOf = st.as_of ?? null; denyBanner(st.denied, st.fda);
  await loadDrive(); await loadState();
  const sel = new URLSearchParams(location.search).get('sel'); // deep link: ?sel=<rel path> highlights an item
  await navigate(location.hash.slice(1) || '', sel ? { highlight: sel } : {});
  $('#panel').classList.remove('away');
}
async function resumeMap(root) {
  hideMenu(); $('#err').textContent = '';
  try { await api('/api/resume', { path: root }); await enterMap(await api('/api/status')); } catch (e) { $('#err').textContent = e.message; }
}
function renderSnaps(list) {
  const el = $('#snaps'); el.innerHTML = ''; $('#last').hidden = !list?.length;
  for (const m of list ?? []) {
    const b = document.createElement('button'); b.type = 'button'; b.className = 'snap';
    b.innerHTML = `<div><div class="n"></div><div class="m"></div></div><span class="go">Open</span>`;
    b.querySelector('.n').textContent = tilde(m.root); b.querySelector('.m').textContent = `${fmt(m.size)} · ${fmtN(m.files)} files · mapped ${ago(m.at)}`;
    b.title = 'Open the last map at once; Rescan later for a fresh one'; b.onclick = () => resumeMap(m.root);
    el.appendChild(b);
  }
}
api('/api/status').then(async (s) => {
  renderSnaps(s.state === 'idle' ? s.snapshots : []);
  applyRu(s.ru);
  if (s.state === 'idle' || location.hash === '#memory') {
    if (s.state === 'done') { scanDone = true; rootPath = s.root.replace(/\/$/, ''); rootName = rootPath.split('/').filter(Boolean).pop() || '/'; rootSize = s.size || 1; mapAsOf = s.as_of ?? null; await loadDrive(); await loadState(); }
    return;
  }
  await enterMap(s);
});

// ---------- loop ----------
let last = performance.now(), spinUntil = 0;
renderer.setAnimationLoop((now) => {
  // the camera only turns on its own while Di is scanning, or for the short reveal spin afterwards
  if (mode === 'disk' && started) controls.autoRotate = !REDUCED && ((scanning && !rotHold) || now < spinUntil);
  const dt = Math.min(0.05, (now - last) / 1000); last = now;
  if (flyHome) { camera.position.lerp(HOME_CAM, REDUCED ? 1 : 1 - Math.exp(-dt * 3)); if (camera.position.distanceTo(HOME_CAM) < 0.5) flyHome = false; }
  updateCamera(dt, now);
  controls.update();
  updateIdle(dt, now);
  updateCrew(dt, now); updateSites(dt, now); updateEscort(dt, now); updateHomeFlyers(dt, now);
  // Ru drifts on the landing, and comes out over the map whenever its panel is open
  ruShow((!$('#landing').hidden && !$('#landing').classList.contains('away')) || (mode === 'disk' && (ru.open || scanning)));
  updateRuBody(dt, now); updateMarks(dt); updateParcels(dt);
  if (mode === 'mem' && memView !== 'orbit') return; // list view is plain HTML; give the GPU a rest
  updateWorld(dt, now);
  updateBlocks(dt, now); updateRipples(dt, now); updateSparks(dt); updateDust(dt, now);
  if (mode === 'mem') { updateOrbit(dt, now); updateShip(dt, now); updateOrbitHover(); } else if (current) updateHover();
  composer.render();
  labelR.render(scene, camera);
});

// ======================= Memory: live dashboard =======================
// Ranked lanes per resource with 60 s sparklines, an odd-behaviour detector, and a drawer per process
// with its open files. Plain HTML: crisp numbers beat a 3D metaphor for rates.
let mode = 'disk';
function setMode(m) { mode = m; document.body.dataset.mode = m; }
const hog = { snap: null, sel: null, timer: null, files: null, filesPid: null, hist: new Map(), since: new Map() };
const N_HIST = 30; // samples kept (2 s each)
const fmtRate = (b) => b < 1024 ? '' : fmt(b) + '/s';
const fmtUp = (s) => s < 3600 ? `${Math.floor(s / 60)}m` : s < 86400 ? `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m` : `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
const family = (p) => p.name.replace(/ Helper.*$/, '').replace(/ \(.*\)$/, '').replace(/^com\.apple\./, '');
const procById = (pid) => hog.snap?.procs.find((p) => p.pid === pid);
const folderOf = (p) => p.slice(0, p.lastIndexOf('/')) || '/';
const gpuVal = (p) => p.gpu_pct != null ? p.gpu_pct : p.gpu_queues * 2 + p.gpu_clients;
const gpuText = (p) => p.gpu_pct != null && p.gpu_pct >= 0.5 ? `${p.gpu_pct.toFixed(0)}%` : gpuVal(p) ? `${p.gpu_queues} queue${p.gpu_queues === 1 ? '' : 's'}, idle` : '';
const netVal = (p) => p.net_in + p.net_out, diskVal = (p) => p.read_rate + p.write_rate;
const memFilter = { q: (new URLSearchParams(location.search).get('q') || '').toLowerCase(), kind: new URLSearchParams(location.search).get('k') || 'all', not: new URLSearchParams(location.search).get('x') === '1' };
if (!['all', 'busy', 'odd', 'mine', 'system'].includes(memFilter.kind)) memFilter.kind = 'all';
$('#memq').value = memFilter.q; for (const x of $('#memfilter').querySelectorAll('button')) x.setAttribute('aria-pressed', String(x.dataset.k === memFilter.kind));
$('#memx').checked = memFilter.not; $('#memfilter').classList.toggle('not', memFilter.not);
$('#memx').onchange = (e) => { memFilter.not = e.target.checked; $('#memfilter').classList.toggle('not', memFilter.not); refilter(); };
const isSystem = (p) => p.user !== (homePath.split('/').pop() || p.user) || /^\/(System|usr\/libexec|usr\/sbin|sbin)\//.test(p.exe);
const terms = () => memFilter.q.split(',').map((t) => t.trim()).filter(Boolean); // "chrome, spotify" = either
function matchesFilter(p, s) {
  const ts = terms();
  if (ts.length) { const hay = `${p.name} ${family(p)} ${p.pid}`.toLowerCase(); if (!ts.some((t) => hay.includes(t))) return false; }
  switch (memFilter.kind) {
    case 'busy': return p.cpu >= 2 || (p.gpu_pct ?? 0) >= 2 || netVal(p) >= 10240 || diskVal(p) >= 10240;
    case 'odd': return (s?.oddPids ?? new Set()).has(p.pid);
    case 'mine': return !isSystem(p);
    case 'system': return isSystem(p);
    default: return true;
  }
}
// "exclude" flips the whole selection: show everything that does NOT match. Nothing selected + exclude = everything.
function passes(p, s) { const active = memFilter.q || memFilter.kind !== 'all'; return memFilter.not && active ? !matchesFilter(p, s) : matchesFilter(p, s); }
function filtered(s) { s.oddPids = new Set(findOdd(s).map((o) => o.p.pid)); return s.procs.filter((p) => passes(p, s)); }
$('#memq').oninput = (e) => { memFilter.q = e.target.value.trim().toLowerCase(); refilter(); };
$('#memfilter').onclick = (e) => { const b = e.target.closest('button[data-k]'); if (!b) return; memFilter.kind = b.dataset.k; for (const x of $('#memfilter').querySelectorAll('button')) x.setAttribute('aria-pressed', String(x === b)); refilter(); };
function refilter() { if (!hog.snap) return; renderDash(); orbitSync(); }
const LANES = [
  { id: 'cpu', label: 'CPU', val: (p) => p.cpu, text: (p) => `${p.cpu.toFixed(0)}%`, color: '#FF7A3D', note: (s) => `${s.cpu.toFixed(0)}% of ${s.cpus} cores` },
  { id: 'gpu', label: 'GPU', val: (p) => p.gpu_pct ?? 0, text: gpuText, color: '#8A5BC7', note: () => 'busy % per process' },
  { id: 'net', label: 'Network', val: netVal, text: (p) => fmtRate(netVal(p)), color: '#4FD1C5', note: () => 'in + out' },
  { id: 'disk', label: 'Disk IO', val: diskVal, text: (p) => fmtRate(diskVal(p)), color: '#F5C26B', note: () => 'read + write' },
];

function recordHistory(s) {
  const seen = new Set();
  for (const p of s.procs) {
    seen.add(p.pid);
    const h = hog.hist.get(p.pid) ?? hog.hist.set(p.pid, { cpu: [], gpu: [], net: [], disk: [], rss: [], nin: [], nout: [], rd: [], wr: [] }).get(p.pid);
    const push = (k, v) => { h[k].push(v); if (h[k].length > N_HIST) h[k].shift(); };
    push('cpu', p.cpu); push('gpu', p.gpu_pct ?? 0); push('net', netVal(p)); push('disk', diskVal(p)); push('rss', p.rss); push('nin', p.net_in); push('nout', p.net_out); push('rd', p.read_rate); push('wr', p.write_rate);
  }
  for (const k of hog.hist.keys()) if (!seen.has(k)) hog.hist.delete(k);
}
const hist = (pid, k) => hog.hist.get(pid)?.[k] ?? [];
const mean = (a) => a.length ? a.reduce((s, v) => s + v, 0) / a.length : 0;
const secs = (n) => `${n * 2}s`;

// ---- odd behaviour: each rule returns a reason string or null
const RULES = [
  { id: 'runaway', sev: 'hot', test: (p, h) => { const k = h.cpu.slice(-8); return k.length >= 5 && k.every((v) => v > 90) ? `Pinning the CPU: ${p.cpu.toFixed(0)}% for ${secs(k.length)}+` : null; } },
  { id: 'busy', sev: 'warm', test: (p, h) => { const k = h.cpu.slice(-15); return k.length >= 10 && mean(k) > 40 && !(k.slice(-8).every((v) => v > 90)) ? `Busy: averaging ${mean(k).toFixed(0)}% CPU over ${secs(k.length)}` : null; } },
  { id: 'bursty-cpu', sev: 'warm', test: (p, h) => { const k = h.cpu; if (k.length < 10) return null; const m = mean(k), mx = Math.max(...k); const spikes = k.filter((v) => v > 50 && v > 4 * m).length; return spikes >= 2 && mx > 60 ? `Bursty CPU: ${spikes} spikes to ${mx.toFixed(0)}% in ${secs(k.length)}, ${m.toFixed(0)}% between` : null; } },
  { id: 'gpu', sev: 'warm', test: (p, h) => { const k = h.gpu.slice(-8); return k.length >= 5 && mean(k) > 50 ? `Leaning on the GPU: ${mean(k).toFixed(0)}% busy for ${secs(k.length)}+` : null; } },
  { id: 'bandwidth', sev: 'hot', test: (p, h) => { const k = h.net.slice(-5); return k.length >= 3 && mean(k) > 5 << 20 ? `Heavy network: ${fmtRate(mean(k))} sustained` : null; } },
  { id: 'bursty-net', sev: 'warm', test: (p, h) => { const k = h.net; if (k.length < 10) return null; const m = mean(k), mx = Math.max(...k); const spikes = k.filter((v) => v > 1 << 20 && v > 4 * m).length; return spikes >= 2 ? `Bursty network: ${spikes} bursts up to ${fmtRate(mx)}, quiet between` : null; } },
  { id: 'uploading', sev: 'warm', test: (p, h) => { const o = h.nout.slice(-10), i = h.nin.slice(-10); return o.length >= 5 && mean(o) > 512 << 10 && mean(o) > 3 * mean(i) ? `Uploading: ${fmtRate(mean(o))} out, ${fmtRate(mean(i)) || 'little'} in` : null; } },
  { id: 'growing', sev: 'warm', test: (p, h) => { const r = h.rss; if (r.length < 10) return null; const d = r[r.length - 1] - r[0]; return d > 200 << 20 && d > r[0] * 0.15 ? `Memory growing: +${fmt(d)} in ${secs(r.length)} (now ${fmt(p.rss)})` : null; } },
  { id: 'thrash', sev: 'warm', test: (p, h) => { const k = h.disk.slice(-5); return k.length >= 3 && mean(k) > 30 << 20 ? `Hammering the disk: ${fmtRate(mean(h.rd.slice(-5)))} read, ${fmtRate(mean(h.wr.slice(-5))) || '0'} write` : null; } },
  { id: 'idle-hog', sev: 'cool', test: (p, h) => { const k = h.cpu; return p.rss > 1 << 30 && k.length >= 15 && k.every((v) => v < 1) && netVal(p) < 1024 ? `Large and idle: ${fmt(p.rss)} held, no CPU or network for ${secs(k.length)}` : null; } },
];
const SEV_COLOR = { hot: '#FF7A3D', warm: '#F5C26B', cool: '#8A5BC7' };
function findOdd(s) {
  const out = [], now = Date.now();
  for (const p of s.procs) {
    const h = hog.hist.get(p.pid); if (!h) continue;
    for (const r of RULES) {
      const why = r.test(p, h); const key = `${p.pid}:${r.id}`;
      if (why) { if (!hog.since.has(key)) hog.since.set(key, now); out.push({ p, rule: r, why, since: hog.since.get(key) }); }
      else hog.since.delete(key);
    }
  }
  const rank = { hot: 0, warm: 1, cool: 2 };
  return out.sort((a, b) => rank[a.rule.sev] - rank[b.rule.sev] || a.since - b.since).slice(0, 8);
}

// ---- sparkline svg (fills to the right edge with the newest sample)
function spark(vals, w = 64, h = 22) {
  if (!vals.length) return '';
  const max = Math.max(1e-9, ...vals), n = N_HIST;
  const pts = vals.map((v, i) => `${((n - vals.length + i) / (n - 1)) * w},${h - 1 - (v / max) * (h - 3)}`);
  const first = pts[0].split(',')[0], last = pts[pts.length - 1].split(',')[0];
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><polygon points="${first},${h} ${pts.join(' ')} ${last},${h}"/><polyline points="${pts.join(' ')}"/></svg>`;
}

// ---- dashboard
function renderDash() {
  const s = hog.snap; if (!s || mode !== 'mem') return;
  const tot = $('#totals'); tot.innerHTML = '';
  const used = 1 - s.available / s.total;
  const netIn = s.procs.reduce((a, p) => a + p.net_in, 0), netOut = s.procs.reduce((a, p) => a + p.net_out, 0);
  const rd = s.procs.reduce((a, p) => a + p.read_rate, 0), wr = s.procs.reduce((a, p) => a + p.write_rate, 0);
  const tile = (l, v, small, w, sub, hot) => { const d = document.createElement('div'); d.className = 'tot'; d.innerHTML = `<div class="l"></div><div class="v"><small></small></div><div class="bar${hot ? ' hot' : ''}"><i></i></div><div class="sub"></div>`; d.querySelector('.l').textContent = l; d.querySelector('.v').prepend(v); d.querySelector('small').textContent = small; d.querySelector('.bar i').style.setProperty('--w', `${Math.min(100, w)}%`); d.querySelector('.sub').append(sub); tot.appendChild(d); return d; };
  tile('Memory', fmt(s.used), `of ${fmt(s.total)}`, used * 100, `${fmt(s.available)} free`, used > 0.9);
  tile('CPU', `${s.cpu.toFixed(0)}%`, `${s.cpus} cores`, s.cpu, `load ${s.load.toFixed(1)}`, s.cpu > 85);
  const gpuTop = [...s.procs].sort((a, b) => (b.gpu_pct ?? 0) - (a.gpu_pct ?? 0))[0];
  tile('GPU', s.gpu == null ? '—' : `${s.gpu}%`, 'busy', s.gpu ?? 0, gpuTop && gpuTop.gpu_pct >= 0.5 ? `${gpuTop.name} ${gpuTop.gpu_pct.toFixed(0)}%` : 'no process busy on it', s.gpu > 85);
  tile('Network', fmtRate(netIn + netOut) || '0', '', Math.min(100, (netIn + netOut) / (10 << 20) * 100), `↓ ${fmtRate(netIn) || '0'} · ↑ ${fmtRate(netOut) || '0'}`, netIn + netOut > 20 << 20);
  tile('Disk IO', fmtRate(rd + wr) || '0', '', Math.min(100, (rd + wr) / (200 << 20) * 100), `read ${fmtRate(rd) || '0'} · write ${fmtRate(wr) || '0'}`, rd + wr > 100 << 20);
  $('#stats').textContent = '';

  const shown = filtered(s);
  $('#memfilter .n').textContent = shown.length === s.procs.length ? `${s.procs.length} processes` : `${shown.length} of ${s.procs.length}`;
  const odd = $('#odd'); odd.innerHTML = '';
  const items = findOdd(s).filter((o) => passes(o.p, s));
  const samples = Math.max(0, ...s.procs.map((p) => hist(p.pid, 'cpu').length));
  if (!items.length) odd.innerHTML = `<div class="none">${samples < 8 ? `Watching for odd behaviour… ${secs(samples)} of history so far.` : 'Nothing odd in the last minute: no runaway CPU, bursts, uploads, memory growth or disk hammering.'}</div>`;
  for (const it of items) {
    const d = document.createElement('div'); d.className = 'odd'; d.style.setProperty('--c', SEV_COLOR[it.rule.sev]);
    d.innerHTML = `<i></i><div><b></b> <span class="why"></span></div><div class="t"></div>`;
    d.querySelector('b').textContent = it.p.name; d.querySelector('.why').textContent = it.why;
    const ago = Math.round((Date.now() - it.since) / 1000); d.querySelector('.t').textContent = ago < 4 ? 'just now' : `for ${ago < 90 ? ago + 's' : Math.round(ago / 60) + 'm'}`;
    d.onclick = () => openDrawer(it.p.pid);
    odd.appendChild(d);
  }

  const lanes = $('#lanes'); lanes.innerHTML = '';
  for (const L of LANES) {
    const lane = document.createElement('div'); lane.className = 'lane'; lane.style.setProperty('--c', L.color);
    lane.innerHTML = `<h3></h3>`; lane.querySelector('h3').innerHTML = `${L.label}<span></span>`; lane.querySelector('h3 span').textContent = L.note(s);
    const top = shown.filter((p) => L.val(p) > 0).sort((a, b) => L.val(b) - L.val(a)).slice(0, 8);
    const max = top[0] ? L.val(top[0]) : 1;
    if (!top.length) lane.insertAdjacentHTML('beforeend', `<div class="empty">Quiet right now.</div>`);
    for (const p of top) {
      const row = document.createElement('div'); row.className = 'lrow' + (p.pid === hog.sel ? ' on' : ''); row.dataset.pid = p.pid;
      row.innerHTML = `<div><div class="n"><span></span><small></small></div><div class="b"><i></i></div></div>${spark(hist(p.pid, L.id))}<div class="v"></div>`;
      row.querySelector('.n span').textContent = p.name; row.querySelector('.n small').textContent = family(p) !== p.name ? family(p) : `pid ${p.pid}`;
      row.querySelector('.b i').style.setProperty('--w', `${Math.max(2, (L.val(p) / max) * 100)}%`);
      row.querySelector('.v').textContent = L.text(p);
      row.title = `${p.name} · pid ${p.pid} · ${fmt(p.rss)}`;
      row.onclick = () => openDrawer(p.pid);
      lane.appendChild(row);
    }
    lanes.appendChild(lane);
  }

  const ml = $('#memlane'); ml.innerHTML = `<h3>Memory <span style="color:var(--ink-3);font-weight:400;font-size:11px">top 12 of ${s.procs.length}</span></h3><div class="grid"></div>`;
  const grid = ml.querySelector('.grid'), top = [...shown].sort((a, b) => b.rss - a.rss).slice(0, 12), max = top[0]?.rss || 1;
  for (const p of top) {
    const row = document.createElement('div'); row.className = 'lrow' + (p.pid === hog.sel ? ' on' : ''); row.style.setProperty('--c', '#3B6FB6');
    row.innerHTML = `<div><div class="n"><span></span><small></small></div><div class="b"><i></i></div></div>${spark(hist(p.pid, 'rss'))}<div class="v"></div>`;
    row.querySelector('.n span').textContent = p.name; row.querySelector('.n small').textContent = family(p) !== p.name ? family(p) : `pid ${p.pid}`;
    row.querySelector('.b i').style.setProperty('--w', `${Math.max(2, (p.rss / max) * 100)}%`);
    row.querySelector('.v').textContent = fmt(p.rss);
    row.onclick = () => openDrawer(p.pid);
    grid.appendChild(row);
  }
  if (hog.sel) renderDrawer();
}

// ---- drawer
async function openDrawer(pid) {
  hog.sel = pid;
  $('#dash').classList.add('narrow'); $('#drawer').hidden = false;
  renderDash();
  if (hog.filesPid !== pid) {
    hog.files = null; hog.filesPid = null; renderDrawer();
    try { const f = await api(`/api/procfiles?pid=${pid}`); if (hog.sel === pid) { hog.files = f; hog.filesPid = pid; } } catch { if (hog.sel === pid) { hog.files = []; hog.filesPid = pid; } }
    renderDrawer();
  }
}
function closeDrawer() { hog.sel = null; $('#dash').classList.remove('narrow'); $('#drawer').hidden = true; renderDash(); }
// the orbit view keeps the drawer too; renderDash is cheap even while hidden
function renderDrawer() {
  const p = procById(hog.sel), d = $('#drawer');
  if (!p) { d.innerHTML = `<button class="btn sm quiet close" id="dclose">✕</button><div class="name">Gone</div><div class="meta">That process has exited.</div>`; $('#dclose').onclick = closeDrawer; return; }
  const h = hog.hist.get(p.pid) ?? {};
  d.innerHTML = `<button class="btn sm quiet close" id="dclose" aria-label="Close">✕</button><div class="name"></div><div class="meta"></div><div id="dodd"></div><div class="sparks"></div><div class="acts"></div><div class="files"></div>`;
  $('#dclose').onclick = closeDrawer;
  d.querySelector('.name').textContent = p.name;
  d.querySelector('.meta').textContent = `pid ${p.pid} · ${p.user} · up ${fmtUp(p.run_time)}${family(p) !== p.name ? ` · part of ${family(p)}` : ''}${p.exe ? ` · ${tilde(p.exe)}` : ''}`;
  const oddHere = findOdd(hog.snap).filter((o) => o.p.pid === p.pid);
  for (const it of oddHere) { const e = document.createElement('div'); e.className = 'odd'; e.style.setProperty('--c', SEV_COLOR[it.rule.sev]); e.innerHTML = `<i></i><div class="why"></div><div></div>`; e.querySelector('.why').textContent = it.why; d.querySelector('#dodd').appendChild(e); }
  const sp = d.querySelector('.sparks');
  const card = (l, v, vals, c) => { const e = document.createElement('div'); e.className = 'spark'; e.style.setProperty('--c', c); e.innerHTML = `<div class="l"><span></span><b></b></div>${spark(vals, 100, 34)}`; e.querySelector('span').textContent = l; e.querySelector('b').textContent = v; sp.appendChild(e); };
  card('CPU', `${p.cpu.toFixed(0)}%`, h.cpu ?? [], '#FF7A3D');
  card('Memory', fmt(p.rss), h.rss ?? [], '#3B6FB6');
  card('GPU', p.gpu_pct != null ? `${p.gpu_pct.toFixed(0)}%${p.gpu_clients ? ` · ${p.gpu_queues} queue${p.gpu_queues === 1 ? '' : 's'}` : ''}` : '0', h.gpu ?? [], '#8A5BC7');
  card('Network', `↓ ${fmtRate(p.net_in) || '0'} ↑ ${fmtRate(p.net_out) || '0'}`, h.net ?? [], '#4FD1C5');
  card('Disk read', fmtRate(p.read_rate) || '0', h.rd ?? [], '#F5C26B');
  card('Disk write', fmtRate(p.write_rate) || '0', h.wr ?? [], '#F5C26B');
  const acts = d.querySelector('.acts');
  const mk = (label, cls, fn) => { const b = document.createElement('button'); b.className = `btn sm ${cls}`; b.textContent = label; b.onclick = fn; acts.appendChild(b); };
  if (p.exe) mk('Reveal app', 'quiet', () => api('/api/reveal', { path: p.exe }).catch((e) => toast(e.message)));
  const pf = d.querySelector('.files');
  if (hog.filesPid !== p.pid) pf.innerHTML = '<h4>Open files<span>looking…</span></h4>';
  else if (!hog.files.length) pf.innerHTML = '<h4>Open files<span>none visible</span></h4><div class="empty">No regular files open, or the process belongs to another user.</div>';
  else {
    const groups = new Map();
    for (const f of hog.files) { const dir = folderOf(f.path); (groups.get(dir) ?? groups.set(dir, []).get(dir)).push(f); }
    const writing = hog.files.filter((f) => f.written_ago != null && f.written_ago < 120).length;
    pf.innerHTML = `<h4>Open files<span>${hog.files.length} in ${groups.size} folders${writing ? ` · ${writing} written in the last 2 min` : ''} · click to see on the disk map</span></h4>`;
    const recency = (files) => Math.min(...files.map((f) => f.written_ago ?? 1e9));
    for (const [dir, files] of [...groups].sort((a, b) => recency(a[1]) - recency(b[1]) || b[1].length - a[1].length).slice(0, 40)) {
      const g = document.createElement('div'); g.className = 'fgrp';
      const hb = document.createElement('button'); hb.innerHTML = `${FOLDER_SVG}<b></b><span></span>`; hb.querySelector('b').textContent = tilde(dir); hb.querySelector('span').textContent = `${files.length} · ${fmt(files.reduce((s, f) => s + f.size, 0))}`;
      hb.title = dir; hb.onclick = () => openInGunk(dir, true); g.appendChild(hb);
      for (const f of files.sort((a, b) => (a.written_ago ?? 1e9) - (b.written_ago ?? 1e9) || b.size - a.size).slice(0, 12)) {
        const fb = document.createElement('button'); fb.className = 'f';
        const live = f.written_ago != null && f.written_ago < 120;
        fb.innerHTML = `<span></span><span class="w${live ? ' live' : ''}"></span>`;
        fb.firstChild.textContent = `${f.path.slice(dir.length + 1)}${f.size ? ' · ' + fmt(f.size) : ''}`;
        fb.lastChild.textContent = f.written_ago == null ? '' : f.written_ago < 5 ? 'writing now' : f.written_ago < 120 ? `written ${f.written_ago}s ago` : f.written_ago < 3600 ? `${Math.round(f.written_ago / 60)}m ago` : '';
        fb.title = f.path; fb.onclick = () => openInGunk(f.path, false); g.appendChild(fb);
      }
      pf.appendChild(g);
    }
  }
}

// ---- mode switching
function enterHog() {
  hideMenu(); tip.hidden = true;
  setMode('mem'); started = true; controls.autoRotate = false; flyHome = false; camGoal = null;
  $('#landing').classList.add('away'); setTimeout(() => ($('#landing').hidden = true), 600);
  $('#app').hidden = false; $('#dash').hidden = false;
  setBlocks([]); current = null; hovered = null; selected = null;
  setMemView(memView);
  hogPoll();
  clearInterval(hog.timer); hog.timer = setInterval(hogPoll, 2000);
  history.replaceState(null, '', location.pathname + location.search + '#memory');
}
async function hogPoll() {
  if (document.hidden || mode !== 'mem') return;
  try { hog.snap = await api('/api/procs'); } catch { return; }
  recordHistory(hog.snap);
  renderDash(); orbitSync();
  const want = hog.pendingSel || +new URLSearchParams(location.search).get('pid'); // return trip, or deep link ?pid=123#memory
  if (want && hog.sel !== want) { if (procById(want)) openDrawer(want); else if (hog.pendingSel) toast('Me: that process has exited', 'me'); }
  hog.pendingSel = null;
}
function leaveHog() {
  clearInterval(hog.timer); hog.timer = null; camGoal = null;
  hog.sel = null; $('#drawer').hidden = true; $('#dash').hidden = true; $('#dash').classList.remove('narrow');
  orbit.on = false; orbit.group.visible = false; orbit.hover = null; tip.hidden = true;
  controls.target.copy(HOME_TARGET); camera.position.copy(HOME_CAM);
  setMode('disk'); document.body.dataset.view = '';
}
function renderMemCrumbs() {
  const el = $('#crumbs'); el.innerHTML = '';
  const home = document.createElement('button'); home.className = 'home'; home.title = 'Home'; home.setAttribute('aria-label', 'Home');
  home.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 11 12 4l8.5 7M5.5 9.5V20h13V9.5"/></svg>';
  home.onclick = showLanding; el.appendChild(home); el.appendChild(whoBadge('me'));
  const b = document.createElement('button'); b.className = 'cur'; b.textContent = 'Memory'; el.appendChild(b);
  const seg = document.createElement('div'); seg.className = 'seg';
  for (const [v, l] of [['list', 'List'], ['orbit', 'Orbit']]) { const x = document.createElement('button'); x.textContent = l; x.setAttribute('aria-pressed', String(memView === v)); x.onclick = () => setMemView(v); seg.appendChild(x); }
  el.appendChild(seg);
  const sub = document.createElement('span'); sub.style.cssText = 'align-self:center;color:var(--ink-2);margin-left:10px'; sub.textContent = 'live · updates every 2 s'; el.appendChild(sub);
}
/** Jump from a file a process has open to that place on the disk map. Scans the folder if it is outside the current scan. */
let pendingHighlight = null;
async function openInGunk(abs, isDir) {
  const dir = isDir ? abs : folderOf(abs);
  const name = isDir ? '' : abs.slice(abs.lastIndexOf('/') + 1);
  const inside = scanDone && (abs === rootPath || abs.startsWith(rootPath + '/'));
  hog.returnTo = hog.sel ? { pid: hog.sel, name: procById(hog.sel)?.name ?? `pid ${hog.sel}` } : null;
  leaveHog();
  if (inside) {
    const rel = abs.slice(rootPath.length + 1);
    $('#app').hidden = false; $('#panel').classList.remove('away'); showTab('browse');
    await navigate(isDir ? rel : parentOf(rel), isDir ? {} : { highlight: rel });
    toast(`Di: here it is · ${isDir ? rel || rootName : rel}`, 'du');
  } else {
    pendingHighlight = name || null;
    toast(`Me: handing you to Di, who is scanning ${tilde(dir)}`, 'me');
    await startScan(dir);
  }
}
$('#modes').onclick = (e) => {
  const b = e.target.closest('.mode'); if (!b) return;
  for (const x of $('#modes').children) x.setAttribute('aria-pressed', String(x === b));
  hog.returnTo = null;
  if (b.dataset.mode === 'mem') { enterHog(); return; }
  if (scanDone && !e.target.closest('.alt')) { backToMap(); return; } // a map already exists: go straight to it
  $('#disk-setup').hidden = false; $('#path').focus();
};
$('.mode .alt').onkeydown = (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); $('#disk-setup').hidden = false; $('#path').focus(); } };
/** Return to the existing disk map from the home screen. */
async function backToMap() {
  hideMenu(); setMode('disk');
  $('#landing').classList.add('away'); setTimeout(() => ($('#landing').hidden = true), 600);
  $('#app').hidden = false;
  controls.autoRotate = false; flyHome = true; camGoal = null;
  await navigate('');
  $('#panel').classList.remove('away');
}

// ======================= Memory: orbit view (gravity well) =======================
// The chosen resource is a well at the centre. Each process is an orb: size = memory, colour = its load on that
// resource, distance from the core = that load (busy spirals in, idle drifts to the rim), angular speed = load.
// Trails show who just moved. Faint lines tie helpers to their app. Pulsing rings mark odd behaviour.
let memView = new URLSearchParams(location.search).get('view') || (() => { try { return localStorage.getItem('dime.memview') || 'orbit'; } catch { return 'orbit'; } })();
if (memView !== 'list') memView = 'orbit';
const orbit = { on: false, by: 'cpu', orbs: new Map(), group: new THREE.Group(), lines: null, core: null, coreLbl: null, rings: [], hover: null, lastTrail: 0 };
scene.add(orbit.group);
const ARRANGE = { rss: ['Memory', (p) => p.rss], cpu: ['CPU', (p) => p.cpu], gpu: ['GPU', (p) => p.gpu_pct ?? 0], net: ['Network', netVal], disk: ['IO', diskVal] };
const R_CORE = 11, R_RIM = 72;
const COL_COOL = new THREE.Color('#3B6FB6'), COL_WARM = new THREE.Color('#8A5BC7'), COL_HOT2 = new THREE.Color('#FF7A3D');
const cpuColor = (t) => t < 0.5 ? new THREE.Color().lerpColors(COL_COOL, COL_WARM, t * 2) : new THREE.Color().lerpColors(COL_WARM, COL_HOT2, (t - 0.5) * 2);
const orbGeo = new THREE.SphereGeometry(1, 32, 20);
const trailMat = new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.85, blending: THREE.AdditiveBlending, depthWrite: false });
const TRAIL_N = 28;

function orbitBuildStatic() {
  if (orbit.core) return;
  const g = orbit.group;
  const core = new THREE.Mesh(new THREE.RingGeometry(R_CORE - 1.2, R_CORE, 96), new THREE.MeshBasicMaterial({ color: '#4FD1C5', transparent: true, opacity: 0.9, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  core.rotation.x = -Math.PI / 2; core.position.y = 0.1; g.add(core); orbit.core = core;
  const glow = new THREE.Mesh(new THREE.CircleGeometry(R_CORE - 1, 96), new THREE.MeshBasicMaterial({ color: '#4FD1C5', transparent: true, opacity: 0.08, blending: THREE.AdditiveBlending, depthWrite: false }));
  glow.rotation.x = -Math.PI / 2; glow.position.y = 0.05; g.add(glow); orbit.glow = glow;
  for (const f of [0.75, 0.5, 0.25]) {
    const r = R_CORE + (1 - f) * (R_RIM - R_CORE);
    const ring = new THREE.Mesh(new THREE.RingGeometry(r - 0.15, r + 0.15, 128), new THREE.MeshBasicMaterial({ color: '#E8E6DF', transparent: true, opacity: 0.07, side: THREE.DoubleSide, depthWrite: false }));
    ring.rotation.x = -Math.PI / 2; ring.position.y = 0.05; g.add(ring);
    const el = document.createElement('div'); el.className = 'oring'; el.textContent = `${Math.round(f * 100)}% of the busiest`;
    const lbl = new CSS2DObject(el); lbl.position.set(r * 0.72, 0, -r * 0.7); g.add(lbl); orbit.rings.push(lbl);
  }
  const cel = document.createElement('div'); cel.className = 'olbl'; orbit.coreLbl = new CSS2DObject(cel); orbit.coreLbl.position.set(0, 0.5, R_CORE + 6); orbit.coreLbl.center.set(0.5, 0); g.add(orbit.coreLbl);
  orbit.lines = new THREE.LineSegments(new THREE.BufferGeometry(), new THREE.LineBasicMaterial({ color: '#4FD1C5', transparent: true, opacity: 0.08, depthWrite: false }));
  g.add(orbit.lines);
}

function orbitSync() {
  // called after every snapshot: retarget every orb from the latest numbers
  const s = hog.snap; if (!s || !orbit.on) return;
  const val = ARRANGE[orbit.by][1];
  const max = Math.max(1e-9, ...s.procs.map(val)), maxRss = s.procs[0]?.rss || 1;
  const odd = new Map(findOdd(s).map((o) => [o.p.pid, o]));
  const shown = new Set(filtered(s).map((p) => p.pid));
  $('#memfilter .n').textContent = shown.size === s.procs.length ? `${s.procs.length} processes` : `${shown.size} of ${s.procs.length}`;
  const seen = new Set();
  for (const p of s.procs) {
    seen.add(p.pid);
    let o = orbit.orbs.get(p.pid);
    const f = Math.sqrt(Math.min(1, val(p) / max)); // sqrt: mid loads still visibly inward
    if (!o) {
      const ang = Math.random() * Math.PI * 2;
      const mat = new THREE.MeshStandardMaterial({ color: '#3B6FB6', emissive: '#3B6FB6', emissiveIntensity: 0.3, roughness: 0.3, metalness: 0.2, transparent: true, opacity: 0 });
      const m = new THREE.Mesh(orbGeo, mat); m.userData.pid = p.pid; orbit.group.add(m);
      const tg = new THREE.BufferGeometry();
      tg.setAttribute('position', new THREE.BufferAttribute(new Float32Array(TRAIL_N * 3), 3));
      tg.setAttribute('color', new THREE.BufferAttribute(new Float32Array(TRAIL_N * 3), 3));
      const trail = new THREE.Line(tg, trailMat); orbit.group.add(trail);
      const el = document.createElement('div'); el.className = 'olbl'; const lbl = new CSS2DObject(el); lbl.center.set(0.5, 1); m.add(lbl);
      const ring = new THREE.Mesh(new THREE.RingGeometry(1.35, 1.55, 48), new THREE.MeshBasicMaterial({ color: '#F5C26B', transparent: true, opacity: 0, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
      ring.rotation.x = -Math.PI / 2; m.add(ring);
      o = { pid: p.pid, m, trail, lbl, ring, ang, r: R_RIM + 30, rt: R_RIM, vr: 0, x: 0, z: 0, hist: [], size: 0.1, dying: false };
      o.x = Math.cos(ang) * o.r; o.z = Math.sin(ang) * o.r;
      orbit.orbs.set(p.pid, o);
    }
    o.p = p; o.f = f; o.dying = false; o.ghost = !shown.has(p.pid);
    o.rt = R_CORE + 2 + (1 - f) * (R_RIM - R_CORE - 2);
    o.sizeT = 1.1 + 6.5 * Math.cbrt(p.rss / maxRss);
    o.col = cpuColor(f);
    o.odd = odd.get(p.pid) ?? null;
  }
  for (const o of orbit.orbs.values()) if (!seen.has(o.pid)) { o.dying = true; o.rt = R_RIM + 40; }
  orbit.coreLbl.element.innerHTML = `${ARRANGE[orbit.by][0]}<small>${orbit.by === 'cpu' ? `${s.cpu.toFixed(0)}% of ${s.cpus} cores` : orbit.by === 'gpu' ? `${s.gpu ?? 0}% busy` : orbit.by === 'rss' ? `${fmt(s.used)} in use` : orbit.by === 'net' ? fmtRate(s.procs.reduce((a, p) => a + netVal(p), 0)) || 'quiet' : fmtRate(s.procs.reduce((a, p) => a + diskVal(p), 0)) || 'quiet'}</small>`;
  // constellation lines: helpers to the biggest member of their family
  const fams = new Map();
  for (const p of s.procs) { const k = family(p); const f = fams.get(k) ?? fams.set(k, []).get(k); f.push(p); }
  orbit.pairs = [];
  for (const members of fams.values()) { if (members.length < 2) continue; const head = members.reduce((a, b) => (b.rss > a.rss ? b : a)); for (const p of members) if (p !== head) orbit.pairs.push([head.pid, p.pid]); }
  renderOrbitOdd(s);
}

function updateOrbit(dt, now) {
  if (!orbit.on || orbit.frozen) return; // space bar pauses the field; data keeps polling underneath
  const f = REDUCED ? 1 : 1 - Math.exp(-dt * 3);
  const orbs = [...orbit.orbs.values()];
  // filtered-out orbs are gone: hidden and left out of the physics so the rest can spread
  for (const o of orbs) { if (o.ghost) { o.m.visible = false; o.trail.visible = false; o.lbl.visible = false; o.m.material.opacity = 0; } else { o.m.visible = true; o.trail.visible = true; } }
  const live = orbs.filter((o) => !o.ghost);
  // radial spring toward the target ring, tangential drift scaled by load, soft collisions
  for (const o of orbs) {
    const k = o.dying ? 2 : 3.5;
    o.vr = (o.vr + (o.rt - o.r) * k * dt) * Math.exp(-dt * 2.4);
    o.r += o.vr * dt;
    o.ang += (0.04 + (o.f ?? 0) * 0.55) * dt * (o.pid % 2 ? 1 : -1) * (REDUCED ? 0 : 1);
    o.tx = Math.cos(o.ang) * o.r; o.tz = Math.sin(o.ang) * o.r;
    o.size = smooth(o.size, o.dying ? 0.05 : o.sizeT ?? 1, f);
  }
  for (let i = 0; i < live.length; i++) for (let j = i + 1; j < live.length; j++) {
    const a = live[i], c = live[j], dx = c.tx - a.tx, dz = c.tz - a.tz, d2 = dx * dx + dz * dz, min = a.size + c.size + 1.2;
    if (d2 >= min * min || d2 === 0) continue;
    const d = Math.sqrt(d2), push = (min - d) * 0.5, nx = dx / d, nz = dz / d;
    a.tx -= nx * push; a.tz -= nz * push; c.tx += nx * push; c.tz += nz * push;
  }
  const doTrail = now - orbit.lastTrail > 90; if (doTrail) orbit.lastTrail = now;
  for (const o of orbs) {
    o.x = smooth(o.x, o.tx, 1 - Math.exp(-dt * 8)); o.z = smooth(o.z, o.tz, 1 - Math.exp(-dt * 8));
    const y = o.size + 0.3 + Math.sin(now / 1100 + o.pid) * 0.25;
    o.m.position.set(o.x, y, o.z); o.m.scale.setScalar(Math.max(0.05, o.size));
    if (o.dying) {
      o.m.material.opacity = Math.max(0, o.m.material.opacity - dt * 1.2);
      if (o.m.material.opacity <= 0.01 && o.r > R_RIM + 30) { orbit.group.remove(o.m, o.trail); o.m.material.dispose(); o.trail.geometry.dispose(); o.lbl.element.remove(); orbit.orbs.delete(o.pid); }
      continue;
    }
    if (o.ghost) { o.hist.length = 0; continue; }
    o.m.material.opacity = Math.min(1, o.m.material.opacity + dt * 1.5);
    const lit = orbit.hover === o.pid || hog.sel === o.pid;
    o.m.material.color.lerp(o.col, f); o.m.material.emissive.lerp(o.col, f);
    o.m.material.emissiveIntensity = smooth(o.m.material.emissiveIntensity, lit ? 1.1 : 0.25 + (o.f ?? 0) * 0.8, f);
    // trail
    if (doTrail) { o.hist.push([o.x, y, o.z]); if (o.hist.length > TRAIL_N) o.hist.shift(); }
    const pos = o.trail.geometry.attributes.position, col = o.trail.geometry.attributes.color;
    const n = o.hist.length;
    for (let i = 0; i < TRAIL_N; i++) {
      const h = o.hist[Math.max(0, i - (TRAIL_N - n))] ?? [o.x, y, o.z];
      pos.setXYZ(i, h[0], h[1], h[2]);
      const t = i / (TRAIL_N - 1), c = o.col;
      col.setXYZ(i, c.r * t * 0.9, c.g * t * 0.9, c.b * t * 0.9);
    }
    pos.needsUpdate = true; col.needsUpdate = true; o.trail.geometry.setDrawRange(0, TRAIL_N);
    // odd ring
    if (o.odd) { o.ring.material.color.set(SEV_COLOR[o.odd.rule.sev]); o.ring.material.opacity = 0.35 + 0.35 * Math.sin(now / 220); const sc = 1 + 0.12 * Math.sin(now / 220); o.ring.scale.set(sc, sc, sc); }
    else o.ring.material.opacity = smooth(o.ring.material.opacity, 0, f);
    // label
    const show = (o.f ?? 0) > 0.45 || o.sizeT > 6.3 || lit || o.odd || ((memFilter.q || memFilter.kind !== 'all') && !memFilter.not);
    o.lbl.visible = !!show;
    if (show) { const p = o.p; o.lbl.element.innerHTML = `${p.name}<small>${orbit.by === 'rss' ? fmt(p.rss) : `${LANES.find((l) => l.id === orbit.by)?.text(p) || fmt(p.rss)} · ${fmt(p.rss)}`}</small>`; o.lbl.position.set(0, 1.3, 0); }
  }
  // constellation lines
  const pairs = (orbit.pairs ?? []).filter(([a, b]) => orbit.orbs.has(a) && orbit.orbs.has(b) && !orbit.orbs.get(a).ghost && !orbit.orbs.get(b).ghost);
  const arr = new Float32Array(pairs.length * 6);
  pairs.forEach(([a, b], i) => { const oa = orbit.orbs.get(a), ob = orbit.orbs.get(b); arr.set([oa.x, oa.size + 0.3, oa.z, ob.x, ob.size + 0.3, ob.z], i * 6); });
  orbit.lines.geometry.setAttribute('position', new THREE.BufferAttribute(arr, 3));
  orbit.core.material.opacity = 0.7 + 0.25 * Math.sin(now / 900);
  orbit.core.rotation.z = now / 9000;
}

// ---- the ship: a saucer that flies in over the orb you picked and lights it until you close the drawer
const ship = { g: null, x: 0, y: 90, z: 0, vx: 0, vz: 0, on: false };
function makeShip() {
  // a retro rocket: cream hull with a coral nose and fins, portholes, one big engine bell
  const g = new THREE.Group();
  const cream = new THREE.MeshStandardMaterial({ color: '#F3EEE3', roughness: 0.35, metalness: 0.25 });
  const coral = new THREE.MeshStandardMaterial({ color: '#FF5A4E', roughness: 0.4, metalness: 0.2, emissive: '#FF5A4E', emissiveIntensity: 0.12 });
  const steel = new THREE.MeshStandardMaterial({ color: '#4A5470', roughness: 0.3, metalness: 0.8 });
  const body = new THREE.Mesh(new THREE.CylinderGeometry(3.2, 3.6, 14, 32), cream); body.position.y = 4; g.add(body);
  const stripe = new THREE.Mesh(new THREE.CylinderGeometry(3.3, 3.35, 1.4, 32), coral); stripe.position.y = 0.5; g.add(stripe);
  const nose = new THREE.Mesh(new THREE.ConeGeometry(3.2, 7, 32), coral); nose.position.y = 14.5; g.add(nose);
  const tip = new THREE.Mesh(new THREE.SphereGeometry(0.5, 12, 8), new THREE.MeshBasicMaterial({ color: '#FFF6E0' })); tip.position.y = 18; g.add(tip);
  for (let i = 0; i < 3; i++) { // fins
    const fin = new THREE.Mesh(new THREE.BoxGeometry(0.5, 6, 4.2), coral);
    const a = i / 3 * Math.PI * 2; fin.position.set(Math.cos(a) * 4.6, -1.2, Math.sin(a) * 4.6); fin.rotation.y = -a; fin.rotation.z = 0.35 * (Math.cos(a) >= 0 ? 1 : 1); fin.geometry.translate(0, 0, 0);
    const finG = new THREE.Group(); finG.rotation.y = -a; const f2 = new THREE.Mesh(new THREE.BoxGeometry(4.6, 6.5, 0.5), coral); f2.position.set(5.2, -1.4, 0); f2.rotation.z = 0.5; finG.add(f2); g.add(finG);
  }
  for (let i = 0; i < 3; i++) { // portholes
    const a = i / 3 * Math.PI * 2 + 0.5; const ring = new THREE.Mesh(new THREE.TorusGeometry(0.9, 0.22, 10, 24), steel); ring.position.set(Math.cos(a) * 3.3, 7 - i * 2.2, Math.sin(a) * 3.3); ring.lookAt(Math.cos(a) * 10, 7 - i * 2.2, Math.sin(a) * 10); g.add(ring);
    const glass = new THREE.Mesh(new THREE.CircleGeometry(0.75, 20), new THREE.MeshBasicMaterial({ color: '#9FE8DF' })); glass.position.copy(ring.position); glass.lookAt(Math.cos(a) * 10, 7 - i * 2.2, Math.sin(a) * 10); g.add(glass);
  }
  const bell = new THREE.Mesh(new THREE.CylinderGeometry(1.8, 3.0, 2.6, 32, 1, true), new THREE.MeshStandardMaterial({ color: '#4A5470', roughness: 0.3, metalness: 0.8, side: THREE.DoubleSide })); bell.position.y = -4.2; g.add(bell);
  // engine flame: a long outer plume and a bright core, both scaled by thrust
  const flames = [];
  const flame = new THREE.Mesh(new THREE.ConeGeometry(2.6, 12, 24, 1, true), new THREE.MeshBasicMaterial({ color: '#FF9A3D', transparent: true, opacity: 0.8, blending: THREE.AdditiveBlending, depthWrite: false, side: THREE.DoubleSide }));
  flame.rotation.x = Math.PI; flame.position.y = -11; g.add(flame);
  const fcore = new THREE.Mesh(new THREE.ConeGeometry(1.2, 8, 16, 1, true), new THREE.MeshBasicMaterial({ color: '#FFF3C4', transparent: true, opacity: 0.95, blending: THREE.AdditiveBlending, depthWrite: false, side: THREE.DoubleSide }));
  fcore.rotation.x = Math.PI; fcore.position.y = -9; g.add(fcore);
  flames.push({ flame, core: fcore, seed: 1 });
  const glow = new THREE.PointLight('#FF9A3D', 60, 40, 1.5); glow.position.set(0, -8, 0); g.add(glow);
  // the searchlight sits in the belly, off-axis from the engine so the beam is not lost in the flame
  // the beam lives in the scene, apex at the rocket's belly, aimed at the orb each frame
  const beam = new THREE.Group(); scene.add(beam);
  const coneGeo = new THREE.ConeGeometry(1, 1, 48, 1, true); coneGeo.translate(0, -0.5, 0); // apex at origin, opens downward
  const cone = new THREE.Mesh(coneGeo, new THREE.MeshBasicMaterial({ color: '#FFF1C8', transparent: true, opacity: 0.3, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false })); beam.add(cone);
  const core = new THREE.Mesh(coneGeo, new THREE.MeshBasicMaterial({ color: '#FFFFFF', transparent: true, opacity: 0.45, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false })); beam.add(core);
  const lamp = new THREE.Mesh(new THREE.SphereGeometry(0.9, 12, 10), new THREE.MeshBasicMaterial({ color: '#FFFBEA' })); lamp.position.set(0, -4.6, 0); g.add(lamp);
  const light = new THREE.SpotLight('#CFFFF7', 400, 90, 0.45, 0.5, 1.2); light.position.set(0, -3, 0); g.add(light); g.add(light.target); light.target.position.set(0, -40, 0);
  const pool = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), new THREE.MeshBasicMaterial({ map: poolTex(), color: '#BFFFF5', transparent: true, opacity: 0, blending: THREE.AdditiveBlending, depthWrite: false })); pool.rotation.x = -Math.PI / 2; scene.add(pool);
  g.visible = false; scene.add(g);
  Object.assign(ship, { g, lights: new THREE.Group(), flames, glow, beam, cone, core, light, pool });
}
function updateShip(dt, now) {
  const o = hog.sel && orbit.orbs.get(hog.sel);
  const want = !!o && !o.dying && !o.ghost && orbit.on;
  if (want && !ship.g) makeShip();
  if (!ship.g) return;
  if (want && !ship.on) { ship.on = true; ship.x = o.x + 90; ship.z = o.z - 60; ship.y = 110; ship.vx = ship.vz = 0; ship.g.visible = true; }
  let tx, tz, ty;
  if (want) { tx = o.x + Math.cos(now / 2500) * 1.2 + o.size * 1.2; tz = o.z + Math.sin(now / 2500) * 1.2; ty = o.size * 2 + 26; } // hangs a little to the side so the flame is not on the orb
  else { if (!ship.on) return; tx = ship.x + 40; tz = ship.z - 40; ty = 150; }
  if (REDUCED) { ship.x = tx; ship.z = tz; ship.y = ty; } else {
    const ax = (tx - ship.x) * 3 - ship.vx * 1.8, az = (tz - ship.z) * 3 - ship.vz * 1.8;
    ship.vx += ax * dt; ship.vz += az * dt; ship.x += ship.vx * dt; ship.z += ship.vz * dt;
    ship.y = smooth(ship.y, ty + Math.sin(now / 800) * 0.5, 1 - Math.exp(-dt * 2.5));
  }
  ship.g.position.set(ship.x, ship.y, ship.z);
  const speed = Math.hypot(ship.vx, ship.vz);
  // a rocket stays upright and leans into its direction of travel
  ship.g.rotation.z = smooth(ship.g.rotation.z, -Math.max(-0.45, Math.min(0.45, ship.vx * 0.02)), 1 - Math.exp(-dt * 3));
  ship.g.rotation.x = smooth(ship.g.rotation.x, Math.max(-0.45, Math.min(0.45, ship.vz * 0.02)), 1 - Math.exp(-dt * 3));
  ship.g.rotation.y += dt * 0.15;
  // flames: idle flicker, big flare while moving
  const thrust = Math.min(1, speed / 25);
  for (const f of ship.flames) {
    const flick = 0.75 + 0.25 * Math.sin(now / 37 + f.seed) * Math.sin(now / 53 + f.seed * 2);
    const len = (0.45 + 1.4 * thrust) * flick;
    f.flame.scale.set(0.75 + 0.45 * thrust, len, 0.75 + 0.45 * thrust); f.core.scale.set(1, len, 1);
    f.flame.material.opacity = 0.55 + 0.4 * thrust; f.core.material.opacity = 0.7 + 0.3 * thrust;
  }
  ship.glow.intensity = 15 + 60 * thrust;
  // beam from the ship's belly to the orb's surface
  const near = want && speed < 8;
  if (want) {
    // conical searchlight from the belly lamp to the orb's centre, widening to wrap the orb
    const belly = new THREE.Vector3(0, -4.6, 0).applyEuler(ship.g.rotation).add(ship.g.position);
    const dir = new THREE.Vector3(o.x, o.y, o.z).sub(belly); const len = dir.length() + o.size * 0.6, r = o.size * 1.35;
    ship.beam.position.copy(belly); ship.beam.quaternion.setFromUnitVectors(new THREE.Vector3(0, -1, 0), dir.normalize());
    ship.cone.scale.set(r, len, r); ship.core.scale.set(r * 0.35, len, r * 0.35);
    ship.beam.visible = true;
    ship.pool.position.set(o.x, 0.12, o.z); const ps = o.size * 3.2; ship.pool.scale.set(ps, ps, 1);
    o.m.material.emissiveIntensity = Math.max(o.m.material.emissiveIntensity, 1.3);
  }
  ship.cone.material.opacity = smooth(ship.cone.material.opacity, near ? 0.3 : 0.08, 1 - Math.exp(-dt * 4));
  ship.core.material.opacity = smooth(ship.core.material.opacity, near ? 0.45 : 0.1, 1 - Math.exp(-dt * 4));
  if (!want) ship.beam.visible = false;
  ship.light.intensity = near ? 500 : 60; if (want) ship.light.target.position.set((o.x - ship.x), o.y + o.size - ship.y, (o.z - ship.z)).applyAxisAngle(new THREE.Vector3(0, 1, 0), -ship.g.rotation.y);
  ship.pool.material.opacity = smooth(ship.pool.material.opacity, near ? 0.6 : 0, REDUCED ? 1 : 1 - Math.exp(-dt * 4));
  if (!want && ship.y > 140) { ship.on = false; ship.g.visible = false; ship.beam.visible = false; ship.pool.material.opacity = 0; }
}
function updateOrbitHover() {
  ray.setFromCamera(mouse, camera);
  const hit = ray.intersectObjects([...orbit.orbs.values()].filter((o) => !o.dying && !o.ghost).map((o) => o.m), false)[0]?.object ?? null;
  const pid = hit?.userData.pid ?? null;
  if (pid === orbit.hover) return;
  orbit.hover = pid;
  renderer.domElement.style.cursor = pid ? 'pointer' : '';
  const o = pid && orbit.orbs.get(pid);
  if (!o) { tip.hidden = true; return; }
  const p = o.p;
  tip.innerHTML = `<div class="n"></div><div class="m"></div><div class="m"></div><div class="f"></div><div class="h"></div>`;
  const [name, m1, m2, fl, hh] = tip.children;
  name.textContent = `${p.name} · pid ${p.pid}`;
  m1.textContent = `${fmt(p.rss)} · ${p.cpu.toFixed(0)}% cpu · gpu ${gpuText(p) || '0'}`;
  m2.textContent = [fmtRate(p.net_in) && `↓ ${fmtRate(p.net_in)}`, fmtRate(p.net_out) && `↑ ${fmtRate(p.net_out)}`, fmtRate(p.read_rate) && `read ${fmtRate(p.read_rate)}`, fmtRate(p.write_rate) && `write ${fmtRate(p.write_rate)}`].filter(Boolean).join(' · ') || 'quiet on disk and network';
  fl.textContent = o.odd ? o.odd.why : ''; if (o.odd) fl.style.color = SEV_COLOR[o.odd.rule.sev];
  hh.textContent = `${family(p) !== p.name ? `part of ${family(p)} · ` : ''}click for details and open files`;
  tip.hidden = false; placeTip();
}
function renderOrbitOdd(s) {
  const el = $('#oodd'); el.innerHTML = '';
  for (const it of findOdd(s).filter((o) => passes(o.p, s)).slice(0, 3)) {
    const d = document.createElement('div'); d.className = 'odd'; d.style.setProperty('--c', SEV_COLOR[it.rule.sev]);
    d.innerHTML = `<i></i><div><b></b> <span class="why"></span></div><div class="t"></div>`;
    d.querySelector('b').textContent = it.p.name; d.querySelector('.why').textContent = it.why;
    d.onclick = () => openDrawer(it.p.pid); el.appendChild(d);
  }
}
$('#arrange').insertAdjacentHTML('beforeend', Object.entries(ARRANGE).map(([k, [l]]) => `<button data-a="${k}" aria-pressed="${k === 'cpu'}">${l}</button>`).join('') + `<button id="pause" class="pause" aria-pressed="false" title="Space">Pause</button>`);
$('#pause').onclick = () => document.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
$('#arrange').onclick = (e) => { const b = e.target.closest('button[data-a]'); if (!b) return; orbit.by = b.dataset.a; for (const x of $('#arrange').querySelectorAll('button')) x.setAttribute('aria-pressed', String(x === b)); orbitSync(); $('#hint').textContent = `Me: arranged by ${ARRANGE[orbit.by][0].toLowerCase()}. The closer to the core, the more it is using right now.`; };

function setMemView(v) {
  memView = v; try { localStorage.setItem('dime.memview', v); } catch {}
  document.body.dataset.view = v;
  if (v === 'orbit') {
    orbitBuildStatic(); orbit.on = true; orbit.group.visible = true;
    controls.target.set(14, 0, 0); camera.position.set(-6, 205, 150); camGoal = null; flyHome = false;
    $('#hint').textContent = 'Me: every orb is a process. The closer to the core, the busier. Click one and I fly over for a closer look.';
    orbitSync();
  } else {
    orbit.on = false; orbit.group.visible = false; tip.hidden = true; orbit.hover = null;
    renderDash();
  }
  renderMemCrumbs();
}

if (location.hash === '#memory') enterHog();


// ---------- export: the cleanup view as a Markdown report you can hand to anything that reads text ----------
function exportReport() {
  if (!current || !summaryData) return;
  const abs = (p) => rootPath + (p ? '/' + p : '');
  const L = [`# DiMe cleanup report`, ``, `- Scan root: ${rootPath || '/'}`, `- Folder: ${abs(current.path)} · ${nodeInfo(current)}`, filter === 'idle' ? `- Filter: only items untouched for ${idleDays}+ days` : '', `- Generated: ${new Date().toLocaleString()}`,
    `- How to read this: Di flags candidates in three tiers. "Safe to remove" is regenerated automatically; "Probably safe" is usually fine after a glance; "Worth a look" is big or old and your call. Sizes are on-disk. Shelving moves an item into ~/.dime/shelf, still on the disk and reversible; deleting is permanent and is what frees the space.`, ``];
  const totals = filter === 'idle' ? new Map() : new Map(summaryData.kinds.map(([id, , , size, n]) => [id, [size, n]]));
  const list = cleanList(), kinds = [...new Set(list.map((c) => c.reason))].map((id) => { const items = list.filter((c) => c.reason === id && !hiddenUnder(c.path)), [size, n] = totals.get(id) ?? [sumOf(items), items.length]; return { id, tier: items[0]?.tier, what: items[0]?.what, size: Math.max(size, sumOf(items)), n: Math.max(n, items.length), items }; }).filter((k) => k.items.length).sort((x, y) => TIER_ORDER[x.tier] - TIER_ORDER[y.tier] || y.size - x.size);
  const tierTotal = (t) => kinds.filter((k) => k.tier === t).reduce((a, k) => [a[0] + k.size, a[1] + k.n], [0, 0]);
  L.push(`## Totals`, ``, ...['safe', 'likely', 'review'].map((t) => { const [sz, n] = tierTotal(t); return `- ${TIER_LABEL[t]}: ${fmt(sz)} · ${n} item${n === 1 ? '' : 's'}`; }), ``);
  let last = null;
  for (const k of kinds) {
    if (k.tier !== last) { L.push(`## ${TIER_LABEL[k.tier]}`, ``); last = k.tier; }
    L.push(`### ${k.what} · ${fmt(k.size)} · ${k.n} item${k.n === 1 ? '' : 's'}`, ``, `${KIND_HINT[k.id] ?? ''}`, ``);
    for (const c of k.items) L.push(`- ${abs(c.path)}${c.is_dir ? '/' : ''} · ${fmt(c.size)} · ${fmtAge(c.age_days)} · ${c.note}`);
    if (k.items.length < k.n) L.push(`- … and ${k.n - k.items.length} more, smaller`);
    L.push(``);
  }
  if (picked.size) L.push(`## Ticked by the user`, ``, ...[...picked.values()].map((c) => `- ${abs(c.path)} · ${fmt(c.size)} · ${TIER_LABEL[c.tier]} · ${c.what}`), ``);
  if (hiddenPaths.size) L.push(`## Hidden from the map by the user`, ``, ...[...hiddenPaths.values()].map((n) => `- ${abs(n.path)} · ${fmt(n.size)}`), ``);
  if (shelfList.length) L.push(`## On the shelf`, ``, ...shelfList.map((e) => `- ${e.name}${e.is_dir ? '/' : ''} · ${fmt(e.size)} · from ${e.from} · shelved ${new Date(e.shelved_at * 1000).toLocaleDateString()}${e.note ? ` · ${e.note}` : ''}`), ``);
  const blob = new Blob([L.join('\n')], { type: 'text/markdown' }), a = document.createElement('a');
  a.href = URL.createObjectURL(blob); a.download = `dime-cleanup-${(current.path ? current.name : rootName).replace(/[^\w.-]+/g, '_')}-${new Date().toISOString().slice(0, 10)}.md`;
  document.body.appendChild(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  toast(`Di: report saved · ${a.download}`, 'du');
}

// ---------- Ru: the wise one. Three areas it can look at: your Selection, the Folder in view, the Shelf ----------
const ru = { open: false, busy: false, turns: [], area: 'folder', pinned: false, abort: null, sel: new Map(), label: null }; // sel: rel path -> item; label: the AI behind Ru, null when none
const ruEl = $('#ru'), ruMsgs = ruEl.querySelector('.msgs'), ruIn = ruEl.querySelector('textarea'), ruSend = ruEl.querySelector('.ask .btn');
const absOf = (p) => rootPath + (p ? '/' + p : '');
const relOf = (abs) => abs === rootPath ? '' : abs.startsWith(rootPath + '/') ? abs.slice(rootPath.length + 1) : null;
const candLine = (c) => `- ${absOf(c.path)} · ${fmt(c.size)} · ${TIER_LABEL[c.tier]} · ${c.what}. ${c.note}`;
const kidLine = (c) => `- ${c.name}${c.is_dir ? '/' : ''} · ${fmt(c.size)} · ${fmtAge(ageDays(c))}${c.types?.some(Boolean) ? ` · mostly ${TYPE_NAMES[dominant(c.types)]}` : ''}`;
// ---- the Selection: items you clicked, ticked, dragged in, or sent from a menu. Each is hydrated with what Di knows about that path.
function ruSelAdd(path, { source = 'drop', node = null, cand = null } = {}) {
  let it = ru.sel.get(path);
  if (!it) {
    const n = node ?? current?.children.find((c) => c.path === path) ?? null;
    it = { path, name: (n?.name ?? cand?.name ?? path.split('/').pop()) || rootName, is_dir: n?.is_dir ?? cand?.is_dir ?? true, size: n?.size ?? cand?.size ?? 0, node: n, cand: cand ?? gunkSet.get(path) ?? null, kids: [], flagged: [], source, ready: false };
    ru.sel.set(path, it);
    ruHydrate(it);
  } else if (source !== 'map') it.source = source; // a click can promote nothing; a tick or drop makes it stick
  ruSync(); return it;
}
function ruSelRemove(path) { const it = ru.sel.get(path); if (!it) return; ru.sel.delete(path); if (it.source === 'tick' && picked.has(path)) { picked.delete(path); if (current) renderCleanup(); } if (it.source === 'map' && selected?.userData.node.path === path) { setSelected(null); return; } ruSync(); }
async function ruHydrate(it) {
  try {
    const q = encodeURIComponent(it.path);
    const idle = filter === 'idle' ? `&idle=${idleDays}` : '';
    const [t, g] = await Promise.all([api(`/api/tree?path=${q}&depth=1${idle}`), api(`/api/gunk?path=${q}${idle}`)]);
    it.name = t.name || it.name; it.is_dir = t.is_dir; it.size = t.size; it.node = t;
    it.kids = (t.children ?? []).filter((c) => c.path).sort((x, y) => y.size - x.size);
    it.flagged = g; it.cand = g.find((c) => c.path === it.path) ?? it.cand;
  } catch {}
  it.ready = true; ruSync();
}
// keep the Selection honest with the map and the Cleanup ticks; dropped and sent items stay until you remove them
function ruReconcile() {
  const mapPath = mode === 'disk' && selected ? selected.userData.node.path : null;
  for (const [p, it] of ru.sel) { if (it.source === 'map' && p !== mapPath) ru.sel.delete(p); if (it.source === 'tick' && !picked.has(p)) ru.sel.delete(p); }
  if (mapPath !== null && !ru.sel.has(mapPath)) { const n = selected.userData.node; ru.sel.set(mapPath, { path: mapPath, name: n.name, is_dir: n.is_dir, size: n.size, node: n, cand: gunkSet.get(mapPath) ?? null, kids: (n.children ?? []).filter((c) => c.path), flagged: gunkList.filter((c) => c.path === mapPath || c.path.startsWith(mapPath + '/')), source: 'map', ready: true }); }
  for (const [p, c] of picked) if (!ru.sel.has(p)) ru.sel.set(p, { path: p, name: c.name, is_dir: c.is_dir, size: c.size, node: null, cand: c, kids: [], flagged: [c], source: 'tick', ready: true });
}
const ruAreas = () => [
  { id: 'selection', label: mode === 'mem' && hog.sel ? 'Process' : ru.sel.size ? `Selection ${ru.sel.size}` : 'Selection', on: mode === 'mem' ? !!hog.sel : ru.sel.size > 0, hint: 'what you clicked, ticked, or dragged in' },
  { id: 'folder', label: 'Folder', on: mode === 'disk' && !!current, hint: 'the folder in view' },
  { id: 'shelf', label: shelfList.length ? `Shelf ${shelfList.length}` : 'Shelf', on: shelfList.length > 0, hint: 'shelved items' },
];
function ruContext() {
  const L = [];
  if (filter === 'idle') L.push(`IDLE VIEW: the user is looking only at files untouched for ${idleDays}+ days. Sizes in the panel are the idle bytes inside each item, not the whole item; "move" or "delete" would act only on those files and leave the rest of the folder in place. Di's index (the di command) reports whole items.`);
  if (ru.area === 'selection') {
    if (mode === 'mem') {
      const p = procById(hog.sel);
      if (p) L.push(`SUBJECT, a process the user is looking at in Me: ${p.name} · pid ${p.pid}${p.parent ? ` · parent pid ${p.parent}` : ''} · user ${p.user} · ${p.exe}\n${fmt(p.rss)} memory · ${p.cpu.toFixed(0)}% CPU · ${fmtRate(p.read_rate + p.write_rate)} disk · ${fmtRate(p.net_in + p.net_out)} network · running ${Math.round(p.run_time / 60)} min`);
      const files = hog.filesPid === hog.sel ? hog.files ?? [] : [];
      if (files.length) L.push(`Files it has open (${files.length}, biggest first):\n` + files.slice(0, 25).map((f) => `- ${f.path} · ${fmt(f.size)}${f.written_ago != null ? ` · written ${fmtAge(Math.round(f.written_ago / 86400))}` : ''}`).join('\n'));
    } else {
      const items = [...ru.sel.values()].slice(0, 12);
      L.push(`SUBJECT, ${items.length === 1 ? 'the item' : `${items.length} items`} the user selected${ru.sel.size > items.length ? ` (${ru.sel.size - items.length} more not shown)` : ''}:`);
      for (const it of items) {
        const idleNote = it.cand?.full_size != null ? ` · of which ${fmt(it.cand.size)} untouched ${idleDays}+ days` : '';
        const head = `${absOf(it.path)}${it.is_dir ? '/' : ''} · ${fmt(it.cand?.full_size ?? it.size)}${idleNote}${it.node ? ` · ${it.is_dir ? fmtN(it.node.files) + ' files · ' : ''}${fmtAge(ageDays(it.node))}` : ''}${it.node?.types?.some(Boolean) ? ` · mostly ${TYPE_NAMES[dominant(it.node.types)]}` : ''}${it.cand ? `\n  Di flagged it: ${TIER_LABEL[it.cand.tier]} · ${it.cand.what}. ${it.cand.note}` : ''}`;
        const kids = it.kids.slice(0, 12).map((c) => '  ' + kidLine(c)).join('\n');
        const fl = it.flagged.filter((c) => c.path !== it.path).slice(0, 12).map((c) => '  ' + candLine(c)).join('\n');
        L.push(`### ${head}${kids ? `\n  Inside, biggest first:\n${kids}` : ''}${fl ? `\n  Di flagged inside it (${it.flagged.length} in all):\n${fl}` : ''}`);
      }
    }
  } else if (ru.area === 'shelf') {
    L.push('SUBJECT, the shelf (~/.dime/shelf). The user set these aside; they still sit on the disk, and each can be put back to its original path or deleted for good:\n' + shelfList.map((e) => `- ${e.name}${e.is_dir ? '/' : ''} · ${fmt(e.size)} · from ${e.from} · shelved ${new Date(e.shelved_at * 1000).toLocaleDateString()}${e.note ? ` · ${e.note}` : ''}`).join('\n'));
  } else if (current) {
    L.push(`SUBJECT, the folder in view: ${absOf(current.path)}\n${nodeInfo(current)}`);
    const kids = current.children.filter((c) => c.path && !hiddenPaths.has(c.path)).slice(0, 30);
    if (kids.length) L.push('Inside it, biggest first:\n' + kids.map(kidLine).join('\n'));
    const all = cleanList(), fl = all.filter((c) => !hiddenUnder(c.path)).slice(0, 25);
    if (fl.length) L.push(`Di flagged under it${filter === 'idle' ? `, untouched ${idleDays}+ days` : ''} (${all.length} in all, biggest ${fl.length}):\n` + fl.map(candLine).join('\n'));
  }
  const also = [];
  if (ru.area !== 'folder' && mode === 'disk' && current) also.push(`folder in view ${absOf(current.path)}`);
  if (ru.area !== 'shelf' && shelfList.length) also.push(`${shelfList.length} item${shelfList.length === 1 ? '' : 's'} on the shelf`);
  if (ru.area !== 'selection' && ru.sel.size) also.push(`${ru.sel.size} item${ru.sel.size === 1 ? '' : 's'} in the user's selection`);
  L.push(`Scan root: ${rootPath || '/'}${also.length ? `. Also on screen, not the subject: ${also.join('; ')}.` : ''}`);
  return L.join('\n\n');
}
// the focus card: one sentence naming what Ru is about, a tally of what goes with it, and the full text on demand
function ruPeek() {
  const f = ruEl.querySelector('.focus'), fs = f.querySelector('.fs'), bits = [];
  let head = '';
  if (ru.area === 'selection') {
    if (mode === 'mem') { const p = procById(hog.sel); head = `<b>${esc(p?.name ?? 'process')}</b> <small>· process in Me</small>`; bits.push(`${Math.min(25, (hog.filesPid === hog.sel ? hog.files ?? [] : []).length)} open files`); }
    else {
      const its = [...ru.sel.values()];
      const one = its.length === 1 ? its[0] : null;
      head = one ? `<b>${esc(one.name)}${one.is_dir ? '/' : ''}</b> <small>· ${fmt(one.size)} · ${{ map: 'selected on the map', tick: 'ticked in Cleanup', drop: 'dragged in', menu: 'sent from a menu' }[one.source]}</small>` : `<b>${its.length} items</b> <small>· ${fmt(its.reduce((a, it) => a + it.size, 0))} · your selection</small>`;
      bits.push(`${its.reduce((a, it) => a + Math.min(12, it.kids.length), 0)} inside`, `${its.reduce((a, it) => a + Math.min(12, it.flagged.length), 0)} flagged by Di`, its.some((it) => !it.ready) ? 'still loading' : '');
    }
  } else if (ru.area === 'shelf') { head = `<b>the Shelf</b> <small>· ${shelfList.length} shelved item${shelfList.length === 1 ? '' : 's'} · ${fmt(sumOf(shelfList))}</small>`; bits.push('every item, with where it came from'); }
  else if (current) { const kids = current.children.filter((c) => c.path && !hiddenPaths.has(c.path)).length, fl = gunkList.filter((c) => !hiddenUnder(c.path)).length; head = `<b>${esc(current.path ? current.name : rootName)}/</b> <small>· ${fmt(current.size)} · the folder in view</small>`; bits.push(`${Math.min(30, kids)} of ${kids} inside`, `${Math.min(25, fl)} of ${fl} flagged by Di`); }
  else head = '<b>nothing yet</b> <small>· map a disk first</small>';
  if (ru.turns.length) bits.push(`last ${Math.min(8, ru.turns.length)} turns`);
  fs.innerHTML = head;
  f.querySelector('.sum').textContent = bits.filter(Boolean).join(' · ');
  f.querySelector('pre').textContent = ruContext();
}
const esc = (x) => String(x).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
{ const f = ruEl.querySelector('.focus'), pre = f.querySelector('pre'), more = f.querySelector('.more'); more.onclick = () => { pre.hidden = !pre.hidden; more.textContent = pre.hidden ? 'show what Ru sees' : 'hide'; }; }
// switcher pills: Selection, the folder, the Shelf; the lit one is what Ru is about. Chips for the Selection underneath.
function ruCtxLine() {
  ruReconcile();
  const A = ruAreas(), on = A.filter((x) => x.on);
  if (!ru.pinned && !ru.turns.length) ru.area = on[0]?.id ?? 'folder'; // before the first question, follow what you touch: a selection beats the folder
  else if (!A.find((x) => x.id === ru.area)?.on) ru.area = on[0]?.id ?? 'folder';
  const el = ruEl.querySelector('.subs'); el.innerHTML = '';
  for (const x of A) { const b = document.createElement('button'); b.type = 'button'; b.textContent = x.label; b.disabled = !x.on; b.title = x.id === ru.area ? `Ru is looking at this (${x.hint})` : x.on ? `Switch Ru to this (${x.hint})` : `Nothing here yet (${x.hint})`; b.setAttribute('aria-pressed', String(x.id === ru.area)); b.onclick = () => { ru.area = x.id; ru.pinned = true; ruCtxLine(); ruSuggest(); }; el.appendChild(b); }
  const chips = ruEl.querySelector('.selrow'); chips.innerHTML = '';
  chips.hidden = !(mode === 'disk' && ru.area === 'selection' && ru.sel.size > 1);
  for (const it of ru.sel.values()) { const c = document.createElement('span'); c.className = `chip ${it.source}`; c.innerHTML = `<b></b><small></small><button type="button" aria-label="Remove">×</button>`; c.querySelector('b').textContent = it.name + (it.is_dir ? '/' : ''); c.querySelector('small').textContent = it.ready ? fmt(it.size) : '…'; c.title = `${absOf(it.path)} · ${{ map: 'selected on the map', tick: 'ticked in Cleanup', drop: 'dragged in', menu: 'sent from a menu' }[it.source]}`; c.querySelector('button').onclick = () => ruSelRemove(it.path); chips.appendChild(c); }
  ruPeek();
}
const ruSync = () => { if (ru.open) { ruCtxLine(); ruSuggest(); } };
function ruSuggest() {
  const el = ruEl.querySelector('.sugg'); el.innerHTML = '';
  if (ru.turns.length) return;
  // one question, because there is only one thing Ru is for
  const q = 'Validate this can be deleted';
  const b = document.createElement('button'); b.type = 'button'; b.textContent = q; b.onclick = () => ruAsk(q); el.appendChild(b);
}
async function ruOpen(area) {
  ru.open = true; ruEl.hidden = false; document.body.classList.add('ru-open'); hideMenu(); tip.hidden = true;
  try { shelfList = await api('/api/shelf'); } catch {}
  if (area) { ru.area = area; ru.pinned = true; }
  ruCtxLine(); ruSuggest(); ruIn.focus();
}
// drag the top-left corner to resize; remembered
{ const rz = ruEl.querySelector('.rz');
  try { const { w, h } = JSON.parse(localStorage.getItem('dime.ru') || '{}'); if (w) ruEl.style.width = `${w}px`; if (h) ruEl.style.height = `${h}px`; } catch {}
  rz.onpointerdown = (e) => {
    e.preventDefault(); rz.setPointerCapture(e.pointerId); const r = ruEl.getBoundingClientRect(); let w = r.width, h = r.height;
    rz.onpointermove = (ev) => { w = Math.round(Math.min(Math.max(320, r.right - ev.clientX), innerWidth - 40)); h = Math.round(Math.min(Math.max(260, r.bottom - ev.clientY), innerHeight - 40)); ruEl.style.width = `${w}px`; ruEl.style.height = `${h}px`; };
    rz.onpointerup = () => { rz.onpointermove = rz.onpointerup = null; try { localStorage.setItem('dime.ru', JSON.stringify({ w, h })); } catch {} };
  }; }
function ruClose() { ru.open = false; ruEl.hidden = true; document.body.classList.remove('ru-open'); }
const NO_RU = 'Ru has no AI to think with. Install the Claude Code CLI or Codex and log in once, run Ollama, or pick an endpoint in the gear menu.';
function applyRu(label) {
  ru.label = label; document.body.classList.toggle('no-ru', !label);
  $('#ru-fab').title = label ? `Ask Ru, the wise one · via ${label}` : NO_RU;
  $('#ru-home').title = label ? `gu.Ru · ask before you delete · via ${label}` : NO_RU;
  $('#ru-home').querySelector('.say').textContent = label ? 'Ask before you let go. Ru weighs whatever you point at, looks closer if it must, and answers: delete, archive, or keep.' : 'Ru is greyed out: no AI found. Open the gear menu to pick one.';
  ruEl.querySelector('.ctx .lbl').textContent = label ? `via ${label}` : 'no AI';
}
$('#ru-fab').onclick = () => (ru.label ? ruOpen() : openSettings()); ruEl.querySelector('.x').onclick = ruClose;
$('#ru-home').onclick = () => { // Ru needs something to look at: go to the map if there is one, else ask for a disk first
  if (!ru.label) { openSettings(); return; }
  hog.returnTo = null;
  if (scanDone) backToMap().then(() => ruOpen()); else { toast('Ru: map a disk with Di first, then ask me about anything on it', 'ru'); $('#disk-setup').hidden = false; $('#path').focus(); }
};
// ---- the gear: which AI Ru thinks with. Saved in ~/.dime/settings.json, switched live.
const gearEl = $('#settings');
async function openSettings() {
  hideMenu(); let v; try { v = await api('/api/ru'); } catch (e) { toast(e.message); return; }
  const h = v.have;
  gearEl.innerHTML = `<div class="head"><b>Ru thinks with</b><button class="x" type="button" aria-label="Close">✕</button></div>
    <label><input type="radio" name="ru" value="auto"><span>Auto <small>· first of: Claude, Codex, an endpoint</small></span></label>
    <label><input type="radio" name="ru" value="claude"${h.claude ? '' : ' disabled'}><span>Claude Code CLI <small>· ${h.claude ? 'installed · read-only tool allowlist' : 'not found · claude.ai/code, then run claude once'}</small></span></label>
    <label><input type="radio" name="ru" value="codex"${h.codex ? '' : ' disabled'}><span>Codex CLI <small>· ${h.codex ? 'installed · sandboxed, no writes' : 'not found · npm i -g @openai/codex, then run codex once'}</small></span></label>
    <label><input type="radio" name="ru" value="api"><span>OpenAI-compatible endpoint <small>· no tools, judges from what Di shows${h.ollama ? ' · Ollama found' : ''}</small></span>
      <div class="api"><input name="url" placeholder="URL, e.g. http://127.0.0.1:11434/v1 or https://api.openai.com/v1" spellcheck="false"><input name="model" placeholder="model, e.g. qwen3:8b or gpt-4o-mini" spellcheck="false"><input name="key" type="password" placeholder="${h.key_set ? 'API key (set)' : 'API key, if the endpoint needs one'}"></div></label>
    <label><input type="radio" name="ru" value="none"><span>None <small>· Ru stays greyed out</small></span></label>
    <div class="now">Now: ${v.current ? esc(v.current) : 'no AI'}</div>
    <div class="foot"><button class="btn sm quiet" type="button">Cancel</button><button class="btn sm" type="button">Use this</button></div>`;
  gearEl.querySelector(`input[value="${v.mode}"]`)?.removeAttribute('disabled'); (gearEl.querySelector(`input[value="${v.mode}"]`) ?? gearEl.querySelector('input[value="auto"]')).checked = true;
  gearEl.querySelector('[name=url]').value = h.url; gearEl.querySelector('[name=model]').value = h.model;
  const apiBox = gearEl.querySelector('.api'), sync = () => apiBox.classList.toggle('on', gearEl.querySelector('input[name=ru]:checked')?.value === 'api'); sync();
  for (const r of gearEl.querySelectorAll('input[name=ru]')) r.onchange = sync;
  gearEl.querySelector('.x').onclick = () => gearEl.close(); gearEl.querySelector('.foot .quiet').onclick = () => gearEl.close();
  gearEl.querySelector('.foot .btn:not(.quiet)').onclick = async () => {
    const mode = gearEl.querySelector('input[name=ru]:checked')?.value ?? 'auto';
    try { const r = await api('/api/ru', { mode, url: gearEl.querySelector('[name=url]').value.trim(), model: gearEl.querySelector('[name=model]').value.trim(), key: gearEl.querySelector('[name=key]').value }); applyRu(r.current); toast(r.current ? `Ru: thinking with ${r.current}` : 'Ru: no AI, greyed out', 'ru'); gearEl.close(); }
    catch (e) { toast(`Settings: ${e.message}`); }
  };
  gearEl.showModal();
}
$('#gear').onclick = openSettings;
// ---- reset: forget the shortlist, the chat, hidden folders and remembered state; empty the shelf if asked; go home
const resetEl = $('#resetdlg');
$('#reset').onclick = async () => {
  hideMenu(); let v = []; try { v = await api('/api/shelf'); } catch {}
  const snaps = (await api('/api/status').catch(() => ({}))).snapshots?.length ?? 0;
  resetEl.innerHTML = `<div class="head">Start over</div><div class="sub">Clears your shortlist, Ru's chat, hidden folders and everything DiMe remembers for every scan, then goes home. The AI setting stays.</div>
    <label class="hot"><input type="checkbox" name="shelf"${v.length ? ' checked' : ''}><span><b>Empty the shelf for good</b> · ${v.length ? `${v.length} item${v.length === 1 ? '' : 's'} · ${fmt(sumOf(v))}` : 'empty'}<small>Everything on the shelf is removed from disk. Cannot be undone. Untick to keep it and put things back later.</small></span></label>
    <label><input type="checkbox" name="snapshots"><span>Forget last maps<small>${snaps ? `${snaps} saved map${snaps === 1 ? '' : 's'} reopen instantly; without them the next launch rescans.` : 'none saved'}</small></span></label>
    <div class="foot"><button class="btn sm quiet" type="button">Cancel</button><button class="btn sm hot" type="button">Reset</button></div>`;
  resetEl.querySelector('.quiet').onclick = () => resetEl.close();
  resetEl.querySelector('.foot .hot').onclick = async () => {
    const shelf = resetEl.querySelector('[name=shelf]').checked, snapshots = resetEl.querySelector('[name=snapshots]').checked;
    let r; try { r = await api('/api/reset', { shelf, snapshots }); } catch (e) { toast(`Reset: ${e.message}`); return; }
    resetEl.close();
    clearTimeout(stateTimer); stateLoaded = false;
    picked.clear(); hiddenPaths.clear(); openRows.clear(); kidCache.clear(); navHist.length = 0; tiersOn.clear(); for (const t of ['safe', 'likely', 'review']) tiersOn.add(t);
    filter = ''; idleDays = 0; syncFilterButtons();
    ru.turns.length = 0; ru.sel.clear(); ru.pinned = false; ruMsgs.innerHTML = ''; ruEl.querySelector('.clear').hidden = true; ruClose();
    shelfList = []; try { for (const k of Object.keys(localStorage)) if (k.startsWith('dime.')) localStorage.removeItem(k); } catch {}
    if (snapshots) renderSnaps([]);
    renderPickBar(); showLanding();
    toast(shelf && r.purged ? `Di: reset · ${r.purged} shelved item${r.purged === 1 ? '' : 's'} deleted for good` : 'Di: reset', 'du');
  };
  resetEl.showModal();
};
ruEl.querySelector('.clear').onclick = () => { if (ru.busy) return; ru.turns.length = 0; ru.pinned = false; ruMsgs.innerHTML = ''; ruEl.querySelector('.clear').hidden = true; ruCtxLine(); ruSuggest(); ruIn.focus(); };
ruEl.querySelector('form').onsubmit = (e) => { e.preventDefault(); if (ru.busy) ru.abort?.abort(); else ruAsk(ruIn.value); }; // the Ask button is Stop while Ru works
ruIn.onkeydown = (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); ruAsk(ruIn.value); } };
ruIn.oninput = () => { ruIn.style.height = 'auto'; ruIn.style.height = `${Math.min(120, ruIn.scrollHeight)}px`; };
// ---- drops: sidebar rows drag natively; map blocks are carried with press-and-hold (a native drag would fight the camera)
const DND = 'text/dime-path';
function dragSource(el, path) { el.draggable = true; el.addEventListener('dragstart', (e) => { e.dataTransfer.setData(DND, path); e.dataTransfer.effectAllowed = 'copy'; document.body.classList.add('carrying'); }); el.addEventListener('dragend', () => document.body.classList.remove('carrying')); }
for (const t of [ruEl, $('#ru-fab')]) {
  t.addEventListener('dragover', (e) => { if (e.dataTransfer.types.includes(DND)) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; t.classList.add('drop'); } });
  t.addEventListener('dragleave', () => t.classList.remove('drop'));
  t.addEventListener('drop', (e) => { e.preventDefault(); t.classList.remove('drop'); document.body.classList.remove('carrying'); const p = e.dataTransfer.getData(DND); if (p === null || p === undefined) return; ruTake(p); });
}
function ruTake(path, source = 'drop') { if (!ru.label) { openSettings(); return; } if (!ru.open) ruOpen('selection'); else { ru.area = 'selection'; ru.pinned = true; } ruSelAdd(path, { source }); ruShow(true); ruParcel(path); toast(`Ru: got ${path.split('/').pop() || rootName}`, 'ru'); }
const carry = { on: null, timer: null, ghost: $('#carry') };
renderer.domElement.addEventListener('pointerdown', (e) => {
  clearTimeout(carry.timer);
  if (e.button !== 0 || mode !== 'disk' || scanning || !hovered) return;
  const start = [e.clientX, e.clientY], n = hovered.userData.node, id = e.pointerId;
  carry.timer = setTimeout(() => {
    if (Math.hypot(mouse.px - start[0], mouse.py - start[1]) > 5) return;
    carry.on = { path: n.path, name: n.name, size: n.size }; controls.enabled = false; document.body.classList.add('carrying');
    renderer.domElement.setPointerCapture(id); carry.ghost.hidden = false; carry.ghost.textContent = `${n.name}${n.is_dir ? '/' : ''} · ${fmt(n.size)} · drop on Ru`; moveGhost(start[0], start[1]);
  }, 320);
});
const moveGhost = (x, y) => { carry.ghost.style.left = `${x + 14}px`; carry.ghost.style.top = `${y + 14}px`; const over = document.elementFromPoint(x, y)?.closest('#ru, #ru-fab'); for (const t of [ruEl, $('#ru-fab')]) t.classList.toggle('drop', !!over && (over === t)); };
renderer.domElement.addEventListener('pointermove', (e) => { mouse.px = e.clientX; mouse.py = e.clientY; if (carry.on) moveGhost(e.clientX, e.clientY); });
addEventListener('pointerup', (e) => {
  clearTimeout(carry.timer);
  if (!carry.on) return;
  const over = document.elementFromPoint(e.clientX, e.clientY)?.closest('#ru, #ru-fab');
  const it = carry.on; carry.on = null; controls.enabled = true; carry.ghost.hidden = true; document.body.classList.remove('carrying'); for (const t of [ruEl, $('#ru-fab')]) t.classList.remove('drop');
  try { renderer.domElement.releasePointerCapture(e.pointerId); } catch {}
  if (over) ruTake(it.path);
  downAt = null; // the hold was not a click
}, true);
// enough markdown for an answer: paragraphs, bullets, bold, code
function md(t) {
  const esc = (x) => x.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
  const inline = (x) => esc(x).replace(/`([^`]+)`/g, '<code>$1</code>').replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>').replace(/^_(.+)_$/, '<i>$1</i>');
  const out = []; let list = null;
  for (const raw of t.split('\n')) {
    const m = raw.match(/^\s*(?:[-*•]|\d+[.)])\s+(.*)/);
    if (m) { (list ??= []).push(`<li>${inline(m[1])}</li>`); continue; }
    if (list) { out.push(`<ul>${list.join('')}</ul>`); list = null; }
    if (raw.trim()) out.push(`<p>${inline(raw.replace(/^#+\s*/, ''))}</p>`);
  }
  if (list) out.push(`<ul>${list.join('')}</ul>`);
  return out.join('');
}
function ruAdd(who, text) { const m = document.createElement('div'); m.className = `m ${who}`; if (who === 'me') m.textContent = text; else m.innerHTML = md(text); ruMsgs.appendChild(m); ruMsgs.scrollTop = ruMsgs.scrollHeight; return m; }
// Ru does not just report what it is checking, it goes and hovers over it
let ruGaze = 0;
function ruGazeNext() {
  const keys = [...ru.sel.keys()].filter((k) => byKey.has(k));
  ruLookAt(keys.length ? keys[ruGaze++ % keys.length] : null);
}
const toolLine = (c) => { const t = tilde(String(c ?? '')); return (rootPath ? t.replaceAll(rootPath + '/', '') : t).slice(0, 90); };
// ---- verdicts: Ru ends an actionable answer with a ```dime JSON block; it becomes buttons instead of text
const splitVerdict = (t) => { const i = t.indexOf('```dime'); if (i < 0) return [t, null]; const j = t.indexOf('```', i + 7); let v = null; try { v = JSON.parse(t.slice(i + 7, j < 0 ? undefined : j)); } catch {} return [t.slice(0, i).trimEnd(), v]; };
function verdictRows(paths) {
  const rows = [], outside = [];
  for (const abs of [...new Set(paths ?? [])]) {
    const rel = relOf(abs); if (rel === null || rel === '') { outside.push(abs); continue; }
    const it = ru.sel.get(rel), c = gunkSet.get(rel) ?? [...ru.sel.values()].flatMap((x) => x.flagged).find((x) => x.path === rel), n = current?.children.find((x) => x.path === rel) ?? [...ru.sel.values()].flatMap((x) => x.kids).find((x) => x.path === rel);
    const size = it?.size ?? c?.size ?? n?.size ?? 0, is_dir = it?.is_dir ?? c?.is_dir ?? n?.is_dir ?? abs.endsWith('/');
    rows.push({ key: rel, name: rel.split('/').pop(), is_dir, size, sub: [parentOf(rel) || rootName, c ? c.what : size ? '' : 'size unknown'].filter(Boolean).join(' · ') });
  }
  return [rows, outside];
}
// Ru answers with {go, keep}. It never picks a destination: "go" only puts things on the shortlist,
// "keep" takes them back off it when Di put them there. Older replies split go into archive/delete.
const goPaths = (v) => [...new Set([...(v.go ?? []), ...(v.archive ?? []), ...(v.delete ?? [])])];
/// Fill in sizes for rows Di never flagged, so the shortlist does not show them as 0 B.
async function sizeUp(rows) {
  const need = rows.filter((r) => !r.size);
  for (const parent of new Set(need.map((r) => parentOf(r.key)))) {
    try {
      const t = await api(`/api/tree?path=${encodeURIComponent(parent)}&depth=1`);
      const by = new Map((t.children ?? []).map((c) => [c.path, c]));
      for (const r of need) { const c = by.get(r.key); if (c) { r.size = c.size; r.is_dir = c.is_dir; } }
    } catch {}
  }
}
function renderVerdict(m, v) {
  const box = document.createElement('div'); box.className = 'verdict';
  const groups = [['Can go', 'ru', goPaths(v)], ['Keep', 'quiet', v.keep ?? []]].filter(([, , paths]) => paths.length);
  if (!groups.length) return;
  for (const [label, cls, paths] of groups) { const g = document.createElement('div'); g.className = `vg ${cls}`; g.innerHTML = `<b></b><ul></ul>`; g.querySelector('b').textContent = `${label} · ${paths.length}`; for (const p of paths) { const li = document.createElement('li'); li.textContent = tilde(p); g.querySelector('ul').appendChild(li); } box.appendChild(g); }
  const acts = document.createElement('div'); acts.className = 'acts'; box.appendChild(acts);
  const mk = (label, cls, fn) => { const b = document.createElement('button'); b.type = 'button'; b.className = `btn sm ${cls}`; b.textContent = label; b.onclick = fn; acts.appendChild(b); };
  // One button for the whole verdict: what Ru says can go joins the shortlist, what it says to keep
  // leaves it. Only the items that would actually change are counted, so the button is never a no-op.
  const [goRows, outside] = verdictRows(goPaths(v)), [keepRows] = verdictRows(v.keep);
  for (const r of goRows) ruMark(r.key, 'go');
  for (const r of keepRows) ruMark(r.key, 'keep');
  const add = goRows.filter((r) => !picked.has(r.key));
  // A keep is only kept if nothing on the shortlist still covers it. Ru usually names folders inside
  // one that Di flagged whole, and leaving that parent listed would move the kept item along with it,
  // so the parent has to go and the items Ru cleared take its place.
  const covers = (entry, path) => entry === path || path.startsWith(entry + '/');
  const drop = [...picked.keys()].filter((k) => keepRows.some((r) => covers(k, r.key)));
  const label = `Modify shortlist · ${[add.length && `add ${add.length}`, drop.length && `drop ${drop.length}`].filter(Boolean).join(', ')}`;
  if (add.length || drop.length) mk(label, 'ru', async () => {
    for (const k of drop) picked.delete(k);
    await sizeUp(add); // Ru can name things Di never flagged, which arrive here with no size on them
    for (const r of add) picked.set(r.key, { path: r.key, name: r.name, size: r.size, is_dir: r.is_dir, tier: 'review', reason: 'ru', what: 'Ru said it can go', note: 'Ru checked this one.', age_days: 0 });
    if (current) renderCleanup(); else renderPickBar();
    toast(`Di: shortlist ${[add.length && `+${add.length}`, drop.length && `-${drop.length}`].filter(Boolean).join(' ')}`, 'du');
    if (picked.size) listDialog([...picked.values()].map(candRow));
  });
  else if (goRows.length || keepRows.length) { const n = document.createElement('div'); n.className = 'note'; n.textContent = 'Your shortlist already matches this.'; box.appendChild(n); }
  if (outside.length) { const o = document.createElement('div'); o.className = 'note'; o.textContent = `${outside.length} path${outside.length === 1 ? ' is' : 's are'} outside the scan root and cannot be acted on here.`; box.appendChild(o); }
  m.appendChild(box);
}
async function ruAsk(q) {
  q = q.trim(); if (!q || ru.busy) return;
  ru.busy = true; ruIn.value = ''; ruIn.style.height = 'auto';
  ru.abort = new AbortController(); ruSend.classList.add('stop'); ruSend.innerHTML = '<i></i>'; ruSend.title = 'Stop';
  ruAdd('me', q); ruSuggest();
  try { shelfList = await api('/api/shelf'); } catch {}
  ruCtxLine();
  const about = ruEl.querySelector('.focus .fs b')?.textContent ?? ruAreas().find((x) => x.id === ru.area)?.label ?? '', sentCtx = ruContext();
  const transcript = ru.turns.slice(-8).map((t) => `${t.who === 'me' ? 'User' : 'Ru'}: ${t.text}`).join('\n\n');
  const prompt = `What the user is looking at in DiMe right now:\n\n${sentCtx}\n\n${transcript ? `Conversation so far:\n\n${transcript}\n\n` : ''}User: ${q}`;
  const m = ruAdd('ru', ''), status = document.createElement('div'); status.className = 'status'; status.textContent = 'Ru is thinking'; m.appendChild(status);
  guru.busy = true; ruGaze = 0; ruGazeNext(); // the light quickens and it starts making its rounds
  let text = '', lastBlock = null, outOfTurns = false;
  const cap = () => { const c = document.createElement('div'); c.className = 'on'; c.textContent = `about ${about} · what Ru saw`; c.title = 'Show exactly what Ru was given for this answer'; c.onclick = () => { const p = c.nextElementSibling; if (p?.classList.contains('sent')) { p.remove(); return; } const pre = document.createElement('pre'); pre.className = 'sent'; pre.textContent = sentCtx; c.after(pre); }; return c; };
  const render = () => { m.innerHTML = md(splitVerdict(text)[0]); m.prepend(cap()); m.appendChild(status); ruMsgs.scrollTop = ruMsgs.scrollHeight; };
  const denied = [];
  const handle = (d) => {
    if (d.t === 'text') { text += d.d; render(); }
    else if (d.t === 'tool') { status.textContent = `Ru is checking ${toolLine(d.d)}`; ruGazeNext(); }
    else if (d.t === 'denied') denied.push(d.d);
    else if (d.t === 'provider') ruEl.querySelector('.ctx .lbl').textContent = `via ${d.d}`;
    else if (d.t === 'done') { if (d.turns) outOfTurns = true; if (d.error && !text.trim()) text = d.error; }
  };
  try {
    const r = await fetch('/api/ask', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ prompt }), signal: ru.abort.signal });
    if (!r.ok) throw new Error(await r.text());
    const reader = r.body.getReader(), dec = new TextDecoder(); let buf = '';
    for (;;) {
      const { value, done } = await reader.read(); if (done) break;
      buf += dec.decode(value, { stream: true });
      let i; while ((i = buf.indexOf('\n')) >= 0) { const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1); if (line) { try { handle(JSON.parse(line)); } catch {} } }
    }
  } catch (e) { text += `${text ? '\n\n' : ''}${e.name === 'AbortError' ? '_Stopped._' : `Ru lost the thread: ${e.message}`}`; }
  status.remove();
  if (denied.length) text += `\n\n_Not allowed to run: ${[...new Set(denied)].slice(0, 3).map((c) => `\`${c.slice(0, 240)}\``).join(' · ')}. Ru only gets read-only commands._`;
  const [shown, verdict] = splitVerdict(text);
  m.innerHTML = md(shown || 'Ru said nothing.'); m.prepend(cap()); if (verdict) renderVerdict(m, verdict); ruMsgs.scrollTop = ruMsgs.scrollHeight;
  ru.turns.push({ who: 'me', text: q }, { who: 'ru', text: shown }); ruEl.querySelector('.clear').hidden = false; // turns live for this session only
  guru.busy = false; ruLookAt(null); // back to drifting
  ru.busy = false; ru.abort = null; ruSend.classList.remove('stop'); ruSend.textContent = 'Ask'; ruSend.title = ''; ruIn.focus();
  if (outOfTurns && !verdict && !q.startsWith('Ru ran out of checks')) ruAsk('Ru ran out of checks before answering. Give the verdict now from what you found, no more checks.'); // one follow-up, with the transcript, so the work is not lost
}
