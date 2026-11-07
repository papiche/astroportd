// Annuaire des stations de l'UPlanet (/stations) : trouver la station la plus proche qui a encore de la place.
// La position du visiteur ne quitte pas ce navigateur : elle ne sert qu'à trier la liste reçue de GET /api/v1/stations.
// Le classement (`rank`) est pur, testé par tests/stations_rank.mjs.

/** Distance orthodromique en km. */
export function distanceKm(a, b) {
  const r = Math.PI / 180, dLat = (b.lat - a.lat) * r, dLon = (b.lon - a.lon) * r;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * r) * Math.cos(b.lat * r) * Math.sin(dLon / 2) ** 2;
  return 12742 * Math.asin(Math.min(1, Math.sqrt(h)));
}

/** Places libres : `Infinity` sans limite annoncée (total 0). */
export function freeSeats(s) {
  return s.total > 0 ? Math.max(0, s.total - s.used) : Infinity;
}

/** Une station peut-elle accueillir ? Ouverte, avec de la place, et joignable (une adresse publique, ou celle-ci). */
export function canHost(s) {
  return !!s.open && freeSeats(s) > 0 && (!!s.join || !!s.self);
}

/**
 * Classe les stations : celles qui accueillent d'abord ; puis, si la position `me` est connue, la plus proche
 * (les stations sans position après les autres) ; à distance égale (±5 km) ou sans position, la plus libre ; enfin la
 * plus récente. Chaque station reçoit `km` (ou `null`).
 */
export function rank(stations, me) {
  const list = stations.map(s => ({ ...s, km: me && s.lat != null && s.lon != null ? distanceKm(me, s) : null }));
  const free = s => { const f = freeSeats(s); return f === Infinity ? 1e9 : f; };
  return list.sort((a, b) => {
    const h = Number(canHost(b)) - Number(canHost(a));
    if (h) return h;
    if (me) {
      if ((a.km == null) !== (b.km == null)) return a.km == null ? 1 : -1;
      if (a.km != null && Math.abs(a.km - b.km) > 5) return a.km - b.km;
    }
    return free(b) - free(a) || (b.at || 0) - (a.at || 0);
  });
}

