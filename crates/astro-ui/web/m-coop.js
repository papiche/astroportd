'use strict';
// Configuration coopérative (Capitaine) : kind 30800 `cooperative-config`, partagé par toute la constellation et lisible par
// les stations legacy. Les valeurs en clair s'éditent ; les SECRETS sont en écriture seule : on voit s'ils sont renseignés,
// jamais leur valeur, et on les remplace ou on les efface. Seules les clés modifiées sont envoyées.
const LABEL = { UPLANET_NAME: 'Nom de la constellation', CURRENCY_NAME: 'Nom de la monnaie (vide : celui d’origine)', CURRENCY_SYMBOL: 'Symbole de la monnaie (vide : celui d’origine)', UI_STYLE: 'Style par défaut des membres', CHARTER_VERSION: 'Version de la charte', CHARTER_TEXT: 'Texte de la charte (signé par la coopérative)', TVA_RATE: 'TVA (%)', IS_RATE_REDUCED: 'Taux réduit (%)', IS_RATE_NORMAL: 'Taux normal (%)', IS_THRESHOLD: 'Seuil du taux réduit (Ẑ par an)', ZENCARD_SATELLITE: 'ZenCard Satellite (€)', ZENCARD_CONSTELLATION: 'ZenCard Constellation (€)', PAF: 'PAF proposée aux nouvelles stations (Ẑ/semaine)', NCARD: 'NCARD (Ẑ/semaine)', ZCARD: 'ZCARD (Ẑ/semaine)', TREASURY_PERCENT: 'Trésorerie (%)', RND_PERCENT: 'R&D (%)', ASSETS_PERCENT: 'Actifs (%)', CAPTAIN_BONUS_PERCENT: 'Prime du Capitaine (%)', OCSLUG: 'Identifiant du collectif', OCAPIKEY: 'Clé d\'API OpenCollective', OC_AUTO_CREDIT: 'Créditer les dons', OC_URL_CLOUD: 'Page Contribuer : bouton « Cloud »', OC_URL_MEMBRE: 'Adresse d\'adhésion (« Membre résident »)', OC_URL_SATELLITE: 'Page Contribuer : bouton « Satellite »', OC_URL_CONSTELLATION: 'Page Contribuer : bouton « Constellation (GPU) »', TIER_SLUG_SATELLITE: 'Paliers Satellite', TIER_SLUG_CONSTELLATION: 'Paliers Constellation', TIER_SLUG_LABO: 'Paliers Labo', TIER_SLUG_CLOUD: 'Paliers Cloud', PLANTNET_API_KEY: 'Clé PlantNet', GIT_HOST: 'Forge (vide : GitHub)', GIT_TOKEN: 'Jeton de la forge', GIT_OWNER: 'Organisation ou compte', MJ_APIKEY_PUBLIC: 'Mailjet : clé publique', MJ_APIKEY_PRIVATE: 'Mailjet : clé privée', MJ_SENDER_EMAIL: 'Mailjet : expéditeur', OVH_APP_KEY: 'OVH : clé d\'application', OVH_APP_SECRET: 'OVH : secret', OVH_CONSUMER_KEY: 'OVH : clé de consommateur', OVH_ZONE: 'Domaine de la coopérative (zone DNS chez OVH)', P2P_RELAYS: 'Stations relais (une adresse par ligne)', ORIGIN_RELAY: 'Relais de la carte des UPlanet (un par ligne)' };
// Exemples de saisie.
const HINT = { CURRENCY_NAME: 'Sol, Graine…', CURRENCY_SYMBOL: '☉', OVH_ZONE: 'copylaradio.com', OCSLUG: 'monnaie-libre', GIT_HOST: 'https://gitlab.exemple.org', GIT_OWNER: 'papiche', TIER_SLUG_SATELLITE: '*parrainage*128*,satellite', TIER_SLUG_CONSTELLATION: '*parrainage*gpu*,constellation', TIER_SLUG_LABO: 'labo,r-d,recherche', OC_URL_MEMBRE: 'https://opencollective.com/…/contribute/…' };

