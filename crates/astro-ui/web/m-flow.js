'use strict';
// Carte animée d'une UPlanet (SVG seul, CSP stricte). Disposition = la boucle circulaire de ZEN.ECONOMY v3, en sens horaire
// depuis le Ğ1 maître en haut : CASH → NODE → membres → HOSTING → IMPOT → RnD/ASSETS → SOCIETY → CAPITAL/AMORTISSEMENT.
// Extérieur en haut à droite (près de CASH) ; INTRUSION et les intrus (vue du Capitaine) à part, en bas à droite.
const NS = 'http://www.w3.org/2000/svg';
export const W = 960, H = 520;
const el = (tag, attrs = {}, ...kids) => { const e = document.createElementNS(NS, tag); for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v); e.append(...kids); return e; };
const RING = ['UPLANETNAME_G1', 'TREASURY', 'NODE', 'membres', 'HOSTING', 'IMPOT', 'RND', 'ASSETS', 'SOCIETY', 'CAPITAL', 'AMORTISSEMENT', 'UPLANETNAME'];
export const short = a => a.length > 16 ? `${a.slice(0, 6)}…${a.slice(-4)}` : a;
export const label = n => n.kind === 'intrus' ? short(n.id) : n.id === 'UPLANETNAME_G1' ? 'Ğ1 maître' : n.id === 'TREASURY' ? 'CASH' : n.id.replace('UPLANETNAME_', '');

export function layout(nodes) {
  const rank = i => { const r = RING.indexOf(nodes[i].id); return r < 0 ? 50 + i : r; };
  const ring = nodes.map((_, i) => i).filter(i => nodes[i].kind === 'members' || (nodes[i].kind === 'wallet' && nodes[i].id !== 'INTRUSION')).sort((a, b) => rank(a) - rank(b));
  const intr = nodes.map((_, i) => i).filter(i => nodes[i].kind === 'intrus');
  return nodes.map((n, i) => {
    if (n.kind === 'billets') return { x: 100, y: H - 50 };
    if (n.kind === 'outside') return { x: W - 70, y: 70 };
    if (n.id === 'INTRUSION') return { x: W - 110, y: H - 45 };
    if (n.kind === 'intrus') { const k = intr.indexOf(i); return { x: W - 40 - 70 * Math.floor(k / 6), y: H - 95 - 34 * (k % 6) }; }
    const a = -Math.PI / 2 + (2 * Math.PI * ring.indexOf(i)) / Math.max(1, ring.length);
    return { x: W / 2 - 40 + 300 * Math.cos(a), y: H / 2 + 200 * Math.sin(a) };
  });
}

// Les tuyaux : un par paire de nœuds qui ont échangé, avec le volume total (centimes de Ğ1).
export function pipes(events) {
  const m = new Map();
  for (const [, f, to, c] of events) {
    if (f === to) continue;
    const k = Math.min(f, to) + '-' + Math.max(f, to), p = m.get(k) || { a: Math.min(f, to), b: Math.max(f, to), total: 0 };
    p.total += c; m.set(k, p);
  }
  return [...m.values()];
}

// Soldes rejoués jusqu'à l'instant `at` (secondes) : somme des virements, sans rien d'autre.
export function balancesAt(events, count, at) {
  const bal = new Array(count).fill(0);
  for (const [t, f, to, c] of events) { if (t > at) break; bal[f] -= c; bal[to] += c; }
  return bal;
}

