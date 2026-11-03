'use strict';
// Visio : appel à deux ou en groupe (4 personnes au plus), de navigateur à navigateur (WebRTC, maillage : une connexion par
// paire). La station ne transporte que la sonnerie et la poignée de main, chiffrées comme un message privé (POST
// /api/v1/call/signal, reçues par le flux en direct) ; le son et l'image vont en direct, ou par le relais de la station de
// l'hôte. Un seul appel à la fois. Groupe : l'hôte invite ; qui arrive annonce « join » et ceux qui sont là lui font une offre
// (jamais deux offres croisées). Chaque côté rassemble ses adresses avant d'envoyer (2,5 s au plus).
const RING_MS = 45000, ICE_MS = 2500, MAX_ROOM = 4;
let K, C = null; // l'appel : { id, host, mine, me, roster, video, ice, joined, since, stream, peers, pending, ui, card, timers }

const rid = () => [...crypto.getRandomValues(new Uint8Array(8))].map(b => b.toString(16).padStart(2, '0')).join('');
const send = (peer, body) => K.api('/api/v1/call/signal', { method: 'POST', body: { peer, ...body } });
const css = () => { if (!document.querySelector('link[href="/ui/m-call.css"]')) document.head.append(K.h('link', { rel: 'stylesheet', href: '/ui/m-call.css' })); };
const myHex = async () => (await K.api('/api/v1/me')).hex;

// Serveurs de traversée : STUN de la station, son relais (identifiants pour ce membre seul) et, pour un invité, le relais de la
// station de l'hôte (`extra`, reçu dans l'offre chiffrée : réservé aux membres et à leurs amis N¹ ; tout le groupe s'y retrouve).
async function iceServers(extra) {
  let r = { servers: [] };
  try { r = await K.api('/api/v1/call/ice'); } catch { /* sans station joignable : adresses directes seulement */ }
  const at = `${location.hostname}:${r.stun_port}`;
  return [...(r.stun_port ? [{ urls: `stun:${at}` }] : []), ...(r.stun_port && r.turn ? [{ urls: `turn:${at}?transport=udp`, username: r.turn.username, credential: r.turn.credential }] : []),
    ...r.servers.map(u => ({ urls: u })), ...(extra ? [extra] : [])];
}
const gathered = pc => new Promise(ok => {
  if (pc.iceGatheringState === 'complete') return ok();
  const t = setTimeout(ok, ICE_MS);
  pc.addEventListener('icegatheringstatechange', () => { if (pc.iceGatheringState === 'complete') { clearTimeout(t); ok(); } });
});
async function media(video) {
  // Hors https (station sans domaine ouverte depuis le réseau local), le navigateur n'offre ni caméra ni micro : on dit pourquoi.
  if (!window.isSecureContext || !navigator.mediaDevices) throw new Error(K.t('La visio demande une adresse sécurisée (https) : le Capitaine l\'active avec un nom de domaine (astroportd init --domain).'));
  try { return await navigator.mediaDevices.getUserMedia({ audio: true, video }); } catch { /* la caméra manque peut-être : le son seul */ }
  try { return await navigator.mediaDevices.getUserMedia({ audio: true }); } catch { throw new Error(K.t('Micro refusé : autorisez-le dans votre navigateur pour appeler.')); }
}

// Sonnerie : deux notes douces toutes les deux secondes (sans fichier son), vibration, titre qui clignote, notification système.
function ringer(label) {
  const title = document.title, flash = setInterval(() => { document.title = document.title === title ? '📹 ' + K.t('{0} vous appelle', label) : title; }, 1000);
  let note = null;
  if (document.hidden && window.Notification?.permission === 'granted') {
    try { note = new Notification(K.t('{0} vous appelle', label), { body: K.t('Appel visio : touchez pour répondre'), tag: 'astroport-call', requireInteraction: true }); note.onclick = () => { window.focus(); note.close(); }; } catch { /* notifications indisponibles */ }
  }
  const stopSeen = () => { clearInterval(flash); document.title = title; note?.close(); };
  let ctx; try { ctx = new AudioContext(); ctx.resume?.().catch(() => {}); } catch { return stopSeen; }
  const beep = () => [0, .25].forEach(dt => { const o = ctx.createOscillator(), g = ctx.createGain(); o.frequency.value = dt ? 660 : 880; g.gain.value = .08; o.connect(g).connect(ctx.destination); o.start(ctx.currentTime + dt); o.stop(ctx.currentTime + dt + .18); });
  beep(); navigator.vibrate?.([300, 200, 300]);
  const i = setInterval(() => { beep(); navigator.vibrate?.([300, 200, 300]); }, 2000);
  return () => { clearInterval(i); navigator.vibrate?.(0); ctx.close().catch(() => {}); stopSeen(); };
}

