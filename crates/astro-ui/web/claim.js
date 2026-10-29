'use strict';
// Accepter, avec son compte de CETTE station, l'invitation d'une autre station : `/claim#origin=<station>&token=<jeton>`.
// Le jeton est dans le fragment (jamais envoyé à un serveur) et retiré de l'adresse dès la lecture. La signature qui prouve
// l'identité est faite par cette station avec la clé du compte : elle ne quitte jamais cette station.
const $ = s => document.querySelector(s);
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
const T = {
  en: { skip: 'Skip to content', loading: 'Loading…', email: 'Your email address', pass: 'PASS code', go: 'Continue', accept: 'Accept these photos', open: 'Open my space',
    ask: (h, who) => `${h} invites you to receive photos. They will be added to the account ${who}.`, login: h => `Log in to receive the photos sent from ${h}.`,
    done: (h, n) => `Accepted: the photos from ${h} are on their way (${n}).`, warn: w => `Note: ${w}`, bad: 'This link is incomplete: open it again from the invitation page.', down: 'Station unreachable.',
    invite_gone: 'This link is no longer valid (used, expired or withdrawn).', pass_denied: 'Wrong PASS code.', pass_locked: 'Too many attempts: try again later.' },
  es: { skip: 'Ir al contenido', loading: 'Cargando…', email: 'Su dirección de correo', pass: 'Código PASS', go: 'Continuar', accept: 'Aceptar estas fotos', open: 'Abrir mi espacio',
    ask: (h, who) => `${h} le invita a recibir fotos. Se añadirán a la cuenta ${who}.`, login: h => `Inicie sesión para recibir las fotos enviadas desde ${h}.`,
    done: (h, n) => `Aceptado: las fotos de ${h} están en camino (${n}).`, warn: w => `Nota: ${w}`, bad: 'Este enlace está incompleto: ábralo de nuevo desde la página de invitación.', down: 'Estación inaccesible.',
    invite_gone: 'Este enlace ya no es válido (usado, caducado o retirado).', pass_denied: 'Código PASS incorrecto.', pass_locked: 'Demasiados intentos: inténtelo más tarde.' },
};
const fr = {
  ask: (h, who) => `${h} vous invite à recevoir des photos. Elles seront ajoutées au compte ${who}.`, login: h => `Connectez-vous pour recevoir les photos envoyées depuis ${h}.`,
  done: (h, n) => `Accepté : les photos de ${h} sont en route (${n}).`, warn: w => `À noter : ${w}`, bad: 'Ce lien est incomplet : rouvrez-le depuis la page d’invitation.', down: 'Station injoignable.',
  invite_gone: 'Ce lien ne sert plus (utilisé, expiré ou retiré).', pass_denied: 'Code PASS incorrect.', pass_locked: 'Trop d’essais : réessayez plus tard.',
};
const t = (k, ...a) => { const v = (T[lang] || fr)[k] ?? fr[k]; return typeof v === 'function' ? v(...a) : v; };
if (lang !== 'fr') {
  document.documentElement.lang = lang;
  for (const el of document.querySelectorAll('[data-t]')) { const v = (T[lang] || {})[el.dataset.t]; if (typeof v === 'string') el.textContent = v; }
}
const why = (r, j) => (j.code && t(j.code)) || j.error || t('down');
const params = new URLSearchParams(location.hash.slice(1));
const token = params.get('token') || '', origin = (() => { try { const u = new URL(params.get('origin') || ''); return u.protocol === 'https:' || u.hostname === 'localhost' ? u.origin : ''; } catch { return ''; } })();
history.replaceState(null, '', location.pathname);
const host = origin && new URL(origin).host;
let basic = null, who = '';

const fail = msg => { $('#title').textContent = ''; $('#lead').textContent = msg; };
async function session(b) {
  try {
    const r = await fetch('/api/v1/me', { headers: { Authorization: 'Basic ' + b }, cache: 'no-store' });
    if (!r.ok) return false;
    who = (await r.json()).email; basic = b; return true;
  } catch { return false; }
}
function ask() {
  $('#login').hidden = true; $('#ask').hidden = false; $('#title').textContent = host; $('#lead').textContent = t('ask', host, who);
}
async function init() {
  if (!/^[0-9a-f]{32}$/.test(token) || !origin) return fail(t('bad'));
  $('#title').textContent = host;
  let saved = null;
  try { saved = localStorage.getItem('basic'); } catch { /* pas de session mémorisée */ }
  if (saved && await session(saved)) return ask();
  $('#lead').textContent = t('login', host); $('#login').hidden = false; $('#u').focus();
}
// Pas de session dans ce navigateur (ex. Safari alors que l'application installée a la sienne) : le PASS suffit.
// La session obtenue reste en mémoire, elle n'est pas enregistrée ici.
$('#login').onsubmit = async ev => {
  ev.preventDefault();
  const b = $('#login-go'); b.disabled = true; $('#login-e').textContent = '';
  try {
    const r = await fetch('/api/v1/account/unlock', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: $('#u').value.trim(), pass: $('#p').value }) }), j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#login-e').textContent = why(r, j); return; }
    $('#p').value = '';
    if (await session(btoa(j.email + ':' + j.token))) ask(); else $('#login-e').textContent = t('down');
  } catch { $('#login-e').textContent = t('down'); } finally { b.disabled = false; }
};
$('#yes').onclick = async ev => {
  ev.target.disabled = true; $('#err').textContent = '';
  try {
    const r = await fetch('/api/v1/invites/remote-accept', { method: 'POST', cache: 'no-store', headers: { Authorization: 'Basic ' + basic, 'Content-Type': 'application/json' }, body: JSON.stringify({ origin, token }) }), j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#err').textContent = why(r, j); ev.target.disabled = false; return; }
    $('#ask').hidden = true; $('#lead').hidden = true; $('#title').textContent = '';
    $('#done').hidden = false; $('#done-t').textContent = t('done', j.from, j.sent); $('#done-t').focus();
    if (j.warnings && j.warnings.length) $('#warns').textContent = t('warn', j.warnings.join(' ; '));
  } catch { $('#err').textContent = t('down'); ev.target.disabled = false; }
};
$('#open').onclick = () => { location.href = '/'; };
init();
