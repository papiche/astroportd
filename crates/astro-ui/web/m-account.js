'use strict';
// Mon compte : identité, profil, langue, accès WebDAV (changement du jeton) et export des clés. La station détient les
// clés de ses comptes : modifier le profil, c'est lui faire signer un nouveau kind 0 ; l'export est une action délibérée.
const LANG = { fr: 'Français', en: 'English', es: 'Español' }, ROLE = { multipass: 'MULTIPASS', zencard: 'ZenCard', captain: 'Capitaine', node: 'Nœud', love: 'LOVE' };

export default async function account(root, K) {
  const { h, t } = K, api = (p, body) => K.api('/api/v1/account' + p, body ? { method: 'POST', body } : undefined);
  root.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-account.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const a = await api('');
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  const code = v => h('code', {}, v || '—');

  // ── Identité ──
  const idCard = h('section', { class: 'card' }, h('h2', {}, 'Identité'), h('dl', { class: 'stack' },
    h('dt', {}, 'Compte'), h('dd', {}, a.email || '—', ' ', h('span', { class: 'pill ok' }, ROLE[a.role] || a.role)),
    h('dt', {}, 'Clé publique Nostr'), h('dd', {}, code(a.npub), ' ', copy(a.npub, 'npub')),
    h('dt', {}, 'Clé Ğ1'), h('dd', {}, code(a.g1pub)),
    h('dt', {}, 'Station d\'accueil'), h('dd', {}, code(a.home_station)),
    h('dt', {}, 'Relais où vous publiez'), h('dd', {}, a.relays.length ? a.relays.map(r => h('div', {}, code(r.url), ` ${r.read && r.write ? '' : r.read ? '(lecture)' : '(écriture)'}`)) : h('span', { class: 'muted' }, 'aucun'))));


  // Téléverser une image de profil : réduite ici (cadre de l'image ou du bandeau) puis envoyée en JPEG ; la station la range, la publie
  // à son adresse et met le profil (kind 0) à jour. L'aperçu ne s'affiche que pour une image de cette station (CSP).
  const FRAME = { picture: [320, 320], banner: [1200, 400] };
  // Lu avant imageTools, qui s'en sert pour proposer les images de /Public.
  const site = await K.api('/api/v1/account/site').catch(() => null);
  const imageTools = (kind, input) => {
    const file = h('input', { type: 'file', accept: 'image/*', hidden: '' }), msg = h('span', { class: 'muted', role: 'status' });
    const pick = h('button', { type: 'button', class: 'ghost', onclick: () => file.click() }, t('Choisir une image sur cet appareil'));
    file.onchange = async () => {
      const f = file.files[0]; file.value = '';
      if (!f) return;
      pick.disabled = true; msg.textContent = t('Envoi…');
      try {
        const [w, hh] = FRAME[kind], bmp = await createImageBitmap(f), k = Math.max(w / bmp.width, hh / bmp.height), c = h('canvas', { width: w, height: hh });
        c.getContext('2d').drawImage(bmp, (w - bmp.width * k) / 2, (hh - bmp.height * k) / 2, bmp.width * k, bmp.height * k);
        const blob = await new Promise(ok => c.toBlob(ok, 'image/jpeg', .85)), data = await new Promise(ok => { const r = new FileReader(); r.onload = () => ok(String(r.result).split(',')[1]); r.readAsDataURL(blob); });
        await api('/image', { kind, data, origin: location.origin }); K.toast(t('Image enregistrée'), 1); account(root, K);
      } catch (e) { msg.textContent = ''; K.toast(e.message); pick.disabled = false; }
    };
    // Choisir une image déjà publiée dans /Public : la station la relit elle-même (aucun autre dossier n'est accepté).
    const grid = h('div', { class: 'ac-grid' });
    const pickPublic = h('button', { type: 'button', class: 'ghost', onclick: async () => {
      let r; try { r = await K.api('/api/v1/public/images'); } catch (e) { return K.toast(e.message); }
      grid.replaceChildren(...(r.images.length ? r.images.slice(0, 24).map(im => h('button', { type: 'button', class: 'ac-thumb', title: im.name, onclick: async () => {
        try { await api('/image', { kind, public_path: im.path, origin: location.origin }); K.toast(t('Image enregistrée'), 1); account(root, K); } catch (e) { K.toast(e.message); }
      } }, h('img', { src: `/site/${site.name}/${im.rel.split('/').map(encodeURIComponent).join('/')}`, alt: im.name, loading: 'lazy' }))) : [h('p', { class: 'muted' }, t('Aucune image dans Public : déplacez-en une avec « Publier » dans l’uCloud.'))]));
    } }, t('Choisir dans mes fichiers publics'));
    let own = false; try { own = input.value && new URL(input.value).origin === location.origin; } catch { /* pas une adresse */ }
    return h('div', {}, h('p', { class: 'row' }, pick, ' ', site ? pickPublic : null, file, ' ', msg), grid, own ? h('img', { src: input.value, alt: '', class: kind === 'banner' ? 'ac-banner' : 'ac-pic' }) : null);
  };

  // ── Profil et langue ──
  const f = { name: h('input', { id: 'ac-name', value: a.profile.name || '', maxlength: 64 }), about: h('textarea', { id: 'ac-about', rows: 3, maxlength: 500 }, a.profile.about || ''), banner: h('input', { id: 'ac-ban', value: a.profile.banner || '', placeholder: 'https://…', maxlength: 256 }), picture: h('input', { id: 'ac-pic', value: a.profile.picture || '', placeholder: 'https://…', maxlength: 256 }),
    lang: h('select', { id: 'ac-lang' }, a.langs.map(l => h('option', { value: l, selected: l === K.lang }, LANG[l] || l))) };
  const save = h('button', { onclick: async ev => {
    const patch = {};
    if (f.name.value.trim() !== (a.profile.name || '')) patch.name = f.name.value.trim();
    if (f.about.value.trim() !== (a.profile.about || '')) patch.about = f.about.value.trim();
    if (f.picture.value.trim() !== (a.profile.picture || '')) patch.picture = f.picture.value.trim();
    if (f.banner.value.trim() !== (a.profile.banner || '')) patch.banner = f.banner.value.trim();
    const langChanged = f.lang.value !== K.lang;
    if (langChanged) patch.lang = f.lang.value;
    if (!Object.keys(patch).length) return K.toast(t('Aucune modification'));
    ev.target.disabled = true;
    try { await api('/profile', patch); K.toast(t('Profil enregistré'), 1); if (langChanged) K.setLang(f.lang.value); else account(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, 'Enregistrer');
  const profile = h('section', { class: 'card' }, h('h2', {}, 'Profil'),
    h('p', { class: 'muted' }, 'Publié sur votre relai : vos Proches le voient.'),
    h('label', { for: 'ac-name' }, 'Nom affiché'), f.name, h('label', { for: 'ac-about' }, 'Présentation'), f.about, h('label', { for: 'ac-pic' }, 'Image (https)'), f.picture, imageTools('picture', f.picture), h('label', { for: 'ac-ban' }, t('Bandeau (https) : fond de vos Ğ1Billets et de votre profil')), f.banner, imageTools('banner', f.banner),
    h('label', { for: 'ac-lang' }, 'Langue de l\'interface'), f.lang, h('p', {}, save));


  // ── Mon site public : le dossier /Public de l'uCloud, servi en clair à tous (modèle : déplacer un fichier dans /Public le publie) ──
  const sitebox = site && h('section', { class: 'card' }, h('h2', {}, t('Mon site public')),
    h('p', { class: 'muted' }, t('Tout ce que vous mettez dans le dossier Public de votre uCloud est public : lisible par tout le monde, sans mot de passe. Déplacez-y un fichier pour le publier, sortez-le pour le retirer. Vous pouvez y construire un site (pages HTML, images, CSS) avec votre client WebDAV.')),
    h('dl', { class: 'stack' },
      h('dt', {}, t('Adresse de votre site')), h('dd', {}, h('a', { href: site.path_url, target: '_blank', rel: 'noopener' }, location.origin + site.path_url), ' ', copy(location.origin + site.path_url, t('Adresse'))),
      site.host_url ? [h('dt', {}, t('Adresse par nom (DNS)')), h('dd', {}, h('a', { href: site.host_url, target: '_blank', rel: 'noopener' }, site.host_url), ' ', copy(site.host_url, t('Adresse')))] : null,
      h('dt', {}, t('Page d’accueil')), h('dd', {}, site.has_index ? t('index.html de votre dossier Public') : t('page par défaut (votre profil et la liste des fichiers)')),
      h('dt', {}, t('Fichiers publiés')), h('dd', {}, String(site.files)),
      site.visits ? [h('dt', {}, t('Visites')), h('dd', {}, `${t('aujourd’hui')} ${site.visits.today} · 7 ${t('jours')} ${site.visits.week} · 90 ${t('jours')} ${site.visits.days90}`,
        site.visits.top.length ? h('ul', { class: 'muted' }, site.visits.top.map(x => h('li', {}, `${x.path} : ${x.hits}`))) : null)] : null,
      h('dt', {}, 'WebDAV'), h('dd', {}, code(a.dav.url), ' ', t('dossier'), ' ', code('/Public'))),
    site.has_index ? null : h('p', {}, h('button', { type: 'button', onclick: async ev => { ev.target.disabled = true; try { await K.api('/api/v1/account/site/example', { method: 'POST', body: {} }); K.toast(t('Site d’exemple créé dans Public'), 1); account(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; } } }, t('Créer un site d’exemple'))),
    h('details', {}, h('summary', {}, t('Exemple de page (index.html)')), h('pre', {}, '<!doctype html>\n<html lang="fr">\n<head>\n  <meta charset="utf-8">\n  <title>Ma page</title>\n  <link rel="stylesheet" href="style.css">\n</head>\n<body>\n  <h1>Bonjour</h1>\n  <p><a href="photos/vacances.jpg">Mes vacances</a></p>\n</body>\n</html>')),
    h('p', { class: 'muted' }, t('Vos fichiers restent chiffrés sur la station : elle ne les déchiffre que pour ce dossier. Les scripts d’une page tournent isolés de la station. Les visites sont comptées par jour et par page, sans garder aucune adresse ni identité de visiteur.')));

  // ── Intelligence artificielle : un Ollama ou une clé d'API déclarés ici font apparaître BRO ──
  const ai = h('section', { class: 'card' }, h('h2', {}, t('Intelligence artificielle')),
    h('p', { class: 'muted' }, t('BRO n\'apparaît dans le menu que si vous déclarez ici un Ollama ou une clé d\'API. La clé est chiffrée pour vous seul : elle n\'est jamais réaffichée.')));
  K.api('/api/v1/me/ai').then(p => {
    const url = h('input', { id: 'ai-url', value: p.ollama_url || '', placeholder: 'http://127.0.0.1:11434', 'aria-label': t('Adresse d\'Ollama'), maxlength: 256, autocomplete: 'off' });
    const prov = h('select', { id: 'ai-prov', 'aria-label': t('Fournisseur d\'API') }, [['', t('Aucun')], ['anthropic', 'Anthropic (Claude)'], ['gemini', 'Google Gemini']].map(([v, l]) => h('option', { value: v, selected: (p.provider || '') === v }, l)));
    const key = h('input', { id: 'ai-key', type: 'password', placeholder: p.has_key ? t('Clé enregistrée : laisser vide pour la garder') : t('Clé d\'API'), autocomplete: 'off', 'aria-label': t('Clé d\'API'), maxlength: 256 });
    const model = h('input', { id: 'ai-model', value: p.model || '', placeholder: t('Modèle (facultatif)'), 'aria-label': t('Modèle'), maxlength: 64, autocomplete: 'off' });
    const send = async body => { try { await K.api('/api/v1/me/ai', { method: 'POST', body }); K.toast(t('Paramètres IA enregistrés'), 1); location.reload(); } catch (e) { K.toast(e.message); } };
    ai.append(h('label', { for: 'ai-url' }, t('Ollama (adresse)')), url, h('label', { for: 'ai-prov' }, t('Ou un fournisseur d\'API')), prov, h('label', { for: 'ai-key' }, t('Clé d\'API')), key, h('label', { for: 'ai-model' }, t('Modèle')), model,
      h('p', {}, h('button', { onclick: () => send({ ollama_url: url.value.trim() || null, provider: prov.value || null, model: model.value.trim() || null, ...(key.value && { api_key: key.value }) }) }, t('Enregistrer')),
        p.has_key ? [' ', h('button', { class: 'ghost', onclick: () => { if (confirm(t('Retirer la clé d\'API enregistrée ?'))) send({ api_key: null }); } }, t('Retirer la clé'))] : null));
  }, e => ai.append(h('p', { class: 'err', role: 'alert' }, e.message)));

  // ── Visio : être joignable (mes amis réciproques voient si je suis là ou en appel) et notifications d'appel sur cet appareil ──
  const visio = h('section', { class: 'card' }, h('h2', {}, t('Visio')));
  K.api('/api/v1/presence').then(p => {
    const box = h('input', { type: 'checkbox', id: 'vi-reach', checked: p.reachable, onchange: async () => {
      box.disabled = true;
      try {
        await K.api('/api/v1/presence', { method: 'POST', body: { reachable: box.checked } });
        K.toast(box.checked ? t('Vos amis voient quand vous êtes là') : t('Vous n\'êtes plus visible'), 1);
        if (box.checked) K.mod('m-call.js').then(m => m.askNotify(), () => {});
      } catch (e) { box.checked = !box.checked; K.toast(e.message); }
      box.disabled = false;
    } });
    const N = window.Notification;
    const notif = !N ? null : N.permission === 'granted' ? h('p', { class: 'muted' }, t('Notifications d\'appel : activées sur cet appareil.'))
      : N.permission === 'denied' ? h('p', { class: 'muted' }, t('Notifications d\'appel bloquées sur cet appareil : autorisez-les dans le navigateur pour être prévenu quand l\'onglet est en arrière-plan.'))
      : h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: async ev => { await N.requestPermission().catch(() => {}); ev.target.replaceWith(h('span', { class: 'muted' }, N.permission === 'granted' ? t('Notifications d\'appel activées') : t('Notifications refusées'))); } }, t('Être prévenu des appels sur cet appareil')));
    visio.append(h('p', {}, h('label', { for: 'vi-reach' }, box, ' ', t('Être joignable en visio'))),
      h('p', { class: 'muted' }, t('Seuls vos amis réciproques voient si vous êtes en ligne ou en appel, et seulement quand une page Astroport est ouverte. Rien n\'est gardé.')), notif);
  }, () => { visio.hidden = true; });

  // ── Mes UPlanet : mes clés dans les autres plans de la station, et l'annonce (publique, sur demande) qu'elles sont les miennes ──
  const planes = h('section', { class: 'card', hidden: true }, h('h2', {}, t('Mes UPlanet')));
  K.api('/api/v1/me/links').then(r => {
    if (!r.siblings.length) return;
    planes.hidden = false;
    planes.append(h('p', { class: 'muted' }, t('Vous avez une clé Nostr différente dans chaque UPlanet. L\'annoncer permet à vos amis de vous écrire dans l\'UPlanet qu\'ils habitent : le lien est public, signé par vos deux clés.')),
      h('ul', {}, r.keys.map(k => h('li', {}, h('strong', {}, k.plane || '?'), ' ', h('small', { class: 'muted' }, k.hex.slice(0, 8) + '… · ' + k.via)))),
      h('p', {}, h('button', { id: 'announce-links', onclick: async ev => {
        if (!confirm(t('Annoncer publiquement que vos comptes des autres UPlanet sont les vôtres ?'))) return;
        ev.target.disabled = true;
        try { const a = await K.api('/api/v1/me/links', { method: 'POST', body: {} }); K.toast(t('{0} lien(s) annoncé(s)', a.linked.length), 1); } catch (e) { K.toast(e.message); } finally { ev.target.disabled = false; }
      } }, t('Annoncer mes autres comptes')), ' ', h('small', { class: 'muted' }, t('À faire dans chaque UPlanet.'))));
  }, () => { /* sans annuaire sur cette station : pas de carte */ });

  // ── Accès WebDAV : jeton ──
  // L'API donne un chemin (`/dav`) : on affiche et on copie l'adresse complète, celle qu'on saisit dans un client WebDAV.
  const davUrl = a.dav.url.startsWith('/') ? location.origin + a.dav.url : a.dav.url;
  const shown = h('div', {});
  const rotate = h('button', { class: 'ghost', onclick: async ev => {
    if (!confirm(t('Changer le jeton ? L\'ancien cesse de fonctionner tout de suite : téléphone, DAVx⁵ et autres clients WebDAV devront être reconfigurés.'))) return;
    ev.target.disabled = true;
    try {
      const r = await api('/token', {});
      K.login(r.login, r.token); // cette session suit le nouveau jeton
      shown.replaceChildren(h('p', { class: 'degraded', role: 'status' }, 'Nouveau jeton — il n\'est montré qu\'une fois :'), h('p', {}, code(r.token), ' ', copy(r.token, 'jeton')));
      K.toast(t('Jeton changé'), 1);
    } catch (e) { K.toast(e.message); }
    ev.target.disabled = false;
  } }, 'Remplacer le jeton principal');
  const dav = h('section', { class: 'card' }, h('h2', {}, 'Accès WebDAV'),
    h('p', { class: 'muted' }, 'Pour monter vos fichiers (uCloud) sur un ordinateur ou un téléphone.'),
    h('dl', {}, h('dt', {}, 'Adresse'), h('dd', {}, code(davUrl), ' ', copy(davUrl, 'adresse')), h('dt', {}, 'Identifiant'), h('dd', {}, code(a.dav.login)), h('dt', {}, 'Jeton principal'), h('dd', {}, a.token_generation ? t('remplacé {0} fois', a.token_generation) : t('celui de la station, jamais remplacé'))),
    h('details', { class: 'fold' }, h('summary', {}, 'Remplacer le jeton principal'),
      h('p', { class: 'muted' }, 'À faire seulement si vous pensez qu\'il a fuité : l\'ancien cesse de fonctionner tout de suite, et les clients WebDAV branchés avec lui (ordinateur, DAVx⁵…) devront être reconfigurés. Pour brancher un téléphone, créez plutôt un jeton d\'appareil (carte « Appareils »).'),
      h('p', {}, rotate)), shown);

  // ── Export des clés ──
  const out = h('div', {});
  const ok = h('input', { type: 'checkbox', id: 'ac-ok' }), exp = h('button', { class: 'ghost', disabled: true, onclick: async ev => {
    ev.target.disabled = true;
    try {
      const k = await api('/keys', { confirm: true });
      out.replaceChildren(h('p', { class: 'degraded', role: 'alert' }, 'Ces clés donnent le contrôle total du compte : ne les copiez que dans un gestionnaire de mots de passe.'),
        h('label', {}, 'Clé secrète Nostr (nsec)'), h('textarea', { rows: 2, readonly: true, 'aria-label': 'nsec' }, k.nsec),
        ...(k.dunikey ? [h('label', {}, 'Clé Ğ1 (.dunikey)'), h('textarea', { rows: 4, readonly: true, 'aria-label': 'dunikey' }, k.dunikey)] : [h('p', { class: 'err', role: 'alert' }, `Clé Ğ1 non exportée : ${k.dunikey_error}`)]),
        h('p', {}, h('button', { class: 'ghost', onclick: () => out.replaceChildren() }, 'Masquer')));
    } catch (e) { K.toast(e.message); }
    ok.checked = false;
  } }, 'Exporter mes clés');
  ok.onchange = () => { exp.disabled = !ok.checked; };
  const keys = h('section', { class: 'card' }, h('h2', {}, 'Clés'),
    h('p', { class: 'muted' }, 'Votre station garde vos clés. Vous pouvez les récupérer pour reprendre le contrôle de votre identité ailleurs (3 exports par heure ; chaque export est tracé).'),
    h('p', {}, ok, h('label', { for: 'ac-ok' }, ' Je comprends que quiconque possède ces clés possède mon compte.')), h('p', {}, exp), out);

  // ── Appareils : un jeton WebDAV par téléphone ou ordinateur, confiné à un dossier ──
  const devices = h('section', { class: 'card' }, h('h2', {}, 'Appareils'),
    h('p', { class: 'muted' }, 'Un jeton par appareil : son envoi WebDAV arrive dans le dossier de votre choix, et il ne voit ni ne touche rien d\'autre. Révocable à tout moment.'));
  try {
    const MODES = { write: 'Envoi seul', readwrite: 'Lecture et écriture', read: 'Lecture seule' };
    const shownToken = h('div');
    const label = h('input', { id: 'dv-label', placeholder: 'Mon téléphone', maxlength: 60 }), root = h('input', { id: 'dv-root', placeholder: '/Photos/Mon téléphone' });
    const mode = h('select', { id: 'dv-mode' }, Object.entries(MODES).map(([v, n]) => h('option', { value: v }, t(n))));
    const add = h('button', { type: 'button', onclick: async ev => {
      ev.target.disabled = true;
      try {
        const r = await K.api('/api/v1/devices', { method: 'POST', body: { label: label.value, root: root.value || `/Photos/${label.value.trim()}`, mode: mode.value } });
        const inp = h('input', { value: r.token, readOnly: true, 'aria-label': 'Jeton', onfocus: () => inp.select() });
        shownToken.replaceChildren(h('div', { class: 'card wide' }, h('h3', {}, t('Appareil « {0} » ajouté', r.label)),
          h('p', { class: 'warn', role: 'note' }, r.note),
          h('dl', {}, h('dt', {}, 'Serveur'), h('dd', {}, code(davUrl), ' ', copy(davUrl, 'adresse')), h('dt', {}, 'Identifiant'), h('dd', {}, code(a.dav.login)), h('dt', {}, 'Mot de passe (jeton)'), h('dd', {}, inp, ' ', copy(r.token, 'jeton')), h('dt', {}, 'Dossier du uCloud'), h('dd', {}, code(r.root), ' · ', t(MODES[r.mode])))));
        K.toast(t('Appareil ajouté'), 1); label.value = ''; root.value = '';
        refresh();
      } catch (e) { K.toast(e.message); } finally { ev.target.disabled = false; }
    } }, 'Ajouter cet appareil');
    const rows = h('div');
    const refresh = async () => {
      const l = await K.api('/api/v1/devices');
      rows.replaceChildren(...(l.devices.length ? l.devices.map(d => h('div', { class: 'row' },
        h('div', { class: 't' }, h('b', {}, d.label), h('span', { class: 'muted' }, ` · ${t(MODES[d.mode])} · `), code(d.root), h('br'), h('span', { class: 'muted' }, d.last_used ? t('Dernier envoi : {0}', K.when(d.last_used)) : t('Jamais utilisé'))),
        h('button', { class: 'ghost', onclick: async ev => { if (!confirm(t('Révoquer « {0} » ? Son jeton cesse de fonctionner tout de suite ; ce qu\'il a déposé reste dans votre uCloud.', d.label))) return; ev.target.disabled = true; try { await K.api('/api/v1/devices/revoke', { method: 'POST', body: { id: d.id } }); K.toast(t('Appareil révoqué'), 1); } catch (e) { K.toast(e.message); } refresh(); } }, 'Révoquer')))
        : [h('p', { class: 'muted' }, 'Aucun appareil : le jeton principal ci-dessus ouvre tout votre uCloud.')]));
    };
    devices.append(rows, shownToken, h('h3', {}, 'Ajouter un appareil'), h('label', { for: 'dv-label' }, 'Nom'), label, h('label', { for: 'dv-root' }, 'Dossier du uCloud (créé s\'il manque)'), root, h('label', { for: 'dv-mode' }, 'Droits'), mode, h('p', {}, add));
    await refresh();
  } catch (e) { devices.append(h('p', { class: 'muted' }, e.message)); }

  // ── Sauvegarde privée chez un ami ──
  const gb = n => n >= 2 ** 30 ? `${(n / 2 ** 30).toFixed(1)} Go` : `${(n / 2 ** 20).toFixed(0)} Mo`;
  const backup = h('section', { class: 'card' }, h('h2', {}, 'Sauvegarde privée'));
  try {
    const b = await K.api('/api/v1/backup'), post = (path, body) => K.api(path, { method: 'POST', body }).then(() => account(root, K), e => K.toast(e.message));
    const mine = b.mine, friend = h('select', { id: 'bk-friend', 'aria-label': 'Ami qui héberge' }, b.friends.map(f => h('option', { value: f.hex, selected: mine && f.hex === mine.host }, f.label)));
    backup.append(h('p', { class: 'muted' }, 'Vos fichiers et contacts non partagés sont copiés, déjà chiffrés, chez un ami qui l\'accepte : il ne peut rien lire et la copie compte dans son quota. Envoi une fois par nuit (20h12 solaire).'));
    if (!b.friends.length && !mine) backup.append(h('p', { class: 'muted' }, 'Aucun proche réciproque : suivez-vous mutuellement pour choisir un hébergeur.'));
    else if (mine && mine.enabled) backup.append(
      h('p', {}, h('span', { class: mine.status === 'accepted' ? 'pill ok' : 'pill' }, { pending: 'En attente de réponse', accepted: 'Acceptée', declined: 'Refusée', revoked: 'Révoquée' }[mine.status] || mine.status), ` ${mine.label}`),
      mine.status === 'accepted' ? h('p', {}, `${mine.files} fichier(s) · ${gb(mine.backed_up_bytes)} / ${gb(mine.quota_bytes)}`) : null,
      h('p', {}, h('button', { class: 'ghost', onclick: () => post('/api/v1/backup', { enabled: false }) }, 'Suspendre')));
    else backup.append(h('label', { for: 'bk-friend' }, 'Ami qui héberge ma sauvegarde'), friend, h('p', {}, h('button', { onclick: () => post('/api/v1/backup', { enabled: true, host: friend.value }) }, 'Demander à cet ami')));
    // Clé de secours : la part P3 est confiée automatiquement aux amis qui hébergent la sauvegarde.
    if (mine && mine.status === 'accepted') backup.append(h('p', { class: b.rescue_friends ? 'muted' : 'warn' }, b.rescue_friends
      ? `Clé de secours : la part de vos amis est confiée à ${b.rescue_friends} ami(s). Si vous perdez votre compte, appelez l'un d'eux : avec votre QR imprimé, sa part rétablit votre compte.`
      : 'Clé de secours : la part de vos amis n\'est pas encore confiée (envoi en cours).'));
    // Côté hébergeur : demandes reçues et sauvegardes hébergées.
    for (const r of b.hosting) {
      const size = h('input', { type: 'number', min: 1, step: 1, value: Math.max(1, Math.round(r.quota_bytes / 2 ** 30)) || 5, 'aria-label': 'Volume en Go' });
      backup.append(h('hr'), h('p', {}, h('strong', {}, r.label), r.status === 'pending' ? ' demande à vous confier sa sauvegarde chiffrée.' : ` : ${gb(r.used_bytes)} / ${gb(r.quota_bytes)} hébergés pour lui.`),
        r.status === 'pending' ? h('p', {}, 'Volume mis à disposition (Go) : ', size, ' ', h('button', { onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: true, quota_bytes: Math.round(Number(size.value) * 2 ** 30) }) }, 'Accepter'), ' ', h('button', { class: 'ghost', onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: false }) }, 'Refuser'))
          : h('p', {}, h('button', { class: 'ghost', onclick: () => confirm(t('Révoquer : sa sauvegarde sera supprimée de votre espace.')) && post('/api/v1/backup/respond', { owner: r.owner, accept: false }) }, 'Révoquer et libérer l\'espace')));
      // Secours : cet ami dit avoir perdu sa station. Je confirme seulement après lui avoir parlé : sa part part alors seule.
      if (r.rescue) {
        const ok = h('input', { type: 'checkbox' }), answer = accept => post('/api/v1/backup/rescue', { owner: r.owner, accept });
        const yes = h('button', { disabled: true, onclick: () => answer(true) }, t('Rendre son compte à {0}', r.label));
        ok.onchange = () => { yes.disabled = !ok.checked; };
        backup.append(h('div', { class: 'warn', role: 'alert' },
          h('p', {}, h('strong', {}, t('🆘 {0} dit avoir perdu sa station', r.label)), ' ', t('et veut retrouver son compte sur « {0} ».', r.rescue.station)),
          h('p', {}, t('Appelez {0} avant de confirmer. Si vous ne reconnaissez pas sa voix, refusez : votre Capitaine sera prévenu.', r.label)),
          h('p', {}, h('label', {}, ok, ' ', t('J\'ai parlé à {0} et je l\'ai reconnu(e)', r.label))),
          h('p', {}, yes, ' ', h('button', { class: 'ghost danger', onclick: () => answer(false) }, t('Ce n\'est pas {0}', r.label)))));
      }
      // « J'appelle un ami » : cet ami me confie une part de sa clé de secours ; je la lui montre au téléphone (texte ou QR).
      if (r.share) {
        const shown = h('div', { hidden: true });
        const svg = r.share_svg && new DOMParser().parseFromString(r.share_svg, 'image/svg+xml').documentElement;
        if (svg && svg.nodeName === 'svg') shown.append(document.importNode(svg, true));
        shown.append(h('p', { class: 'muted', style: 'word-break:break-all' }, r.share));
        backup.append(h('p', { class: 'muted' }, `${r.label} vous a confié une part de sa clé de secours. Seule, elle ne sert à rien. Montrez-la-lui seulement s'il vous appelle et que vous reconnaissez sa voix.`),
          h('p', {}, h('button', { class: 'ghost', onclick: ev => { shown.hidden = !shown.hidden; ev.target.textContent = shown.hidden ? 'Montrer sa part de secours' : 'Cacher'; } }, 'Montrer sa part de secours')), shown);
      }
    }
  } catch (e) { backup.append(h('p', { class: 'muted' }, e.message)); }

  // Compte invité : l'adresse a été générée par la station. On ne promet pas une fonction qui n'existe pas (changer d'adresse).
  const guest = a.guest ? h('p', { class: 'warn', role: 'note' }, h('strong', {}, 'Compte invité. '), 'Votre adresse a été créée par la station : c\'est votre identifiant UPlanet, pas une boîte aux lettres. Gardez la clé de secours imprimée à la création et votre code PASS : sans email, ce sont vos seuls moyens de récupérer le compte.') : null;
  // Bandeau d'identité (comme « Mon compte » d'UPlanet). `replaceChildren` écrirait le texte « null » pour un nœud absent : on filtre.
  const title = a.profile.name || a.email || 'Mon compte';
  const hero = h('div', { class: 'hero' }, h('div', { class: 'avatar', 'aria-hidden': 'true' }, [...title][0].toUpperCase()),
    h('div', { class: 'who' }, h('h2', {}, title), a.email && a.email !== title ? h('div', { class: 'sub' }, a.email) : null, h('span', { class: 'pill' }, ROLE[a.role] || a.role), a.guest ? h('span', { class: 'pill' }, 'Invité') : null));
  // « Si je perds tout » : la procédure de la clé de secours (3 morceaux, 2 suffisent), dite sans cryptographie.
  const rescue = h('section', { class: 'card' }, h('h2', {}, t('Si je perds tout')),
    h('p', {}, t('Votre clé est découpée en 3 morceaux ; 2 suffisent pour tout retrouver : la carte de secours imprimée chez vous, la station, et l\'ami qui garde votre sauvegarde.')),
    h('details', { class: 'fold' }, h('summary', {}, t('J\'ai oublié mon code PASS')), h('p', {}, t('Votre station marche toujours : demandez à son Capitaine un code de secours. Rien n\'est perdu.'))),
    h('details', { class: 'fold' }, h('summary', {}, t('Ma station est détruite, ou je déménage')), h('ol', {},
      h('li', {}, t('Sur une autre station de la même UPlanet, ouvrez la page « Rétablir mon compte » (adresse /restore).')),
      h('li', {}, t('Scannez votre carte de secours imprimée.')),
      h('li', {}, t('Appuyez sur « Prévenir mes gardiens », puis appelez l\'ami qui garde votre sauvegarde : il confirme d\'un geste dans Mon compte › Ma sécurité. Reconnaître votre voix est la seule vérification.')),
      h('li', {}, t('Dès qu\'il confirme, votre compte renaît tout seul. Notez le nouveau code PASS affiché.')),
      h('li', {}, t('Vos amis, vos contacts et vos fichiers reviennent d\'eux-mêmes au fil des heures.')))));
  // Trois onglets plutôt qu'une colonne de dix cartes : l'onglet ouvert se garde dans l'adresse (#/account?tab=securite).
  const TABS = [['moi', t('Moi'), [profile, sitebox, visio, ai, planes]], ['appareils', t('Mes appareils'), [dav, devices]], ['securite', t('Ma sécurité'), [rescue, backup, idCard, keys]]];
  const want = new URLSearchParams(location.hash.split('?')[1] || '').get('tab'), cur = TABS.some(x => x[0] === want) ? want : 'moi';
  const panels = TABS.map(([k, , cards]) => h('div', { class: 'grid', role: 'tabpanel', id: 'ac-p-' + k, 'aria-labelledby': 'ac-t-' + k, hidden: k !== cur }, cards));
  const tabs = TABS.map(([k, label], i) => h('button', { type: 'button', class: 'link', role: 'tab', id: 'ac-t-' + k, 'aria-controls': 'ac-p-' + k, 'aria-selected': String(k === cur), tabIndex: k === cur ? 0 : -1, onclick: () => pick(i),
    onkeydown: ev => { const d = { ArrowRight: 1, ArrowLeft: -1 }[ev.key]; if (d) { ev.preventDefault(); pick((i + d + TABS.length) % TABS.length, true); } } }, label));
  const pick = (i, focus) => {
    tabs.forEach((b, j) => { b.setAttribute('aria-selected', String(i === j)); b.tabIndex = i === j ? 0 : -1; panels[j].hidden = i !== j; });
    if (focus) tabs[i].focus();
    history.replaceState(null, '', '#/account?tab=' + TABS[i][0]);
  };
  root.replaceChildren(...[hero, guest, h('div', { class: 'ac-tabs', role: 'tablist', 'aria-label': t('Mon compte') }, tabs), ...panels].filter(Boolean));
}
