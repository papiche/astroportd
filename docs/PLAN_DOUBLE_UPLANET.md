# Plan — astroportd sur plusieurs UPlanet à la fois (ORIGIN, ZEN, …)

Écrit le 2026-10-31, **révisé le même jour** après les précisions de Fred. Plan de conception, **rien n'est codé**. Il s'appuie sur le code du dépôt (références `fichier:ligne` ou module) et sur les mesures faites lors de la reprise du uCloud de `fred@qwantic.com`.

## 0. Vision (Fred, 2026-10-31)

- **Une UPlanet est un ensemble de règles et de valeurs** : économie (monnaie, parités, parts, loyers), conditions d'admission, et aussi des **savoirs et savoir-faire** (une UPlanet peut être un syndicat professionnel, un métier, une communauté de pratique). Ce n'est pas seulement une constellation réseau.
- **Un même email peut être enregistré sur plusieurs UPlanet** : une personne, plusieurs MULTIPASS (un par UPlanet), chacun avec sa clé.
- **ORIGIN les réunit toutes.** C'est l'UPlanet fédératrice : la personne y retrouve la liste de ses rattachements.
- **La personne choisit ce qui passe d'une UPlanet à l'autre** : quels répertoires du uCloud (WebDAV) se synchronisent, dans quel sens, vers où. Rien ne passe sans ce choix.

- **ORIGIN n'est pas obligatoire sur une station.** Une station peut porter **ORIGIN + ZEN**, **ZEN + ZEN** (deux UPlanet privées, sans Kubo), **ZEN seule** ou **ORIGIN seule**. « ORIGIN réunit toutes les UPlanet » vaut quand la station (ou l'utilisateur) y a un compte ; sinon le rattachement et la synchronisation fonctionnent entre les UPlanet présentes.

Conséquence sur le plan : il ne s'agit plus de « deux plans fixes » mais de **N plans**, chacun avec une **charte**, plus une couche de **rattachement** et de **synchronisation sélective** entre eux.

## 1. Définitions et plans d'essai

| | ORIGIN | ZEN (essais : `swarm.key` = 64 × `1`) | Autre UPlanet (essais : 64 × `2`, « syndicat ») |
|---|---|---|---|
| `UPLANETNAME` | 64 × `0` | dernière ligne de la `swarm.key` | idem |
| Réseau | IPFS public par **Kubo** (clé `000…`) | **libp2p privé** (PSK) + protocole interne `astro-p2p::blob` | idem ZEN |
| Blobs | Kubo (CID v1 `raw` ; anciens CID v0 en lecture) | disque + P2P interne : **UENC v2, blocs de 64 Kio**, `bafkrei…` ; **jamais Kubo** | idem ZEN |
| Clés d'un MULTIPASS | formule `isolated` : `PEPPER_<sha256(UPLANETNAME)[:16]>` | idem, d'où **d'autres clés** pour les mêmes `(SALT, PEPPER)` | idem |
| Charte | règles ORIGIN (développement : 1 Zen = 0,1 Ğ1) | règles ZEN (1 ẐEN = 1 €) | règles propres (admission, savoirs…) |

**Compositions de station** (déclarées dans `station.conf`, `planes = …`) :

| Composition | Kubo | Réseaux libp2p privés | Exemple |
|---|---|---|---|
| ORIGIN seule | oui | aucun | station actuelle |
| ORIGIN + ZEN | oui (pour ORIGIN seulement) | 1 | essai `0…0` + `1…1` |
| **ZEN + ZEN** | **non** | 2 (deux `swarm.key`) | essai `1…1` + `2…2`, sans Kubo |
| ZEN seule | non | 1 | station privée d'une communauté |

Règle : **au plus un plan utilise Kubo, et c'est ORIGIN**. Une station sans ORIGIN n'a pas besoin de Kubo.

`fred@qwantic.com` a **un MULTIPASS par UPlanet** : N comptes, N clés Nostr, N uCloud. Ses données servent d'**oracle** pour vérifier qu'une donnée ne passe d'une UPlanet à l'autre que par une règle voulue, et sans perte.

## 2. État actuel du code (ce qui verrouille un seul plan)

Une station = une constellation, décidée au démarrage par `--swarm-key` :

