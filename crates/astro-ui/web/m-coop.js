'use strict';
// Configuration coopérative (Capitaine) : kind 30800 `cooperative-config`, partagé par toute la constellation et lisible par
// les stations legacy. Les valeurs en clair s'éditent ; les SECRETS sont en écriture seule : on voit s'ils sont renseignés,
// jamais leur valeur, et on les remplace ou on les efface. Seules les clés modifiées sont envoyées.
const LABEL = { UPLANET_NAME: 'Nom de la constellation', CHARTER_VERSION: 'Version de la charte', CHARTER_TEXT: 'Texte de la charte (signé par la coopérative)', TVA_RATE: 'TVA (%)', IS_RATE_REDUCED: 'IS taux réduit (%)', IS_RATE_NORMAL: 'IS taux normal (%)', IS_THRESHOLD: 'Seuil IS (€)', ZENCARD_SATELLITE: 'ZenCard Satellite (€)', ZENCARD_CONSTELLATION: 'ZenCard Constellation (€)', PAF: 'PAF (Ẑ/semaine)', NCARD: 'NCARD (Ẑ/semaine)', ZCARD: 'ZCARD (Ẑ/semaine)', TREASURY_PERCENT: 'Trésorerie (%)', RND_PERCENT: 'R&D (%)', ASSETS_PERCENT: 'Actifs (%)', CAPTAIN_BONUS_PERCENT: 'Prime du Capitaine (%)', OCSLUG: 'Identifiant du collectif', OCAPIKEY: 'Clé d\'API OpenCollective', OC_URL_CLOUD: 'Adresse « Cloud »', OC_URL_MEMBRE: 'Adresse « Membre résident »', OC_URL_SATELLITE: 'Adresse « Satellite »', OC_URL_CONSTELLATION: 'Adresse « Constellation (GPU) »', TIER_SLUG_SATELLITE: 'Paliers Satellite', TIER_SLUG_CONSTELLATION: 'Paliers Constellation', TIER_SLUG_LABO: 'Paliers Labo', TIER_SLUG_CLOUD: 'Paliers Cloud', PLANTNET_API_KEY: 'Clé PlantNet', GIT_HOST: 'Serveur Git', GIT_TOKEN: 'Jeton Git', GIT_OWNER: 'Propriétaire Git', MJ_APIKEY_PUBLIC: 'Mailjet : clé publique', MJ_APIKEY_PRIVATE: 'Mailjet : clé privée', MJ_SENDER_EMAIL: 'Mailjet : expéditeur', OVH_APP_KEY: 'OVH : clé d\'application', OVH_APP_SECRET: 'OVH : secret', OVH_CONSUMER_KEY: 'OVH : clé de consommateur', OVH_ZONE: 'OVH : zone DNS', P2P_RELAYS: 'Stations relais (une adresse par ligne)' };

const TITLE = { uplanet: 'Nom et charte de la constellation', fiscal: 'Paramètres fiscaux', shares: 'Parts coopératives', '3x13': 'Règle 3×1/3 + 1 %', oc: 'OpenCollective', tiers: 'Routage des paliers', api: 'Clés d\'API', mj: 'Mailjet', ovh: 'DNSLink OVH', mesh: 'Stations relais du maillage' };
const GROUPS = [
  ['essential', 'Essentiel', 'Ce qui met la station en service : le nom et la charte de la constellation, les adresses de la coopérative (OVH) et les stations relais qui relient les stations derrière une box.'],
  ['services', 'Services', 'Paiements (OpenCollective), courriels (Mailjet) et clés d\'API.'],
  ['experts', 'Réglages experts', 'Fiscalité, parts et répartition 3×1/3 : communs à toute la constellation, rarement modifiés.'],
];

