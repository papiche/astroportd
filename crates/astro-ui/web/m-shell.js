'use strict';
// Coque de navigation, pilotée par la table des routes : onglets de l'espace courant, sous-onglets, retour, interrupteur
// Mon espace ⇄ Ma station (Capitaine), pastille « Aide » et bouton de l'assistant.
import { TABS, segments, parentOf, titleOf } from '/ui/m-routes.js';

export default function shell(K, me) {
  const { h, t } = K;
  const nav = document.querySelector('header nav'), tools = document.querySelector('.tools');
  let space = null, bar = null, fab = null, sw = null;

  const tabs = sp => {
    nav.replaceChildren(...TABS[sp].map(([id, icon, label, to]) => h('a', { href: '#' + to, 'data-r': id }, h('span', { class: 'ic', 'aria-hidden': 'true' }, icon), h('span', { class: 'lb' }, t(label)))));
    nav.setAttribute('aria-label', sp === 'station' ? t('Ma station') : t('Mon espace'));
    document.documentElement.dataset.space = sp;
  };
  const switcher = sp => {
    if (!me.captain) return;
    if (!sw) { sw = h('a', { class: 'space-sw' }); tools.prepend(sw); }
    sw.href = sp === 'station' ? '#/' : '#/cockpit';
    sw.textContent = sp === 'station' ? '🏠 ' + t('Mon espace') : '🛰️ ' + t('Ma station');
    sw.title = sp === 'station' ? t('Retour à mon espace') : t('Gérer la station');
    if (help) sw.append(help);
  };
  // Demandes d'aide d'un Capitaine ami : pastille sur l'interrupteur.
  let help = null;
  const poll = () => K.api('/api/v1/station/admin/assist').then(v => {
    const n = v.incoming.filter(r => r.status === 'pending').length;
    if (!n) { help?.remove(); help = null; return; }
    if (!help) { help = h('span', { class: 'ch-badge' }); sw?.append(help); }
    help.textContent = n; help.title = t('{0} demande(s) d\'aide en attente', n);
  }, () => {});
  if (me.captain) { poll(); setInterval(poll, 30000); }

  // Barre sous l'en-tête : retour (écran enfant) ou sous-onglets (écrans frères).
  const strip = res => {
    const back = parentOf(res), segs = segments(res);
    bar?.remove(); bar = null;
    if (back) bar = h('nav', { class: 'segs back', 'aria-label': t('Retour') }, h('a', { href: '#' + back.path }, '← ' + t(back.title)), h('strong', {}, t(titleOf(res))));
    else if (segs.length) bar = h('nav', { class: 'segs', 'aria-label': t('Rubrique') }, segs.map(s => h('a', { href: '#' + s.path, 'aria-current': s.path === res.route.path ? 'page' : null }, t(s.label))));
    if (bar) document.getElementById('main').before(bar);
  };
  const assistant = res => {
    fab?.remove(); fab = null;
    if (me.ai && res.route.path !== '/bro' && res.space === 'me') document.body.append(fab = h('a', { class: 'fab', href: '#/bro', 'aria-label': t('Assistant'), title: t('Assistant') }, '✨'));
  };

  return {
    update(res) {
      if (space !== res.space) { space = res.space; tabs(space); switcher(space); }
      nav.querySelectorAll('a').forEach(a => (a.dataset.r === res.tab ? a.setAttribute('aria-current', 'page') : a.removeAttribute('aria-current')));
      strip(res); assistant(res);
      document.title = `${t(titleOf(res))} · Astroport`;
    },
  };
}