- `src/serve.rs` : `a.swarm_key` fixe `uplanet_name` (l. ~460), le PSK de `p2p::start`, la politique des tunnels (`is_origin`), le `Registrar`, `ui::wire`, les tâches de nuit (`serve/tasks.rs`), `funds`, `coop_config`, `zen_react`.
- Un seul `AutoBlobStore` (Kubo si `--kubo`, sinon disque) pour tout le processus.
- Une seule base `uplanet.db` ; `identities.hex` est la clé primaire, mais **`email` n'est pas unique** et l'authentification fait `WHERE hosted = 1 AND (hex = ?1 OR email = ?1) ORDER BY hex LIMIT 1` (`astro-dav/src/auth.rs:116`) : avec deux comptes de même email, **l'un des deux est choisi selon l'ordre des clés**. Premier défaut à corriger.
- Un seul dossier de comptes : `~/.zen/game/nostr/<email>/` (deux MULTIPASS de même email s'écraseraient).
- `nostr_events` n'a pas de colonne de plan ; registre Ğ1-N² (`ledger_n2`), `social_graph`, marqueurs `done`, `kv` sont globaux à la base.
- Déjà disponible et réutilisable : le **journal des changements** `dav_changes` (triggers SQL sur `ucloud_entries`, jeton `urn:astro:sync:<n>`, purge à 30 jours), construit pour `sync-collection` : il dit, pour un répertoire, ce qui a changé depuis le dernier passage.

## 3. Invariants (à tester, pas seulement à écrire)

1. **I1 — Étanchéité.** Aucun blob, événement ni message d'une UPlanet à réseau privé n'atteint Kubo, l'IPFS public ou un relai d'une autre UPlanet, sauf par une règle de synchronisation.
2. **I2 — Un compte, une UPlanet.** Un MULTIPASS appartient à exactement une UPlanet. Le même email dans deux UPlanet désigne deux comptes distincts, **reliés par un rattachement** (§6), jamais fusionnés.
3. **I3 — Rien sans règle.** Le passage d'une UPlanet à l'autre n'a lieu que par une **règle de synchronisation** créée par le titulaire **des deux comptes**, tracée, révocable, rejouable sans doublon.
4. **I4 — Les chartes se cumulent.** Un transfert exige que la charte de la source **autorise la sortie** et que celle de la cible **autorise l'entrée** (un syndicat peut interdire d'exporter ses dossiers, ou n'admettre que certains types de fichiers).
5. **I5 — Aucun mélange de secrets.** Les secrets (`.secret.*`, trousseau uCloud, cartes de secours) d'une UPlanet ne sont jamais lisibles depuis une autre. **Seule exception, voulue : la clé d'un fichier que la personne a choisi de partager entre ses propres comptes** (adoption, I6) ; ses clés de compte et ses cartes ne sont jamais partagées.
6. **I6 — Une seule copie par station.** Sur une même station, un même contenu chiffré n'est **stocké qu'une fois**, quel que soit le nombre de plans qui le référencent : les plans partagent le blob et gardent chacun leur entrée et leur trousseau. Un transfert entre deux plans de la station est une **adoption** (aucun octet copié), pas une copie.

## 4. Architecture : N plans, une base par plan

**Phase 0 (sans refonte) : un processus par plan.** Chaque `astroportd serve` avec ses `--db`, `--blobs`, `--game-dir`, ports et sa `--swarm-key`. Suffit pour écrire et valider tout ce qui suit.

**Cible : un processus, N plans.** Un type `Plane` regroupe ce qui est aujourd'hui global : `{ nom, uplanet_name, psk, Db, BlobStore, nœud P2P, Relay, dossier de comptes, charte }`. `serve` construit la liste des plans ; un routeur choisit le plan de chaque requête.

**Une base par plan** (retenu) : ORIGIN reste la base actuelle sans migration ; l'étanchéité est **structurelle** (aucune requête SQL ne peut mélanger deux plans), le `replay` reste inchangé par plan. Le seul code qui lit deux plans est la couche de synchronisation (§7).

**Aucun plan n'est requis ni privilégié par le code** : ORIGIN est un plan comme un autre, seul à pouvoir utiliser Kubo. **Même `IPFSNODEID` dans tous les plans** (retenu) : même PeerID et même clé de NODE (formule `Raw`), annoncés dans plusieurs essaims ; un nœud libp2p (et un port) par plan réseau privé.

**Kubo : ORIGIN seulement** (retenu) ; les plans à réseau privé n'ont jamais de configuration Kubo (I1).

- **Bassin de blobs commun à la station.** Les blobs chiffrés vivent dans **un seul dossier adressé par contenu** (`~/.zen/astroportd/pool/`), lu par tous les plans ; chaque plan garde sa base (entrées, trousseau, événements). Un blob n'est supprimé que si **plus aucun plan ne le référence** (comptage de références : au moment du ramasse-miettes, la station interroge la base de chaque plan, jamais l'inverse). Seul ORIGIN y ajoute Kubo (I1).
- **Routage HTTP.** Par nom d'hôte (`<plan>.<station>`) ou préfixe de chemin ; en test, un port par plan. Le plan est connu **avant** l'authentification : l'ambiguïté d'email disparaît par construction.
- **Dossiers.** `~/.zen/astroportd/<plan>/{uplanet.db,blobs,charte.toml}` et `~/.zen/game/<plan>/nostr/<email>/` ; l'actuel `~/.zen/game` reste ORIGIN.
- **Interface.** Un sélecteur de plan dans l'en-tête ; l'écran « Passerelles » (§7). **CLI** : `--plane <nom>` sur `multipass`, `token`, `import-ucloud`, `replay`, `doctor`.

