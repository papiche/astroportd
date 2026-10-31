'use strict';
// Table de tri des visages (module chargé à la demande) : chaque visage est une pastille sur une table lumineuse ;
// on les regroupe à la main ou au lasso, puis on les GLISSE sur la pochette d'une personne. Sans bibliothèque :
// canvas 2D, grappes en spirale de Vogel, disques empilés sans chevauchement, zoom et déplacement au pointeur.
// Les empreintes 512D ne quittent jamais la station : le serveur ne donne que des positions PCA et des groupes.
const R = 18, VOGEL = 1.1 * R, GOLDEN = 2.399963229728653, MAXZ = 4, CELL = 160;
const THUMB = [96, 192]; // vignettes demandées au serveur : petite, grande (visage affiché en grand)
let K, S, root, cv, cx, tip, tray, dirty = false, dpr = 1, colors = {};

const key = f => (f.named ? 'p:' + (f.pubkey || f.name) : f.group_id ? 'g:' + f.group_id : 's:' + f.id);
const css = v => { const s = document.createElement('span'); s.style.color = `var(${v})`; document.body.append(s); const c = getComputedStyle(s).color; s.remove(); return c; };
const loadColors = () => { colors = { accent: css('--accent'), ok: css('--ok'), warn: css('--warn'), line: css('--line'), panel: css('--panel'), ink: css('--ink'), muted: css('--muted') }; dirty = true; };
const initials = n => (n.startsWith('Inconnu') ? '?' : n.split(/\s+/).map(w => w[0]).join('').slice(0, 2).toUpperCase());

/* ── Mise en page : une grappe = un disque ─────────────────────────────────── */
function clusters(faces) {
  const by = new Map();
  for (const f of faces) { const k = key(f); (by.get(k) || by.set(k, { key: k, members: [], person: k[0] === 'p', group: k[0] === 'g' }).get(k)).members.push(f); }
  const out = [...by.values()];
  for (const c of out) { c.n = c.members.length; c.r = c.n === 1 ? R : VOGEL * Math.sqrt(c.n) + R * 1.05; }
  // Les plus nombreuses d'abord au centre ; les visages isolés, rangés par angle PCA, en couronne.
  out.sort((a, b) => b.n - a.n || (Math.atan2(a.members[0].y, a.members[0].x) - Math.atan2(b.members[0].y, b.members[0].x)) || (a.key < b.key ? -1 : 1));
  return out;
}
function layout() {
  const list = clusters(S.visible()), grid = new Map(), cell = c => `${Math.floor(c.x / CELL)},${Math.floor(c.y / CELL)}`;
  const free = (x, y, r) => {
    for (let gx = Math.floor((x - r) / CELL) - 1; gx <= Math.floor((x + r) / CELL) + 1; gx++) for (let gy = Math.floor((y - r) / CELL) - 1; gy <= Math.floor((y + r) / CELL) + 1; gy++)
      for (const o of grid.get(gx + ',' + gy) || []) if (Math.hypot(o.x - x, o.y - y) < o.r + r + 3) return false;
    return true;
  };
  const put = c => { const k = cell(c); (grid.get(k) || grid.set(k, []).get(k)).push(c); };
  let t0 = 0;
  const kept = new Map();
  for (const c of list) { const p = S.pos.get(c.key); if (p && free(p.x, p.y, c.r)) { c.x = p.x; c.y = p.y; put(c); kept.set(c.key, c); } }
  for (const c of list) {
    if (kept.has(c.key)) continue;
    for (let t = t0; ; t++) {
      const rad = 14 * Math.sqrt(t), x = rad * Math.cos(t * GOLDEN), y = rad * Math.sin(t * GOLDEN);
      if (free(x, y, c.r)) { c.x = x; c.y = y; t0 = Math.max(0, t - 40); break; }
    }
    put(c);
  }
  S.pos = new Map(list.map(c => [c.key, { x: c.x, y: c.y }]));
  S.clusters = list;
  S.nodes = [];
  S.hash = new Map();
  S.byId = new Map();
  for (const c of list) c.members.forEach((f, i) => {
    const r = c.n === 1 ? 0 : VOGEL * Math.sqrt(i + .5), a = i * GOLDEN, n = { f, c, x: c.x + r * Math.cos(a), y: c.y + r * Math.sin(a) };
    S.nodes.push(n); S.byId.set(f.id, n);
    const k = `${Math.floor(n.x / CELL)},${Math.floor(n.y / CELL)}`; (S.hash.get(k) || S.hash.set(k, []).get(k)).push(n);
  });
  dirty = true;
}
const world = (px, py) => ({ x: (px - S.tx) / S.s, y: (py - S.ty) / S.s });
const hitFace = (px, py) => {
  const w = world(px, py);
  let best = null, bd = R * R * 1.15;
  for (let gx = Math.floor((w.x - R) / CELL); gx <= Math.floor((w.x + R) / CELL); gx++) for (let gy = Math.floor((w.y - R) / CELL); gy <= Math.floor((w.y + R) / CELL); gy++)
    for (const n of S.hash.get(gx + ',' + gy) || []) { const d = (n.x - w.x) ** 2 + (n.y - w.y) ** 2; if (d < bd) { bd = d; best = n; } }
  return best;
};
const hitCluster = (px, py) => { const w = world(px, py); return S.clusters.find(c => c.n > 1 && Math.hypot(c.x - w.x, c.y - w.y) <= c.r); };

