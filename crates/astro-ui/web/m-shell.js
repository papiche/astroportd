'use strict';
// Coque de navigation, pilotée par la table des routes : onglets de l'espace courant, sous-onglets, retour, interrupteur
// Mon espace ⇄ Ma station (Capitaine), pastille « Aide », solde en Ẑen, pli des sections et bouton de l'assistant.
import { TABS, segments, parentOf, titleOf } from '/ui/m-routes.js';
import { folds } from '/ui/m-fold.js';

export default function shell(K, me) {
  const { h, t } = K;
  const nav = document.querySelector('header nav'), tools = document.querySelector('.tools');
  let space = null, bar = null, fab = null, sw = null;
  // Toute carte titrée se plie d'un geste sur son titre ; l'état est retenu par écran et par titre (m-fold.js).
  folds(document.getElementById('main'));

  const tabs = sp => {
    nav.replaceChildren(...TABS[sp].map(([id, icon, label, to]) => h('a', { href: '#' + to, 'data-r': id }, h('span', { class: 'ic', 'aria-hidden': 'true' }, icon), h('span', { class: 'lb' }, t(label)))));
    nav.setAttribute('aria-label', sp === 'station' ? t('Ma station') : t('Mon espace'));
    document.documentElement.dataset.space = sp;
  };
  const switcher = sp => {
    if (!me.captain) return;
    if (!sw) { sw = h('a', { class: 'space-sw' }); tools.prepend(sw); }
    sw.href = sp === 'station' ? '#/' : help ? '#/services' : '#/cockpit';
    sw.textContent = sp === 'station' ? '🏠 ' + t('Mon espace') : '🛰️ ' + t('Ma station');
    sw.title = sp === 'station' ? t('Retour à mon espace') : t('Gérer la station');
    if (help) sw.append(help);
  };
  // Demandes d'aide d'un Capitaine ami : pastille sur l'interrupteur.
  let help = null;
  const poll = () => K.api('/api/v1/station/admin/assist').then(v => {
    const n = v.incoming.filter(r => r.status === 'pending').length;
    if (!n) { help?.remove(); help = null; if (sw && space !== 'station') sw.href = '#/cockpit'; return; }
    if (!help) { help = h('span', { class: 'ch-badge' }); sw?.append(help); }
    // Une demande d'aide attend : l'interrupteur mène droit à l'écran où l'on y répond.
    if (sw && space !== 'station') sw.href = '#/services';
    help.textContent = n; help.title = t('{0} demande(s) d\'aide en attente', n);
  }, () => {});
  if (me.captain) { poll(); setInterval(poll, 30000); }

  // Solde en Ẑen du membre, discret dans l'en-tête ; un clic ouvre Mes Ẑen sur l'historique des virements. Centimes entiers, mis en forme sans flottant.
  const zc = c => `${c < 0 ? '-' : ''}${Math.trunc(Math.abs(c) / 100)}${c % 100 ? ',' + String(Math.abs(c) % 100).padStart(2, '0') : ''}`;
  let purse = null;
  const balance = () => K.api('/api/v1/dashboard').then(d => {
    const c = d.balance?.zen_cents;
    if (c == null) { purse?.remove(); purse = null; return; }
    if (!purse) { purse = h('a', { class: 'zen-in', href: '#/money?h=1' }); tools.prepend(purse); }
    purse.textContent = K.cur(`${zc(c)} Ẑ`);
    purse.title = t('Voir l’historique des virements') + (d.balance.stale ? ' · ' + t('Dernier solde connu : le réseau est lent.') : '');
    purse.setAttribute('aria-label', K.cur(`${t('Mes Ẑen')} : ${zc(c)} Ẑen`));
  }, () => {});
  balance(); setInterval(balance, 120000);

  // Une seule case de recherche pour tout le uCloud, sous les sous-onglets de Fichiers (Dossiers, Photos, Médiathèque) ; son
  // texte grisé ne promet que ce que la station sait chercher (le serveur le dit).
  let hintP = null;
  const finder = res => {
    if (res.tab !== 'files') return null;
    const onFind = res.route.path === '/find', qs = new URLSearchParams(onFind ? location.hash.split('?')[1] || '' : '');
    const q = h('input', { type: 'search', value: qs.get('q') || '', placeholder: t('Chercher dans mes fichiers…'), 'aria-label': t('Chercher dans mes fichiers'), maxlength: 256, autocomplete: 'off', enterkeyhint: 'search' });
    hintP ??= K.api('/api/v1/me/reading').then(r => {
      const text = r.state === 'local' || r.state === 'friend', image = r.state !== 'blocked' && (r.local || []).includes('vision');
      return text && image ? t('Nom, texte d’un document, ce que montre la photo, Léa, Lyon, 2024…') : text ? t('Nom, texte d’un document, Léa, Lyon, 2024…') : image ? t('Nom, ce que montre la photo, Léa, Lyon, 2024…') : t('Nom, description, Léa, Lyon, 2024…');
    }, () => null);
    hintP.then(p => { if (p) q.placeholder = p; });
    return h('form', { class: 'find', role: 'search', onsubmit: ev => { ev.preventDefault(); const v = q.value.trim(); if (!v) return; qs.set('q', v); location.hash = '#/find?' + qs; } },
      q, h('button', { 'aria-label': t('Chercher'), title: t('Chercher') }, '🔍'));
  };
  // Barre sous l'en-tête : retour (écran enfant) ou sous-onglets (écrans frères), et la recherche dans Fichiers.
  const strip = res => {
    const back = parentOf(res), segs = segments(res, me);
    bar?.remove(); bar = null;
    let nav = null;
    if (back) nav = h('nav', { class: 'segs back', 'aria-label': t('Retour') }, h('a', { href: '#' + back.path }, '← ' + t(back.title)), h('strong', {}, t(titleOf(res))));
    else if (segs.length) nav = h('nav', { class: 'segs', 'aria-label': t('Rubrique') }, segs.map(s => h('a', { href: '#' + s.path, 'aria-current': s.path === res.route.path ? 'page' : null }, t(s.label))));
    const f = finder(res);
    if (nav || f) document.getElementById('main').before(bar = h('div', { class: 'strip' }, nav, f));
    // Sur téléphone, les sous-onglets défilent : l'actif est amené au centre, et un fondu dit de quel côté il en reste.
    const cur = nav?.querySelector('[aria-current]');
    if (cur && nav.scrollWidth > nav.clientWidth) {
      const edge = () => { nav.classList.toggle('more-r', nav.scrollLeft + nav.clientWidth < nav.scrollWidth - 4); nav.classList.toggle('more-l', nav.scrollLeft > 4); };
      nav.scrollLeft = cur.offsetLeft - (nav.clientWidth - cur.offsetWidth) / 2; edge(); nav.addEventListener('scroll', edge, { passive: true });
    }
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
