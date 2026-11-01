'use strict';
// Lien magique : création du lien partageable d'une sélection de fichiers (dialogue #share). Chargé à la demande.

export default ({ api, toast, h, $, when, b64 }) => {
return async paths => {
const box = $('#share-b'), dlg = $('#share');
$('#share-t').textContent = 'Lien magique';
box.replaceChildren(h('p', { class: 'muted' }, 'Création du lien…'));
if (!dlg.open) dlg.showModal();
try {
const r = await api('/api/v1/ucloud/share', { method: 'POST', body: { paths, ttl_hours: 72 } });
const inp = h('input', { value: r.url, readOnly: true, 'aria-label': 'Lien magique', onfocus: () => inp.select() });
const copy = h('button', { onclick: async () => { try { await navigator.clipboard.writeText(r.url); copy.textContent = 'Copié ✓'; toast('Lien magique copié', 1); } catch { inp.select(); } } }, 'Copier');
toast(`Lien magique créé (${r.items} fichier(s))`, 1);
box.replaceChildren(
h('p', { class: 'muted' }, `${r.items} fichier(s), valable jusqu'au ${when(r.expires_at)}. La clé de déchiffrement est dans la partie après « # » : un navigateur ne l'envoie jamais au serveur.`),
inp, h('p', {}, copy),
r.qr_svg && h('img', { alt: 'QR code du lien à scanner avec un smartphone', src: 'data:image/svg+xml;base64,' + b64(r.qr_svg) }));
} catch (e) { box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
};
};
