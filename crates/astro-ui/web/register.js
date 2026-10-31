'use strict';
const $ = s => document.querySelector(s);
const FIELDS = [
  ['email', 'Identifiant de connexion (email)'],
  ['token', 'Jeton de connexion web (avec l’email)'],
  ['pass', 'Code PASS (connexion sur téléphone : email + PASS)'],
  ['salt', 'SALT (secret de récupération 1/2)'],
  ['pepper', 'PEPPER (secret de récupération 2/2)'],
  ['card', 'Carte secrète (rétablit le compte sur une autre station) : ne la montrez à personne'],
  ['nsec', 'Clé secrète Nostr (nsec) : ne la montrez à personne'],
  ['npub', 'Clé publique Nostr (npub)'],
  ['g1pub', 'Portefeuille Ğ1'],
  ['nostrns', 'Espace IPNS'],
];
let card = null;
$('#guest').onchange = ev => { const g = ev.target.checked; $('#email').required = !g; $('#email').hidden = g; document.querySelector('label[for=email]').hidden = g; $('#alias-box').hidden = !g; };
const msg = { 400: 'Adresse refusée : vérifiez-la.', 409: 'Cette adresse a déjà un MULTIPASS ici.', 429: 'Trop d’inscriptions depuis votre réseau. Patientez un peu.', 501: 'L’inscription est fermée sur cette station.' };
async function init() {
  try {
    const r = await fetch('/api/v1/register', { cache: 'no-store' });
    const j = await r.json();
    $('#const').textContent = j.constellation || '—';
    if (r.ok && j.open) { $('#info').textContent = 'Constellation ' + j.constellation + ' · inscription ouverte.'; $('#f').hidden = false; $('#email').focus(); }
    else $('#info').textContent = j.error || 'Inscription fermée sur cette station.';
  } catch { $('#info').textContent = 'Station injoignable.'; }
}
$('#f').onsubmit = async ev => {
  ev.preventDefault();
  const b = $('#go'); b.disabled = true; $('#err').textContent = '';
  try {
    const r = await fetch('/api/v1/register', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify($('#guest').checked ? { guest: true, alias: $('#alias').value.trim() || undefined, lang: $('#lang').value } : { email: $('#email').value.trim(), lang: $('#lang').value }) });
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
    // QR de la carte secrète, rendu par la station (SVG autonome) : lu comme document SVG, jamais inséré comme HTML.
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
  location.href = '/';
};
init();