export function flow(host, { nodes, events }, { reduced = false, onEvent = null } = {}) {
  const pos = layout(nodes), svg = el('svg', { viewBox: `0 0 ${W} ${H}`, role: 'img', class: 'fl paused', 'aria-label': 'Carte animée des virements de la coopérative' });
  const defs = el('defs', {}, el('filter', { id: 'fl-glow', x: '-50%', y: '-50%', width: '200%', height: '200%' }, el('feGaussianBlur', { stdDeviation: '3', result: 'b' }), el('feMerge', {}, el('feMergeNode', { in: 'b' }), el('feMergeNode', { in: 'SourceGraphic' }))),
    el('pattern', { id: 'fl-grid', width: 40, height: 40, patternUnits: 'userSpaceOnUse' }, el('path', { d: 'M40 0H0V40', class: 'fl-gridl' })));
  const pipeLayer = el('g', {}), pulseLayer = el('g', { filter: 'url(#fl-glow)' }), nodeLayer = el('g', { filter: 'url(#fl-glow)' }), labelLayer = el('g', {});
  svg.append(defs, el('rect', { width: W, height: H, class: 'fl-bg' }), el('rect', { width: W, height: H, fill: 'url(#fl-grid)' }), pipeLayer, pulseLayer, nodeLayer, labelLayer);
  const paths = new Map(), maxVol = Math.max(1, ...pipes(events).map(p => p.total));
  const curve = (a, b) => { const A = pos[a], B = pos[b], mx = (A.x + B.x) / 2 + (W / 2 - (A.x + B.x) / 2) * .22, my = (A.y + B.y) / 2 + (H / 2 - (A.y + B.y) / 2) * .22; return `M${A.x.toFixed(1)} ${A.y.toFixed(1)}Q${mx.toFixed(1)} ${my.toFixed(1)} ${B.x.toFixed(1)} ${B.y.toFixed(1)}`; };
  for (const p of pipes(events)) {
    const d = curve(p.a, p.b), w = 1 + 3.5 * Math.sqrt(p.total / maxVol);
    const pipe = el('path', { d, class: 'fl-pipe', 'stroke-width': w });
    pipeLayer.append(el('path', { d, class: 'fl-pipe-bed', 'stroke-width': w + 4 }), pipe);
    paths.set(p.a + '-' + p.b, { hit: pipe, a: p.a });
  }
  const dots = nodes.map((n, i) => {
    const c = el('circle', { cx: pos[i].x, cy: pos[i].y, r: 8, class: `fl-n fl-${n.kind}${i === 0 ? ' fl-master' : ''}${n.id === 'INTRUSION' ? ' fl-intrus' : ''}` });
    nodeLayer.append(c);
    labelLayer.append(el('text', { x: pos[i].x, y: pos[i].y + (pos[i].y > H / 2 + 20 ? 26 : -16), class: 'fl-label', 'text-anchor': 'middle' }, label(n)));
    return c;
  });
  host.replaceChildren(svg);

  const t0 = events.length ? events[0][0] : 0, t1 = events.length ? events[events.length - 1][0] : 0, span = Math.max(1, t1 - t0);
  const st = { cur: t0, idx: 0, bal: new Array(nodes.length).fill(0), vol: new Array(nodes.length).fill(0), playing: false, speed: 1, last: 0, pulses: [], raf: 0 };
  const listeners = new Set();
  const radius = i => nodes[i].kind === 'wallet' ? 7 + 9 * Math.sqrt(Math.max(0, st.bal[i]) / Math.max(1, ...st.bal.filter((_, j) => nodes[j].kind === 'wallet'))) : 9 + 6 * Math.sqrt(st.vol[i] / maxVol);
  const paint = () => { dots.forEach((c, i) => c.setAttribute('r', radius(i).toFixed(1))); listeners.forEach(f => f(st)); };
  const apply = ([, f, to, c], visible) => {
    st.bal[f] -= c; st.bal[to] += c; st.vol[f] += c; st.vol[to] += c;
    if (visible && onEvent) onEvent(nodes[f], nodes[to], c);
    if (visible && st.pulses.length < 70 && f !== to) {
      const k = Math.min(f, to) + '-' + Math.max(f, to), p = paths.get(k);
      if (p) { const dot = el('circle', { r: 2.5 + 3.5 * Math.sqrt(c / maxVol), class: 'fl-pulse' }); pulseLayer.append(dot); st.pulses.push({ dot, p: p.hit, len: p.hit.getTotalLength(), born: performance.now(), rev: f !== p.a }); }
    }
  };
  const seek = at => {
    st.cur = Math.min(t1, Math.max(t0, at)); st.idx = 0; st.bal.fill(0); st.vol.fill(0); st.pulses.forEach(p => p.dot.remove()); st.pulses = [];
    while (st.idx < events.length && events[st.idx][0] <= st.cur) apply(events[st.idx++], false);
    paint();
  };
  const frame = now => {
    const dt = Math.min(0.1, (now - st.last) / 1000); st.last = now;
    if (st.playing) {
      st.cur += dt * (span / 30) * st.speed; // toute l'histoire en 30 s à la vitesse 1
      while (st.idx < events.length && events[st.idx][0] <= st.cur) apply(events[st.idx++], true);
      if (st.idx >= events.length && st.cur >= t1) api.pause();
    }
    st.pulses = st.pulses.filter(q => {
      const k = (now - q.born) / (reduced ? 1 : 1100 / Math.sqrt(st.speed)); if (k >= 1) { q.dot.remove(); return false; }
      const pt = q.p.getPointAtLength((q.rev ? 1 - k : k) * q.len); q.dot.setAttribute('cx', pt.x.toFixed(1)); q.dot.setAttribute('cy', pt.y.toFixed(1)); return true;
    });
    paint();
    st.raf = requestAnimationFrame(frame);
  };
  const api = {
    t0, t1, state: st, count: events.length,
    play() { if (!events.length) return; if (st.cur >= t1) seek(t0); st.playing = true; svg.classList.remove('paused'); },
    pause() { st.playing = false; svg.classList.add('paused'); listeners.forEach(f => f(st)); },
    toggle() { st.playing ? api.pause() : api.play(); },
    seek, speed(v) { st.speed = v; }, on(f) { listeners.add(f); },
    destroy() { cancelAnimationFrame(st.raf); },
  };
  seek(t1); // à l'arrivée : l'image finale montre les soldes d'aujourd'hui
  st.last = performance.now(); st.raf = requestAnimationFrame(frame);
  return api;
}
