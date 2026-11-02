'use strict';
// Publier (ou retirer) des fichiers : un déplacement vers le dossier /Public de l'uCloud, lisible ensuite par tout le monde sans mot de passe.
// Le consentement est explicite et dans la page : on dit ce qui va devenir public avant de le faire. `paths` : la sélection de l'explorateur.
export default function publish(box, paths, K, current, done) {
  const { h, t } = K, out = current === '/Public' || current.startsWith('/Public/'), n = paths.length;
  const err = h('p', { class: 'err', role: 'alert' });
  const go = h('button', { type: 'button', class: 'primary' }, out ? t('Retirer de Public') : t('Publier'));
  const cancel = h('button', { type: 'button', class: 'ghost' }, t('Annuler'));
  cancel.onclick = () => { box.hidden = true; box.replaceChildren(); };
  go.onclick = async () => {
    go.disabled = true; err.textContent = '';
    try {
      const r = await K.api('/api/v1/public/move', { method: 'POST', body: { paths, publish: !out } });
      const bad = r.refused.map(x => `${x.path} : ${t(x.why)}`);
      K.toast(t(out ? '{0} élément(s) retiré(s) de Public' : '{0} élément(s) publié(s) dans Public', r.moved.length), r.moved.length > 0);
      if (bad.length) { err.textContent = bad.join(' ; '); go.hidden = true; cancel.textContent = t('Fermer'); } else done();
      if (r.moved.length && !bad.length) box.hidden = true;
    } catch (e) { err.textContent = e.message; go.disabled = false; }
  };
  box.replaceChildren(h('h3', {}, out ? t('Retirer {0} élément(s) du site public ?', n) : t('Publier {0} élément(s) ?', n)),
    h('p', { class: out ? 'muted' : 'degraded', role: 'status' }, out ? t('Ils ne seront plus lisibles par tout le monde : ils sont déplacés dans « Retirés de Public », visibles de vous seul.')
      : t('Ils seront déplacés dans le dossier Public : lisibles par tout le monde, sans mot de passe, sur votre site. Vérifiez qu’il n’y a rien de privé (visages, adresses, documents).')),
    h('ul', { class: 'muted' }, paths.slice(0, 8).map(p => h('li', {}, p)), n > 8 ? h('li', {}, t('… et {0} autre(s)', n - 8)) : null),
    err, h('p', { class: 'row' }, go, ' ', cancel));
}
