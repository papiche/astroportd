'use strict';
// Plans de la station (ORIGIN et UPlanet privées) : voir ceux qui existent, fonder une UPlanet (la station crée sa clé de réseau privé) ou
// en rejoindre une avec la swarm.key reçue. Rien n'est jamais supprimé : retirer un plan ne touche ni à sa base, ni à ses comptes, ni à sa clé.
export default async function planes(root, K) {
  const { h, t } = K;
  const state = h('div', {}), result = h('div', {});
  // Après un redémarrage demandé, on attend le retour de la station puis on recharge la page.
  const back = async () => {
    await new Promise(ok => setTimeout(ok, 3000));
    for (let i = 0; i < 60; i++) {
      try { if ((await fetch('/health', { cache: 'no-store' })).ok) return location.reload(); } catch { /* la station redémarre */ }
      await new Promise(ok => setTimeout(ok, 2000));
    }
  };
  // Démarrage automatique : une unité systemd qui lance encore « serve » ne servirait que ORIGIN au prochain démarrage.
  const service = sv => !sv ? null
    : sv.mismatch ? h('div', { class: 'degraded', role: 'alert' }, h('p', {}, t('Le démarrage automatique lance encore « serve » : au prochain démarrage, seule la constellation ORIGIN sera servie.')),
        h('p', {}, t('Faites-lui lancer :'), ' ', h('code', {}, sv.command), ' ', t('(« astroportd init --systemd » le fait pour vous).')),
        h('p', {}, K.sure(t('Basculer le service en multi-constellations'), async () => {
          try { const x = await K.api('/api/v1/station/planes/service', { method: 'POST', body: {} }); K.toast(x.state === 'switched' ? t('Démarrage automatique basculé sur « planes »') : t('Rien à basculer'), 1); draw(); } catch (e) { K.toast(e.message); }
        }, t('L\'unité systemd sera réécrite (l\'ancienne est gardée à côté).'))))
    : sv.unit === 'planes' ? h('p', { class: 'ok' }, t('Démarrage automatique : toutes les constellations sont lancées (« astroportd planes »).'))
    : sv.unit === 'serve' ? h('p', { class: 'muted' }, t('Démarrage automatique : « astroportd serve » (la constellation ORIGIN seule).'))
    : sv.unit === 'foreign' ? h('p', { class: 'muted' }, t('Une unité systemd personnalisée est installée ({0}) : faites-lui lancer « astroportd planes ».', sv.path))
    : h('p', { class: 'muted' }, t('Pas de démarrage automatique (systemd) : « astroportd init --systemd » l\'installe.'));
  const restart = r => r.restart_needed && r.service?.restart && !r.service.mismatch
    ? h('p', {}, K.sure(t('Redémarrer la station'), async () => {
      try { await K.api('/api/v1/station/restart', { method: 'POST' }); K.toast(t('Redémarrage demandé : la page se rechargera au retour de la station.'), 1); back(); } catch (e) { K.toast(e.message); }
    }, t('La station sera indisponible quelques secondes.'))) : null;
  const draw = async () => {
    const r = await K.api('/api/v1/station/planes');
    state.replaceChildren(
      ...[service(r.service)].filter(Boolean),
      h('p', { class: r.multi_process ? 'ok' : 'muted' }, r.multi_process
        ? t('Cette station sert plusieurs constellations dans un seul processus.')
        : t('Cette station ne sert pour l\'instant que la constellation ORIGIN (astroportd serve). Pour en servir aussi d\'autres, privées, dans le même processus, lancez « astroportd planes » à la place.')),
      K.table(t('Constellations de cette station'), ['Constellation', 'Type', 'Adresse', 'Base', 'État', ''],
        r.planes.map(p => [h('strong', {}, p.name), p.origin ? 'ORIGIN' : t('Constellation privée'), `http://${p.http}`, p.db_exists ? t('créée') : t('pas encore créée'),
          p.running ? K.pill('ok') : h('span', { class: 'pill disabled' }, t('au prochain démarrage')),
          p.origin ? h('small', { class: 'muted' }, t('ORIGIN reste toujours en service')) : K.sure(t('Retirer'), async () => {
            try { await K.api('/api/v1/station/planes/remove', { method: 'POST', body: { name: p.name } }); K.toast(t('Constellation retirée'), 1); draw(); } catch (e) { K.toast(e.message); }
          }, t('Retirer la constellation « {0} » de la configuration ? Sa base, ses comptes et sa clé restent sur le disque.', p.name))])),
      ...(r.restart_needed ? [h('p', { class: 'degraded', role: 'status' }, t('Redémarrez « astroportd planes » pour servir : {0}.', r.pending.join(', ')))] : []), ...[restart(r)].filter(Boolean));
  };

  // ── Ajouter une constellation ──
  const name = h('input', { placeholder: 'zen1', 'aria-label': t('Nom de la constellation'), required: true, maxlength: 32, pattern: '[a-z0-9]([a-z0-9\\-]*[a-z0-9])?', autocomplete: 'off' });
  const found = h('input', { type: 'radio', name: 'how', id: 'pl-found', checked: true }), join = h('input', { type: 'radio', name: 'how', id: 'pl-join' });
  const key = h('textarea', { rows: 3, placeholder: '/key/swarm/psk/1.0.0/\n/base16/\n…', 'aria-label': t('swarm.key reçue'), hidden: true, spellcheck: false });
  join.onchange = found.onchange = () => { key.hidden = !join.checked; };
  const form = h('form', { class: 'card', onsubmit: async ev => {
    ev.preventDefault();
    const body = { name: name.value.trim() };
    if (join.checked) body.swarm_key = key.value;
    try {
      const r = await K.api('/api/v1/station/planes', { method: 'POST', body });
      K.toast(t('Constellation « {0} » ajoutée', r.name), 1);
      const created = r.created_key ? h('div', {}, h('p', {}, t('Voici la swarm.key de cette constellation : donnez-la, par un canal sûr, à chaque station qui doit la rejoindre. Sans elle personne n\'y entre.')),
        h('textarea', { rows: 4, readOnly: true, 'aria-label': 'swarm.key', onfocus: ev2 => ev2.target.select() }, r.created_key)) : null;
      const sv = r.service?.state, note = sv === 'switched' ? [h('p', { class: 'ok' }, t('Le démarrage automatique lance maintenant toutes les constellations : redémarrez la station pour servir la nouvelle.'))]
        : sv === 'error' ? [h('p', { class: 'err', role: 'alert' }, t('Démarrage automatique non mis à jour : {0}', r.service.error))] : [];
      result.replaceChildren(h('p', {}, t('Écoute HTTP {0} · libp2p {1} · clé enregistrée dans {2}', r.http, r.p2p, r.swarm_key_path)), ...note, ...(created ? [created] : []));
      name.value = ''; key.value = ''; draw();
    } catch (e) { K.toast(e.message); }
  } },
  h('h3', {}, t('Ajouter une constellation')),
  h('p', {}, h('label', {}, t('Nom'), ' ', name), h('small', { class: 'muted' }, ' ' + t('lettres minuscules, chiffres et tirets'))),
  h('p', {}, h('label', { class: 'pl-opt' }, found, ' ' + t('Fonder une constellation neuve : la station crée sa clé de réseau privé')), h('br'), h('label', { class: 'pl-opt' }, join, ' ' + t('Rejoindre une constellation existante : coller la swarm.key reçue d\'un Capitaine'))),
  key, h('p', {}, h('button', {}, t('Ajouter'))));

  root.replaceChildren(h('h2', {}, t('Constellations')),
    h('div', { class: 'grid' }, h('section', { class: 'card wide' }, state, form, result)));
  try { await draw(); } catch (e) { state.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
