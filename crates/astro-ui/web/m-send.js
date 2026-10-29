'use strict';
// Envoyer des fichiers du uCloud à des proches (module chargé à la demande) : seuls les proches RÉCIPROQUES (vous vous
// suivez mutuellement) sont proposés. Un proche hébergé ici reçoit le fichier tout de suite ; un proche d'une autre
// station le reçoit par un message chiffré, et sa station rapatrie le fichier par le maillage.
export default async function send(paths, K) {
  const { h } = K;
  let dlg = document.getElementById('send');
  if (!dlg) { dlg = h('dialog', { id: 'send', 'aria-labelledby': 'send-t' }); document.body.append(dlg); }
  const close = h('button', { type: 'button', class: 'x', 'aria-label': 'Fermer', onclick: () => dlg.close() }, '×');
  const body = h('div', {}, h('p', { class: 'muted' }, 'Chargement des proches…'));
  dlg.replaceChildren(close, h('h2', { id: 'send-t' }, `Envoyer ${paths.length} élément(s)`), body);
  if (!dlg.open) dlg.showModal();
  try {
    const { friends } = await K.api('/api/v1/friends');
    if (!friends.length) return body.replaceChildren(h('p', { class: 'muted' }, 'Aucun proche réciproque pour le moment : suivez quelqu\'un qui vous suit aussi (double suivi) pour lui envoyer des fichiers.'));
    const boxes = friends.map(f => h('input', { type: 'checkbox', id: 'sf-' + f.hex, value: f.hex }));
    const go = h('button', { disabled: true }, 'Envoyer');
    boxes.forEach(b => { b.onchange = () => { go.disabled = !boxes.some(x => x.checked); }; });
    const form = h('form', { onsubmit: async ev => {
      ev.preventDefault(); go.disabled = true; go.textContent = 'Envoi…';
      try {
        const r = await K.api('/api/v1/ucloud/send', { method: 'POST', body: { paths, friends: boxes.filter(b => b.checked).map(b => b.value) } });
        body.replaceChildren(h('p', {}, `${r.files} fichier(s) :`), h('ul', {}, r.results.map(x => h('li', { class: x.error ? 'degraded' : 'ok' }, `${x.label || x.friend.slice(0, 8)} : ${x.sent} envoyé(s)${x.via === 'maillage' ? ' (sa station les récupère par le maillage)' : ''}${x.error ? ' — ' + x.error : ''}`))));
        K.toast(`${r.results.reduce((n, x) => n + x.sent, 0)} envoi(s) effectué(s)`, 1);
      } catch (e) { body.append(h('p', { class: 'err', role: 'alert' }, e.message)); go.disabled = false; go.textContent = 'Envoyer'; }
    } }, h('ul', { class: 'files' }, friends.map((f, i) => h('li', {}, boxes[i], h('label', { for: 'sf-' + f.hex }, f.label), h('span', { class: 'muted' }, f.hosted ? 'sur cette station' : 'autre station')))), h('p', {}, go));
    body.replaceChildren(form);
  } catch (e) { body.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
