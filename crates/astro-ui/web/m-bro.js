'use strict';
// BRO LifeOS : fil d'activité en direct et dialogue avec l'agent. Chargé à la demande.

export default async function bro(root, K) {
  const { h, api, feed, listeners } = K;
const log = h('ol', { id: 'log', 'aria-live': 'polite', 'aria-label': 'Activité de la station', 'data-empty': t('Rien pour l\'instant : écrivez à BRO ci-dessous, ou attendez l\'activité de la station.') });
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
K.cleanup(() => listeners.delete(add));
}
