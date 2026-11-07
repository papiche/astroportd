'use strict';
// Maillage et Capitaines amis (Capitaine) : sa station dans le maillage, les stations jointes (ajout à chaud, gardées comme
// bootstrap), les Capitaines connus (suivre / ne plus suivre) et, pour chaque ami réciproque, les services que SA station nous
// ouvre — demandés à elle, jamais publiés. « Se connecter » ouvre un accès local à chaud vers le service.
export default async function mesh(root, K) {
  const { h, t } = K, api = p => K.api('/api/v1/station/admin/' + p), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const [m, c, cs] = await Promise.all([api('mesh').catch(e => ({ error: e.message })), api('captains').catch(e => ({ error: e.message })), K.api('/api/v1/station/constellation').catch(() => null)]);
  const act = (p, body, msg) => async ev => { const b = ev.target; b.disabled = true; try { await post(p, body); if (msg) K.toast(msg, 1); mesh(root, K); } catch (e) { K.toast(e.message); b.disabled = false; } };
  // Action à confirmer d'un second appui (pas de boîte de dialogue).
  const sure = (label, run) => { const b = h('button', { class: 'ghost', onclick: ev => { if (b.dataset.armed) return run(ev); b.dataset.armed = 1; b.textContent = t('Confirmer ?'); } }, label); return b; };

  // ── Ma station ──
  const me = m.error ? h('p', { class: 'degraded' }, m.error) : !m.active
    ? h('p', { class: 'degraded', role: 'status' }, t('Maillage coupé : relancez la station sans --no-p2p pour rejoindre les autres stations.'))
    : [h('p', {}, t('UPlanet : {0} · {1} station(s) connectée(s).', m.plane, m.connected.length)),
      h('p', { class: 'muted' }, t('Adresse à donner aux autres Capitaines :')), m.addrs.length ? h('ul', {}, m.addrs.map(a => h('li', {}, h('code', {}, a)))) : h('p', { class: 'muted' }, m.peer_id)];

  // ── Stations ──
  let stations = null;
  if (m.active) {
    const addr = h('input', { placeholder: '/ip4/…/tcp/4101/p2p/12D3Koo…', autocomplete: 'off', 'aria-label': t('Adresse de la station') });
    const keep = h('input', { type: 'checkbox', checked: true, id: 'ms-keep' });
    stations = h('section', { class: 'card wide' }, h('h2', {}, t('Stations')),
      m.bootstrap.length ? h('ul', {}, m.bootstrap.map(b => h('li', {}, b.connected ? K.pill('ok') : h('span', { class: 'pill disabled' }, t('absente')), ' ', h('code', {}, b.addr), ` (${b.source === 'gardée' ? t('gardée') : t('démarrage')}) `,
        b.source === 'gardée' ? sure(t('Retirer'), act('mesh', { forget: b.addr }, t('Station retirée'))) : null))) : h('p', { class: 'muted' }, t('Aucune station gardée.')),
      h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Joindre une station')), addr, ' ', h('label', { for: 'ms-keep' }, keep, ' ', t('Garder (rejointe à chaque démarrage)')), ' ',
        h('button', { onclick: ev => addr.value.trim() ? act('mesh', { dial: addr.value.trim(), keep: keep.checked }, t('Station jointe'))(ev) : K.toast(t('Adresse de la station') + ' ?') }, t('Joindre'))),
      h('p', { class: 'muted' }, t('Seule une station de la même UPlanet (même swarm.key) peut être jointe.')));
  }

  // ── Capitaines ──
  const how = (s, a) => s === 'ssh' ? `ssh -p ${a.split(':').pop()} <utilisateur>@127.0.0.1` : s.startsWith('bureau') ? t('client de bureau à distance vers {0}', a) : `http://${a}`;
  const svc = (cap, s) => h('li', {}, h('strong', {}, s.name), ' ', s.local
    ? [h('code', {}, how(s.name, s.local.addr)), ' ', h('button', { class: 'ghost', onclick: act('captains', { disconnect: { peer: cap.peer, service: s.name } }) }, t('Fermer'))]
    : h('button', { onclick: act('captains', { connect: { peer: cap.peer, service: s.name } }, t('Accès ouvert')) }, t('Se connecter')));
  const card = cap => h('div', { class: 'card' }, h('h3', {}, cap.label, ' ', h('span', { class: cap.friend ? 'pill ok' : 'pill disabled' }, cap.friend ? t('ami') : cap.follows_me ? t('vous suit') : cap.i_follow ? t('suivi') : t('inconnu'))),
    cap.i_follow ? sure(t('Ne plus suivre'), act('captains', { unfollow: cap.hex }, t('Vous ne suivez plus {0}', cap.label))) : h('button', { onclick: act('captains', { follow: cap.hex }, t('Vous suivez {0}', cap.label)) }, t('Suivre')),
    !cap.friend ? (cap.i_follow ? h('p', { class: 'muted' }, t('En attente : il doit vous suivre aussi pour partager.')) : null)
      : !cap.peer ? h('p', { class: 'muted' }, t('Station inconnue.'))
      : cap.services?.error ? h('p', { class: 'muted' }, t('Station injoignable : {0}', cap.services.error))
      : cap.services?.length ? h('ul', {}, cap.services.map(s => svc(cap, s))) : h('p', { class: 'muted' }, t('Rien de partagé avec vous pour l\'instant.')));
  const caps = c.captains || [], list = h('div', { class: 'grid' }), find = h('input', { type: 'search', placeholder: t('Chercher un Capitaine'), 'aria-label': t('Chercher un Capitaine') });
  const show = () => list.replaceChildren(...caps.filter(x => !find.value || x.label.toLowerCase().includes(find.value.toLowerCase())).map(card));
  find.oninput = show; show();
  const captains = h('section', { class: 'card wide' }, h('h2', {}, t('Capitaines')),
    c.error ? h('p', { class: 'degraded' }, c.error) : !caps.length ? h('p', { class: 'muted' }, t('Aucun autre Capitaine connu pour l\'instant.')) : [caps.length > 8 ? find : null, list],
    h('p', { class: 'muted' }, t('Un ami réciproque vous montre ce qu\'il vous partage ; c\'est sa station qui décide. Vos propres partages se règlent dans Options › Tunnels.')));

  // ── Essaim : toutes les stations vues, de la plus puissante à la moins puissante ──
  const TIER = { Light: '🌿', Standard: '⚡', Brain: '🔥' }, nodes = cs?.nodes || [];
  const node = n => { const net = n.services?.filter(x => x !== 'ssh') || [];
    return h('div', { class: 'card ck-node' + (n.self ? ' self' : '') }, h('div', {}, h('strong', {}, n.node_id ? '…' + n.node_id.slice(-8) : '—'), n.self ? h('small', { class: 'muted' }, ' ' + t('(vous)')) : null, ' ', h('span', { class: 'ck-st ' + (n.status === 'known' ? 'off' : 'on') }, n.status === 'known' ? t('vue') : t('en ligne'))),
      h('p', {}, `${TIER[n.tier] || ''} ${n.tier} · ${n.power_score}`, n.distance_km == null ? '' : ` · ${n.distance_km} km`), net.length ? h('p', {}, net.map(x => h('span', { class: 'ck-chip' }, x))) : h('p', { class: 'muted' }, t('aucun service partagé'))); };
  const grid = h('div', { class: 'grid' }), only = h('input', { type: 'checkbox', id: 'ms-ia' });
  const paint = () => grid.replaceChildren(...nodes.filter(n => !only.checked || n.services?.includes('ollama')).map(node));
  only.onchange = paint; paint();
  const swarm = h('section', { class: 'card wide' }, h('h2', {}, t('L\'essaim'), h('small', { class: 'muted' }, ` · ${nodes.length}`)),
    h('p', {}, h('label', { for: 'ms-ia' }, only, ' ', t('Seulement celles qui offrent l\'IA (Ollama)'))), grid,
    cs && !cs.swarm_dir_configured ? h('p', { class: 'muted' }, t('Aucun dossier de pairs configuré (--swarm-dir) : seules les stations jointes apparaissent.')) : null,
    h('p', {}, h('a', { class: 'ck-btn', href: '#/planes' }, t('Plans (UPlanet)'))));

  root.replaceChildren(...[captains, swarm, stations, h('section', { class: 'card wide' }, h('h2', {}, t('Ma station')), me)].filter(Boolean));
}
