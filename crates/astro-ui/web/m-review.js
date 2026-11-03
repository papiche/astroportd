'use strict';
// Trier en grand (chargé au clic depuis la table de tri) : une question à la fois, en plein écran, sur l'état déjà chargé
// de la table. D'abord les ressemblances « est-ce X ? » (de la plus sûre à la moins sûre), puis les groupes anonymes, du plus
// nombreux au plus petit (« Qui est-ce ? ») : la file vient de m-people.js. « Non » transforme la question en « Qui est-ce
// alors ? ». Les réponses partent une à une ; la table n'est rechargée qu'en quittant, si quelque chose a changé.
let K, S, X, el;

async function thumbs(ids, imgs) {
  try { const r = await K.api('/api/v1/faces/thumbs', { method: 'POST', body: { ids, size: X.size } }); ids.forEach((id, i) => { if (r.thumbs[id] && imgs[i]) imgs[i].src = r.thumbs[id]; }); } catch { /* les cercles vides suffisent */ }
}
function close() {
  if (!S.rv) return;
  const changed = S.rv.changed; S.rv.off(); S.rv = null;
  if (changed) X.load().catch(e => K.toast(e.message));
}
const item = () => S.rv.q[S.rv.i];
const active = () => item().ids.filter(id => !S.rv.out.has(id));
function next() { S.rv.i++; S.rv.out = new Set(); draw(); }
// `tg` : la fiche où ranger ({ name, contact } choisie, { name } nouvelle ou unique, { name, new } homonyme voulu) ; null = annulé.
async function assign(tg) {
  const ids = active();
  if (!tg) return;
  if (!ids.length) return K.toast('Tous les visages sont écartés : passez à la suite.');
  try {
    const r = await K.api('/api/v1/faces/assign', { method: 'POST', body: { ids, ...tg } });
    ids.forEach(id => { const f = X.faceOf(id); if (f) Object.assign(f, { named: true, name: r.name, contact: r.contact?.uid || f.contact, pubkey: tg.pubkey || f.pubkey }); });
    S.rv.done += r.assigned; S.rv.changed = true;
    K.toast(`${r.assigned} visage(s) rangé(s) dans « ${r.name} »${r.contact?.created ? ' · nouvelle fiche' : ''}`, 1);
    next();
  } catch (e) { K.toast(e.message); }
}
// « Oui » : la fiche suggérée (son UID) ; une suggestion d'avant les fiches n'a que le nom, qui passe par le choix entre homonymes.
function yes() { const s = item().sug; s.contact ? assign({ name: s.name, contact: s.contact }) : X.target(s.name).then(assign); }
function no() { const it = item(); it.ids.forEach(id => S.no.add(id)); it.refused = true; S.rv.mode = 'name'; question(); }
async function forget() {
  const ids = active();
  if (!ids.length || !confirm(`Ne plus reconnaître ${ids.length} visage(s) ? Les photos restent dans votre uCloud.`)) return;
  try { await K.api('/api/v1/faces/forget', { method: 'POST', body: { ids } }); S.rv.changed = true; next(); } catch (e) { K.toast(e.message); }
}

function draw() {
  const h = K.h, it = item(), total = S.rv.q.length, stage = el.querySelector('.pe-rv-stage');
  el.querySelector('.pe-rv-n').textContent = total ? `${Math.min(S.rv.i + 1, total)} / ${total}` : '';
  Object.assign(el.querySelector('progress'), { max: total || 1, value: S.rv.i });
  if (!it) {
    stage.replaceChildren(h('div', { class: 'pe-rv-done' }, h('h2', {}, 'Plus rien à trier ici.'),
      h('p', {}, (total ? `Vous avez parcouru les ${total} questions` : 'Aucune ressemblance ni aucun groupe à identifier pour l\'instant') + (S.rv.done ? ` et rangé ${S.rv.done} visage(s).` : '.') + ' Les visages isolés restent sur la table.'),
      h('button', { onclick: close }, 'Retour à la table')));
    return;
  }
  S.rv.mode = it.sug && !it.refused ? 'ask' : 'name';
  const shown = it.ids.slice(0, 24), many = it.ids.length > 1, big = h('div', { class: 'pe-rv-photo' });
  const imgs = shown.map(() => h('img', { alt: '', width: 96, height: 96 }));
  const faces = h('div', { class: 'pe-rv-faces' + (many ? ' many' : '') }, shown.map((id, i) => h('figure', { 'data-id': id },
    h('button', { type: 'button', class: 'pe-rv-face', title: 'Voir sa photo', onclick: () => show(id) }, imgs[i]),
    many && h('button', { type: 'button', class: 'pe-rv-x', title: 'Écarter ce visage : ce n\'est pas la même personne', 'aria-label': 'Écarter', onclick: ev => { const fig = ev.currentTarget.parentElement; S.rv.out.has(id) ? S.rv.out.delete(id) : S.rv.out.add(id); fig.classList.toggle('out', S.rv.out.has(id)); question(); } }, '×'))),
    it.ids.length > 24 && h('p', { class: 'muted' }, `+ ${it.ids.length - 24} autres`));
  // Photo entière du visage choisi, le visage entouré : pour savoir qui est désigné sur une photo de groupe.
  const show = id => { big.replaceChildren(X.framed(X.faceOf(id))); faces.querySelectorAll('figure').forEach(x => x.classList.toggle('on', +x.dataset.id === id)); };
  stage.replaceChildren(h('div', { class: 'pe-rv-left' }, faces, big), h('div', { class: 'pe-rv-right' }));
  thumbs(shown, imgs); show(shown[0]); question();
}

