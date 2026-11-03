'use strict';
// Photo ENTIÈRE d'un visage, le visage entouré : pour savoir qui est désigné sur une photo de groupe (table de tri, Trier en grand,
// fiche d'une personne). `bbox` est dans le repère de l'image tournée selon l'EXIF, comme ce qu'affiche le navigateur : on le
// place en pourcentages de la taille naturelle de l'image. `f` : { path, bbox }.

// Feuille de style chargée une fois, à la première photo (hors coque : seuls les écrans de visages en ont besoin).
export function css(K) {
  if (!document.querySelector('link[href="/ui/m-framed.css"]')) document.head.append(K.h('link', { rel: 'stylesheet', href: '/ui/m-framed.css' }));
}

export function framed(K, f) {
  css(K);
  const ring = K.h('span', { class: 'fr-ring', hidden: true }), img = K.h('img', { alt: f.path.split('/').pop() }), box = K.h('span', { class: 'fr-framed' }, img, ring);
  img.onload = () => {
    const [x1, y1, x2, y2] = f.bbox, W = img.naturalWidth, H = img.naturalHeight;
    Object.assign(ring.style, { left: x1 / W * 100 + '%', top: y1 / H * 100 + '%', width: (x2 - x1) / W * 100 + '%', height: (y2 - y1) / H * 100 + '%' });
    ring.hidden = false;
  };
  K.blob(f.path).then(u => { img.src = u; }, e => box.replaceChildren(K.h('p', { class: 'err' }, K.t('Photo indisponible : ') + e.message)));
  return box;
}

// La même photo en grand, dans un dialogue ; « Ouvrir dans la visionneuse » garde la navigation de l'uCloud.
export function showFace(K, f, caption) {
  const { h, t } = K;
  const d = h('dialog', { class: 'fr-photo', 'aria-label': t('Photo du visage') },
    h('form', { method: 'dialog' }, h('button', { class: 'x', 'aria-label': t('Fermer') }, '×')), framed(K, f),
    h('p', { class: 'muted fr-cap' }, caption || f.path),
    h('menu', {}, h('button', { type: 'button', class: 'ghost', onclick: () => { d.close(); K.open([{ path: f.path, mime: 'image/jpeg', mtime: 0 }], 0); } }, t('Ouvrir dans la visionneuse'))));
  d.addEventListener('close', () => d.remove());
  document.body.append(d); d.showModal();
}
