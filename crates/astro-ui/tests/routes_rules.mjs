// Table des routes (web/m-routes.js) : chaque écran existe, chaque onglet a une entrée, chaque parent existe,
// et chaque lien « #/… » écrit dans l'interface tombe sur une route connue.
//   node crates/astro-ui/tests/routes_rules.mjs
import { readFileSync, readdirSync, writeFileSync, mkdtempSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const web = join(dirname(fileURLToPath(import.meta.url)), '..', 'web');
const tmp = mkdtempSync(join(tmpdir(), 'routes-'));
writeFileSync(join(tmp, 'r.mjs'), readFileSync(join(web, 'm-routes.js')));
const { ROUTES, TABS, ALIASES, resolve, segments, parentOf } = await import(pathToFileURL(join(tmp, 'r.mjs')));

for (const r of ROUTES) {
  if (r.file) assert.ok(existsSync(join(web, r.file)), `${r.path} : ${r.file} manque`);
  if (r.parent) assert.ok(ROUTES.some(x => x.path === r.parent), `${r.path} : parent ${r.parent} inconnu`);
}
for (const [sp, tabs] of Object.entries(TABS)) for (const [id, , , entry] of tabs) {
  assert.ok(ROUTES.some(r => r.path === entry), `onglet ${id} : entrée ${entry} inconnue`);
  assert.ok(ROUTES.some(r => (r.space === sp || r.space === 'both') && (r.tab === id || r.space === 'both')), `onglet ${id} vide`);
}
for (const to of Object.values(ALIASES)) assert.ok(ROUTES.some(r => r.path === to), `alias vers ${to}`);

// Liens écrits dans le code (hors dictionnaires et table).
const cap = { captain: true }, mem = { captain: false };
const seen = new Set();
for (const f of readdirSync(web).filter(f => /\.js$/.test(f) && !/^m-lang|^m-routes|^scan|^jsqr|^invite|^claim|^register|^restore|^stations/.test(f))) {
  const src = readFileSync(join(web, f), 'utf8');
  for (const [, p] of src.matchAll(/#(\/[a-z]*(?:\/[a-z]+)?)(?=[?'"`#\s)]|$)/g)) {
    if (p === '/' && !/href: '#\/'/.test(src)) continue;
    const key = f + p; if (seen.has(key)) continue; seen.add(key);
    const r = resolve(p, cap);
    assert.ok(r.redirect || r.route.path === p || p.startsWith(r.route.path + '/'), `${f} : lien #${p} sans route`);
  }
}
// « ?grand » n'a de sens que sur le tri ; « Décider » (partages) mène au tri aussi.
for (const f of readdirSync(web).filter(f => /^m-(?!lang).*\.js$/.test(f))) assert.ok(!/#\/people\?grand/.test(readFileSync(join(web, f), 'utf8')), `${f} : #/people?grand n'ouvre pas le tri`);
// Comportements clés.
assert.equal(resolve('/people/tri', mem).route.path, '/people/tri');
assert.equal(resolve('/people/tri', mem).tab, 'people');
assert.equal(parentOf(resolve('/people/tri', mem)).path, '/people');
assert.equal(resolve('/station', mem).redirect, '/cockpit');
assert.equal(resolve('/chiffres', mem).redirect, '/money');
assert.equal(resolve('/cockpit', mem).route.path, '/', 'écran Capitaine interdit à un membre');
assert.equal(resolve('/constellation', cap, 'station').tab, 'eco');
assert.equal(resolve('/constellation', cap, 'me').tab, 'me');
assert.equal(parentOf(resolve('/constellation', mem)).path, '/money');
assert.equal(resolve('/constellation', mem, 'station').space, 'me');
assert.deepEqual(segments(resolve('/library', mem)).map(s => s.path), ['/cloud', '/library']);
assert.deepEqual(segments(resolve('/constellation', cap, 'station')).map(s => s.path), ['/constellation', '/coop']);
assert.deepEqual(segments(resolve('/cockpit', cap)), []);
console.log('routes : OK');
