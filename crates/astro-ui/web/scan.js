'use strict';
// Encaisser un Ğ1Billet : scanner (caméra, photo ou saisie), dire où envoyer la valeur, confirmer. Public : le billet est un titre
// au porteur. Les parts du billet ne sont gardées qu'en mémoire, jamais écrites ni journalisées, et envoyées une seule fois.
import { classify, amount as money, shortAddr, verdict } from '/ui/scan-rules.js';
import { t, locale, localize } from '/ui/scan-i18n.js';
const amount = c => money(c, locale);

const $ = s => document.querySelector(s);
const el = (tag, attrs = {}, ...kids) => { const e = document.createElement(tag); for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v); e.append(...kids.filter(k => k != null)); return e; };
const st = { parts: [], billet: null, verified: false, dest: '', destOk: false, mode: 'billet', cam: null, last: ['', 0] };

async function call(path, body) {
  let r;
  try { r = await fetch('/api/v1/billet/' + path, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), cache: 'no-store' } : { cache: 'no-store' }); }
  catch { throw new Error(t('down')); }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((j.code && t(j.code) !== j.code && t(j.code)) || j.error || `Error ${r.status}`);
  return j;
}

// ── Étapes ───────────────────────────────────────────────────────────────────────────────────────────────────────
let booted = false;
function show(id) {
  for (const s of ['s1', 's2', 's3', 'done']) $('#' + s).hidden = s !== id;
  const n = { s1: 0, s2: 1, s3: 2, done: 3 }[id];
  document.querySelectorAll('#steps li').forEach((li, i) => { li.className = i === n ? 'on' : i < n ? 'done' : ''; li.toggleAttribute('aria-current', i === n); });
  if (id !== 's1' && id !== 's2') stopCamera();
  if (booted) $('#' + id + '-t')?.focus(); // pas de focus au chargement : il masquerait l'accueil
}
const hint = t => { $('#hint').textContent = t || ''; };

// ── Caméra : BarcodeDetector quand il existe, sinon jsQR (chargé seulement alors) ───────────────────────────────
let jsqrLoading = null;
const loadJsqr = () => window.jsQR ? Promise.resolve() : (jsqrLoading ||= new Promise((ok, ko) => { const s = el('script', { src: '/ui/jsqr.min.js' }); s.onload = ok; s.onerror = () => ko(new Error('lecteur QR indisponible')); document.head.append(s); }));
async function makeReader() {
  if ('BarcodeDetector' in window) {
    try {
      if (!BarcodeDetector.getSupportedFormats || (await BarcodeDetector.getSupportedFormats()).includes('qr_code')) {
        const d = new BarcodeDetector({ formats: ['qr_code'] });
        return async src => (await d.detect(src))[0]?.rawValue || null;
      }
    } catch { /* repli sur jsQR */ }
  }
  await loadJsqr();
  const canvas = el('canvas'), ctx = canvas.getContext('2d', { willReadFrequently: true });
  return async src => {
    const w = src.videoWidth || src.width, h = src.videoHeight || src.height;
    if (!w || !h) return null;
    const k = Math.min(1, 720 / Math.max(w, h)); canvas.width = Math.round(w * k); canvas.height = Math.round(h * k);
    ctx.drawImage(src, 0, 0, canvas.width, canvas.height);
    const d = ctx.getImageData(0, 0, canvas.width, canvas.height);
    return window.jsQR(d.data, d.width, d.height, { inversionAttempts: 'attemptBoth' })?.data || null;
  };
}
async function startCamera() {
  if (st.cam) return;
  if (!navigator.mediaDevices?.getUserMedia) return hint(t('noCam'));
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: 'environment' }, width: { ideal: 1280 } }, audio: false });
    const video = $('#video'); video.srcObject = stream; await video.play();
    const read = await makeReader();
    st.cam = { stream, timer: 0 };
    $('#cam').hidden = false; $('#cam-on').hidden = true;
    const loop = async () => {
      if (!st.cam) return;
      try { const text = await read(video); if (text) onText(text); } catch { /* image sans QR : on continue */ }
      if (st.cam) st.cam.timer = setTimeout(loop, 160);
    };
    loop();
  } catch (e) {
    st.cam = null;
    hint(e && e.name === 'NotAllowedError' ? t('camDenied') : t('camFail'));
  }
}
function stopCamera() {
  if (!st.cam) return;
  clearTimeout(st.cam.timer); st.cam.stream.getTracks().forEach(t => t.stop()); st.cam = null;
  $('#video').srcObject = null; $('#cam').hidden = true; $('#cam-on').hidden = st.mode === 'dest';
}
function flash() { const c = $('#cam'); c.classList.add('hit'); setTimeout(() => c.classList.remove('hit'), 350); navigator.vibrate?.(40); }