/* ── Dessin ────────────────────────────────────────────────────────────────── */
function fit() {
  if (!S.nodes.length) return;
  let x1 = 1e9, y1 = 1e9, x2 = -1e9, y2 = -1e9;
  for (const c of S.clusters) { x1 = Math.min(x1, c.x - c.r); y1 = Math.min(y1, c.y - c.r); x2 = Math.max(x2, c.x + c.r); y2 = Math.max(y2, c.y + c.r); }
  const w = cv.clientWidth, h = cv.clientHeight, s = Math.min(w / (x2 - x1 + 40), h / (y2 - y1 + 40), 1.2);
  S.s = s; S.tx = w / 2 - (x1 + x2) / 2 * s; S.ty = h / 2 - (y1 + y2) / 2 * s; dirty = true;
}
function focus(c) {
  const w = cv.clientWidth, h = cv.clientHeight, s = Math.min(w, h) / (2 * c.r + 60);
  S.s = Math.min(Math.max(s, .05), MAXZ); S.tx = w / 2 - c.x * S.s; S.ty = h / 2 - c.y * S.s; dirty = true;
}
function draw() {
  dirty = false;
  const w = cv.clientWidth, h = cv.clientHeight;
  cx.setTransform(dpr, 0, 0, dpr, 0, 0); cx.clearRect(0, 0, w, h);
  cx.setTransform(dpr * S.s, 0, 0, dpr * S.s, dpr * S.tx, dpr * S.ty);
  const v = { x1: -S.tx / S.s - R, y1: -S.ty / S.s - R, x2: (w - S.tx) / S.s + R, y2: (h - S.ty) / S.s + R }, px = R * S.s;
  for (const c of S.clusters) {
    if (c.n < 2 || c.x + c.r < v.x1 || c.x - c.r > v.x2 || c.y + c.r < v.y1 || c.y - c.r > v.y2) continue;
    cx.beginPath(); cx.arc(c.x, c.y, c.r, 0, 7);
    cx.globalAlpha = c.person ? .1 : .14; cx.fillStyle = c.person ? colors.ok : colors.accent; cx.fill();
    cx.globalAlpha = .55; cx.lineWidth = 1.2 / S.s; cx.strokeStyle = c.person ? colors.ok : colors.accent; cx.stroke(); cx.globalAlpha = 1;
    if (px * 1.2 > 12 && c.person) { cx.fillStyle = colors.ink; cx.font = `${13 / S.s}px system-ui`; cx.textAlign = 'center'; cx.fillText(`${c.members[0].name} · ${c.n}`, c.x, c.y - c.r - 4 / S.s); }
  }
  const want = [];
  for (const n of S.nodes) {
    if (n.x < v.x1 || n.x > v.x2 || n.y < v.y1 || n.y > v.y2) continue;
    const f = n.f, sel = S.sel.has(f.id);
    if (px < 3.2) { cx.fillStyle = sel ? colors.accent : f.named ? colors.ok : colors.muted; cx.fillRect(n.x - R * .5, n.y - R * .5, R, R); continue; }
    const big = px >= 30, img = (big && S.img[1].get(f.id)) || S.img[0].get(f.id);
    if (px >= 8 && (!img || (big && !S.img[1].get(f.id))) && !S.bad.has(f.id)) want.push(f.id);
    cx.beginPath(); cx.arc(n.x, n.y, R - 1, 0, 7);
    if (img && px >= 8) { cx.save(); cx.clip(); cx.drawImage(img, n.x - R, n.y - R, 2 * R, 2 * R); cx.restore(); } else { cx.fillStyle = colors.line; cx.fill(); if (px >= 10) { cx.fillStyle = colors.ink; cx.font = `${R * .9}px system-ui`; cx.textAlign = 'center'; cx.textBaseline = 'middle'; cx.fillText(initials(f.name), n.x, n.y + 1); } }
    cx.beginPath(); cx.arc(n.x, n.y, R - 1, 0, 7);
    cx.lineWidth = (sel ? 3.2 : 1.8) / Math.max(S.s, .6); cx.strokeStyle = sel ? colors.accent : f.suggest ? colors.warn : f.named ? colors.ok : colors.line; cx.stroke();
  }
  if (S.lassoPts.length > 1) {
    cx.setTransform(dpr, 0, 0, dpr, 0, 0); cx.beginPath(); S.lassoPts.forEach((p, i) => (i ? cx.lineTo(p.x, p.y) : cx.moveTo(p.x, p.y)));
    cx.closePath(); cx.globalAlpha = .12; cx.fillStyle = colors.accent; cx.fill(); cx.globalAlpha = 1; cx.setLineDash([6, 4]); cx.strokeStyle = colors.accent; cx.lineWidth = 1.5; cx.stroke(); cx.setLineDash([]);
  }
  fetchThumbs(want, px >= 30 ? 1 : 0);
}
const redraw = () => { if (!dirty) { dirty = true; } requestAnimationFrame(() => dirty && draw()); };

