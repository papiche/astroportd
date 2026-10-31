'use strict';
// Ranger : réorganiser le uCloud par type (Documents, Images, Musique, Vidéos, Archives), comme sur un ordinateur de bureau.
// Seuls les chemins changent : aucun fichier n'est recopié ni re-chiffré. Aperçu d'abord, annulation possible.

export default async function organize(root, K, tabs) {
  const { h, t, bytes, when } = K;
  const call = (path, body) => K.api('/api/v1/organize' + path, body === undefined ? undefined : { method: 'POST', body });
  const out = h('div'), runsBox = h('div'), jobBox = h('div');
  let timer = false;

  const scope = h('input', { id: 'og-scope', value: '/', 'aria-describedby': 'og-scope-h' });
  const date = h('select', { id: 'og-date' }, h('option', { value: 'none' }, t('Aucun')), h('option', { value: 'year' }, t('Par année')), h('option', { value: 'month' }, t('Par année et mois')));
  const KINDS = [['documents', 'Documents'], ['images', 'Images'], ['music', 'Musique'], ['videos', 'Vidéos'], ['archives', 'Archives']];
  const boxes = KINDS.map(([id, name]) => h('label', { class: 'inline' }, h('input', { type: 'checkbox', name: id, checked: true }), ' ', t(name)));
  const options = () => ({ scope: scope.value.trim() || '/', by_date: date.value, kinds: boxes.map(b => b.querySelector('input')).filter(i => i.checked).map(i => i.name) });

  const preview = async ev => {
    ev.target.disabled = true;
    try {
      const p = await call('/preview', options());
      const rows = p.by_kind.filter(k => k.files).map(k => h('tr', {}, h('td', {}, h('strong', {}, k.folder)), h('td', {}, String(k.files)), h('td', {}, bytes(k.bytes)),
        h('td', { class: 'muted' }, k.sample.map(s => h('div', {}, h('code', {}, s.from), ' → ', h('code', {}, s.to))))));
      out.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, 'Aperçu'),
        p.total ? [h('p', {}, t('{0} fichier(s) seraient rangés.', p.total), p.capped ? ' ' + t('(un premier passage ; relancez ensuite pour la suite)') : ''),
          h('table', {}, h('thead', {}, h('tr', {}, h('th', {}, 'Dossier'), h('th', {}, 'Fichiers'), h('th', {}, 'Taille'), h('th', {}, 'Exemples'))), h('tbody', {}, rows)),
          h('p', { class: 'muted' }, t('Laissés en place : {0} déjà rangé(s), {1} protégé(s) (coffre, contacts, agenda, partages, dossiers cachés), {2} de type inconnu.', p.placed, p.protected, p.unknown)),
          h('p', {}, h('button', { onclick: go }, 'Ranger maintenant'))]
          : h('p', { class: 'muted' }, t('Rien à ranger : tout est déjà à sa place ({0} fichier(s) déjà rangé(s), {1} de type inconnu).', p.placed, p.unknown))));
    } catch (e) { K.toast(e.message); } finally { ev.target.disabled = false; }
  };
  const go = async ev => {
    ev.target.disabled = true;
    try { await call('/run', options()); out.replaceChildren(); K.toast(t('Rangement lancé'), 1); await draw(); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  };

  const form = h('section', { class: 'card wide' }, h('h2', {}, 'Ranger mon uCloud par type'),
    h('p', { class: 'muted' }, 'Les fichiers sont rangés dans Documents, Images, Musique, Vidéos et Archives. Seuls les chemins changent : rien n\'est recopié ni re-chiffré, vos visages et vos partages suivent, et vous pouvez annuler. Le coffre, les contacts, l\'agenda, les partages reçus et les dossiers cachés ne sont jamais touchés ; un fichier de type inconnu reste où il est ; un nom déjà pris reçoit un numéro.'),
    h('div', { class: 'grid' },
      h('div', {}, h('label', { for: 'og-scope' }, 'Dossier à ranger'), scope, h('p', { id: 'og-scope-h', class: 'muted' }, '/ : tout le uCloud, ou par exemple /Imports')),
      h('div', {}, h('label', { for: 'og-date' }, 'Sous-dossiers par date'), date)),
    h('p', { class: 'inline' }, ...boxes),
    h('p', {}, h('button', { class: 'ghost', onclick: preview }, 'Voir ce qui serait rangé')));

  const act = (path, body, msg) => async ev => { ev.target.disabled = true; try { const r = await call(path, body); K.toast(t(msg, r.restored ?? ''), 1); } catch (e) { K.toast(e.message); } await draw(); };

  async function draw() {
    const s = await call('');
    const j = s.job;
    jobBox.replaceChildren(...(j ? [h('section', { class: 'card wide' }, h('h2', {}, 'Rangement'),
      h('p', {}, h('span', { class: 'pill ' + (j.state === 'done' ? 'ok' : j.state === 'failed' ? 'down' : 'degraded') }, t({ running: 'En cours', done: 'Terminé', failed: 'Échec' }[j.state]))),
      j.state === 'running' ? [h('progress', { max: Math.max(j.progress.total, 1), value: j.progress.done }), h('p', { class: 'muted' }, t('{0} / {1} fichiers', j.progress.done, j.progress.total)), h('p', {}, h('button', { class: 'ghost', onclick: act('/stop', {}, 'Arrêt demandé') }, 'Arrêter'))]
        : j.report ? [h('p', { class: 'muted' }, t('{0} fichier(s) rangé(s) · {1} dossier(s) vide(s) supprimé(s) · {2} échec(s)', j.report.moved, j.report.pruned, j.report.errors), j.report.stopped ? ' · ' + t('arrêté avant la fin') : ''),
          ...(j.report.first_errors || []).map(e => h('p', { class: 'err' }, `${e.path} : ${e.why}`))]
        : h('p', { class: 'err', role: 'alert' }, j.error))] : []));
    runsBox.replaceChildren(...(s.runs.length ? [h('section', { class: 'card wide' }, h('h2', {}, 'Derniers rangements'),
      ...s.runs.map(r => h('div', { class: 'row' }, h('div', { class: 't' }, h('b', {}, when(r.at)), h('span', { class: 'muted' }, ' · ' + t('{0} fichier(s) rangé(s)', r.moved), r.undone ? ' · ' + t('annulé') : '')),
        r.undone || r.moved === 0 || j?.state === 'running' ? null : h('button', { class: 'ghost', onclick: ev => confirm(t('Remettre ces fichiers à leur place d\'avant ?')) && act('/undo', { run: r.run_id }, '{0} fichier(s) remis en place')(ev) }, 'Annuler ce rangement'))))] : []));
    if (j?.state === 'running' && !timer) { timer = true; K.every(1500, () => draw().catch(() => {})); }
  }
  root.replaceChildren(tabs, form, out, jobBox, runsBox);
  await draw();
}
