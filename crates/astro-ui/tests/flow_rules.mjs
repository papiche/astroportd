// Règles de la carte animée (m-flow.js) : disposition, tuyaux, soldes rejoués. `node crates/astro-ui/tests/flow_rules.mjs`
import assert from 'node:assert/strict';
import { layout, pipes, balancesAt, label, W, H } from '../web/m-flow.js';

const nodes = [{ id: 'UPLANETNAME_G1', kind: 'wallet' }, { id: 'TREASURY', kind: 'wallet' }, { id: 'RND', kind: 'wallet' }, { id: 'billets', kind: 'billets' }, { id: 'membres', kind: 'members' }, { id: 'extérieur', kind: 'outside' },
  { id: 'HOSTING', kind: 'wallet' }, { id: 'INTRUSION', kind: 'wallet' }, { id: 'g1PbEcAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAKqaZ', kind: 'intrus' }];
const pos = layout(nodes);
assert.ok(pos[0].y < Math.min(...pos.slice(1).map(p => p.y)) + 1 && Math.abs(pos[0].x - (W / 2 - 40)) < 1, 'le maître est en haut de la boucle');
assert.ok(pos[1].x > pos[0].x, 'CASH suit le maître dans le sens des aiguilles d\'une montre');
assert.ok(pos[4].y > H / 2, 'les membres sont en bas de la boucle');
assert.ok(pos[5].x > W * .8 && pos[5].y < H / 3, 'l\'extérieur est en haut à droite, près de CASH');
assert.ok(pos[7].x > W * .8 && pos[7].y > H * .8 && pos[8].x > W * .8, 'INTRUSION et les intrus sont à part, en bas à droite');
assert.ok(pos.every(p => p.x >= 0 && p.x <= W && p.y >= 0 && p.y <= H), 'tout tient dans la carte');
assert.equal(new Set(pos.map(p => `${p.x.toFixed(0)},${p.y.toFixed(0)}`)).size, pos.length, 'aucun nœud ne se superpose');
assert.equal(label(nodes[8]), 'g1PbEc…KqaZ'); assert.equal(label(nodes[0]), 'Ğ1 maître'); assert.equal(label(nodes[1]), 'CASH');

const events = [[100, 5, 0, 5000], [200, 0, 1, 2000], [300, 1, 0, 500], [400, 0, 0, 10]];
const p = pipes(events);
assert.equal(p.length, 2, 'un tuyau par paire, dans les deux sens, sans boucle sur soi-même');
assert.equal(p.find(x => x.a === 0 && x.b === 1).total, 2500);

assert.deepEqual(balancesAt(events, 6, 50), [0, 0, 0, 0, 0, 0], 'avant le premier virement');
assert.deepEqual(balancesAt(events, 6, 200), [3000, 2000, 0, 0, 0, -5000]);
assert.deepEqual(balancesAt(events, 6, 1e9), [3500, 1500, 0, 0, 0, -5000], 'la somme des soldes reste nulle');
assert.equal(balancesAt(events, 6, 1e9).reduce((a, b) => a + b, 0), 0);
console.log('flow_rules : ok');
