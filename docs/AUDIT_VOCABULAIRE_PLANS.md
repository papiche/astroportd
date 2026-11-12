# Audit : la machinerie des plans ne doit pas fuiter vers le membre

État : **constat, aucun code modifié**. Règle visée : un membre ordinaire a *un compte, un coffre, des amis*. Seul le Capitaine voit les constellations, les plans, les passerelles. (`CLAUDE.md` : « ne JAMAIS afficher de jargon cryptographique à un membre ».)

Méthode : recherche des mots `plan`, `plane`, `constellation`, `UPlanet`, `ORIGIN`, `bridge`, `bassin`, `swarm`, `passerelle` dans toutes les pages web (`crates/astro-ui/web/`) et dans les messages d'erreur renvoyés par l'API, puis, pour chaque occurrence, **qui peut la voir** (les écrans Capitaine sont listés dans `CAP` d'`app.js` ; les routes Capitaine sont protégées côté serveur par `captain_only`).

## Ce qui est correct

- **Écrans Capitaine** (`options`, `mesh`, `planes`, `coop`, `cockpit`, `station`) : `m-planes.js`, `m-coop.js`, `m-mesh.js`, `m-options.js` parlent librement de constellations, `swarm.key`, ORIGIN. C'est l'exception prévue.
- **Sélecteur de plan** (`app.js`, `planeSwitch`) : monté seulement si `me.captain`.
- **Serveur** : les routes `/api/v1/station/planes` et ses sœurs refusent un membre (`captain_only`), et le menu « Station » est masqué hors Capitaine. Un membre ne peut donc ni voir ni appeler la gestion des plans.
- **Vocabulaire** : l'interface dit « constellation », jamais « plan » ; `plan`/`planes` reste un terme interne (noms de fichiers, `planes.json`).

## Les fuites vers le membre

Classées par gravité. « Visible » = affiché à un compte non Capitaine.

| # | Où | Ce que le membre voit | Quand | Gravité |
|---|---|---|---|---|
| 1 | `m-account.js` : carte **« Mes constellations »** (`planes`) | titre « constellations », une clé Nostr « différente dans chaque constellation », **nom du plan + 8 premiers caractères hexadécimaux de chaque clé + `via`**, bouton « Annoncer mes autres comptes » (lien public signé des deux clés) | station à plusieurs plans | **haute** |
| 2 | `m-card.js` : fiche d'un contact, bloc **« Messagerie : clés de cette personne »** | pastille avec le **nom du plan**, « lien signé des deux clés », « clé de la fiche », hex tronqué, boutons « Écrire depuis {plan} » | contact connu dans plusieurs plans | **haute** |
| 3 | `m-chat.js` : liste des conversations | pastilles avec le **nom des plans** (+ ● non lu) sous chaque conversation | conversation à cheval sur plusieurs plans | moyenne |
| 4 | `src/chat.rs:169` (message d'erreur API) | « cette personne n'a pas de clé connue dans l'UPlanet « zen1 » : écrivez-lui depuis zen2 ou zen3 » | écrire à un contact d'un autre plan | moyenne |
| 5 | `m-constellation.js` / `m-home.js:51` : écran **« Ma UPlanet »** (ouvert à tout membre) | « Ğ1 maître », « INTRUSION », « CASH » (= TREASURY), « charte », noms de portefeuilles coopératifs | toujours | moyenne (jargon économique plus que plans) |
| 6 | `register.js`, `invite.js`, `stations.js`, pages scan de billets | « Constellation X · inscription ouverte », « Billet authentique de cette UPlanet », « Aucune station de cette UPlanet… » | nouvel arrivant, avant d'avoir un compte | faible : le mot désigne la communauté, pas la machinerie |
| 7 | `src/invites.rs:814` | notification au parrain : « {nom} a rejoint **la constellation** grâce à votre invitation » | à chaque invitation acceptée | faible |

Les cinq premiers sont **le même problème** : le fait qu'une personne ait *plusieurs identités, une par plan* remonte jusqu'à l'écran du membre. Pour lui, il n'y a qu'une personne.

## Corrections proposées

**A. Ranger les plans derrière « un contact = une personne » (fuites 2, 3, 4).**
Le serveur sait déjà regrouper les clés d'une même personne (`person_keys`). Il suffit que l'API choisisse **seule** la bonne clé et que l'interface n'affiche plus le plan :
- `m-chat.js` : supprimer les pastilles de plan ; une personne = une conversation (les `channels` restent un détail serveur).
- `m-card.js` : remplacer le bloc « clés de cette personne » par le seul bouton **« Écrire »**. Si la personne n'est joignable que depuis un autre plan, un texte humain : « Cette personne n'est pas joignable d'ici pour le moment », sans nommer de plan ni de clé.
- `src/chat.rs:169` : même message humain ; le détail (plan, clés) va dans le bus d'activité **Capitaine** (`Audience::Captain`), pas dans l'erreur.

**B. Rendre l'annonce des liens automatique (fuite 1).**
Au lieu d'une carte « Mes constellations » à actionner à la main dans chaque plan, annoncer le lien signé au moment de la création du compte, avec un consentement formulé simplement : « Me rendre joignable depuis mes autres espaces ? (oui/non) ». La carte disparaît de l'écran membre ; elle subsiste **côté Capitaine** (écran Constellations) pour le dépannage. Compromis à trancher : l'annonce est **publique** (signée par deux clés) ; le consentement doit rester explicite.

**C. Traduire « Ma UPlanet » en langage de membre (fuite 5).**
Pour le membre : « Ma coopérative » avec trois lignes (ce que je possède, ce que je dois, la charte). Les noms `INTRUSION`, `CASH`, `Ğ1 maître`, les adresses de portefeuilles vont dans l'écran Capitaine « Économie ». C'est un sujet à part (jargon économique), mais il appartient au même principe.

**D. Les mots « constellation » / « UPlanet » (fuites 6, 7).**
À garder tant qu'ils désignent la *communauté* qu'on rejoint (« inscription ouverte dans la constellation de Fred »). Vérifier seulement qu'aucun nom technique (`zen1`, `UPLANETNAME`, `swarm`) n'apparaît : c'est le cas aujourd'hui.

## Test qui empêcherait la régression

Un test Node (comme `crates/astro-ui/tests/lang_parts.mjs`) qui charge **les écrans membres** et échoue si une chaîne affichée contient `plan`, `ORIGIN`, `swarm`, `bridge`, `bassin`, un hexadécimal de 8 caractères suivi de `…`. Les écrans Capitaine (liste `CAP`) en sont exclus.

## Ordre conseillé

1. **A** (fuites 2-3-4) : petit, sans décision de produit, supprime l'essentiel de l'exposition.
2. **B** : demande de trancher le consentement, puis changement de flux à la création de compte.
3. **C** : à planifier avec la refonte de l'écran Économie.
4. Test de non-régression en dernier, une fois A et B faites.
