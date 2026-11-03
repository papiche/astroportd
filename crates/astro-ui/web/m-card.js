'use strict';
// Fiche d'un contact, affichée et modifiée au même endroit. Pour « Ma fiche », le profil Nostr et la fiche chiffrée sont réunis : un seul
// formulaire, les champs publics partent aussi sur Nostr, les champs privés (téléphones, adresses, anniversaire, notes) restent dans la fiche.

const KINDS = [['mobile', 'Mobile'], ['work', 'Travail'], ['home', 'Maison'], ['other', 'Autre']];
const copy = o => JSON.parse(JSON.stringify(o));

// Photo de la fiche : envoyer une image (réduite ici à 256 px), prendre l'un des visages reconnus de la personne, ou la retirer.
async function photoEditor(K, box, path, onSaved) {
  const { h, t, api, toast } = K;
  if (!box.hidden) { box.hidden = true; return; }
  box.hidden = false;
  const save = async uri => { try { await api('/api/v1/book/photo', { method: 'POST', body: { path, uri } }); toast(uri ? t('Photo enregistrée') : t('Photo retirée'), 1); onSaved(path, true); } catch (e) { toast(e.message); } };
  const file = h('input', { type: 'file', accept: 'image/*', hidden: true, onchange: async () => {
    const f = file.files[0]; if (!f) return;
    try {
      const bmp = await createImageBitmap(f), k = Math.min(1, 256 / Math.max(bmp.width, bmp.height)), c = document.createElement('canvas');
      c.width = Math.max(1, Math.round(bmp.width * k)); c.height = Math.max(1, Math.round(bmp.height * k)); c.getContext('2d').drawImage(bmp, 0, 0, c.width, c.height);
      save(c.toDataURL('image/jpeg', .85));
    } catch (e) { toast(t('Image illisible')); }
    file.value = '';
  } });
  const faces = h('div', { class: 'ab-faces' }, h('span', { class: 'muted' }, t('Visages reconnus…')));
  box.replaceChildren(h('p', {}, h('button', { type: 'button', onclick: () => file.click() }, t('Envoyer une image')), ' ', h('button', { type: 'button', class: 'ghost danger', onclick: () => save(null) }, t('Retirer la photo')), file), faces);
  try {
    const ids = (await api('/api/v1/book/photo-choices?path=' + encodeURIComponent(path))).faces;
    if (!ids.length) { faces.replaceChildren(h('span', { class: 'muted' }, t('Aucun visage reconnu pour cette personne : envoyez une image.'))); return; }
    const th = (await api('/api/v1/faces/thumbs', { method: 'POST', body: { ids, size: 128 } })).thumbs;
    faces.replaceChildren(h('span', { class: 'muted' }, t('Choisir parmi ses visages :')), ' ', ...ids.filter(i => th[i]).map(i => h('button', { type: 'button', class: 'ab-face', 'aria-label': t('Utiliser ce visage'), onclick: () => save(th[i]) }, h('img', { src: th[i], alt: '', width: 56, height: 56 }))));
  } catch (e) { faces.replaceChildren(h('span', { class: 'muted' }, e.message)); }
}