const TITLE = { uplanet: 'Nom et charte de la constellation', look: 'Monnaie et apparence', ovh: 'Domaine de la coopérative (OVH)', mesh: 'Stations relais du maillage', oc: 'OpenCollective', tiers: 'Routage des paliers', '3x13': 'Règle 3×1/3 + 1 %', fiscal: 'Impôt sur les sociétés', shares: 'PAF suggérée', api: 'Retours des utilisateurs (Git)', plantnet: 'Reconnaissance des plantes (Pl@ntNet)', legacy: 'Stations Astroport.ONE' };
// Où trouver chaque valeur : une ancre par section (`id` de `coop_config::SECTIONS`) dans l'annexe publiée du guide.
const DOC = 'https://papiche.github.io/astroportd/CONFIG_COOP.html#';
// Par tâche : ce qui met la station en service d'abord ; le reste replié. [niveau, titre, pourquoi, ouvert]
const GROUPS = [
  ['essential', 'Mettre en service', ''],
  ['money', 'Recevoir les dons', 'Un don devient des Ẑen sur le MULTIPASS de même adresse e-mail.', true],
  ['rules', 'Règles communes', 'Communes à toute la constellation ; valent dès la clôture suivante.'],
  ['services', 'Services aux membres', 'Bogues et idées des applications → tickets Git ; sans forge, journal de la station. Avec une clé Pl@ntNet, chaque membre peut faire reconnaître une plante sur ses photos.'],
  ['legacy', 'Pour les stations Astroport.ONE', 'Lues seulement par les stations Astroport.ONE : laissez vide sinon.'],
];

