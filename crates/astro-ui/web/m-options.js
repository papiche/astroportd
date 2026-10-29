'use strict';
// Options de la station (Capitaine) : moteur FaceID commutable à chaud et matrice du relai par kind. Rien ne redémarre :
// la station applique le réglage dès qu'il est enregistré, et le conserve.
const READ = { public: 'Tous', addressed: 'Auteur et destinataires', friends: 'Auteur et ses Proches', circle: 'Cercle social', deny: 'Personne' };
const WRITE = { circle: 'Cercle social', open: 'Tous (débit limité)', deny: 'Personne' };
const MODE = { off: 'Arrêté', local: 'ONNX local (cette machine)', remote: 'Brain GPU distant' };

export default async function options(root, K) {
  const { h } = K, api = p => K.api('/api/v1/station/admin/' + p), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  root.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
  const [face, kinds] = await Promise.all([api('face'), api('kinds')]);
  const ok = (v, text) => h('li', {}, v ? '✔ ' : '✘ ', text);

  // ── Moteur FaceID ──
  const url = h('input', { id: 'fo-url', placeholder: 'http://brain.local:8190/faces', value: face.url || '', autocomplete: 'off', disabled: !!face.locked });
  const radios = Object.entries(MODE).map(([m, label]) => h('label', {}, h('input', { type: 'radio', name: 'fo-mode', value: m, checked: face.mode === m, disabled: !!face.locked }), ' ' + label));
  const apply = h('button', { disabled: !!face.locked, onclick: async ev => {
    const mode = radios.map(r => r.firstChild).find(r => r.checked)?.value;
    ev.target.disabled = true;
    try { await post('face', { mode, url: url.value.trim() }); K.toast('Moteur FaceID appliqué', 1); options(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, 'Appliquer');
  const running = face.label ? `En service : ${face.label}` : 'Aucun moteur en service : les nouvelles photos attendent.';
  const faceCard = h('section', { class: 'card' }, h('h2', {}, 'Reconnaissance des visages'),
    h('p', { class: face.label ? '' : 'degraded', role: 'status' }, running),
    face.locked ? h('p', { class: 'muted' }, `Imposé par ${face.locked} sur la ligne de commande : retirez l'option pour le régler ici.`) : null,
    h('fieldset', { class: 'coop-s' }, h('legend', {}, 'Moteur'), radios, h('label', { for: 'fo-url' }, 'Adresse du Brain (mode distant)'), url),
    h('ul', { class: 'muted' }, ok(face.onnx_compiled, 'fonction ONNX compilée dans ce binaire'), ok(face.models, face.models ? `modèles : ${face.models}` : 'modèles introuvables (det_10g.onnx, w600k_r50.onnx)'), ok(face.library, face.library ? `onnxruntime : ${face.library}` : 'libonnxruntime introuvable (ORT_DYLIB_PATH)')),
    h('p', { class: 'muted' }, `${face.pending} photo(s) en attente · ${face.failed} abandonnée(s).`), h('p', {}, apply));

  // ── Matrice des kinds ──
  const sel = (choices, labels, cur) => h('select', {}, choices.map(c => h('option', { value: c, selected: c === cur }, labels[c] || c)));
  const rows = kinds.kinds.map(k => {
    const read = sel(kinds.choices.read, READ, k.read), write = sel(kinds.choices.write, WRITE, k.write), proj = h('input', { type: 'checkbox', checked: k.project, 'aria-label': `Projection du kind ${k.kind}` });
    const spec = () => `read=${read.value} write=${write.value} project=${proj.checked ? 'on' : 'off'}`;
    return { k, read, write, proj, spec, changed: () => spec() !== `read=${k.read} write=${k.write} project=${k.project ? 'on' : 'off'}` };
  });
  const addNo = h('input', { type: 'number', min: 0, max: 65535, placeholder: 'kind', 'aria-label': 'Ajouter un kind' });
  const save = h('button', { onclick: async ev => {
    const rules = {};
    rows.filter(r => r.changed()).forEach(r => { rules[r.k.kind] = r.spec(); });
    if (addNo.value) rules[addNo.value] = 'read=circle write=circle project=on';
    if (!Object.keys(rules).length) return K.toast('Aucune modification');
    ev.target.disabled = true;
    try { await post('kinds', { rules }); K.toast('Matrice du relai appliquée', 1); options(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, 'Appliquer la matrice');
  const kindsCard = h('section', { class: 'card wide' }, h('h2', {}, 'Relai : lecture, écriture, projection par kind'),
    h('p', { class: 'muted' }, 'Lecture : qui reçoit les événements. Écriture : qui peut en publier. Projection : l\'événement alimente-t-il les tables dérivées (comptes, ledger…) ; il reste stocké et servi. S\'applique tout de suite.'),
    h('table', {}, h('caption', { class: 'muted' }, 'Un kind sans réglage suit les défauts du relai.'), h('thead', {}, h('tr', {}, ['Kind', 'Lecture', 'Écriture', 'Projeté', 'Origine'].map(x => h('th', { scope: 'col' }, x)))),
      h('tbody', {}, rows.map(r => h('tr', {}, h('td', {}, h('strong', {}, String(r.k.kind)), r.k.label ? ` ${r.k.label}` : ''), h('td', {}, r.read), h('td', {}, r.write), h('td', {}, r.proj), h('td', { class: 'muted' }, r.k.source,
        r.k.source === 'capitaine' ? [' ', h('button', { class: 'ghost', onclick: async () => { try { await post('kinds', { reset: [r.k.kind] }); options(root, K); } catch (e) { K.toast(e.message); } } }, 'Défaut')] : null))))),
    h('p', {}, addNo, ' ', save), h('p', { class: 'muted' }, `Fixes : 1059 — ${kinds.fixed['1059']} ; 22242 — ${kinds.fixed['22242']}.`));

  root.replaceChildren(h('h2', {}, 'Options de la station'), h('div', { class: 'grid' }, faceCard, kindsCard));
}
