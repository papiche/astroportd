'use strict';
// Finitions de l'interface, chargées à la demande : tableaux lisibles sur téléphone, raccourcis de la liste de fichiers, préchargement des onglets.
const link = document.createElement('link');
link.rel = 'stylesheet'; link.href = '/ui/m-ux.css';
document.head.append(link);

// Chaque cellule reçoit le nom de sa colonne (affiché par m-ux.css sur petit écran) ; les écrans se redessinent seuls, d'où l'observateur.
function label(root) {
  root.querySelectorAll('table').forEach(t => {
    const heads = [...(t.tHead?.rows[0]?.cells ?? [])].map(c => c.textContent.trim());
    t.querySelectorAll('tbody tr').forEach(tr => [...tr.cells].forEach((c, i) => { if (heads[i]) c.dataset.l = heads[i]; }));
  });
}

// Raccourcis de la liste de fichiers : Maj+clic coche une plage, Ctrl+A coche tout (hors saisie de texte).
function selection() {
  let last = null;
  const boxes = list => [...list.querySelectorAll('input[type=checkbox]')];
  const set = (b, v) => { if (b.checked !== v) { b.checked = v; b.dispatchEvent(new Event('change', { bubbles: true })); } };
  document.addEventListener('click', ev => {
    const b = ev.target;
    if (!(b instanceof HTMLInputElement) || b.type !== 'checkbox' || !b.closest('ul.files')) return;
    const all = boxes(b.closest('ul.files'));
    if (ev.shiftKey && last && all.includes(last)) {
      const [i, j] = [all.indexOf(last), all.indexOf(b)].sort((x, y) => x - y);
      all.slice(i, j + 1).forEach(x => set(x, b.checked));
    }
    last = b;
  });
  document.addEventListener('keydown', ev => {
    const list = document.querySelector('ul.files');
    if (!list || !(ev.ctrlKey || ev.metaKey) || ev.key !== 'a' || ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName) || document.querySelector('dialog[open]')) return;
    ev.preventDefault();
    const all = boxes(list), every = all.every(x => x.checked);
    all.forEach(x => set(x, !every));
  });
}

// Préchargement : survoler ou toucher un onglet télécharge déjà son écran (modulepreload) ; K.mod le trouvera en cache.
function prefetch() {
  const done = new Set(), alias = { health: 'cockpit', members: 'members', net: 'mesh', eco: 'constellation', files: '', home: 'home', people: 'contacts', me: 'account' };
  document.querySelector('nav').addEventListener('pointerover', ev => {
    const r = ev.target.dataset?.r, n = alias[r] ?? r;
    if (!n || done.has(n)) return;
    done.add(n);
    const l = document.createElement('link');
    l.rel = 'modulepreload'; l.href = `/ui/m-${n}.js`;
    document.head.append(l);
  }, { passive: true });
}

export default function ux() {
  prefetch();
  selection();
  const main = document.getElementById('main');
  label(main);
  new MutationObserver(() => label(main)).observe(main, { childList: true, subtree: true });
}
