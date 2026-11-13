'use strict';
// Cockpit du Capitaine : un verdict unique, ce qui demande une action (3 au plus), les six étages de la station et l'essaim résumé
// (le détail des stations est dans l'onglet Essaim et pairs).
// Les règles (`analyze`) sont une fonction pure : tests/cockpit_rules.mjs les rejoue sans navigateur.
const RANK = { down: 0, degraded: 1, info: 2, ok: 3 };
const FLOORS = [['machine', '🖥️', 'Machine', '#/machine'], ['station', '📡', 'Station', '#/machine'], ['mesh', '🌐', 'Constellation', '#/planes'], ['services', '🔌', 'Services et accès', '#/options'], ['members', '👥', 'Membres', '#/members'], ['economy', '🪙', 'Économie', '#/coop']];
const worst = (a, b) => (RANK[a] <= RANK[b] ? a : b);

export function analyze({ health, cs, ec, ac, tun, tk, dash, dns, en }) {
  const todos = [], add = (level, floor, title, why, label, href, post) => todos.push({ level, floor, title, why, label, href, post });
  const nodes = cs?.nodes || [], me = nodes.find(n => n.self), others = nodes.filter(n => !n.self);
  // Station
  for (const [name, v] of Object.entries(health?.subsystems || {})) {
    if (v.status === 'down' || v.status === 'degraded') add(v.status, 'station', `${name} : ${v.status === 'down' ? 'en panne' : 'dégradé'}`, v.detail || '', 'Voir le détail', '#/machine');
  }
  // Constellation
  if (dash?.p2p && dash.p2p.enabled === false) add('degraded', 'mesh', 'Le maillage P2P est arrêté', 'Sans lui, aucune station ne peut vous joindre ni partager Ollama ou ComfyUI. Démarrez avec --p2p-listen.', 'Voir les constellations', '#/planes');
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
  if (full) add('degraded', 'members', `${full} MULTIPASS presque plein(s)`, 'Plus de 90 % du disque : leurs prochains envois seront refusés.', 'Voir les comptes', '#/members');
  // PASS verrouillé : l'action se fait d'ici (un bouton par compte, 3 au plus), sans passer par la table des comptes.
  const locked = accounts.filter(a => a.pass_locked_until);
  for (const a of locked.slice(0, 3)) add('degraded', 'members', ['Le PASS de {0} est verrouillé', a.email || a.hex?.slice(0, 12)], 'Trop de PASS faux : le membre l\'a peut-être oublié, ou quelqu\'un essaie. Débloquez si c\'est bien lui qui vous le demande.', 'Débloquer', '#/members?f=locked', `/api/v1/station/accounts/${a.hex}/unlock-pass`);
  if (locked.length > 3) add('degraded', 'members', ['{0} autre(s) compte(s) verrouillé(s)', locked.length - 3], '', 'Voir les comptes', '#/members?f=locked');
  const grace = accounts.filter(a => a.grace_ends_at).length;
  if (grace) add('degraded', 'members', `${grace} compte(s) en période de grâce`, 'Hébergement non réglé : passé le délai, le compte est suspendu.', 'Voir les comptes', '#/members?f=grace_period');
  // Économie
  if (ec?.kind === 'drawdown') add('degraded', 'economy', `Semaine ${ec.week} : trésorerie entamée`, 'Les dépenses ont dépassé les recettes de la semaine.', 'Voir la régie', '#/coop');
  else if (ec?.kind === 'volunteer') add('info', 'economy', `Semaine ${ec.week} : bénévolat actif`, 'Votre apport a couvert l\'écart de la semaine.', 'Voir la régie', '#/coop');
  // Énergie (plugin système energy) : absent = rien à dire ; arrêté ou hors contrat = à regarder.
  if (en?.state === 'stale') add('degraded', 'machine', 'Les mesures d\'énergie sont arrêtées', 'Le collecteur du plugin energy n\'écrit plus depuis plus d\'une heure : systemctl --user status energy-collect.timer', 'Voir la machine', '#/cockpit');
  else if (en?.state === 'invalid') add('degraded', 'machine', 'Mesures d\'énergie illisibles', en.error || '', 'Voir la machine', '#/cockpit');
  todos.sort((a, b) => RANK[a.level] - RANK[b.level]);
  const level = todos.some(x => x.level === 'down') ? 'down' : todos.some(x => x.level === 'degraded') ? 'degraded' : 'ok';
  const floors = Object.fromEntries(FLOORS.map(([k]) => [k, todos.filter(x => x.floor === k).reduce((l, x) => worst(l, x.level), 'ok')]));
  const urgent = todos.filter(x => x.level !== 'info').length;
  const verdict = level === 'ok' ? 'Tout va bien' : level === 'down' ? 'Intervention nécessaire' : urgent === 1 ? 'Un point à regarder' : `${urgent} points à regarder`;
  return { level, verdict, todos, floors, me, others, nodes };
}

