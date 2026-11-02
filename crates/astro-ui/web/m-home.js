'use strict';
// Accueil d'un membre : trois gestes (photos, proches, assistant), « Bien démarrer » coché d'après l'état réel, et une alerte
// seulement si quelque chose le demande. Les chiffres (solde, loyer, disque…) sont à un clic : #/chiffres.
export function steps({ dash, faces, friends, backup, me }) {
  const files = dash?.sovereignty?.encrypted_files || 0;
  const list = [
    { done: files > 0, title: 'Envoyer vos premières photos', why: 'Elles sont chiffrées avant de quitter votre appareil.', href: '#/cloud' },
    ...(faces?.enabled ? [{ done: (faces.people || []).length > 0, title: 'Reconnaître un visage', why: 'Nommez une personne : ses photos se rangent toutes seules.', href: '#/people' }] : []),
    { done: (friends?.friends || []).length > 0, title: 'Retrouver un proche', why: 'Suivez-vous mutuellement pour partager vos photos.', href: '#/contacts' },
    { done: backup?.mine?.status === 'accepted', title: 'Mettre vos fichiers à l\'abri chez un ami', why: 'Si votre station tombe en panne, rien n\'est perdu.', href: '#/account' },
    { done: !!me?.ai, title: 'Brancher un assistant', why: 'BRO répond avec votre propre IA, sans rien envoyer ailleurs.', href: '#/account' },
  ];
  return { list, left: list.filter(s => !s.done).length, files };
}

export default async function home(root, K) {
  const { h, t } = K, soft = p => K.api(p).catch(() => null);
  const draw = async () => {
    const [me, dash, faces, friends, backup, fp] = await Promise.all([K.api('/api/v1/me'), K.api('/api/v1/dashboard'), soft('/api/v1/faces'), soft('/api/v1/friends'), soft('/api/v1/backup'), soft('/api/v1/me/footprint')]);
    const s = steps({ dash, faces, friends, backup, me }), nFriends = (friends?.friends || []).length;
    const disk = fp?.disk, full = disk && !disk.unlimited && disk.quota_bytes && disk.used_bytes / disk.quota_bytes >= .9;
    const tile = (icon, title, line, href) => h('a', { class: 'card hm-tile', href }, h('span', { class: 'hm-ico', 'aria-hidden': 'true' }, icon), h('strong', {}, t(title)), h('span', { class: 'muted' }, line));
    const first = (me.email || '').split('@')[0];
    root.replaceChildren(...[
      h('section', { class: 'hm-hero' }, h('h2', {}, first ? t('Bonjour {0}', first) : t('Bonjour')), h('p', { class: 'muted' }, s.files ? t('{0} fichier(s) chiffré(s) sur votre station, à vous seul.', s.files) : t('Votre espace est prêt. Commencez par y déposer quelques photos.'))),
      full ? h('p', { class: 'degraded', role: 'status' }, t('Votre espace est presque plein ({0} %).', Math.round(100 * disk.used_bytes / disk.quota_bytes)), ' ', h('a', { href: '#/chiffres' }, t('Voir mon espace'))) : null,
      me.captain ? h('p', { class: 'hm-cap' }, '⚓ ', t('Vous êtes Capitaine.'), ' ', h('a', { href: '#/cockpit' }, t('Ouvrir le cockpit de la station'))) : null,
      h('div', { class: 'grid hm-tiles' },
        tile('📸', 'Mes photos', s.files ? t('{0} fichier(s)', s.files) : t('Envoyer des photos'), s.files ? '#/library' : '#/cloud'),
        tile('👥', 'Mes proches', nFriends ? t('{0} ami(s)', nFriends) : t('Retrouver vos proches'), '#/people'),
        me.ai ? tile('🤖', 'Mon assistant', t('BRO vous répond'), '#/bro') : tile('🤖', 'Mon assistant', t('Brancher une IA'), '#/account')),
      s.left ? h('section', { class: 'card' }, h('h2', {}, t('Bien démarrer'), h('small', { class: 'muted' }, ` · ${s.list.length - s.left}/${s.list.length}`)),
        h('progress', { max: s.list.length, value: s.list.length - s.left, 'aria-label': t('Étapes terminées') }),
        h('ul', { class: 'hm-steps' }, s.list.map(x => h('li', { class: x.done ? 'done' : '' }, h('span', { 'aria-hidden': 'true' }, x.done ? '✅' : '⬜'), h('div', {}, x.done ? h('span', {}, t(x.title)) : h('a', { href: x.href }, h('strong', {}, t(x.title))), x.done ? null : h('p', { class: 'muted' }, t(x.why))))))) : null,
      h('p', { class: 'muted' }, h('a', { href: '#/chiffres' }, t('Mes chiffres : solde, loyer, espace utilisé')))].filter(Boolean));
  };
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-home.css' }); document.head.append(link); K.cleanup(() => link.remove());
  await draw();
  K.every(60000, () => draw().catch(() => {}));
}
