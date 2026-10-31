'use strict';
// Style (Aurore, Sobre, Kraft) et mode (clair, sombre) choisis dans l'application : appliqués aussi aux pages autonomes.
try {
  const style = localStorage.getItem('style'), mode = localStorage.getItem('mode'), root = document.documentElement;
  root.dataset.style = ['aurore', 'sobre', 'kraft'].includes(style) ? style : 'aurore';
  if (mode === 'light' || mode === 'dark') root.dataset.mode = mode;
} catch { /* stockage indisponible : style par défaut */ }
