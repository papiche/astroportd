// Règles de la carte animée (m-flow.js) : disposition, tuyaux, soldes rejoués. `node crates/astro-ui/tests/flow_rules.mjs`
import assert from 'node:assert/strict';
import { layout, pipes, balancesAt, W, H } from '../web/m-flow.js';

const nodes = [{ id: 'UPLANETNAME_G1', kind: 'wallet' }, { id: 'TREASURY', kind: 'wallet' }, { id: 'RND', kind: 'wallet' }, { id: 'billets', kind: 'billets' }, { id: 'membres', kind: 'members' }, { id: 'extérieur', kind: 'outside' }];
const pos = layout(nodes);
assert.deepEqual(pos[0], { x: W / 2, y: H / 2 }, 'le maître est au centre');
assert.deepEqual([pos[4].x < W / 2, pos[5].x > W / 2], [true, true], 'membres à gauche, extérieur à droite');
assert.ok(pos[3].x < W / 2 && pos[3].y < H / 2, 'les billets sont dans le coin haut gauche');
assert.ok(pos.every(p => p.x >= 0 && p.x <= W && p.y >= 0 && p.y <= H), 'tout tient dans la carte');
assert.notDeepEqual(pos[1], pos[2], 'deux portefeuilles ne se superposent pas');

const events = [[100, 5, 0, 5000], [200, 0, 1, 2000], [300, 1, 0, 500], [400, 0, 0, 10]];
const p = pipes(events);
assert.equal(p.length, 2, 'un tuyau par paire, dans les deux sens, sans boucle sur soi-même');
assert.equal(p.find(x => x.a === 0 && x.b === 1).total, 2500);

assert.deepEqual(balancesAt(events, 6, 50), [0, 0, 0, 0, 0, 0], 'avant le premier virement');
assert.deepEqual(balancesAt(events, 6, 200), [3000, 2000, 0, 0, 0, -5000]);
assert.deepEqual(balancesAt(events, 6, 1e9), [3500, 1500, 0, 0, 0, -5000], 'la somme des soldes reste nulle');
assert.equal(balancesAt(events, 6, 1e9).reduce((a, b) => a + b, 0), 0);
console.log('flow_rules : ok');
