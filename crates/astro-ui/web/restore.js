'use strict';
// Rétablir son MULTIPASS sur cette station depuis deux parts de la clé de secours (QR ou texte), ou l'ancienne carte complète. Elles restent dans cette page : elles ne sont
// envoyées qu'à cette station (POST /api/v1/restore), jamais stockées, et le champ est vidé dès la réponse.
const $ = s => document.querySelector(s);
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
const T = {
  en: { skip: 'Skip to content', title: 'Restore my account here', label: 'Your rescue card (and, if your friend reads it to you, their share: one per line)', ask: 'Alert my guardians',
    manual: 'No answer? Your friend can also read you their share (My account › My security › “Show their rescue share”): paste it under your card, one per line.',
    p1: 'Scan or paste your printed rescue card first.', searching: 'Looking for your contacts on the network… (a few minutes)', waiting: '{0} contact(s) alerted. Call the friend who keeps your backup: as soon as they confirm, your account comes back here by itself. Keep this page open (or come back with your card within 24 h).', denied: 'Did not recognise the request: {0}.', ready: '{0} confirmed: restoring…', scan: 'Scan the QR with the camera', go: 'Restore my account', done: 'Your account is restored', open: 'Open my space',
    lead: 'Your old station is down, or you are moving? Scan your <strong>rescue card</strong> (the printed QR), then <strong>alert your guardians</strong>: the friend who keeps your backup gets your request, you call each other, and they confirm with one tap. Your account then comes back here by itself; your friends, contacts and files return on their own.',
    after: '<strong>Write down your new PASS code</strong>: the old one cannot be recovered. Change nothing until the restoration is over: the station finds your friends, then your files, which come back from them over the next hours. You will be told.',
    pass: 'New PASS code', email: 'Login (email)', token: 'Web token', busy: 'Restoring…', cam: 'Camera unavailable: paste the codes instead.', down: 'Station unreachable.', fail: 'Restoration failed.' },
  es: { skip: 'Ir al contenido', title: 'Restablecer mi cuenta aquí', label: 'Su tarjeta de rescate (y, si su amigo se la lee, su parte: una por línea)', ask: 'Avisar a mis guardianes',
    manual: '¿Sin respuesta? Su amigo también puede leerle su parte (Mi cuenta › Mi seguridad › «Mostrar su parte de rescate»): péguela bajo su tarjeta, una por línea.',
    p1: 'Primero escanee o pegue su tarjeta de rescate impresa.', searching: 'Buscando sus contactos en la red… (unos minutos)', waiting: '{0} contacto(s) avisado(s). Llame al amigo que guarda su copia: en cuanto confirme, su cuenta renace aquí sola. Mantenga esta página abierta (o vuelva con su tarjeta en 24 h).', denied: 'No reconoció la solicitud: {0}.', ready: '{0} confirmó: restableciendo…', scan: 'Escanear el QR con la cámara', go: 'Restablecer mi cuenta', done: 'Su cuenta está restablecida', open: 'Abrir mi espacio',
    lead: '¿Su antigua estación está averiada o se muda? Escanee su <strong>tarjeta de rescate</strong> (el QR impreso) y luego <strong>avise a sus guardianes</strong>: el amigo que guarda su copia recibe su solicitud, se llaman y él confirma con un gesto. Su cuenta renace aquí sola; sus amigos, contactos y archivos vuelven por sí mismos.',
    after: '<strong>Anote su nuevo código PASS</strong>: el anterior no se puede recuperar. No cambie nada hasta que termine la restauración: la estación encuentra a sus amigos y luego sus archivos, que vuelven de ellos a lo largo de las horas. Se le avisará.',
    pass: 'Nuevo código PASS', email: 'Identificador (correo)', token: 'Token web', busy: 'Restableciendo…', cam: 'Cámara no disponible: pegue los códigos.', down: 'Estación inaccesible.', fail: 'Error al restablecer.' },
};
const fr = { p1: 'Scannez ou collez d’abord votre carte de secours imprimée.', searching: 'Recherche de vos contacts sur le réseau… (quelques minutes)', waiting: '{0} contact(s) prévenu(s). Appelez l’ami qui garde votre sauvegarde : dès qu’il confirme, votre compte renaît ici, tout seul. Gardez cette page ouverte (ou revenez avec votre carte dans les 24 h).', denied: 'N’a pas reconnu la demande : {0}.', ready: '{0} a confirmé : rétablissement…', pass: 'Nouveau code PASS', email: 'Identifiant de connexion (email)', token: 'Jeton de connexion web', busy: 'Rétablissement…', cam: 'Caméra indisponible : collez les codes.', down: 'Station injoignable.', fail: 'Échec du rétablissement.' };
const t = (k, ...a) => a.reduce((s, v, i) => s.replace(`{${i}}`, v), (T[lang] || fr)[k] ?? fr[k]);
if (lang !== 'fr') {
  document.documentElement.lang = lang;
  for (const el of document.querySelectorAll('[data-t]')) { const v = (T[lang] || {})[el.dataset.t]; if (typeof v === 'string') el.innerHTML = v; }
}
let result = null, stream = null, timer = 0;
const stopCam = () => { clearInterval(timer); if (stream) for (const tr of stream.getTracks()) tr.stop(); stream = null; $('#cam').hidden = true; };
if ('BarcodeDetector' in window && navigator.mediaDevices?.getUserMedia) {
  $('#scan').hidden = false;
  $('#scan').onclick = async () => {
    $('#err').textContent = '';
    try {
      const detector = new BarcodeDetector({ formats: ['qr_code'] });
      stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' } });
      const v = $('#cam'); v.srcObject = stream; v.hidden = false; await v.play();
      timer = setInterval(async () => {
        try {
          // Chaque QR lu s'ajoute (une part par ligne) ; la caméra s'arrête quand il y a deux parts, ou une ancienne carte complète.
          for (const c of await detector.detect(v)) {
            const code = c.rawValue, box = $('#card');
            if (!/^astro-(share|secret):/.test(code) || box.value.includes(code)) continue;
            box.value = (box.value.trim() + '\n' + code).trim();
            const shares = box.value.split(/\s+/).filter(w => w.startsWith('astro-share:')).length;
            if (shares >= 2 || code.startsWith('astro-secret:')) stopCam();
            else if (p1() && !poll) ask(false); // une demande aux gardiens est peut-être déjà ouverte
          }
        } catch { /* image pas prête : on réessaie */ }
      }, 300);
    } catch { stopCam(); $('#err').textContent = t('cam'); }
  };
}
$('#f').onsubmit = async ev => {
  ev.preventDefault();
  stopCam();
  const b = $('#go'); b.disabled = true; $('#err').textContent = ''; $('#info').textContent = t('busy');
  try {
    const r = await fetch('/api/v1/restore', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ card: $('#card').value.trim(), lang }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#err').textContent = j.error || t('fail'); return; }
    result = j;
    clearInterval(poll); poll = 0; $('#rs').textContent = '';
    $('#card').value = '';
    const dl = $('#out');
    dl.replaceChildren();
    for (const [k, label] of [['email', t('email')], ['pass', t('pass')], ['token', t('token')]]) {
      const dt = document.createElement('dt'), dd = document.createElement('dd');
      dt.textContent = label; dd.textContent = j[k] || '';
      dl.append(dt, dd);
    }
    $('#f').hidden = true; $('#done').hidden = false; $('#done-t').focus();
  } catch { $('#err').textContent = t('down'); } finally { b.disabled = false; $('#info').textContent = ''; }
};
// Secours par les gardiens : seule la carte imprimée (part 1) part, vers cette station seulement. La station prévient les
// contacts du compte ; la part de l'ami qui confirme arrive sur la station, jamais dans cette page. Relevé toutes les 10 s.
const p1 = () => $('#card').value.split(/\s+/).find(w => w.startsWith('astro-share:') && w.split('|')[5] === '1');
let poll = 0;
const ask = async send => {
  const card = p1();
  if (!card) { if (send) $('#err').textContent = t('p1'); return; }
  try {
    const r = await fetch('/api/v1/restore/ask', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ card, send }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { if (send) $('#err').textContent = j.error || t('fail'); return; }
    const denied = j.denied?.length ? ' ' + t('denied', j.denied.join(', ')) : '';
    if (j.status === 'none') return;
    if (!poll) poll = setInterval(() => ask(false), 10000);
    if (j.status === 'ready') {
      clearInterval(poll); poll = 0;
      $('#rs').textContent = t('ready', j.by || '');
      $('#f').requestSubmit(); // la station joint d'elle-même la part rendue à la carte
      return;
    }
    $('#rs').textContent = (j.status === 'searching' ? t('searching') : t('waiting', j.sent)) + denied;
  } catch { if (send) $('#err').textContent = t('down'); }
};
$('#ask').onclick = async () => { const b = $('#ask'); stopCam(); b.disabled = true; $('#err').textContent = ''; await ask(true); b.disabled = false; };
// Revenir avec sa carte reprend une demande déjà ouverte (24 h).
$('#card').addEventListener('input', () => { if (p1() && !poll) ask(false); });

$('#open').onclick = () => {
  if (!result) return;
  try { localStorage.setItem('basic', btoa(result.email + ':' + result.token)); } catch { /* connexion manuelle */ }
  location.href = '/';
};
