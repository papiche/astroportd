'use strict';
// Style (Aurore, Sobre, Kraft), mode (auto, clair, sombre) et présentation de l'UPlanet : un seul fichier pour l'application et
// les pages autonomes, chargé avant elles. Kraft est RÉSERVÉ à ORIGIN (on voit d'un coup d'œil qu'on est dans le bac à sable
// public) ; une UPlanet privée a son style par défaut et peut renommer sa monnaie (configuration coopérative). `/api/v1/look`
// (public) dit qui on est ; la dernière réponse est gardée pour ne pas changer de style à chaque chargement. `window.astroLook`
// la donne à l'application (nom de la monnaie, nom de l'UPlanet).
{
  const root = document.documentElement, $ = id => document.getElementById(id);
  const get = k => { try { return localStorage.getItem(k); } catch { return null; } }, set = (k, v) => { try { localStorage.setItem(k, v); } catch { /* stockage indisponible */ } };
  const MODES = ['auto', 'light', 'dark'], ICON = { auto: '◐', light: '☀', dark: '☾' };
  let look = {}; try { look = JSON.parse(get('look')) || {}; } catch { /* rien en cache */ }
  const apply = () => {
    const ok = look.origin ? ['kraft'] : ['aurore', 'sobre'], style = get('style'), m = get('mode'), mode = MODES.includes(m) ? m : 'auto';
    root.dataset.style = ok.includes(style) ? style : ok.includes(look.style) ? look.style : ok[0];
    if (mode === 'auto') delete root.dataset.mode; else root.dataset.mode = mode;
    const pick = $('style'), btn = $('mode'), brand = document.querySelector('.brand');
    if (pick) { for (const o of pick.options) o.hidden = !ok.includes(o.value); pick.hidden = ok.length < 2; pick.value = root.dataset.style; }
    if (btn) btn.textContent = ICON[mode];
    if (brand) {
      brand.querySelector('.up')?.remove();
      if (look.name) { const s = document.createElement('small'); s.className = 'up'; s.title = look.origin ? 'ORIGIN : réseau public d\'essai' : 'UPlanet'; s.textContent = look.name; brand.append(s); }
    }
    window.astroLook = look;
    return mode;
  };
  const start = () => {
    const mode = apply(), pick = $('style'), btn = $('mode');
    if (pick) pick.onchange = () => { set('style', pick.value); apply(); };
    if (btn) btn.onclick = () => { set('mode', MODES[(MODES.indexOf(apply()) + 1) % MODES.length]); apply(); };
    fetch('/api/v1/look').then(r => (r.ok ? r.json() : null)).then(l => { if (!l) return; look = l; set('look', JSON.stringify(l)); apply(); }, () => { /* hors ligne : le cache suffit */ });
    return mode;
  };
  apply();
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
}