/* ── Vignettes (recadrées par la station, par lots) ────────────────────────── */
let timer = 0, queue = new Set(), qsize = 0;
function fetchThumbs(ids, level) {
  for (const id of ids) if (!S.pend[level].has(id)) queue.add(id);
  qsize = level;
  if (queue.size && !timer) timer = setTimeout(flush, 140);
}
async function flush() {
  timer = 0;
  const level = qsize, ids = [...queue].slice(0, 60); ids.forEach(i => { queue.delete(i); S.pend[level].add(i); });
  if (!ids.length) return;
  try {
    const r = await K.api('/api/v1/faces/thumbs', { method: 'POST', body: { ids, size: THUMB[level] } });
    for (const [id, url] of Object.entries(r.thumbs)) { const im = new Image(); im.onload = () => { S.img[level].set(+id, im); redraw(); }; im.src = url; }
    for (const [id] of r.failed) S.bad.add(id);
  } catch (e) { ids.forEach(i => S.bad.add(i)); if (!S.warned) { S.warned = true; K.toast('Vignettes indisponibles : ' + e.message); } }
  if (queue.size) timer = setTimeout(flush, 60);
}

/* ── Sélection et actions ──────────────────────────────────────────────────── */
function select(ids, add) { if (!add) S.sel.clear(); ids.forEach(i => S.sel.add(i)); trayDraw(); redraw(); }
function toggle(id) { S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); trayDraw(); redraw(); }
async function act(label, call) {
  try { const r = await call(); K.toast(label(r), 1); S.sel.clear(); await load(); } catch (e) { K.toast(e.message); }
}
const assign = (ids, name, pubkey) => act(r => `${r.assigned} visage(s) rangé(s) dans « ${name} »`, () => K.api('/api/v1/faces/assign', { method: 'POST', body: { ids, name, ...(pubkey && { pubkey }) } }));
const forget = ids => confirm(`Ne plus reconnaître ${ids.length} visage(s) ? Les photos restent dans votre uCloud.`) && act(r => `${r.forgotten} visage(s) oublié(s)`, () => K.api('/api/v1/faces/forget', { method: 'POST', body: { ids } }));
function ask(ids) {
  const d = root.querySelector('.pe-name'), inp = d.querySelector('input'), list = d.querySelector('datalist');
  list.replaceChildren(...S.people.map(p => K.h('option', { value: p.name })));
  d.querySelector('p').textContent = `${ids.length} visage(s) à ranger. Un nom déjà utilisé les ajoute à cette personne.`;
  d.onsubmit = ev => { if (ev.submitter?.value === 'ok' && inp.value.trim()) assign(ids, inp.value.trim(), S.people.find(p => p.name === inp.value.trim())?.pubkey); };
  inp.value = ''; d.showModal(); inp.focus();
}
const photo = id => { const f = S.byId.get(id)?.f; if (f) K.open([{ path: f.path, mime: 'image/jpeg', mtime: 0 }], 0); };

