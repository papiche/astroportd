'use strict';
// Cockpit du Capitaine : un verdict unique, ce qui demande une action (3 au plus), les six étages de la station et l'essaim.
// Les règles (`analyze`) sont une fonction pure : tests/cockpit_rules.mjs les rejoue sans navigateur.
const RANK = { down: 0, degraded: 1, info: 2, ok: 3 };
const FLOORS = [['machine', '🖥️', 'Machine', '#/station'], ['station', '📡', 'Station', '#/station'], ['mesh', '🌐', 'Constellation', '#/planes'], ['services', '🔌', 'Services et accès', '#/options'], ['members', '👥', 'Membres', '#/station'], ['economy', '🪙', 'Économie', '#/coop']];
const worst = (a, b) => (RANK[a] <= RANK[b] ? a : b);

export function analyze({ health, cs, ec, ac, tun, tk, dash, dns }) {
  const todos = [], add = (level, floor, title, why, label, href) => todos.push({ level, floor, title, why, label, href });
  const nodes = cs?.nodes || [], me = nodes.find(n => n.self), others = nodes.filter(n => !n.self);
  // Station
  for (const [name, v] of Object.entries(health?.subsystems || {})) {
    if (v.status === 'down' || v.status === 'degraded') add(v.status, 'station', `${name} : ${v.status === 'down' ? 'en panne' : 'dégradé'}`, v.detail || '', 'Voir le détail', '#/station');
  }
  // Constellation
  if (dash?.p2p && dash.p2p.enabled === false) add('degraded', 'mesh', 'Le maillage P2P est arrêté', 'Sans lui, aucune station ne peut vous joindre ni partager Ollama ou ComfyUI. Démarrez avec --p2p-listen.', 'Voir les plans', '#/planes');
  else if (cs && !others.length) add('info', 'mesh', 'Vous êtes seul dans l\'essaim', 'Aucune autre station n\'est vue : rejoignez une constellation ou indiquez un dossier de pairs (--swarm-dir).', 'Rejoindre', '#/planes');
  // Adresses de la coopérative (DNSLink) : station et MULTIPASS
  if (dns?.configured === false) add('info', 'mesh', 'Les adresses de la coopérative ne sont pas activées', 'Renseignez les clés OVH dans la configuration coopérative : la station et chaque MULTIPASS auront une adresse.', 'Configurer', '#/coop');
  if (dns?.failed) add('degraded', 'mesh', `${dns.failed} adresse(s) de la coopérative non enregistrée(s)`, dns.errors?.[0] || '', 'Configurer', '#/coop');
  // Services et accès
  for (const sv of tun?.services || []) {
    if (sv.up === false && (sv.customized || sv.grants?.length)) add('degraded', 'services', `${sv.name} est réglé mais ne répond pas`, `Rien n'écoute sur ${sv.target || 'cette machine'} : les amis qui l'utilisent n'y arrivent pas.`, 'Régler', '#/options');
    if (sv.name === 'ssh' && sv.mode === 'swarm') add('degraded', 'services', 'SSH est ouvert à tout le réseau', 'N\'importe quel membre peut tenter d\'entrer sur cette machine. Préférez « Capitaines amis ».', 'Restreindre', '#/options');
  }
  const failed = (tk?.kinds || []).reduce((n, k) => n + (k.failed || 0), 0);
  if (failed) add('degraded', 'station', `${failed} tâche(s) abandonnée(s)`, 'Une analyse, un envoi ou une sauvegarde n\'a pas abouti : rien n\'est perdu, vous pouvez la relancer.', 'Voir la file', '#/options');
  // Membres
  const accounts = ac?.accounts || [], full = accounts.filter(a => !a.unlimited && a.quota_bytes && a.used_bytes / a.quota_bytes >= .9).length;
  if (full) add('degraded', 'members', `${full} MULTIPASS presque plein(s)`, 'Plus de 90 % du disque : leurs prochains envois seront refusés.', 'Voir les comptes', '#/station');
  const locked = accounts.filter(a => a.pass_locked_until).length;
  if (locked) add('degraded', 'members', `${locked} compte(s) verrouillé(s) après des PASS faux`, 'Un anti-bruteforce a joué : le membre a peut-être oublié son PASS, ou quelqu\'un essaie.', 'Voir les comptes', '#/station');
  const grace = accounts.filter(a => a.grace_ends_at).length;
  if (grace) add('degraded', 'members', `${grace} compte(s) en période de grâce`, 'Hébergement non réglé : passé le délai, le compte est suspendu.', 'Voir les comptes', '#/station');
  // Économie
  if (ec?.kind === 'drawdown') add('degraded', 'economy', `Semaine ${ec.week} : trésorerie entamée`, 'Les dépenses ont dépassé les recettes de la semaine.', 'Voir la régie', '#/coop');
  else if (ec?.kind === 'volunteer') add('info', 'economy', `Semaine ${ec.week} : bénévolat actif`, 'Votre apport a couvert l\'écart de la semaine.', 'Voir la régie', '#/coop');
  todos.sort((a, b) => RANK[a.level] - RANK[b.level]);
  const level = todos.some(x => x.level === 'down') ? 'down' : todos.some(x => x.level === 'degraded') ? 'degraded' : 'ok';
  const floors = Object.fromEntries(FLOORS.map(([k]) => [k, todos.filter(x => x.floor === k).reduce((l, x) => worst(l, x.level), 'ok')]));
  const urgent = todos.filter(x => x.level !== 'info').length;
  const verdict = level === 'ok' ? 'Tout va bien' : level === 'down' ? 'Intervention nécessaire' : urgent === 1 ? 'Un point à regarder' : `${urgent} points à regarder`;
  return { level, verdict, todos, floors, me, others, nodes };
}

