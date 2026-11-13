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
const { steps, news } = await import(pathToFileURL(tmp));

// Compte neuf : rien de fait, tout est à proposer.
let r = steps({ dash: { sovereignty: { encrypted_files: 0 } }, faces: { enabled: true, people: [] }, friends: { friends: [] }, backup: { mine: null }, me: { ai: false } });
assert.equal(r.list.length, 6); assert.equal(r.left, 6); assert.equal(r.files, 0);
// Sans assistant, l'étape smartphone vient tout de suite après les photos (quitter Google et Apple).
assert.equal(r.list[1].title, 'Synchroniser votre smartphone');

// Tout fait : plus rien à proposer.
r = steps({ dash: { sovereignty: { encrypted_files: 12 } }, faces: { enabled: true, people: [{}] }, friends: { friends: [{ hex: 'a' }] }, backup: { mine: { status: 'accepted' } }, me: { ai: true }, devices: { devices: [{}] } });
assert.equal(r.left, 0); assert.equal(r.files, 12);

// Sauvegarde seulement demandée (pas acceptée) : pas cochée.
r = steps({ dash: {}, faces: { enabled: true, people: [] }, friends: {}, backup: { mine: { status: 'pending' } }, me: {} });
assert.equal(r.list.find(s => s.title.startsWith('Mettre')).done, false);

// Reconnaissance des visages absente de la station : l'étape n'existe pas.
r = steps({ dash: {}, faces: { enabled: false }, friends: {}, backup: null, me: {} });
assert.equal(r.list.length, 5); assert.ok(!r.list.some(s => s.title.includes('visage')));

// Appels qui échouent (null) : jamais d'exception.
r = steps({}); assert.equal(r.list.length, 5); assert.equal(r.left, 5);
console.log('accueil : étapes OK');

// « Pour vous » : rien à faire = encart vide ; partages en attente d'abord, puis ressemblances (avant les visages inconnus).
assert.deepEqual(news({ faces: null, shares: null }), []);
let n = news({ faces: { suggestions: [{}, {}], unknown: 9 }, shares: { pending: [{ count: 12, name: 'Fred' }] } });
assert.equal(n.length, 2); assert.deepEqual(n[0].text.slice(1), [12, 'Fred']); assert.equal(n[1].href, '#/people/tri?grand'); assert.deepEqual(n[1].text, ['{0} visage(s) à confirmer', 2]);
assert.deepEqual(news({ faces: { suggestions: [], unknown: 4 } })[0].text, ['{0} visage(s) à nommer', 4]);
// Un ami en secours passe avant tout le reste.
n = news({ faces: { suggestions: [{}], unknown: 0 }, shares: { pending: [] }, backup: { hosting: [{ label: 'Léa', rescue: { station: 'x' } }, { label: 'Max' }] } });
assert.deepEqual(n.map(x => x.icon), ['🆘', '🫂']); assert.equal(n[0].href, '#/account?tab=securite'); assert.deepEqual(n[0].text[1], 'Léa');
console.log('accueil : « Pour vous » OK');