function trayDraw() {
  const ids = [...S.sel];
  tray.hidden = !ids.length;
  if (!ids.length) return;
  const pick = K.h('select', { 'aria-label': 'Personne' }, K.h('option', { value: '' }, 'Ranger dans…'), S.people.slice(0, 200).map(p => K.h('option', { value: p.name }, `${p.name} (${p.photos})`)));
  pick.onchange = () => { const p = S.people.find(x => x.name === pick.value); if (p) assign(ids, p.name, p.pubkey); };
  tray.replaceChildren(K.h('strong', {}, `${ids.length} visage(s)`), pick,
    K.h('button', { onclick: () => ask(ids) }, 'Nouvelle personne…'), K.h('button', { class: 'ghost', onclick: () => forget(ids) }, 'Pas à reconnaître'),
    ids.length === 1 && K.h('button', { class: 'ghost', onclick: () => photo(ids[0]) }, 'Voir la photo'), K.h('button', { class: 'ghost', onclick: () => select([]) }, 'Annuler'));
}

/* ── Pointeur : déplacer, zoomer, sélectionner, lasso, glisser vers une pochette ─ */
function pointer() {
  const ptrs = new Map();
  let g = null, ghost = null, over = null;
  const at = e => { const r = cv.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; };
  const zoom = (p, k) => { const s = Math.min(MAXZ, Math.max(.02, S.s * k)), w = world(p.x, p.y); S.s = s; S.tx = p.x - w.x * s; S.ty = p.y - w.y * s; redraw(); };
  cv.addEventListener('wheel', e => { e.preventDefault(); zoom(at(e), Math.exp(-e.deltaY * .0015)); }, { passive: false });
  cv.addEventListener('pointerdown', e => {
    cv.setPointerCapture(e.pointerId); const p = at(e); ptrs.set(e.pointerId, p);
    if (ptrs.size === 2) { const [a, b] = [...ptrs.values()]; g = { kind: 'pinch', d: Math.hypot(a.x - b.x, a.y - b.y) }; return; }
    const n = hitFace(p.x, p.y);
    g = { kind: S.lasso || e.shiftKey ? 'lasso' : n ? 'face' : 'pan', n, p, moved: false, add: e.ctrlKey || e.metaKey || e.shiftKey };
    if (g.kind === 'lasso') S.lassoPts = [p];
  });
  cv.addEventListener('pointermove', e => {
    const p = at(e);
    if (!g) { const n = hitFace(p.x, p.y); cv.style.cursor = n ? 'grab' : 'default'; tip.hidden = !n; if (n) { tip.textContent = n.f.suggest ? `Est-ce ${n.f.suggest.name} ? ${Math.round(n.f.suggest.score * 100)} %` : n.f.named ? n.f.name : 'À ranger'; tip.style.left = e.clientX + 14 + 'px'; tip.style.top = e.clientY + 14 + 'px'; } return; }
    if (!ptrs.has(e.pointerId)) return;
    const prev = ptrs.get(e.pointerId); ptrs.set(e.pointerId, p);
    if (g.kind === 'pinch' && ptrs.size === 2) { const [a, b] = [...ptrs.values()], d = Math.hypot(a.x - b.x, a.y - b.y); zoom({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }, d / g.d); g.d = d; return; }
    if (Math.hypot(p.x - g.p.x, p.y - g.p.y) > 6) g.moved = true;
    if (g.kind === 'pan' && g.moved) { S.tx += p.x - prev.x; S.ty += p.y - prev.y; redraw(); }
    else if (g.kind === 'lasso') { S.lassoPts.push(p); redraw(); }
    else if (g.kind === 'face' && g.moved) {
      if (!ghost) { if (!S.sel.has(g.n.f.id)) select([g.n.f.id], g.add); ghost = K.h('div', { class: 'pe-ghost' }, String(S.sel.size)); document.body.append(ghost); }
      ghost.style.left = e.clientX + 'px'; ghost.style.top = e.clientY + 'px';
      const t = document.elementFromPoint(e.clientX, e.clientY)?.closest('[data-drop]');
      if (t !== over) { over?.classList.remove('over'); t?.classList.add('over'); over = t; }
    }
  });
  const end = e => {
    ptrs.delete(e.pointerId);
    if (g?.kind === 'pinch') { if (ptrs.size < 2) g = null; return; }
    if (!g || ptrs.size) return;
    const p = at(e);
    if (g.kind === 'lasso') {
      if (S.lassoPts.length > 2) { const poly = S.lassoPts.map(q => world(q.x, q.y)), inside = (x, y) => { let c = false; for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) if ((poly[i].y > y) !== (poly[j].y > y) && x < (poly[j].x - poly[i].x) * (y - poly[i].y) / (poly[j].y - poly[i].y) + poly[i].x) c = !c; return c; }; select(S.nodes.filter(n => inside(n.x, n.y)).map(n => n.f.id), g.add); }
      S.lassoPts = []; redraw();
    } else if (g.kind === 'face') {
      if (ghost) {
        const t = over, ids = [...S.sel]; ghost.remove(); ghost = null; over?.classList.remove('over'); over = null;
        if (t) { const kind = t.dataset.drop; if (kind === 'person') assign(ids, t.dataset.name, t.dataset.key || undefined); else if (kind === 'new') ask(ids); else if (kind === 'trash') forget(ids); }
      } else toggle(g.n.f.id);
    } else if (g.kind === 'pan' && !g.moved) { const c = hitCluster(p.x, p.y); if (c) select(c.members.map(m => m.id), g.add); else if (!g.add) select([]); }
    g = null;
  };
  cv.addEventListener('pointerup', end); cv.addEventListener('pointercancel', end);
  cv.addEventListener('pointerleave', () => { tip.hidden = true; });
  cv.addEventListener('dblclick', e => { const p = at(e), n = hitFace(p.x, p.y); if (n) photo(n.f.id); });
  cv.addEventListener('keydown', e => {
    const k = { '+': () => zoom({ x: cv.clientWidth / 2, y: cv.clientHeight / 2 }, 1.3), '-': () => zoom({ x: cv.clientWidth / 2, y: cv.clientHeight / 2 }, 1 / 1.3), ArrowLeft: () => { S.tx += 60; redraw(); }, ArrowRight: () => { S.tx -= 60; redraw(); }, ArrowUp: () => { S.ty += 60; redraw(); }, ArrowDown: () => { S.ty -= 60; redraw(); }, Escape: () => select([]), 0: () => { fit(); redraw(); } }[e.key];
    if (k) { e.preventDefault(); k(); }
  });
}

