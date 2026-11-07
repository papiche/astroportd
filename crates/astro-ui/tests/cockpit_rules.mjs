// Règles du cockpit du Capitaine (web/m-cockpit.js) : fonction pure, rejouée sans navigateur.
//   node crates/astro-ui/tests/cockpit_rules.mjs
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

// Le dossier web n'a pas de package.json : on recopie le module en .mjs pour que Node le lise comme ESM.
const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../web/m-cockpit.js'), 'utf8');
const tmp = join(mkdtempSync(join(tmpdir(), 'cockpit-')), 'm-cockpit.mjs');
writeFileSync(tmp, src);
const { analyze, setup, g1 } = await import(pathToFileURL(tmp));

const me = { self: true, node_id: 'A', tier: 'Brain', power_score: 60 };
const peer = (extra = {}) => ({ node_id: 'B', status: 'connected', tier: 'Standard', power_score: 20, ...extra });
const healthy = { health: { status: 'ok', subsystems: { db: { status: 'ok' } } }, cs: { nodes: [me, peer()] }, ec: { kind: 'split', week: 3 }, ac: { accounts: [] }, tun: { services: [] }, tk: { kinds: [] }, dash: { p2p: { enabled: true } } };

// 1. Tout va bien : verdict vert, aucune action.
let r = analyze(healthy);
assert.equal(r.level, 'ok'); assert.equal(r.todos.length, 0); assert.equal(r.verdict, 'Tout va bien');
assert.ok(Object.values(r.floors).every(l => l === 'ok'));

// 2. Données manquantes (appels qui échouent) : jamais d'exception, jamais de faux rouge.
r = analyze({}); assert.equal(r.level, 'ok'); assert.equal(r.todos.length, 0);

// 3. Une panne passe avant tout et colore son étage.
r = analyze({ ...healthy, health: { subsystems: { relay: { status: 'down', detail: 'LMDB' }, db: { status: 'degraded' } } } });
assert.equal(r.level, 'down'); assert.equal(r.todos[0].level, 'down'); assert.equal(r.floors.station, 'down'); assert.equal(r.verdict, 'Intervention nécessaire');

// 4. Station seule : information, pas d'alerte ; P2P arrêté : à regarder. Le code des autres stations n'est pas comparé.
r = analyze({ ...healthy, cs: { nodes: [me] } }); assert.equal(r.level, 'ok'); assert.equal(r.todos[0].level, 'info'); assert.equal(r.floors.mesh, 'info');
r = analyze({ ...healthy, dash: { p2p: { enabled: false } } }); assert.equal(r.level, 'degraded'); assert.equal(r.floors.mesh, 'degraded');

// 5. Adresses de la coopérative : non activées = information ; en échec = à regarder ; jamais lancées = rien.
r = analyze({ ...healthy, dns: { configured: false } }); assert.equal(r.level, 'ok'); assert.equal(r.todos[0].level, 'info');
r = analyze({ ...healthy, dns: { configured: true, failed: 2, errors: ['x : OVH : refusé'] } }); assert.equal(r.level, 'degraded'); assert.equal(r.floors.mesh, 'degraded');
r = analyze({ ...healthy, dns: { configured: null } }); assert.equal(r.todos.length, 0);

// 6. Services : SSH ouvert à tous, service réglé qui ne répond plus. Un service jamais réglé et éteint ne dit rien.
const svc = (o) => ({ name: 'ollama', mode: 'friends', grants: [], customized: false, up: true, ...o });
r = analyze({ ...healthy, tun: { services: [svc({ name: 'ssh', mode: 'swarm' })] } }); assert.equal(r.floors.services, 'degraded');
r = analyze({ ...healthy, tun: { services: [svc({ up: false, customized: true })] } }); assert.equal(r.todos.length, 1);
r = analyze({ ...healthy, tun: { services: [svc({ up: false })] } }); assert.equal(r.todos.length, 0);

// 7. Membres : disque plein, PASS verrouillé, période de grâce ; illimité ignoré.
const acc = (o) => ({ quota_bytes: 100, used_bytes: 10, ...o });
r = analyze({ ...healthy, ac: { accounts: [acc({ used_bytes: 95 }), acc({ used_bytes: 99, unlimited: true }), acc({ pass_locked_until: 1 }), acc({ grace_ends_at: 1 })] } });
assert.equal(r.todos.length, 3); assert.equal(r.floors.members, 'degraded');
// PASS verrouillé : un bouton par compte qui débloque directement (3 au plus, puis un renvoi vers la table).
r = analyze({ ...healthy, ac: { accounts: [acc({ pass_locked_until: 1, hex: 'aa', email: 'lea@x' })] } });
assert.deepEqual(r.todos[0].title, ['Le PASS de {0} est verrouillé', 'lea@x']); assert.equal(r.todos[0].post, '/api/v1/station/accounts/aa/unlock-pass');
r = analyze({ ...healthy, ac: { accounts: [1, 2, 3, 4, 5].map(i => acc({ pass_locked_until: 1, hex: 'h' + i })) } });
assert.equal(r.todos.filter(x => x.post).length, 3); assert.deepEqual(r.todos[3].title, ['{0} autre(s) compte(s) verrouillé(s)', 2]);

// 8. Tâches abandonnées et économie.
r = analyze({ ...healthy, tk: { kinds: [{ failed: 2 }, { failed: 1 }] } }); assert.match(r.todos[0].title, /^3 tâche/);
r = analyze({ ...healthy, ec: { kind: 'drawdown', week: 9 } }); assert.equal(r.floors.economy, 'degraded');

// 9. Ordre : les pannes d'abord, puis « à regarder », puis les informations.
r = analyze({ ...healthy, cs: { nodes: [me] }, health: { subsystems: { db: { status: 'down' } } }, tk: { kinds: [{ failed: 1 }] } });
assert.deepEqual(r.todos.map(x => x.level), ['down', 'degraded', 'info']);
// 10. Mise en service : station neuve = 5 étapes à faire ; tout fait = rien à proposer ; sources absentes = pas d'exception.
let s = setup({ dash: { station: {}, p2p: { enabled: false } }, cs: { nodes: [me] }, ac: { accounts: [{}] }, dns: { configured: false } });
assert.equal(s.list.length, 5); assert.equal(s.left, 5);
s = setup({ dash: { station: { domain: 'x.org' }, p2p: { enabled: true } }, cs: { nodes: [me, peer()] }, ac: { accounts: [{}, {}] }, dns: { configured: true } });
assert.equal(s.left, 0);
assert.equal(setup({}).left, 5);
// Carburant : montants en Ğ1 depuis des centimes, sans flottant.
assert.equal(g1(300), '3,00'); assert.equal(g1(5), '0,05'); assert.equal(g1(123457), '1234,57');
console.log('cockpit : règles OK');