export default async function coop(box, K) {
  const { h, t } = K;
  box.replaceChildren(h('h2', {}, t('Configuration coopérative')), h('p', { class: 'muted' }, t('Chargement…')));
  try {
    const v = await K.api('/api/v1/station/coop'), inputs = new Map();
    const pct = () => ['TREASURY_PERCENT', 'RND_PERCENT', 'ASSETS_PERCENT', 'CAPTAIN_BONUS_PERCENT'].reduce((n, k) => n + (parseFloat(inputs.get(k)?.value) || 0), 0);
    const PCT = ['TREASURY_PERCENT', 'RND_PERCENT', 'ASSETS_PERCENT', 'CAPTAIN_BONUS_PERCENT'];
    const warn = h('p', { class: 'degraded', role: 'status' });
    // Total faux : toujours signalé ; la publication n'est bloquée que si CES champs ont été modifiés (sinon un total déjà faux
    // empêcherait de corriger autre chose, comme la zone OVH).
    const check = () => {
      const p = Math.round(pct() * 100) / 100, off = inputs.has('TREASURY_PERCENT') && Math.abs(p - 100) > 0.001;
      const touched = PCT.some(k => inputs.get(k) && inputs.get(k).input.value.trim() !== inputs.get(k).orig);
      warn.textContent = off ? t('La règle 3×1/3 + 1 % additionne {0} % (100 % attendus).', p) + (touched ? ' ' + t('Publication impossible tant que le total n\'est pas 100 %.') : '') : '';
      go.disabled = off && touched;
      if (off) box.querySelector('.coop-experts')?.setAttribute('open', '');
    };
    const field = f => {
      const id = 'cf-' + f.key, label = h('label', { for: id }, t(LABEL[f.key] || f.key));
      if (f.secret) {
        const input = h('input', { id, type: 'password', autocomplete: 'off', placeholder: f.set ? (f.readable ? t('● renseigné — saisir pour remplacer') : t('● renseigné mais illisible ici (autre constellation ?)')) : t('non renseigné') });
        const clear = f.set ? h('label', { class: 'muted' }, h('input', { type: 'checkbox', id: id + '-x' }), ' ' + t('effacer')) : null;
        inputs.set(f.key, { input, clear: clear?.querySelector('input'), secret: true });
        return h('div', {}, label, input, clear);
      }
      const input = f.key === 'CHARTER_TEXT' ? h('textarea', { id, rows: 14, maxlength: 6000, placeholder: t('Laisser vide : la charte type est proposée aux membres') }, f.value ?? '')
        : f.key === 'P2P_RELAYS' ? h('textarea', { id, rows: 3, maxlength: 1024, placeholder: '/ip4/203.0.113.7/tcp/4101/p2p/12D3Koo…\n/dns4/relais.exemple.org/tcp/4101/p2p/12D3Koo…' }, f.value ?? '') : h('input', { id, value: f.value ?? '', autocomplete: 'off', oninput: check });
      inputs.set(f.key, { input, orig: f.value ?? '', get value() { return input.value; } });
      return h('div', {}, label, input);
    };
    // Publier engage toute la constellation (la configuration est rediffusée, mais un nouvel envoi la remplace) : un premier clic
    // résume ce qui change, le second confirme. Toute nouvelle saisie désarme.
    let armed = false;
    const summary = h('p', { class: 'muted', role: 'status' });
    const disarm = () => { armed = false; summary.textContent = ''; go.textContent = t('Publier'); };
    const form = h('form', { oninput: disarm, onsubmit: async ev => {
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
        summary.textContent = t('Vous allez modifier pour toute la constellation : {0}.', keys.map(k => t(LABEL[k] || k)).join(', '));
        go.textContent = t('Confirmer la publication');
        return;
      }
      go.disabled = true;
      try { await K.api('/api/v1/station/coop', { method: 'POST', body: { values } }); K.toast(t('Configuration publiée ({0} clé(s))', keys.length), 1); coop(box, K); } catch (e) { K.toast(e.message); go.disabled = false; disarm(); }
    } });
    const go = h('button', {}, t('Publier'));
    // Trois niveaux : l'essentiel en tête, les services ensuite, les réglages experts repliés (rarement touchés).
    const filled = f => (f.secret ? f.set : !!String(f.value ?? '').trim());
    const OVH = ['OVH_APP_KEY', 'OVH_APP_SECRET', 'OVH_CONSUMER_KEY', 'OVH_ZONE'];
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
    const groups = GROUPS.map(([lv, title, why]) => {
      const secs = v.sections.filter(s => (s.level || 'essential') === lv), fs = secs.flatMap(s => s.fields);
      if (!secs.length) return null;
      const count = h('small', { class: 'muted' }, ' · ' + t('{0}/{1} renseigné(s)', fs.filter(filled).length, fs.length));
      const body = [h('p', { class: 'muted' }, t(why)), ...secs.map(s => h('fieldset', { class: 'coop-s' }, h('legend', {}, t(TITLE[s.id] || s.title)), s.fields.map(field), s.id === 'ovh' ? tester() : null))];
      return lv === 'essential' ? h('section', {}, h('h3', {}, t(title), count), ...body) : h('details', { class: 'coop-' + lv, open: lv === 'services' }, h('summary', {}, h('strong', {}, t(title)), count), ...body);
    });
    form.append(...groups.filter(Boolean), warn, summary, h('p', {}, go));
    box.replaceChildren(h('h2', {}, t('Configuration coopérative'), ' ', K.plane ? h('span', { class: 'pill ok', title: t('Constellation modifiée par cet écran') }, K.plane) : null, ' ', h('small', {}, v.exists ? t('modifiée le {0}', K.when(v.updated_at)) : t('pas encore publiée'))),
      h('p', { class: 'muted' }, t('Identité : {0}… · kind 30800, répliqué aux autres stations de la constellation.', v.did.slice(0, 22))), form,
      ...(v.unmanaged.length ? [h('p', { class: 'muted' }, t('Clés conservées telles quelles (hors de cet écran) : {0}.', v.unmanaged.join(', ')))] : []));
    check();
  } catch (e) { box.replaceChildren(h('h2', {}, t('Configuration coopérative')), h('p', { class: 'err', role: 'alert' }, e.message)); }
}