export default async function card(K, { path, me, blank, groups, onSaved, onClose }) {
  const { h, t } = K, api = K.api;
  const d = blank ? { fields: { name: '', phones: [], emails: [], addresses: [], urls: [], groups: [] }, me: false, path: null, diff: null } : await api(me ? '/api/v1/book/me' : '/api/v1/book/card?path=' + encodeURIComponent(path));
  const embedded = (path, name) => {
    const av = h('span', { class: 'ab-av' }, (name || '?')[0].toUpperCase());
    if (path) api('/api/v1/book/photo?path=' + encodeURIComponent(path)).then(r => { if (r.uri) av.replaceChildren(h('img', { src: r.uri, alt: '', width: 64, height: 64, class: 'ab-photo' })); }, () => {});
    return av;
  };
  const f = copy(d.fields), isMe = !!d.me, pub = isMe || !!d.pubkey;
  const box = h('form', { class: 'ab-card', onsubmit: async ev => { ev.preventDefault(); await save(); } });
  const text = (key, label, opts = {}) => {
    const i = h(opts.area ? 'textarea' : 'input', { id: 'f-' + key, value: f[key] || '', rows: opts.area ? 3 : null, maxlength: opts.max || 200, autocomplete: 'off', ...(opts.area ? {} : { type: opts.type || 'text' }), placeholder: opts.ph || '', oninput: () => { f[key] = i.value; } });
    if (opts.area) i.textContent = f[key] || '';
    return h('div', { class: 'ab-f' }, h('label', { for: 'f-' + key }, label, opts.public && pub ? h('small', { class: 'pill ok' }, t('public')) : null), i);
  };
  // Listes : téléphones, emails (type + valeur), sites, adresses.
  const typedList = (key, label, ph, type) => {
    const wrap = h('div', { class: 'ab-f' }, h('label', {}, label)), list = h('div', {});
    const draw = () => {
      list.replaceChildren(...f[key].map((x, i) => h('div', { class: 'ab-row' },
        h('select', { 'aria-label': t('Type'), onchange: ev => { x.kind = ev.target.value; } }, KINDS.map(([v, l]) => h('option', { value: v, selected: x.kind === v }, t(l)))),
        h('input', { value: x.value, type, placeholder: ph, 'aria-label': label, maxlength: 120, oninput: ev => { x.value = ev.target.value; } }),
        h('button', { type: 'button', class: 'ghost', 'aria-label': t('Retirer'), onclick: () => { f[key].splice(i, 1); draw(); } }, '×'))));
    };
    draw();
    wrap.append(list, h('button', { type: 'button', class: 'ghost', onclick: () => { f[key].push({ kind: 'mobile', value: '' }); draw(); const ins = list.querySelectorAll('input'); ins[ins.length - 1]?.focus(); } }, '+ ' + label.toLowerCase()));
    return wrap;
  };
  const urls = () => {
    const wrap = h('div', { class: 'ab-f' }, h('label', {}, t('Sites web'), pub ? h('small', { class: 'pill ok' }, t('le premier est public')) : null)), list = h('div', {});
    const draw = () => list.replaceChildren(...f.urls.map((u, i) => h('div', { class: 'ab-row' }, h('input', { value: u, type: 'url', 'aria-label': t('Site web'), maxlength: 256, oninput: ev => { f.urls[i] = ev.target.value; } }), h('button', { type: 'button', class: 'ghost', 'aria-label': t('Retirer'), onclick: () => { f.urls.splice(i, 1); draw(); } }, '×'))));
    draw(); wrap.append(list, h('button', { type: 'button', class: 'ghost', onclick: () => { f.urls.push(''); draw(); } }, '+ ' + t('site')));
    return wrap;
  };
  const addresses = () => {
    const wrap = h('div', { class: 'ab-f' }, h('label', {}, t('Adresses'))), list = h('div', {});
    const draw = () => list.replaceChildren(...f.addresses.map((a, i) => h('div', { class: 'ab-addr' },
      h('select', { 'aria-label': t('Type'), onchange: ev => { a.kind = ev.target.value; } }, KINDS.filter(k => k[0] !== 'mobile').map(([v, l]) => h('option', { value: v, selected: a.kind === v }, t(l)))),
      ...[['street', 'Rue'], ['code', 'Code postal'], ['city', 'Ville'], ['country', 'Pays']].map(([k, l]) => h('input', { value: a[k] || '', placeholder: t(l), 'aria-label': t(l), maxlength: 120, oninput: ev => { a[k] = ev.target.value; } })),
      h('button', { type: 'button', class: 'ghost', 'aria-label': t('Retirer'), onclick: () => { f.addresses.splice(i, 1); draw(); } }, '×'))));
    draw(); wrap.append(list, h('button', { type: 'button', class: 'ghost', onclick: () => { f.addresses.push({ kind: 'home' }); draw(); } }, '+ ' + t('adresse')));
    return wrap;
  };
  const groupsEditor = () => {
    const wrap = h('div', { class: 'ab-f' }, h('label', { for: 'f-group' }, t('Groupes'))), chips = h('span', {}), inp = h('input', { id: 'f-group', placeholder: t('Ajouter un groupe'), maxlength: 40, autocomplete: 'off' });
    const draw = () => chips.replaceChildren(...f.groups.map((g, i) => h('button', { type: 'button', class: 'chip', title: t('Retirer'), onclick: () => { f.groups.splice(i, 1); draw(); } }, g + ' ×')));
    const add = () => { const v = inp.value.trim(); if (v && !f.groups.includes(v)) f.groups.push(v); inp.value = ''; draw(); };
    inp.onkeydown = ev => { if (ev.key === 'Enter') { ev.preventDefault(); add(); } }; inp.onchange = add;
    inp.setAttribute('list', 'ab-groups'); // `list` est en lecture seule côté DOM
    draw(); wrap.append(chips, h('datalist', { id: 'ab-groups' }, groups.map(g => h('option', { value: g }))), inp);
    return wrap;
  };

  // En-tête : qui est-ce, lien avec un MULTIPASS, visages.
  const pic = h('div', { class: 'ab-picker', hidden: true });
  const head = h('div', { class: 'ab-head' }, h('button', { type: 'button', class: 'ghost ab-back', onclick: onClose }, '← ' + t('Contacts')),
    h('div', { class: 'ab-pic' }, f.picture ? h('img', { src: f.picture, alt: '', width: 64, height: 64, referrerpolicy: 'no-referrer' }) : embedded(d.path, f.name), !isMe && d.path ? h('button', { type: 'button', class: 'ghost', onclick: () => photoEditor(K, pic, d.path, onSaved) }, t('Photo…')) : null),
    h('div', {}, h('h3', {}, f.name || t('Nouvelle fiche'), isMe ? h('span', { class: 'pill ok' }, t('Moi')) : null),
      isMe ? h('p', { class: 'muted' }, t('Votre profil Nostr et votre fiche sont réunis : ce que vous changez ici est publié (champs « public ») et enregistré dans votre fiche chiffrée (tout le reste, qui ne quitte jamais votre uCloud).'))
        : d.pubkey ? [h('p', {}, h('span', { class: 'pill ok' }, d.hosted_here ? t('MULTIPASS de la station') : t('Clé Nostr')), d.faces ? ` · ${d.faces} ${t('visage(s) reconnu(s)')}` : ''),
          // Visio : une personne reliée à un MULTIPASS s'appelle d'ici (la sonnerie ne passe que si elle vous suit aussi).
          h('p', {}, h('button', { type: 'button', onclick: () => K.mod('m-call.js').then(m => m.call(K, d.pubkey, f.name), e => K.toast(e.message)) }, '📹 ' + t('Appeler en visio')))] : h('p', { class: 'muted' }, t('Pas encore reliée à un MULTIPASS'), d.faces ? ` · ${d.faces} ${t('visage(s) reconnu(s)')}` : '')));

  // Comparaison avec le profil Nostr de la personne : on prend chaque valeur, une par une.
  const compare = () => {
    if (isMe || !d.diff) return null;
    const rows = d.diff.filter(r => r.theirs || r.mine);
    return h('section', { class: 'ab-diff' }, h('h4', {}, t('Son profil Nostr')), h('p', { class: 'muted' }, t('Ce que la personne publie, comparé à votre fiche : prenez les valeurs que vous voulez, une par une.')),
      h('table', {}, h('thead', {}, h('tr', {}, [t('Champ'), t('Votre fiche'), t('Son profil'), ''].map(x => h('th', { scope: 'col' }, x)))),
        h('tbody', {}, rows.map(r => h('tr', {}, h('td', {}, t(r.label)), h('td', {}, r.mine || '—'), h('td', {}, r.theirs || '—'),
          h('td', {}, r.same ? h('span', { class: 'pill ok' }, '✓') : r.can_take ? h('button', { type: 'button', class: 'ghost', onclick: async ev => { ev.target.disabled = true; try { await api('/api/v1/book/take', { method: 'POST', body: { path: d.path, keys: [r.key] } }); K.toast(t('Valeur prise'), 1); onSaved(d.path, true); } catch (e) { K.toast(e.message); ev.target.disabled = false; } } }, t('Prendre')) : null))))));
  };

  // Relier à un MULTIPASS (fiche d'un autre).
  const target = h('input', { placeholder: t('email du MULTIPASS ou npub…'), 'aria-label': t('email du MULTIPASS ou npub…'), autocomplete: 'off' });
  const link = isMe ? null : h('div', { class: 'ab-f' }, h('label', {}, t('Lien avec un MULTIPASS')), h('div', { class: 'ab-row' }, target,
    h('button', { type: 'button', class: 'ghost', onclick: async () => { try { await api('/api/v1/contacts/link', { method: 'POST', body: { path: d.path, target: target.value.trim() } }); K.toast(t('Fiche reliée'), 1); onSaved(d.path, true); } catch (e) { K.toast(e.message); } } }, t('Relier')),
    d.pubkey && d.via === 'vcard' ? h('button', { type: 'button', class: 'ghost', onclick: async () => { try { await api('/api/v1/contacts/link', { method: 'POST', body: { path: d.path, target: null } }); K.toast(t('Fiche déliée'), 1); onSaved(d.path, true); } catch (e) { K.toast(e.message); } } }, t('Délier')) : null));

  // Les clés de la personne, une par UPlanet (retrouvées par ses emails et par les liens signés des deux clés) : on écrit dans l'UPlanet commune.
  const keys = isMe || !(d.keys || []).length ? null : h('div', { class: 'ab-f' }, h('label', {}, t('Messagerie : clés de cette personne')),
    h('ul', { class: 'ab-keys' }, d.keys.map(k => {
      const here = !k.plane || k.plane === d.plane, away = !here && K.planeHref ? K.planeHref(k.plane, '#/chat?peer=' + k.hex) : null;
      const via = { station: t('compte hébergé sur la station'), lien: t('lien signé des deux clés'), fiche: t('clé de la fiche') }[k.via] || k.via;
      return h('li', {}, h('span', { class: 'pill' + (here ? ' ok' : '') }, k.plane || t('UPlanet inconnue')), ' ', h('small', { class: 'muted' }, via + ' · ' + k.hex.slice(0, 8) + '…'), ' ',
        here ? h('a', { class: 'btn', href: '#/chat?peer=' + k.hex }, t('Écrire')) : away ? h('a', { class: 'ghost', href: away }, t('Écrire depuis {0}', k.plane)) : h('small', { class: 'muted' }, t('depuis {0}', k.plane)));
    })));

  async function save() {
    if (!f.name?.trim()) { K.toast(t('Un nom est nécessaire')); return; }
    const body = copy(f);
    body.phones = body.phones.filter(x => x.value.trim()); body.emails = body.emails.filter(x => x.value.trim()); body.urls = body.urls.filter(x => x.trim());
    body.addresses = body.addresses.filter(a => ['street', 'code', 'city', 'country'].some(k => (a[k] || '').trim()));
    try {
      const r = isMe ? await api('/api/v1/book/me', { method: 'POST', body }) : await api('/api/v1/book/card', { method: 'POST', body: { path: d.path || null, fields: body } });
      K.toast(isMe ? t('Profil publié et fiche enregistrée') : t('Fiche enregistrée'), 1);
      onSaved(r.path || d.path, true);
    } catch (e) { K.toast(e.message); }
  }
  const remove = isMe || !d.path ? null : h('button', { type: 'button', class: 'ghost danger', onclick: async () => {
    if (!confirm(t('Supprimer la fiche de {0} ?', f.name))) return;
    try { await api('/api/v1/book/delete', { method: 'POST', body: { path: d.path } }); K.toast(t('Fiche supprimée'), 1); onSaved(null, true); } catch (e) { K.toast(e.message); }
  } }, t('Supprimer'));

  // Ses visages, reliés à la fiche par son UID : un clic montre la photo d'origine, le visage entouré.
  const seen = !blank && d.faces ? h('section', { class: 'ab-seen' }, h('h4', {}, t('Ses visages ({0})', d.faces)), h('div', { class: 'ab-faces' }, h('span', { class: 'muted' }, t('Chargement…')))) : null;
  if (seen) (async () => {
    const r = await api('/api/v1/book/photo-choices?path=' + encodeURIComponent(d.path)), th = (await api('/api/v1/faces/thumbs', { method: 'POST', body: { ids: r.photos.map(p => p.id), size: 96 } })).thumbs, fr = await K.mod('m-framed.js');
    seen.lastChild.replaceChildren(...r.photos.map(p => h('button', { type: 'button', class: 'ab-face', title: t('Voir la photo'), onclick: () => fr.showFace(K, p, f.name + ' · ' + p.path) }, h('img', { src: th[p.id] || '', alt: '', width: 56, height: 56 }))),
      r.total > r.photos.length ? h('a', { href: '#/people/tri' }, t('et {0} autre(s) sur la table de tri', r.total - r.photos.length)) : null);
  })().catch(e => seen.lastChild.replaceChildren(h('span', { class: 'muted' }, e.message)));
  box.append(head, pic, seen, compare(),
    h('fieldset', {}, h('legend', {}, t('Identité')), text('name', t('Nom affiché'), { public: true, max: 80 }), h('div', { class: 'ab-2' }, text('given', t('Prénom'), { max: 60 }), text('family', t('Nom de famille'), { max: 60 })), h('div', { class: 'ab-2' }, text('org', t('Société'), { max: 80 }), text('title', t('Fonction'), { max: 80 })), text('birthday', t('Anniversaire'), { type: 'date', max: 12 })),
    h('fieldset', {}, h('legend', {}, t('Joindre')), typedList('phones', t('Téléphones'), '+33 6 12 34 56 78', 'tel'), typedList('emails', t('Emails'), 'nom@exemple.org', 'email'), addresses(), urls(), link, keys),
    h('fieldset', {}, h('legend', {}, pub ? t('Profil public (Nostr)') : t('À propos')), text('about', t('Présentation'), { area: true, max: 500, public: true }), text('picture', t('Image (https)'), { type: 'url', max: 256, public: true }), text('nip05', t('Adresse vérifiée (nom@domaine)'), { ph: 'nom@domaine.org', max: 100, public: true }), text('lud16', t('Adresse Lightning'), { ph: 'nom@domaine.org', max: 100, public: true })),
    h('fieldset', {}, h('legend', {}, t('Classement et notes privées')), groupsEditor(), text('note', t('Notes (privées)'), { area: true, max: 2000 })),
    h('div', { class: 'ab-actions' }, h('button', {}, isMe ? t('Publier et enregistrer') : t('Enregistrer')), remove));
  return box;
}
