'use strict';
// Colonne « Code » de la carte Constellation (chargée avec l'écran Station) : le code exécuté par chaque station, comparé au vôtre.
const h = (tag, props, ...kids) => { const e = document.createElement(tag); for (const [k, v] of Object.entries(props || {})) e.setAttribute(k, v); e.append(...kids.flat(2).filter(x => x != null)); return e; };
const pill = (tone, text) => h('span', { class: 'pill ' + tone }, text);
const CODE = { self: ['ok', 'ce binaire'], identical: ['ok', 'identique'], diverges: ['down', 'empreinte différente'], other_version: ['degraded', 'autre version'], other_target: ['degraded', 'autre plateforme'], unknown: ['disabled', 'non attesté'] };
// Code exécuté par une station : version et début de l'empreinte SHA-256 (l'attestation est déclarative, signée par son NODE).
export function codeCell(b, t) {
const [tone, label] = CODE[b?.verdict] || CODE.unknown;
return [pill(tone, t(label)), b?.version ? h('small', { class: 'muted' }, ` ${b.version}${b.sha256 ? ` · ${b.sha256.slice(0, 10)}` : ''}${b.target ? ` · ${b.target}` : ''}`) : null];
}
export function codeSummary(nodes, t) {
const n = v => nodes.filter(x => x.build?.verdict === v).length, bad = n('diverges'), rest = n('other_version') + n('unknown');
if (nodes.length < 2) return null;
return h('p', { class: bad ? 'down' : rest ? 'degraded' : 'muted', role: bad ? 'alert' : 'status' }, bad ? t('{0} station(s) ont une empreinte différente de la vôtre pour la même version : binaire recompilé ou modifié, à vérifier.', bad) : t('Code : {0} station(s) identique(s) à la vôtre', n('identical')) + (rest ? t(', {0} autre(s) version(s) ou sans attestation', rest) : '') + '.');
}
