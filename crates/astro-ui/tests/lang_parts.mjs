// Dictionnaires découpés par écran (web/m-lang-<lg>.js + m-lang-<lg>-<écran>.js) : chaque phrase d'une part est bien
// utilisée par son écran (sinon elle ne serait jamais chargée), aucune n'est en double, les langues ont les mêmes parts et
// les mêmes clés, et la liste `parts` des dictionnaires communs correspond aux fichiers.
//   node crates/astro-ui/tests/lang_parts.mjs
import { readFileSync, readdirSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const web = join(dirname(fileURLToPath(import.meta.url)), '..', 'web');
const tmp = mkdtempSync(join(tmpdir(), 'lang-'));
// Les fichiers commencent par 'use strict' et un export : on les charge comme modules depuis une copie .mjs.
const load = async f => { const p = join(tmp, f.replace(/\.js$/, '.mjs')); writeFileSync(p, readFileSync(join(web, f))); return (await import(pathToFileURL(p))).default; };
const files = readdirSync(web).filter(f => /^m-lang-[a-z]{2}(-[a-z]+)?\.js$/.test(f));
const langs = [...new Set(files.map(f => f.slice(7, 9)))];
assert.ok(langs.includes('en') && langs.includes('es'), `langues : ${langs}`);

// La liste des écrans à phrases est exportée par le dictionnaire commun de chaque langue (`parts`), identique partout.
const loadMod = async f => { const p = join(tmp, f.replace(/\.js$/, '.mjs')); writeFileSync(p, readFileSync(join(web, f))); return import(pathToFileURL(p)); };
const declared = new Set((await loadMod('m-lang-en.js')).parts.split(' '));
for (const lg of langs) assert.deepEqual(new Set((await loadMod(`m-lang-${lg}.js`)).parts.split(' ')), declared, `${lg} : même liste de parts`);
const partsOf = lg => new Set(files.filter(f => f.startsWith(`m-lang-${lg}-`)).map(f => f.slice(10, -3)));
for (const lg of langs) assert.deepEqual(partsOf(lg), declared, `${lg} : les parts présentes doivent être celles de la liste parts`);

const forms = k => [k, k.replace(/'/g, "\\'"), k.replace(/"/g, '\\"'), k.replace(/\n/g, '\\n')];
const keys = {};
for (const lg of langs) {
  const common = await load(`m-lang-${lg}.js`);
  const seen = new Set(Object.keys(common));
  keys[lg] = { common: new Set(Object.keys(common)) };
  for (const p of declared) {
    const part = await load(`m-lang-${lg}-${p}.js`), src = readFileSync(join(web, `m-${p}.js`), 'utf8');
    keys[lg][p] = new Set(Object.keys(part));
    for (const k of Object.keys(part)) {
      assert.ok(!seen.has(k), `${lg} : « ${k} » est en double (part ${p})`);
      seen.add(k);
      assert.ok(forms(k).some(x => src.includes(x)), `${lg} : « ${k} » est rangée dans la part ${p} mais m-${p}.js ne l'utilise pas`);
    }
  }
}
for (const p of declared) assert.deepEqual(keys.es[p], keys.en[p], `part ${p} : mêmes clés en anglais et en espagnol`);
const onlyEs = [...keys.es.common].filter(k => !keys.en.common.has(k)), onlyEn = [...keys.en.common].filter(k => !keys.es.common.has(k));
assert.ok(onlyEn.length === 0 && onlyEs.length <= 1, `parts communes : en seul ${onlyEn}, es seul ${onlyEs}`);
console.log(`dictionnaires : ${langs.length} langues, ${declared.size} parts d'écran, ${keys.en.common.size} phrases communes`);
