'use strict';
// Mouvements de Ẑen (module chargé à la demande), en deux temps : le récapitulatif (destinataire, ce qui est débité, ce qui reste),
// puis une confirmation explicite en deux clics. Un identifiant de versement est gardé tant que la demande ne change pas :
// rejouer après une panne reprend le versement sans jamais payer deux fois.
//  - Capitaine (`mode` absent) : verser du Ğ1 maître à un MULTIPASS, en crédit d'usage ou en part sociale (satellite, constellation).
//  - Membre (`mode: 'member'`) : envoyer de son propre solde à la trésorerie de la coopérative ou à un proche réciproque.
const c2 = c => (c / 100).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const short = a => a && a.length > 16 ? `${a.slice(0, 6)}…${a.slice(-6)}` : (a || '');
const newId = () => (crypto.randomUUID ? crypto.randomUUID() : `${Date.now()}-${Math.random().toString(16).slice(2)}`).replace(/[^A-Za-z0-9-]/g, '');
const KINDS = [['usage', 'Crédit d\'usage'], ['satellite', 'Part sociale satellite'], ['constellation', 'Part sociale constellation']];

export default async function credit(K, opts = {}) {
  const { h, t } = K, member = opts.mode === 'member', base = member ? '/api/v1/me/zen' : '/api/v1/station/zen-credit';
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-credit.css' }); document.head.append(link);
  let dlg = document.getElementById('credit');
  if (!dlg) { dlg = h('dialog', { id: 'credit', 'aria-labelledby': 'credit-t' }); document.body.append(dlg); }
  const close = h('button', { type: 'button', class: 'x', 'aria-label': t('Fermer'), onclick: () => dlg.close() }, '×');
  const body = h('div', {}, h('p', { class: 'muted' }, t('Chargement…')));
  dlg.replaceChildren(close, h('h2', { id: 'credit-t' }, member ? t('Envoyer des Ẑen') : t('Verser des Ẑen')), body);
  if (!dlg.open) dlg.showModal();
  let list;
  try { list = await K.api(`${base}/recipients`, { method: 'POST', body: {} }); }
  catch (e) { body.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); return; }
  const max = list.max_zen;
  if (!list.recipients.length) { body.replaceChildren(h('p', { class: 'muted' }, t('Aucun destinataire sur cette station.'))); return; }

  let id = newId(), key = '';
  const value = r => member ? r.to : r.hex;
  const label = r => member ? (r.to === 'cash' ? t('La trésorerie de la coopérative (CASH)') : r.label) : (r.me ? `${r.email} (${t('moi')})` : r.email);
  const to = h('select', { id: 'cr-to' }, list.recipients.map(r => h('option', { value: value(r) }, label(r))));
  if (opts.to && list.recipients.some(r => value(r) === opts.to)) to.value = opts.to;
  const kind = member ? null : h('select', { id: 'cr-kind' }, KINDS.map(([k, l]) => h('option', { value: k }, t(l))));
  const amount = h('input', { id: 'cr-amount', type: 'number', min: 1, max, step: 1, inputmode: 'numeric', value: opts.amount || 10, required: '' });
  const err = h('p', { class: 'err', role: 'alert' });
  const step = h('div', {});
  const ask = async ev => {
    ev.preventDefault(); err.textContent = '';
    const req = { to: to.value, amount_zen: Number(amount.value) };
    if (kind) req.kind = kind.value;
    if (!Number.isInteger(req.amount_zen) || req.amount_zen < 1 || req.amount_zen > max) { err.textContent = t('De 1 à {0} Ẑen par versement.', max); return; }
    const k = `${req.to}|${req.kind || ''}|${req.amount_zen}`;
    if (k !== key) { key = k; id = newId(); } // une autre demande est un autre versement
    try {
      const q = await K.api(`${base}/preview`, { method: 'POST', body: req });
      const who = q.email || t('la trésorerie de la coopérative');
      const send = () => K.api(`${base}/send`, { method: 'POST', body: { ...req, credit_id: id, confirm: true } }).then(r => {
        K.toast(t('{0} Ẑen envoyés à {1}', req.amount_zen, who), 1);
        body.replaceChildren(h('p', { class: 'ok', role: 'status' }, '✅ ', t('{0} Ẑen envoyés à {1}', req.amount_zen, who)), h('menu', {}, h('button', { type: 'button', onclick: () => dlg.close() }, t('Fermer'))));
        return r;
      }, e => { err.textContent = e.message; });
      step.replaceChildren(
        h('h3', {}, t('Récapitulatif')),
        h('dl', { class: 'cr-sum' },
          h('dt', {}, t('Destinataire')), h('dd', {}, who, q.address ? [h('br'), h('code', { title: q.address }, short(q.address))] : null),
          h('dt', {}, t('Montant')), h('dd', { class: 'big' }, `${c2(q.amount_zen_cents)} Ẑen`),
          h('dt', {}, member ? t('Débité de votre portefeuille') : t('Débité du Ğ1 maître')), h('dd', {}, `${c2(q.g1_cents)} Ğ1`, q.activation_g1_cents ? ` (${t('dont {0} Ğ1 d\'activation du nouveau compte', c2(q.activation_g1_cents))})` : ''),
          h('dt', {}, member ? t('Il vous restera') : t('Reste sur le maître')), h('dd', {}, `${c2(q.payer_after_g1_cents)} Ğ1 ≈ ${c2(q.payer_after_zen_cents)} Ẑen`),
          q.split?.length ? [h('dt', {}, t('Répartition')), h('dd', {}, q.split.map(x => h('div', {}, `${x.label} : ${c2(x.zen_cents)} Ẑen`)))] : null),
        h('p', { class: 'muted' }, member ? t('Le virement part de votre portefeuille, depuis cette station. Il est définitif.') : t('Le versement passe par un portefeuille de la coopérative (UPLANETNAME ou SOCIETY), comme le faisait la commande historique : la première transaction du MULTIPASS reste le Ğ1 maître.')),
        K.sure(t('Envoyer {0} Ẑen', req.amount_zen), send, t('{0} Ẑen quittent {1} vers {2}', req.amount_zen, member ? t('votre portefeuille') : t('le Ğ1 maître'), who), { class: '' }));
    } catch (e) { step.replaceChildren(); err.textContent = e.message; }
  };
  body.replaceChildren(h('form', { class: 'cr-form', onsubmit: ask },
    h('label', { for: 'cr-to' }, t('À qui ?'), to),
    kind ? h('label', { for: 'cr-kind' }, t('Genre de versement'), kind) : null,
    h('label', { for: 'cr-amount' }, t('Montant (Ẑen, 1 à {0})', max), amount),
    h('button', { type: 'submit' }, t('Voir le récapitulatif'))), err, step);
}
