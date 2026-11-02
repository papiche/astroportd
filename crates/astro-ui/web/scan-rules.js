'use strict';
// Règles de la page /scan, sans navigateur : ce que contient un QR lu, et comment afficher un montant ou une adresse.

const SS58 = /^g1[1-9A-HJ-NP-Za-km-z]{44,52}$/;
const BASE58_V1 = /^[1-9A-HJ-NP-Za-km-z]{43,44}$/;

// Un QR de billet contient : une part (64 hexadécimaux), un lien de vérification `…/scan?g1pub=…`, ou une adresse Ğ1.
export function classify(text) {
  const t = String(text || '').trim();
  if (/^[0-9a-fA-F]{64}$/.test(t)) return { kind: 'part', value: t.toLowerCase() };
  if (SS58.test(t) || BASE58_V1.test(t)) return { kind: 'address', value: t };
  try {
    const u = new URL(t);
    const g = u.searchParams.get('g1pub') || u.searchParams.get('dest');
    if (g && (SS58.test(g) || BASE58_V1.test(g))) return { kind: 'address', value: g };
    if (/^g1[1-9A-HJ-NP-Za-km-z]{44,52}$/.test(u.hash.slice(1))) return { kind: 'address', value: u.hash.slice(1) };
  } catch { /* pas une adresse web */ }
  return { kind: 'unknown', value: t.slice(0, 40) };
}

// Centimes → « 5,00 » (séparateur français), entiers uniquement.
export function amount(cents, locale = 'fr-FR') {
  const c = Math.max(0, Math.round(Number(cents) || 0));
  const dec = (0.5).toLocaleString(locale).charAt(1);
  return `${Math.floor(c / 100).toLocaleString(locale)}${dec}${String(c % 100).padStart(2, '0')}`;
}

export function shortAddr(a) { return a && a.length > 16 ? `${a.slice(0, 6)}…${a.slice(-6)}` : (a || ''); }

// État d'un billet → clé de traduction (`v_<état>`) ; `ok` = on peut l'encaisser.
export function verdict(v) {
  return v.status === 'ready' ? { ok: true, key: '' } : { ok: false, key: 'v_' + (v.status === 'returned' ? 'expired' : ['expired', 'redeemed', 'pending', 'foreign'].includes(v.status) ? v.status : 'empty') };
}
