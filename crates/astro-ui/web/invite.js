'use strict';
const $ = s => document.querySelector(s);
const token = (location.pathname.match(/^\/i\/([0-9a-f]{32})\/?$/) || [])[1];
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
// Textes de la page : le français est la référence ; en et es suivent.
const T = {
  en: {
    skip: 'Skip to content', loading: 'Loading…', email: 'Your email address', go: 'Get my photos',
    privacy: 'A private account is created for you on this station. Your photos are encrypted; only the people you share them with can see them.',
    ready: 'Your account is ready', note: 'Write this down now.', once: 'It will never be shown again: without it, a lost account cannot be recovered.',
    save: 'Download my card (.txt)', open: 'Open my space', have: 'I already have a MULTIPASS',
    title: n => `${n} sent you photos`, lead: (n, c) => `${n} invites you to receive ${c} file(s). Enter your email to create your private space and see them.`,
    gone: 'This link is no longer valid.', down: 'Station unreachable.', got: (c, n) => `${c} file(s) from ${n} are waiting for you.`, warn: w => `Note: ${w}`,
    e400: 'Address refused: check it.', e429: 'Too many sign-ups from your network. Wait a little.', e501: 'Sign-ups are closed on this station.',
    cardHead: 'MULTIPASS', keep: 'Keep this file offline. It will never be shown again.',
    pwlead: 'This address already has a MULTIPASS here. Enter its PASS code to add these photos.', pwlabel: 'PASS code', pwgo: 'Continue',
    accept: 'Accept these photos', other: 'Use another account',
    hello: (n, c) => `Hello ${n}, add these ${c} file(s) to your account?`, added: (c, n) => `${c} file(s) from ${n} were added to your space.`,
    invite_used: 'This link has already been used.', invite_expired: 'This link has expired: ask for a new one.', invite_revoked: 'This link was withdrawn by the person who sent it.',
    pass_denied: 'Wrong PASS code.', pass_locked: 'Too many attempts: try again later.',
    email_taken: 'This address already has a MULTIPASS here: log in with it, then reopen this link to accept with your account.', invite_own: 'This is your own invitation.',
    rate_limited: 'Too many attempts from your network. Wait a little.', registration_closed: 'Sign-ups are closed on this station.',
    guest: 'I prefer not to give my email', guestnote: 'The station creates an address like name-x7k2p9@… for you. It is your UPlanet identifier: it is not meant to receive mail. Keep your card afterwards: without an email, it is your only way to recover the account.',
  },
  es: {
    skip: 'Ir al contenido', loading: 'Cargando…', email: 'Su dirección de correo', go: 'Recibir mis fotos',
    privacy: 'Se crea una cuenta privada para usted en esta estación. Sus fotos están cifradas; solo las personas con quienes las comparte pueden verlas.',
    ready: 'Su cuenta está lista', note: 'Anote esta información ahora.', once: 'No se volverá a mostrar: sin ella, una cuenta perdida no se puede recuperar.',
    save: 'Descargar mi tarjeta (.txt)', open: 'Abrir mi espacio', have: 'Ya tengo un MULTIPASS',
    title: n => `${n} le envía fotos`, lead: (n, c) => `${n} le invita a recibir ${c} archivo(s). Escriba su correo para crear su espacio privado y verlos.`,
    gone: 'Este enlace ya no es válido.', down: 'Estación inaccesible.', got: (c, n) => `${c} archivo(s) de ${n} le esperan.`, warn: w => `Nota: ${w}`,
    e400: 'Dirección rechazada: compruébela.', e429: 'Demasiados registros desde su red. Espere un poco.', e501: 'El registro está cerrado en esta estación.',
    cardHead: 'MULTIPASS', keep: 'Guarde este archivo sin conexión. No se volverá a mostrar.',
    pwlead: 'Esta dirección ya tiene un MULTIPASS aquí. Introduzca su código PASS para añadir estas fotos.', pwlabel: 'Código PASS', pwgo: 'Continuar',
    accept: 'Aceptar estas fotos', other: 'Usar otra cuenta',
    hello: (n, c) => `Hola ${n}, ¿añadir estos ${c} archivo(s) a su cuenta?`, added: (c, n) => `${c} archivo(s) de ${n} se añadieron a su espacio.`,
    invite_used: 'Este enlace ya se ha usado.', invite_expired: 'Este enlace ha caducado: pida uno nuevo.', invite_revoked: 'Quien lo envió retiró este enlace.',
    pass_denied: 'Código PASS incorrecto.', pass_locked: 'Demasiados intentos: inténtelo más tarde.',
    email_taken: 'Esta dirección ya tiene un MULTIPASS aquí: inicie sesión y vuelva a abrir este enlace para aceptarlo con su cuenta.', invite_own: 'Esta es su propia invitación.',
    rate_limited: 'Demasiados intentos desde su red. Espere un poco.', registration_closed: 'El registro está cerrado en esta estación.',
    guest: 'Prefiero no dar mi correo', guestnote: 'La estación le crea una dirección del tipo nombre-x7k2p9@… Es su identificador UPlanet: no sirve para recibir correos. Guarde después su tarjeta: sin correo, es su única forma de recuperar la cuenta.',
  },
};
const fr = {
  accept: 'Accepter ces photos', other: 'Utiliser un autre compte',
  hello: (n, c) => `Bonjour ${n}, ajouter ces ${c} fichier(s) à votre compte ?`, added: (c, n) => `${c} fichier(s) de ${n} ont été ajoutés à votre espace.`,
  invite_used: 'Ce lien a déjà été utilisé.', invite_expired: 'Ce lien a expiré : demandez-en un nouveau.', invite_revoked: 'Ce lien a été retiré par la personne qui vous l’a envoyé.',
  pass_denied: 'Code PASS incorrect.', pass_locked: 'Trop d’essais : réessayez plus tard.',
  email_taken: 'Cette adresse a déjà un MULTIPASS ici : connectez-vous avec, puis rouvrez ce lien pour l’accepter avec votre compte.', invite_own: 'C’est votre propre invitation.',
  rate_limited: 'Trop d’inscriptions depuis votre réseau. Patientez un peu.', registration_closed: 'L’inscription est fermée sur cette station.',
  title: n => `${n} vous envoie des photos`, lead: (n, c) => `${n} vous invite à recevoir ${c} fichier(s). Indiquez votre adresse email pour créer votre espace privé et les voir.`,
  gone: 'Ce lien n’est plus valable.', down: 'Station injoignable.', got: (c, n) => `${c} fichier(s) de ${n} vous attendent.`, warn: w => `À noter : ${w}`,
  e400: 'Adresse refusée : vérifiez-la.', e429: 'Trop d’inscriptions depuis votre réseau. Patientez un peu.', e501: 'L’inscription est fermée sur cette station.',
  cardHead: 'MULTIPASS', keep: 'Gardez ce fichier hors ligne. Il ne sera plus jamais affiché.',
};
const t = (k, ...a) => { const v = (T[lang] || fr)[k] ?? fr[k]; return typeof v === 'function' ? v(...a) : v; };
if (lang !== 'fr') {
  document.documentElement.lang = lang;
  for (const el of document.querySelectorAll('[data-t]')) { const v = (T[lang] || {})[el.dataset.t]; if (typeof v === 'string') el.textContent = v; }
}
// Message d'une erreur de l'API : le code stable est traduit ; à défaut, le statut, puis le texte du serveur.
const why = (r, j) => (j.code && t(j.code)) || t('e' + r.status) || j.error || t('down');
const FIELDS = [['email', 'Email'], ['token', 'Token'], ['pass', 'PASS (email + PASS)'], ['salt', 'SALT'], ['pepper', 'PEPPER'], ['nsec', 'nsec (secret)'], ['npub', 'npub'], ['g1pub', 'Ğ1'], ['nostrns', 'IPNS']];
let card = null, info = null;
$('#guest').onchange = ev => { const g = ev.target.checked; $('#email').required = !g; $('#email').hidden = g; document.querySelector('label[for=email]').hidden = g; $('#guest-note').hidden = !g; };
const fail = msg => { $('#title').textContent = ''; $('#lead').textContent = msg; $('#f').hidden = true; };

