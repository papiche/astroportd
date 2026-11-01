'use strict';
// Carte du monde de vos photos géolocalisées, sans aucun serveur de tuiles : le fond est un contour embarqué, les photos sont regroupées par
// la station (jamais 30 000 points dans le navigateur). Molette ou boutons pour zoomer, glisser pour se déplacer, clic sur un groupe pour voir ses photos.
import { WORLD } from './m-world.js';

const day = 86400, MAX_S = 6000;
// Couleur résolue d'un jeton du thème (rgb) : le canvas ne sait pas lire `color-mix()` ni les variables telles quelles.
const css = n => { const s = document.createElement('span'); s.style.color = `var(${n})`; document.body.append(s); const c = getComputedStyle(s).color; s.remove(); return c; };

export default function mapView(K, grid) {
  const { h, t } = K, A = '/api/v1/ucloud/map';
  const land = new Path2D(WORLD);
  const cv = h('canvas', { class: 'map', role: 'img', 'aria-label': t('Carte des photos') }), cx = cv.getContext('2d');
  const info = h('p', { class: 'muted', role: 'status' }), histo = h('div', { class: 'histo', role: 'group', 'aria-label': t('Photos géolocalisées par mois') });
  const from = h('input', { type: 'date', 'aria-label': t('Du') }), to = h('input', { type: 'date', 'aria-label': t('Au') });
  const out = h('div', {});
  let col = null, w = 800, hgt = 440, v = { lon: 10, lat: 25, s: 2 }, pts = [], cell = 5, timer = 0, token = 0, picked = null, months = [], dragging = null;
  const utc = x => (x ? Date.parse(x + 'T00:00:00Z') / 1000 : null);
  const X = lon => (lon - v.lon) * v.s + w / 2, Y = lat => (v.lat - lat) * v.s + hgt / 2;
  const lonAt = x => (x - w / 2) / v.s + v.lon, latAt = y => v.lat - (y - hgt / 2) / v.s;
  const fit = () => { w = Math.max(280, Math.min(cv.parentElement?.clientWidth || 800, 1200)); hgt = Math.round(Math.min(560, Math.max(260, w * .55))); const d = devicePixelRatio || 1; cv.width = w * d; cv.height = hgt * d; cv.style.width = w + 'px'; cv.style.height = hgt + 'px'; cx.setTransform(d, 0, 0, d, 0, 0); };
  const clamp = () => { v.s = Math.max(w / 360, Math.min(MAX_S, v.s)); v.lat = Math.max(-85, Math.min(85, v.lat)); v.lon = Math.max(-180, Math.min(180, v.lon)); };

  function draw() {
    const { bg, ink, line, accent, panel } = (col ||= { bg: css('--panel'), ink: css('--ink'), line: css('--line'), accent: css('--accent'), panel: css('--bg') });
    cx.fillStyle = bg; cx.fillRect(0, 0, w, hgt);
    cx.save(); cx.setTransform((devicePixelRatio || 1) * v.s / 2, 0, 0, (devicePixelRatio || 1) * v.s / 2, (devicePixelRatio || 1) * X(-180), (devicePixelRatio || 1) * Y(90));
    cx.fillStyle = line; cx.globalAlpha = .55; cx.fill(land); cx.restore(); cx.globalAlpha = 1;
    cx.strokeStyle = line; cx.lineWidth = .5; cx.globalAlpha = .5;
    for (let g = -180; g <= 180; g += 30) { cx.beginPath(); cx.moveTo(X(g), 0); cx.lineTo(X(g), hgt); cx.stroke(); }
    for (let g = -60; g <= 60; g += 30) { cx.beginPath(); cx.moveTo(0, Y(g)); cx.lineTo(w, Y(g)); cx.stroke(); }
    cx.globalAlpha = 1;
    for (const p of pts) {
      const x = X(p.lon), y = Y(p.lat), r = 6 + 3.2 * Math.log2(p.n);
      if (x < -r || y < -r || x > w + r || y > hgt + r) continue;
      cx.beginPath(); cx.arc(x, y, r, 0, 7); cx.globalAlpha = p === picked ? .95 : .72; cx.fillStyle = accent; cx.fill(); cx.globalAlpha = 1;
      cx.lineWidth = p === picked ? 3 : 1.5; cx.strokeStyle = p === picked ? ink : panel; cx.stroke();
      if (p.n > 1 && r >= 9) { cx.fillStyle = panel; cx.font = '600 11px system-ui'; cx.textAlign = 'center'; cx.textBaseline = 'middle'; cx.fillText(p.n > 999 ? Math.round(p.n / 100) / 10 + 'k' : String(p.n), x, y); }
    }
  }

  const period = () => ({ from: utc(from.value), to: utc(to.value) != null ? utc(to.value) + day : null });
  function query(extra = {}) {
    const q = new URLSearchParams(), p = period();
    if (p.from != null) q.set('from', p.from);
    if (p.to != null) q.set('to', p.to);
    Object.entries(extra).forEach(([k, val]) => q.set(k, val));
    return q;
  }
  async function load() {
    col = null; // le thème a pu changer
    const me = ++token, south = Math.max(-90, latAt(hgt)), north = Math.min(90, latAt(0)), west = Math.max(-180, lonAt(0)), east = Math.min(180, lonAt(w));
    cell = Math.max(.01, 38 / v.s);
    try {
      const r = await K.api(`${A}?${query({ bbox: [south, west, north, east].map(n => n.toFixed(4)).join(','), cell: cell.toFixed(4) })}`);
      if (me !== token) return;
      pts = r.points; months = r.months; picked = null;
      info.textContent = !r.geotagged ? t('Aucune photo géolocalisée : la position vient des données EXIF des photos (GPS de l\'appareil).') : t('{0} photo(s) géolocalisée(s) sur {1} · {2} groupe(s) dans cette vue', r.geotagged, r.photos, pts.length);
      drawHisto(); draw();
    } catch (e) { K.toast(e.message); }
  }
  const later = () => { clearTimeout(timer); timer = setTimeout(load, 220); };

  // Histogramme par mois : un clic choisit le mois, un second clic le retire.
  function drawHisto() {
    const max = Math.max(1, ...months.map(m => m.n));
    histo.replaceChildren(...months.map(m => {
      const start = Date.UTC(+m.ym.slice(0, 4), +m.ym.slice(5) - 1) / 1000, end = Date.UTC(+m.ym.slice(0, 4), +m.ym.slice(5)) / 1000 - day;
      const iso = s => new Date(s * 1000).toISOString().slice(0, 10), on = from.value === iso(start) && to.value === iso(end);
      return h('button', { class: 'bar-m' + (on ? ' on' : ''), title: `${m.ym} · ${m.n}`, 'aria-label': `${m.ym} : ${m.n}`, 'aria-pressed': String(on), style: `height:${Math.max(6, Math.round(m.n / max * 100))}%`,
        onclick: () => { if (on) { from.value = ''; to.value = ''; } else { from.value = iso(start); to.value = iso(end); } load(); } });
    }));
  }
  const preset = (label, days) => h('button', { class: 'ghost', onclick: () => { const end = new Date(); to.value = days ? end.toISOString().slice(0, 10) : ''; from.value = days ? new Date(end - days * day * 1000).toISOString().slice(0, 10) : ''; load(); } }, label);
  from.onchange = to.onchange = () => load();

  async function show(p) {
    picked = p; draw();
    const half = Math.max(cell, .02) * .75, south = Math.max(-90, p.lat - half), north = Math.min(90, p.lat + half), west = Math.max(-180, p.lon - half), east = Math.min(180, p.lon + half);
    try {
      const r = await K.api(`${A}?${query({ items: 1, bbox: [south, west, north, east].join(',') })}`);
      const list = [], g = grid(r.items, list);
      out.replaceChildren(h('h3', {}, t('{0} photo(s) ici', p.n) + (p.n > r.items.length ? ' · ' + t('les {0} plus récentes', r.items.length) : '')), g);
      out.scrollIntoView({ block: 'nearest' });
    } catch (e) { K.toast(e.message); }
  }
  const hit = ev => { const b = cv.getBoundingClientRect(), x = ev.clientX - b.left, y = ev.clientY - b.top; return pts.filter(p => Math.hypot(X(p.lon) - x, Y(p.lat) - y) <= 6 + 3.2 * Math.log2(p.n) + 3).sort((a, b2) => b2.n - a.n)[0]; };
  const zoomAt = (f, x = w / 2, y = hgt / 2) => { const lo = lonAt(x), la = latAt(y); v.s *= f; clamp(); v.lon = lo - (x - w / 2) / v.s; v.lat = la + (y - hgt / 2) / v.s; clamp(); draw(); later(); };
  cv.addEventListener('wheel', ev => { ev.preventDefault(); const b = cv.getBoundingClientRect(); zoomAt(ev.deltaY < 0 ? 1.4 : 1 / 1.4, ev.clientX - b.left, ev.clientY - b.top); }, { passive: false });
  cv.addEventListener('pointerdown', ev => { cv.setPointerCapture(ev.pointerId); dragging = { x: ev.clientX, y: ev.clientY, lon: v.lon, lat: v.lat, moved: false }; });
  cv.addEventListener('pointermove', ev => {
    if (!dragging) { cv.style.cursor = hit(ev) ? 'pointer' : 'grab'; return; }
    const dx = ev.clientX - dragging.x, dy = ev.clientY - dragging.y;
    if (Math.abs(dx) + Math.abs(dy) > 4) dragging.moved = true;
    if (dragging.moved) { v.lon = dragging.lon - dx / v.s; v.lat = dragging.lat + dy / v.s; clamp(); draw(); }
  });
  cv.addEventListener('pointerup', ev => { const d = dragging; dragging = null; if (!d) return; if (d.moved) later(); else { const p = hit(ev); if (p) show(p); } });
  cv.addEventListener('dblclick', ev => { const b = cv.getBoundingClientRect(); zoomAt(2, ev.clientX - b.left, ev.clientY - b.top); });
  new ResizeObserver(() => { fit(); clamp(); draw(); }).observe(cv.parentElement || document.body);

  const box = h('div', {}, h('div', { class: 'bar' }, preset(t('Tout'), 0), preset(t('12 mois'), 365), preset(t('30 jours'), 30), from, to,
    h('button', { class: 'ghost', 'aria-label': t('Zoom avant'), onclick: () => zoomAt(2) }, '＋'), h('button', { class: 'ghost', 'aria-label': t('Zoom arrière'), onclick: () => zoomAt(.5) }, '－'),
    h('button', { class: 'ghost', onclick: () => { v = { lon: 10, lat: 25, s: w / 360 }; clamp(); load(); } }, t('Monde'))), histo, cv, info, out);
  queueMicrotask(() => { fit(); v.s = w / 360 * 1.1; clamp(); draw(); load(); });
  return box;
}
