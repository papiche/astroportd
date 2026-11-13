'use strict';
// Table des routes : l'unique source de vérité de la navigation (onglet actif, sous-onglets, retour, titre).
// Deux espaces : « me » (tout membre) et « station » (Capitaine). Une route « both » prend l'espace où l'on se trouvait.
// Un écran dont `file` est null est encore dans app.js (uCloud).

// Barre principale de chaque espace : [id, icône, libellé, route d'entrée].
export const TABS = {
  me: [['home', '🏠', 'Accueil', '/'], ['files', '🗂️', 'Fichiers', '/cloud'], ['people', '👥', 'Proches', '/people'], ['me', '⚙️', 'Moi', '/account']],
  station: [['health', '🛰️', 'Santé', '/cockpit'], ['members', '👥', 'Membres', '/members'], ['net', '🌐', 'Réseau', '/mesh'], ['eco', '🪙', 'Économie', '/constellation']],
};

// [chemin, fichier, espace, onglet, sous-onglet (ou null : pas de sous-onglet), parent, titre]
const R = [
  ['/', 'm-home.js', 'me', 'home', null, null, 'Accueil'],
  ['/bro', 'm-bro.js', 'me', 'home', null, '/', 'Assistant'],
  ['/cloud', null, 'me', 'files', 'Dossiers', null, 'Fichiers'],
  ['/library', 'm-library.js', 'me', 'files', 'Photos', null, 'Photos'],
  ['/people', 'm-contacts.js', 'me', 'people', 'Carnet', null, 'Proches'],
  ['/chat', 'm-chat.js', 'me', 'people', 'Conversations', null, 'Conversations'],
  ['/people/tri', 'm-people.js', 'me', 'people', null, '/people', 'Trier les visages'],
  ['/account', 'm-account.js', 'me', 'me', 'Compte', null, 'Mon compte'],
  ['/money', 'm-money.js', 'me', 'me', 'Mon argent', null, 'Mon argent'],
  ['/replication', 'm-replication.js', 'me', 'me', 'Sauvegarde', null, 'Sauvegarde'],
  ['/billets', 'm-billets.js', 'me', 'me', null, '/money', 'Billets'],
  ['/constellation', 'm-constellation.js', 'both', null, null, null, 'Constellation'],
  ['/cockpit', 'm-cockpit.js', 'station', 'health', null, null, 'Santé'],
  ['/machine', 'm-machine.js', 'station', 'health', null, '/cockpit', 'Détails techniques'],
  ['/members', 'm-members.js', 'station', 'members', null, null, 'Membres'],
  ['/mesh', 'm-mesh.js', 'station', 'net', 'Essaim', null, 'Essaim et pairs'],
  ['/planes', 'm-planes.js', 'station', 'net', 'Constellations', null, 'Constellations'],
  ['/options', 'm-options.js', 'station', 'net', 'Réglages', null, 'Réglages'],
  ['/coop', 'm-coop.js', 'station', 'eco', 'Régie', null, 'Régie'],
];
export const ROUTES = R.map(([path, file, space, tab, seg, parent, title]) => ({ path, file, space, tab, seg, parent, title }));
// Les adresses d'avant la refonte continuent de marcher.
export const ALIASES = { '/chiffres': '/money', '/station': '/cockpit', '/contacts': '/people', '/sort': '/people/tri' };
// Une route « both » vit dans l'espace Station pour le Capitaine qui y était, sinon dans « Mon argent ».
const BOTH = { station: { tab: 'eco', seg: 'Constellation', parent: null }, me: { tab: 'me', seg: null, parent: '/money' } };

const find = p => ROUTES.filter(r => p === r.path || (r.path !== '/' && p.startsWith(r.path + '/'))).sort((a, b) => b.path.length - a.path.length)[0];

// → { route, space, tab, parent, redirect? } ; une route inconnue ou interdite retombe sur l'accueil.
export function resolve(path, me, last = 'me') {
  path = (path || '/').replace(/\/+$/, '') || '/';
  const al = Object.keys(ALIASES).find(a => path === a || path.startsWith(a + '/'));
  if (al) return { redirect: ALIASES[al] + path.slice(al.length) };
  let r = find(path);
  if (!r || (r.space === 'station' && !me?.captain)) r = find('/');
  const space = r.space === 'both' ? (me?.captain && last === 'station' ? 'station' : 'me') : r.space;
  const x = r.space === 'both' ? BOTH[space] : r;
  return { route: r, space, tab: x.tab, parent: x.parent, seg: x.seg };
}

// Sous-onglets de l'onglet courant (au moins deux, sinon rien).
export function segments(res) {
  const entry = TABS[res.space].find(t => t[0] === res.tab)?.[3];
  const all = ROUTES.filter(r => r.space === res.space && r.tab === res.tab && r.seg).map(r => ({ path: r.path, label: r.seg }));
  if (res.route.space === 'both' && res.space === 'station') all.push({ path: res.route.path, label: 'Constellation' });
  all.sort((a, b) => (b.path === entry) - (a.path === entry));
  return all.length > 1 ? all : [];
}

// Où ramène « ← retour » : le parent déclaré, sinon rien.
export const parentOf = res => (res.parent ? { path: res.parent, title: find(res.parent).title } : null);
export const titleOf = res => (res.route.space === 'both' && res.space === 'station' ? 'Constellation' : res.route.title);