// ── Ce que contient un QR ───────────────────────────────────────────────────────────────────────────────────────
function onText(text) {
  const now = Date.now();
  if (st.last[0] === text && now - st.last[1] < 2500) return; // la même image revient à chaque image de la vidéo
  st.last = [text, now];
  const c = classify(text);
  if (st.mode === 'dest') {
    if (c.kind !== 'address') return hint(t('hintNotAddr'));
    flash(); stopCamera(); $('#dest').value = c.value; checkDest(); return;
  }
  if (c.kind === 'part') { flash(); addPart(c.value); }
  else if (c.kind === 'address') { flash(); checkBillet(c.value); }
  else hint(t('hintNotBillet'));
}

function chips() {
  const items = [];
  if (st.billet) items.push(el('li', { class: 'ok' }, t('chipBillet')));
  for (let i = 0; i < 2; i++) items.push(el('li', { class: i < st.parts.length ? 'ok' : '' }, i < st.parts.length ? t('chipRead', i + 1) : t('chipTodo', i + 1)));
  $('#chips').replaceChildren(...items);
}
function card() {
  const v = st.billet, c = $('#card');
  if (!v) { c.hidden = true; return; }
  const ver = verdict(v), lines = [];
  if (ver.ok) {
    lines.push(el('div', { class: 'big' }, `${amount(v.zen_cents)} Ẑen`), el('p', {}, t('g1', amount(v.value_g1_cents))));
    lines.push(el('p', {}, v.authentic === true ? t('authentic') : t('unverified')));
    if (v.expires_at) lines.push(el('p', {}, t('until', new Date(v.expires_at * 1000).toLocaleDateString(locale, { day: 'numeric', month: 'long', year: 'numeric' }))));
  } else lines.push(el('p', {}, el('strong', {}, t(ver.key))));
  c.className = 'sc-card' + (ver.ok ? '' : ' bad'); c.replaceChildren(...lines); c.hidden = false;
}
function refresh() {
  chips(); card();
  const ready = st.verified && st.billet?.status === 'ready';
  $('#s1-next').hidden = !ready;
  if (ready) hint(t('hintReady'));
}
function resetParts(why) { st.parts = []; st.verified = false; chips(); hint(why); refresh(); }

async function checkBillet(addr) {
  hint(t('hintChecking'));
  try {
    st.billet = await call('check?g1pub=' + encodeURIComponent(addr)); st.verified = false; st.parts = [];
    refresh();
    hint(st.billet.status === 'ready' ? t('hintValue') : '');
  } catch (e) { hint(e.message); }
}
async function addPart(p) {
  if (st.parts.includes(p)) return hint(t('hintSame'));
  if (st.parts.length >= 2) return;
  st.parts.push(p); chips();
  if (st.parts.length < 2) return hint(t('hintFirst'));
  hint(t('hintChecking'));
  try {
    const known = st.billet?.g1pub, v = await call('identify', { parts: st.parts });
    if (known && v.g1pub !== known) return resetParts(t('hintMismatch'));
    st.billet = v; st.verified = true; refresh();
    if (v.status !== 'ready') hint('');
  } catch (e) { resetParts(e.message + ' ' + t('retry')); }
}

// ── Étape 2 : destination ───────────────────────────────────────────────────────────────────────────────────────
let destTimer = 0;
function checkDest() {
  clearTimeout(destTimer); $('#s2-next').disabled = true; st.destOk = false;
  const c = classify($('#dest').value), s = $('#dest-state');
  if (!$('#dest').value.trim()) { s.textContent = ''; return; }
  if (c.kind !== 'address') { s.textContent = t('destNotAddr'); return; }
  s.textContent = t('destChecking');
  destTimer = setTimeout(async () => {
    try {
      const r = await call('destination?g1pub=' + encodeURIComponent(c.value));
      st.destOk = r.ok; st.dest = r.g1pub; $('#s2-next').disabled = !r.ok;
      s.textContent = r.ok ? t('destOk', shortAddr(r.g1pub)) : t('destNo');
    } catch (e) { s.textContent = e.message; }
  }, 350);
}