// Présence : « en appel » pour les amis qui me voient (sans effet si je n'ai pas choisi d'être joignable).
const inCall = on => K.api('/api/v1/presence', { method: 'POST', body: { in_call: on } }).catch(() => {});
// Les notifications d'appel se demandent sur un geste (jamais à froid) : au premier appel passé, ou dans Mon compte.
const askNotify = () => { if (window.Notification?.permission === 'default') Notification.requestPermission().catch(() => {}); };
export { askNotify };

// Un bouton rond toujours accompagné de son nom : un pictogramme seul se lit mal (décrocher et raccrocher se ressemblent).
const ctl = (btn, label) => K.h('span', { class: 'call-ctl' }, btn, K.h('small', { 'aria-hidden': 'true' }, label));
const names = () => [...C.peers.values()].map(p => p.label).join(', ');
function status(text) { if (C?.ui) C.ui.status.textContent = text; }
function layout() {
  if (!C?.ui) return;
  C.ui.grid.className = 'call-grid n' + Math.max(1, C.peers.size);
  C.ui.title.textContent = names();
  C.ui.add.hidden = !C.mine || C.peers.size + 1 >= MAX_ROOM;
}

// Écran d'appel plein écran : une tuile par personne (son nom, son état), sa propre image en vignette, gros boutons au pouce.
function screen() {
  const { h, t } = K;
  const local = h('video', { class: 'call-local', autoplay: true, playsInline: true, muted: true });
  const mic = h('button', { type: 'button', class: 'call-btn', 'aria-pressed': 'false', 'aria-label': t('Couper le micro'), onclick: () => toggle('audio', mic) }, '🎙');
  const cam = h('button', { type: 'button', class: 'call-btn', 'aria-pressed': 'false', 'aria-label': t('Couper la caméra'), onclick: () => toggle('video', cam) }, '📷');
  const radio = h('button', { type: 'button', class: 'call-btn', 'aria-pressed': 'false', 'aria-label': t('Voix seule : couper la vidéo dans les deux sens'), onclick: () => voiceOnly(radio, cam) }, '📻');
  const add = ctl(h('button', { type: 'button', class: 'call-btn', 'aria-label': t('Ajouter quelqu\'un à l\'appel'), onclick: pick }, '＋'), t('Ajouter'));
  const grid = h('div', { class: 'call-grid n1' }), title = h('strong', {}), statusEl = h('p', { class: 'call-status', role: 'status' });
  const el = h('section', { class: 'call', 'aria-label': t('Appel visio') }, grid, local, h('header', { class: 'call-top' }, title, statusEl),
    h('div', { class: 'call-bar' }, ctl(mic, t('Micro')), ctl(cam, t('Caméra')), ctl(radio, t('Voix seule')), add, ctl(h('button', { type: 'button', class: 'call-btn call-end call-hang', 'aria-label': t('Raccrocher'), onclick: hangup }, '✆'), t('Raccrocher'))));
  document.body.append(el); document.body.classList.add('in-call');
  local.srcObject = C.stream;
  C.ui = { el, grid, title, add, status: statusEl };
  layout();
}
function toggle(kind, btn) {
  const tracks = C?.stream.getTracks().filter(tr => tr.kind === kind) || [];
  if (!tracks.length) return K.toast(kind === 'video' ? K.t('Pas de caméra pour cet appel') : K.t('Pas de micro'));
  const off = tracks[0].enabled; tracks.forEach(tr => { tr.enabled = !off; });
  btn.setAttribute('aria-pressed', String(off)); btn.classList.toggle('off', off);
}