## 5. La charte d'une UPlanet

Un fichier `charte.toml` par plan (et son empreinte publiée), lu au démarrage :

- identité : nom lisible, `UPLANETNAME` (jamais publié ; seule l'empreinte `sha256(UPLANETNAME)[:16]` l'est) ;
- économie : unité, parité, parts 3×1/3, loyers (ce que lisent aujourd'hui `funds`, `coop_config`, `zen_react`, `astro-economy`) ;
- admission : ouverte, sur invitation, sur parrainage, sur preuve de compétence ;
- **savoirs et savoir-faire** : domaines, niveaux, attestations entre pairs (le système WoTx² existant : kinds 30500/30503/30504/31922), ce qui fait qu'une UPlanet peut être un syndicat professionnel ;
- **échanges** : `sortie` et `entrée` autorisées (aucun, propriétaire seul, types de fichiers, plafonds de volume, UPlanet partenaires), appliquées par I4.

## 6. Rattachement : un même email sur plusieurs UPlanet

- **Création.** S'enregistrer sur une UPlanet avec un email déjà connu ailleurs crée **un nouveau MULTIPASS** de cette UPlanet (nouveau SALT/PEPPER, nouvelle clé, nouvelle carte de secours). Rien n'est copié depuis l'autre compte.
- **Carnet de rattachements, tenu dans chaque compte concerné.** Une liste **chiffrée pour son titulaire seul** (NIP-44 vers soi, événement 31903 `d = rattachements`, comme le registre des agents) : pour chaque autre UPlanet de la personne, `{ empreinte de la UPlanet, clé du compte, date }`. Chaque compte garde **son exemplaire** : sur une station ZEN + ZEN, sans ORIGIN, le carnet vit donc dans les deux comptes ZEN. Quand la personne a aussi un compte ORIGIN, celui-ci en garde un exemplaire complet et sert d'**annuaire** (c'est là que « ORIGIN réunit toutes les UPlanet » prend son sens). Jamais public : un lien entre identités est une donnée sensible.
- **Preuve de contrôle.** Un rattachement n'est valide que s'il porte **la signature des deux comptes** (les clés des deux comptes reliés), pour qu'on ne puisse pas s'attribuer le compte d'un autre.
- **Carte de secours.** Chaque UPlanet a sa carte ; elle ne sert que dans l'UPlanet où elle a été émise (scannée ailleurs, les mêmes SALT/PEPPER donneraient une autre clé : refus avec message clair, comportement actuel `OtherUplanet`). Restaurer un compte redonne son exemplaire du carnet, donc la liste de ses autres UPlanet ; les clés de celles-ci restent protégées par **leurs propres** cartes. (Option à décider : un « carnet de secours maître » dans un compte choisi (ORIGIN s'il existe) qui garderait, chiffrées, les parts de ces cartes, au prix d'une concentration du risque.)

