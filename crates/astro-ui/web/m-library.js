'use strict';
// Bibliothèque : « ce jour-là », recherche à filtres, chronologie par mois et doublons. Tout est calculé par la station sur
// les métadonnées de vos fichiers ; rien n'est supprimé sans votre confirmation.
const day = 86400;
const utc = v => (v ? Date.parse(v + 'T00:00:00Z') / 1000 : null);

export default async function library(root, K) {
  const { h, t } = K, A = '/api/v1/ucloud/';
  const grid = (items, list = []) => { const g = h('div', { class: 'photos' }); K.tiles(g, list, items); return g; };
  const month = ym => new Date(Date.UTC(+ym.slice(0, 4), +ym.slice(5) - 1)).toLocaleDateString(K.lang, { month: 'long', year: 'numeric', timeZone: 'UTC' });

  // ── Ce jour-là ──
  const mem = h('div', {});
  const now = new Date(), two = n => String(n).padStart(2, '0');
  K.api(A + `memories?md=${two(now.getMonth() + 1)}-${two(now.getDate())}&y=${now.getFullYear()}`).then(r => {
    if (!r.memories.length) return;
    mem.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, t('Ce jour-là')),
      r.memories.flatMap(m => [h('h3', {}, (m.ago === 1 ? t('Il y a 1 an') : t('Il y a {0} ans', m.ago)) + ' · ' + m.year), grid(m.items)])));
  }, () => {});

  // ── Recherche ──
  const q = h('input', { type: 'search', placeholder: t('Nom, description…'), 'aria-label': t('Rechercher'), maxlength: 256, autocomplete: 'off' });
  const kind = h('select', { 'aria-label': t('Type') }, [['', 'Tout'], ['image', 'Photos'], ['video', 'Vidéos'], ['other', 'Autres fichiers']].map(([v, l]) => h('option', { value: v }, t(l))));
  const person = h('input', { placeholder: t('Personne (nom exact)'), 'aria-label': t('Personne'), maxlength: 128, autocomplete: 'off' });
  const from = h('input', { type: 'date', 'aria-label': t('Du') }), to = h('input', { type: 'date', 'aria-label': t('Au') });
  const out = h('div', {}), more = h('button', { class: 'ghost', hidden: true }, t('Plus de résultats'));
  let list = [], cursor = null;
  const run = async reset => {
    if (reset) { list = []; cursor = null; out.replaceChildren(); }
    const p = new URLSearchParams({ limit: 60 });
    if (q.value.trim()) p.set('q', q.value.trim());
    if (kind.value) p.set('kind', kind.value);
    if (person.value.trim()) p.set('person', person.value.trim());
    if (utc(from.value) != null) p.set('from', utc(from.value));
    if (utc(to.value) != null) p.set('to', utc(to.value) + day);
    if (cursor) { p.set('after_t', cursor.t); p.set('after_p', cursor.path); }
    try {
      const r = await K.api(A + 'search?' + p);
      if (reset && !r.items.length) out.append(h('p', { class: 'muted' }, t('Aucun résultat.')));
      const g = h('div', { class: 'photos' }); K.tiles(g, list, r.items); out.append(g);
      cursor = r.next; more.hidden = !r.next;
    } catch (e) { K.toast(e.message); }
  };
  more.onclick = () => run(false);
  const search = h('section', { class: 'card wide' }, h('h2', {}, t('Recherche')),
    h('form', { class: 'bar', onsubmit: ev => { ev.preventDefault(); run(true); } }, q, kind, person, from, to, h('button', {}, t('Chercher'))), out, h('p', {}, more));

  // ── Chronologie ──
  const tl = h('section', { class: 'card wide' }, h('h2', {}, t('Chronologie')), h('p', { class: 'muted' }, t('Chargement…')));
  K.api(A + 'timeline').then(r => {
    if (!r.months.length) { tl.lastChild.textContent = t('Aucune photo datée pour le moment.'); return; }
    const box = h('div', {});
    tl.lastChild.replaceWith(h('div', { class: 'bar' }, r.months.map(m => h('button', { class: 'ghost', onclick: async ev => {
      const btn = ev.currentTarget; btn.disabled = true;
      try { const d = await K.api(A + 'timeline?ym=' + m.ym); box.replaceChildren(h('h3', {}, month(m.ym)), grid(d.items)); box.scrollIntoView({ block: 'nearest' }); } catch (e) { K.toast(e.message); }
      btn.disabled = false;
    } }, `${month(m.ym)} · ${m.count}`))), box);
  }, e => { tl.lastChild.textContent = e.message; });

  // ── Doublons ──
  const dupBox = h('div', {});
  const dupBtn = h('button', { class: 'ghost', onclick: async ev => {
    const self = ev.currentTarget; self.disabled = true;
    try {
      const r = await K.api(A + 'duplicates'), name = p => p.split('/').pop();
      const group = (g, label) => {
        const keep = g.items[g.keep], others = g.items.filter((_, i) => i !== g.keep);
        return h('div', { class: 'card' }, h('h3', {}, label), grid(g.items),
          h('p', { class: 'muted' }, t('À garder : {0} ({1})', name(keep.path), K.bytes(keep.size))),
          h('button', { class: 'ghost', onclick: async ev2 => {
            const del = ev2.currentTarget;
            if (!confirm(t('Supprimer définitivement {0} fichier(s) ? Le fichier gardé est « {1} ».', others.length, name(keep.path)))) return;
            del.disabled = true;
            try { for (const o of others) { const r2 = await K.del(o.path); if (!r2.ok && r2.status !== 404) throw new Error(`${name(o.path)} : ${r2.status}`); } K.toast(t('{0} fichier(s) supprimé(s)', others.length), 1); dupBtn.click(); } catch (e) { K.toast(e.message); del.disabled = false; }
          } }, t('Supprimer les {0} autre(s)', others.length)));
      };
      dupBox.replaceChildren(...[
        r.exact.length || r.similar.length ? null : h('p', { class: 'muted' }, t('Aucun doublon.')),
        r.exact.length ? [h('h3', {}, t('Fichiers identiques')), r.exact.map(g => group(g, t('{0} copies identiques', g.items.length)))] : null,
        r.similar.length ? [h('h3', {}, t('Images très ressemblantes')), r.similar.map(g => group(g, t('{0} versions de la même image', g.items.length)))] : null].flat(2).filter(Boolean));
    } catch (e) { K.toast(e.message); }
    self.disabled = false;
  } }, t('Chercher les doublons'));
  const dups = h('section', { class: 'card wide' }, h('h2', {}, t('Doublons')), h('p', { class: 'muted' }, t('Mêmes fichiers et images presque identiques (recadrage, recompression). Rien n\'est supprimé sans votre confirmation.')), h('p', {}, dupBtn), dupBox);

  root.replaceChildren(h('h2', {}, t('Bibliothèque')), mem, h('div', { class: 'grid' }, search, tl, dups));
}