const TIER = { Light: '🌿', Standard: '⚡', Brain: '🔥' };
export default async function cockpit(root, K) {
  const { h, t } = K, api = p => K.api(p), soft = p => K.api(p).catch(() => null);
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-cockpit.css' }); document.head.append(link);
  const draw = async () => {
    const [dash, health, cs, ec, ac, tun, tk, dns] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json()).catch(() => null), soft('/api/v1/station/constellation'), soft('/api/v1/station/economy'), soft('/api/v1/station/accounts'), soft('/api/v1/station/admin/tunnels'), soft('/api/v1/station/admin/tasks'), soft('/api/v1/station/admin/dnslink')]);
    const a = analyze({ health, cs, ec, ac, tun, tk, dash, dns }), st = dash.station;
    const todo = x => h('li', { class: 'ck-todo ' + x.level }, h('div', {}, h('strong', {}, t(x.title)), h('p', { class: 'muted' }, t(x.why))), h('a', { class: 'ck-btn', href: x.href }, t(x.label)));
    const floors = FLOORS.map(([k, icon, name, href]) => {
      const lv = a.floors[k], line = {
        machine: a.me ? `${TIER[a.me.tier] || ''} ${a.me.tier} · Power-Score ${a.me.power_score}` : '—',
        station: `${st.version} · ${t('en marche depuis')} ${K.span(st.uptime_secs)}`,
        mesh: `${a.nodes.length} ${t('station(s)')} · ${a.others.filter(n => n.status === 'connected').length} ${t('connectée(s)')}`,
        services: tun ? tun.services.map(s => `${s.name} ${s.up === false ? '○' : '●'}`).join(' · ') : '—',
        members: ac ? `${ac.accounts.length} MULTIPASS` : '—',
        economy: ec?.week ? `${t('semaine')} ${ec.week}` : t('aucune clôture'),
      }[k];
      return h('a', { class: 'card ck-floor ' + lv, href }, h('span', { class: 'ck-ico', 'aria-hidden': 'true' }, icon), h('div', {}, h('strong', {}, t(name)), h('p', { class: 'muted' }, line)), h('span', { class: 'ck-dot', role: 'img', 'aria-label': { ok: t('OK'), info: t('Info'), degraded: t('À regarder'), down: t('Panne') }[lv] }));
    });
    const node = n => {
      const net = n.services?.filter(s => s !== 'ssh') || [];
      return h('div', { class: 'card ck-node' + (n.self ? ' self' : '') }, h('div', {}, h('strong', {}, n.node_id ? '…' + n.node_id.slice(-8) : '—'), n.self ? h('small', { class: 'muted' }, ' ' + t('(vous)')) : null, ' ', h('span', { class: 'ck-st ' + (n.status === 'known' ? 'off' : 'on'), title: n.status }, n.status === 'known' ? t('vue') : t('en ligne'))),
        h('p', {}, `${TIER[n.tier] || ''} ${n.tier} · ${n.power_score}`, n.distance_km == null ? '' : ` · ${n.distance_km} km`), net.length ? h('p', {}, net.map(s => h('span', { class: 'ck-chip' }, s))) : h('p', { class: 'muted' }, t('aucun service partagé')));
    };
    const brains = a.nodes.filter(n => n.tier === 'Brain' && n.services?.includes('ollama')).length;
    root.replaceChildren(...[
      h('section', { class: 'card ck-hero ' + a.level, 'aria-live': 'polite' }, h('span', { class: 'ck-big', 'aria-hidden': 'true' }, { ok: '✅', degraded: '🟠', down: '🔴' }[a.level]), h('div', {}, h('h2', {}, t(a.verdict)), h('p', { class: 'muted' }, a.todos.length ? t('Voici par quoi commencer :') : t('Rien ne demande votre attention. La page se met à jour toute seule.')))),
      a.todos.length ? h('section', { class: 'card' }, h('h2', {}, t('À faire')), h('ul', { class: 'ck-list' }, a.todos.slice(0, 3).map(todo)), a.todos.length > 3 ? h('details', {}, h('summary', {}, t('Voir les {0} autres', a.todos.length - 3)), h('ul', { class: 'ck-list' }, a.todos.slice(3).map(todo))) : null) : null,
      h('h2', { class: 'ck-h' }, t('Les étages de la station')), h('div', { class: 'grid' }, floors),
      h('h2', { class: 'ck-h' }, t('L\'essaim'), h('small', { class: 'muted' }, brains ? ` · ${brains} ${t('Brain(s) offrent Ollama')}` : '')), h('div', { class: 'grid' }, a.nodes.map(node)),
      dns?.configured ? h('section', { class: 'card' }, h('h2', {}, t('Adresses de la coopérative')),
        dns.station?.name ? h('p', {}, '📡 ', h('code', {}, dns.station.name), h('small', { class: 'muted' }, ' ' + t('cette station'))) : null,
        h('p', { class: 'muted' }, t('{0} adresse(s) à jour sur {1}', dns.unchanged + dns.written, dns.total), dns.pending ? ' · ' + t('{0} en attente', dns.pending) : '', dns.failed ? ' · ' + t('{0} en échec', dns.failed) : ''),
        h('button', { onclick: async ev => { ev.currentTarget.disabled = true; try { await K.api('/api/v1/station/admin/dnslink', { method: 'POST', body: { action: 'sync' } }); await draw(); } catch (e) { K.toast(e.message); ev.currentTarget.disabled = false; } } }, t('Enregistrer maintenant'))) : null,
      h('p', { class: 'muted' }, h('a', { href: '#/station' }, t('Détails techniques')), ' · ', h('a', { href: '#/options' }, t('Tunnels et accès')), ' · ', h('a', { href: '#/coop' }, t('Configuration coopérative')))].filter(Boolean));
  };
  K.cleanup(() => link.remove());
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
