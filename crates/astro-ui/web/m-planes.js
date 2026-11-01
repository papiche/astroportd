'use strict';
// Plans de la station (ORIGIN et UPlanet privées) : voir ceux qui existent, fonder une UPlanet (la station crée sa clé de réseau privé) ou
// en rejoindre une avec la swarm.key reçue. Rien n'est jamais supprimé : retirer un plan ne touche ni à sa base, ni à ses comptes, ni à sa clé.
export default async function planes(root, K) {
  const { h, t } = K;
  const state = h('div', {}), result = h('div', {});
  const draw = async () => {
    const r = await K.api('/api/v1/station/planes');
    state.replaceChildren(
      h('p', { class: r.multi_process ? 'ok' : 'muted' }, r.multi_process
        ? t('Cette station sert plusieurs plans dans un seul processus.')
        : t('Cette station ne sert pour l\'instant que ORIGIN (astroportd serve). Pour servir aussi des UPlanet privées dans le même processus, lancez « astroportd planes » à la place.')),
      K.table(t('Plans de cette station'), ['Plan', 'Type', 'Adresse', 'Base', 'État', ''],
        r.planes.map(p => [h('strong', {}, p.name), p.origin ? 'ORIGIN' : t('UPlanet privée'), `http://${p.http}`, p.db_exists ? t('créée') : t('pas encore créée'),
          p.running ? K.pill('ok') : h('span', { class: 'pill disabled' }, t('au prochain démarrage')),
          p.origin ? '—' : h('button', { class: 'ghost', onclick: async () => {
            if (!confirm(t('Retirer le plan « {0} » de la configuration ? Sa base, ses comptes et sa clé restent sur le disque.', p.name))) return;
            try { await K.api('/api/v1/station/planes/remove', { method: 'POST', body: { name: p.name } }); K.toast(t('Plan retiré'), 1); draw(); } catch (e) { K.toast(e.message); }
          } }, t('Retirer'))])),
      r.restart_needed ? h('p', { class: 'degraded', role: 'status' }, t('Redémarrez « astroportd planes » pour servir : {0}.', r.pending.join(', '))) : null);
  };

  // ── Ajouter un plan ──
  const name = h('input', { placeholder: 'zen1', 'aria-label': t('Nom du plan'), required: true, maxlength: 32, pattern: '[a-z0-9]([a-z0-9\\-]*[a-z0-9])?', autocomplete: 'off' });
  const found = h('input', { type: 'radio', name: 'how', id: 'pl-found', checked: true }), join = h('input', { type: 'radio', name: 'how', id: 'pl-join' });
  const key = h('textarea', { rows: 3, placeholder: '/key/swarm/psk/1.0.0/\n/base16/\n…', 'aria-label': t('swarm.key reçue'), hidden: true, spellcheck: false });
  join.onchange = found.onchange = () => { key.hidden = !join.checked; };
  const form = h('form', { class: 'card', onsubmit: async ev => {
    ev.preventDefault();
    const body = { name: name.value.trim() };
    if (join.checked) body.swarm_key = key.value;
    try {
      const r = await K.api('/api/v1/station/planes', { method: 'POST', body });
      K.toast(t('Plan « {0} » ajouté', r.name), 1);
      const created = r.created_key ? h('div', {}, h('p', {}, t('Voici la swarm.key de cette UPlanet : donnez-la, par un canal sûr, à chaque station qui doit la rejoindre. Sans elle personne n\'y entre.')),
        h('textarea', { rows: 4, readOnly: true, 'aria-label': 'swarm.key', onfocus: ev2 => ev2.target.select() }, r.created_key)) : null;
      result.replaceChildren(h('p', {}, t('Écoute HTTP {0} · libp2p {1} · clé enregistrée dans {2}', r.http, r.p2p, r.swarm_key_path)), created);
      name.value = ''; key.value = ''; draw();
    } catch (e) { K.toast(e.message); }
  } },
  h('h3', {}, t('Ajouter un plan')),
  h('p', {}, h('label', {}, t('Nom'), ' ', name), h('small', { class: 'muted' }, ' ' + t('lettres minuscules, chiffres et tirets'))),
  h('p', {}, found, h('label', { for: 'pl-found' }, ' ' + t('Fonder une UPlanet neuve : la station crée sa clé de réseau privé')), h('br'), join, h('label', { for: 'pl-join' }, ' ' + t('Rejoindre une UPlanet existante : coller la swarm.key reçue d\'un Capitaine'))),
  key, h('p', {}, h('button', {}, t('Ajouter'))));

  root.replaceChildren(h('h2', {}, t('Plans (UPlanet)')),
    h('div', { class: 'grid' }, h('section', { class: 'card wide' }, state, form, result)));
  try { await draw(); } catch (e) { state.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
