'use strict';
// Moi › Mon argent : solde, durée couverte, espace, loyer, parts de matériel. Reprend l'ancien écran « Mes chiffres » (#/chiffres), en mots simples.
const zen = c => (c == null ? '—' : (c / 100).toFixed(2));

export default async function money(root, K) {
  const { h, t, api, bytes } = K;
  const draw = async () => {
    const [d, fp, me] = await Promise.all([api('/api/v1/dashboard'), api('/api/v1/me/footprint'), api('/api/v1/me')]);
    const b = d.balance, e = d.energy, w = d.sovereignty, k = fp.disk, ox = fp.opex, cx = fp.capex;
    const pct = k.quota_bytes ? k.used_bytes / k.quota_bytes : 0;
    const card = (title, ...kids) => h('section', { class: 'card' }, h('h2', {}, t(title)), ...kids);
    const big = (v, unit) => h('p', { class: 'big' }, v, h('small', { class: 'muted' }, ' ' + unit));
    root.replaceChildren(
      h('h2', {}, t('Mon argent')),
      h('div', { class: 'grid' },
        card('Solde',
          b.error ? h('p', { class: 'degraded', role: 'status' }, b.error) : [big(zen(b.zen_cents), 'Ẑen'), h('p', { class: 'muted' }, `${zen(b.g1_cents)} Ğ1`), b.stale ? h('p', { class: 'muted' }, t('Dernier solde connu : le réseau est lent.')) : null,
            h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: async () => (await K.mod('m-credit.js')).default(K, me.captain ? { to: me.hex } : { mode: 'member' }) }, me.captain ? t('Verser des Ẑen') : t('Envoyer des Ẑen')))]),
        card('Durée couverte',
          e.weeks == null ? h('p', { class: 'muted' }, t('Solde indisponible.')) : [
            h('meter', { min: 0, max: e.max_weeks, low: 2, high: 6, optimum: e.max_weeks, value: Math.min(e.weeks, e.max_weeks), 'aria-label': t('Semaines de loyer couvertes') }),
            big(e.weeks.toFixed(1), t('semaine(s) de loyer')), h('p', { class: 'muted' }, `${t('Loyer')} : ${zen(e.weekly_rent_cents)} Ẑ / ${t('semaine')}`)]),
        card('Mon espace',
          h('meter', { min: 0, max: 1, low: .7, high: .9, optimum: 0, value: Math.min(pct, 1), 'aria-label': t('Espace utilisé') }),
          big(bytes(k.used_bytes), k.unlimited ? t('sans limite') : `/ ${bytes(k.quota_bytes)}`),
          h('p', { class: 'muted' }, t('{0} fichier(s) chiffré(s) · {1}', w.encrypted_files ?? k.files, bytes(w.encrypted_bytes ?? k.used_bytes)))),
        card('Dépenses des 28 jours', big(zen(ox.total_28d_cents), 'Ẑen'),
          h('p', { class: 'muted' }, `${t('Loyer')} : ${zen(ox.weekly_rent_cents)} Ẑ / ${t('semaine')} · ${t('payé')} ${zen(ox.rent_28d_cents)} Ẑ`),
          h('p', { class: 'muted' }, t('Elles paient l\'usage de la station.'))),
        card('Mes parts de matériel',
          cx.assets.length ? [big(zen(cx.net_cents), t('Ẑen nets')), h('p', { class: 'muted' }, t('Valeur {0} · amortis {1}', zen(cx.value_cents), zen(cx.amortized_cents))), h('ul', { class: 'muted' }, cx.assets.map(a => h('li', {}, `${a.label || a.id} : ${zen(a.net_cents)} Ẑ`)))]
            : h('p', { class: 'muted' }, cx.zencard ? t('Aucune part à votre nom pour l\'instant.') : t('Une ZenCard fait de vous sociétaire : le matériel financé reste votre capital.')))),
      h('p', { class: 'ck-acts' },
        me.billet_issuer ? h('a', { class: 'ck-btn', href: '#/billets' }, '🎫 ' + t('Mes Ğ1Billets')) : null,
        h('a', { class: 'ck-btn', href: '#/constellation' }, '🌍 ' + t('Voir la constellation'))));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