/* ── Pochettes, groupes à ranger, suggestions « est-ce X ? » ───────────────── */
function side() {
  const box = root.querySelector('.pe-side'), q = box.querySelector('input').value.trim().toLowerCase();
  const list = box.querySelector('.pe-people');
  list.replaceChildren(...S.people.filter(p => !q || p.name.toLowerCase().includes(q)).slice(0, 200).map(p => {
    const f = S.faces.find(x => x.named && (x.pubkey || x.name) === (p.pubkey || p.name)), b = K.h('button', { class: 'pe-pochette', 'data-drop': 'person', 'data-name': p.name, 'data-key': p.pubkey || '', title: 'Déposer ici pour ranger · cliquer pour sélectionner ses visages', onclick: () => { select(S.faces.filter(x => x.named && (x.pubkey || x.name) === (p.pubkey || p.name)).map(x => x.id)); if (S.view !== 'all') setView('all'); const n = S.byId.get(f?.id); if (n) focus(n.c); } },
      K.h('strong', {}, p.name), K.h('span', { class: 'muted' }, `${p.photos} photo(s)`));
    return b;
  }));
  const todo = S.clusters.filter(c => !c.person).slice(0, 30);
  root.querySelector('.pe-todo').replaceChildren(...todo.map(c => K.h('button', { class: 'ghost', 'aria-pressed': String(c.members.every(m => S.sel.has(m.id))), onclick: () => { select(c.members.map(m => m.id)); focus(c); } }, c.n > 1 ? `Groupe de ${c.n} visages` : `Visage ${c.members[0].id}`)),
    S.clusters.filter(c => !c.person).length > 30 && K.h('p', { class: 'muted' }, `… et ${S.clusters.filter(c => !c.person).length - 30} autre(s) sur la table.`));
}
/* Envois de photos en attente : un clic par ami (visage flou, de profil ou ressemblance incertaine). */
async function shares() {
  const box = root.querySelector('.pe-shares'), r = await K.api('/api/v1/faces/pending-shares');
  const reply = (b, accept) => act(x => accept ? `${x.sent} photo(s) partagée(s) avec ${b.name}` : `Rien n'est envoyé à ${b.name}`, () => K.api('/api/v1/faces/pending-shares/confirm', { method: 'POST', body: { recipient_hex: b.recipient_hex, accept } }));
  box.replaceChildren(...r.pending.map(b => K.h('div', { class: 'pe-share' },
    K.h('p', {}, K.h('strong', {}, 'Partage de photos : '), `${b.count} photo(s) où ${b.name} apparaît semblent prêtes à lui être envoyées.`),
    K.h('button', { onclick: () => reply(b, true) }, `Partager avec ${b.name} en 1 clic`), ' ',
    K.h('button', { class: 'ghost', onclick: () => reply(b, false) }, 'Ignorer'))));
}
function maybe() {
  const box = root.querySelector('.pe-maybe'), s = S.faces.find(f => f.suggest && !f.named && !S.no.has(f.id));
  if (!s) return box.replaceChildren();
  const img = K.h('img', { alt: 'visage à confirmer', width: 64, height: 64 });
  K.api('/api/v1/faces/thumbs', { method: 'POST', body: { ids: [s.id], size: 96 } }).then(r => { if (r.thumbs[s.id]) img.src = r.thumbs[s.id]; }).catch(() => {});
  box.replaceChildren(img, K.h('div', {}, K.h('p', {}, 'Est-ce ', K.h('strong', {}, s.suggest.name), ` ? (${Math.round(s.suggest.score * 100)} % de ressemblance)`),
    K.h('button', { onclick: () => act(() => `Photo associée à ${s.suggest.name}`, () => K.api(`/api/v1/faces/${s.id}/accept`, { method: 'POST' })) }, `Oui, c'est ${s.suggest.name}`), ' ',
    K.h('button', { class: 'ghost', onclick: () => { S.no.add(s.id); maybe(); } }, 'Non, quelqu\'un d\'autre')));
}
function setView(v) {
  S.view = v; root.querySelectorAll('.pe-views button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.v === v)));
  layout(); side(); fit(); redraw();
}

/* ── Chargement ────────────────────────────────────────────────────────────── */
async function load() {
  const r = await K.api('/api/v1/faces/board');
  if (!r.enabled) { root.replaceChildren(K.h('p', { class: 'muted' }, 'La reconnaissance des visages n\'est pas activée sur cette station (le Capitaine l\'active dans Options).')); return false; }
  K.dav(r.dav);
  S.faces = r.faces; S.people = r.people; S.byId = new Map(); S.sel = new Set([...S.sel].filter(i => r.faces.some(f => f.id === i)));
  S.visible = () => (S.view === 'all' ? S.faces : S.faces.filter(f => !f.named));
  root.querySelector('.pe-count').textContent = `${r.unknown} à ranger · ${r.people.length} personne(s)${r.truncated ? ' · catalogue tronqué aux plus récents' : ''}${r.groups_skipped ? ' · groupes non calculés (trop de visages)' : ''}`;
  root.querySelector('[data-v=todo]').textContent = `À ranger (${r.unknown})`; root.querySelector('[data-v=all]').textContent = `Tous (${r.faces.length})`;
  const first = !S.nodes.length; layout(); side(); maybe(); shares().catch(() => {}); trayDraw(); if (first) fit(); redraw();
  root.querySelector('.pe-empty').hidden = !!r.faces.length;
  return true;
}

export default async function people(el, kit) {
  K = kit; root = el;
  S = { faces: [], people: [], nodes: [], clusters: [], pos: new Map(), byId: new Map(), hash: new Map(), sel: new Set(), view: 'todo', s: 1, tx: 0, ty: 0, img: [new Map(), new Map()], pend: [new Set(), new Set()], bad: new Set(), no: new Set(), lasso: false, lassoPts: [], visible: () => [] };
  const h = K.h, link = h('link', { rel: 'stylesheet', href: '/ui/m-people.css' });
  document.head.append(link); K.cleanup(() => link.remove());
  cv = h('canvas', { class: 'pe-board', tabindex: 0, role: 'application', 'aria-label': 'Table de tri des visages. Flèches : déplacer, plus et moins : zoom, zéro : tout voir. Utilisez la liste « À ranger » pour un tri au clavier.' });
  tip = h('div', { class: 'pe-tip', hidden: true });
  tray = h('div', { class: 'pe-tray', role: 'region', 'aria-label': 'Visages sélectionnés', hidden: true });
  root.replaceChildren(h('div', { class: 'pe' },
    h('div', { class: 'bar' }, h('h2', {}, 'Table de tri'), h('span', { class: 'pe-count muted' }, ''), h('span', { class: 'sp' }),
      h('span', { class: 'pe-views' }, h('button', { 'data-v': 'todo', class: 'ghost', 'aria-pressed': 'true', onclick: () => setView('todo') }, 'À ranger'), h('button', { 'data-v': 'all', class: 'ghost', 'aria-pressed': 'false', onclick: () => setView('all') }, 'Tous')),
      h('button', { class: 'ghost', 'aria-pressed': 'false', onclick: ev => { S.lasso = !S.lasso; ev.target.setAttribute('aria-pressed', String(S.lasso)); } }, 'Lasso'),
      h('button', { class: 'ghost', onclick: () => { fit(); redraw(); } }, 'Tout voir')),
    h('div', { class: 'pe-maybe card' }), h('div', { class: 'pe-shares' }),
    h('div', { class: 'pe-main' }, h('div', { class: 'pe-stage' }, cv, h('p', { class: 'pe-empty muted', hidden: true }, 'Aucun visage pour le moment : déposez des photos dans votre uCloud, la station les analyse.')),
      h('aside', { class: 'pe-side card' }, h('h3', {}, 'Pochettes'),
        h('button', { class: 'pe-pochette new', 'data-drop': 'new' }, '＋ Nouvelle personne'), h('button', { class: 'pe-pochette trash', 'data-drop': 'trash' }, '✕ Pas à reconnaître'),
        h('input', { type: 'search', placeholder: 'Chercher une personne', 'aria-label': 'Chercher une personne', oninput: () => side() }), h('div', { class: 'pe-people' }),
        h('h3', {}, 'À ranger'), h('div', { class: 'pe-todo' }))),
    tray, h('dialog', { class: 'pe-name', 'aria-labelledby': 'pe-n' }, h('form', { method: 'dialog' }, h('h2', { id: 'pe-n' }, 'Qui est-ce ?'), h('p', { class: 'muted' }), h('input', { 'list-id': 'pe-l', required: true, maxLength: 80, 'aria-label': 'Nom de la personne' }), h('datalist', { id: 'pe-l' }), h('menu', {}, h('button', { class: 'ghost', value: 'no' }, 'Annuler'), h('button', { value: 'ok' }, 'Ranger'))))), tip);
  root.querySelector('input[list-id]').setAttribute('list', 'pe-l'); // `list` est en lecture seule côté DOM
  cx = cv.getContext('2d');
  const size = () => { dpr = window.devicePixelRatio || 1; cv.width = Math.round(cv.clientWidth * dpr); cv.height = Math.round(cv.clientHeight * dpr); redraw(); };
  const ro = new ResizeObserver(size); ro.observe(cv); K.cleanup(() => ro.disconnect());
  K.cleanup(() => { clearTimeout(timer); timer = 0; queue.clear(); tip.remove(); document.querySelectorAll('.pe-ghost').forEach(g => g.remove()); });
  const mq = matchMedia('(prefers-color-scheme: dark)'); mq.onchange = loadColors; K.cleanup(() => { mq.onchange = null; });
  loadColors(); pointer(); size();
  try { await load(); } catch (e) { root.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
  K.every(60000, () => { if (!S.sel.size) load().catch(() => {}); });
}