## 7. Synchronisation sélective entre UPlanet

**Règle de synchronisation** (créée dans l'écran « Passerelles » par le titulaire des deux comptes) :

`{ source: (plan, compte, répertoire), cible: (plan, compte, répertoire), mode, sens, filtres }`

- **Mode** : `copie` (une fois) ou `miroir` (suivi continu, par le journal `dav_changes` de la source : seuls les changements sont lus).
- **Sens** : source → cible, ou dans les deux sens (jamais de suppression croisée par défaut).
- **Filtres** : types, taille, date, motifs de chemins.
- **Conflits** : jamais d'écrasement silencieux ; les deux versions sont gardées (`nom (de <UPlanet>).ext`).
- **Aperçu obligatoire** (`--dry-run`, et dans l'interface) : nombre de fichiers, volume, ce qui serait refusé par une charte (I4).
- **Pause et révocation** à tout moment ; une règle révoquée n'efface rien.
- **Où vivent les règles.** Dans le compte **source** de la règle (événement 31903 `d = passerelles`, chiffré pour son titulaire, donc sauvegardé et répliqué comme le reste), avec un exemplaire dans le compte cible : aucune règle ne dépend de la présence d'ORIGIN ; la table de correspondance `(plan, cid, sha_plain) → (plan, cid)` est un cache local rejouable.

**Topologie en étoile, un seul exemplaire.** Chaque personne a un **plan pivot** (ORIGIN s'il existe, sinon le premier plan où elle s'est inscrite) ; les autres plans sont des **rayons**. Les règles relient un rayon au pivot (N−1 règles, pas N(N−1)/2) ; de rayon à rayon, on passe par le pivot. Le pivot détient l'exemplaire de référence ; les rayons le **référencent**.

**Mécanique d'un transfert** (`astroportd bridge sync <règle>`), du plus économe au plus coûteux :

1. **Adoption (zéro octet copié)** : source et cible sont sur la **même station** et le blob peut être partagé (voir règle ci-dessous) → l'entrée de la cible référence le **même blob avec la même clé de fichier** (déjà ce que fait `adopt_shared_file` entre deux MULTIPASS d'une station). Aucun octet lu ni écrit ; la clé est copiée dans le trousseau de la cible.
2. **Déjà connu par son empreinte** : l'index `sha_plain → (blob, clé)` de la personne (sur tous ses comptes de la station) dit que ce contenu existe déjà → adoption sans rien relire. Même mécanisme pour la reprise `pull-webdav` : si le serveur donne une somme de contrôle (Nextcloud : `oc:checksums`), un fichier connu n'est **pas retéléchargé**.
3. **Récupération par le réseau privé** : les stations d'un même essaim ZEN s'échangent déjà les blobs par CID (`astro-p2p::blob`) ; un blob qu'une station possède n'est jamais redemandé.
4. **Ré-chiffrement** (seul cas qui duplique) : la source est dans un plan **privé** et la cible est **ORIGIN** (le chiffré d'un réseau privé ne doit pas entrer dans Kubo, I1), ou les deux plans sont sur des **stations sans réseau commun**. Nouvelle clé, UENC v2 / 64 Kio, nouveau CID ; le SHA-256 du clair reste l'empreinte de comparaison.

**Quand le blob peut-il être partagé entre deux plans ?** (1) Les deux comptes sont **à la même personne** (rattachement à double signature). (2) La charte des deux plans l'autorise (I4). (3) Le blob **n'est pas né dans un plan privé pour aller dans ORIGIN** (I1) ; ORIGIN → plan privé est permis (le chiffré est déjà public). (4) Pour deux plans **privés** différents, la personne l'accepte explicitement : le même CID est alors servi dans deux essaims, ce qui permet à quelqu'un présent dans les deux de rapprocher le contenu.

**Ce que coûte le partage de blob** : la **clé de fichier est la même** dans les deux comptes. Compromettre le compte d'un rayon donne la clé des fichiers qu'il partage avec le pivot (pas des autres). C'est le prix de la copie unique ; la personne le choisit par règle.

**Écartée : le chiffrement convergent** (clé dérivée du contenu). Il dédupliquerait aussi sans partager de clé entre comptes, mais il oblige à changer UENC (nonces déterministes), et sans secret propre à la personne il révèle si un fichier connu est stocké. À rouvrir seulement avec un secret par personne.

**Premier jalon déjà là** : la reprise `astroportd pull-webdav` (uCloud › Sources) tire un dossier du WebDAV d'un autre serveur, y compris d'une autre station astroportd (testé entre deux instances). C'est la moitié « entrée » d'une règle de synchronisation entre UPlanet : une station peut tirer depuis le WebDAV de l'autre plan, avec la mémoire `pull_imported` pour ne rien reprendre deux fois.

Cette même brique, appliquée **dans une seule UPlanet**, convertit les anciens blobs v1 (CID v0, lisibles seulement par Kubo) en blobs natifs : c'est ce qui permettra de les sauvegarder chez les amis et de les répliquer en P2P (aujourd'hui exclus, voir la reprise du uCloud).

## 8. Plan d'essai avec `fred@qwantic.com`

Mise en place : ORIGIN (existant, hex `71b4b56f…`), ZEN (`swarm.key` 64 × `1`) et une UPlanet « syndicat » (64 × `2`), sur d'autres ports et dossiers ; `astroportd multipass create fred@qwantic.com` dans chacune (clés **différentes**).

| N° | Scénario | Résultat attendu |
|---|---|---|
| T1 | Trois comptes, même email, trois clés | `hex` distincts ; chaque plan n'authentifie que le sien ; le jeton d'un plan est refusé par les autres |
| T2 | Dépôt WebDAV dans ZEN | CID `bafkrei…`, entrée dans la base ZEN seulement ; absent de `ipfs pin ls` ; `ipfs cat` échoue |
| T3 | Rattachement ZEN → ORIGIN | refusé sans la double signature ; accepté avec ; le carnet n'est lisible que du titulaire |
| T4 | Règle `copie` : `/Photos/2018` ORIGIN → ZEN | même `sha_plain`, **autre CID, autre clé** ; seul ce répertoire passe ; la source est intacte |
| T5 | Rejeu de la règle | zéro doublon, zéro écriture |
| T6 | Règle `miroir` : nouveau fichier côté source | arrive côté cible au passage suivant ; seul le delta est lu (jeton `dav_changes`) |
| T7 | Fichier modifié des deux côtés | les deux versions sont gardées, rien n'est écrasé |
| T8 | Charte ZEN interdisant la sortie d'un dossier | la règle ZEN → ORIGIN refuse ce dossier avec la raison ; l'aperçu l'annonce |
| T9 | Règle révoquée | plus aucun transfert ; rien n'est supprimé |
| T10 | Événement publié par le compte ZEN | absent du relai ORIGIN et de l'IPFS ; présent seulement chez un pair possédant la `swarm.key` |
| T11 | Pair **sans** `swarm.key` sur le port P2P ZEN | poignée de main refusée |
| T12 | Sauvegarde chez un ami, par plan | l'ami ne reçoit que des blobs natifs du plan ; aucun CID `Qm…` |
| T13 | `replay` d'un plan | reconstruit ce plan seulement |
| T14 | Arrêt de Kubo | ORIGIN dégradé (anciens fichiers illisibles), **les autres plans inchangés** |
| T16 | **Station ZEN + ZEN** (`1…1` et `2…2`), **Kubo éteint** | démarrage normal ; T1, T2, T4, T5, T6, T7, T10 identiques ; aucune requête vers `127.0.0.1:5001` |
| T17 | Rattachement entre les deux comptes ZEN, sans compte ORIGIN | accepté avec la double signature ; chaque compte lit son exemplaire du carnet |
| T18 | Un pair de la `swarm.key` `1…1` tente le port de `2…2` | poignée de main refusée (les deux essaims restent étanches) |
| T15 | Carte ORIGIN scannée sur la station ZEN | refusée : « carte d'une autre UPlanet » ; aucun compte créé |
| T19 | Règle pivot → rayon, même station, 5 Go de photos | **0 octet de plus** sur le disque (taille du bassin identique) ; les deux comptes lisent le fichier ; clés identiques |
| T20 | Même fichier dans deux plans, supprimé dans l'un | le blob reste (référencé par l'autre) ; supprimé des deux, il disparaît au ramasse-miettes suivant |
| T21 | Plan privé → ORIGIN | **re-chiffré** (nouveau CID, nouvelle clé) ; le CID du plan privé est absent de Kubo |
| T22 | Reprise `pull-webdav` d'un fichier connu par sa somme de contrôle | pas de téléchargement, adoption |
| T23 | Rayon A → rayon B | passe par le pivot ; une seule copie sur disque ; I1 respecté entre deux plans privés (accord explicite requis) |

