'use strict';
// Signataire Nostr de l'application : `window.nostr` (NIP-07) pour les scripts de cette origine, sans extension. La clé secrète
// reste sur la station : chaque événement lui est envoyé, signé avec la clé du compte, et revient complété (id, sig). Un jeton
// volé se révoque (Mon compte → changer le jeton) ; une clé Nostr, elle, ne se change pas.
export default function install(me, api) {
  if (window.nostr) return; // une extension est déjà là : on ne la remplace pas
  window.nostr = {
    astro: true,
    getPublicKey: async () => me.hex,
    signEvent: ev => api('/api/v1/account/sign', { method: 'POST', body: { kind: ev.kind, content: ev.content ?? '', tags: ev.tags ?? [], created_at: ev.created_at } }),
  };
}
