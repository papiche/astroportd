'use strict';
// Station › Membres : comptes hébergés (cycle de vie, disque, hébergement), jeton de secours, déverrouillage du PASS, éjection.
const LIFE = { active_rental: 'Locataire', cooperative_member: 'Coopérateur', grace_period: 'En grâce', suspended: 'Suspendu' };
const FILTERS = [['all', 'Tous', () => true], ['ok', 'Actifs', a => !['grace_period', 'suspended'].includes(a.status)], ['grace_period', 'En grâce', a => a.status === 'grace_period'], ['suspended', 'Suspendus', a => a.status === 'suspended'], ['locked', 'PASS verrouillé', a => !!a.pass_locked_until]];
const tone = s => (s === 'grace_period' ? 'degraded' : s === 'suspended' ? 'down' : s ? 'ok' : 'disabled');
const day = t => (t ? new Date(t * 1000).toLocaleDateString('fr', { dateStyle: 'medium' }) : '—');

export default async function members(root, K, q) {
  const { h, t, api, toast } = K;
  const price = z => (z > 0 ? `${z} ẑ / ${t('semaine')}` : t('gratuit'));
  let flt = FILTERS.some(f => f[0] === q?.get('f')) ? q.get('f') : 'all';

  const token = async a => {
    const dlg = document.getElementById('share'), box = document.getElementById('share-b');
    document.getElementById('share-t').textContent = t('Jeton de secours');
    box.replaceChildren(h('p', { class: 'muted' }, t('Génération…')));
    if (!dlg.open) dlg.showModal();
    try {
      const r = await api(`/api/v1/station/accounts/${a.hex}/token`, { method: 'POST' });
      const inp = h('input', { value: r.token, readOnly: true, 'aria-label': t('Jeton'), onfocus: () => inp.select() });
      toast(t('Jeton généré'), 1);
      box.replaceChildren(h('p', { class: 'muted' }, t('Compte {0}. À donner de vive voix ou par canal chiffré. Action tracée dans le journal.', r.login)), inp);
    } catch (e) { box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
  };
  const edit = h('section', { class: 'card wide', hidden: true });
  const hosting = async (a, hd) => { edit.hidden = false; (await K.mod('m-hosting.js')).default(edit, K, a, hd); edit.scrollIntoView?.({ behavior: 'smooth' }); };
  const eject = ev => {
    const b = ev.target, a = b._a;
    if (!b.dataset.armed) { b.dataset.armed = 1; b.textContent = t('Confirmer ?'); return setTimeout(() => { delete b.dataset.armed; b.textContent = t('Éjecter'); }, 5000); }
    b.disabled = true;
    api(`/api/v1/station/accounts/${a.hex}/eject`, { method: 'POST' }).then(() => { toast(t('MULTIPASS éjecté'), 1); draw(); }, e => { toast(e.message); b.disabled = false; });
  };
  const actions = a => [
    h('button', { class: 'ghost', onclick: () => token(a) }, t('Jeton')),
    a.unlimited ? null : h('button', { class: 'ghost', onclick: () => hosting(a) }, t('Hébergement')),
    a.pass_locked_until ? h('button', { onclick: async () => { await api(`/api/v1/station/accounts/${a.hex}/unlock-pass`, { method: 'POST' }); toast(t('PASS déverrouillé'), 1); draw(); } }, t('Déverrouiller')) : null,
    a.onboarding?.ejectable ? Object.assign(h('button', { class: 'ghost', title: t('Aucune connexion en 24 h ni Ğ1 reçue : ses clés sont rangées, l\'adresse pourra se réinscrire.'), onclick: eject }, t('Éjecter')), { _a: a }) : null,
  ].filter(Boolean);

  let data = null;
  const paint = () => {
    const { ac, hd } = data, rows = ac.accounts.filter(FILTERS.find(f => f[0] === flt)[2]);
    const chips = FILTERS.map(([k, label, fn]) => {
      const n = ac.accounts.filter(fn).length;
      return h('button', { type: 'button', class: k === flt ? '' : 'ghost', 'aria-pressed': String(k === flt), onclick: () => { flt = k; history.replaceState(null, '', '#/members' + (k === 'all' ? '' : '?f=' + k)); paint(); } }, `${t(label)} ${n}`);
    });
    root.replaceChildren(
      h('h2', {}, t('Membres'), ' ', h('small', { class: 'muted' }, String(ac.accounts.length))),
      h('p', { class: 'muted' }, `${t('Formule par défaut')} : ${hd.default_quota_bytes == null ? t('celle de leur offre') : K.bytes(hd.default_quota_bytes)} · ${price(hd.default_weekly_zen)} `, h('button', { class: 'ghost', onclick: () => hosting(null, hd) }, t('Modifier'))),
      edit,
      h('p', { class: 'filters', role: 'group', 'aria-label': t('Filtrer') }, chips),
      rows.length ? K.table(t('Comptes de la station'), [t('Compte'), t('État'), t('Disque'), t('Hébergement'), t('Fichiers'), t('Actions')],
        rows.map(a => [
          a.email || a.hex.slice(0, 12),
          [K.pill(tone(a.status)), ' ', t(LIFE[a.status] || a.status || 'inconnu'), a.grace_ends_at ? ` (${t('jusqu\'au')} ${day(a.grace_ends_at)})` : '', a.onboarding ? ` · ${a.onboarding.proved_at ? t('clés reçues') : t('jamais connecté')}${a.onboarding.primo === 'paid' ? ' · 1 Ğ1' : ''}` : ''],
          [h('meter', { min: 0, max: 1, low: .7, high: .9, optimum: 0, value: a.quota_bytes ? Math.min(a.used_bytes / a.quota_bytes, 1) : 0, 'aria-label': t('Disque') }), ` ${K.bytes(a.used_bytes)} / ${a.unlimited ? t('illimité') : K.bytes(a.quota_bytes)}`],
          a.unlimited ? '—' : `${price(a.weekly_zen)}${a.billet_issuer ? ' 🎫' : ''}`,
          String(a.files),
          h('span', { class: 'acts' }, actions(a)),
        ])) : h('p', { class: 'muted' }, t('Aucun compte.')));
  };
  const draw = async () => {
    const [ac, hd] = await Promise.all([api('/api/v1/station/accounts'), api('/api/v1/station/hosting')]);
    data = { ac, hd }; paint();
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
