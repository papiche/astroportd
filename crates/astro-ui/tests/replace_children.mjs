// `replaceChildren` est la méthode du DOM, pas le h() de l'application : elle n'écarte pas `null` (qu'elle écrit « null »)
// et n'aplatit pas un tableau (qu'elle écrit « [object HTMLParagraphElement],… »). Ce test refuse, dans tous les écrans,
// un argument de premier niveau qui vaut `cond ? x : null` (ou `: false`) ou un tableau littéral : il faut l'étaler
// (`...(cond ? [x] : [])`) ou filtrer (`.filter(Boolean)`). Défaut vu en vrai : « Qui est-ce alors ? null » dans Trier en grand.
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const web = join(dirname(fileURLToPath(import.meta.url)), '../web');

// Arguments de premier niveau de l'appel dont la parenthèse ouvrante précède `i` (chaînes et gabarits sautés).
function args(s, i) {
  const out = []; let cur = '', depth = 0, q = null;
  for (let j = i; j < s.length; j++) {
    const c = s[j];
    if (q) { cur += c; if (c === '\\') { cur += s[++j]; continue; } if (c === q) q = null; continue; }
    if (c === '\'' || c === '"' || c === '`') { q = c; cur += c; continue; }
    if ('([{'.includes(c)) { depth++; cur += c; continue; }
    if (')]}'.includes(c)) { if (depth === 0) { out.push(cur); return out; } depth--; cur += c; continue; }
    if (c === ',' && depth === 0) { out.push(cur); cur = ''; continue; }
    cur += c;
  }
  return out;
}
// Le premier niveau d'un argument : le contenu des parenthèses et crochets imbriqués est retiré (h() y filtre déjà).
function top(a) {
  let out = '', d = 0, q = null;
  for (const c of a) {
    if (q) { if (c === q) q = null; continue; }
    if (c === '\'' || c === '"' || c === '`') { q = c; continue; }
    if ('([{'.includes(c)) { if (d === 0) out += c; d++; continue; }
    if (')]}'.includes(c)) { d--; if (d === 0) out += c; continue; }
    if (d === 0) out += c;
  }
  return out.trim();
}

const bad = [];
for (const f of readdirSync(web).filter(n => n.endsWith('.js'))) {
  const s = readFileSync(join(web, f), 'utf8');
  for (const m of s.matchAll(/\.replaceChildren\(/g)) {
    for (const a of args(s, m.index + m[0].length)) {
      const t = top(a);
      if (t.startsWith('...')) continue;
      if (/:\s*(null|false)\b|\?\s*\[|^\[/.test(t)) bad.push(`${f}:${s.slice(0, m.index).split('\n').length} : ${a.trim().slice(0, 80)}`);
    }
  }
}
assert.deepEqual(bad, [], `replaceChildren reçoit null/false ou un tableau (étalez ou filtrez) :\n${bad.join('\n')}`);
console.log('replaceChildren : aucun « null » ni tableau écrit en texte');
