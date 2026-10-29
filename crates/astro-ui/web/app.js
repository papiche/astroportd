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
const b64 = s => btoa(String.fromCharCode(...new TextEncoder().encode(s)));
const bytes = n => { if (n == null) return '—'; const u = ['o', 'Ko', 'Mo', 'Go', 'To']; let i = 0; while (n >= 1024 && i < 4) { n /= 1024; i++; } return `${n < 10 && i ? n.toFixed(1) : Math.round(n)} ${u[i]}`; };
const when = t => (t ? new Date(t * 1000).toLocaleString('fr', { dateStyle: 'medium', timeStyle: 'short' }) : '—');
const span = s => { s = Math.floor(s); const d = Math.floor(s / 86400), hh = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return d ? `${d} j ${hh} h` : hh ? `${hh} h ${m} min` : `${m} min`; };
const zen = c => (c == null ? '—' : (c / 100).toFixed(2));
const enc = p => p.split('/').map(encodeURIComponent).join('/');
const toast = (msg, ok) => { const t = h('div', { class: 'toast' + (ok ? ' ok' : '') }, msg), box = $('#toasts'); box.append(t); while (box.children.length > 4) box.firstChild.remove(); setTimeout(() => t.remove(), 4500); };
const lastName = p => p.slice(p.lastIndexOf('/') + 1);
const ext = () => window.nostr && !window.nostr.astro; // l'extension du navigateur, pas notre signataire distant
async function authHeader(method, url) {
const basic = store.get('basic');
if (basic) return 'Basic ' + basic;
if (store.get('nip07') && ext()) {
const ev = await window.nostr.signEvent({ kind: 27235, created_at: Math.floor(Date.now() / 1000), tags: [['u', url], ['method', method]], content: '' });
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
async function req(path, { method = 'GET', body } = {}) {
for (let i = 0; i < 3; i++) {
const url = new URL(path, location.origin).href, a = await authHeader(method, url);
const r = await fetch(url, { method, cache: 'no-store', headers: { ...(a && { Authorization: a }), ...(body && { 'Content-Type': 'application/json' }) }, body: body && JSON.stringify(body) });
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
at = (i + items.length) % items.length;
const it = items[at], box = $('#view-b');
box.replaceChildren(h('p', { class: 'muted' }, 'Déchiffrement…'));
$('#view-t').replaceChildren(`${lastName(it.path)} · ${when(it.taken_at || it.mtime)} · `, h('button', { class: 'ghost', onclick: () => share([it.path]) }, 'Partager'));
try {
const u = await blobUrl(it.path);
if (items[at] === it) box.replaceChildren(kindOf(it.mime) === 'video' ? h('video', { src: u, controls: true, autoplay: true }) : h('img', { src: u, alt: lastName(it.path) }));
} catch (e) { box.replaceChildren(h('p', { class: 'err' }, e.message)); }
}
function open(list, i) { items = list; viewer.showModal(); show(i); }
$('#view-x').onclick = () => viewer.close();
viewer.addEventListener('close', () => $('#view-b').replaceChildren());
viewer.addEventListener('keydown', e => { if (e.key === 'ArrowRight') show(at + 1); else if (e.key === 'ArrowLeft') show(at - 1); });
$('#share-x').onclick = () => $('#share').close();
async function share(paths) {
const box = $('#share-b'), dlg = $('#share');
$('#share-t').textContent = 'Lien magique';
box.replaceChildren(h('p', { class: 'muted' }, 'Création du lien…'));
if (!dlg.open) dlg.showModal();
try {
const r = await api('/api/v1/ucloud/share', { method: 'POST', body: { paths, ttl_hours: 72 } });
const inp = h('input', { value: r.url, readOnly: true, 'aria-label': 'Lien magique', onfocus: () => inp.select() });
const copy = h('button', { onclick: async () => { try { await navigator.clipboard.writeText(r.url); copy.textContent = 'Copié ✓'; toast('Lien magique copié', 1); } catch { inp.select(); } } }, 'Copier');
toast(`Lien magique créé (${r.items} fichier(s))`, 1);
box.replaceChildren(
h('p', { class: 'muted' }, `${r.items} fichier(s), valable jusqu'au ${when(r.expires_at)}. La clé de déchiffrement est dans la partie après « # » : un navigateur ne l'envoie jamais au serveur.`),
inp, h('p', {}, copy),
r.qr_svg && h('img', { alt: 'QR code du lien à scanner avec un smartphone', src: 'data:image/svg+xml;base64,' + b64(r.qr_svg) }));
} catch (e) { box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
let cleanups = [];
const every = (ms, fn) => { const t = setInterval(fn, ms); cleanups.push(() => clearInterval(t)); };
const pill = s => h('span', { class: 'pill ' + s }, { ok: 'OK', degraded: 'Dégradé', down: 'Panne', disabled: 'Inactif' }[s] || s);
const row = (k, v) => [h('dt', {}, k), h('dd', {}, v)];
const RES = { disk_10gb: 'Disque 10 Go', disk_128gb: 'Disque 128 Go', disk_256gb: 'Disque 256 Go', gpu_1_24: 'GPU 1/24e' };
const LEASE = { active: 'actif', underpaid: 'sous-payé', unpriced: 'sans tarif', full: 'complet', invalid: 'invalide', revoked: 'révoqué' };
const LIFE = { active_rental: 'ActiveRental', cooperative_member: 'CooperativeMember', grace_period: 'GracePeriod', suspended: 'Suspended' };
const tone = s => (s === 'grace_period' ? 'degraded' : s === 'suspended' ? 'down' : s ? 'ok' : 'disabled');
const day = t => (t ? new Date(t * 1000).toLocaleDateString('fr', { dateStyle: 'medium' }) : '—');
const table = (cap, head, rows) => h('table', {}, h('caption', { class: 'muted' }, cap), h('thead', {}, h('tr', {}, head.map(x => h('th', { scope: 'col' }, x)))), h('tbody', {}, rows.map(r => h('tr', {}, r.map(c => h('td', {}, c))))));
const priceOf = t => `${zen(t.price_cents)} Ẑ/${'sem.'}`;
function consentForm(cp, redraw) {
const ok = h('input', { type: 'checkbox', id: 'cp-ok' }), go = h('button', { disabled: true, onclick: async () => {
try { await api('/api/v1/consent/private-copy', { method: 'POST', body: { version: cp.current_version } }); toast('Copie privée attestée', 1); redraw(); } catch (e) { toast(e.message, 0); }
} }, 'J\'atteste');
ok.onchange = () => { go.disabled = !ok.checked; };
return [h('p', {}, cp.text), h('p', {}, ok, h('label', { for: 'cp-ok' }, ' J\'ai lu et j\'accepte ce texte.')), h('p', {}, go)];
}
async function viewHome(root) {
const draw = async () => {
const [d, fp, cp] = await Promise.all([api('/api/v1/dashboard'), api('/api/v1/me/footprint'), api('/api/v1/consent/private-copy').catch(() => null)]);
const b = d.balance, e = d.energy, w = d.sovereignty, k = fp.disk, pct = k.quota_bytes ? k.used_bytes / k.quota_bytes : 0, ox = fp.opex, cx = fp.capex;
root.replaceChildren(h('div', { class: 'grid' },
h('section', { class: 'card' }, h('h2', {}, 'Solde'),
b.error ? h('p', { class: 'degraded', role: 'status' }, b.error) : [h('p', { class: 'big' }, zen(b.zen_cents), h('small', { class: 'muted' }, ' Ẑen')), h('p', { class: 'muted' }, `${zen(b.g1_cents)} Ğ1`)]),
h('section', { class: 'card' }, h('h2', {}, 'Autonomie'),
e.weeks == null ? h('p', { class: 'muted' }, 'Solde indisponible.') : [h('meter', { min: 0, max: e.max_weeks, low: 2, high: 6, optimum: e.max_weeks, value: Math.min(e.weeks, e.max_weeks), 'aria-label': 'Autonomie en semaines' }),
h('p', { class: 'big' }, e.weeks.toFixed(1), h('small', { class: 'muted' }, ' semaine(s) de loyer')), h('p', { class: 'muted' }, `Loyer : ${zen(e.weekly_rent_cents)} Ẑen / semaine`)]),
h('section', { class: 'card' }, h('h2', {}, 'Empreinte sur les communs'),
h('meter', { min: 0, max: 1, low: .7, high: .9, optimum: 0, value: Math.min(pct, 1), 'aria-label': 'Disque utilisé' }),
h('p', { class: 'big' }, bytes(k.used_bytes), h('small', { class: 'muted' }, ` / ${bytes(k.quota_bytes)}`)),
h('p', { class: 'muted' }, `${k.files} fichier(s) · formule ${k.tier}${k.leased ? ' · quota relevé par un bail' : ''}`),
fp.tariffs.length ? h('p', { class: 'muted' }, 'Tarifs du nœud : ' + fp.tariffs.map(t => `${RES[t.resource] || t.resource} ${priceOf(t)}`).join(' · ')) : null),
h('section', { class: 'card' }, h('h2', {}, 'Souveraineté ', h('small', {}, 'vos données, chiffrées')),
h('p', { class: 'big' }, String(w.encrypted_files), h('small', { class: 'muted' }, ` fichiers chiffrés · ${bytes(w.encrypted_bytes)}`)),
h('dl', {}, row('Dernier manifeste', w.last_manifest_at ? when(w.last_manifest_at) : 'aucun'), row('Événement Nostr', w.last_manifest_event || '—'), row('Révision', w.last_manifest_rev ?? '—'))),
h('section', { class: 'card' }, h('h2', {}, 'Ẑen Opex ', h('small', {}, 'redevances d\'usage')),
h('p', { class: 'big' }, zen(ox.total_28d_cents), h('small', { class: 'muted' }, ' Ẑen · 28 jours')),
h('dl', {}, row('Loyer MULTIPASS', `${zen(ox.weekly_rent_cents)} Ẑ / sem.`), row('Loyers payés', `${zen(ox.rent_28d_cents)} Ẑ`), row('Baux d\'usage payés', `${zen(ox.usage_28d_cents)} Ẑ`), row('Baux en cours', `${zen(ox.usage_weekly_cents)} Ẑ / sem.`)),
h('p', { class: 'muted' }, 'Dépenses courantes : elles paient l\'usage du nœud et ne créent aucun capital.')),
h('section', { class: 'card' }, h('h2', {}, 'Ẑen Capex ', h('small', {}, 'parts d\'investissement · ZenCard')),
cx.assets.length ? [h('p', { class: 'big' }, zen(cx.net_cents), h('small', { class: 'muted' }, ' Ẑen nets')), h('p', { class: 'muted' }, `valeur ${zen(cx.value_cents)} · amortis ${zen(cx.amortized_cents)}`), h('ul', { class: 'muted' }, cx.assets.map(a => h('li', {}, `${a.label || a.id} : ${zen(a.net_cents)} Ẑ`)))]
: h('p', { class: 'muted' }, cx.zencard ? 'Sociétaire ZenCard : aucune part de matériel enregistrée à votre nom.' : 'Aucune part. Une ZenCard fait de vous sociétaire : le matériel financé reste votre capital, il s\'amortit.'),
h('p', { class: 'muted' }, 'Investissement : acheté une fois, jamais confondu avec les redevances.')),
cp && cp.available && h('section', { class: 'card wide' }, h('h2', {}, 'Copie privée ', h('small', {}, cp.article)),
cp.accepted ? h('p', { class: 'ok' }, `Attestée le ${when(cp.accepted_at)} : vous pouvez déposer films et séries ; ils ne sont montrés qu'à vos amis N¹.`) : consentForm(cp, draw)),
h('section', { class: 'card wide' }, h('h2', {}, 'Baux 1/24e et paliers de disque'),
fp.leases.length ? table('Vos baux, du plus récent au plus ancien', ['Ressource', 'Début', 'Durée', 'Utilisé', 'Payé', 'Statut'],
fp.leases.map(l => [RES[l.resource] || l.resource, when(l.start), `${l.units} ${'sem.'}`, l.resource === 'gpu_1_24' ? '1 h/jour' : '—', `${zen(l.paid_cents)} Ẑ`, [pill(l.active ? 'ok' : l.status === 'active' ? 'disabled' : 'degraded'), ' ', l.active ? 'en cours' : (LEASE[l.status] || l.status)]]))
: h('p', { class: 'muted' }, 'Aucun bail pour le moment. Réservez une part de GPU ou un palier de disque par un virement Ẑen au nœud.'))));
};
await draw();
every(30000, () => draw().catch(() => {}));
}
async function viewStation(root) {
const draw = async () => {
const [d, z, cs, ec, ac] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json()), api('/api/v1/station/constellation'), api('/api/v1/station/economy'), api('/api/v1/station/accounts')]);
const s = d.station, r = d.relay, p = d.p2p, sub = z.subsystems || {};
const love = ec.love, sh = ec.shares;
root.replaceChildren(h('div', { class: 'grid' },
h('section', { class: 'card' }, h('h2', {}, 'Station ', pill(z.status)),
h('dl', {}, row('Nœud', s.node), row('PeerID', s.peer_id || '—'), row('Domaine', s.domain || '—'), row('Version', s.version), row('Disponibilité', span(s.uptime_secs)))),
h('section', { class: 'card' }, h('h2', {}, 'Relais Nostr ', pill(sub.relay?.status || 'ok')),
h('p', { class: 'big' }, String(r.events ?? '—'), h('small', { class: 'muted' }, ' événements')),
h('dl', {}, row('Dernier événement', when(r.last_event_at)), row('Base de données', bytes(r.db_bytes)))),
h('section', { class: 'card' }, h('h2', {}, 'Sous-systèmes'), h('dl', {}, Object.entries(sub).flatMap(([n, v]) => row(n, [pill(v.status), ' ', v.detail])))),
h('section', { class: 'card wide' }, h('h2', {}, 'Constellation ', h('small', {}, `${cs.nodes.length} nœud(s)${p.enabled ? ` · ${p.peers} pair(s) connecté(s)` : ''}`)),
table('Nœuds de l\'essaim, du plus puissant au moins puissant', ['Nœud', 'Power-Score', 'Distance', 'Statut', 'Services', 'Tarifs'],
cs.nodes.map(n => [[h('code', {}, n.node_id ? n.node_id.slice(-8) : '—'), n.self ? ' (ce nœud)' : ''], `${n.power_score} · ${n.tier}`, n.distance_km == null ? '—' : `${n.distance_km} km`, pill(n.status === 'connected' || n.self ? 'ok' : 'disabled'), n.services.join(', ') || '—', n.tariffs.map(priceOf).join(' · ') || '—'])),
cs.swarm_dir_configured ? null : h('p', { class: 'muted' }, 'Aucun dossier de pairs configuré (--swarm-dir) : seule cette station est listée.')),
h('section', { class: 'card wide' }, h('h2', {}, 'Régie coopérative 3×1/3 ', h('small', {}, ec.week ? `semaine ${ec.week}` : '')),
ec.kind === 'split' ? [
h('div', { class: 'shares' }, sh.map(x => h('div', {}, h('strong', {}, x.name), h('progress', { max: 100, value: x.pct, 'aria-label': `${x.name} ${x.pct} %` }), h('span', { class: 'muted' }, `${zen(x.cents)} Ẑ · ${x.pct} %`)))),
h('dl', {}, row('Surplus réparti', `${zen(ec.allocation.surplus_cents)} Ẑ`), row('Provision fiscale (IS)', `${zen(ec.allocation.is_provision_cents)} Ẑ`), row('Prime du Capitaine', `${zen(ec.allocation.captain_bonus_cents)} Ẑ`))]
: ec.kind === 'none' ? h('p', { class: 'muted' }, 'Aucune clôture hebdomadaire enregistrée.')
: h('p', { class: 'degraded' }, ec.kind === 'drawdown' ? `Semaine ${ec.week} : trésorerie entamée (${zen(ec.gap_cents)} Ẑ).` : `Semaine ${ec.week} : bénévolat actif, apport du Capitaine de ${zen(ec.gap_cents)} Ẑ.`),
h('h3', {}, 'Love Ledger'), h('p', {}, h('strong', {}, `${zen(love.total_cents)} Ẑ`), ` donnés sur ${love.weeks} semaine(s)`),
love.recent.length ? h('ul', { class: 'muted' }, love.recent.map(x => h('li', {}, `${day(x.at)} : ${zen(x.amount_cents)} Ẑ${x.detail?.week ? ` (semaine ${x.detail.week})` : ''}`))) : null),
h('section', { class: 'card wide' }, h('h2', {}, 'Comptes hébergés ', h('small', {}, `${ac.accounts.length}`)),
ac.accounts.length ? table('MULTIPASS et ZenCards de la station', ['Compte', 'Cycle de vie', 'Disque', 'Fichiers', 'Secours'],
ac.accounts.map(a => [a.email || a.hex.slice(0, 12), [pill(tone(a.status)), ' ', LIFE[a.status] || a.status || 'inconnu', a.grace_ends_at ? ` (jusqu'au ${day(a.grace_ends_at)})` : ''],
[h('meter', { min: 0, max: 1, low: .7, high: .9, optimum: 0, value: a.quota_bytes ? Math.min(a.used_bytes / a.quota_bytes, 1) : 0, 'aria-label': 'Disque' }), ` ${bytes(a.used_bytes)} / ${bytes(a.quota_bytes)}`], String(a.files),
[h('button', { class: 'ghost', onclick: () => token(a) }, 'Jeton de secours'), a.pass_locked_until ? [' ', h('button', { onclick: async () => { await api(`/api/v1/station/accounts/${a.hex}/unlock-pass`, { method: 'POST' }); toast('PASS déverrouillé', 1); draw(); } }, 'Déverrouiller le PASS')] : null]]))
: h('p', { class: 'muted' }, 'Aucun compte hébergé.')),
h('section', { class: 'card wide' }, h('h2', {}, 'Configuration coopérative'), h('p', { class: 'muted' }, 'Paramètres partagés par toute la constellation (OpenCollective, Mailjet, règle 3×1/3…). Les secrets sont chiffrés et ne s\'affichent jamais.'), h('button', { onclick: () => { location.hash = '#/coop'; } }, 'Modifier la configuration')),
h('section', { class: 'card wide' }, h('h2', {}, 'Options de la station'), h('p', { class: 'muted' }, 'Moteur de reconnaissance des visages (local, Brain GPU ou arrêt) et règles du relai par kind, sans redémarrer.'), h('button', { onclick: () => { location.hash = '#/options'; } }, 'Ouvrir les options'))));
};
await draw();
every(30000, () => draw().catch(() => {}));
}
async function token(a) {
const box = $('#share-b'), dlg = $('#share');
$('#share-t').textContent = 'Jeton de secours';
box.replaceChildren(h('p', { class: 'muted' }, 'Génération…'));
if (!dlg.open) dlg.showModal();
try {
const r = await api(`/api/v1/station/accounts/${a.hex}/token`, { method: 'POST' });
const inp = h('input', { value: r.token, readOnly: true, 'aria-label': 'Jeton', onfocus: () => inp.select() });
toast('Jeton de secours généré', 1);
box.replaceChildren(h('p', { class: 'muted' }, `Compte ${r.login}. À transmettre de vive voix ou par un canal chiffré : l'opération est tracée dans le journal d'audit.`), inp);
} catch (e) { box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
async function viewCloud(root, q) {
const path = q.get('p') || '/', album = q.get('m') === 'album';
const go = (p, m) => { location.hash = '#/cloud?' + new URLSearchParams({ p, ...(m && { m }) }); };
const tabs = h('div', { class: 'bar' },
h('button', { class: album ? 'ghost' : '', onclick: () => go(path) }, 'Dossiers'),
h('button', { class: album ? '' : 'ghost', onclick: () => go(path, 'album') }, 'Album'));
if (album) {
const grid = h('div', { class: 'photos' }), more = h('button', { class: 'ghost' }, 'Plus de photos'), list = [];
root.replaceChildren(tabs, grid, h('p', {}, more));
const load = async before => {
const r = await api('/api/v1/ucloud/photos?limit=60' + (before ? '&before=' + before : ''));
dav = r.dav || dav;
for (const it of r.items) {
const i = list.push(it) - 1;
const img = h('img', { alt: '', decoding: 'async' });
grid.append(lazy(h('button', { class: 'tile', 'aria-label': lastName(it.path), onclick: () => open(list, i) }, img, kindOf(it.mime) === 'video' && h('span', { class: 'badge' }, '▶')), () => (kindOf(it.mime) === 'image' ? blobUrl(it.path, !!it.thumb).then(u => { img.src = u; }) : 0)));
}
more.hidden = !r.next; more.onclick = () => load(r.next);
if (!list.length) grid.replaceWith(h('p', { class: 'muted' }, 'Aucune photo pour le moment.'));
};
return load();
}
const r = await api('/api/v1/ucloud/list?path=' + encodeURIComponent(path));
dav = r.dav || dav;
const sel = new Set(), btn = h('button', { disabled: true, onclick: () => share([...sel]) }, 'Partager la sélection'), send = h('button', { disabled: true, class: 'ghost', onclick: () => import('/ui/m-send.js').then(m => m.default([...sel], K)) }, 'Envoyer à des proches');
const parts = path.split('/').filter(Boolean), crumbs = h('span', { class: 'crumbs' }, h('a', { href: '#/cloud' }, 'uCloud'));
parts.forEach((n, i) => crumbs.append(' / ', h('a', { href: '#/cloud?p=' + encodeURIComponent('/' + parts.slice(0, i + 1).join('/')) }, n)));
const media = r.entries.filter(x => x.type === 'file' && kindOf(x.mime) !== 'file');
const li = x => {
const dir = x.type === 'dir', k = kindOf(x.mime);
const act = dir ? () => go(x.path) : k !== 'file' ? () => open(media, media.indexOf(x)) : async () => { const u = await blobUrl(x.path); h('a', { href: u, download: x.name }).click(); };
return h('li', {},
h('input', { type: 'checkbox', 'aria-label': 'Sélectionner ' + x.name, onchange: ev => { ev.target.checked ? sel.add(x.path) : sel.delete(x.path); btn.disabled = send.disabled = !sel.size; } }),
h('span', { 'aria-hidden': 'true' }, dir ? '📁' : k === 'image' ? '🖼' : k === 'video' ? '🎬' : '📄'),
h('button', { class: 'link', onclick: act }, x.name), h('span', { class: 'muted' }, dir ? '' : bytes(x.size)), h('span', { class: 'muted' }, when(x.mtime)));
};
root.replaceChildren(tabs, h('div', { class: 'bar' }, crumbs, h('span', { class: 'sp' }), send, btn),
r.entries.length ? h('ul', { class: 'files' }, r.entries.map(li)) : h('p', { class: 'muted' }, 'Dossier vide.'));
}
async function viewBro(root) {
const log = h('ol', { id: 'log', 'aria-live': 'polite', 'aria-label': 'Activité de la station' });
const add = (ev, me) => { const stick = log.scrollTop + log.clientHeight >= log.scrollHeight - 30; log.append(h('li', { class: me ? 'me' : '' }, h('span', { class: 'k' }, me ? 'vous' : ev.kind), ev.message)); if (stick) log.scrollTop = log.scrollHeight; };
const input = h('input', { placeholder: 'Parler à BRO… (#solde, #statut, #aide)', 'aria-label': 'Message pour BRO', maxLength: 2000, required: true });
const say = h('form', { id: 'say', onsubmit: async ev => {
ev.preventDefault();
const text = input.value; input.value = ''; add({ message: text }, true);
try { add({ kind: 'bro', message: (await api('/api/v1/bro/say', { method: 'POST', body: { text } })).reply }); } catch (e) { add({ kind: 'erreur', message: e.message }); }
} }, input, h('button', {}, 'Envoyer'));
root.replaceChildren(h('div', { class: 'grid' }, h('section', { class: 'card wide' }, h('h2', {}, 'BRO LifeOS ', h('small', {}, 'en direct'), ' '), log, say)));
feed.forEach(ev => add(ev));
listeners.add(add);
cleanups.push(() => listeners.delete(add));
}
const feed = [], listeners = new Set(), started = Date.now();
function startLive() {
startLive = () => {};
live(ev => {
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
// Multilingue : le français du code sert de clé ; m-lang-<lg>.js donne les textes exacts traduits (sinon le français reste).
let dict = {};
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
try { dict = (await import(`/ui/m-lang-${lang}.js`)).default; } catch (e) { toast(e.message); return; }
tr(document.body); new MutationObserver(ms => ms.forEach(m => m.addedNodes.forEach(tr))).observe(document.body, { childList: true, subtree: true });
}
// Écrans lourds chargés à la demande (hors budget de la coque) : ils reçoivent la boîte à outils ci-dessous.
const K = { t, lang, setLang, login: (u, p) => store.set('basic', btoa(`${u}:${p}`)), h, api, toast, when, bytes, span, pill, table, open, every, enc, dav: v => { if (v) dav = v; }, cleanup: f => cleanups.push(f) };
const lazyView = f => async (root, q) => (await import('/ui/' + f)).default(root, K, q);
const views = { home: viewHome, station: viewStation, cloud: viewCloud, account: lazyView('m-account.js'), contacts: lazyView('m-contacts.js'), people: lazyView('m-people.js'), replication: lazyView('m-replication.js'), coop: lazyView('m-coop.js'), options: lazyView('m-options.js'), bro: viewBro };
let me = null;
async function route() {
cleanups.forEach(c => c()); cleanups = [];
blobs.forEach(p => p.then(URL.revokeObjectURL, () => {})); blobs.clear();
const [p, qs] = (location.hash.slice(1) || '/').split('?'), main = $('#main');
main.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
try {
if (!me) { me = await api('/api/v1/me'); import('/ui/m-nostr.js').then(m => m.default(me, api), () => {}); }
document.querySelector('nav a[data-r=station]').hidden = !me.captain; // l'écran Capitaine n'existe pas pour un MULTIPASS
const name = ['cloud', 'bro', 'people', 'replication', 'account', 'contacts'].find(n => p.startsWith('/' + n)) || (p.startsWith('/options') && me.captain ? 'options' : p.startsWith('/coop') && me.captain ? 'coop' : p.startsWith('/station') && me.captain ? 'station' : 'home');
document.querySelectorAll('nav a').forEach(a => (a.dataset.r === (name === 'coop' || name === 'options' ? 'station' : name) ? a.setAttribute('aria-current', 'page') : a.removeAttribute('aria-current')));
await views[name](main, new URLSearchParams(qs || ''));
$('#out').hidden = false;
startLive();
} catch (e) { main.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
addEventListener('hashchange', route);
addEventListener('offline', () => toast('Hors ligne : la station est injoignable'));
addEventListener('online', () => toast('Connexion rétablie', 1));
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/ui/sw.js', { scope: '/' }).catch(() => { /* contexte non sécurisé : pas de PWA, l'application reste utilisable */ });
loadLang().then(route);
