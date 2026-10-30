'use strict';
// Finitions de l'interface, chargées à la demande : tableaux lisibles sur téléphone et pastille « demande d'aide » du Capitaine.
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

// Pastille à côté de « Station » : nombre de demandes d'aide des Capitaines amis qui attendent une réponse.
function badge(api, t) {
  const station = document.querySelector('nav a[data-r=station]');
  let el = null;
  const draw = n => {
    if (!n) { el?.remove(); el = null; return; }
    if (!el) {
      el = document.createElement('a');
      el.href = '#/options';
      Object.assign(el.style, { background: 'var(--bad)', color: '#fff', borderRadius: '1rem', padding: '0 .55rem', fontSize: '.85rem', textDecoration: 'none', alignSelf: 'center' });
      station.after(el);
    }
    el.textContent = t('Aide {0}', n);
    el.setAttribute('aria-label', t('{0} demande(s) d\'aide en attente de votre réponse', n));
    el.title = el.getAttribute('aria-label');
  };
  const poll = () => api('/api/v1/station/admin/assist').then(v => draw(v.incoming.filter(r => r.status === 'pending').length), () => {});
  poll();
  setInterval(poll, 30000);
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

export default function ux(me, api, t) {
  const cloud = document.querySelector('nav a[data-r=cloud]');
  if (cloud && !document.querySelector('nav a[data-r=library]')) {
    const a = document.createElement('a');
    a.href = '#/library'; a.dataset.r = 'library'; a.textContent = t('Bibliothèque');
    cloud.after(a);
    if (location.hash.startsWith('#/library')) a.setAttribute('aria-current', 'page');
  }
  selection();
  const main = document.getElementById('main');
  label(main);
  new MutationObserver(() => label(main)).observe(main, { childList: true, subtree: true });
  if (me.captain) badge(api, t);
}