Un test automatisé « plusieurs plans, un seul processus » reprend T1, T2, T10 et T13 avec de faux pairs, sans réseau.

## 9. Phases

| Phase | Contenu | Taille |
|---|---|---|
| 0 | Plans en processus séparés (ports, dossiers, `swarm.key` d'essai), **y compris une paire ZEN + ZEN sans Kubo** ; MULTIPASS de `fred@qwantic.com` dans chacun ; T1, T2, T10, T11, T14, T16, T18 à la main | S |
| 1a | **Bassin de blobs commun + comptage de références + index `sha_plain`** (aussi utile dans un seul plan : une photo déposée deux fois n'occupe qu'une place) ; T20 | M |
| 1b | `astroportd bridge copy` : adoption par défaut, ré-chiffrement seulement si I1 l'impose ; audit, idempotence ; T4, T5, T19, T21 | M |
| 2 | Charte (`charte.toml`) + contrôle I4 + aperçu ; T8 | M |
| 3 | Rattachements (carnet chiffré dans chaque compte, double signature, annuaire ORIGIN s'il existe) ; T3, T17 | M |
| 4 | Règles `miroir` par `dav_changes`, conflits, révocation ; T6, T7, T9 | M |
| 5 | Conversion v1 → natif dans un plan ; sauvegarde des anciens fichiers ; T12 | M |
| 6 | Type `Plane`, une base par plan, routage par hôte, `--plane` partout ; fin de l'ambiguïté d'email ; test automatisé | L |
| 7 | Interface : sélecteur de plan, écran « Passerelles » (règles, aperçu, état) | M |

## 10. Décisions de Fred (2026-10-31)

1. **Kubo n'appartient qu'à ORIGIN** (clé de swarm `000…`) ; les autres UPlanet n'en ont jamais.
2. **Une base par UPlanet.**
3. **Même `IPFSNODEID`** dans tous les plans.
4. **N plans** (une UPlanet = règles, valeurs, savoirs ; un même email sur plusieurs) ; **ORIGIN les réunit** quand il est présent ; le titulaire **choisit quels répertoires** passent de l'une à l'autre.
5. **Des stations ZEN/ZEN** (et ZEN seules) existent : ORIGIN et Kubo ne sont requis nulle part.
6. **Clé de fichier partagée entre les comptes d'une même personne : acceptée par défaut** pour les fichiers qu'elle choisit de synchroniser (prix de la copie unique, I6). Chaque règle le rappelle, et la personne peut exiger un ré-chiffrement.
7. **Deux plans privés peuvent partager un même blob, sur accord explicite de la personne** (priorité : minimiser l'espace disque). L'accord se donne par règle de synchronisation, qui rappelle la conséquence : le même CID est servi dans deux essaims, et quelqu'un présent dans les deux pourrait rapprocher le contenu. Sans accord, on re-chiffre.

## 11. Points encore ouverts

1. **Carnet de secours maître dans ORIGIN** (§6) : oui ou non ? Il simplifie la vie de la personne (une seule carte à garder) mais fait de la carte ORIGIN la clé de toutes ses UPlanet.
2. **Qui peut créer une UPlanet ?** Le Capitaine d'une station, n'importe qui avec une `swarm.key`, ou une validation par ORIGIN ?
3. **Annuaire ORIGIN des UPlanet.** Publier leur empreinte, nom et charte résumée sur ORIGIN pour qu'on les découvre ? (Une UPlanet privée peut refuser d'y figurer.)
4. **Sens d'un miroir bidirectionnel** : autorisé dès la phase 4, ou d'abord source → cible seulement ?
5. **Économie.** Les parités et valeurs entre UPlanet (1 Zen = 0,1 Ğ1 contre 1 ẐEN = 1 €) : hors de ce plan, à traiter à part.
