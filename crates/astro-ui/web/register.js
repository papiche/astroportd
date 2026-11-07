'use strict';
const $ = s => document.querySelector(s);
// Seuls l'identifiant et le PASS sont mis en avant ; le reste (clés) est replié dans « Détails techniques » et dans le .txt.
const FIELDS = [
  ['email', 'Identifiant de connexion (email)'],
  ['pass', 'Code PASS (connexion : identifiant + PASS)'],
  ['token', 'Jeton de connexion web (avec l’email)'],
  ['card', 'Clé de secours, part 1 sur 3 (seule, elle ne révèle rien ; avec la part d’un ami qui héberge votre sauvegarde, elle rétablit le compte sur une autre station)'],
  ['nsec', 'Clé secrète Nostr (nsec) : ne la montrez à personne'],
  ['npub', 'Clé publique Nostr (npub)'],
  ['g1pub', 'Portefeuille Ğ1'],
  ['nostrns', 'Espace IPNS'],
];
let card = null, captain = false;
const wtoken = location.pathname.startsWith('/w/') ? encodeURIComponent(location.pathname.slice(3)) : ''; // lien d'accueil de la station
$('#guest').onchange = ev => { const g = ev.target.checked; $('#email').required = !g; $('#email').hidden = g; document.querySelector('label[for=email]').hidden = g; $('#alias-box').hidden = !g; };
const msg = { 400: 'Adresse refusée : vérifiez-la.', 409: 'Cette adresse a déjà un MULTIPASS ici.', 429: 'Trop d’inscriptions depuis votre réseau. Patientez un peu.', 501: 'L’inscription est fermée sur cette station.' };
async function init() {
  try {
    if (wtoken) { // lien d'accueil : la discussion avec l'hôte s'ouvre à l'arrivée
      const w = await fetch('/api/v1/welcome/' + wtoken, { cache: 'no-store' }), v = await w.json().catch(() => ({}));
      if (!w.ok) { $('#info').textContent = v.error || 'Ce lien d’accueil ne sert plus.'; return; }
      document.title = $('h1').textContent = 'Rejoindre ' + v.from;
      $('.lead').textContent = v.from + ' vous accueille sur sa station' + (v.label ? ' (' + v.label + ')' : '') + '. Votre MULTIPASS se crée en un instant, et vous arrivez dans la discussion avec ' + v.from + '.';
      $('#const').textContent = v.constellation; $('#info').textContent = v.left + ' place(s) restante(s) sur ce lien.';
      $('#open').textContent = 'Ouvrir la discussion'; $('#f').hidden = false; $('#email').focus();
      return;
    }
    const r = await fetch('/api/v1/register', { cache: 'no-store' });
    const j = await r.json();
    $('#const').textContent = j.constellation || '—';
    if (r.ok && j.captain_setup) { // station neuve : le premier compte est celui du Capitaine, gardé par le code d'installation
      captain = true;
      document.title = $('h1').textContent = $('#go').textContent = 'Créer le compte du Capitaine';
      $('.lead').textContent = 'Cette station n’a pas encore de Capitaine. Ce compte l’administrera : il se crée une seule fois, avec le code d’installation.';
      $('#guest').parentElement.hidden = true; $('#code-box').hidden = false; $('#code').required = true;
      $('#info').textContent = 'Constellation ' + j.constellation + ' · première installation.'; $('#f').hidden = false; $('#code').focus();
    } else if (r.ok && j.open) { $('#info').textContent = 'Constellation ' + j.constellation + ' · inscription ouverte.'; $('#f').hidden = false; $('#email').focus(); }
    else { // complète ou fermée : renvoi vers l'annuaire des stations de l'UPlanet
      $('#info').textContent = (j.error || 'Inscription fermée sur cette station.') + ' ';
      const a = document.createElement('a'); a.href = '/stations'; a.textContent = 'Trouver une station proche qui accueille encore →'; $('#info').append(a);
    }
  } catch { $('#info').textContent = 'Station injoignable.'; }
}
$('#f').onsubmit = async ev => {
  ev.preventDefault();
  const b = $('#go'); b.disabled = true; $('#err').textContent = '';
  try {
    const r = await fetch(wtoken ? '/api/v1/welcome/' + wtoken : '/api/v1/register', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(captain ? { setup_code: $('#code').value.trim(), email: $('#email').value.trim(), lang: $('#lang').value } : $('#guest').checked ? { guest: true, alias: $('#alias').value.trim() || undefined, lang: $('#lang').value } : { email: $('#email').value.trim(), lang: $('#lang').value }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#err').textContent = (j.error && r.status !== 429 ? j.error : msg[r.status]) || j.error || 'Échec de l’inscription.'; return; }
    card = j;
    const dl = $('#card');
    dl.replaceChildren();
    $('#k-email').textContent = j.email || '';
    $('#k-pass').textContent = j.pass || '';
    $('#k-pass').parentElement.querySelectorAll('dt')[1].hidden = $('#k-pass').hidden = !j.pass;
    for (const [k, label] of FIELDS) {
      if (!j[k] || k === 'email' || k === 'pass') continue;
      const dt = document.createElement('dt'), dd = document.createElement('dd');
      dt.textContent = label; dd.textContent = j[k];
      dl.append(dt, dd);
    }
    // QR de la clé de secours (part 1), rendu par la station (SVG autonome) : lu comme document SVG, jamais inséré comme HTML.
    const qr = $('#qr');
    qr.querySelector('svg')?.remove();
    const svg = j.card_svg && new DOMParser().parseFromString(j.card_svg, 'image/svg+xml').documentElement;
    if (svg && svg.nodeName === 'svg') { qr.append(document.importNode(svg, true)); qr.hidden = false; }
    $('#f').hidden = true; $('#done').hidden = false; $('#done-t').focus();
  } catch { $('#err').textContent = 'Station injoignable.'; } finally { b.disabled = false; }
};
// Quartier : la position du navigateur, arrondie par la station à 0,01° (≈ 1 km). Rien n'est envoyé sans ce geste.
$('#geo-go').onclick = () => {
  const st = $('#geo-st'), b = $('#geo-go');
  if (!card || !navigator.geolocation) { st.textContent = 'Position indisponible sur cet appareil.'; return; }
  b.disabled = true; st.textContent = 'Recherche de votre position…';
  navigator.geolocation.getCurrentPosition(async p => {
    try {
      const r = await fetch('/api/v1/account/position', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json', Authorization: 'Basic ' + btoa(card.email + ':' + card.token) }, body: JSON.stringify({ lat: p.coords.latitude, lon: p.coords.longitude }) });
      const j = await r.json().catch(() => ({}));
      st.textContent = r.ok ? 'Quartier enregistré ✓' : (j.error || 'Échec de l’enregistrement.');
      if (!r.ok) b.disabled = false;
    } catch { st.textContent = 'Station injoignable.'; b.disabled = false; }
  }, e => { st.textContent = e.code === 1 ? 'Vous avez refusé la position : vous pourrez l’indiquer plus tard.' : 'Position introuvable.'; b.disabled = false; }, { enableHighAccuracy: false, timeout: 15000, maximumAge: 600000 });
};
$('#print').onclick = () => window.print();
$('#save').onclick = () => {
  if (!card) return;
  const text = 'MULTIPASS ' + card.constellation + '\n' + FIELDS.filter(([k]) => card[k]).map(([k, l]) => l + '\n  ' + card[k]).join('\n') + '\n\nGardez ce fichier hors ligne. Il ne sera plus jamais affiché.\n';
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
  a.download = 'multipass-' + card.email + '.txt';
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
};
$('#open').onclick = () => {
  if (!card) return;
  try { localStorage.setItem('basic', btoa(card.email + ':' + card.token)); } catch { /* connexion manuelle */ }
  location.href = card.chat_peer ? '/#/chat?peer=' + card.chat_peer : '/';
};
init();