// Voix seule (réseau faible) : ma caméra se coupe et chaque pair cesse de m'envoyer la sienne (signal `media`, appliqué
// par `setParameters` sans renégocier). Le son seul passe sous 64 kbit/s. Un second appui rend l'image.
function voiceOnly(btn, cam) {
  C.radio = !C.radio;
  const vids = C.stream.getVideoTracks();
  if (C.radio) { C.camWas = vids.some(tr => tr.enabled); vids.forEach(tr => { tr.enabled = false; }); } else if (C.camWas) vids.forEach(tr => { tr.enabled = true; });
  cam.setAttribute('aria-pressed', String(!vids.some(tr => tr.enabled))); cam.classList.toggle('off', !vids.some(tr => tr.enabled));
  btn.setAttribute('aria-pressed', String(C.radio)); btn.classList.toggle('off', C.radio);
  C.ui?.grid.classList.toggle('radio', C.radio);
  for (const hex of C.peers.keys()) send(hex, { type: 'media', call: C.id, video: !C.radio }).catch(() => {});
}
// Un pair passe en voix seule : je cesse de lui envoyer ma vidéo (ou je la reprends).
function sendVideo(p, on) {
  for (const s of p?.pc?.getSenders() || []) {
    if (s.track?.kind !== 'video') continue;
    const prm = s.getParameters();
    (prm.encodings || []).forEach(e => { e.active = on; });
    s.setParameters(prm).catch(() => {});
  }
}

// Une personne de l'appel : sa tuile et sa connexion.
async function addPeer(hex, label) {
  const { h } = K, video = h('video', { class: 'call-remote', autoplay: true, playsInline: true }), state = h('small', {});
  const tile = h('figure', { class: 'call-tile' }, video, h('figcaption', {}, h('strong', {}, label), ' ', state));
  const p = { hex, label, video, state, tile, timers: {} };
  C.peers.set(hex, p); C.ui?.grid.append(tile); layout();
  const pc = new RTCPeerConnection({ iceServers: await iceServers(C.ice) });
  C.stream.getTracks().forEach(tr => pc.addTrack(tr, C.stream));
  pc.ontrack = ev => { video.srcObject = ev.streams[0]; };
  pc.onconnectionstatechange = () => {
    if (C?.peers.get(hex) !== p) return;
    if (pc.connectionState === 'connected' && !p.since) {
      p.since = Date.now(); clearTimeout(p.timers.ring); state.textContent = '';
      if (!C.since) { C.since = Date.now(); inCall(true); C.timers.clock = setInterval(() => { const s = Math.floor((Date.now() - C.since) / 1000); status(K.t('En ligne · {0}', `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`)); }, 1000); }
    } else if (pc.connectionState === 'disconnected') state.textContent = K.t('Connexion perdue, reconnexion…');
    else if (pc.connectionState === 'failed') {
      send(hex, { type: 'hangup', call: C.id }).catch(() => {});
      drop(hex, C.peers.size > 1 ? K.t('{0} : pas de chemin entre vos réseaux', label) : K.t('L\'appel n\'a pas pu s\'établir : aucun chemin entre vos deux réseaux, même par le relais de la station. Essayez en Wi-Fi, ou plus tard.'));
    }
  };
  p.pc = pc;
  if (C.radio) send(hex, { type: 'media', call: C.id, video: false }).catch(() => {});
  return p;
}
// Offre à une personne (l'hôte joint la liste des participants : la station de l'invité les admettra).
async function offer(p, extra) {
  await p.pc.setLocalDescription(await p.pc.createOffer());
  await gathered(p.pc);
  if (C?.peers.get(p.hex) !== p) return;
  await send(p.hex, { type: 'offer', call: C.id, sdp: p.pc.localDescription.sdp, video: C.video, turn_host: location.hostname, ...extra });
}
async function answer(p, sdp) {
  await p.pc.setRemoteDescription({ type: 'offer', sdp });
  await p.pc.setLocalDescription(await p.pc.createAnswer());
  await gathered(p.pc);
  if (C?.peers.get(p.hex) === p) await send(p.hex, { type: 'answer', call: C.id, sdp: p.pc.localDescription.sdp });
}
// L'hôte invite : sonnerie de 45 s, puis « ne répond pas » (et « Appel manqué » chez lui).
async function invite(hex, label) {
  const p = await addPeer(hex, label);
  p.invited = true; p.state.textContent = K.t('Appel…');
  p.timers.ring = setTimeout(() => { if (C?.peers.get(hex) === p && !p.since) { send(hex, { type: 'hangup', call: C.id, missed: true }).catch(() => {}); drop(hex, K.t('{0} ne répond pas', label)); } }, RING_MS);
  await offer(p, { roster: C.roster });
}

