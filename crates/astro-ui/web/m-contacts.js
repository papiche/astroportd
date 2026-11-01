'use strict';
// Contacts : un vrai carnet d'adresses. Recherche instantanée, index A–Z, groupes, fiche détaillée modifiable, import vCard, doublons à fusionner.
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
          h('span', { class: 'ab-name' }, h('strong', {}, c.name), c.me ? h('span', { class: 'pill ok' }, t('Moi')) : null, h('small', { class: 'muted' }, [c.org, c.emails[0], c.phones[0]?.value].filter(Boolean).join(' · ')))));
    }));
    index.replaceChildren(...s.data.letters.map(l => h('a', { href: '#/contacts', onclick: ev => { ev.preventDefault(); document.getElementById('ab-' + l)?.scrollIntoView({ block: 'start' }); } }, l)));
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
      const card = (await import('/ui/m-card.js')).default;
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
  async function duplicates() {
    try {
      const r = await api('/api/v1/book/duplicates');
      if (!r.groups.length) { K.toast(t('Aucun doublon probable'), 1); return; }
      s.open = true; main.classList.add('open');
      detail.replaceChildren(h('h3', {}, t('Doublons probables')), h('p', { class: 'muted' }, t('Choisissez la fiche à garder : les autres y sont fusionnées (téléphones, emails, adresses, groupes et notes sont additionnés) puis supprimées.')),
        ...r.groups.map(g => h('div', { class: 'card' }, h('p', { class: 'muted' }, g.reasons.join(', ')),
          h('ul', {}, g.cards.map(c => h('li', {}, h('strong', {}, c.name), ' ', h('small', { class: 'muted' }, [...c.emails, ...c.phones].join(' · ')), ' ',
            h('button', { class: 'ghost', onclick: async ev => { ev.target.disabled = true; try { await api('/api/v1/book/merge', { method: 'POST', body: { keep: c.path, drop: g.cards.filter(x => x.path !== c.path).map(x => x.path) } }); K.toast(t('Fiches fusionnées'), 1); duplicates(); load(); } catch (e) { K.toast(e.message); ev.target.disabled = false; } } }, t('Garder celle-ci')))))))); }
    catch (e) { K.toast(e.message); }
  }
  const tabs = h('div', { class: 'bar', role: 'tablist' }, h('button', { role: 'tab', 'aria-selected': 'true' }, t('Carnet')),
    h('button', { role: 'tab', class: 'ghost', 'aria-selected': 'false', onclick: async () => { const sync = (await import('/ui/m-sync.js')).default; K.cleanup(() => {}); await sync(root, K); root.prepend(h('p', {}, h('button', { class: 'ghost', onclick: () => contacts(root, K) }, '← ' + t('Retour au carnet')))); } }, t('Synchroniser et inviter')));
  tools.append(h('button', { onclick: create }, '＋ ' + t('Nouvelle fiche')), h('button', { class: 'ghost', onclick: () => file.click() }, t('Importer un .vcf')), h('button', { class: 'ghost', onclick: duplicates }, t('Doublons')), selbtn, file);
  q.oninput = () => { clearTimeout(timer); timer = setTimeout(() => { s.q = q.value; load(); }, 200); };
  K.cleanup(() => clearTimeout(timer));
  root.replaceChildren(h('h2', {}, t('Contacts')), tabs, tools, selbar, main);
  show0();
}
