'use strict';
// Réglage de l'hébergement dans la page (Capitaine) : quota, prix en ẑ par semaine et, pour un MULTIPASS, le droit de créer des
// Ğ1Billets (l'option apparaît alors dans son interface). Un champ vide retire le réglage propre au compte (retour au palier par défaut).
// `a` : le compte ; sinon `hd` : le palier par défaut des MULTIPASS sans réglage propre.
export default function hosting(box, K, a, hd) {
  const { h, t } = K, who = a && (a.email || a.hex.slice(0, 12));
  const gb = b => b ? Math.round(b / 2 ** 30) : '';
  const quota = h('input', { id: 'ho-q', inputmode: 'decimal', value: a ? gb(a.own_quota_bytes) : gb(hd.default_quota_bytes) });
  const zen = h('input', { id: 'ho-z', inputmode: 'numeric', value: a ? (a.own_weekly_zen ?? '') : (hd.default_weekly_zen || '') });
  const issuer = a ? h('input', { id: 'ho-b', type: 'checkbox' }) : null;
  if (issuer) issuer.checked = !!a.billet_issuer;
  const err = h('p', { class: 'err', role: 'alert' });
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-hosting.css' }); document.head.append(link);
  const close = () => { box.hidden = true; box.replaceChildren(); link.remove(); };
  const save = h('button', { type: 'button', class: 'primary' }, t('Enregistrer'));
  save.onclick = async () => {
    err.textContent = '';
    const g = quota.value.trim() === '' ? null : Number(quota.value.replace(',', '.')), z = zen.value.trim() === '' ? null : Number(zen.value.replace(',', '.'));
    if (g !== null && !(g >= 0 && Number.isFinite(g))) return void (err.textContent = t('Nombre de Go attendu'));
    if (z !== null && !(Number.isInteger(z) && z >= 0)) return void (err.textContent = t('Nombre entier de ẑ attendu'));
    save.disabled = true;
    const body = { quota_bytes: g === null ? null : Math.round(g * 2 ** 30), weekly_zen: z };
    if (issuer) body.billet_issuer = issuer.checked;
    try { await K.api(a ? `/api/v1/station/accounts/${a.hex}/hosting` : '/api/v1/station/hosting', { method: 'POST', body }); K.toast(t('Enregistré'), 1); location.reload(); }
    catch (e) { err.textContent = e.message; save.disabled = false; }
  };
  const cancel = h('button', { type: 'button', class: 'ghost' }, t('Annuler'));
  cancel.onclick = close;
  box.replaceChildren(h('h2', {}, a ? t('Hébergement de {0}', who) : t('Palier par défaut des MULTIPASS')),
    h('div', { class: 'ho-form' },
      h('label', { for: 'ho-q' }, a ? t('Quota de {0}, en Go (vide : palier par défaut)', who) : t('Quota par défaut des MULTIPASS, en Go (vide : celui de leur formule)')), quota,
      h('label', { for: 'ho-z' }, a ? t('Prix de {0}, en ẑ par semaine (0 : gratuit ; vide : prix par défaut)', who) : t('Prix par défaut, en ẑ par semaine (vide ou 0 : gratuit)')), zen),
    ...(issuer ? [h('p', {}, h('label', { for: 'ho-b' }, issuer, ' ', t('Peut créer des Ğ1Billets (l’option apparaît dans son interface)')))] : []),
    err, h('p', { class: 'row' }, save, ' ', cancel));
  (quota.focus && quota.focus());
}