function question() {
  const h = K.h, it = item(), n = active().length, right = el.querySelector('.pe-rv-right'), who = n > 1 ? `ces ${n} visages` : 'ce visage';
  const minor = h('div', { class: 'pe-rv-minor' }, h('button', { class: 'ghost', onclick: next }, h('kbd', {}, 'Espace'), ' Passer'), h('button', { class: 'ghost danger', onclick: forget }, 'Pas à reconnaître'));
  if (S.rv.mode === 'ask') {
    const s = it.sug, refs = S.faces.filter(f => f.named && (s.contact ? f.contact === s.contact : f.name === s.name)).slice(0, 6), imgs = refs.map(() => h('img', { alt: '', width: 72, height: 72 }));
    right.replaceChildren(h('h2', {}, n > 1 ? `Ces ${n} visages sont-ils ` : 'Est-ce ', h('strong', {}, s.name), ' ?'),
      refs.length ? h('div', { class: 'pe-rv-refs' }, refs.map((f, i) => h('button', { type: 'button', title: 'Voir la photo', onclick: () => X.photo(f.id) }, imgs[i]))) : null,
      h('p', { class: 'muted' }, `${s.name}${refs.length ? ' tel que vous l\'avez déjà rangé' : ''} · ressemblance ${Math.round(s.score * 100)} %`),
      h('div', { class: 'pe-rv-act' }, h('button', { class: 'ghost pe-rv-big', onclick: no }, h('kbd', {}, 'N'), ' Non'), h('button', { class: 'pe-rv-big', onclick: yes }, h('kbd', {}, 'O'), ` Oui, c'est ${s.name}`)), minor);
    if (refs.length) thumbs(refs.map(f => f.id), imgs);
    return;
  }
  const inp = h('input', { required: true, maxLength: 80, placeholder: 'Son nom', 'aria-label': 'Nom de la personne', autocomplete: 'off' });
  inp.setAttribute('list', 'pe-l'); // les noms de vos Personnes (fiches et pochettes)
  X.root.querySelector('#pe-l').replaceChildren(...X.names().map(v => h('option', { value: v })));
  const top = [...S.people].sort((a, b) => b.photos - a.photos).slice(0, 12);
  right.replaceChildren(h('h2', {}, it.refused ? 'Qui est-ce alors ?' : 'Qui est-ce ?'),
    n > 1 ? h('p', { class: 'muted' }, `${n} visages semblables. Écartez d'un × ceux qui ne sont pas la même personne.`) : null,
    h('form', { class: 'pe-rv-form', onsubmit: ev => { ev.preventDefault(); const v = inp.value.trim(); if (v) X.target(v).then(assign); } }, inp, h('button', {}, `Ranger ${who}`)),
    top.length ? [h('p', { class: 'muted' }, 'Ou dans une pochette existante :'), h('div', { class: 'pe-rv-chips' }, top.map(p => h('button', { type: 'button', class: 'ghost', onclick: () => assign({ name: p.name, ...(p.contact && { contact: p.contact }), ...(p.pubkey && { pubkey: p.pubkey }) }) }, p.name)))] : null, minor);
  setTimeout(() => inp.focus(), 30);
}

export default function review(ctx) {
  ({ K, S } = ctx); X = ctx;
  if (S.rv) return;
  const h = K.h;
  el = h('section', { class: 'pe-rv', 'aria-label': 'Trier en grand' },
    h('header', { class: 'pe-rv-top' }, h('button', { class: 'ghost', onclick: close }, '✕ Quitter'), h('span', { class: 'pe-rv-n' }), h('progress', { max: 1, value: 0 }),
      h('span', { class: 'pe-rv-keys muted' }, h('kbd', {}, 'O'), ' oui ', h('kbd', {}, 'N'), ' non ', h('kbd', {}, 'Espace'), ' passer ', h('kbd', {}, 'Échap'), ' quitter')),
    h('div', { class: 'pe-rv-stage' }));
  const keys = e => {
    if (!S.rv || document.querySelector('dialog[open]')) return; // la photo ouverte par-dessus garde ses touches
    const typing = /INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName || '');
    if (e.key === 'Escape') { e.preventDefault(); close(); }
    else if (typing) return;
    else if (S.rv.mode === 'ask' && (e.key === 'o' || e.key === 'O')) { e.preventDefault(); yes(); }
    else if (S.rv.mode === 'ask' && (e.key === 'n' || e.key === 'N')) { e.preventDefault(); no(); }
    else if (e.key === ' ') { e.preventDefault(); next(); }
  };
  S.rv = { q: ctx.queue(), i: 0, out: new Set(), done: 0, mode: 'ask', changed: false,
    off: () => { document.removeEventListener('keydown', keys); el.remove(); document.body.classList.remove('pe-reviewing'); } };
  X.root.append(el); document.body.classList.add('pe-reviewing');
  document.addEventListener('keydown', keys);
  draw();
}