// « Mettre la station en service » : ce qu'un Capitaine fait la première semaine, coché d'après l'état réel.
export function setup({ dash, cs, ac, dns }) {
  const list = [
    { done: !!dash?.station?.domain, title: 'Donner un nom de domaine à la station', why: 'Pour que vos membres l\'ouvrent en HTTPS depuis leur téléphone.', href: '#/cockpit', act: 'domain' },
    { done: dash?.p2p?.enabled === true, title: 'Rejoindre le maillage P2P', why: 'Sans lui, aucune autre station ne peut vous joindre (--p2p-listen).', href: '#/planes' },
    { done: (cs?.nodes || []).some(n => !n.self), title: 'Voir d\'autres stations', why: 'Rejoignez une constellation ou indiquez un dossier de pairs.', href: '#/planes' },
    { done: dns?.configured === true, title: 'Activer les adresses de la coopérative', why: 'La station et chaque MULTIPASS obtiennent leur adresse : renseignez les clés OVH.', href: '#/coop' },
    { done: (ac?.accounts || []).length > 1, title: 'Inviter votre premier membre', why: 'Créez un lien d\'accueil et son QR : qui l\'ouvre crée son MULTIPASS et arrive dans la discussion avec vous.', href: '#/cockpit', act: 'welcome' },
  ];
  return { list, left: list.filter(s => !s.done).length };
}

// Liens d'accueil : un lien et son QR pour plusieurs personnes ; qui l'ouvre devient ami du Capitaine et arrive dans sa discussion.
function welcomeDialog(K) {
  const { h, t } = K, list = h('ul', { class: 'wl-list' }), shown = h('div', { class: 'wl-shown', role: 'status' });
  const show = l => shown.replaceChildren(h('p', {}, h('strong', {}, l.label || t('Lien d\'accueil'))), svgOf(l.qr_svg), h('p', {}, h('code', {}, l.url)),
    h('p', {}, h('button', { type: 'button', onclick: () => navigator.clipboard?.writeText(l.url).then(() => K.toast(t('Lien copié'), 1), e => K.toast(e.message)) }, t('Copier le lien'))));
  const act = (label, f) => h('button', { type: 'button', class: 'ghost', onclick: () => f().catch(e => K.toast(e.message)) }, label);
  const load = async () => {
    const r = await K.api('/api/v1/station/welcome');
    list.replaceChildren(...(r.links.length ? r.links.map(l => h('li', { class: l.active ? '' : 'muted' },
      h('div', {}, h('strong', {}, l.label || '—'), h('br'), h('small', { class: 'muted' }, t('{0} sur {1} place(s) prise(s)', l.uses, l.max_uses), ' · ', l.active ? t('jusqu\'au {0}', K.when(l.expires_at)) : t('ne sert plus'))),
      l.active ? h('span', {}, act(t('QR'), async () => show((await K.api('/api/v1/station/welcome?qr=' + l.token)).links.find(x => x.token === l.token))), ' ',
        act(t('Retirer'), async () => { await K.api(`/api/v1/station/welcome/${l.token}/revoke`, { method: 'POST' }); await load(); })) : null))
      : [h('li', { class: 'muted' }, t('Aucun lien pour l\'instant.'))]));
  };
  const label = h('input', { id: 'wl-label', maxLength: 80, placeholder: t('Hackerspace, marché du samedi, article…') });
  const uses = h('input', { id: 'wl-uses', type: 'number', min: 1, max: 1000, value: 50 });
  const form = h('form', { onsubmit: async ev => {
    ev.preventDefault();
    try { show(await K.api('/api/v1/station/welcome', { method: 'POST', body: { label: label.value, max_uses: Math.trunc(+uses.value) } })); label.value = ''; await load(); } catch (e) { K.toast(e.message); }
  } }, h('label', { for: 'wl-label' }, t('Pour qui, ou pour où ?')), label, h('label', { for: 'wl-uses' }, t('Nombre de places')), uses, h('p', {}, h('button', { type: 'submit' }, t('Créer le lien'))));
  const dlg = h('dialog', { 'aria-labelledby': 'wl-t', onclose: () => dlg.remove() }, h('button', { type: 'button', class: 'x', 'aria-label': t('Fermer'), onclick: () => dlg.close() }, '×'),
    h('h2', { id: 'wl-t' }, t('Liens d\'accueil')),
    h('p', { class: 'muted' }, t('Un lien et son QR, pour plusieurs personnes : à l\'entrée d\'un lieu, sur un site ou dans un article. Qui l\'ouvre crée son MULTIPASS, devient votre ami et arrive dans la discussion avec vous. Valable 90 jours, retirable à tout moment.')),
    form, shown, h('h3', {}, t('Vos liens')), list);
  document.body.append(dlg); dlg.showModal(); label.focus();
  load().catch(e => K.toast(e.message));
  return dlg;
}

