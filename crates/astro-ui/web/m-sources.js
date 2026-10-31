'use strict';
// Sources : remplir son uCloud depuis un dossier d'un AUTRE serveur WebDAV (Nextcloud, ownCloud, une autre station). La reprise
// se lance d'ici, tourne en arrière-plan et se rejoue sans rien écraser ni dupliquer. Les fichiers arrivent tels quels sous le
// dossier de dépôt, sans tri : ranger le uCloud est un autre geste.

const STATE = { running: 'En cours', done: 'Terminée', failed: 'Échec' };

export default async function sources(root, K, tabs) {
  const { h, t, bytes, when } = K;
  const call = (path, body) => K.api('/api/v1/pull' + path, body === undefined ? undefined : { method: 'POST', body });
  const list = h('div', { class: 'grid' });
  let timer = false;

  const form = h('form', { class: 'card wide', onsubmit: async ev => {
    ev.preventDefault();
    const f = Object.fromEntries(new FormData(ev.target));
    const btn = ev.target.querySelector('button'); btn.disabled = true;
    try { await call('', { url: f.url, username: f.username, password: f.password, label: f.label, remote_path: f.remote_path, dest: f.dest }); ev.target.reset(); K.toast(t('Source ajoutée'), 1); await draw(); }
    catch (e) { K.toast(e.message); } finally { btn.disabled = false; }
  } },
    h('h2', {}, 'Ajouter une source'),
    h('p', { class: 'muted' }, 'Adresse WebDAV du serveur, par exemple https://nextcloud.example/remote.php/dav/files/votre-identifiant. Créez de préférence un mot de passe d\'application : il est rangé chiffré pour vous seul et vous pouvez le révoquer côté serveur.'),
    h('label', { for: 'src-url' }, 'Adresse WebDAV'), h('input', { id: 'src-url', name: 'url', type: 'url', required: true, placeholder: 'https://nextcloud.example/remote.php/dav/files/identifiant' }),
    h('div', { class: 'grid' },
      h('div', {}, h('label', { for: 'src-user' }, 'Identifiant'), h('input', { id: 'src-user', name: 'username', required: true, autocomplete: 'off' })),
      h('div', {}, h('label', { for: 'src-pass' }, 'Mot de passe d\'application'), h('input', { id: 'src-pass', name: 'password', type: 'password', required: true, autocomplete: 'new-password' }))),
    h('div', { class: 'grid' },
      h('div', {}, h('label', { for: 'src-rp' }, 'Dossier distant (facultatif)'), h('input', { id: 'src-rp', name: 'remote_path', placeholder: 'Photos' })),
      h('div', {}, h('label', { for: 'src-label' }, 'Libellé (facultatif)'), h('input', { id: 'src-label', name: 'label', placeholder: 'Mon Nextcloud' }))),
    h('label', { for: 'src-dest' }, 'Dossier de dépôt dans le uCloud (facultatif)'), h('input', { id: 'src-dest', name: 'dest', placeholder: '/Imports/Mon Nextcloud' }),
    h('p', { class: 'muted' }, 'Les fichiers arrivent tels qu\'ils sont sur le serveur, sans tri par dates. Seules les adresses publiques sont jointes.'),
    h('p', {}, h('button', {}, 'Vérifier et ajouter')));

  const line = r => t('{0} vus · {1} repris · {2} déjà repris · {3} échec(s)', r.remote, r.imported, r.skipped, r.errors);
  const card = s => {
    const j = s.job, running = j?.state === 'running';
    const act = (path, body, msg) => async ev => { ev.target.disabled = true; try { await call(path, body); K.toast(t(msg), 1); } catch (e) { K.toast(e.message); } await draw(); };
    const status = [];
    if (j) {
      status.push(h('p', {}, h('span', { class: 'pill ' + (j.state === 'done' ? 'ok' : j.state === 'failed' ? 'down' : 'degraded') }, t(STATE[j.state])), j.dry_run ? ' ' + t('(aperçu : rien n\'est écrit)') : ''));
      if (running) status.push(h('progress', { max: Math.max(j.progress.total, 1), value: j.progress.done }), h('p', { class: 'muted' }, t('{0} / {1} fichiers · {2} repris · {3} déjà repris · {4} échec(s)', j.progress.done, j.progress.total, j.progress.imported, j.progress.skipped, j.progress.errors)));
      else if (j.report) status.push(h('p', { class: 'muted' }, line(j.report), j.report.imported ? ` · ${bytes(j.report.bytes)}` : '', j.report.stopped ? ' · ' + t('arrêtée avant la fin') : '', j.report.capped ? ' · ' + t('parcours borné') : ''),
        ...(j.report.first_errors || []).map(e => h('p', { class: 'err' }, `${e.path} : ${e.why}`)));
      else if (j.error) status.push(h('p', { class: 'err', role: 'alert' }, j.error));
    } else if (s.last) status.push(h('p', { class: 'muted' }, t('Dernière reprise : {0}', when(s.last.at)), ' · ', line(s.last.report)));
    else status.push(h('p', { class: 'muted' }, t('Jamais lancée.')));
    return h('section', { class: 'card' }, h('h2', {}, s.label), h('p', { class: 'muted' }, h('code', {}, s.url)),
      h('p', {}, t('Dossier distant : {0}', s.remote_path || t('tout')), ' → ', h('code', {}, s.dest_prefix)), ...status,
      h('p', { class: 'inline' },
        running ? h('button', { class: 'ghost', onclick: act('/stop', {}, 'Arrêt demandé') }, 'Arrêter')
          : [h('button', { onclick: act('/run', { id: s.id }, 'Reprise lancée') }, 'Reprendre'), h('button', { class: 'ghost', onclick: act('/run', { id: s.id, dry_run: true }, 'Aperçu lancé') }, 'Aperçu'),
            h('button', { class: 'ghost', onclick: ev => confirm(t('Retirer cette source ? Ce qui a déjà été repris reste dans votre uCloud.')) && act('/remove', { id: s.id }, 'Source retirée')(ev) }, 'Retirer')]));
  };

  async function draw() {
    const r = await call('');
    list.replaceChildren(...(r.sources.length ? r.sources.map(card) : [h('section', { class: 'card wide' }, h('p', { class: 'muted' }, 'Aucune source : ajoutez-en une ci-dessous pour remplir votre uCloud depuis un autre serveur.'))]));
    // Tant qu'une reprise tourne, l'écran se met à jour seul.
    if (r.sources.some(s => s.job?.state === 'running') && !timer) { timer = true; K.every(2000, () => draw().catch(() => {})); }
  }
  root.replaceChildren(tabs, list, form);
  await draw();
}