function drop(hex, message) {
  const p = C?.peers.get(hex);
  if (!p) return;
  Object.values(p.timers).forEach(clearTimeout);
  p.pc?.close(); p.tile.remove(); C.peers.delete(hex);
  if (!C.peers.size) return end(message);
  if (message) K.toast(message);
  layout();
}
function end(message) {
  if (!C) return;
  const c = C; C = null;
  if (c.since) inCall(false);
  Object.values(c.timers).forEach(x => { clearTimeout(x); clearInterval(x); });
  c.stopRing?.();
  c.peers.forEach(p => { Object.values(p.timers).forEach(clearTimeout); p.pc?.close(); });
  c.stream?.getTracks().forEach(tr => tr.stop());
  c.ui?.el.remove(); c.card?.remove(); c.picker?.remove();
  document.body.classList.remove('in-call');
  if (message) K.toast(message);
}
function hangup() {
  if (!C) return;
  // Raccroché avant qu'un invité décroche : il garde « Appel manqué » dans la conversation.
  C.peers.forEach(p => send(p.hex, { type: 'hangup', call: C.id, ...(p.invited && !p.since && { missed: true }) }).catch(() => {}));
  end();
}

// « Ajouter » : un proche de plus (l'hôte seul ; la liste mise à jour part d'abord vers ceux qui sont déjà là).
async function pick() {
  const { h, t } = K;
  let friends = [];
  try { friends = (await K.api('/api/v1/friends')).friends.filter(f => f.hex !== C.me && !C.peers.has(f.hex)); } catch (e) { return K.toast(e.message); }
  if (!friends.length) return K.toast(t('Aucun autre proche à ajouter'));
  const d = h('dialog', { class: 'call-pick', 'aria-label': t('Ajouter quelqu\'un à l\'appel') }, h('h2', {}, t('Ajouter quelqu\'un à l\'appel')),
    h('div', { class: 'who-list' }, friends.slice(0, 50).map(f => h('button', { type: 'button', class: 'ghost', onclick: async () => {
      d.close();
      if (!C || C.peers.size + 1 >= MAX_ROOM) return K.toast(t('Appel de groupe : {0} personnes au plus', MAX_ROOM));
      C.roster = [...C.roster, f.hex];
      await Promise.all([...C.peers.keys()].map(x => send(x, { type: 'roster', call: C.id, roster: C.roster }).catch(() => {})));
      invite(f.hex, f.label).catch(e => K.toast(e.message));
    } }, f.label))),
    h('menu', {}, h('button', { type: 'button', class: 'ghost', onclick: () => d.close() }, t('Annuler'))));
  d.addEventListener('close', () => d.remove());
  C.picker = d; document.body.append(d); d.showModal();
}

// Appeler `peer` (clé du proche) : écran tout de suite, la sonnerie part dès que les adresses sont prêtes.
export async function call(kit, peer, label, video = true) {
  K = kit; css();
  if (C) return K.toast(K.t('Un appel est déjà en cours'));
  askNotify();
  C = { id: rid(), mine: true, video, joined: true, peers: new Map(), timers: {} };
  try {
    C.me = C.host = await myHex();
    C.roster = [C.me, peer];
    C.stream = await media(video);
    if (!C) return;
    screen(); status(K.t('Appel de {0}…', label));
    await invite(peer, label);
  } catch (e) { end(e.message); }
}

