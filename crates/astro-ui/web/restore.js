'use strict';
// Rétablir son MULTIPASS sur cette station depuis deux parts de la clé de secours (QR ou texte), ou l'ancienne carte complète. Elles restent dans cette page : elles ne sont
// envoyées qu'à cette station (POST /api/v1/restore), jamais stockées, et le champ est vidé dès la réponse.
const $ = s => document.querySelector(s);
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
const T = {
  en: { skip: 'Skip to content', title: 'Restore my account here', label: 'Your rescue key and your friend’s share (one per line)', scan: 'Scan the QR with the camera', go: 'Restore my account', done: 'Your account is restored', open: 'Open my space',
    lead: 'Your old station is down, or you are moving? Scan or paste your <strong>rescue key</strong> (the QR you printed), then the <strong>share of a friend</strong> who hosts your backup: call them, they read it in Account › Private backup. This station then becomes your new home; your friends, contacts and files come back by themselves.',
    after: '<strong>Write down your new PASS code</strong>: the old one cannot be recovered. Change nothing until the restoration is over: the station finds your friends, then your files, which come back from them over the next hours. You will be told.',
    pass: 'New PASS code', email: 'Login (email)', token: 'Web token', busy: 'Restoring…', cam: 'Camera unavailable: paste the codes instead.', down: 'Station unreachable.', fail: 'Restoration failed.' },
  es: { skip: 'Ir al contenido', title: 'Restablecer mi cuenta aquí', label: 'Su clave de rescate y la parte de su amigo (una por línea)', scan: 'Escanear el QR con la cámara', go: 'Restablecer mi cuenta', done: 'Su cuenta está restablecida', open: 'Abrir mi espacio',
    lead: '¿Su antigua estación está averiada o se muda? Escanee o pegue su <strong>clave de rescate</strong> (el QR que imprimió) y luego la <strong>parte de un amigo</strong> que aloja su copia: llámelo, la lee en Cuenta › Copia privada. Esta estación pasa a ser su nuevo hogar; sus amigos, contactos y archivos vuelven solos.',
    after: '<strong>Anote su nuevo código PASS</strong>: el anterior no se puede recuperar. No cambie nada hasta que termine la restauración: la estación encuentra a sus amigos y luego sus archivos, que vuelven de ellos a lo largo de las horas. Se le avisará.',
    pass: 'Nuevo código PASS', email: 'Identificador (correo)', token: 'Token web', busy: 'Restableciendo…', cam: 'Cámara no disponible: pegue los códigos.', down: 'Estación inaccesible.', fail: 'Error al restablecer.' },
};
const fr = { pass: 'Nouveau code PASS', email: 'Identifiant de connexion (email)', token: 'Jeton de connexion web', busy: 'Rétablissement…', cam: 'Caméra indisponible : collez les codes.', down: 'Station injoignable.', fail: 'Échec du rétablissement.' };
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
          // Chaque QR lu s'ajoute (une part par ligne) ; la caméra s'arrête quand il y a deux parts, ou une ancienne carte complète.
          for (const c of await detector.detect(v)) {
            const code = c.rawValue, box = $('#card');
            if (!/^astro-(share|secret):/.test(code) || box.value.includes(code)) continue;
            box.value = (box.value.trim() + '\n' + code).trim();
            const shares = box.value.split(/\s+/).filter(w => w.startsWith('astro-share:')).length;
            if (shares >= 2 || code.startsWith('astro-secret:')) stopCam();
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
