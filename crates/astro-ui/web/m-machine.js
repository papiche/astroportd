'use strict';
// Station › Santé › Détails techniques : nœud, relais, sous-systèmes. Ancien écran « Station » (#/station).
export default async function machine(root, K) {
  const { h, t, api } = K;
  const row = (k, v) => [h('dt', {}, k), h('dd', {}, v)];
  const draw = async () => {
    const [d, z] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json())]);
    const s = d.station, r = d.relay, sub = z.subsystems || {};
    root.replaceChildren(h('h2', {}, t('Détails techniques')), h('div', { class: 'grid' },
      h('section', { class: 'card' }, h('h2', {}, t('Station'), ' ', K.pill(z.status)),
        h('dl', {}, row(t('Nœud'), s.node), row('PeerID', s.peer_id || '—'), row(t('Domaine'), s.domain || '—'), row(t('Version'), s.version), row(t('En marche depuis'), K.span(s.uptime_secs)))),
      h('section', { class: 'card' }, h('h2', {}, t('Relais'), ' ', K.pill(sub.relay?.status || 'ok')),
        h('p', { class: 'big' }, String(r.events ?? '—'), h('small', { class: 'muted' }, ' ' + t('événements'))),
        h('dl', {}, row(t('Dernier événement'), K.when(r.last_event_at)), row(t('Base de données'), K.bytes(r.db_bytes)))),
      h('section', { class: 'card' }, h('h2', {}, t('Sous-systèmes')), h('dl', {}, Object.entries(sub).flatMap(([n, v]) => row(n, [K.pill(v.status), ' ', v.detail]))))));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