// ── Raccourci « mon compte » : si ce navigateur a une session sur cette station, un geste remplit l'adresse de son portefeuille ───
async function mineFrom(basic) {
  try {
    const r = await fetch('/api/v1/account', { headers: { Authorization: 'Basic ' + basic }, cache: 'no-store' });
    const a = r.ok ? await r.json() : null, addr = a && a.wallet && classify(a.wallet).kind === 'address' ? a.wallet : ''; // le portefeuille Duniter du MULTIPASS, pas sa clé Nostr
    if (!addr) return false;
    const b = $('#dest-mine'); b.textContent = t('mine', a.email || ''); b.hidden = false; $('#login').hidden = true;
    b.onclick = () => { $('#dest').value = addr; checkDest(); };
    return true;
  } catch { return false; /* le raccourci est un confort : sans lui, on colle ou on scanne l'adresse */ }
}
// Session mémorisée par ce navigateur ; sinon (iPhone dont l'application installée a la sienne…) email + PASS, gardés en mémoire seulement.
async function showMine() {
  let basic = null;
  try { basic = localStorage.getItem('basic'); } catch { /* pas de session mémorisée */ }
  if (basic && await mineFrom(basic)) return;
  $('#login').hidden = false;
}
$('#login-go').onclick = async () => {
  const b = $('#login-go'), e = $('#login-e'); b.disabled = true; e.textContent = '';
  try {
    const r = await fetch('/api/v1/account/unlock', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: $('#u').value.trim(), pass: $('#p').value }) }), j = await r.json().catch(() => ({}));
    if (!r.ok) { e.textContent = (j.code && t(j.code) !== j.code && t(j.code)) || j.error || t('down'); return; }
    $('#p').value = '';
    if (!await mineFrom(btoa(j.email + ':' + j.token))) e.textContent = t('noWallet');
  } catch { e.textContent = t('down'); } finally { b.disabled = false; }
};

// ── Événements ──────────────────────────────────────────────────────────────────────────────────────────────────
$('#cam-on').onclick = () => { st.mode = 'billet'; startCamera(); };
$('#manual-go').onclick = () => { const t = $('#manual').value; if (t.trim()) { onText(t); $('#manual').value = ''; } };
$('#file').onchange = async ev => {
  const f = ev.target.files[0]; ev.target.value = '';
  if (!f) return;
  try {
    const read = await makeReader(), bmp = await createImageBitmap(f), text = await read(bmp);
    if (text) { st.last = ['', 0]; onText(text); } else hint(t('noQr'));
  } catch { hint(t('badImage')); }
};
$('#s1-next').onclick = () => { st.mode = 'dest'; show('s2'); $('#dest').focus(); };
$('#dest').oninput = checkDest;
$('#dest-cam').onclick = () => { st.mode = 'dest'; $('#dest-state').before($('#cam')); startCamera(); };
$('#s2-back').onclick = () => { stopCamera(); st.mode = 'billet'; $('#chips').before($('#cam')); show('s1'); };
$('#s2-next').onclick = () => {
  stopCamera(); const v = st.billet;
  $('#recap').replaceChildren(el('div', { class: 'big' }, `${amount(v.zen_cents)} Ẑen`), el('p', {}, t('sentTo', amount(v.value_g1_cents))), el('p', {}, el('strong', {}, shortAddr(st.dest))));
  $('#err').textContent = ''; show('s3');
};
$('#s3-back').onclick = () => show('s2');
$('#go').onclick = async () => {
  const b = $('#go'); b.disabled = true; $('#err').textContent = '';
  try {
    const r = await call('redeem', { parts: st.parts, dest: st.dest });
    st.parts = []; st.verified = false;
    $('#done-t').textContent = r.status === 'done' ? t('doneT', amount(r.zen_cents)) : t('pendT');
    $('#done-p').textContent = r.status === 'done' ? t('doneP', shortAddr(r.dest)) : t('pendP', shortAddr(r.dest));
    show('done');
  } catch (e) { $('#err').textContent = e.message; }
  finally { b.disabled = false; }
};
$('#again').onclick = () => { st.billet = null; st.parts = []; st.verified = false; st.mode = 'billet'; $('#dest').value = ''; $('#s2-next').disabled = true; $('#dest-state').textContent = ''; refresh(); show('s1'); startCamera(); };
addEventListener('pagehide', stopCamera);
showMine();

// ── Démarrage : le lien de vérification d'un billet porte son adresse (`/scan?g1pub=…`) ─────────────────────────
localize(); chips(); show('s1'); booted = true;
const first = new URLSearchParams(location.search).get('g1pub');
if (first && classify(first).kind === 'address') checkBillet(first);
else hint(t('hintFrame'));
startCamera();
