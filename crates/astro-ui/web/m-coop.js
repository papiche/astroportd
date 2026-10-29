'use strict';
// Configuration coopérative (Capitaine) : kind 30800 `cooperative-config`, partagé par toute la constellation et lisible par
// les stations legacy. Les valeurs en clair s'éditent ; les SECRETS sont en écriture seule : on voit s'ils sont renseignés,
// jamais leur valeur, et on les remplace ou on les efface. Seules les clés modifiées sont envoyées.
const LABEL = { TVA_RATE: 'TVA (%)', IS_RATE_REDUCED: 'IS taux réduit (%)', IS_RATE_NORMAL: 'IS taux normal (%)', IS_THRESHOLD: 'Seuil IS (€)', ZENCARD_SATELLITE: 'ZenCard Satellite (€)', ZENCARD_CONSTELLATION: 'ZenCard Constellation (€)', PAF: 'PAF (Ẑ/semaine)', NCARD: 'NCARD (Ẑ/semaine)', ZCARD: 'ZCARD (Ẑ/semaine)', TREASURY_PERCENT: 'Trésorerie (%)', RND_PERCENT: 'R&D (%)', ASSETS_PERCENT: 'Actifs (%)', CAPTAIN_BONUS_PERCENT: 'Prime du Capitaine (%)', OCSLUG: 'Identifiant du collectif', OCAPIKEY: 'Clé d\'API OpenCollective', OC_URL_CLOUD: 'Adresse « Cloud »', OC_URL_MEMBRE: 'Adresse « Membre résident »', OC_URL_SATELLITE: 'Adresse « Satellite »', OC_URL_CONSTELLATION: 'Adresse « Constellation (GPU) »', TIER_SLUG_SATELLITE: 'Paliers Satellite', TIER_SLUG_CONSTELLATION: 'Paliers Constellation', TIER_SLUG_LABO: 'Paliers Labo', TIER_SLUG_CLOUD: 'Paliers Cloud', PLANTNET_API_KEY: 'Clé PlantNet', GIT_HOST: 'Serveur Git', GIT_TOKEN: 'Jeton Git', GIT_OWNER: 'Propriétaire Git', MJ_APIKEY_PUBLIC: 'Mailjet : clé publique', MJ_APIKEY_PRIVATE: 'Mailjet : clé privée', MJ_SENDER_EMAIL: 'Mailjet : expéditeur', OVH_APP_KEY: 'OVH : clé d\'application', OVH_APP_SECRET: 'OVH : secret', OVH_CONSUMER_KEY: 'OVH : clé de consommateur', OVH_ZONE: 'OVH : zone DNS' };

export default async function coop(box, K) {
  const { h } = K;
  box.replaceChildren(h('h2', {}, 'Configuration coopérative'), h('p', { class: 'muted' }, 'Chargement…'));
  try {
    const v = await K.api('/api/v1/station/coop'), inputs = new Map();
    const pct = () => ['TREASURY_PERCENT', 'RND_PERCENT', 'ASSETS_PERCENT', 'CAPTAIN_BONUS_PERCENT'].reduce((n, k) => n + (parseFloat(inputs.get(k)?.value) || 0), 0);
    const warn = h('p', { class: 'degraded', role: 'status' });
    const check = () => { const p = pct(); warn.textContent = inputs.has('TREASURY_PERCENT') && Math.abs(p - 100) > 0.001 ? `La règle 3×1/3 + 1 % additionne ${p} % (100 % attendus).` : ''; };
    const field = f => {
      const id = 'cf-' + f.key, label = h('label', { for: id }, LABEL[f.key] || f.key);
      if (f.secret) {
        const input = h('input', { id, type: 'password', autocomplete: 'off', placeholder: f.set ? (f.readable ? '● renseigné — saisir pour remplacer' : '● renseigné mais illisible ici (autre constellation ?)') : 'non renseigné' });
        const clear = f.set ? h('label', { class: 'muted' }, h('input', { type: 'checkbox', id: id + '-x' }), ' effacer') : null;
        inputs.set(f.key, { input, clear: clear?.querySelector('input'), secret: true });
        return h('div', {}, label, input, clear);
      }
      const input = h('input', { id, value: f.value ?? '', autocomplete: 'off', oninput: check });
      inputs.set(f.key, { input, orig: f.value ?? '', get value() { return input.value; } });
      return h('div', {}, label, input);
    };
    const form = h('form', { onsubmit: async ev => {
      ev.preventDefault();
      const values = {};
      for (const [k, f] of inputs) {
        if (f.secret) { if (f.clear?.checked) values[k] = null; else if (f.input.value) values[k] = f.input.value; }
        else if (f.input.value.trim() !== f.orig) values[k] = f.input.value.trim() || null;
      }
      if (!Object.keys(values).length) return K.toast('Aucune modification');
      go.disabled = true;
      try { await K.api('/api/v1/station/coop', { method: 'POST', body: { values } }); K.toast(`Configuration publiée (${Object.keys(values).length} clé(s))`, 1); coop(box, K); } catch (e) { K.toast(e.message); go.disabled = false; }
    } });
    const go = h('button', {}, 'Publier');
    form.append(...v.sections.map(s => h('fieldset', { class: 'coop-s' }, h('legend', {}, s.title), s.fields.map(field))), warn, h('p', {}, go));
    box.replaceChildren(h('h2', {}, 'Configuration coopérative ', h('small', {}, v.exists ? `modifiée le ${K.when(v.updated_at)}` : 'pas encore publiée')),
      h('p', { class: 'muted' }, `Identité : ${v.did.slice(0, 22)}… · kind 30800, répliqué aux autres stations de la constellation.`), form,
      v.unmanaged.length ? h('p', { class: 'muted' }, `Clés conservées telles quelles (hors de cet écran) : ${v.unmanaged.join(', ')}.`) : null);
    check();
  } catch (e) { box.replaceChildren(h('h2', {}, 'Configuration coopérative'), h('p', { class: 'err', role: 'alert' }, e.message)); }
}