const TIER = { Light: '🌿', Standard: '⚡', Brain: '🔥' };
// Nombre à la française (virgule décimale), `d` décimales.
const fmt = (v, d) => Number(v ?? 0).toLocaleString('fr-FR', { minimumFractionDigits: d, maximumFractionDigits: d });
// Montant en Ğ1 depuis des centimes, sans flottant.
export const g1 = c => `${Math.floor(c / 100)},${String(c % 100).padStart(2, '0')}`;
const svgOf = s => { const d = s && new DOMParser().parseFromString(s, 'image/svg+xml').documentElement; return d?.nodeName === 'svg' ? document.importNode(d, true) : null; };
export default async function cockpit(root, K) {
  const { h, t } = K, api = p => K.api(p), soft = p => K.api(p).catch(() => null);
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-cockpit.css' }); document.head.append(link);
  // Carburant : le portefeuille maître à créditer pour ouvrir les portefeuilles coopératifs (transaction primale).
  // Une jauge d'abord (réserve / besoin, en centimes entiers) ; l'adresse et son QR, repliés, ne servent qu'au moment du virement.
  // Énergie de la station (plugin système energy) : consommation, production solaire ; rien tant que le plugin manque.
  const energyCard = e => {
    if (!e || e.state === 'absent' || e.state === 'invalid') return null;
    const c = e.consumption, p = e.production;
    return h('section', { class: 'card' }, h('h2', {}, '⚡ ', t('Énergie de la station'), e.state === 'stale' ? h('small', { class: 'muted' }, ' · ' + t('mesures arrêtées depuis {0}', K.span(e.age_secs))) : null),
      c ? h('p', {}, '🔌 ', h('strong', {}, `${fmt(c.power_w ?? c.avg_w_recent, 1)} W`), ' · ', t('{0} kWh aujourd\'hui · {1} kWh ce mois-ci · moyenne {2} W sur 30 jours', fmt(c.today_kwh, 2), fmt(c.month_kwh, 1), fmt(c.avg_w_recent, 1))) : null,
      p ? h('p', {}, '☀️ ', h('strong', {}, `${fmt(p.power_w ?? 0, 0)} W`), ' · ', t('{0} Wh aujourd\'hui · {1} kWh ce mois-ci', fmt(p.today_wh, 0), fmt(p.month_wh / 1000, 1)), p.battery_v ? ` · 🔋 ${fmt(p.battery_v, 1)} V` : '') : null,
      c ? h('p', { class: 'muted' }, t('Mesuré depuis le {0} ({1} jours) : la balise publique de la station porte ces chiffres, pour que la PAF reste juste.', c.since, c.days_recorded)) : null);
  };
  const fuelCard = f => (f?.needed_g1_cents > 0 ? h('section', { class: 'card ck-fuel' }, h('h2', {}, '⛽ ', t('Réserve de démarrage')),
    h('p', {}, h('strong', {}, t('{0} Ğ1 sur {1}', g1(f.spendable_g1_cents), g1(f.spendable_g1_cents + f.needed_g1_cents)))),
    h('progress', { max: f.spendable_g1_cents + f.needed_g1_cents, value: f.spendable_g1_cents, 'aria-label': t('Réserve de démarrage') }),
    h('p', { class: 'muted' }, t('Pour ouvrir ses {0} portefeuilles coopératifs, la station envoie 1 Ğ1 à chacun depuis son portefeuille maître : il lui manque {1} Ğ1.', f.waiting, g1(f.needed_g1_cents))),
    h('details', {}, h('summary', {}, t('Alimenter depuis Cesium ou Ğecko')),
      h('div', { class: 'ck-fuel-row' }, svgOf(f.qr_svg), h('div', {}, h('code', { class: 'ck-addr' }, f.master),
        h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: () => navigator.clipboard?.writeText(f.master).then(() => K.toast(t('Adresse copiée'), 1), e => K.toast(e.message)) }, t('Copier l\'adresse'))),
        h('p', { class: 'muted' }, t('Disponible : {0} Ğ1 · vérifié le {1}', g1(f.spendable_g1_cents), K.when(f.at))))),
      h('p', { class: 'muted' }, t('Virez-les depuis Cesium ou Ğecko. La station revérifie toutes les 6 h : les portefeuilles s\'ouvrent seuls dès que le dépôt arrive.')))) : null);
  // PAF de cette machine : fixée par le Capitaine (1×PAF au NODE, 2×PAF à son MULTIPASS), estimée d'après ce que la machine apporte.
  const pafCard = (p, reload) => {
    if (!p) return null;
    const z = c => fmt(c / 100, 2) + ' Ẑ', sg = p.suggestion;
    const inp = h('input', { id: 'ck-paf', type: 'number', min: 0, max: p.max_cents / 100, step: p.step_cents / 100, inputMode: 'numeric', value: p.paf_cents != null ? (p.paf_cents / 100).toFixed(2) : (sg.paf_cents / 100).toFixed(2), 'aria-describedby': 'ck-paf-h' });
    const save = async cents => { try { await K.api('/api/v1/station/paf', { method: 'POST', body: { paf_cents: cents } }); K.toast(t('PAF enregistrée : elle sera réglée dès le redémarrage de la station.')); reload(); } catch (e) { K.toast(e.message); } };
    return h('section', { class: 'card' }, h('h2', {}, '💶 ', t('PAF de cette machine')),
      h('p', {}, p.paf_cents != null ? h('strong', {}, t('{0} par semaine', z(p.paf_cents))) : t('Non fixée : la station ne règle aucune charge hebdomadaire.'), p.paf_cents != null ? ' · ' + t('NODE {0} · Capitaine {1}', z(p.paf_cents), z(2 * p.paf_cents)) : ''),
      p.pending_restart ? h('p', { class: 'muted' }, '⏳ ', t('Enregistrée, pas encore réglée : redémarrez la station.')) : null,
      p.source === 'flag' ? h('p', { class: 'muted' }, t('La commande de lancement impose --weekly-costs : elle l\'emporte sur cette valeur.')) : null,
      h('p', { class: 'muted', id: 'ck-paf-h' }, t('Estimation : {0} d\'énergie ({1} W {2} × {3} Ẑ/kWh) + {4} de capacité (Power-Score {5}) = {6}, par paliers de {7}.', z(sg.energy_cents), sg.watts, sg.watts_source === 'measured' ? t('mesurés') : t('supposés'), fmt(sg.kwh_price_cents / 100, 2), z(sg.capacity_cents), sg.power_score, z(sg.paf_cents), z(p.step_cents))),
      sg.coverage_pct != null ? h('p', { class: sg.bingo_cents ? '' : 'muted' }, '☀️ ', t('Le solaire couvre {0} % de la consommation du mois : effort d\'autosuffisance {1}.', sg.coverage_pct, z(sg.solar_cents)), sg.bingo_cents ? [' ', h('strong', {}, t('🎯 Bingo résilience : la production dépasse la consommation, {0} de plus.', z(sg.bingo_cents)))] : null) : null,
      p.coop_default_cents != null ? h('p', { class: 'muted' }, t('Valeur par défaut de la coopérative : {0}.', z(p.coop_default_cents))) : null,
      h('form', { class: 'row', onsubmit: ev => { ev.preventDefault(); const c = Math.round(Number(inp.value) * 100); if (!Number.isFinite(c) || c < 0) return K.toast(t('Montant invalide')); save(c); } },
        h('label', { for: 'ck-paf' }, t('PAF (Ẑ par semaine)')), ' ', inp, ' ', h('button', { type: 'submit', class: 'ck-btn' }, t('Enregistrer')), ' ',
        h('button', { type: 'button', class: 'ck-btn', onclick: () => { inp.value = (sg.paf_cents / 100).toFixed(2); } }, t('Reprendre l\'estimation'))));
  };
  // Nom de domaine : vérifié (DNS, ports), enregistré dans station.conf, appliqué au redémarrage (systemd relance la station).
  const restartBlock = (r, out) => (r.restart
    ? h('button', { type: 'button', class: 'primary', onclick: async ev => {
      const btn = ev.currentTarget; btn.disabled = true;
      try { await K.api('/api/v1/station/restart', { method: 'POST' }); out.replaceChildren(h('p', {}, t('Redémarrage en cours. Dans une minute, ouvrez :'), ' ', h('a', { href: r.url }, r.url)), h('p', { class: 'muted' }, t('Le certificat HTTPS s\'obtient au premier démarrage ; en cas d\'échec, le cockpit en donne la cause.'))); } catch (e) { K.toast(e.message); btn.disabled = false; }
    } }, t('Redémarrer la station'))
    : h('p', {}, t('Puis redémarrez la station sur la machine :'), ' ', h('code', {}, r.restart_command)));
  // Réglages hors de la station (box, proxy), une ligne par geste : donnés par la station selon le mode choisi.
  const stepsBlock = steps => (steps?.length ? h('div', {}, h('h3', {}, t('Réseau, à régler une fois')), h('ul', {}, steps.map(x => h('li', {}, x)))) : null);
  const domainDialog = async () => {
    const info = await api('/api/v1/station/domain').catch(e => { K.toast(e.message); return null; });
    if (!info) return;
    const out = h('div', { role: 'status', 'aria-live': 'polite' });
    const pending = info.pending ? h('p', { class: 'muted' }, t('Enregistré : {0}, en attente de redémarrage.', info.saved), ' ', restartBlock({ ...info, url: `https://${info.saved}/` }, out)) : null;
    const inp = h('input', { id: 'ck-dom', type: 'text', inputMode: 'url', autocomplete: 'off', spellcheck: false, required: true, placeholder: 'station.mondomaine.org', value: info.saved || info.domain || '' });
    // Non touchée, la case laisse la station détecter le proxy (ports 80/443 pris ici, ou domaine servi par une autre machine).
    let touched = false;
    const px = h('input', { id: 'ck-px', type: 'checkbox', checked: !!info.proxy, onchange: () => { touched = true; } });
    const steps = h('div', {}, stepsBlock(info.steps));
    const go = h('button', { type: 'submit' }, t('Vérifier et enregistrer'));
    const form = h('form', { onsubmit: async ev => {
      ev.preventDefault(); go.disabled = true; out.replaceChildren(h('p', { class: 'muted' }, t('Vérification du DNS…')));
      try {
        const r = await K.api('/api/v1/station/domain', { method: 'POST', body: { domain: inp.value, proxy: touched ? px.checked : null } });
        pending?.remove(); px.checked = r.proxy; steps.replaceChildren();
        out.replaceChildren(h('p', {}, '✅ ', t(r.proxy ? '{0} pointe vers {1} : domaine enregistré, servi par votre reverse proxy.' : '{0} pointe vers {1} : domaine enregistré.', r.domain, r.ips.join(', '))), h('ul', {}, r.warnings.map(w => h('li', {}, w))),
          stepsBlock(r.steps), h('p', { class: 'muted' }, t('Après le redémarrage, l\'interface sera à {0}.', r.url)), restartBlock(r, out));
      } catch (e) { out.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); } finally { go.disabled = false; }
    } }, h('label', { for: 'ck-dom' }, t('Nom de domaine')), inp,
      h('p', {}, h('label', {}, px, ' ', t('Un reverse proxy (Nginx Proxy Manager…) sert ce domaine, sur cette machine ou une autre'))), h('p', {}, go));
    const dlg = h('dialog', { 'aria-labelledby': 'ck-dom-t', onclose: () => dlg.remove() },
      h('button', { type: 'button', class: 'x', 'aria-label': t('Fermer'), onclick: () => dlg.close() }, '×'),
      h('h2', { id: 'ck-dom-t' }, t('Nom de domaine de la station')),
      h('ol', {}, h('li', {}, t('Chez votre fournisseur de domaine, faites pointer le nom (enregistrement A) vers l\'adresse publique de votre box.')), h('li', {}, t('Enregistrez ici : la station vérifie qui répond à ce nom et vous dit quoi régler dans la box ou le proxy.')), h('li', {}, t('Redémarrez : la station obtient seule son certificat HTTPS, ou se présente derrière votre proxy.'))),
      pending, form, steps, out);
    document.body.append(dlg); dlg.showModal(); inp.focus();
    K.cleanup(() => dlg.remove());
  };
  const welcome = () => { const d = welcomeDialog(K); K.cleanup(() => d.remove()); };
  const draw = async () => {
    const [dash, health, cs, ec, ac, tun, tk, dns, fuel, en, paf] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json()).catch(() => null), soft('/api/v1/station/constellation'), soft('/api/v1/station/economy'), soft('/api/v1/station/accounts'), soft('/api/v1/station/admin/tunnels'), soft('/api/v1/station/admin/tasks'), soft('/api/v1/station/admin/dnslink'), soft('/api/v1/station/fuel'), soft('/api/v1/station/energy'), soft('/api/v1/station/paf')]);
    const a = analyze({ health, cs, ec, ac, tun, tk, dash, dns, en }), st = dash.station, su = setup({ dash, cs, ac, dns });
    // Tant que la mise en service n'est pas finie, ses étapes disent déjà les informations (station seule, adresses) : pas de doublon.
    const todos = su.left ? a.todos.filter(x => x.level !== 'info') : a.todos;
    const todo = x => h('li', { class: 'ck-todo ' + x.level }, h('div', {}, h('strong', {}, t(...[x.title].flat())), h('p', { class: 'muted' }, t(x.why))), x.post
      ? h('button', { type: 'button', class: 'ck-btn', onclick: async ev => { const b = ev.currentTarget; b.disabled = true; try { await K.api(x.post, { method: 'POST' }); K.toast(t('PASS déverrouillé'), 1); await draw(); } catch (e) { K.toast(e.message); b.disabled = false; } } }, t(x.label))
      : h('a', { class: 'ck-btn', href: x.href }, t(x.label)));
    const floors = FLOORS.map(([k, icon, name, href]) => {
      const lv = a.floors[k], line = {
        machine: (a.me ? `${TIER[a.me.tier] || ''} ${a.me.tier} · Power-Score ${a.me.power_score}` : '—') + (en?.state === 'fresh' && en.consumption?.power_w != null ? ` · ⚡ ${fmt(en.consumption.power_w, 1)} W` : ''),
        station: `${st.version} · ${t('en marche depuis')} ${K.span(st.uptime_secs)}`,
        mesh: `${a.nodes.length} ${t('station(s)')} · ${a.others.filter(n => n.status === 'connected').length} ${t('connectée(s)')}`,
        services: tun ? tun.services.map(s => `${s.name} ${s.up === false ? '○' : '●'}`).join(' · ') : '—',
        members: ac ? `${ac.accounts.length} MULTIPASS` : '—',
        economy: ec?.week ? `${t('semaine')} ${ec.week}` : t('aucune clôture'),
      }[k];
      return h('a', { class: 'card ck-floor ' + lv, href }, h('span', { class: 'ck-ico', 'aria-hidden': 'true' }, icon), h('div', {}, h('strong', {}, t(name)), h('p', { class: 'muted' }, line)), h('span', { class: 'ck-dot', role: 'img', 'aria-label': { ok: t('OK'), info: t('Info'), degraded: t('À regarder'), down: t('Panne') }[lv] }));
    });
    const online = a.others.filter(n => n.status === 'connected').length, brains = a.nodes.filter(n => n.tier === 'Brain' && n.services?.includes('ollama')).length;
    root.replaceChildren(...[
      h('section', { class: 'card ck-hero ' + a.level, 'aria-live': 'polite' }, h('span', { class: 'ck-big', 'aria-hidden': 'true' }, { ok: '✅', degraded: '🟠', down: '🔴' }[a.level]), h('div', {}, h('h2', {}, t(a.verdict)), h('p', { class: 'muted' }, todos.length ? t('Voici par quoi commencer :') : t('Rien ne demande votre attention. La page se met à jour toute seule.')))),
      todos.length ? h('section', { class: 'card' }, h('h2', {}, t('À faire')), h('ul', { class: 'ck-list' }, todos.slice(0, 3).map(todo)), todos.length > 3 ? h('details', {}, h('summary', {}, t('Voir les {0} autres', todos.length - 3)), h('ul', { class: 'ck-list' }, todos.slice(3).map(todo))) : null) : null,
      fuelCard(fuel),
      energyCard(en),
      pafCard(paf, draw),
      su.left ? h('section', { class: 'card' }, h('h2', {}, t('Mettre la station en service'), h('small', { class: 'muted' }, ` · ${su.list.length - su.left}/${su.list.length}`)),
        h('progress', { max: su.list.length, value: su.list.length - su.left, 'aria-label': t('Étapes terminées') }),
        h('ul', { class: 'ck-steps' }, su.list.map(x => h('li', { class: x.done ? 'done' : '' }, h('span', { 'aria-hidden': 'true' }, x.done ? '✅' : '⬜'), h('div', {}, x.done ? t(x.title) : h('a', { href: x.href, onclick: x.act ? ev => { ev.preventDefault(); x.act === 'domain' ? domainDialog() : welcome(); } : null }, h('strong', {}, t(x.title))), x.done ? null : h('p', { class: 'muted' }, t(x.why))))))) : null,
      h('h2', { class: 'ck-h' }, t('Les étages de la station')), h('div', { class: 'grid' }, floors),
      h('a', { class: 'card ck-floor ' + (online ? 'ok' : 'info'), href: '#/mesh' }, h('span', { class: 'ck-ico', 'aria-hidden': 'true' }, '🌐'), h('div', {}, h('strong', {}, t('L\'essaim')),
        h('p', { class: 'muted' }, `${a.others.length} ${t('station(s) vue(s)')} · ${online} ${t('connectée(s)')}${brains ? ` · ${brains} ${t('Brain(s) offrent Ollama')}` : ''}`)), h('span', { class: 'ck-btn' }, t('Voir l\'essaim'))),
      dns?.configured ? h('section', { class: 'card' }, h('h2', {}, t('Adresses de la coopérative')),
        dns.station?.name ? h('p', {}, '📡 ', h('code', {}, dns.station.name), h('small', { class: 'muted' }, ' ' + t('cette station'))) : null,
        h('p', { class: 'muted' }, t('{0} adresse(s) à jour sur {1}', dns.unchanged + dns.written, dns.total), dns.pending ? ' · ' + t('{0} en attente', dns.pending) : '', dns.failed ? ' · ' + t('{0} en échec', dns.failed) : ''),
        h('button', { onclick: async ev => { const btn = ev.currentTarget; btn.disabled = true; try { await K.api('/api/v1/station/admin/dnslink', { method: 'POST', body: { action: 'sync' } }); await draw(); } catch (e) { K.toast(e.message); btn.disabled = false; } } }, t('Enregistrer maintenant'))) : null,
      h('p', { class: 'ck-acts' }, h('button', { class: 'primary', onclick: welcome }, '✉️ ' + t('Liens d\'accueil')), h('a', { class: 'ck-btn', href: '#/machine' }, t('Détails techniques')))].filter(Boolean));
  };
  K.cleanup(() => link.remove());
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
