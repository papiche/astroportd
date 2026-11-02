'use strict';
// Textes de la page /scan en français, anglais et espagnol. Les messages de la station arrivent avec un code stable (`billet_expired`…)
// : on traduit d'après le code, jamais d'après le texte. La langue vient du navigateur (français par défaut, anglais hors fr/es).
export const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
export const locale = { fr: 'fr-FR', en: 'en-GB', es: 'es-ES' }[lang];

const fr = {
  mine: e => e ? `👤 Mon compte (${e})` : '👤 Mon compte',
  loginSum: 'Me connecter avec mon email et mon PASS', email: 'Votre adresse email', pass: 'Code PASS', loginGo: 'Me connecter', noWallet: 'Ce compte n’a pas de portefeuille Ğ1 sur cette station.',
  pass_denied: 'Code PASS incorrect.', pass_locked: 'Trop d’essais : réessayez plus tard.',
  title: 'Encaisser un Ğ1Billet', lead: 'Scannez le billet, dites où envoyer sa valeur. Pas de compte à créer.', skip: 'Aller au contenu', steps: 'Étapes',
  st1: 'Scanner', st2: 'Destination', st3: 'Encaisser',
  s1: 'Scannez le billet', camOn: '📷 Ouvrir la caméra', photo: '🖼️ Choisir une photo', manual: 'Saisir le texte à la main', manualLbl: 'Texte lu sous le QR ou adresse du billet', validate: 'Valider', next: 'Continuer',
  s2: 'Où envoyer la valeur ?', s2p: 'Le portefeuille d’un compte de cette UPlanet. Son adresse commence par g1… (visible dans Zelkova ou dans « Mon compte »).', destLbl: 'Adresse du portefeuille', destCam: 'Scanner l’adresse avec la caméra', back: 'Retour',
  s3: 'Confirmer', s3p: 'Le transfert est définitif et vide entièrement le billet.', go: '💰 Encaisser', again: 'Encaisser un autre billet',
  chipBillet: '✓ Billet reconnu', chipRead: n => `✓ QR ${n} lu`, chipTodo: n => `○ QR ${n} à lire`,
  hintFrame: 'Placez le QR du billet dans le cadre.', hintChecking: 'Vérification du billet…', hintReady: 'Billet vérifié. Vous pouvez l’encaisser.',
  hintValue: 'Valeur vérifiée. Pour l’encaisser, lisez les deux QR du billet : celui en clair, puis celui sous le pli.', hintFirst: 'Premier QR lu. Dépliez la bande du billet et lisez le second QR.',
  hintSame: 'Ce QR est déjà lu : lisez l’autre QR du billet (sous le pli).', hintNotBillet: 'Ce QR n’est pas celui d’un billet.', hintNotAddr: 'Ce QR n’est pas une adresse Ğ1 : visez le QR de votre portefeuille.',
  hintMismatch: 'Ces QR ne sont pas ceux du billet vérifié : recommencez avec le même billet.', retry: 'Recommencez la lecture.',
  noCam: 'Caméra indisponible dans ce navigateur : choisissez une photo du QR ou saisissez le texte.', camDenied: 'La caméra est refusée. Autorisez-la dans votre navigateur, ou choisissez une photo du QR.',
  camFail: 'Caméra indisponible : choisissez une photo du QR ou saisissez le texte.', noQr: 'Aucun QR lisible sur cette photo : rapprochez-vous, ou prenez-la à plat et bien éclairée.', badImage: 'Cette image n’a pas pu être lue.',
  zen: 'Ẑen', g1: n => `soit ${n} Ğ1`, authentic: '✅ Billet authentique de cette UPlanet', unverified: '⚠️ Origine non vérifiée pour l’instant', until: d => `⏳ Valable jusqu’au ${d}`,
  sentTo: n => `soit ${n} Ğ1, envoyés à`,
  destNotAddr: 'Ce n’est pas une adresse Ğ1 (elle commence par g1…).', destChecking: 'Vérification…', destOk: a => `✅ Portefeuille reconnu par l’UPlanet (${a})`,
  destNo: '❌ Ce portefeuille n’est pas reconnu par l’UPlanet : le billet ne serait pas touché. Utilisez le portefeuille d’un compte de cette UPlanet.',
  doneT: n => `✅ ${n} Ẑen envoyés`, doneP: a => `La valeur du billet est sur le portefeuille ${a}.`, pendT: '⏳ Billet vidé, transfert en cours',
  pendP: a => `Le billet est vidé : sa valeur est en sécurité sur la coopérative et arrive sur ${a} d’ici quelques minutes, sans autre action de votre part.`,
  down: 'Station injoignable : vérifiez votre connexion.',
  v_expired: 'Ce billet a expiré : sa valeur est revenue à son émetteur.', v_redeemed: 'Ce billet a déjà été encaissé.', v_pending: 'Ce billet est en cours d’encaissement : la station termine le transfert.',
  v_foreign: 'Ce billet n’a pas été émis par cette UPlanet.', v_empty: 'Ce billet est vide.',
  billet_bad_part: 'QR illisible : ce n’est pas une part de billet.', billet_same_part: 'C’est deux fois la même part : lisez l’autre QR du billet.',
  billet_no_match: 'Ces deux QR ne correspondent à aucun billet : vérifiez qu’ils viennent du même billet.', billet_ambiguous: 'Lecture ambiguë : réessayez en lisant le QR en clair, puis celui sous le pli.',
  billet_bad_address: 'Adresse Ğ1 illisible.', billet_two_parts: 'Deux QR sont nécessaires.', billet_bad_op: 'Demande inconnue.', billet_dest_is_billet: 'Le destinataire ne peut pas être le billet lui-même.',
  billet_expired: 'Ce billet a expiré : sa valeur est revenue à son émetteur.', billet_redeemed: 'Ce billet a déjà été encaissé.', billet_pending: 'L’encaissement de ce billet est en cours : la station termine le transfert.',
  billet_foreign: 'Ce billet n’a pas été émis par cette UPlanet.', billet_empty: 'Ce billet est vide ou ne vaut rien.',
  billet_dest_unknown: 'Ce portefeuille n’est pas reconnu par l’UPlanet : le billet n’a pas été touché.', billet_no_indexer: 'Cette station ne peut pas vérifier le billet pour l’instant (indexeur Ğ1 absent).',
  billet_indexer_down: 'Indexeur Ğ1 injoignable : réessayez dans quelques minutes.', billet_rate_limited: 'Trop de demandes : réessayez dans quelques minutes.',
  billet_chain_error: 'La chaîne Ğ1 ne répond pas : réessayez dans quelques minutes.', billet_drain_failed: 'Le vidage du billet a échoué : il n’a pas été touché, réessayez.',
  billet_unknown_outcome: 'L’issue du transfert est inconnue : ne réessayez pas, le Capitaine est prévenu.', billet_unavailable: 'Service momentanément indisponible.', billet_internal: 'Erreur interne de la station.',
};
// Le français est dans la page ; l'anglais et l'espagnol ne se téléchargent que pour qui les lit.
const dict = lang === 'fr' ? fr : (await import(`/ui/scan-lang-${lang}.js`)).default;
export const t = (k, ...a) => { const v = dict[k] ?? fr[k] ?? k; return typeof v === 'function' ? v(...a) : v; };

// Textes fixes de la page : `data-t` (contenu), `data-tl` (aria-label), `data-tp` (placeholder).
export function localize() {
  document.documentElement.lang = lang; document.title = t('title');
  for (const e of document.querySelectorAll('[data-t]')) e.textContent = t(e.dataset.t);
  for (const e of document.querySelectorAll('[data-tl]')) e.setAttribute('aria-label', t(e.dataset.tl));
}
