'use strict';
// Contacts : le carnet d'adresses (CardDAV) et son lien avec vos photos et vos MULTIPASS. Une fiche reliée à un MULTIPASS
// (clé Nostr dans la fiche, ou email d'un compte de cette station) donne sa clé aux visages du même nom : leurs photos
// se partagent alors toutes seules si vous êtes Proches l'un de l'autre.

let fresh = null; // dernière invitation créée : son lien et son QR restent affichés jusqu'au prochain rechargement

export default async function contacts(root, K) {
  const { h, t } = K, api = (p, body) => K.api('/api/v1/contacts' + p, body ? { method: 'POST', body } : undefined);
  root.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
  const v = await api('');
  const invites = await K.api('/api/v1/invites').then(r => r.invites, () => null); // null : invitations non disponibles sur cette station
  const redraw = () => contacts(root, K);
  const act = (fn, done) => async ev => { ev.target.disabled = true; try { const r = await fn(); K.toast(t(done, r.linked ?? ''), 1); redraw(); } catch (e) { K.toast(e.message); ev.target.disabled = false; } };


  // Inviter : crée la fiche si la personne n'en a pas (elle porte le lien avec son futur MULTIPASS), puis le lien.
  const invite = (name, c) => async ev => {
    ev.target.disabled = true;
    try {
      const path = c ? c.path : (await api('/from-person', { name })).path;
      fresh = { ...(await K.api('/api/v1/invites', { method: 'POST', body: { person: name, label: name, email: c?.emails?.[0], contact_path: path } })), to: c?.emails?.[0] };
      K.toast(t('Invitation créée'), 1); redraw();
    } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  };
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  // Envoi par le moyen de l'expéditeur (sa messagerie, son téléphone) : la station n'envoie jamais de courrier elle-même.
  const text = url => t('Voici des photos pour vous : {0}', url), q = encodeURIComponent;
  const send = inv => [
    h('a', { class: 'btn', href: `mailto:${inv.to ? q(inv.to) : ''}?subject=${q(t('Photos pour vous'))}&body=${q(text(inv.url))}` }, 'Email'),
    h('a', { class: 'btn', href: `sms:?&body=${q(text(inv.url))}` }, 'SMS'),
    h('a', { class: 'btn', href: `https://wa.me/?text=${q(text(inv.url))}`, target: '_blank', rel: 'noopener noreferrer' }, 'WhatsApp')];
  const share = inv => navigator.share ? h('button', { class: 'ghost', type: 'button', onclick: () => navigator.share({ title: t('Photos pour vous'), url: inv.url }).catch(() => {}) }, 'Partager') : null;
  const STATES = { pending: 'pas encore ouverte', opened: 'ouverte', claiming: 'en cours', claimed: 'a rejoint', expired: 'expirée', revoked: 'retirée' };

  const phone = h('section', { class: 'card' }, h('h2', {}, 'Synchroniser avec mon téléphone'),
    h('p', { class: 'muted' }, 'Android : DAVx⁵ → Ajouter un compte → URL et identifiant. iPhone : Réglages → Contacts → Comptes → Autre → CardDAV. Le mot de passe est votre jeton (voir Mon compte).'),
    h('dl', {}, h('dt', {}, 'Serveur'), h('dd', {}, h('code', {}, v.carddav.server)), h('dt', {}, 'Identifiant'), h('dd', {}, h('code', {}, v.carddav.login))),
    h('p', {}, h('button', { class: 'ghost', onclick: act(() => api('/sync', {}), 'Carnet relu') }, 'Relire le carnet')));

  const state = c => c.pubkey ? [h('span', { class: 'pill ok' }, c.via === 'email' ? 'MULTIPASS de la station' : 'Clé Nostr'), ' ', c.friends ? 'Proches réciproques' : h('span', { class: 'muted' }, 'pas encore Proches')] : h('span', { class: 'muted' }, 'sans lien');
  const rows = v.contacts.map(c => {
    const target = h('input', { placeholder: 'email du MULTIPASS ou npub…', 'aria-label': 'email du MULTIPASS ou npub…', autocomplete: 'off' });
    return h('tr', {}, h('td', {}, h('strong', {}, c.name), c.warning ? h('div', { class: 'err' }, c.warning) : null, h('div', { class: 'muted' }, c.emails.join(', '))), h('td', {}, state(c)),
      h('td', {}, c.faces ? `${c.faces} (${c.faces_linked})` : '—'),
      h('td', {}, target, ' ', h('button', { class: 'ghost', onclick: act(() => api('/link', { path: c.path, target: target.value.trim() }), 'Fiche reliée') }, 'Relier'),
        c.pubkey && c.via === 'vcard' ? [' ', h('button', { class: 'ghost', onclick: act(() => api('/link', { path: c.path, target: null }), 'Fiche déliée') }, 'Délier')] : null,
        invites && !c.pubkey ? [' ', h('button', { class: 'ghost', disabled: !c.faces, title: c.faces ? '' : t('Aucune photo reconnue de cette personne'), onclick: invite(c.name, c) }, 'Inviter')] : null));
  });
  const book = h('section', { class: 'card wide' }, h('h2', {}, 'Carnet d\'adresses ', h('small', {}, String(v.contacts.length))),
    v.contacts.length ? h('table', {}, h('thead', {}, h('tr', {}, ['Contact', 'Lien MULTIPASS', 'Visages (reliés)', 'Relier'].map(x => h('th', { scope: 'col' }, x)))), h('tbody', {}, rows))
      : h('p', { class: 'muted' }, 'Aucune fiche. Ajoutez des contacts depuis votre téléphone, ou créez-les ci-dessous à partir de vos photos.'));

  const people = v.people_without_contact.length ? h('section', { class: 'card wide' }, h('h2', {}, 'Personnes reconnues sans fiche'),
    h('ul', {}, v.people_without_contact.map(p => h('li', {}, h('strong', {}, p.name), ` — ${p.faces} `, 'visages', ' ', h('button', { class: 'ghost', onclick: act(() => api('/from-person', { name: p.name }), 'Fiche créée') }, 'Créer la fiche'), invites ? [' ', h('button', { class: 'ghost', onclick: invite(p.name, null) }, 'Inviter')] : null)))) : null;


  const link = fresh ? h('section', { class: 'card wide', 'aria-live': 'polite' }, h('h2', {}, t('Lien pour {0}', fresh.label || '')),
    h('p', { class: 'muted' }, t('{0} photo(s) seront partagées dès qu\'elle ouvrira ce lien et créera son compte. Valable jusqu\'au {1}.', fresh.count, new Date(fresh.expires_at * 1000).toLocaleDateString())),
    h('p', { class: 'row' }, h('input', { readonly: true, value: fresh.url, 'aria-label': t('Lien d\'invitation'), onfocus: ev => ev.target.select() }), copy(fresh.url, t('Lien')), share(fresh), send(fresh)),
    fresh.qr_svg ? h('img', { alt: t('QR code du lien, à scanner avec le téléphone de la personne'), width: 200, height: 200, src: 'data:image/svg+xml;base64,' + btoa(fresh.qr_svg) }) : null) : null;
  const sent = invites?.length ? h('section', { class: 'card wide' }, h('h2', {}, 'Invitations ', h('small', {}, String(invites.length))),
    h('table', {}, h('thead', {}, h('tr', {}, ['Pour', 'Photos', 'État', ''].map(x => h('th', { scope: 'col' }, x)))), h('tbody', {}, invites.map(i => {
      const open = i.state === 'pending' || i.state === 'opened';
      return h('tr', {}, h('td', {}, h('strong', {}, i.label || '—')), h('td', {}, String(i.count)),
        h('td', {}, h('span', { class: 'pill' + (i.state === 'claimed' ? ' ok' : '') }, STATES[i.state] || i.state), ' ', h('span', { class: 'muted' }, K.when(i.claimed_at || i.opened_at || i.created_at))),
        h('td', {}, open ? [copy(i.url, t('Lien')), ' ', h('button', { class: 'ghost', onclick: act(() => K.api('/api/v1/invites/' + i.token, { method: 'DELETE' }), 'Lien retiré') }, 'Retirer')] : null));
    })))) : null;
  fresh = null;
  root.replaceChildren(h('h2', {}, 'Contacts'), h('div', { class: 'grid' }, link, phone, book, people, sent));
}
