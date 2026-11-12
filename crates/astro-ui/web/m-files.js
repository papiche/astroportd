'use strict';
// Gestion de fichiers dans le navigateur, comme sur un drive en ligne : téléverser (avec jauge), glisser-déposer, nouveau dossier,
// renommer, déplacer, supprimer. Tout passe par les verbes WebDAV du serveur (PUT, MKCOL, MOVE, DELETE) : rien à ajouter côté station,
// et un déplacement ne change que des chemins (aucun fichier n'est recopié ni re-chiffré).

// Dossiers fonctionnels du VFS (coffre, contacts, agenda, partages, sauvegardes d'amis) et fichiers cachés : ni renommés, ni déplacés,
// ni remplis à la main.
const RESERVED = ['Coffre', 'Contacts', 'Calendrier', 'Partages', 'Sauvegardes'];
const segs = p => p.split('/').filter(Boolean);
const locked = p => segs(p).some((s, i) => (i === 0 && RESERVED.includes(s)) || s.startsWith('.'));
const lastName = p => segs(p).pop() || '';
const parent = p => '/' + segs(p).slice(0, -1).join('/');
const join = (d, n) => (d === '/' ? '' : d) + '/' + n;
const badName = n => !n || n === '.' || n === '..' || n.includes('/') || n.startsWith('.') || n.length > 255;
const WHY = { 403: 'Dossier protégé', 404: 'Introuvable', 409: 'Dossier de destination introuvable', 412: 'Un élément porte déjà ce nom', 413: 'Fichier trop volumineux', 423: 'Élément verrouillé', 507: 'Espace insuffisant' };
// « photo.jpg » pris → « photo (2).jpg » : un dépôt n'écrase jamais un fichier existant.
const unique = (name, taken) => {
  if (!taken.has(name)) return name;
  const i = name.lastIndexOf('.'), base = i > 0 ? name.slice(0, i) : name, tail = i > 0 ? name.slice(i) : '';
  for (let n = 2; ; n++) if (!taken.has(`${base} (${n})${tail}`)) return `${base} (${n})${tail}`;
};

let K, dav = '/dav', reload = () => {}, panel = null, queue = [], running = 0, total = 0, done = 0, failed = [];
const dest = p => location.origin + dav + K.enc(p);

async function mv(from, to) {
  const r = await K.req(dav + K.enc(from), { method: 'MOVE', headers: { Destination: dest(to), Overwrite: 'F' } });
  if (!r.ok) throw new Error(K.t(WHY[r.status] || `Erreur ${r.status}`));
}
async function mkcol(p) {
  const r = await K.req(dav + K.enc(p), { method: 'MKCOL' });
  if (!r.ok && r.status !== 405) throw new Error(K.t(WHY[r.status] || `Erreur ${r.status}`)); // 405 : le dossier existe déjà
}
async function rm(p) {
  const r = await K.req(dav + K.enc(p), { method: 'DELETE' });
  if (!r.ok) throw new Error(K.t(WHY[r.status] || `Erreur ${r.status}`));
}
async function names(dir) { return new Set((await K.api('/api/v1/ucloud/list?path=' + encodeURIComponent(dir))).entries.map(e => e.name)); }