export default async function coop(box, K) {
  const { h, t } = K;
  box.replaceChildren(h('h2', {}, t('Configuration coopérative')), h('p', { class: 'muted' }, t('Chargement…')));
  try {
    const v = await K.api('/api/v1/station/coop'), inputs = new Map();
    const PCT = ['TREASURY_PERCENT', 'RND_PERCENT', 'ASSETS_PERCENT', 'CAPTAIN_BONUS_PERCENT'];
    const pct = () => PCT.reduce((n, k) => n + (parseFloat(inputs.get(k)?.value) || 0), 0);
    const warn = h('p', { class: 'degraded', role: 'status' });
    // Total faux : toujours signalé ; la publication n'est bloquée que si CES champs ont été modifiés (sinon un total déjà faux
    // empêcherait de corriger autre chose, comme la zone OVH).
    const check = () => {
      const p = Math.round(pct() * 100) / 100, off = inputs.has('TREASURY_PERCENT') && Math.abs(p - 100) > 0.001;
      const touched = PCT.some(k => inputs.get(k) && inputs.get(k).input.value.trim() !== inputs.get(k).orig);
      warn.textContent = off ? t('La règle 3×1/3 + 1 % additionne {0} % (100 % attendus).', p) + (touched ? ' ' + t('Publication impossible tant que le total n\'est pas 100 %.') : '') : '';
      go.disabled = off && touched;
      if (off) box.querySelector('.coop-rules')?.setAttribute('open', '');
    };
    const field = f => {
      const id = 'cf-' + f.key, label = h('label', { for: id }, t(LABEL[f.key] || f.key));
      if (f.secret) {
        const input = h('input', { id, secret: true, placeholder: f.set ? (f.readable ? t('● renseigné — saisir pour remplacer') : t('● renseigné mais illisible ici (autre constellation ?)')) : t('non renseigné') });
        const clear = f.set ? h('label', { class: 'muted' }, h('input', { type: 'checkbox', id: id + '-x' }), ' ' + t('effacer')) : null;
        inputs.set(f.key, { input, clear: clear?.querySelector('input'), secret: true });
        // Clé que la station peut surdéfinir pour elle seule (quota de la coopérative atteint) : jamais publiée.
        if (f.station === undefined) return h('div', {}, label, input, clear);
        const sid = id + '-st', own = h('input', { id: sid, secret: true, placeholder: f.station ? t('● clé de la station en service — saisir pour remplacer') : t('vide : la clé de la coopérative sert') });
        const drop = f.station ? h('label', { class: 'muted' }, h('input', { type: 'checkbox' }), ' ' + t('revenir à la clé de la coopérative')) : null;
        inputs.set('station:' + f.key, { input: own, clear: drop?.querySelector('input'), secret: true });
        return h('div', {}, label, input, clear, h('label', { for: sid }, t('Clé de cette station seulement (prioritaire, jamais publiée)')), own, drop);
      }
      const val = f.value ?? '';
      const input = f.key === 'CHARTER_TEXT' ? h('textarea', { id, rows: 14, maxlength: 6000, placeholder: t('Laisser vide : la charte type est proposée aux membres') }, val)
        : f.key === 'P2P_RELAYS' ? h('textarea', { id, rows: 3, maxlength: 1024, placeholder: '/dns4/relais.exemple.org/tcp/4101/p2p/12D3Koo…\n/ip4/203.0.113.7/tcp/4101/p2p/12D3Koo…' }, val)
        : f.key === 'ORIGIN_RELAY' ? h('textarea', { id, rows: 2, maxlength: 1024, placeholder: t('vide : wss://relay.copylaradio.com · none : rien') }, val)
        : f.key === 'UI_STYLE' ? h('select', { id }, [['', t('Aurore (par défaut)')], ['sobre', t('Sobre')]].map(([o, l]) => h('option', { value: o, selected: (val === 'sobre' ? 'sobre' : '') === o }, l)))
        : f.key === 'OC_AUTO_CREDIT' ? h('select', { id }, [['', t('Je confirme chaque crédit (par défaut)')], ['1', t('La station crédite seule')]].map(([o, l]) => h('option', { value: o, selected: (val === '1' ? '1' : '') === o }, l)))
        : h('input', { id, value: val, autocomplete: 'off', placeholder: HINT[f.key] || '', oninput: check });
      const orig = (f.key === 'OC_AUTO_CREDIT' && val !== '1') || (f.key === 'UI_STYLE' && val !== 'sobre') ? '' : val;
      inputs.set(f.key, { input, orig, get value() { return input.value; } });
      return h('div', {}, label, input);
    };
    // Publier engage toute la constellation (la configuration est rediffusée, mais un nouvel envoi la remplace) : un premier clic
    // résume ce qui change, le second confirme. Toute nouvelle saisie désarme.
    let armed = false;
    const summary = h('p', { class: 'muted', role: 'status' });
    const disarm = () => { armed = false; summary.textContent = ''; go.textContent = t('Publier'); };
    const form = h('form', { oninput: disarm, onchange: disarm, onsubmit: async ev => {
      ev.preventDefault();
      const values = {};
      for (const [k, f] of inputs) {
        if (f.secret) { if (f.clear?.checked) values[k] = null; else if (f.input.value) values[k] = f.input.value; }
        else if (f.input.value.trim() !== f.orig) values[k] = f.input.value.trim() || null;
      }
      const keys = Object.keys(values);
      if (!keys.length) return K.toast(t('Aucune modification'));
      if (!armed) {
        armed = true;
        const name = k => (k.startsWith('station:') ? t('{0} (cette station)', t(LABEL[k.slice(8)] || k)) : t(LABEL[k] || k));
        summary.textContent = keys.every(k => k.startsWith('station:')) ? t('Vous allez modifier pour cette station seulement : {0}.', keys.map(name).join(', ')) : t('Vous allez modifier pour toute la constellation : {0}.', keys.map(name).join(', '));
        go.textContent = t('Confirmer la publication');
        return;
      }
      go.disabled = true;
      try { await K.api('/api/v1/station/coop', { method: 'POST', body: { values } }); K.toast(t('Configuration publiée ({0} clé(s))', keys.length), 1); coop(box, K); } catch (e) { K.toast(e.message); go.disabled = false; disarm(); }
    } });
    const go = h('button', {}, t('Publier'));
    const filled = f => (f.secret ? f.set : !!String(f.value ?? '').trim());
    const OVH = ['OVH_APP_KEY', 'OVH_APP_SECRET', 'OVH_CONSUMER_KEY', 'OVH_ZONE'];
    // État de chaque section, rempli sans bloquer l'écran.
    const state = child => { const p = h('p', { class: 'muted', role: 'status' }); if (child) p.append(child); return p; };
    const says = (p, ok, text) => { p.className = ok === null ? 'muted' : ok ? 'ok' : 'degraded'; p.textContent = (ok === null ? '' : ok ? '✔ ' : '✘ ') + text; };
    // Essai de connexion en lecture seule : la saisie en cours (sinon la valeur publiée) est envoyée, rien n'est écrit chez OVH.
    const tester = () => {
      const out = h('span', { role: 'status' }), btn = h('button', { type: 'button', class: 'ghost', onclick: async () => {
        const values = {};
        for (const k of OVH) { const f = inputs.get(k); if (f && f.input.value.trim()) values[k] = f.input.value.trim(); }
        btn.disabled = true; out.className = 'muted'; out.textContent = ' ' + t('Essai en cours…');
        try {
          const r = await K.api('/api/v1/station/admin/dnslink', { method: 'POST', body: { action: 'test', values } });
          out.className = r.ok ? 'ok' : 'err'; out.textContent = ' ' + (r.ok ? t('✔ Connexion OVH réussie (zone {0})', r.zone) : t('✘ {0}', r.error));
        } catch (e) { out.className = 'err'; out.textContent = ' ✘ ' + e.message; } finally { btn.disabled = false; }
      } }, t('Tester la connexion OVH'));
      return h('p', {}, btn, out, ' ', h('small', { class: 'muted' }, t('Essai en lecture seule : rien n\'est écrit chez OVH.')));
    };
    const charter = s => { const c = s.fields.find(f => f.key === 'CHARTER_TEXT'); return state(c && filled(c) ? t('Charte de la coopérative publiée.') : t('Charte type proposée aux membres (aucun texte publié).')); };
    const mesh = () => {
      const p = state();
      K.api('/api/v1/station/admin/mesh').then(m => {
        if (!m.active) return says(p, false, t('Maillage coupé sur cette station.'));
        const r = m.relays || [], booked = r.filter(x => x.reserved || x.self).length;
        if (!r.length) return says(p, false, t('Aucune station relais : derrière une box, pas de lien hors du réseau local.'));
        says(p, booked > 0, t('{0} station(s) relais, place réservée sur {1}.', r.length, booked));
        p.append(' ', h('a', { href: '#/mesh' }, t('Voir le maillage')));
      }, () => {});
      return p;
    };
    const oc = s => {
      const p = state(), set = s.fields.filter(f => f.key === 'OCSLUG' || f.key === 'OCAPIKEY').every(filled);
      if (!set) { says(p, null, t('Identifiant et clé d\'API requis pour lire les dons.')); return p; }
      says(p, null, t('Lecture des dons du mois…'));
      // `peek` : jamais de crédit.
      K.api('/api/v1/station/oc/peek').then(r => r.error ? says(p, false, r.error) : r.waiting ? says(p, null, t('Première lecture dans quelques minutes.')) : says(p, true, t('Collectif lu : {0} don(s) ce mois, {1} à créditer.', (r.donations || []).length, r.pending || 0)), e => says(p, false, e.message));
      return p;
    };
    // ORIGIN : sa clé coopérative se calcule par tous, sa monnaie et son style ne changent pas.
    const lookState = () => state(K.look().origin ? h('span', { class: 'degraded' }, t('Sur ORIGIN, la monnaie reste « Ẑen » et le style Kraft : ces réglages ne valent que pour une UPlanet privée.')) : h('span', {}, t('Le style Kraft est réservé à ORIGIN : on voit tout de suite qu\'on est sur le réseau public d\'essai.')));
    const STATE = { uplanet: charter, look: lookState, mesh, oc, shares: () => state(h('span', {}, t('Valeur proposée ; chaque station fixe la sienne :'), ' ', h('a', { href: '#/constellation' }, t('Économie')))) };
    const help = s => h('a', { href: DOC + s.id, target: '_blank', rel: 'noopener', title: t('Où trouver ces valeurs'), 'aria-label': t('Où trouver ces valeurs') }, '?');
    const groups = GROUPS.map(([lv, title, why, open]) => {
      const secs = v.sections.filter(s => (s.level || 'essential') === lv), fs = secs.flatMap(s => s.fields);
      if (!secs.length) return null;
      const count = h('small', { class: 'muted' }, ' · ' + t('{0}/{1} renseigné(s)', fs.filter(filled).length, fs.length));
      const body = [why ? h('p', { class: 'muted' }, t(why)) : null, ...secs.map(s => h('fieldset', { class: 'coop-s' }, h('legend', {}, t(TITLE[s.id] || s.title), ' ', help(s)), STATE[s.id]?.(s), s.fields.map(field), s.id === 'ovh' ? tester() : null))];
      return lv === 'essential' ? h('section', {}, h('h3', {}, t(title), count), ...body) : h('details', { class: 'coop-' + lv, open: !!open }, h('summary', {}, h('strong', {}, t(title)), count), ...body);
    });
    form.append(...groups.filter(Boolean), warn, summary, h('p', {}, go));
    box.replaceChildren(h('h2', {}, t('Configuration coopérative'), ' ', K.plane ? h('span', { class: 'pill ok', title: t('Constellation modifiée par cet écran') }, K.plane) : null, ' ', h('small', {}, v.exists ? t('modifiée le {0}', K.when(v.updated_at)) : t('pas encore publiée'))),
      h('p', { class: 'muted' }, t('Un réglage pour toute la constellation. « ? » : où trouver les valeurs.'), ' ', h('small', {}, t('Identité : {0}… · kind 30800.', v.did.slice(0, 22)))), form,
      ...(v.unmanaged.length ? [h('p', { class: 'muted' }, t('Clés conservées telles quelles (hors de cet écran) : {0}.', v.unmanaged.join(', ')))] : []));
    check();
  } catch (e) { box.replaceChildren(h('h2', {}, t('Configuration coopérative')), h('p', { class: 'err', role: 'alert' }, e.message)); }
}
