'use strict';
// « Mes Ğ1Billets » : créer une planche de billets papier, les financer un par un, suivre leur état. Réservé aux émetteurs que le
// Capitaine a choisis (l'écran n'apparaît que pour eux). Les parts P1 et P2 ne sont rendues qu'une fois, à la création : on imprime tout de suite.
const DAYS = [[0, 'Sans limite'], [7, '7 jours'], [30, '30 jours'], [90, '3 mois'], [365, '1 an']];
const STATUS = { ready: 'valide', redeemed: 'encaissé', expired: 'expiré', returned: 'renvoyé à l’émetteur', pending: 'encaissement en cours', empty: 'pas encore financé', foreign: 'étranger' };
const short = a => a && a.length > 16 ? `${a.slice(0, 6)}…${a.slice(-6)}` : (a || '');
const fmt = c => (c / 100).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });

// Un SVG de QR rendu par la station, inséré sans `innerHTML` (CSP) : on le relit comme un document XML.
function svg(text, cls) {
  if (!text) return document.createTextNode('');
  const doc = new DOMParser().parseFromString(text, 'image/svg+xml'), n = document.importNode(doc.documentElement, true);
  n.setAttribute('class', cls); n.removeAttribute('width'); n.removeAttribute('height');
  return n;
}

export default async function billets(box, K) {
  const { h, t } = K;
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-billets.css' }); document.head.append(link); K.cleanup(() => link.remove());
  const me = await K.api('/api/v1/me').catch(() => ({}));
  if (!me.billet_issuer) { box.replaceChildren(h('h2', {}, t('Mes Ğ1Billets')), h('p', { class: 'muted' }, t('Le Capitaine ne vous a pas autorisé à créer des Ğ1Billets.'))); return; }

  const sheet = h('div', { class: 'bs-sheet-box' }), listBox = h('div', {});
  const num = (id, v, min, max) => h('input', { id, type: 'number', min, max, value: v, inputmode: 'numeric' });
  const count = h('select', { id: 'bs-count' }, [6, 5, 4, 3, 2, 1].map(n => h('option', { value: n }, String(n))));
  const amount = num('bs-amount', 10, 0, 10000);
  const days = h('select', { id: 'bs-days' }, DAYS.map(([d, l]) => h('option', { value: d }, t(l))));
  const back = h('input', { id: 'bs-return', type: 'checkbox', disabled: '' });
  const total = h('p', { class: 'muted', role: 'status' });
  const err = h('p', { class: 'err', role: 'alert' });
  const sync = () => { const v = Number(days.value) > 0; back.disabled = !v; if (!v) back.checked = false; total.textContent = t('Total engagé : {0} ẑ ({1} billet(s) de {2} ẑ). Un dépôt d’activation de 1 Ğ1 par billet est avancé par la coopérative et lui revient à l’encaissement.', Number(count.value) * (Number(amount.value) || 0), count.value, Number(amount.value) || 0); };
  [count, amount, days].forEach(e => e.addEventListener('input', sync));
  const go = h('button', { type: 'button', class: 'primary' }, t('Créer la planche'));
  go.onclick = async () => {
    err.textContent = ''; go.disabled = true;
    try {
      const r = await K.api('/api/v1/billet/issue', { method: 'POST', body: { count: Number(count.value), amount_zen: Number(amount.value) || 0, valid_days: Number(days.value) || null, return_to_issuer: back.checked, origin: location.origin } });
      show(r); loadList();
    } catch (e) { err.textContent = t(e.message); } finally { go.disabled = false; }
  };
  const form = h('section', { class: 'card wide' }, h('h2', {}, t('Nouvelle planche')),
    h('p', { class: 'muted' }, t('Chaque billet est une clé Ğ1 jetable. Le porteur l’encaisse avec la caméra sur la page /scan, sans compte.')),
    h('div', { class: 'bs-form' },
      h('label', { for: 'bs-count' }, t('Nombre de billets')), count,
      h('label', { for: 'bs-amount' }, t('Valeur de chaque billet (ẑ)')), amount,
      h('label', { for: 'bs-days' }, t('Durée de validité')), days),
    h('p', {}, h('label', {}, back, ' ', t('Si le billet n’est pas encaissé à la date de fin, sa valeur revient sur mon portefeuille'))),
    h('p', { class: 'muted' }, t('Avec le retour automatique, la station garde deux des trois parts (chiffrées, pour elle seule) : elle peut renvoyer la valeur à la date de fin, mais elle garde le billet en dépôt jusque-là.')),
    total, err, h('p', {}, go));

  function show(r) {
    const rows = r.billets.map((b, i) => {
      const state = h('span', { class: 'muted', role: 'status' }), btn = h('button', { type: 'button' }, t('Financer'));
      const fund = async () => {
        btn.disabled = true; state.textContent = t('Financement…');
        try { const f = await K.api('/api/v1/billet/fund', { method: 'POST', body: { g1pub: b.g1pub } }); state.textContent = f.already ? t('✅ Déjà financé') : t('✅ Financé'); }
        catch (e) { state.textContent = '❌ ' + t(e.message); btn.disabled = false; }
      };
      btn.onclick = fund; b.fund = fund; b.state = state;
      // Un billet pas encore financé se retire de Nostr : planche pliée ou abandonnée, il n'encombre pas la liste.
      const del = K.sure(t('Supprimer'), async ev => {
        ev.target.disabled = true;
        try { await K.api('/api/v1/billet/delete', { method: 'POST', body: { g1pub: b.g1pub } }); b.deleted = true; btn.disabled = true; del.hidden = true; state.textContent = t('🗑 Supprimé de Nostr'); loadList(); }
        catch (e) { ev.target.disabled = false; state.textContent = '❌ ' + t(e.message); }
      }, t('Supprimer ce billet de Nostr ? La planche imprimée ne vaudra plus rien.'));
      b.del = del;
      return h('div', { class: 'bs-fund' }, h('strong', {}, `#${i + 1}`), h('code', { title: b.g1pub }, short(b.g1pub)), btn, ' ', del, state);
    });
    const all = h('button', { type: 'button', class: 'primary' }, t('Tout financer'));
    all.onclick = async () => { all.disabled = true; for (const b of r.billets) if (!b.deleted) await b.fund(); all.disabled = false; };
    const print = h('button', { type: 'button' }, '🖨️ ' + t('Imprimer la planche'));
    print.onclick = () => window.print();
    const host = location.host;
    const st = r.style || {};
    const cards = r.billets.map(b => h('article', { class: 'bs-bill' + (st.background ? ' bs-img' : ''), ...{} },
      h('div', { class: 'bs-strip' }, svg(b.qr_p2, 'bs-qr'), h('span', {}, t('SOUS LE PLI'))),
      h('div', { class: 'bs-main' },
        h('div', { class: 'bs-head' }, st.icon ? h('img', { class: 'bs-icon', src: st.icon, alt: '' }) : null, h('strong', {}, 'Ğ1Billet'), h('span', {}, host)),
        h('div', { class: 'bs-amt' }, `${b.amount_zen} Ẑen`),
        h('div', { class: 'bs-qrs' }, h('figure', {}, svg(b.qr_p1, 'bs-qr big'), h('figcaption', {}, t('À scanner'))), h('figure', {}, svg(b.qr_verify, 'bs-qr'), h('figcaption', {}, t('Vérifier'))) ),
        h('div', { class: 'bs-foot' }, short(b.g1pub), b.expires_at ? ` · ${t('valable jusqu’au {0}', new Date(b.expires_at * 1000).toLocaleDateString())}` : '', b.return_to_issuer ? ` · ${t('retour à l’émetteur')}` : ''))));
    sheet.replaceChildren(
      h('section', { class: 'card wide bs-warn' }, h('h2', {}, t('Planche créée')),
        h('p', { class: 'degraded', role: 'status' }, t('Les QR ne s’affichent qu’une seule fois : imprimez maintenant, puis financez chaque billet. Un billet non financé ne vaut rien.')),
        h('p', {}, print, ' ', all), ...rows),
      h('div', { class: 'bs-sheet', 'aria-label': t('Planche de billets à imprimer') }, ...cards));
    // Le bandeau du profil sert de fond (voilé de blanc pour garder les QR lisibles) ; posé par le DOM, la CSP interdit un style en ligne.
    if (st.background) sheet.querySelectorAll('.bs-bill').forEach(c => { c.style.backgroundImage = `linear-gradient(rgba(255,255,255,.80), rgba(255,255,255,.80)), url("${st.background}")`; });
    sheet.scrollIntoView?.({ behavior: 'smooth' });
  }

  async function loadList() {
    try {
      const r = await K.api('/api/v1/billet/issued');
      listBox.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, t('Mes billets créés')),
        r.billets.length ? h('div', { class: 'tw' }, h('table', { class: 'bs-list' }, h('thead', {}, h('tr', {}, ['Billet', 'Valeur', 'État', 'Fin', ''].map(x => h('th', {}, x ? t(x) : '')))),
          h('tbody', {}, r.billets.map(b => h('tr', {}, h('td', {}, h('code', { title: b.g1pub }, short(b.g1pub))), h('td', {}, `${b.amount_zen ?? '—'} Ẑen${b.return_to_issuer ? ' ↩' : ''}`), h('td', {}, t(STATUS[b.status] || b.status)), h('td', {}, b.expires_at ? new Date(b.expires_at * 1000).toLocaleDateString() : '—'), h('td', {}, b.status === 'empty' ? K.sure(t('Supprimer'), async ev => { ev.target.disabled = true; try { await K.api('/api/v1/billet/delete', { method: 'POST', body: { g1pub: b.g1pub } }); K.toast(t('Billet supprimé de Nostr')); loadList(); } catch (e) { ev.target.disabled = false; K.toast(t(e.message)); } }, t('Supprimer ce billet de Nostr ? La planche imprimée ne vaudra plus rien.')) : null)))))) : h('p', { class: 'muted' }, t('Aucun billet pour l’instant.')),
        h('p', { class: 'muted' }, t('↩ : la valeur revient sur votre portefeuille à la date de fin si le billet n’est pas encaissé.'))));
    } catch (e) { listBox.replaceChildren(h('p', { class: 'err', role: 'alert' }, t(e.message))); }
  }
  sync();
  box.replaceChildren(h('h2', {}, t('Mes Ğ1Billets')), form, sheet, listBox);
  await loadList();
}
