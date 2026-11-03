'use strict';
const $ = s => document.querySelector(s);
const FIELDS = [
  ['email', 'Identifiant de connexion (email)'],
  ['token', 'Jeton de connexion web (avec l’email)'],
  ['pass', 'Code PASS (connexion sur téléphone : email + PASS)'],
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
    else $('#info').textContent = j.error || 'Inscription fermée sur cette station.';
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
    for (const [k, label] of FIELDS) {
      if (!j[k]) continue;
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
