'use strict';
// Personnes : le carnet d'adresses et les visages réunis. Une personne EST une fiche (vCard) : ses visages s'y relient par son UID.
// En tête, le sas « N visages à ranger » mène à Trier en grand (#/people/tri?grand) ou à la table de tri. Ensuite, un vrai carnet :
// recherche instantanée, index A–Z, groupes, fiche détaillée modifiable (avec ses visages), import vCard, doublons à fusionner.
// La première ligne est « Ma fiche » : votre profil Nostr et votre fiche réunis. Les fiches restent des fichiers chiffrés synchronisables avec un téléphone.

export default async function contacts(root, K) {
  const { h, t } = K, api = K.api;
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-book.css' });
  document.head.append(link); K.cleanup(() => link.remove());
  const s = { q: '', group: '', path: null, me: false, blank: false, data: null, open: false };
  const q = h('input', { type: 'search', placeholder: t('Rechercher un nom, un numéro, une société…'), 'aria-label': t('Rechercher dans les contacts'), autocomplete: 'off', maxlength: 100 });
  const list = h('ul', { class: 'ab-list', role: 'listbox', 'aria-label': t('Contacts') }), index = h('nav', { class: 'ab-index', 'aria-label': t('Index alphabétique') });
  const chips = h('div', { class: 'ab-chips' }), count = h('span', { class: 'muted' }), detail = h('section', { class: 'ab-detail' }), tools = h('div', { class: 'bar' });
  const main = h('div', { class: 'ab' }, h('div', { class: 'ab-side' }, h('div', { class: 'ab-search' }, q, count), chips, h('div', { class: 'ab-scroll' }, list, index)), detail);
  let timer = 0, groups = [];
  const sel = new Set(), selbar = h('div', { class: 'bar', hidden: true }), selbtn = h('button', { class: 'ghost' }, t('Sélectionner'));
  const seen = new Map(), shown = new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting) { shown.unobserve(e.target); photo(e.target); } }), { root: null, rootMargin: '200px' });
  K.cleanup(() => shown.disconnect());
  async function photo(av) {
    const path = av.dataset.path; if (!path) return;
    if (!seen.has(path)) seen.set(path, api('/api/v1/book/photo?path=' + encodeURIComponent(path)).then(r => r.uri, () => null));
    const uri = await seen.get(path);
    if (uri && av.isConnected) av.replaceChildren(h('img', { src: uri, alt: '', width: 36, height: 36 }));
  }

  async function load() {
    const p = new URLSearchParams({ q: s.q }); if (s.group) p.set('group', s.group);
    try { s.data = await api('/api/v1/book?' + p); } catch (e) { list.replaceChildren(h('li', { class: 'err', role: 'alert' }, e.message)); return; }
    groups = s.data.groups.map(g => g.name);
    count.textContent = t('{0} fiche(s)', s.data.total);
    chips.replaceChildren(h('button', { class: 'chip' + (s.group ? '' : ' on'), onclick: () => { s.group = ''; load(); } }, t('Tous')),
      ...s.data.groups.map(g => h('button', { class: 'chip' + (s.group === g.name ? ' on' : ''), onclick: () => { s.group = s.group === g.name ? '' : g.name; load(); }, ondblclick: () => rename(g.name) }, `${g.name} · ${g.count}`)));
    let last = '';
    list.replaceChildren(...s.data.contacts.map(c => {
      const first = c.letter !== last && !c.me; last = c.me ? last : c.letter;
      return h('li', { id: first ? 'ab-' + c.letter : null, role: 'option', 'aria-selected': String(c.path === s.path), class: c.path === s.path ? 'on' : '' },
        !c.me && s.select ? h('input', { type: 'checkbox', class: 'ab-chk', 'aria-label': c.name, checked: sel.has(c.path), onchange: ev => { if (ev.target.checked) sel.add(c.path); else sel.delete(c.path); bar(); } }) : null,
        h('button', { type: 'button', class: 'ab-item', onclick: () => (s.select && !c.me ? toggle(c) : select(c)) },
          c.picture ? h('img', { src: c.picture, alt: '', width: 36, height: 36, loading: 'lazy', referrerpolicy: 'no-referrer' }) : avatar(c),
          h('span', { class: 'ab-name' }, h('strong', {}, c.name), c.me ? h('span', { class: 'pill ok' }, t('Moi')) : null, h('small', { class: 'muted' }, [c.faces ? t('{0} visage(s)', c.faces) : '', c.org, c.emails[0], c.phones[0]?.value].filter(Boolean).join(' · ')))));
    }));
    index.replaceChildren(...s.data.letters.map(l => h('a', { href: '#/people', onclick: ev => { ev.preventDefault(); document.getElementById('ab-' + l)?.scrollIntoView({ block: 'start' }); } }, l)));
    if (!s.data.contacts.length) list.replaceChildren(h('li', { class: 'muted ab-empty' }, s.q || s.group ? t('Aucune fiche ne correspond.') : t('Aucune fiche. Créez-en une, importez un fichier .vcf ou synchronisez votre téléphone.')));
  }
  function toggle(c) { if (sel.has(c.path)) sel.delete(c.path); else sel.add(c.path); load(); bar(); }
  function bar() {
    const mine = (s.data?.contacts || []).filter(c => !c.me).map(c => c.path);
    selbar.hidden = !s.select;
    const rm = h('button', { class: 'ghost danger', disabled: !sel.size, onclick: async ev => {
      if (!confirm(t('Supprimer définitivement {0} fiche(s) ?', sel.size))) return;
      ev.target.disabled = true;
      try {
        const r = await api('/api/v1/book/delete-many', { method: 'POST', body: { paths: [...sel] } });
        K.toast(t('{0} fiche(s) supprimée(s)', r.deleted) + (r.failed.length ? ' · ' + t('{0} refusée(s)', r.failed.length) : ''), r.failed.length ? 0 : 1);
        sel.clear(); s.path = null; s.open = false; show0();
      } catch (e) { K.toast(e.message); ev.target.disabled = false; }
    } }, t('Supprimer la sélection ({0})', sel.size));
    selbar.replaceChildren(h('strong', {}, t('{0} sélectionnée(s)', sel.size)),
      h('button', { class: 'ghost', onclick: () => { mine.forEach(p => sel.add(p)); load(); bar(); } }, t('Tout sélectionner') + ` (${mine.length})`),
      h('button', { class: 'ghost', onclick: () => { sel.clear(); load(); bar(); } }, t('Tout désélectionner')), rm);
  }
  selbtn.onclick = () => { s.select = !s.select; sel.clear(); selbtn.textContent = s.select ? t('Terminer la sélection') : t('Sélectionner'); load().then(bar); };
  function avatar(c) {
    const av = h('span', { class: 'ab-av' }, (c.name || '?')[0].toUpperCase());
    if (!c.me && c.path) { av.dataset.path = c.path; if (seen.has(c.path)) photo(av); else shown.observe(av); }
    return av;
  }
  async function show() {
    detail.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
    main.classList.toggle('open', s.open);
    try {
      const card = (await K.mod('m-card.js')).default;
      const el = await card(K, { path: s.path, me: s.me, blank: s.blank, groups, onClose: () => { s.open = false; s.path = null; s.blank = false; show0(); }, onSaved: async path => { s.path = path; s.me = false; s.blank = false; await load(); if (path) show(); else { s.open = false; show0(); } } });
      detail.replaceChildren(el);
    } catch (e) { detail.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
  }
  const show0 = () => { main.classList.toggle('open', s.open); detail.replaceChildren(h('p', { class: 'muted ab-hint' }, t('Choisissez une fiche, ou créez-en une.'))); load(); };
  const select = c => { s.path = c.me ? null : c.path; s.me = !!c.me; s.blank = false; s.open = true; load(); show(); };
  const create = () => { s.path = null; s.me = false; s.blank = true; s.open = true; show(); };
  async function rename(name) {
    const to = prompt(t('Renommer le groupe « {0} » (vide : le retirer de toutes les fiches)', name), name);
    if (to === null) return;
    try { const r = await api('/api/v1/book/group', { method: 'POST', body: { from: name, to: to.trim() || null } }); K.toast(t('{0} fiche(s) modifiée(s)', r.changed), 1); if (s.group === name) s.group = to.trim(); load(); } catch (e) { K.toast(e.message); }
  }
  const file = h('input', { type: 'file', accept: '.vcf,text/vcard', hidden: true, onchange: async () => {
    const f = file.files[0]; if (!f) return;
    try { const r = await api('/api/v1/book/import', { method: 'POST', body: { vcf: await f.text() } }); K.toast(t('{0} fiche(s) importée(s), {1} doublon(s) ignoré(s)', r.imported, r.duplicates) + (r.rejected.length ? ' · ' + t('{0} refusée(s)', r.rejected.length) : ''), 1); load(); } catch (e) { K.toast(e.message); }
    file.value = '';
  } });
  // Doublons EXACTS (même nom, société, emails et téléphones) : fusionnés d'un geste, par lots de 50, avec progression et arrêt.
  // Rien n'est perdu : notes, groupes et adresses s'additionnent ; la fiche gardée est celle qui a le plus de visages.
  async function exact(box) {
    const p = await api('/api/v1/book/merge-exact', { method: 'POST', body: { apply: false } });
    if (!p.groups) return box.replaceChildren();
    const bar = h('progress', { max: p.groups, value: 0, hidden: true }), msg = h('p', { class: 'muted', role: 'status' });
    let stop = false;
    const halt = h('button', { class: 'ghost', hidden: true, onclick: () => { stop = true; halt.disabled = true; } }, t('Arrêter'));
    const go = h('button', { onclick: async () => {
      if (!confirm(t('Fusionner {0} groupe(s) de doublons exacts ? {1} fiche(s) en trop seront supprimées ; rien n\'est perdu (notes, groupes et adresses s\'additionnent).', p.groups, p.cards))) return;
      go.disabled = true; bar.hidden = halt.hidden = false;
      let done = 0, removed = 0, failed = [];
      try {
        while (!stop) {
          const r = await api('/api/v1/book/merge-exact', { method: 'POST', body: { apply: true, limit: 50 } });
          done += r.merged; removed += r.removed; failed = failed.concat(r.failed);
          bar.value = done; msg.textContent = t('{0} / {1} groupe(s) fusionné(s)', done, p.groups);
          if (!r.remaining || !r.merged) break; // fini, ou plus rien n'avance (échecs) : on s'arrête et on le dit
        }
        K.toast(t('{0} fiche(s) en trop supprimée(s)', removed), 1);
        msg.textContent = t('{0} groupe(s) fusionné(s), {1} fiche(s) supprimée(s)', done, removed) + (stop ? ' · ' + t('arrêté') : '') + (failed.length ? ' · ' + t('{0} échec(s) : {1}', failed.length, failed.slice(0, 3).map(f => f.name + ' (' + f.error + ')').join(', ')) : '');
      } catch (e) { msg.textContent = e.message; }
      halt.hidden = true; load(); dupCount();
    } }, t('Fusionner les {0} doublons exacts', p.groups));
    box.replaceChildren(h('h4', {}, t('Doublons exacts')),
      h('p', { class: 'muted' }, t('{0} groupe(s) de fiches identiques (même nom, société, emails et téléphones), soit {1} fiche(s) en trop. Par exemple :', p.groups, p.cards)),
      h('ul', {}, p.sample.map(x => h('li', {}, h('strong', {}, x.name), ' ', h('small', { class: 'muted' }, [x.org, ...x.emails, ...x.phones].filter(Boolean).join(' · ') + ' · ' + t('{0} copies', x.copies))))),
      h('p', { class: 'bar' }, go, halt), bar, msg);
  }
  const dupBtn = h('button', { class: 'ghost', onclick: () => duplicates() }, t('Doublons'));
  const dupCount = () => api('/api/v1/book/merge-exact', { method: 'POST', body: { apply: false } }).then(p => { dupBtn.textContent = p.groups ? t('Doublons ({0} exacts)', p.groups) : t('Doublons'); }, () => {});
  async function duplicates() {
    try {
      const top = h('div', { class: 'card' }), r = await api('/api/v1/book/duplicates');
      s.open = true; main.classList.add('open');
      if (!r.groups.length) { detail.replaceChildren(h('p', { class: 'muted' }, t('Aucun doublon probable'))); return; }
      exact(top).catch(e => top.replaceChildren(h('p', { class: 'err' }, e.message)));
      detail.replaceChildren(top, h('h3', {}, t('Doublons probables')), h('p', { class: 'muted' }, t('Choisissez la fiche à garder : les autres y sont fusionnées (téléphones, emails, adresses, groupes et notes sont additionnés) puis supprimées.')),
        ...r.groups.map(g => h('div', { class: 'card' }, h('p', { class: 'muted' }, g.reasons.join(', ')),
          h('ul', {}, g.cards.map(c => h('li', {}, h('strong', {}, c.name), ' ', h('small', { class: 'muted' }, [...c.emails, ...c.phones].join(' · ')), ' ',
            h('button', { class: 'ghost', onclick: async ev => { ev.target.disabled = true; try { await api('/api/v1/book/merge', { method: 'POST', body: { keep: c.path, drop: g.cards.filter(x => x.path !== c.path).map(x => x.path) } }); K.toast(t('Fiches fusionnées'), 1); duplicates(); load(); } catch (e) { K.toast(e.message); ev.target.disabled = false; } } }, t('Garder celle-ci')))))))); }
    catch (e) { K.toast(e.message); }
  }
  const tabs = h('div', { class: 'bar', role: 'tablist' }, h('button', { role: 'tab', 'aria-selected': 'true' }, t('Carnet')),
    h('button', { role: 'tab', class: 'ghost', 'aria-selected': 'false', onclick: async () => { const sync = (await K.mod('m-sync.js')).default; K.cleanup(() => {}); await sync(root, K); root.prepend(h('p', {}, h('button', { class: 'ghost', onclick: () => contacts(root, K) }, '← ' + t('Retour au carnet')))); } }, t('Synchroniser et inviter')));
  tools.append(h('button', { onclick: create }, '＋ ' + t('Nouvelle fiche')), h('button', { class: 'ghost', onclick: () => file.click() }, t('Importer un .vcf')), dupBtn, selbtn, file);
  dupCount();
  q.oninput = () => { clearTimeout(timer); timer = setTimeout(() => { s.q = q.value; load(); }, 200); };
  K.cleanup(() => clearTimeout(timer));
  // Le sas : les visages à ranger d'abord (une question à la fois), la table de tri pour les gestes en masse.
  const sas = h('section', { class: 'card ab-sas', hidden: true });
  api('/api/v1/faces').then(f => {
    if (!f.enabled) return;
    const n = f.unknown + f.suggestions.length, go = to => () => { location.hash = to; };
    sas.hidden = false;
    sas.replaceChildren(h('span', { class: 'ab-sas-n', 'aria-hidden': 'true' }, n > 999 ? '999+' : String(n)),
      h('div', {}, h('strong', {}, n ? t('{0} visage(s) à ranger', n) : t('Tous les visages sont rangés')),
        h('p', { class: 'muted' }, n ? t('Une question à la fois : « Est-ce Léa ? », « Qui est-ce ? ». Chaque réponse range les visages dans la fiche de la personne.') : t('La station analyse vos nouvelles photos : les visages à ranger apparaîtront ici.'))),
      h('span', { class: 'ab-sas-go' }, n ? h('button', { onclick: go('#/people/tri?grand') }, t('Trier en grand')) : null, h('button', { class: 'ghost', onclick: go('#/people/tri') }, t('Table de tri'))));
  }, () => {});
  root.replaceChildren(h('h2', {}, t('Personnes')), sas, tabs, tools, selbar, main);
  show0();
}