async function accept() {
  const c = C, o = c.pending;
  c.stopRing?.(); c.card?.remove(); c.card = null;
  try {
    c.me = await myHex();
    c.stream = await media(c.video);
    if (C !== c) return;
    c.joined = true;
    screen(); status(K.t('Connexion…'));
    await answer(await addPeer(o.from, o.label), o.sdp);
    // Groupe : j'annonce mon arrivée aux autres participants ; ceux qui sont déjà là m'appelleront.
    c.roster.filter(x => x !== c.me && x !== o.from).forEach(x => send(x, { type: 'join', call: c.id }).catch(() => {}));
  } catch (e) { send(o.from, { type: 'hangup', call: c.id }).catch(() => {}); end(e.message); }
}
function decline() { send(C.pending.from, { type: 'reject', call: C.id }).catch(() => {}); end(); }

// Un signal reçu par le flux en direct de la station (`call:<type>`, `data` = { type, call, from, label, sdp, ice, roster,
// friend, video, at }). `friend` : l'expéditeur est un proche ; un participant qui ne l'est pas n'est admis que dans la salle en cours.
export function signal(kit, d) {
  K = kit; css();
  if (!d || Date.now() / 1000 - d.at > 60) return; // signal rejoué ou périmé
  const { h, t } = K, here = C && C.id === d.call, p = here ? C.peers.get(d.from) : null;
  if (d.type === 'offer') {
    if (here && C.joined && !p && (d.friend || C.roster?.includes(d.from))) { // un participant me rejoint (il est arrivé avant moi)
      addPeer(d.from, d.label).then(q => answer(q, d.sdp)).catch(e => K.toast(e.message));
      return;
    }
    if (C) { if (!here) send(d.from, { type: 'busy', call: d.call }).catch(() => {}); return; }
    if (d.friend === false) return; // seul un proche fait sonner
    C = { id: d.call, host: d.from, mine: false, video: d.video !== false, ice: d.ice || null, roster: d.roster || [d.from], peers: new Map(), pending: { from: d.from, label: d.label, sdp: d.sdp }, timers: {} };
    const others = (d.roster || []).length > 2 ? t('vous invite à un appel de groupe') : d.video === false ? t('vous appelle') : t('vous appelle en visio');
    C.card = h('section', { class: 'call-ring', role: 'alertdialog', 'aria-label': t('{0} vous appelle', d.label) },
      h('div', {}, h('strong', {}, d.label), h('p', { class: 'muted' }, others)),
      ctl(h('button', { type: 'button', class: 'call-btn call-end', 'aria-label': t('Refuser'), onclick: decline }, '✕'), t('Refuser')),
      ctl(h('button', { type: 'button', class: 'call-btn call-ok', 'aria-label': t('Décrocher'), onclick: accept }, '✆'), t('Décrocher')));
    document.body.append(C.card); C.card.querySelector('.call-ok').focus();
    C.stopRing = ringer(d.label);
    C.timers.ring = setTimeout(() => end(), RING_MS);
    return;
  }
  if (!here) return;
  if (d.type === 'join') {
    if (C.joined && !p && C.roster?.includes(d.from)) addPeer(d.from, d.label).then(q => offer(q)).catch(e => K.toast(e.message)); // j'étais là : je l'appelle
  } else if (d.type === 'roster') {
    if (d.from === C.host && d.roster) C.roster = d.roster;
  } else if (d.type === 'handled') {
    if (!C.joined) end(t('Appel pris sur un autre écran')); // cet onglet sonnait encore : un autre a répondu
  } else if (d.type === 'answer') {
    p?.pc.setRemoteDescription({ type: 'answer', sdp: d.sdp }).then(() => { p.state.textContent = t('Connexion…'); if (!C.since) status(t('Connexion…')); }, e => drop(d.from, e.message));
  } else if (d.type === 'hangup') {
    if (!C.joined) { if (d.from === C.host) end(t('Appel manqué de {0}', d.label)); }
    else drop(d.from, C.peers.size > 1 ? t('{0} a quitté l\'appel', d.label) : t('{0} a raccroché', d.label));
  } else if (d.type === 'reject') drop(d.from, t('{0} ne peut pas répondre pour l\'instant', d.label));
  else if (d.type === 'busy') drop(d.from, t('{0} est déjà en ligne', d.label));
  else if (d.type === 'media') sendVideo(p, d.video !== false);
}
