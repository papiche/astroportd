'use strict';
import { css } from '/ui/m-framed.js';
// Plusieurs fiches portent le nom tapé : on fait choisir, on ne devine jamais (deux « Marie » sont deux personnes). Chaque fiche
// est montrée avec ce qui la distingue (société, email, téléphone, visages déjà rangés). Rend la cible du rangement :
// { name, contact } (une fiche choisie), { name, new: true } (une nouvelle personne de ce nom), ou null (annulé).

export default function who(K, name, same) {
  const { h, t } = K;
  css(K);
  return new Promise(done => {
    let answer = null;
    const pick = v => { answer = v; d.close(); };
    const d = h('dialog', { class: 'who', 'aria-labelledby': 'who-t' },
      h('h2', { id: 'who-t' }, t('Quelle « {0} » ?', name)),
      h('p', { class: 'muted' }, t('{0} fiches portent ce nom. Choisissez la bonne personne : ses visages la rejoindront, pas les autres.', same.length)),
      h('div', { class: 'who-list' }, same.map(c => h('button', { type: 'button', class: 'ghost', onclick: () => pick({ name: c.name, contact: c.uid }) },
        h('strong', {}, c.name), h('small', { class: 'muted' }, [c.org, c.emails?.[0], c.phones?.[0]?.value, c.faces ? t('{0} visage(s)', c.faces) : t('aucun visage encore')].filter(Boolean).join(' · '))))),
      h('menu', {}, h('button', { type: 'button', class: 'ghost', onclick: () => pick(null) }, t('Annuler')), h('button', { type: 'button', onclick: () => pick({ name, new: true }) }, t('Une nouvelle personne'))));
    d.addEventListener('close', () => { d.remove(); done(answer); });
    document.body.append(d); d.showModal();
  });
}