async function init() {
  if (!token) return fail(t('gone'));
  try {
    const r = await fetch('/api/v1/invite/' + token, { cache: 'no-store' });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) return fail(why(r, j) || t('gone'));
    document.title = t('title', j.from);
    $('#title').textContent = t('title', j.from);
    $('#lead').textContent = t('lead', j.from, j.count);
    const box = $('#thumbs');
    for (const n of j.thumbs || []) {
      const img = document.createElement('img');
      img.alt = ''; img.loading = 'lazy'; img.src = `/api/v1/invite/${token}/thumb/${n}`;
      img.onerror = () => img.remove();
      box.append(img);
    }
    $('#f').hidden = false; $('#email').focus();
    info = j; offerAccept(j);
    if (j.email_hint) $('#email').placeholder = j.email_hint;
  } catch { fail(t('down')); }
}

// Déjà connecté à cette station : proposer d'accepter avec son compte, sans formulaire. L'identité vient de la session.
async function offerAccept(j, given) {
  const basic = given || (() => { try { return localStorage.getItem('basic'); } catch { return null; } })();
  if (!basic) return;
  const auth = { Authorization: 'Basic ' + basic };
  try {
    const me = await fetch('/api/v1/me', { headers: auth, cache: 'no-store' });
    if (!me.ok) return;
    const who = (await me.json()).email;
    $('#acc-t').textContent = t('hello', j.label || who, j.count);
    $('#acc').hidden = false; $('#f').hidden = true; $('#lead').hidden = true;
    $('#acc-other').onclick = () => { $('#acc').hidden = true; $('#lead').hidden = false; $('#f').hidden = false; $('#email').focus(); };
    $('#acc-go').onclick = async ev => {
      ev.target.disabled = true; $('#acc-e').textContent = '';
      try {
        const r = await fetch(`/api/v1/invite/${token}/accept`, { method: 'POST', headers: auth, cache: 'no-store' }), a = await r.json().catch(() => ({}));
        if (!r.ok) { $('#acc-e').textContent = why(r, a); ev.target.disabled = false; return; }
        for (const id of ['#acc', '#thumbs', '#lead']) $(id).hidden = true;
        $('#title').textContent = ''; $('#acc-done').hidden = false; $('#acc-done-t').textContent = t('added', a.shared, a.from); $('#acc-done-t').focus();
        if (a.warnings && a.warnings.length) $('#acc-warns').textContent = t('warn', a.warnings.join(' ; '));
      } catch { $('#acc-e').textContent = t('down'); ev.target.disabled = false; }
    };
    $('#acc-open').onclick = () => { location.href = '/'; };
  } catch { /* station injoignable : le formulaire reste disponible */ }
}
$('#f').onsubmit = async ev => {
  ev.preventDefault();
  const b = $('#go'); b.disabled = true; $('#err').textContent = '';
  try {
    const r = await fetch(`/api/v1/invite/${token}/claim`, { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify($('#guest').checked ? { guest: true, lang } : { email: $('#email').value.trim(), lang }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { if (j.code === 'email_taken') return askPass(); $('#err').textContent = why(r, j); return; }
    card = j;
    $('#got').textContent = t('got', j.shared, j.from);
    if (j.warnings && j.warnings.length) $('#warns').textContent = t('warn', j.warnings.join(' ; '));
    const dl = $('#card'); dl.replaceChildren();
    for (const [k, label] of FIELDS) {
      if (!j[k]) continue;
      const dt = document.createElement('dt'), dd = document.createElement('dd');
      dt.textContent = label; dd.textContent = j[k]; dl.append(dt, dd);
    }
    for (const id of ['#f', '#thumbs', '#lead']) $(id).hidden = true;
    $('#title').textContent = ''; $('#done').hidden = false; $('#done-t').focus();
  } catch { $('#err').textContent = t('down'); } finally { b.disabled = false; }
};
// Adresse déjà hébergée ici (typiquement dans Safari alors que la session est dans la PWA) : le PASS suffit, sans quitter la page.
// La session obtenue reste en mémoire : elle n'est pas enregistrée dans ce navigateur.
function askPass() {
  $('#f').hidden = true; $('#lead').hidden = true; $('#pw').hidden = false; $('#pw-pass').focus();
}
$('#pw').onsubmit = async ev => {
  ev.preventDefault();
  const b = $('#pw-go'); b.disabled = true; $('#pw-e').textContent = '';
  try {
    const r = await fetch('/api/v1/account/unlock', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: $('#email').value.trim(), pass: $('#pw-pass').value }) }), j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#pw-e').textContent = why(r, j); return; }
    $('#pw').hidden = true; $('#pw-pass').value = '';
    await offerAccept(info, btoa(j.email + ':' + j.token));
  } catch { $('#pw-e').textContent = t('down'); } finally { b.disabled = false; }
};
$('#save').onclick = () => {
  if (!card) return;
  const text = t('cardHead') + ' ' + card.constellation + '\n' + FIELDS.filter(([k]) => card[k]).map(([k, l]) => l + '\n  ' + card[k]).join('\n') + '\n\n' + t('keep') + '\n';
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
  a.download = 'multipass-' + card.email + '.txt';
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
};
$('#open').onclick = () => {
  if (!card) return;
  try { localStorage.setItem('basic', btoa(card.email + ':' + card.token)); } catch { /* connexion manuelle */ }
  location.href = '/';
};
init();
