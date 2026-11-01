'use strict';
// Chat privé : conversations 1 à 1, chiffrées de bout en bout (NIP-17). Liste à gauche (non lus, dernier message), fil à droite.
// Les messages arrivent en direct (flux d'activité de la station) ; un message non remis reste dans le fil, marqué « non remis ».

export default async function chat(root, K, query) {
  const { h, t, api } = K;
  for (const f of ['m-book.css', 'm-chat.css']) { const l = h('link', { rel: 'stylesheet', href: '/ui/' + f }); document.head.append(l); K.cleanup(() => l.remove()); }
  let here = ''; // UPlanet de cette interface, connue par l'annuaire
  const s = { peer: null, label: '', convs: [], msgs: [], more: false, open: false };
  const list = h('ul', { class: 'ab-list', role: 'listbox', 'aria-label': t('Conversations') }), thread = h('div', { class: 'ch-thread', role: 'log', 'aria-live': 'polite' });
  const detail = h('section', { class: 'ab-detail ch-detail' }), main = h('div', { class: 'ab' });
  const picker = h('select', { 'aria-label': t('Nouvelle conversation') }, h('option', { value: '' }, t('Nouvelle conversation…')));
  main.append(h('div', { class: 'ab-side' }, h('div', { class: 'ab-search' }, picker), h('div', { class: 'ab-scroll' }, list)), detail);
  root.replaceChildren(main);

  const initial = n => (n || '?')[0].toUpperCase();
  const day = at => new Date(at * 1000).toLocaleDateString('fr', { weekday: 'long', day: 'numeric', month: 'long' });
  const hour = at => new Date(at * 1000).toLocaleTimeString('fr', { hour: '2-digit', minute: '2-digit' });

  async function loadList() {
    try { const r = await api('/api/v1/chat'); s.convs = r.conversations; here = r.plane || ''; } catch (e) { list.replaceChildren(h('li', { class: 'err', role: 'alert' }, e.message)); return; }
    list.replaceChildren(...s.convs.map(c => h('li', { role: 'option', 'aria-selected': String(c.peer === s.peer), class: c.peer === s.peer ? 'on' : '' },
      h('button', { type: 'button', class: 'ab-item', onclick: () => openConv(c.peer, c.label) },
        h('span', { class: 'ab-av' }, initial(c.label)),
        h('span', { class: 'ab-name' }, h('strong', {}, c.label), h('small', { class: 'muted' }, (c.mine ? t('Vous : ') : '') + c.last),
          here && (c.channels.length > 1 || c.plane !== here) ? h('small', { class: 'ch-planes' }, [...new Set(c.channels.map(x => x.plane))].map(p => h('span', { class: 'pill' + (p === here ? ' ok' : '') }, p + (c.channels.some(x => x.plane === p && x.unread) ? ' ●' : '')))) : null),
        h('span', { class: 'ch-meta' }, h('small', { class: 'muted' }, hour(c.at)), c.unread ? h('span', { class: 'ch-badge', 'aria-label': t('{0} non lu(s)', c.unread) }, String(c.unread)) : null)))));
    if (!s.convs.length) list.replaceChildren(h('li', { class: 'muted ab-empty' }, t('Aucune conversation. Choisissez un proche pour écrire le premier message.')));
    K.chatUnread?.(s.convs.reduce((n, c) => n + c.unread, 0));
  }
  async function loadFriends() {
    try { for (const f of (await api('/api/v1/friends')).friends) picker.append(h('option', { value: f.hex, 'data-label': f.label }, f.label)); } catch { /* sans proche, la liste reste vide */ }
    picker.onchange = () => { const o = picker.selectedOptions[0]; if (o.value) openConv(o.value, o.dataset.label); picker.value = ''; };
  }
  function bubble(m, prev) {
    const out = [];
    if (!prev || day(prev.at) !== day(m.at)) out.push(h('div', { class: 'ch-day' }, day(m.at)));
    out.push(h('div', { class: 'ch-msg ' + (m.mine ? 'mine' : 'theirs') + (m.state === 'failed' ? ' failed' : '') },
      h('p', {}, m.text), h('small', { class: 'muted' }, hour(m.at) + (m.state === 'failed' ? ' · ' + t('non remis') : ''))));
    return out;
  }
  function paint(stick) {
    const old = thread.scrollHeight - thread.scrollTop;
    const more = s.more ? [h('button', { type: 'button', class: 'chip', onclick: older }, t('Messages plus anciens'))] : [];
    thread.replaceChildren(...more, ...s.msgs.flatMap((m, i) => bubble(m, s.msgs[i - 1])));
    thread.scrollTop = stick ? thread.scrollHeight : thread.scrollHeight - old;
  }
  async function older() {
    const r = await api('/api/v1/chat/messages?' + new URLSearchParams({ peer: s.peer, before: s.msgs[0].at }));
    s.msgs = [...r.messages, ...s.msgs]; s.more = r.more; paint(false);
  }
  async function refresh(stick) {
    if (!s.peer) return;
    const peer = s.peer, r = await api('/api/v1/chat/messages?' + new URLSearchParams({ peer }));
    if (peer !== s.peer) return;
    s.msgs = r.messages; s.more = r.more; paint(stick);
    if (document.visibilityState === 'visible') api('/api/v1/chat/read', { method: 'POST', body: { peer } }).then(loadList).catch(() => {});
  }
  const box = h('textarea', { rows: 1, maxlength: 4000, placeholder: t('Écrire un message…'), 'aria-label': t('Message') });
  async function send() {
    const text = box.value.trim(); if (!text || !s.peer) return;
    box.disabled = true;
    try { await api('/api/v1/chat/send', { method: 'POST', body: { peer: s.peer, text } }); box.value = ''; await refresh(true); await loadList(); }
    catch (e) { K.toast(e.message); } finally { box.disabled = false; box.focus(); }
  }
  box.onkeydown = ev => { if (ev.key === 'Enter' && !ev.shiftKey) { ev.preventDefault(); send(); } };
  const form = h('form', { class: 'ch-form', onsubmit: ev => { ev.preventDefault(); send(); } }, box, h('button', { type: 'submit' }, t('Envoyer')));

  async function openConv(peer, label) {
    s.peer = peer; s.label = label || peer.slice(0, 8); s.open = true; s.msgs = []; s.more = false;
    main.classList.add('open');
    detail.replaceChildren(h('div', { class: 'ch-head' }, h('button', { type: 'button', class: 'ab-back', onclick: () => { s.open = false; s.peer = null; main.classList.remove('open'); loadList(); } }, '← ' + t('Retour')),
      h('span', { class: 'ab-av' }, initial(s.label)), h('strong', {}, s.label), h('small', { class: 'muted' }, t('Chiffré de bout en bout'))), thread, form);
    await refresh(true); await loadList(); box.focus();
  }
  K.listen(ev => { if (ev.source === 'chat:msg') { loadList(); if (ev.message === s.peer) refresh(true); } });
  await Promise.all([loadList(), loadFriends()]);
  const want = new URLSearchParams(query || '').get('peer');
  if (want) { const c = s.convs.find(x => x.peer === want); openConv(want, c ? c.label : [...picker.options].find(o => o.value === want)?.dataset.label); }
  else detail.replaceChildren(h('p', { class: 'muted ab-hint' }, t('Choisissez une conversation, ou commencez-en une avec un proche.')));
}
