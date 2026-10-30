'use strict';
// Mon compte : identité, profil, langue, accès WebDAV (changement du jeton) et export des clés. La station détient les
// clés de ses comptes : modifier le profil, c'est lui faire signer un nouveau kind 0 ; l'export est une action délibérée.
const LANG = { fr: 'Français', en: 'English', es: 'Español' }, ROLE = { multipass: 'MULTIPASS', zencard: 'ZenCard', captain: 'Capitaine', node: 'Nœud', love: 'LOVE' };

export default async function account(root, K) {
  const { h, t } = K, api = (p, body) => K.api('/api/v1/account' + p, body ? { method: 'POST', body } : undefined);
  root.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
  const a = await api('');
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  const code = v => h('code', {}, v || '—');

  // ── Identité ──
  const idCard = h('section', { class: 'card' }, h('h2', {}, 'Identité'), h('dl', {},
    h('dt', {}, 'Compte'), h('dd', {}, a.email || '—', ' ', h('span', { class: 'pill ok' }, ROLE[a.role] || a.role)),
    h('dt', {}, 'Clé publique Nostr'), h('dd', {}, code(a.npub), ' ', copy(a.npub, 'npub')),
    h('dt', {}, 'Clé Ğ1'), h('dd', {}, code(a.g1pub)),
    h('dt', {}, 'Station d\'accueil'), h('dd', {}, code(a.home_station)),
    h('dt', {}, 'Relais annoncés (NIP-65)'), h('dd', {}, a.relays.length ? a.relays.map(r => h('div', {}, code(r.url), ` ${r.read && r.write ? '' : r.read ? '(lecture)' : '(écriture)'}`)) : h('span', { class: 'muted' }, 'aucun'))));

  // ── Profil et langue ──
  const f = { name: h('input', { id: 'ac-name', value: a.profile.name || '', maxlength: 64 }), about: h('textarea', { id: 'ac-about', rows: 3, maxlength: 500 }, a.profile.about || ''), picture: h('input', { id: 'ac-pic', value: a.profile.picture || '', placeholder: 'https://…', maxlength: 256 }),
    lang: h('select', { id: 'ac-lang' }, a.langs.map(l => h('option', { value: l, selected: l === K.lang }, LANG[l] || l))) };
  const save = h('button', { onclick: async ev => {
    const patch = {};
    if (f.name.value.trim() !== (a.profile.name || '')) patch.name = f.name.value.trim();
    if (f.about.value.trim() !== (a.profile.about || '')) patch.about = f.about.value.trim();
    if (f.picture.value.trim() !== (a.profile.picture || '')) patch.picture = f.picture.value.trim();
    const langChanged = f.lang.value !== K.lang;
    if (langChanged) patch.lang = f.lang.value;
    if (!Object.keys(patch).length) return K.toast(t('Aucune modification'));
    ev.target.disabled = true;
    try { await api('/profile', patch); K.toast(t('Profil enregistré'), 1); if (langChanged) K.setLang(f.lang.value); else account(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, 'Enregistrer');
  const profile = h('section', { class: 'card' }, h('h2', {}, 'Profil'),
    h('p', { class: 'muted' }, 'Publié sur le relai (kind 0) : vos Proches le voient.'),
    h('label', { for: 'ac-name' }, 'Nom affiché'), f.name, h('label', { for: 'ac-about' }, 'Présentation'), f.about, h('label', { for: 'ac-pic' }, 'Image (https)'), f.picture,
    h('label', { for: 'ac-lang' }, 'Langue de l\'interface'), f.lang, h('p', {}, save));

  // ── Accès WebDAV : jeton ──
  const shown = h('div', {});
  const rotate = h('button', { class: 'ghost', onclick: async ev => {
    if (!confirm(t('Changer le jeton ? L\'ancien cesse de fonctionner tout de suite : téléphone, DAVx⁵ et autres clients WebDAV devront être reconfigurés.'))) return;
    ev.target.disabled = true;
    try {
      const r = await api('/token', {});
      K.login(r.login, r.token); // cette session suit le nouveau jeton
      shown.replaceChildren(h('p', { class: 'degraded', role: 'status' }, 'Nouveau jeton — il n\'est montré qu\'une fois :'), h('p', {}, code(r.token), ' ', copy(r.token, 'jeton')));
      K.toast(t('Jeton changé'), 1);
    } catch (e) { K.toast(e.message); }
    ev.target.disabled = false;
  } }, 'Changer le jeton');
  const dav = h('section', { class: 'card' }, h('h2', {}, 'Accès WebDAV'),
    h('p', { class: 'muted' }, 'Pour monter vos fichiers (uCloud) sur un ordinateur ou un téléphone.'),
    h('dl', {}, h('dt', {}, 'Adresse'), h('dd', {}, code(a.dav.url), ' ', copy(a.dav.url, 'adresse')), h('dt', {}, 'Identifiant'), h('dd', {}, code(a.dav.login)), h('dt', {}, 'Génération du jeton'), h('dd', {}, String(a.token_generation))),
    h('p', {}, rotate), shown);

  // ── Export des clés ──
  const out = h('div', {});
  const ok = h('input', { type: 'checkbox', id: 'ac-ok' }), exp = h('button', { class: 'ghost', disabled: true, onclick: async ev => {
    ev.target.disabled = true;
    try {
      const k = await api('/keys', { confirm: true });
      out.replaceChildren(h('p', { class: 'degraded', role: 'alert' }, 'Ces clés donnent le contrôle total du compte : ne les copiez que dans un gestionnaire de mots de passe.'),
        h('label', {}, 'Clé secrète Nostr (nsec)'), h('textarea', { rows: 2, readonly: true, 'aria-label': 'nsec' }, k.nsec),
        k.dunikey ? [h('label', {}, 'Clé Ğ1 (.dunikey)'), h('textarea', { rows: 4, readonly: true, 'aria-label': 'dunikey' }, k.dunikey)] : h('p', { class: 'err', role: 'alert' }, `Clé Ğ1 non exportée : ${k.dunikey_error}`),
        h('p', {}, h('button', { class: 'ghost', onclick: () => out.replaceChildren() }, 'Masquer')));
    } catch (e) { K.toast(e.message); }
    ok.checked = false;
  } }, 'Exporter mes clés');
  ok.onchange = () => { exp.disabled = !ok.checked; };
  const keys = h('section', { class: 'card' }, h('h2', {}, 'Clés'),
    h('p', { class: 'muted' }, 'Votre station garde vos clés. Vous pouvez les récupérer pour reprendre le contrôle de votre identité ailleurs (3 exports par heure ; chaque export est tracé).'),
    h('p', {}, ok, h('label', { for: 'ac-ok' }, ' Je comprends que quiconque possède ces clés possède mon compte.')), h('p', {}, exp), out);

  // ── Sauvegarde privée chez un ami ──
  const gb = n => n >= 2 ** 30 ? `${(n / 2 ** 30).toFixed(1)} Go` : `${(n / 2 ** 20).toFixed(0)} Mo`;
  const backup = h('section', { class: 'card' }, h('h2', {}, 'Sauvegarde privée'));
  try {
    const b = await K.api('/api/v1/backup'), post = (path, body) => K.api(path, { method: 'POST', body }).then(() => account(root, K), e => K.toast(e.message));
    const mine = b.mine, friend = h('select', { id: 'bk-friend', 'aria-label': 'Ami qui héberge' }, b.friends.map(f => h('option', { value: f.hex, selected: mine && f.hex === mine.host }, f.label)));
    backup.append(h('p', { class: 'muted' }, 'Vos fichiers et contacts non partagés sont copiés, déjà chiffrés, chez un ami qui l\'accepte : il ne peut rien lire et la copie compte dans son quota. Envoi une fois par nuit (20h12 solaire).'));
    if (!b.friends.length && !mine) backup.append(h('p', { class: 'muted' }, 'Aucun proche réciproque : suivez-vous mutuellement pour choisir un hébergeur.'));
    else if (mine && mine.enabled) backup.append(
      h('p', {}, h('span', { class: mine.status === 'accepted' ? 'pill ok' : 'pill' }, { pending: 'En attente de réponse', accepted: 'Acceptée', declined: 'Refusée', revoked: 'Révoquée' }[mine.status] || mine.status), ` ${mine.label}`),
      mine.status === 'accepted' ? h('p', {}, `${mine.files} fichier(s) · ${gb(mine.backed_up_bytes)} / ${gb(mine.quota_bytes)}`) : null,
      h('p', {}, h('button', { class: 'ghost', onclick: () => post('/api/v1/backup', { enabled: false }) }, 'Suspendre')));
    else backup.append(h('label', { for: 'bk-friend' }, 'Ami qui héberge ma sauvegarde'), friend, h('p', {}, h('button', { onclick: () => post('/api/v1/backup', { enabled: true, host: friend.value }) }, 'Demander à cet ami')));
    // Côté hébergeur : demandes reçues et sauvegardes hébergées.
    for (const r of b.hosting) {
      const size = h('input', { type: 'number', min: 1, step: 1, value: Math.max(1, Math.round(r.quota_bytes / 2 ** 30)) || 5, 'aria-label': 'Volume en Go' });
      backup.append(h('hr'), h('p', {}, h('strong', {}, r.label), r.status === 'pending' ? ' demande à vous confier sa sauvegarde chiffrée.' : ` : ${gb(r.used_bytes)} / ${gb(r.quota_bytes)} hébergés pour lui.`),
        r.status === 'pending' ? h('p', {}, 'Volume mis à disposition (Go) : ', size, ' ', h('button', { onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: true, quota_bytes: Math.round(Number(size.value) * 2 ** 30) }) }, 'Accepter'), ' ', h('button', { class: 'ghost', onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: false }) }, 'Refuser'))
          : h('p', {}, h('button', { class: 'ghost', onclick: () => confirm(t('Révoquer : sa sauvegarde sera supprimée de votre espace.')) && post('/api/v1/backup/respond', { owner: r.owner, accept: false }) }, 'Révoquer et libérer l\'espace')));
    }
  } catch (e) { backup.append(h('p', { class: 'muted' }, e.message)); }

  // Compte invité : l'adresse a été générée par la station. On ne promet pas une fonction qui n'existe pas (changer d'adresse).
  const guest = a.guest ? h('p', { class: 'warn', role: 'note' }, h('strong', {}, 'Compte invité. '), 'Votre adresse a été créée par la station : c\'est votre identifiant UPlanet, pas une boîte aux lettres. Gardez la carte remise à la création (SALT, PEPPER) et votre code PASS : sans email, ce sont vos seuls moyens de récupérer le compte.') : null;
  root.replaceChildren(h('h2', {}, 'Mon compte'), guest, h('div', { class: 'grid' }, idCard, profile, dav, backup, keys));
}