// ── Téléversement : XMLHttpRequest, seul moyen d'avoir la progression de l'envoi sur tous les navigateurs mobiles.
async function put(file, to, onProgress) {
  const url = new URL(dav + K.enc(to), location.origin).href, auth = await K.auth('PUT', url);
  return new Promise((ok, ko) => {
    const x = new XMLHttpRequest();
    x.open('PUT', url);
    if (auth) x.setRequestHeader('Authorization', auth);
    x.setRequestHeader('Content-Type', file.type || 'application/octet-stream');
    x.upload.onprogress = e => e.lengthComputable && onProgress(e.loaded / e.total);
    x.onload = () => (x.status < 300 ? ok() : ko(new Error(K.t(WHY[x.status] || `Erreur ${x.status}`))));
    x.onerror = () => ko(new Error(K.t('Connexion interrompue')));
    x.send(file);
  });
}
function showPanel() {
  const { h } = K;
  if (panel?.isConnected) return panel;
  panel = h('div', { class: 'fm-up', role: 'status', 'aria-live': 'polite' });
  document.body.append(panel);
  return panel;
}
function paint(lines) {
  const { h, t } = K, p = showPanel(), over = running === 0 && !queue.length;
  p.replaceChildren(h('strong', {}, over ? t('Téléversement terminé') : t('Téléversement : {0} / {1}', done, total)),
    ...(over ? [h('button', { class: 'ghost x', 'aria-label': t('Fermer'), onclick: () => { p.remove(); failed = []; } }, '×')] : []),
    ...lines.map(l => h('div', { class: 'fm-line' }, h('span', {}, l.name), h('progress', { max: 1, value: l.value }))),
    ...failed.map(f => h('p', { class: 'err' }, `${f.name} : ${f.why}`)));
}
const live = new Set();
async function pump() {
  while (running < 3 && queue.length) {
    const job = queue.shift(), line = { name: lastName(job.to), value: 0 };
    running++; live.add(line);
    put(job.file, job.to, v => { line.value = v; paint([...live]); })
      .catch(e => failed.push({ name: line.name, why: e.message }))
      .finally(() => { running--; done++; live.delete(line); paint([...live]); if (queue.length) pump(); else if (!running) finish(); });
    paint([...live]);
  }
}
function finish() {
  K.toast(failed.length ? K.t('{0} fichier(s) non envoyé(s)', failed.length) : K.t('Fichiers envoyés'), !failed.length);
  if (location.hash.startsWith('#/cloud')) reload();
  total = done = 0;
}
// `list` : [{ file, rel }] où `rel` est le chemin relatif au dossier cible (« Album/été/a.jpg » pour un dossier déposé).
async function upload(target, list) {
  const { t, toast } = K;
  if (locked(target)) return toast(t('Ce dossier est géré par la station : on n\'y dépose rien à la main.'));
  if (!list.length) return;
  try {
    await K.api('/api/v1/me'); // rouvre la connexion si elle a expiré, avant de lancer les envois
    const taken = await names(target), top = new Map(); // premier niveau rendu unique (fichier ou dossier), jamais d'écrasement
    const base = target === '/' ? '' : target, dirs = new Set(), jobs = [];
    for (const { file, rel } of list) {
      const parts = rel.split('/');
      if (badName(parts[0]) && parts[0].startsWith('.')) { failed.push({ name: rel, why: t('Fichier caché ignoré') }); continue; }
      if (!top.has(parts[0])) { const u = unique(parts[0], taken); taken.add(u); top.set(parts[0], u); }
      parts[0] = top.get(parts[0]);
      for (let i = 1; i < parts.length; i++) dirs.add(base + '/' + parts.slice(0, i).join('/'));
      jobs.push({ file, to: base + '/' + parts.join('/') });
    }
    for (const d of [...dirs].sort((a, b) => a.length - b.length)) await mkcol(d);
    total += jobs.length; queue.push(...jobs);
    if (!jobs.length) return paint([...live]);
    pump();
  } catch (e) { toast(e.message); }
}
// Dossier déposé : on parcourt l'arborescence fournie par le navigateur (les entrées doivent être lues avant tout `await`).
async function walk(en, rel, out) {
  if (en.isFile) return out.push({ file: await new Promise((ok, ko) => en.file(ok, ko)), rel: rel + en.name });
  const rd = en.createReader();
  for (let b; (b = await new Promise((ok, ko) => rd.readEntries(ok, ko))).length;) for (const c of b) await walk(c, rel + en.name + '/', out);
}
async function fromDrop(dt) {
  const ens = [...dt.items].filter(i => i.kind === 'file').map(i => i.webkitGetAsEntry?.() || i.getAsFile()), out = [];
  for (const en of ens) { if (en instanceof File) out.push({ file: en, rel: en.name }); else if (en) await walk(en, '', out); }
  return out;
}

