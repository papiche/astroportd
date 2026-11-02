// Règles de la page /scan (web/scan-rules.js) : lecture d'un QR de billet, montants, verdicts. `node crates/astro-ui/tests/scan_rules.mjs`
import assert from 'node:assert/strict';
import { classify, amount, shortAddr, verdict } from '../web/scan-rules.js';

const part = '0208121c1a202a3c3248425c5a506a7c6268929c9a808abcb2a8a2dcdad0cafc';
const ss58 = 'g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq', v1 = '6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN';
assert.deepEqual(classify(part), { kind: 'part', value: part });
assert.deepEqual(classify('  ' + part.toUpperCase() + '\n'), { kind: 'part', value: part }, 'casse et espaces libres');
assert.deepEqual(classify(ss58), { kind: 'address', value: ss58 });
assert.deepEqual(classify(v1), { kind: 'address', value: v1 }, 'clé v1 en base58');
assert.deepEqual(classify(`https://station.exemple/scan?g1pub=${ss58}`), { kind: 'address', value: ss58 }, 'lien de vérification du billet');
assert.equal(classify('https://station.exemple/scan').kind, 'unknown');
assert.equal(classify('https://exemple.org/?g1pub=<script>').kind, 'unknown', 'une adresse invalide dans le lien est refusée');
assert.equal(classify('0208').kind, 'unknown');
assert.equal(classify(part.slice(0, 63) + 'z').kind, 'unknown', '64 caractères dont un non hexadécimal');
assert.equal(classify(null).kind, 'unknown');

assert.equal(amount(500), '5,00');
assert.equal(amount(5), '0,05');
assert.equal(amount(123456), '1 234,56');
assert.equal(amount(-3), '0,00');
assert.equal(amount('x'), '0,00');
assert.equal(shortAddr(ss58), 'g1LsRG…qZDq'.replace('qZDq', ss58.slice(-6)));
assert.equal(shortAddr('court'), 'court');

assert.deepEqual(verdict({ status: 'ready' }), { ok: true, key: '' });
for (const st of ['expired', 'redeemed', 'pending', 'foreign']) assert.deepEqual(verdict({ status: st }), { ok: false, key: 'v_' + st });
assert.equal(verdict({ status: 'empty' }).key, 'v_empty');

// Chaque langue a les mêmes clés que le français (rien ne reste sans traduction).
import { readFileSync } from 'node:fs';
const read = f => readFileSync(new URL('../web/' + f, import.meta.url), 'utf8');
const keys = text => new Set([...text.matchAll(/(?:^|[\s,{])([A-Za-z_0-9]+):/g)].map(m => m[1]));
const fr = read('scan-i18n.js'), kfr = keys(fr.slice(fr.indexOf('const fr = {'), fr.indexOf('\n};\n')));
for (const n of ['en', 'es']) assert.deepEqual([...kfr].filter(x => !keys(read(`scan-lang-${n}.js`)).has(x)), [], `clés absentes en ${n}`);
assert.ok(kfr.size > 60 && kfr.has('billet_expired') && kfr.has('hintFrame'));
console.log('scan_rules : ok');
