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
  const mem = h('div', {}), memList = [];
  const now = new Date(), two = n => String(n).padStart(2, '0');
  K.api(A + `memories?md=${two(now.getMonth() + 1)}-${two(now.getDate())}&y=${now.getFullYear()}`).then(r => {
    if (!r.memories.length) return;
    mem.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, t('Ce jour-là')),
      r.memories.flatMap(m => [h('h3', {}, (m.ago === 1 ? t('Il y a 1 an') : t('Il y a {0} ans', m.ago)) + ' · ' + m.year), grid(m.items, memList)])));
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

  // ── Temps : un fil continu, jour par jour, du plus récent au plus ancien ──
  const streamList = []; // une seule liste pour tout le fil : la visionneuse passe d'un jour au suivant
  const tl = h('section', { class: 'card wide' }, h('h2', {}, t('Temps')), h('p', { class: 'muted' }, t('Chargement…')));
  K.api(A + 'timeline').then(r => {
    if (!r.months.length) { tl.lastChild.textContent = t('Aucune photo datée pour le moment.'); return; }
    const months = r.months, box = h('div', { class: 'stream' }), more2 = h('button', { class: 'ghost' }, t('Mois précédents'));
    const dayLabel = s => new Date(s * 1000).toLocaleDateString(K.lang, { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' });
    const jump = h('select', { 'aria-label': t('Aller au mois') }, months.map((m, i) => h('option', { value: i }, `${month(m.ym)} · ${m.count}`)));
    let next = 0, busy = false;
    const block = (m, items) => {
      const byDay = new Map();
      for (const it of items) { const d = Math.floor((it.t || 0) / day) * day; (byDay.get(d) || byDay.set(d, []).get(d)).push(it); }
      return h('section', { class: 'month' }, h('h3', { class: 'sticky' }, `${month(m.ym)} · ${m.count}`),
        [...byDay].flatMap(([d, its]) => [h('h4', {}, `${dayLabel(d)} · ${its.length}`), grid(its, streamList)]));
    };
    const loadNext = async () => {
      if (busy || next >= months.length) return;
      busy = true;
      try { const m = months[next++], d = await K.api(A + 'timeline?ym=' + m.ym); box.append(block(m, d.items)); } catch (e) { K.toast(e.message); next--; }
      busy = false; more2.hidden = next >= months.length;
    };
    more2.onclick = loadNext;
    jump.onchange = () => { box.replaceChildren(); streamList.length = 0; next = +jump.value; loadNext(); };
    new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) loadNext(); }, { rootMargin: '600px' }).observe(more2);
    tl.lastChild.replaceWith(h('div', {}, h('p', {}, h('label', {}, t('Aller au mois'), ' '), jump), box, h('p', {}, more2)));
    loadNext();
  }, e => { tl.lastChild.textContent = e.message; });

  // ── Carte ──
  const mapSection = h('section', { class: 'card wide' }, h('h2', {}, t('Carte')), h('p', { class: 'muted' }, t('Vos photos géolocalisées, regroupées par la station : aucun service de carte extérieur n\'est appelé.')));
  K.mod('m-map.js').then(m => mapSection.append(m.default(K, grid)), e => mapSection.append(h('p', { class: 'err' }, e.message)));

  // ── Doublons ──
  const dupBox = h('div', {});
  const dupBtn = h('button', { class: 'ghost', onclick: async ev => {
    const self = ev.currentTarget; self.disabled = true;
    try {
      const r = await K.api(A + 'duplicates'), name = p => p.split('/').pop();
      const groups = [...r.exact.map(g => [g, t('{0} copies identiques', g.items.length)]), ...r.similar.map(g => [g, t('{0} versions de la même image', g.items.length)])];
      if (!groups.length) { dupBox.replaceChildren(h('p', { class: 'muted' }, t('Aucun doublon.'))); self.disabled = false; return; }
      const boxes = [], total = h('strong', {}), del = h('button', { class: 'ghost danger' });
      const chosen = () => boxes.filter(b => b.checked);
      const sync = () => { const n = chosen().length; total.textContent = t('{0} sélectionné(s)', n); del.textContent = t('Supprimer la sélection ({0})', n); del.disabled = !n; };
      const setAll = f => { boxes.forEach(b => { b.checked = f(b); }); sync(); };
      const rows = groups.map(([g, label]) => h('section', { class: 'card' }, h('h3', {}, label),
        g.items.map((it, i) => {
          const cb = h('input', { type: 'checkbox', id: 'dup-' + boxes.length, 'aria-label': name(it.path), onchange: sync }); cb.dataset.path = it.path; cb.dataset.keep = i === g.keep ? '1' : ''; cb.checked = i !== g.keep; boxes.push(cb);
          const th = h('div', { class: 'photos dup-th' }); K.tiles(th, g.items, [it]);
          return h('div', { class: 'dup-row' }, cb, th, h('label', { for: cb.id }, h('strong', {}, name(it.path)), h('small', { class: 'muted' }, ` ${K.bytes(it.size)} · ${it.path}`), i === g.keep ? h('span', { class: 'pill ok' }, t('à garder')) : null));
        })));
      del.onclick = async () => {
        const sel = chosen(), paths = sel.map(b => b.dataset.path);
        const groupsAll = groups.filter(([g]) => g.items.every(it => paths.includes(it.path))).length;
        if (!confirm(t('Supprimer définitivement {0} fichier(s) ?', paths.length) + (groupsAll ? '\n' + t('Attention : dans {0} groupe(s), toutes les copies sont cochées.', groupsAll) : ''))) return;
        del.disabled = true; let done = 0;
        try { for (const p of paths) { const r2 = await K.del(p); if (!r2.ok && r2.status !== 404) throw new Error(`${name(p)} : ${r2.status}`); done++; } K.toast(t('{0} fichier(s) supprimé(s)', done), 1); }
        catch (e) { K.toast(e.message); }
        dupBtn.click();
      };
      dupBox.replaceChildren(h('div', { class: 'bar dup-bar' }, total, h('button', { class: 'ghost', onclick: () => setAll(() => true) }, t('Tout sélectionner')),
        h('button', { class: 'ghost', onclick: () => setAll(b => !b.dataset.keep) }, t('Tout sauf un par groupe')), h('button', { class: 'ghost', onclick: () => setAll(() => false) }, t('Tout désélectionner')), del), ...rows);
      sync();
    } catch (e) { K.toast(e.message); }
    self.disabled = false;
  } }, t('Chercher les doublons'));
  const dups = h('section', { class: 'card wide' }, h('h2', {}, t('Doublons')), h('p', { class: 'muted' }, t('Images identiques ou presque (recadrage, recompression). Cochez ce que vous voulez supprimer, en une seule action : rien n\'est supprimé sans votre confirmation.')), h('p', {}, dupBtn), dupBox);

  // ── Onglets : une vue à la fois (le fil du temps est long, la carte a besoin de place) ──
  const views = [['time', 'Temps', tl], ['map', 'Carte', mapSection], ['search', 'Recherche', search], ['dups', 'Doublons', dups]];
  const tabs = h('div', { class: 'bar', role: 'tablist' }, views.map(([id, label]) => h('button', { role: 'tab', id: 'lib-tab-' + id, 'aria-selected': 'false', onclick: () => pick(id) }, t(label))));
  const pick = id => {
    views.forEach(([k, , el]) => { el.hidden = k !== id; mem.hidden = id !== 'time'; tabs.querySelector('#lib-tab-' + k).setAttribute('aria-selected', String(k === id)); tabs.querySelector('#lib-tab-' + k).className = k === id ? '' : 'ghost'; });
    if (id === 'map') window.dispatchEvent(new Event('resize'));
  };
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-photos.css' }); document.head.append(css); K.cleanup(() => css.remove());
  // ── Sélection de vignettes : tout sélectionner, tourner, supprimer ──
  const P = K.picker, selBtn = h('button', { class: 'ghost', 'aria-pressed': 'false' }, t('Sélectionner')), selBar = h('div', { class: 'bar', hidden: true });
  const visible = () => [...P.all].filter(b => b.isConnected && b.offsetParent !== null).map(b => b.dataset.path);
  const paint = () => {
    const n = P.set.size, none = !n;
    selBar.replaceChildren(h('strong', {}, t('{0} sélectionnée(s)', n)),
      h('button', { class: 'ghost', onclick: () => { visible().forEach(p => P.set.add(p)); K.mark(); } }, t('Tout sélectionner')),
      h('button', { class: 'ghost', disabled: none, onclick: () => { P.set.clear(); K.mark(); } }, t('Tout désélectionner')),
      h('button', { class: 'ghost', disabled: none, 'aria-label': t('Tourner vers la gauche'), title: t('Tourner vers la gauche'), onclick: () => K.photos().then(p => p.turn([...P.set], -1)) }, '↺'),
      h('button', { class: 'ghost', disabled: none, 'aria-label': t('Tourner vers la droite'), title: t('Tourner vers la droite'), onclick: () => K.photos().then(p => p.turn([...P.set], 1)) }, '↻'),
      h('button', { class: 'ghost danger', disabled: none, onclick: () => K.photos().then(p => p.remove([...P.set])) }, t('Supprimer la sélection ({0})', n)));
  };
  selBtn.onclick = () => {
    P.on = !P.on; P.set.clear(); K.mark();
    selBtn.setAttribute('aria-pressed', String(P.on)); selBtn.textContent = P.on ? t('Terminer la sélection') : t('Sélectionner');
    selBar.hidden = !P.on; root.classList.toggle('sel-on', P.on);
  };
  P.change = paint; paint();
  K.cleanup(() => { P.on = false; P.set.clear(); P.all.clear(); P.lists.clear(); P.change = () => {}; });
  root.replaceChildren(h('h2', {}, t('Bibliothèque')), tabs, h('div', { class: 'bar' }, selBtn), selBar, mem, h('div', { class: 'grid' }, ...views.map(v => v[2])));
  pick('time');
}