// ── Choix du dossier de destination (arborescence navigable).
function chooser(srcs) {
  const { h, t, toast } = K;
  const dlg = h('dialog', { class: 'fm-dlg', 'aria-label': t('Déplacer vers…') }), list = h('ul', { class: 'files' }), cur = h('p', { class: 'muted' });
  const go = h('button', {}, t('Déplacer ici'));
  let at = '/';
  const inside = d => srcs.some(s => d === s || d.startsWith(s + '/'));
  const draw = async d => {
    at = d; cur.textContent = d;
    go.disabled = locked(d) || inside(d) || srcs.every(s => parent(s) === d);
    try {
      const r = await K.api('/api/v1/ucloud/list?path=' + encodeURIComponent(d));
      list.replaceChildren(...(d === '/' ? [] : [h('li', {}, h('button', { class: 'link', onclick: () => draw(parent(d)) }, '↑ ' + t('Dossier parent')))]),
        ...r.entries.filter(x => x.type === 'dir' && !locked(x.path) && !inside(x.path)).map(x => h('li', {}, h('span', { 'aria-hidden': 'true' }, '📁'), h('button', { class: 'link', onclick: () => draw(x.path) }, x.name))));
      if (!list.children.length) list.append(h('li', { class: 'muted' }, t('Aucun sous-dossier')));
    } catch (e) { toast(e.message); }
  };
  go.onclick = async () => {
    go.disabled = true;
    const bad = [];
    for (const s of srcs) { try { await mv(s, join(at, lastName(s))); } catch (e) { bad.push(`${lastName(s)} : ${e.message}`); } }
    dlg.close();
    toast(bad.length ? bad[0] : t('Déplacé'), !bad.length);
    reload();
  };
  dlg.append(h('h2', {}, t('Déplacer {0} élément(s) vers…', srcs.length)), cur, list, h('menu', {}, h('button', { class: 'ghost', onclick: () => dlg.close() }, t('Annuler')), go));
  dlg.onclose = () => dlg.remove();
  document.body.append(dlg); dlg.showModal(); draw('/');
}

