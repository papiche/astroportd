'use strict';
const $ = (s, e = document) => e.querySelector(s);
const h = (tag, props, ...kids) => {
const e = document.createElement(tag);
for (const [k, v] of Object.entries(props || {})) {
if (v == null || v === false) continue;
if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
else if (k === 'class') e.className = v;
else if (k in e) e[k] = v;
else e.setAttribute(k, v);
}
e.append(...kids.flat().filter(x => x != null && x !== false));
return e;
};
const store = {
get: k => { try { return localStorage.getItem(k); } catch { return null; } },
set: (k, v) => { try { v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch { /* stockage indisponible : session éphémère */ } },
};
// Style (Aurore, Sobre, Kraft) et mode (auto, clair, sombre) : préférences de cet appareil, appliquées dès le chargement.
const STYLES = ['aurore', 'sobre', 'kraft'], MODES = ['auto', 'light', 'dark'], MODE_ICON = { auto: '◐', light: '☀', dark: '☾' };
const look = () => {
  const style = STYLES.includes(store.get('style')) ? store.get('style') : 'aurore', mode = MODES.includes(store.get('mode')) ? store.get('mode') : 'auto';
  document.documentElement.dataset.style = style;
  if (mode === 'auto') delete document.documentElement.dataset.mode; else document.documentElement.dataset.mode = mode;
  return { style, mode };
};
{
  const { style, mode } = look(), pick = document.getElementById('style'), btn = document.getElementById('mode');
  pick.value = style; btn.textContent = MODE_ICON[mode];
  pick.onchange = () => { store.set('style', pick.value); look(); };
  btn.onclick = () => { const next = MODES[(MODES.indexOf(look().mode) + 1) % MODES.length]; store.set('mode', next); btn.textContent = MODE_ICON[next]; look(); };
}
const b64 = s => btoa(String.fromCharCode(...new TextEncoder().encode(s)));
const bytes = n => { if (n == null) return '—'; const u = ['o', 'Ko', 'Mo', 'Go', 'To']; let i = 0; while (n >= 1024 && i < 4) { n /= 1024; i++; } return `${n < 10 && i ? n.toFixed(1) : Math.round(n)} ${u[i]}`; };
const when = t => (t ? new Date(t * 1000).toLocaleString('fr', { dateStyle: 'medium', timeStyle: 'short' }) : '—');
const span = s => { s = Math.floor(s); const d = Math.floor(s / 86400), hh = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return d ? `${d} j ${hh} h` : hh ? `${hh} h ${m} min` : `${m} min`; };
const enc = p => p.split('/').map(encodeURIComponent).join('/');
const toast = (msg, ok) => { const t = h('div', { class: 'toast' + (ok ? ' ok' : '') }, msg), box = $('#toasts'); box.append(t); while (box.children.length > 4) box.firstChild.remove(); setTimeout(() => t.remove(), 4500); };
const lastName = p => p.slice(p.lastIndexOf('/') + 1);
const ext = () => window.nostr && !window.nostr.astro; // l'extension du navigateur, pas notre signataire distant
async function authHeader(method, url) {
const basic = store.get('basic');
if (basic) return 'Basic ' + basic;
if (store.get('nip07') && ext()) {
const ev = await window.nostr.signEvent({ kind: 27235, created_at: Math.floor(Date.now() / 1000), tags: [['u', url], ['method', method], ['nonce', [...crypto.getRandomValues(new Uint8Array(8))].map(b => b.toString(16).padStart(2, '0')).join('')]], content: '' });
return 'Nostr ' + b64(JSON.stringify(ev));
}
return null;
}
let loginP = null;
function login() {
loginP ??= new Promise(done => {
const d = $('#login'), f = $('#login-f'), er = $('#login-e'), nip = $('#login-n');
const ok = () => { d.close(); f.reset(); loginP = null; $('#out').hidden = false; done(); };
const probe = async a => (await fetch('/api/v1/me', { headers: a ? { Authorization: a } : {} })).ok;
er.textContent = '';
nip.hidden = !ext();
d.oncancel = ev => ev.preventDefault();
f.onsubmit = async ev => {
ev.preventDefault();
let u = f.u.value.trim(), p = f.p.value;
if (/^\d{4,6}$/.test(p)) { // code PASS : la station le vérifie (essais limités) et rend le jeton
const r = await fetch('/api/v1/account/unlock', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: u, pass: p }) }), j = await r.json().catch(() => ({}));
if (!r.ok) return er.textContent = j.error || 'Connexion refusée.';
u = j.email; p = j.token;
}
const basic = b64(u + ':' + p);
if (await probe('Basic ' + basic)) { store.set('basic', basic); store.set('nip07', null); ok(); } else er.textContent = 'Identifiant ou jeton refusé.';
};
nip.onclick = async () => {
store.set('basic', null); store.set('nip07', '1');
try { if (await probe(await authHeader('GET', new URL('/api/v1/me', location.origin).href))) return ok(); } catch { /* refus de l'extension */ }
store.set('nip07', null); er.textContent = 'Extension Nostr refusée ou compte inconnu.'; store.set('nip07_off', '1');
};
if (ext() && !store.get('nip07_off')) nip.onclick().then(() => { if (loginP) { store.set('nip07_off', '1'); d.showModal(); } });
else d.showModal();
});
return loginP;
}
async function req(path, { method = 'GET', body, headers } = {}) {
for (let i = 0; i < 3; i++) {
const url = new URL(path, location.origin).href, a = await authHeader(method, url);
const r = await fetch(url, { method, cache: 'no-store', headers: { ...(a && { Authorization: a }), ...(body && { 'Content-Type': 'application/json' }), ...headers }, body: body && JSON.stringify(body) });
if (r.status !== 401) return r;
await login();
}
throw new Error('Connexion requise');
}
async function api(path, opts) {
const r = await req(path, opts), j = await r.json().catch(() => ({}));
if (!r.ok) throw new Error(j.error || r.statusText);
return j;
}
$('#out').onclick = () => { store.set('basic', null); store.set('nip07', null); store.set('nip07_off', '1'); location.reload(); };
const blobs = new Map();
let dav = '/dav';
function blobUrl(path, thumb) {
const k = (thumb ? 't:' : 'o:') + path;
if (!blobs.has(k)) {
const get = t => req(dav + enc(path) + (t ? '?thumb' : '')).then(async r => { if (!r.ok) throw new Error(`${path} : ${r.status}`); return URL.createObjectURL(await r.blob()); });
blobs.set(k, thumb ? get(true).catch(() => blobUrl(path)) : get(false));
}
return blobs.get(k);
}
const seen = new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting) { seen.unobserve(e.target); e.target._go(); } }), { rootMargin: '200px' });
const lazy = (el, go) => { el._go = go; seen.observe(el); return el; };
const kindOf = m => (m || '').startsWith('image/') ? 'image' : (m || '').startsWith('video/') ? 'video' : 'file';
const viewer = $('#view');
let items = [], at = 0;
async function show(i) {
if (!items.length) { viewer.close(); return; }
at = (i + items.length) % items.length;
const it = items[at], box = $('#view-b');
box.replaceChildren(h('p', { class: 'muted' }, 'Déchiffrement…'));
const isImg = kindOf(it.mime) === 'image';
$('#view-p').hidden = $('#view-n').hidden = items.length < 2;
$('#view-t').replaceChildren(h('span', {}, `${at + 1} / ${items.length} · ${lastName(it.path)} · ${when(it.taken_at || it.mtime)} `),
h('button', { class: 'ghost', onclick: () => share([it.path]) }, 'Partager'));
photos().then(m => { if (items[at] === it) $('#view-t').append(...m.actions(it, isImg)); });
try {
const u = await blobUrl(it.path);
if (items[at] === it) box.replaceChildren(kindOf(it.mime) === 'video' ? h('video', { src: u, controls: true, autoplay: true }) : h('img', { src: u, alt: lastName(it.path) }));
} catch (e) { box.replaceChildren(h('p', { class: 'err' }, e.message)); }
}
function open(list, i) { items = list; viewer.showModal(); show(i); }
$('#view-x').onclick = () => viewer.close();
$('#view-p').onclick = () => show(at - 1);
$('#view-n').onclick = () => show(at + 1);
viewer.addEventListener('close', () => $('#view-b').replaceChildren());
viewer.addEventListener('keydown', e => { if (e.key === 'ArrowRight') show(at + 1); else if (e.key === 'ArrowLeft') show(at - 1); else if (e.key === 'Home') show(0); else if (e.key === 'End') show(items.length - 1); else if (e.key === 's') share([items[at].path]); });
let x0 = null;
$('#view-b').addEventListener('pointerdown', e => { x0 = e.clientX; });
$('#view-b').addEventListener('pointerup', e => { if (x0 !== null && Math.abs(e.clientX - x0) > 60) show(at + (e.clientX < x0 ? 1 : -1)); x0 = null; });
const del = p => req(dav + enc(p), { method: 'DELETE' });
const picker = { on: false, set: new Set(), all: new Set(), lists: new Set(), change: () => {} };
function mark() {
for (const b of picker.all) { if (!b.isConnected) { picker.all.delete(b); continue; } const on = picker.set.has(b.dataset.path); b.classList.toggle('sel', on); b.setAttribute('aria-pressed', String(on)); }
picker.change();
}
function togglePick(path) { if (picker.set.has(path)) picker.set.delete(path); else picker.set.add(path); mark(); }
const photos = async () => (await mod('m-photos.js')).default({ api, toast, t, h, blobs, del, picker, mark, viewer, cur: () => ({ items, at }), show });
$('#share-x').onclick = () => $('#share').close();
const share = async paths => (await mod('m-share.js')).default({ api, toast, h, $, when, b64 })(paths);
let cleanups = [];
const every = (ms, fn) => { const t = setInterval(fn, ms); cleanups.push(() => clearInterval(t)); };
const pill = s => h('span', { class: 'pill ' + s }, { ok: 'OK', degraded: 'Dégradé', down: 'Panne', disabled: 'Inactif' }[s] || s);
const table = (cap, head, rows) => h('table', {}, h('caption', { class: 'muted' }, cap), h('thead', {}, h('tr', {}, head.map(x => h('th', { scope: 'col' }, x)))), h('tbody', {}, rows.map(r => h('tr', {}, r.map(c => h('td', {}, Array.isArray(c) ? c.flat(Infinity) : c))))));
async function viewCloud(root, q) {
const path = q.get('p') || '/', mode = q.get('m'), sources = mode === 'sources', organize = mode === 'organize';
const go = (p, m) => { location.hash = '#/cloud?' + new URLSearchParams({ p, ...(m && { m }) }); };
const tabs = h('div', { class: 'bar' },
h('button', { class: sources || organize ? 'ghost' : '', onclick: () => go(path) }, 'Dossiers'),
h('button', { class: sources ? '' : 'ghost', onclick: () => go(path, 'sources') }, 'Sources'),
h('button', { class: organize ? '' : 'ghost', onclick: () => go(path, 'organize') }, 'Ranger'));
if (sources) return (await mod('m-sources.js')).default(root, K, tabs);
if (organize) return (await mod('m-organize.js')).default(root, K, tabs);
const r = await api('/api/v1/ucloud/list?path=' + encodeURIComponent(path));
dav = r.dav || dav;
const sel = new Set(), btn = h('button', { disabled: true, onclick: () => share([...sel]) }, 'Partager la sélection'), send = h('button', { disabled: true, class: 'ghost', onclick: () => mod('m-send.js').then(m => m.default([...sel], K)) }, 'Envoyer à des proches'), pubBox = h('div', { class: 'card', hidden: '' }), pub = h('button', { disabled: true, class: 'ghost', onclick: () => { pubBox.hidden = false; mod('m-publish.js').then(m => m.default(pubBox, [...sel], K, path, () => go(path))); } }, path.startsWith('/Public') ? 'Retirer de Public' : 'Publier');
const parts = path.split('/').filter(Boolean), crumbs = h('span', { class: 'crumbs' }, h('a', { href: '#/cloud' }, t('Fichiers')));
parts.forEach((n, i) => crumbs.append(' / ', h('a', { href: '#/cloud?p=' + encodeURIComponent('/' + parts.slice(0, i + 1).join('/')) }, n)));
const media = r.entries.filter(x => x.type === 'file' && kindOf(x.mime) !== 'file');
const li = x => {
const dir = x.type === 'dir', k = kindOf(x.mime);
const act = dir ? () => go(x.path) : k !== 'file' ? () => open(media, media.indexOf(x)) : async () => { const u = await blobUrl(x.path); h('a', { href: u, download: x.name }).click(); };
return h('li', { 'data-path': x.path, 'data-dir': dir ? 1 : null },
h('input', { type: 'checkbox', 'aria-label': 'Sélectionner ' + x.name, onchange: ev => { ev.target.checked ? sel.add(x.path) : sel.delete(x.path); btn.disabled = send.disabled = pub.disabled = !sel.size; } }),
h('span', { 'aria-hidden': 'true' }, dir ? '📁' : k === 'image' ? '🖼' : k === 'video' ? '🎬' : '📄'),
h('button', { class: 'link', onclick: act }, x.name), h('span', { class: 'muted' }, dir ? '' : bytes(x.size)), h('span', { class: 'muted' }, when(x.mtime)));
};
root.replaceChildren(tabs, h('div', { class: 'bar' }, crumbs, h('span', { class: 'sp' }), pub, send, btn), pubBox,
r.entries.length ? h('ul', { class: 'files' }, r.entries.map(li)) : h('p', { class: 'muted' }, 'Dossier vide.'));
// Téléverser, créer, renommer, déplacer, supprimer, glisser-déposer : m-files.js, chargé avec la liste.
mod('m-files.js').then(m => m.default(root, K, { path, dav, reload: route }), e => toast(e.message));
}
// Tuiles de photos ajoutées à `grid` ; `list` est la liste que la visionneuse parcourt (flèches, Début, Fin).
function tiles(grid, list, its) {
for (const it of its) {
list.push(it); picker.lists.add(list);
const img = h('img', { alt: '', decoding: 'async' });
const btn = h('button', { class: 'tile', 'aria-label': lastName(it.path), onclick: () => (picker.on ? togglePick(it.path) : open(list, list.indexOf(it))) }, img, kindOf(it.mime) === 'video' && h('span', { class: 'badge' }, '▶'), h('span', { class: 'tick', 'aria-hidden': 'true' }, '✓'));
btn.dataset.path = it.path; picker.all.add(btn);
grid.append(lazy(btn, () => (kindOf(it.mime) === 'image' ? blobUrl(it.path, !!it.thumb).then(u => { img.src = u; }) : 0)));
}
}
const feed = [], listeners = new Set(), started = Date.now();
function badge(n) { const a = $('nav a[data-r=people]'); if (!a) return; let b = a.querySelector('.ch-badge'); if (!n) return b?.remove(); if (!b) a.append(b = h('span', { class: 'ch-badge' })); b.textContent = n; }
const refreshBadge = () => api('/api/v1/chat').then(r => badge(r.unread)).catch(() => {}); // sans chat sur cette station : pas de pastille
function startLive() {
startLive = () => {};
live(ev => {
if (ev.source === 'chat:msg') { refreshBadge(); if (!location.hash.startsWith('#/chat')) toast(t('Nouveau message'), 1); }
if (ev.kind === 'call') mod('m-call.js').then(m => m.signal(K, ev.data), e => toast(e.message)); // visio
feed.push(ev); if (feed.length > 200) feed.shift();
listeners.forEach(l => l(ev));
if (ev.at * 1000 > started - 5000) { if (ev.kind === 'economy') toast(ev.message, 1); else if (ev.source === 'bro:upload') toast(ev.message, 1); }
}, new AbortController().signal);
}
async function live(onEvent, signal) {
let last = '', wait = 500;
while (!signal.aborted) {
try {
const url = new URL('/api/v1/activity', location.origin).href, a = await authHeader('GET', url);
const r = await fetch(url, { signal, headers: { ...(a && { Authorization: a }), ...(last && { 'Last-Event-ID': last }) } });
if (r.status === 401) await login();
if (!r.ok) throw new Error(String(r.status));
wait = 500;
const rd = r.body.pipeThrough(new TextDecoderStream()).getReader();
let buf = '';
for (;;) {
const { value, done } = await rd.read();
if (done) break;
buf += value;
for (let i; (i = buf.indexOf('\n\n')) >= 0;) {
const blk = buf.slice(0, i); buf = buf.slice(i + 2);
let data = '', ev = '';
for (const l of blk.split('\n')) { if (l.startsWith('id:')) last = l.slice(3).trim(); else if (l.startsWith('event:')) ev = l.slice(6).trim(); else if (l.startsWith('data:')) data += l.slice(5).trimStart(); }
if (data && ev !== 'lag') { try { onEvent(JSON.parse(data)); } catch { /* ligne incomplète */ } }
}
}
} catch { if (signal.aborted) return; }
await new Promise(ok => setTimeout(ok, wait)); wait = Math.min(wait * 2, 30000);
}
}
// Le français sert de clé ; m-lang-<lg>.js traduit (sinon le français reste).
let dict = {};
// Phrases propres à un écran : chargées avec lui (voir m-lang-<lg>.js).
let PARTS = new Set(); const parts = new Set();
const mod = async f => { const p = f.slice(2, -3); if (lang !== 'fr' && PARTS.has(p) && !parts.has(p)) { parts.add(p); try { Object.assign(dict, (await import(`/ui/m-lang-${lang}-${p}.js`)).default); tr(document.body); } catch (e) { toast(e.message); } } return import('/ui/' + f); };
const LANGS = ['fr', 'en', 'es'], lsGet = () => { try { return localStorage.getItem('lang'); } catch { return null; } };
const lang = LANGS.find(l => l === (lsGet() || navigator.language.slice(0, 2))) || 'fr';
const t = (s, ...a) => a.reduce((r, v, i) => r.replace(`{${i}}`, v), dict[s] ?? s);
const tr = n => {
if (n.nodeType === 3) { const k = n.nodeValue.trim(); if (dict[k]) n.nodeValue = n.nodeValue.replace(k, dict[k]); return; }
if (n.nodeType !== 1) return;
for (const a of ['placeholder', 'aria-label', 'title']) { const v = n.getAttribute(a); if (dict[v]) n.setAttribute(a, dict[v]); }
n.childNodes.forEach(tr);
};
const setLang = l => { try { localStorage.setItem('lang', l); } catch { /* préférence non conservée */ } location.reload(); };
async function loadLang() {
document.documentElement.lang = lang;
if (lang === 'fr') return;
try { const m = await import(`/ui/m-lang-${lang}.js`); dict = m.default; PARTS = new Set(m.parts.split(' ')); } catch (e) { toast(e.message); return; }
tr(document.body); new MutationObserver(ms => ms.forEach(m => m.addedNodes.forEach(tr))).observe(document.body, { childList: true, subtree: true });
}
// Écrans lourds chargés à la demande (hors budget de la coque) : ils reçoivent la boîte à outils ci-dessous.
// Confirmation sans boîte bloquante : un premier clic arme le bouton (et dit ce qui va se passer), le second exécute ; il se désarme seul après 5 s.
const sure = (label, run, why, attrs) => {
  const note = h('small', { class: 'muted', role: 'status' }), b = h('button', { class: 'ghost', ...attrs, onclick: ev => {
    if (b.dataset.armed) { delete b.dataset.armed; note.textContent = ''; return run(ev); }
    b.dataset.armed = 1; b.textContent = t('Confirmer ?'); note.textContent = ' ' + (why || '');
    setTimeout(() => { delete b.dataset.armed; b.textContent = label; note.textContent = ''; }, 5000);
  } }, label);
  return h('span', {}, b, note);
};
const K = { req, auth: authHeader, sure, mod, t, lang, setLang, login: (u, p) => store.set('basic', btoa(`${u}:${p}`)), h, api, toast, when, bytes, span, pill, table, open, blob: blobUrl, tiles, every, enc, del, picker, mark, photos, dav: v => { if (v) dav = v; }, cleanup: f => cleanups.push(f), planeHref: (n, hash) => planeHref && planeHref(n, hash), listen: f => { listeners.add(f); cleanups.push(() => listeners.delete(f)); }, chatUnread: n => badge(n), feed, listeners };
const lazyView = f => async (root, q) => (await mod(f)).default(root, K, q);
let me = null, shell = null;
async function route() {
cleanups.forEach(c => c()); cleanups = [];
blobs.forEach(p => p.then(URL.revokeObjectURL, () => {})); blobs.clear();
// Chaque navigation reçoit un `<main>` neuf : le code encore en cours d'un écran quitté (chargement lent) n'écrit plus que dans
// un arbre détaché, jamais dans l'écran suivant, et son erreur éventuelle n'y apparaît pas.
const [p, qs] = (location.hash.slice(1) || '/').split('?'), main = h('main', { id: 'main', tabindex: '-1' }, h('p', { class: 'muted' }, 'Chargement…'));
$('#main').replaceWith(main);
try {
if (!me) { me = await api('/api/v1/me'); mod('m-nostr.js').then(m => m.default(me, api), () => {}); mod('m-ux.js').then(m => m.default(), () => {}); }
const R = await mod('m-routes.js');
const res = R.resolve(p, me, store.get('space') || 'me');
if (res.redirect) { location.replace('#' + res.redirect + (qs ? '?' + qs : '')); return; }
if (res.route.path === '/bro' && !me.ai) { toast(t('Déclarez une IA dans Mon compte pour utiliser l\'assistant')); location.hash = '#/account'; return; }
if (res.route.space !== 'both') store.set('space', res.space);
if (me.captain) planeSwitch();
(shell ??= (await mod('m-shell.js')).default(K, me)).update(res);
await (res.route.file ? lazyView(res.route.file)(main, new URLSearchParams(qs || '')) : viewCloud(main, new URLSearchParams(qs || '')));
$('#out').hidden = false;
startLive(); refreshBadge();
} catch (e) { main.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
// Sélecteur de plan (Capitaine) : quand la station route par nom d'hôte (`astroportd planes --front`), `<plan>.<domaine>` ouvre l'interface de ce plan.
let planeSwitched = false, planeHref = null;
async function planeSwitch() {
  if (planeSwitched) return;
  planeSwitched = true;
  try {
    const r = await api('/api/v1/station/planes');
    if (!r.front || r.planes.length < 2) return;
    const names = r.planes.map(p => p.name), host = location.hostname, first = host.split('.')[0];
    const onPlane = host.includes('.') && names.includes(first), base = onPlane ? host.slice(first.length + 1) : host;
    const current = onPlane ? first : (r.planes.find(p => p.origin) || r.planes[0]).name;
    K.plane = current; // constellation de cette interface (écrans du Capitaine)
    planeHref = (n, hash) => { const origin = r.planes.find(p => p.name === n)?.origin; return `${location.protocol}//${origin ? '' : n + '.'}${base}${location.port ? ':' + location.port : ''}/${hash || ''}`; };
    const go = n => { const origin = r.planes.find(p => p.name === n)?.origin; location.href = `${location.protocol}//${origin ? '' : n + '.'}${base}${location.port ? ':' + location.port : ''}/`; };
    $('.tools').prepend(h('select', { id: 'plane', 'aria-label': t('Constellation'), title: t('Constellation'), onchange: ev => go(ev.target.value) },
      r.planes.map(p => h('option', { value: p.name, selected: p.name === current }, p.name))));
  } catch { /* pas d'écran des plans (station sans configuration) : pas de sélecteur */ }
}
let stale; const had = !!navigator.serviceWorker?.controller;
addEventListener('hashchange', () => (stale && !$('.call-local') ? location.reload() : route()));
addEventListener('offline', () => toast('Hors ligne : la station est injoignable'));
addEventListener('online', () => toast('Connexion rétablie', 1));
if ('serviceWorker' in navigator) navigator.serviceWorker.oncontrollerchange = () => { stale = had; }, navigator.serviceWorker.register('/ui/sw.js', { scope: '/' }).catch(() => { /* contexte non sécurisé : sans PWA */ });
loadLang().then(route);