if (typeof document !== 'undefined') {
  const $ = s => document.querySelector(s);
  const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
  const T = {
    en: { skip: 'Skip to content', title: 'Find my station', lead: 'Your MULTIPASS lives on one of the cooperative’s stations. Pick the closest one to your home that still has room: it is also the one that carries your neighbourhood’s news.', geo: '📍 Locate me', privacy: 'Your position stays in this browser: it only sorts the stations, it is sent to no one.', advice: 'Recommended station', join: 'Create my MULTIPASS here', loading: 'Reading the directory…', note: 'Each station publishes its free seats and approximate position (about 1 km) every few hours. A station silent for more than 8 days disappears.', here: 'Create my MULTIPASS on this station', back: 'Back to login',
      none: 'No station of this UPlanet has published its seats yet.', down: 'Directory unavailable on this station.', located: 'Sorted by distance.', denied: 'Position refused: stations are sorted by free seats.', nogeo: 'Position unavailable on this device.', locating: 'Locating…',
      me: 'this station', unlimited: 'room available', seats: '{0} seat(s) left', full: 'full', closed: 'registration closed', km: '{0} km', seen: 'seen {0}', week: '{0} Ẑ / week', join2: 'Join', far: 'Far away? It still works: everything is reachable from anywhere.', why: '{0} · {1}', noplace: 'No station has room right now. Come back soon, or ask a Captain to open one.', checking: 'Checking that {0} still has room…', h: '{0} h ago', d: '{0} d ago', now: 'just now', unknownpos: 'position not published' },
    es: { skip: 'Ir al contenido', title: 'Encontrar mi estación', lead: 'Su MULTIPASS vive en una estación de la cooperativa. Elija la más cercana a su casa que aún tenga sitio: también es la que lleva las noticias de su barrio.', geo: '📍 Localizarme', privacy: 'Su posición se queda en este navegador: solo sirve para ordenar las estaciones, no se envía a nadie.', advice: 'Estación recomendada', join: 'Crear mi MULTIPASS aquí', loading: 'Leyendo el directorio…', note: 'Cada estación publica cada pocas horas sus plazas libres y su posición aproximada (a 1 km). Una estación callada desde hace más de 8 días desaparece.', here: 'Crear mi MULTIPASS en esta estación', back: 'Volver al inicio de sesión',
      none: 'Ninguna estación de esta UPlanet ha publicado aún sus plazas.', down: 'Directorio no disponible en esta estación.', located: 'Ordenadas por distancia.', denied: 'Posición rechazada: estaciones ordenadas por plazas libres.', nogeo: 'Posición no disponible en este dispositivo.', locating: 'Localizando…',
      me: 'esta estación', unlimited: 'hay sitio', seats: '{0} plaza(s) libre(s)', full: 'completa', closed: 'inscripción cerrada', km: '{0} km', seen: 'vista {0}', week: '{0} Ẑ / semana', join2: 'Unirme', far: '¿Lejos? Funciona igual: todo es accesible desde cualquier lugar.', why: '{0} · {1}', noplace: 'Ninguna estación tiene sitio ahora. Vuelva pronto o pida a un Capitán que abra una.', checking: 'Comprobando que {0} aún tiene sitio…', h: 'hace {0} h', d: 'hace {0} d', now: 'ahora mismo', unknownpos: 'posición no publicada' },
  };
  const fr = { none: 'Aucune station de cette UPlanet n’a encore publié ses places.', down: 'Annuaire indisponible sur cette station.', located: 'Triées par distance.', denied: 'Position refusée : stations triées par places libres.', nogeo: 'Position indisponible sur cet appareil.', locating: 'Localisation…',
    me: 'cette station', unlimited: 'place disponible', seats: '{0} place(s) libre(s)', full: 'complète', closed: 'inscriptions fermées', km: '{0} km', seen: 'vue {0}', week: '{0} Ẑ / semaine', join2: 'Rejoindre', far: 'Loin ? Ça marche quand même : tout est joignable de partout.', why: '{0} · {1}', noplace: 'Aucune station n’a de place en ce moment. Revenez bientôt, ou demandez à un Capitaine d’en ouvrir une.', checking: 'Vérification des places de {0}…', h: 'il y a {0} h', d: 'il y a {0} j', now: 'à l’instant', unknownpos: 'position non publiée' };
  const t = (k, ...a) => a.reduce((s, v, i) => s.replace(`{${i}}`, v), (T[lang] || {})[k] ?? fr[k] ?? k);
  if (lang !== 'fr') {
    document.documentElement.lang = lang;
    for (const el of document.querySelectorAll('[data-t]')) { const v = (T[lang] || {})[el.dataset.t]; if (typeof v === 'string') el.textContent = v; }
    document.title = t('title');
  }
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const ago = at => { const s = Date.now() / 1000 - at; return s < 3600 ? t('now') : s < 172800 ? t('h', Math.round(s / 3600)) : t('d', Math.round(s / 86400)); };
  const seatsText = s => !s.open ? t('closed') : freeSeats(s) === 0 ? t('full') : freeSeats(s) === Infinity ? t('unlimited') : t('seats', freeSeats(s));
  const href = s => s.self ? '/register' : s.join;
  let stations = [], me = null, checked = new Map();

  // Les places annoncées datent de quelques heures : la station conseillée est revérifiée en direct (GET /api/v1/register).
  async function live(s) {
    if (s.self || !s.astroportd || !s.url) return true;
    if (checked.has(s.station)) return checked.get(s.station);
    let ok = true;
    try {
      const r = await fetch(s.url + '/api/v1/register', { cache: 'no-store', signal: AbortSignal.timeout(5000) });
      const j = await r.json();
      ok = r.ok && !!j.open;
    } catch { ok = true; } // injoignable depuis ce navigateur (réseau local, ancien navigateur) : on garde l'annonce
    checked.set(s.station, ok);
    return ok;
  }

  async function render() {
    const ranked = rank(stations, me);
    const list = $('#list'); list.replaceChildren();
    for (const s of ranked) {
      const li = el('li', 'st' + (canHost(s) ? '' : ' full'));
      const h = el('h3', '', s.name);
      if (s.self) h.append(el('span', 'tag me', t('me')));
      if (s.tier && s.tier !== 'light') h.append(el('span', 'tag', s.tier + (s.gpu_gb > 0 ? ' · GPU ' + s.gpu_gb + ' Go' : '')));
      const meta = el('p', 'meta');
      meta.append(el('span', 'seats', seatsText(s)));
      meta.append(el('span', '', s.km != null ? t('km', s.km < 10 ? s.km.toFixed(1) : Math.round(s.km)) : (s.lat == null ? t('unknownpos') : '')));
      if (s.price && Number(s.price) > 0) meta.append(el('span', '', t('week', String(Number(s.price)))));
      meta.append(el('span', '', t('seen', ago(s.at))));
      const act = el('div', 'act');
      if (canHost(s)) { const a = el('a', 'btn', t('join2')); a.href = href(s); act.append(a); }
      li.append(h, act, meta);
      list.append(li);
    }
    $('#info').textContent = ranked.length ? (me ? t('located') : '') : t('none');
    const best = $('#best');
    for (const s of ranked.filter(canHost).slice(0, 4)) {
      $('#info').textContent = t('checking', s.name);
      if (!(await live(s))) { s.open = false; continue; }
      $('#best-name').textContent = s.name + (s.self ? ' (' + t('me') + ')' : '');
      $('#best-why').textContent = t('why', s.km != null ? t('km', s.km < 10 ? s.km.toFixed(1) : Math.round(s.km)) : t('unknownpos'), seatsText(s));
      $('#best-go').href = href(s);
      best.hidden = false;
      $('#info').textContent = me ? t('located') : '';
      return;
    }
    best.hidden = true;
    if (ranked.length) $('#info').textContent = t('noplace');
  }

  function locate() {
    const st = $('#geo-st');
    if (!navigator.geolocation) { st.textContent = t('nogeo'); return; }
    st.textContent = t('locating');
    navigator.geolocation.getCurrentPosition(p => {
      me = { lat: p.coords.latitude, lon: p.coords.longitude };
      st.textContent = '';
      render();
    }, () => { st.textContent = t('denied'); }, { maximumAge: 600000, timeout: 15000 });
  }
  $('#geo').onclick = locate;

  (async () => {
    try {
      const r = await fetch('/api/v1/stations', { cache: 'no-store' });
      const j = await r.json();
      if (!r.ok) { $('#info').textContent = j.error || t('down'); return; }
      stations = j.stations || [];
      await render();
      // Position déjà autorisée pour ce site : tri par distance sans clic.
      const perm = await navigator.permissions?.query({ name: 'geolocation' }).catch(() => null);
      if (perm?.state === 'granted') locate();
    } catch { $('#info').textContent = t('down'); }
  })();
}