export default function files(root, toolbox, ctx) {
  K = toolbox; dav = ctx.dav; reload = ctx.reload;
  const { h, t, toast } = K, path = ctx.path;
  if (!document.querySelector('link[href="/ui/m-files.css"]')) document.head.append(h('link', { rel: 'stylesheet', href: '/ui/m-files.css' }));
  const bar = root.children[1], rows = () => [...root.querySelectorAll('li[data-path]')];
  const fail = e => toast(e.message);
  const open = !locked(path);

  // Barre d'outils : ＋ Ajouter (menu), puis les actions groupées sur la sélection.
  const pick = (attrs, then) => { const i = h('input', { type: 'file', hidden: true, ...attrs, onchange: () => { then([...i.files]); i.value = ''; } }); return i; };
  const send = fs => upload(path, fs.map(file => ({ file, rel: (file.webkitRelativePath || file.name) })));
  const inFiles = pick({ multiple: true }, send), inDir = pick({ webkitdirectory: true }, send), inCam = pick({ accept: 'image/*,video/*', capture: 'environment' }, send);
  const nameIn = h('input', { 'aria-label': t('Nom du dossier'), placeholder: t('Nom du nouveau dossier'), onkeydown: ev => ev.key === 'Enter' && makeDir() });
  const makeDir = async () => {
    const n = nameIn.value.trim();
    if (badName(n)) return toast(t('Nom invalide : ni « / », ni point au début.'));
    try { if ((await names(path)).has(n)) return toast(t('Un élément porte déjà ce nom')); await mkcol(join(path, n)); reload(); } catch (e) { fail(e); }
  };
  const menu = h('div', { class: 'card fm-menu', hidden: true },
    h('div', { class: 'bar' },
      h('button', { onclick: () => inFiles.click() }, '⬆ ' + t('Téléverser des fichiers')),
      h('button', { class: 'ghost', onclick: () => inDir.click() }, t('Un dossier')),
      h('button', { class: 'ghost', onclick: () => inCam.click() }, t('Prendre une photo'))),
    h('div', { class: 'bar' }, nameIn, h('button', { class: 'ghost', onclick: makeDir }, '＋ ' + t('Nouveau dossier'))),
    inFiles, inDir, inCam);
  const sel = () => [...root.querySelectorAll('li input:checked')].map(i => i.closest('li').dataset.path);
  const mvSel = h('button', { class: 'ghost', disabled: true, onclick: () => chooser(sel()) }, t('Déplacer…'));
  const delSel = K.sure(t('Supprimer'), async () => { await delAll(sel()); }, t('Définitif : la suppression ne peut pas être annulée.'));
  const delBtn = delSel.querySelector('button'); delBtn.disabled = true;
  const sync = () => { const s = sel(), ok = s.length && !s.some(locked); mvSel.disabled = delBtn.disabled = !ok; };
  root.addEventListener('change', sync);
  if (open) {
    const add = h('button', { 'aria-expanded': 'false', onclick: () => { menu.hidden = !menu.hidden; add.setAttribute('aria-expanded', String(!menu.hidden)); } }, '＋ ' + t('Ajouter'));
    bar.querySelector('.sp')?.after(add, mvSel, delSel);
    bar.after(menu);
  }
  const delAll = async ps => {
    const bad = [];
    for (const p of ps) { try { await rm(p); } catch (e) { bad.push(`${lastName(p)} : ${e.message}`); } }
    toast(bad.length ? bad[0] : t('Supprimé'), !bad.length);
    reload();
  };

  // Actions sur chaque ligne : ⋯ → Renommer, Déplacer, Supprimer (rien sur les dossiers réservés ni dans un dossier réservé).
  const rename = li => {
    const p = li.dataset.path, old = lastName(p), btn = li.querySelector('button.link'), dot = old.lastIndexOf('.');
    const back = () => inp.replaceWith(btn);
    const ok = async () => {
      const n = inp.value.trim();
      if (n === old) return back();
      if (badName(n)) return toast(t('Nom invalide : ni « / », ni point au début.'));
      try { await mv(p, join(parent(p), n)); reload(); } catch (e) { fail(e); }
    };
    const inp = h('input', { value: old, 'aria-label': t('Nouveau nom'), onkeydown: ev => { if (ev.key === 'Enter') ok(); else if (ev.key === 'Escape') back(); } });
    btn.replaceWith(inp); inp.focus(); inp.setSelectionRange(0, dot > 0 && li.dataset.dir == null ? dot : old.length);
  };
  for (const li of rows()) {
    const p = li.dataset.path;
    if (locked(p)) { li.append(h('span', { class: 'muted', title: t('Géré par la station'), 'aria-label': t('Géré par la station') }, '🔒')); continue; }
    const more = h('button', { class: 'ghost fm-more', 'aria-label': t('Actions pour {0}', lastName(p)), 'aria-expanded': 'false', onclick: () => {
      const box = li.querySelector('.fm-act');
      if (box) { box.remove(); more.setAttribute('aria-expanded', 'false'); return; }
      more.setAttribute('aria-expanded', 'true');
      li.append(h('div', { class: 'fm-act' }, h('button', { class: 'ghost', onclick: () => rename(li) }, t('Renommer')), h('button', { class: 'ghost', onclick: () => chooser([p]) }, t('Déplacer…')),
        K.sure(t('Supprimer'), () => delAll([p]), t('Définitif : la suppression ne peut pas être annulée.'))));
    } }, '⋯');
    li.append(more);
  }

  // Glisser-déposer : sur la liste (dossier courant) ou sur un sous-dossier. Les fichiers du dessus seuls déclenchent la zone de dépôt.
  const files = ev => ev.dataTransfer?.types?.includes('Files');
  const target = ev => ev.target.closest?.('li[data-dir]');
  let hot = null;
  const light = el => { if (hot !== el) { hot?.classList.remove('fm-drop'); hot = el; hot?.classList.add('fm-drop'); } };
  root.addEventListener('dragover', ev => { if (!files(ev)) return; ev.preventDefault(); light(target(ev) || root); });
  root.addEventListener('dragleave', ev => { if (!ev.relatedTarget || !root.contains(ev.relatedTarget)) light(null); });
  root.addEventListener('drop', async ev => {
    if (!files(ev)) return;
    ev.preventDefault();
    const to = target(ev)?.dataset.path || path;
    light(null);
    try { upload(to, await fromDrop(ev.dataTransfer)); } catch (e) { fail(e); }
  });
}
