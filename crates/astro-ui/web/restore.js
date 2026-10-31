'use strict';
// Rétablir son MULTIPASS sur cette station depuis la carte secrète (QR ou texte). La carte reste dans cette page : elle n'est
// envoyée qu'à cette station (POST /api/v1/restore), jamais stockée, et le champ est vidé dès la réponse.
const $ = s => document.querySelector(s);
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
const T = {
  en: { skip: 'Skip to content', title: 'Restore my account here', label: 'Secret card', scan: 'Scan the QR with the camera', go: 'Restore my account', done: 'Your account is restored', open: 'Open my space',
    lead: 'Your old station is down, or you are moving? Scan or paste the <strong>secret card</strong> of your MULTIPASS: this station becomes your new home. Your friends, contacts and files come back by themselves, with nobody having to do anything.',
    after: '<strong>Write down your new PASS code</strong>: the old one cannot be recovered. Change nothing until the restoration is over: the station finds your friends, then your files, which come back from them over the next hours. You will be told.',
    pass: 'New PASS code', email: 'Login (email)', token: 'Web token', busy: 'Restoring…', cam: 'Camera unavailable: paste the card instead.', down: 'Station unreachable.', fail: 'Restoration failed.' },
  es: { skip: 'Ir al contenido', title: 'Restablecer mi cuenta aquí', label: 'Tarjeta secreta', scan: 'Escanear el QR con la cámara', go: 'Restablecer mi cuenta', done: 'Su cuenta está restablecida', open: 'Abrir mi espacio',
    lead: '¿Su antigua estación está averiada o se muda? Escanee o pegue la <strong>tarjeta secreta</strong> de su MULTIPASS: esta estación pasa a ser su nuevo hogar. Sus amigos, contactos y archivos vuelven solos, sin que nadie tenga que hacer nada.',
    after: '<strong>Anote su nuevo código PASS</strong>: el anterior no se puede recuperar. No cambie nada hasta que termine la restauración: la estación encuentra a sus amigos y luego sus archivos, que vuelven de ellos a lo largo de las horas. Se le avisará.',
    pass: 'Nuevo código PASS', email: 'Identificador (correo)', token: 'Token web', busy: 'Restableciendo…', cam: 'Cámara no disponible: pegue la tarjeta.', down: 'Estación inaccesible.', fail: 'Error al restablecer.' },
};
const fr = { pass: 'Nouveau code PASS', email: 'Identifiant de connexion (email)', token: 'Jeton de connexion web', busy: 'Rétablissement…', cam: 'Caméra indisponible : collez la carte.', down: 'Station injoignable.', fail: 'Échec du rétablissement.' };
const t = k => (T[lang] || fr)[k] ?? fr[k];
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
          const found = (await detector.detect(v)).find(c => c.rawValue.startsWith('astro-secret:'));
          if (found) { $('#card').value = found.rawValue; stopCam(); }
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
$('#open').onclick = () => {
  if (!result) return;
  try { localStorage.setItem('basic', btoa(result.email + ':' + result.token)); } catch { /* connexion manuelle */ }
  location.href = '/';
};
