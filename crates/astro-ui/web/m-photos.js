'use strict';
// Actions sur des photos : quart de tour (JPEG sans perte par l'EXIF) et suppression. Chargé à la demande par la visionneuse et la bibliothèque.

export default c => {
  const { api, toast, t, h, blobs, del, picker, mark, viewer, cur, show } = c, names = p => p.split('/').pop();
  // Recharge les vignettes d'une photo modifiée (la miniature est refaite en arrière-plan : on revient un peu plus tard).
  const refreshPhotos = paths => {
    const go = () => { for (const p of paths) { blobs.delete('t:' + p); blobs.delete('o:' + p); } for (const b of picker.all) if (paths.includes(b.dataset.path) && b._go) b._go(); };
    go(); setTimeout(go, 1500); setTimeout(go, 5000);
  };
  async function turn(paths, q) {
    try {
      const r = await api('/api/v1/ucloud/rotate', { method: 'POST', body: { paths, turn: q } });
      toast(t('{0} photo(s) tournée(s)', r.rotated) + (r.failed.length ? ' · ' + r.failed[0].error : ''), r.failed.length ? 0 : 1);
      refreshPhotos(paths);
      const v = cur();
      if (viewer.open && paths.includes(v.items[v.at]?.path)) setTimeout(() => show(v.at), 1600);
    } catch (e) { toast(e.message, 0); }
  }
  async function remove(paths) {
    if (!confirm(t('Supprimer définitivement {0} fichier(s) ?', paths.length))) return false;
    const ok = new Set(); let last = '';
    for (const p of paths) { try { const r = await del(p); if (!r.ok && r.status !== 404) throw new Error(`${names(p)} : ${r.status}`); ok.add(p); blobs.delete('t:' + p); blobs.delete('o:' + p); picker.set.delete(p); } catch (e) { last = e.message; } }
    for (const b of picker.all) if (ok.has(b.dataset.path)) { b.remove(); picker.all.delete(b); }
    for (const l of picker.lists) for (let i = l.length - 1; i >= 0; i--) if (ok.has(l[i].path)) l.splice(i, 1);
    if (viewer.open) { const v = cur(); show(Math.min(v.at, v.items.length - 1)); }
    toast(t('{0} fichier(s) supprimé(s)', ok.size) + (last ? ' · ' + last : ''), last ? 0 : 1);
    mark();
    return ok.size > 0;
  }
  // Boutons de la visionneuse pour la photo `it`.
  const actions = (it, isImg) => [
    ...(isImg ? [[-1, '↺', 'Tourner vers la gauche'], [1, '↻', 'Tourner vers la droite']].map(([q, g, l]) => h('button', { class: 'ghost', 'aria-label': t(l), title: t(l), onclick: () => turn([it.path], q) }, g)) : []),
    h('button', { class: 'ghost danger', onclick: () => remove([it.path]) }, t('Supprimer'))];
  return { turn, remove, actions };
};
