'use strict';
// Réplication de vos données (module chargé à la demande) : où, et par qui. Les chiffres viennent de la station,
// qui interroge les relais des autres stations (par le maillage P2P) pour vos derniers événements et fichiers.
const VIA = { p2p: 'Maillage P2P', wss: 'Adresse publique (repli)', direct: 'Adresse directe', down: 'Injoignable' };
const LEVEL = { 1: 'Proche', 2: 'Proche de proche' };
const short = t => t.replace(/^p2p:\/\//, '').replace(/\|.*$/, '').slice(-10);

function ring(h, part, total) {
  const ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg'), pct = total ? part / total : 0, c = 2 * Math.PI * 42;
  svg.setAttribute('viewBox', '0 0 100 100'); svg.setAttribute('class', 'rp-ring'); svg.setAttribute('role', 'img'); svg.setAttribute('aria-label', `${Math.round(pct * 100)} % de vos données sont répliquées`);
  const circle = (cls, dash) => { const e = document.createElementNS(ns, 'circle'); e.setAttribute('cx', 50); e.setAttribute('cy', 50); e.setAttribute('r', 42); e.setAttribute('class', cls); if (dash != null) { e.setAttribute('stroke-dasharray', `${dash} ${c}`); e.setAttribute('transform', 'rotate(-90 50 50)'); } svg.append(e); };
  circle('rp-bg'); circle('rp-fg', c * pct);
  const t = document.createElementNS(ns, 'text'); t.setAttribute('x', 50); t.setAttribute('y', 56); t.setAttribute('text-anchor', 'middle'); t.setAttribute('class', 'rp-txt'); t.textContent = `${Math.round(pct * 100)} %`; svg.append(t);
  return svg;
}

export default async function replication(root, K) {
  const { h } = K, link = h('link', { rel: 'stylesheet', href: '/ui/m-replication.css' });
  document.head.append(link); K.cleanup(() => link.remove());
  const draw = async () => {
    const r = await K.api('/api/v1/replication'), m = r.mine, c = r.crawl, now = Date.now() / 1000;
    const peers = m ? m.peers : [], ok = peers.filter(p => !p.error), copies = ok.filter(p => p.have > 0);
    const hero = m
      ? h('section', { class: 'card rp-hero' }, ring(h, m.replicated, m.events),
        h('div', {}, h('h2', {}, 'Vos données ailleurs'),
          m.events ? h('p', { class: 'big' }, `${m.replicated} sur ${m.events}`, h('small', { class: 'muted' }, ' événements répliqués')) : h('p', { class: 'muted' }, 'Aucun événement à répliquer pour le moment.'),
          h('p', { class: 'muted' }, copies.length ? `Présents chez ${copies.length} autre(s) station(s)${copies.some(p => p.level) ? `, dont ${copies.filter(p => p.level).length} de vos proches` : ''}. Mesuré ${K.span(now - m.at)} avant maintenant.` : 'Aucune autre station ne détient encore vos événements.'),
          m.unreachable ? h('p', { class: 'degraded', role: 'status' }, `${m.unreachable} station(s) n'ont pas répondu : leur contenu est inconnu, pas absent.`) : null))
      : h('section', { class: 'card' }, h('h2', {}, 'Vos données ailleurs'), h('p', { class: 'muted' }, 'Pas encore de mesure : la première passe a lieu quelques minutes après le démarrage de la station.'));
    const stations = m && peers.length
      ? h('section', { class: 'card wide' }, h('h2', {}, 'Qui détient quoi ', h('small', {}, `${peers.length} station(s) de la constellation`)),
        K.table('Chaque station est interrogée sur vos derniers événements', ['Station', 'Capitaine', 'Lien', 'Événements', 'État'],
          peers.slice().sort((a, b) => (b.have ?? -1) - (a.have ?? -1)).map(p => [p.name, p.captain || '—', p.level ? LEVEL[p.level] : 'Autre station',
            p.error ? '—' : [h('meter', { min: 0, max: Math.max(m.events, 1), value: p.have, 'aria-label': `${p.have} sur ${m.events}` }), ` ${p.have} / ${m.events}`],
            p.error ? [K.pill('down'), ' ', p.error] : K.pill(p.have === m.events ? 'ok' : p.have ? 'degraded' : 'disabled')])))
      : h('section', { class: 'card wide' }, h('h2', {}, 'Qui détient quoi'), h('p', { class: 'muted' }, m ? 'Aucune autre station connue (dossier --swarm-dir).' : '—'));
    const md = m?.media;
    const media = h('section', { class: 'card' }, h('h2', {}, 'Vos fichiers'),
      md ? [h('p', { class: 'big' }, `${md.replicated} sur ${md.files}`, h('small', { class: 'muted' }, ' fournis par d\'autres stations')),
        md.peers.length ? h('ul', { class: 'muted' }, md.peers.map(p => h('li', {}, `${p.name} : ${p.have} fichier(s)`))) : h('p', { class: 'muted' }, 'Aucune autre station ne fournit encore vos derniers fichiers.'),
        md.lookup_failed ? h('p', { class: 'degraded' }, `${md.lookup_failed} recherche(s) sans réponse.`) : null]
        : h('p', { class: 'muted' }, 'Non mesuré : la station n\'a pas accès à IPFS (option --kubo).'));
    const routes = r.routes || [];
    const sync = h('section', { class: 'card wide' }, h('h2', {}, 'Synchronisation de vos proches ', h('small', {}, 'N¹ et N²')),
      c ? h('p', {}, c.error ? h('span', { class: 'down' }, `Dernier passage interrompu : ${c.error}`) : `Dernier passage ${K.span(now - c.at)} avant maintenant : ${c.inserted} nouvel(s) événement(s) répliqué(s), ${c.fetched} reçu(s) de ${c.keys} membre(s) sur ${c.relays} relais, en ${(c.ms / 1000).toFixed(1)} s.`) : h('p', { class: 'muted' }, 'Aucun passage encore : le premier a lieu une minute après le démarrage.'),
      c?.errors?.length ? h('p', { class: 'degraded' }, `${c.errors.length} échec(s), par exemple : ${c.errors[0].reason}`) : null,
      routes.length ? K.table('Voie utilisée pour joindre chaque station', ['Station', 'Voie', 'Dernier essai', 'Détail'], routes.map(x => [h('code', {}, short(x.target)), [K.pill(x.via === 'down' ? 'down' : x.via === 'wss' ? 'degraded' : 'ok'), ' ', VIA[x.via] || x.via], K.when(x.at), x.error || '—'])) : null);
    root.replaceChildren(h('div', { class: 'grid' }, hero, media, stations, sync));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
