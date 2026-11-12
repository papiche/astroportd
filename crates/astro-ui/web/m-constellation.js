'use strict';
// « Ma UPlanet » : le nom et la charte, les portefeuilles de la coopérative avec leurs soldes, et l'économie rejouée en carte animée.
// Visible de tous les membres ; seul le Capitaine modifie le nom et la charte (écran Configuration coopérative).
import { flow, short } from '/ui/m-flow.js';

const KIND = { wallet: 'portefeuille', master: 'maître', billets: 'billets', members: 'membres', outside: 'extérieur', intrus: 'intrusion' };
const PRIMAL = { verified: 'reçue du Ğ1 maître ✓', foreign: '✗ venue d\'ailleurs', done: 'envoyée, vérification en cours', pending: 'en attente', master: 'maître' };
const g1 = c => c == null ? '—' : (c / 100).toLocaleString('fr-FR', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

// Texte de la charte : titres `#`, **gras**, liens `[texte](adresse)` réduits à leur texte ; jamais d'HTML.
export function charterNodes(text, h) {
  return text.split(/\n{2,}/).map(par => {
    const clean = par.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').trim();
    if (clean.startsWith('# ')) return h('h3', {}, clean.slice(2));
    return h('p', {}, ...clean.split(/(\*\*[^*]+\*\*)/).filter(Boolean).map(x => x.startsWith('**') ? h('strong', {}, x.slice(2, -2)) : x));
  });
}

// Un SVG de QR rendu par la station, inséré sans `innerHTML` (CSP) : on le relit comme un document XML.
function svg(text, cls) {
  if (!text) return document.createTextNode('');
  const doc = new DOMParser().parseFromString(text, 'image/svg+xml'), n = document.importNode(doc.documentElement, true);
  n.setAttribute('class', cls); n.removeAttribute('width'); n.removeAttribute('height');
  return n;
}

export default async function constellation(box, K) {
  const { h, t } = K;
  box.replaceChildren(h('h2', {}, t('Ma UPlanet')), h('p', { class: 'muted' }, t('Chargement…')));
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-flow.css' }); document.head.append(link); K.cleanup(() => link.remove());
  let ov, led;
  try { [ov, led] = await Promise.all([K.api('/api/v1/constellation'), K.api('/api/v1/constellation/ledger').catch(() => null)]); }
  catch (e) { box.replaceChildren(h('h2', {}, t('Ma UPlanet')), h('p', { class: 'err', role: 'alert' }, e.message)); return; }
  const me = await K.api('/api/v1/me').catch(() => ({})), c = ov.charter || {}, hist = ov.history || {};
  const total = ov.wallets.reduce((n, w) => n + (w.zen_cents || 0), 0);

  const rows = ov.wallets.map(w => h('tr', { class: w.name === 'INTRUSION' && w.g1_cents > 0 ? 'cs-bad' : '' }, h('td', {}, h('strong', {}, w.name)), h('td', { class: 'muted', title: w.address }, `${w.address.slice(0, 6)}…${w.address.slice(-4)}`),
    h('td', { class: 'n' }, g1(w.g1_cents)), h('td', { class: 'n' }, g1(w.zen_cents)), h('td', { class: w.primal === 'verified' ? 'ok' : w.primal === 'foreign' ? 'down' : '' }, t(PRIMAL[w.primal] || w.primal))));
  const wallets = h('section', { class: 'card wide' }, h('h2', {}, t('Portefeuilles de la coopérative')),
    h('p', { class: 'muted' }, t('Visibles par tous les membres : l\'argent commun n\'a rien de caché. Soldes lus sur la chaîne Ğ1.')),
    ov.chain ? null : h('p', { class: 'degraded', role: 'status' }, t('Cette station n\'est pas reliée à la chaîne Ğ1 : les soldes ne sont pas disponibles.')),
    h('div', { class: 'tw' }, h('table', { class: 'cs-wallets' }, h('thead', {}, h('tr', {}, h('th', {}, t('Portefeuille')), h('th', {}, t('Adresse')), h('th', { class: 'n' }, 'Ğ1'), h('th', { class: 'n' }, 'Ẑen'), h('th', {}, t('Transaction primale')))), h('tbody', {}, rows))),
    h('p', {}, h('strong', {}, t('Total : {0} Ẑen', g1(total)))));

  // Alimenter la coopérative (Capitaine) : n'importe quel portefeuille Ğ1 (Cesium, Gecko…) peut verser sur le Ğ1 maître.
  const dp = ov.deposit, qr = svg(dp?.qr, 'cs-qr');
  const deposit = dp ? h('section', { class: 'card wide cs-dep' }, h('h2', {}, t('Alimenter la coopérative')),
    h('p', { class: 'muted' }, t('Depuis n\'importe quel portefeuille Ğ1 (Cesium, Gecko…), virez des Ğ1 sur l\'adresse du Ğ1 maître : il finance les Ẑen et sa première transaction marque tous les portefeuilles de l\'UPlanet.')),
    h('div', { class: 'cs-dep-row' }, qr, h('div', {}, h('p', {}, h('strong', {}, t('Adresse du Ğ1 maître'))),
      h('p', { class: 'cs-addr' }, h('code', {}, dp.address), ' ', h('button', { type: 'button', class: 'ghost', 'aria-label': t('Copier l\'adresse'), onclick: () => navigator.clipboard.writeText(dp.address).then(() => K.toast(t('Adresse copiée'), 1), e => K.toast(e.message)) }, '📋 ', t('Copier'))),
      h('p', {}, t('Solde du Ğ1 maître : {0} Ğ1', g1(dp.g1_cents))),
      h('p', {}, h('button', { type: 'button', onclick: async () => (await K.mod('m-credit.js')).default(K, { to: me.hex }) }, t('Verser des Ẑen à un membre')))))) : null;
  // Dons OpenCollective (Capitaine) : ce qui est à créditer en Ẑen (l'`oc2uplanet.sh` du legacy). La station liste, le Capitaine confirme.
  const oc = me.captain ? await K.api('/api/v1/station/oc/donations').catch(() => null) : null;
  const OCK = { usage: 'crédit d\'usage', satellite: 'part satellite', constellation: 'part constellation', labo: 'R&D' };
  const ocPending = (oc?.donations || []).filter(d => d.status === 'pending' || d.status === 'failed');
  const ocCard = oc?.configured ? h('section', { class: 'card wide' }, h('h2', {}, t('Dons OpenCollective')),
    oc.error ? h('p', { class: 'degraded', role: 'status' }, oc.error) : null,
    h('p', {}, t('{0} don(s) déjà versé(s) ce mois-ci · {1} à créditer ({2} Ẑen)', oc.done || 0, oc.pending || 0, g1(oc.pending_zen_cents || 0)), oc.blocked ? ' · ' + t('{0} bloqué(s)', oc.blocked) : '', oc.elsewhere ? ' · ' + t('{0} pour une autre station', oc.elsewhere) : ''),
    ocPending.length ? h('ul', {}, ocPending.slice(0, 10).map(d => h('li', {}, `${d.email} : ${g1(d.zen_cents)} Ẑen (${t(OCK[d.kind] || d.kind)})`, d.detail ? ` — ${d.detail}` : ''))) : null,
    (oc.donations || []).some(d => d.status === 'blocked') ? h('details', {}, h('summary', {}, t('Dons bloqués')), h('ul', {}, oc.donations.filter(d => d.status === 'blocked').map(d => h('li', {}, `${d.email || '?'} : ${g1(d.zen_cents)} — ${d.detail}`)))) : null,
    oc.pending ? K.sure(t('Créditer {0} don(s)', oc.pending), async () => { try { await K.api('/api/v1/station/oc/sync', { method: 'POST', body: { confirm: true } }); K.toast(t('Dons crédités'), 1); constellation(box, K); } catch (e) { K.toast(e.message); } }, t('{0} Ẑen quittent le Ğ1 maître vers les membres et la coopérative', g1(oc.pending_zen_cents))) : null,
    h('p', { class: 'muted' }, oc.auto ? t('Crédit automatique activé (OC_AUTO_CREDIT) : la station verse seule.') : t('Crédit automatique désactivé : la station liste, vous confirmez (réglage OC_AUTO_CREDIT).'))) : null;
  // Ğ1Billets de l'UPlanet : suivi agrégé, visible de tous (aucun détail personnel).
  const bl = ov.billets, cell = (n, label) => h('div', {}, h('b', {}, String(n)), h('span', { class: 'muted' }, t(label)));
  const billets = bl && bl.issued ? h('section', { class: 'card wide' }, h('h2', {}, t('Ğ1Billets en circulation')),
    h('div', { class: 'cs-bil' }, cell(bl.issued, 'créés'), cell(bl.circulating, 'en circulation'), cell(bl.redeemed, 'encaissés'), cell(bl.returned, 'renvoyés à l’émetteur'), cell(bl.expired, 'expirés non renvoyés')),
    h('p', {}, h('strong', {}, t('Valeur en circulation : {0} Ẑen', bl.circulating_zen)), bl.pending ? ' · ' + t('{0} encaissement(s) à terminer', bl.pending) : ''),
    h('p', { class: 'muted' }, t('Un billet financé mais pas encore encaissé est une valeur qui circule hors de tout compte : elle revient à la coopérative quand on l’encaisse. Sur la carte, ils forment le point « billets ».'))) : null;
  // ── Capitaine : intrus = émetteurs extérieurs (primale ≠ maître) dont les Ğ1 ont été renvoyés vers INTRUSION ──
  const vg = ov.vigilance, rowOf = new Map();
  const copy = a => h('button', { type: 'button', class: 'ghost', 'aria-label': t('Copier l\'adresse'), onclick: () => navigator.clipboard.writeText(a).then(() => K.toast(t('Adresse copiée'), 1), e => K.toast(e.message)) }, '📋');
  const addr = a => [h('code', { title: a }, a), ' ', copy(a)];
  const who = w => w.startsWith('g1') && w.length > 40 ? h('span', {}, h('code', { title: w }, short(w)), ' ', copy(w)) : h('strong', {}, w);
  const wrong = vg?.wrongful?.length ? h('details', {}, h('summary', {}, t('Renvois à tort vers INTRUSION : {0}', vg.wrongful.length)),
    h('ul', {}, vg.wrongful.map(w => h('li', {}, `${g1(w.cents)} Ğ1 · ${K.when(w.at)} · `, t('source'), ' ', who(w.source), ' → ', who(w.via), ' → INTRUSION'))),
    h('p', { class: 'muted' }, t('Source conforme : renvoi fait par erreur.'))) : null;
  const intruders = vg?.intruders?.length ? h('ul', { class: 'cs-intr' }, vg.intruders.map(x => { const li = h('li', {}, h('div', {}, '🚨 ', ...addr(x.address)),
    h('p', { class: 'muted' }, t('{0} Ğ1 renvoyés vers INTRUSION ({1} virement(s)) · dernier le {2} · reçus par', g1(x.cents), x.count, K.when(x.last_at)), ' ', ...x.via.flatMap(w => [who(w), ' '])),
    h('p', { class: 'cs-chain' }, h('span', { class: 'muted' }, t('Primales :')), ...x.chain.slice(1).flatMap((c, i) => [i ? '→' : '', c.master ? h('strong', { class: 'ok' }, t('Ğ1 maître de cette UPlanet')) : c.address ? h('span', {}, h('code', { title: c.address }, short(c.address)), ' ', copy(c.address)) : h('span', { class: 'muted' }, t('suite à lire chez l\'indexeur'))]),
      x.chain.length < 2 ? h('span', { class: 'muted' }, t('inconnue')) : null)); rowOf.set(x.address, li); return li; })) : null;
  const vigil = vg && h('section', { class: 'card wide' + (vg.alert ? ' cs-alert' : ''), id: 'cs-vigil' }, h('h2', {}, vg.alert ? '🚨 ' : '🛡️ ', t('Vigilance des portefeuilles')),
    vg.error ? h('p', { class: 'degraded' }, vg.error) : null,
    vg.officials?.bad?.length ? h('div', { role: 'alert' }, h('p', { class: 'down' }, h('strong', {}, t('Portefeuilles officiels non conformes :'))), h('ul', {}, vg.officials.bad.map(o => h('li', {}, h('strong', {}, o.name), ' ', ...addr(o.address), ' ← ', o.primal ? who(o.primal) : t('aucune'))))) : vg.officials ? h('p', { class: 'ok' }, t('Portefeuilles officiels conformes sur la chaîne : {0}/{1}', vg.officials.checked - vg.officials.unverified, vg.officials.checked)) : null,
    vg.intrusion?.above_reserve_cents > 0 ? h('p', { class: 'down', role: 'alert' }, t('INTRUSION détient {0} Ğ1 au-delà de sa réserve de 1 Ğ1 : décidez de leur renvoi (rien n\'est déplacé automatiquement).', g1(vg.intrusion.above_reserve_cents))) : null,
    intruders || h('p', { class: 'ok' }, t('Aucune intrusion.')),
    wrong,
    vg.unknown_source_cents ? h('p', { class: 'muted' }, t('{0} Ğ1 renvoyés sans émetteur retrouvé (vidage complet).', g1(vg.unknown_source_cents))) : null,
    vg.pending ? h('p', { class: 'muted' }, t('{0} renvoi(s) encore à analyser.', vg.pending)) : null,
    h('p', { class: 'muted' }, t('Règle : tout virement reçu doit venir d\'un portefeuille dont la primale remonte au Ğ1 maître.')));
  const view = h('div', {}), bar = h('div', { class: 'fl-bar' });
  const banner = hist.partial ? h('p', { class: 'degraded', role: 'status' }, hist.error ? t('Historique partiel : la dernière lecture de l\'indexeur a échoué ({0}).', hist.error) : hist.indexer ? t('Historique partiel : l\'indexeur n\'a pas encore été lu.') : t('Historique partiel : cette station n\'a pas d\'indexeur Ğ1 (--squid-url) ; seuls les soldes d\'aujourd\'hui sont connus.')) : null;
  const legend = h('div', { class: 'fl-legend' }, Object.entries(KIND).filter(([k]) => k !== 'intrus' || led?.nodes?.some(n => n.kind === 'intrus')).map(([k, label]) => h('span', {}, h('i', { class: 'fl-lg-' + k }), t(label))));
  let api = null;
  if (led && led.events.length) {
    const play = h('button', { type: 'button' }, '▶ ' + t('Rejouer')), range = h('input', { type: 'range', min: 0, max: 1000, value: 1000, 'aria-label': t('Date dans l\'histoire') });
    const speed = h('select', { 'aria-label': t('Vitesse') }, [1, 4, 16].map(v => h('option', { value: v }, `×${v}`))), when = h('span', { class: 'muted', role: 'status' });
    const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
    // Pendant la lecture, l'intrus dont un virement passe s'éclaire dans la liste.
    api = flow(view, led, { reduced, onEvent: (from, to) => { for (const n of [from, to]) { const li = n.kind === 'intrus' && rowOf.get(n.id); if (li) { li.classList.add('on'); setTimeout(() => li.classList.remove('on'), 1500); } } } });
    const show = st => { when.textContent = `${K.when(st.cur)} · ${t('{0} virement(s) rejoué(s)', st.idx)}`; if (document.activeElement !== range) range.value = api.t1 > api.t0 ? Math.round(1000 * (st.cur - api.t0) / (api.t1 - api.t0)) : 1000; play.textContent = (st.playing ? '⏸ ' + t('Pause') : '▶ ' + t('Rejouer')); };
    api.on(show); show(api.state);
    play.onclick = () => api.toggle();
    range.oninput = () => { api.pause(); api.seek(api.t0 + (api.t1 - api.t0) * range.value / 1000); };
    speed.onchange = () => api.speed(Number(speed.value));
    bar.append(play, range, speed, when);
    K.cleanup(() => api.destroy());
  } else {
    view.append(h('p', { class: 'muted' }, t('Aucun virement à rejouer pour l\'instant.')));
  }
  const map = h('section', { class: 'card wide' }, h('h2', {}, t('L\'économie en mouvement')),
    h('p', { class: 'muted' }, t('Chaque virement est une impulsion dans un tuyau ; la taille d\'un portefeuille suit son solde. Les membres sont regroupés : aucune adresse personnelle n\'apparaît.')),
    banner, view, bar, legend,
    led?.truncated ? h('p', { class: 'muted' }, t('Historique tronqué aux {0} virements les plus anciens.', led.events.length)) : null, vigil);

  const sg = c.signatories || {}, list = sg.list || [], mine = list.find(x => x.mine);
  const sign = h('button', { type: 'button', onclick: async ev => {
    ev.target.disabled = true;
    try { await K.api('/api/v1/constellation/charter/accept', { method: 'POST', body: { sha256: c.sha256 } }); K.toast(t('Charte signée'), 1); constellation(box, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, t('Je signe cette charte (v{0})', c.version || '1.0'));
  const signatories = h('div', {}, h('h3', {}, t('Capitaines signataires')),
    list.length ? h('ul', { class: 'cs-sign' }, list.map(x => h('li', {}, x.current ? '✅ ' : '⬜ ', h('code', {}, x.captain), x.here ? ' · ' + t('cette station') : '', ' — ',
      x.current ? t('a signé la version {0}', sg.version) : x.version ? t('a signé la version {0}, ancienne', x.version) : t('n\'a pas encore signé'), x.at ? ' · ' + K.when(x.at) : '')))
      : h('p', { class: 'muted' }, t('Aucun Capitaine connu de cette station.')),
    me.captain && mine && !mine.current ? h('p', {}, sign) : null,
    h('p', { class: 'muted' }, t('Un Capitaine signe d\'un événement Nostr public ; une nouvelle version de la charte demande une nouvelle signature.')));
  const charter = h('section', { class: 'card wide cs-charter' }, h('h2', {}, t('Charte'), h('small', { class: 'muted' }, ` · v${c.version || '1.0'}${c.custom ? '' : ' · ' + t('charte type')}`)),
    ...charterNodes(c.text || '', h),
    h('p', { class: 'muted' }, c.custom && c.signed_by ? t('Signée par la clé de la coopérative {0}… ; empreinte {1}…', c.signed_by.slice(0, 12), (c.sha256 || '').slice(0, 12)) : t('Texte proposé par défaut : le Capitaine peut le remplacer et le publier, signé par la coopérative.')),
    signatories,
    me.captain ? h('p', {}, h('a', { href: '#/coop' }, t('Modifier le nom et la charte'))) : null);

  const ec = me.captain ? await K.api('/api/v1/station/economy').catch(() => null) : null;
  const regie = ec && h('section', { class: 'card wide' }, h('h2', {}, t('Régie coopérative 3×1/3'), h('small', { class: 'muted' }, ec.week ? ` · ${t('semaine')} ${ec.week}` : '')),
    ec.kind === 'split' ? [h('div', { class: 'shares' }, ec.shares.map(x => h('div', {}, h('strong', {}, x.name), h('progress', { max: 100, value: x.pct, 'aria-label': `${x.name} ${x.pct} %` }), h('span', { class: 'muted' }, `${g1(x.cents)} Ẑ · ${x.pct} %`)))),
      h('p', { class: 'muted' }, t('Surplus réparti {0} Ẑ · provision fiscale {1} Ẑ · prime du Capitaine {2} Ẑ', g1(ec.allocation.surplus_cents), g1(ec.allocation.is_provision_cents), g1(ec.allocation.captain_bonus_cents)))]
      : ec.kind === 'none' ? h('p', { class: 'muted' }, t('Aucune clôture hebdomadaire enregistrée.'))
      : h('p', { class: 'degraded' }, ec.kind === 'drawdown' ? t('Semaine {0} : trésorerie entamée ({1} Ẑ).', ec.week, g1(ec.gap_cents)) : t('Semaine {0} : bénévolat actif, apport du Capitaine de {1} Ẑ.', ec.week, g1(ec.gap_cents))),
    h('h3', {}, 'Love Ledger'), h('p', {}, t('{0} Ẑ donnés sur {1} semaine(s)', g1(ec.love.total_cents), ec.love.weeks)),
    h('p', {}, h('a', { class: 'ck-btn', href: '#/coop' }, t('Configuration coopérative'))));

  const banner2 = vg?.alert ? h('p', { class: 'card cs-alert down', role: 'alert' }, '🚨 ', vg.intruders?.length ? t('Des Ğ1 venus de l\'extérieur de cette UPlanet ont été renvoyés vers INTRUSION.') : t('INTRUSION détient des Ğ1 au-delà de sa réserve.'), ' ', h('button', { type: 'button', onclick: () => document.getElementById('cs-vigil')?.scrollIntoView({ behavior: 'smooth' }) }, t('Voir les intrus'))) : null;
  box.replaceChildren(...[banner2, h('h2', {}, c.name || t('Ma UPlanet')), h('p', { class: 'muted' }, c.name ? t('Le nom de cette constellation') : t('Cette constellation n\'a pas encore de nom : le Capitaine le choisit dans la configuration coopérative.')), map, wallets, deposit, ocCard, billets, regie, charter].filter(Boolean));
}
