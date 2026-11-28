'use strict';
// Moi › Appareils, carte IA (docs/design/IA.md) : l'IA prêtée par la station, puis la sienne si le Capitaine le permet.
const CAPS = [['chat', 'Discuter'], ['tools', 'Agir (outils)'], ['vision', 'Voir les images'], ['embeddings', 'Mémoire'], ['images', 'Créer des images']];
// Fonction → capacité qu'elle demande.
const USES = [['Discuter avec BRO', 'chat'], ['Résumer un document, une conversation', 'chat'], ['Traduire un message', 'chat'], ['BRO agit pour vous (fichiers, agenda, proches)', 'tools'], ['Décrire une photo, lire un document difficile', 'vision'], ['Créer une image', 'images']];

export default async function aiCard(card, K) {
  const { h, t, head } = K;
  card.append(head(t('Intelligence artificielle'), t('Donnez une IA à BRO : celle de votre station, ou un fournisseur sur Internet. Une clé est chiffrée pour vous seul et n\'est jamais réaffichée.')));
  const p = await K.api('/api/v1/me/ai'), me = { captain: p.captain };
  const tests = p.tests || {}, R = p.rights || {};
  const body = h('div');
  card.append(body);
  if (!p.captain && R.mode === 'off') return void body.append(h('p', { class: 'muted' }, t('Votre Capitaine n\'a pas ouvert l\'IA à votre compte.')));

  // ── L'IA que la station prête : ce qu'elle est (jamais sa clé), ses limites, son test ──
  const lim = [R.quota_tokens_week ? t('jusqu\'à {0} jetons par semaine', R.quota_tokens_week.toLocaleString()) : null, R.price_zen_week ? t('{0} ẑ par semaine', R.price_zen_week) : null].filter(Boolean).join(' · ');
  const st = p.station;
  const lent = st ? h('fieldset', { class: 'ai-fam' }, h('legend', {}, '🛰️ ', t('Prêtée par votre station')),
    h('p', {}, st.family === 'cloud' ? t('Par Internet, chez {0}', st.provider || '?') : t('Sur votre station : tout reste chez vous.'), st.model ? ' · ' + st.model : ''),
    st.family === 'cloud' ? h('p', { class: 'muted' }, t('Vos messages sortent de la constellation, vers ce fournisseur.')) : '',
    lim && !p.captain ? h('p', { class: 'muted' }, lim) : '', result('station'), h('p', {}, testBtn('station')))
    : p.captain ? '' : h('p', { class: 'muted' }, t('La station ne prête pas d\'IA pour l\'instant.'));

  // ── Sur votre station : une offre de la station (le Capitaine peut aussi donner une autre adresse locale) ──
  const offers = p.offers || [];
  const known = offers.some(o => o.url === p.ollama_url);
  const pick = h('select', { id: 'ai-offer', 'aria-label': t('Service de la station') },
    h('option', { value: '' }, t('Aucun')), offers.map(o => h('option', { value: o.url, selected: o.url === p.ollama_url }, t(o.label))),
    me.captain ? h('option', { value: '*', selected: !!p.ollama_url && !known }, t('Autre adresse sur cette machine…')) : null);
  const other = h('input', { id: 'ai-url', value: known ? '' : (p.ollama_url || ''), placeholder: 'http://127.0.0.1:8000', 'aria-label': t('Adresse sur cette machine'), maxlength: 256, autocomplete: 'off' });
  const showOther = () => { other.hidden = pick.value !== '*'; };
  pick.onchange = showOther; showOther();
  const local = h('fieldset', { class: 'ai-fam' }, h('legend', {}, '🏠 ', t('Sur votre station')), h('p', { class: 'muted' }, t('Tout reste chez vous.')),
    offers.length || me.captain ? [pick, other] : h('p', { class: 'muted' }, t('Votre station ne propose pas encore d\'IA : demandez à votre Capitaine.')),
    result('ucloud'));

  // ── Par Internet : un fournisseur et sa clé ──
  const prov = h('select', { id: 'ai-prov', 'aria-label': t('Fournisseur') }, h('option', { value: '' }, t('Aucun')),
    (p.providers || []).map(x => h('option', { value: x.id, selected: p.provider === x.id }, x.label)));
  const rpm = h('input', { id: 'ai-rpm', inputmode: 'numeric', value: p.rpm || '', placeholder: String(p.default_rpm || 50), 'aria-label': t('Requêtes par minute') });
  const key = h('input', { id: 'ai-key', secret: true, placeholder: p.has_key ? t('Clé enregistrée : laisser vide pour la garder') : t('Clé d\'API'), autocomplete: 'off', 'aria-label': t('Clé d\'API'), maxlength: 256 });
  const refer = h('p', { class: 'muted' });
  const showRefer = () => {
    const url = (p.referrals || {})[prov.value], name = prov.selectedOptions[0]?.textContent;
    refer.replaceChildren(...(url ? [t('Pas encore de compte ?'), ' ', h('a', { href: url, target: '_blank', rel: 'noopener' }, t('S\'inscrire chez {0}', name)), ' — ', t('lien de parrainage de votre coopérative : le fournisseur lui verse une commission sur votre usage.')] : []));
  };
  prov.addEventListener('change', showRefer); showRefer();
  const cloud = h('fieldset', { class: 'ai-fam' }, h('legend', {}, '🌐 ', t('Par Internet')), h('p', { class: 'muted' }, t('Vos messages sortent de la constellation, vers ce fournisseur.')),
    h('label', { for: 'ai-prov' }, t('Fournisseur')), prov, refer, h('label', { for: 'ai-key' }, t('Clé d\'API')), key,
    h('label', { for: 'ai-rpm' }, t('Requêtes par minute que permet votre compte (vide : {0})', p.default_rpm || 50)), rpm,
    h('p', { class: 'muted' }, t('OpenRouter et MixRoute donnent accès à des centaines de modèles avec une seule clé ; MixRoute ouvre des modèles gratuits pour commencer.')), result('cloud'));

  // ── Modèle : choisi dans la liste du dernier test (on peut encore le taper) ──
  const all = [...(tests.ucloud?.models || []), ...(tests.cloud?.models || [])];
  const list = h('datalist', { id: 'ai-models' }, all.slice(0, 400).map(m => h('option', { value: m.id })));
  const model = h('input', { id: 'ai-model', value: p.model || '', placeholder: t('Choisi pour vous si vide'), 'aria-label': t('Modèle'), maxlength: 64, autocomplete: 'off' });
  model.setAttribute('list', 'ai-models'); // « list » n'est lisible qu'en attribut
  const price = h('p', { class: 'muted' });
  const showPrice = () => {
    const m = all.find(x => x.id === model.value);
    price.replaceChildren(m && m.price_in != null ? t('Prix par million de jetons : {0} $ envoyés, {1} $ reçus', dollars(m.price_in), m.price_out == null ? '?' : dollars(m.price_out)) : '',
      m && m.context ? ' · ' + t('lit jusqu\'à {0} jetons', m.context.toLocaleString()) : '');
  };
  model.oninput = showPrice; showPrice();

  const save = async () => {
    const url = pick.value === '*' ? other.value.trim() : pick.value;
    const n = rpm.value.trim() === '' ? null : Number(rpm.value);
    if (n !== null && !(Number.isInteger(n) && n >= 1 && n <= 10000)) throw new Error(t('Requêtes par minute : un entier de 1 à 10 000'));
    await K.api('/api/v1/me/ai', { method: 'POST', body: { ollama_url: url || null, provider: prov.value || null, model: model.value.trim() || null, rpm: n, ...(key.value && { api_key: key.value }) } });
  };
  const run = async (btn, fam) => {
    btn.disabled = true; btn.textContent = t('Test en cours…');
    try { if (fam !== 'station') await save(); await K.api('/api/v1/me/ai/test', { method: 'POST', body: { family: fam } }); }
    catch (e) { K.toast(e.message); }
    K.mod('m-ai.js').then(m => { card.replaceChildren(); m.default(card, K); });
  };
  function testBtn(fam) { return h('button', { type: 'button', class: 'ghost', onclick: ev => run(ev.target, fam) }, t('Tester')); }
  local.append(offers.length || me.captain ? h('p', {}, testBtn('ucloud')) : '');
  cloud.append(h('p', {}, testBtn('cloud')));

  const mine = p.can_own ? [h('h3', {}, t('Votre propre IA')), local, cloud, h('label', { for: 'ai-model' }, t('Modèle')), model, list, price]
    : [h('p', { class: 'muted' }, t('Utiliser votre propre clé ou un autre service : à demander à votre Capitaine.'))];
  body.append(p.declared || st ? h('p', {}, h('span', { class: 'pill ok' }, t('Branché')), ' ', h('a', { href: '#/bro' }, t('Parler à BRO') + ' →')) : '',
    lent, ...mine,
    !p.can_own ? '' : h('p', {}, h('button', { type: 'button', onclick: async ev => { ev.target.disabled = true; try { await save(); K.toast(t('Paramètres IA enregistrés'), 1); location.reload(); } catch (e) { K.toast(e.message); ev.target.disabled = false; } } }, t('Enregistrer')),
      p.has_key ? [' ', K.sure(t('Retirer la clé'), async () => { try { await K.api('/api/v1/me/ai', { method: 'POST', body: { api_key: null } }); location.reload(); } catch (e) { K.toast(e.message); } }, t('Retirer la clé d\'API enregistrée ?'))] : ''),
    uses());

  // Dernier test d'une famille : réponse, délai, capacités, crédit.
  function result(fam) {
    const r = tests[fam];
    if (!r) return h('p', { class: 'muted' }, t('Pas encore testé.'));
    const caps = h('ul', { class: 'ai-caps' }, CAPS.map(([k, l]) => h('li', { class: 'ai-' + mark(r.caps?.[k]) }, sign(r.caps?.[k]), ' ', t(l))));
    return h('div', { class: 'ai-res' },
      r.ok ? h('p', {}, '✅ ', t('Répond en {0} ms avec {1}', r.latency_ms ?? '?', r.model)) : h('p', { class: 'err' }, '⚠️ ', r.error || t('Ne répond pas')),
      h('small', { class: 'muted' }, t('Testé {0}', K.when(r.at)), r.models_total ? ' · ' + t('{0} modèles proposés', r.models_total) : '',
        r.credit?.remaining != null ? ' · ' + t('crédit restant : {0} $', String(r.credit.remaining)) : ''), r.ok ? caps : '');
  }
  // Ce que l'IA permet : une fonction est ouverte si une des deux familles a la capacité.
  function uses() {
    // Seules comptent les IA dont il peut se servir : la sienne seulement s'il y a droit.
    const has = k => (p.can_own ? ['station', 'ucloud', 'cloud'] : ['station']).some(f => tests[f]?.ok && tests[f].caps?.[k] === true);
    return h('details', { class: 'fold', open: !!(tests.station || tests.ucloud || tests.cloud) }, h('summary', {}, t('Ce que votre IA permet')),
      h('ul', { class: 'ai-caps' }, USES.map(([l, k]) => h('li', { class: 'ai-' + (has(k) ? 'yes' : 'no') }, has(k) ? '✅ ' : '➖ ', t(l)))),
      h('p', { class: 'muted' }, t('Sans IA, votre station fait déjà : recherche dans vos photos, lecture du texte des images, visages, recherche par le sens.')));
  }
}

const mark = v => (v === true ? 'yes' : v === false ? 'no' : 'unk');
const sign = v => (v === true ? '✅' : v === false ? '➖' : '❔');
// Millièmes de dollar (entier) → « 0,200 » : jamais de calcul flottant sur un prix.
const dollars = n => `${Math.floor(n / 1000)},${String(n % 1000).padStart(3, '0')}`;
