// Étapes « Bien démarrer » de l'accueil d'un membre (web/m-home.js) : fonction pure, rejouée sans navigateur.
//   node crates/astro-ui/tests/home_steps.mjs
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../web/m-home.js'), 'utf8');
const tmp = join(mkdtempSync(join(tmpdir(), 'home-')), 'm-home.mjs');
writeFileSync(tmp, src);
const { steps } = await import(pathToFileURL(tmp));

// Compte neuf : rien de fait, tout est à proposer.
let r = steps({ dash: { sovereignty: { encrypted_files: 0 } }, faces: { enabled: true, people: [] }, friends: { friends: [] }, backup: { mine: null }, me: { ai: false } });
assert.equal(r.list.length, 5); assert.equal(r.left, 5); assert.equal(r.files, 0);

// Tout fait : plus rien à proposer.
r = steps({ dash: { sovereignty: { encrypted_files: 12 } }, faces: { enabled: true, people: [{}] }, friends: { friends: [{ hex: 'a' }] }, backup: { mine: { status: 'accepted' } }, me: { ai: true } });
assert.equal(r.left, 0); assert.equal(r.files, 12);

// Sauvegarde seulement demandée (pas acceptée) : pas cochée.
r = steps({ dash: {}, faces: { enabled: true, people: [] }, friends: {}, backup: { mine: { status: 'pending' } }, me: {} });
assert.equal(r.list.find(s => s.title.startsWith('Mettre')).done, false);

// Reconnaissance des visages absente de la station : l'étape n'existe pas.
r = steps({ dash: {}, faces: { enabled: false }, friends: {}, backup: null, me: {} });
assert.equal(r.list.length, 4); assert.ok(!r.list.some(s => s.title.includes('visage')));

// Appels qui échouent (null) : jamais d'exception.
r = steps({}); assert.equal(r.list.length, 4); assert.equal(r.left, 4);
console.log('accueil : étapes OK');
