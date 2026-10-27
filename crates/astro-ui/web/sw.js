'use strict';
// Service worker d'Astroport : seule la COQUE de l'application (page, CSS, JS, icône) est mise en cache, pour un
// démarrage instantané et un écran qui s'ouvre hors ligne. Les données (API, WebDAV, liens magiques, santé,
// WebSocket) ne passent JAMAIS par le cache : elles sont privées et doivent toujours venir de la station.
const V = 'astroport-v1', SHELL = ['/', '/ui/app.css', '/ui/app.js', '/ui/icon.svg', '/ui/manifest.webmanifest'];
self.addEventListener('install', e => e.waitUntil(caches.open(V).then(c => Promise.all(SHELL.map(u => fetch(new Request(u, { cache: 'reload', headers: { Accept: 'text/html,*/*' } })).then(r => (r.ok ? c.put(u, r) : 0))))).then(() => self.skipWaiting())));
self.addEventListener('activate', e => e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k !== V).map(k => caches.delete(k)))).then(() => self.clients.claim())));
self.addEventListener('fetch', e => {
  const r = e.request, u = new URL(r.url), page = r.mode === 'navigate' && u.pathname === '/';
  if (r.method !== 'GET' || u.origin !== location.origin || !(page || (u.pathname.startsWith('/ui/') && u.pathname !== '/ui/sw.js'))) return;
  const key = page ? '/' : u.pathname;
  // Cache d'abord (démarrage immédiat), réseau en parallèle pour rafraîchir la copie.
  e.respondWith(caches.open(V).then(async c => {
    const hit = await c.match(key, { ignoreVary: true });
    const net = fetch(r).then(res => { if (res.ok) c.put(key, res.clone()); return res; }).catch(() => null);
    return hit || (await net) || new Response('Hors ligne : la station est injoignable.', { status: 503, headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
  }));
});
