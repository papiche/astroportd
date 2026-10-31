# astroportd

[![Licence AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue)](LICENSE)
[![Release](https://img.shields.io/github/v/release/papiche/astroportd?include_prereleases&label=release)](https://github.com/papiche/astroportd/releases)
[![CI](https://github.com/papiche/astroportd/actions/workflows/ci.yml/badge.svg)](https://github.com/papiche/astroportd/actions/workflows/ci.yml)

**Votre cloud, vos photos, vos amis et votre économie coopérative, sur une machine qui est à vous.**
astroportd est une **station personnelle** : un seul programme, de quelques dizaines de Mo, qui tient sur un Raspberry Pi
comme sur un serveur GPU. Il garde vos fichiers **chiffrés**, retrouve les visages dans vos photos et les partage avec
vos amis, vous fait **sauvegarder par vos proches** (et retrouver votre compte si votre machine brûle), et fait tourner
une **comptabilité coopérative en Ẑen** (monnaie libre Ğ1) — sans plateforme, sans abonnement, sans serveur central.
C'est le cœur souverain d'[Astroport.ONE](https://github.com/papiche/Astroport.ONE) et d'UPlanet, réécrit en Rust.

> **Préversion 0.5.0-beta.** Le code est testé de bout en bout (≈ 600 tests, `clippy` sans avertissement) mais jeune :
> lisez [les limites connues](#8-état-du-projet-et-limites-connues). Licence **AGPL-3.0** · Auteur : Fred
> (support@qo-op.com) · Rust 2024 (≥ 1.85).

---

## Ce que vous pouvez faire avec astroportd

| Je veux… | astroportd… |
|---|---|
| **Garder mes fichiers chez moi**, accessibles partout | Un cloud personnel (`uCloud`) **chiffré avant de quitter l'appareil**, monté comme un disque réseau (WebDAV : Finder, Explorateur Windows, mobile) et dans le navigateur. |
| **Retrouver mes photos par visage** et les partager entre amis | `FaceCloud` reconnaît les visages et envoie les photos à vos amis **chiffrées, directement à leur station**. Une zone de doute (« est-ce X ? ») vous laisse valider. |
| **Faire entrer quelqu'un** dans le réseau en lui envoyant une photo | Un **lien d'invitation** (ou un QR) : la personne crée son compte en un geste, ou l'accepte avec le sien, même sur une autre station ; ses photos arrivent. |
| **Ne rien perdre si ma machine tombe en panne** | Mes amis hébergent une **copie chiffrée** que seul moi peux lire. Avec ma **clé de secours** (QR) et la part d'un ami (« j'appelle un ami »), je rétablis mon compte sur n'importe quelle station : mes amis renvoient tout d'eux-mêmes, sans rien faire. |
| **Changer de station** (déménager chez un Capitaine plus proche) | Le même parcours : la nouvelle station s'annonce comme ma maison, l'ancienne cesse d'héberger mon compte. |
| **Parler et publier entre amis**, sans plateforme | Un **relai Nostr** intégré, des contacts et un agenda synchronisables avec le téléphone (CardDAV, CalDAV), un cercle social à deux niveaux (amis N¹, amis d'amis N²). |
| **Faire vivre une économie coopérative** | Une comptabilité en **Ẑen** adossée à la Ğ1 : règle des 3×1/3, registre signé, micro-dons par simple réaction `+N`. |
| **Avoir un assistant** qui me connaît | **BRO** : une mémoire personnelle à recherche vectorielle locale (`sqlite-vec`), dont les embeddings sont calculés par votre propre serveur Ollama. |

**Pour qui ?** Un **membre** s'inscrit sur la station d'un Capitaine (ou la sienne) ; un **Capitaine** héberge des membres et
règle sa station ; un **développeur** étend l'ensemble par des plugins WebAssembly.

## Ce qui le distingue

Beaucoup d'outils font une partie de cela. astroportd les réunit **dans un seul programme que vous possédez**, avec des
choix que les services habituels ne font pas :

| Domaine | Services habituels | Alternatives libres / P2P | Ce que fait astroportd |
|---|---|---|---|
| **Fichiers et synchronisation** | Google Drive, Dropbox, Nextcloud | Filecoin, Sia, IPFS Desktop | Chiffrement **UENC v2** (flux authentifié par blocs de 64 Kio, lecture partielle `Range`), déduplication par CID, et un **manifeste de votre cloud ancré sur Nostr** : la base SQLite n'est qu'un cache, tout se rejoue. |
| **Photos et visages** | Google Photos, Apple Photos | Immich, PhotoPrism | Photos **chiffrées**, empreintes de visages 512D **cloisonnées par propriétaire** (`sqlite-vec`), partage vers les amis reconnus par messages chiffrés **NIP-59/NIP-17**, avec validation de l'utilisateur dans la zone de doute. |
| **Identité et réseau social** | Keybase, X, Mastodon | Urbit, Farcaster, clients Nostr | Une **identité dérivée de votre clé SSH** (Y-Level : SSH, IPFS, Ğ1 et Nostr sont la même clé) et un **pare-feu social NIP-42 à deux niveaux** : seuls les membres du cercle (hébergés, N¹, N²) publient, et tout contenu non public exige d'être authentifié dans le cercle. |
| **Sauvegarde et reprise** | Sauvegarde cloud payante | Syncthing, Tahoe-LAFS | Des **amis réciproques hébergent votre copie chiffrée** (ils ne peuvent rien lire), et restituent **d'eux-mêmes** après une panne : ma **clé de secours** (QR) plus la part d'un ami suffisent à retrouver compte, contacts et fichiers sur une autre station. |
| **Communication entre machines** | Tailscale, ZeroTier, ngrok | libp2p, Iroh | Tunnels **« DRAGON »** natifs `/x/<service>-<PeerID>` sur le maillage libp2p, sans ouvrir de port sur la box ; une politique réglée par le Capitaine dit qui peut ouvrir quel service. |
| **Économie et monnaie** | OpenCollective, Stripe | DAO, Lightning | Comptabilité coopérative adossée à la **monnaie libre Ğ1** (Duniter v2s) : **règle des 3×1/3**, registre Ğ1-N² signé et chaîné, **micro-dons sur réaction Nostr `+N`**. Le Ẑen est une unité comptable de la coopérative, pas une monnaie publique. |

Ce qu'on ne prétend pas : astroportd n'est pas un réseau social grand public, ni un service hébergé. C'est **votre** station,
avec ses [limites connues](#8-état-du-projet-et-limites-connues).

---

## L'application phare : FaceCloud + partages N¹ / N² entre uCloud

![FaceCloud : photos, visages, album, amis N¹, lien secret](docs/img/facecloud-partage.svg)

**Vos photos retrouvent les visages, et circulent entre amis — sans passer par une plateforme.** Tout part de votre
**uCloud**, un cloud personnel **chiffré avant de quitter votre appareil** :

- **Déposez vos photos** (WebDAV : Finder, Explorateur Windows, application mobile ; ou l'API). Chaque fichier est chiffré
  en flux (UENC v2) ; le clair ne touche jamais le disque de la station (`astro-dav`).
- **FaceCloud reconnaît les visages** : détection et empreinte 512D, comparaison cosinus. À partir de **0,82** le visage
  est reconnu ; entre **0,55 et 0,82** la station demande « est-ce X ? » et **vous validez** ; en dessous, il reste
  `Inconnu_<hash8>`. Une seule inférence à la fois, file d'attente persistante (`astro-facecloud`).
- **Partage automatique entre amis N¹** (ceux que vous suivez *et* qui vous suivent) : la photo où figure votre ami
  est copiée dans `/Partages/De_<vous>/` chez lui s'il est sur la même station ; sinon elle lui est envoyée **chiffrée
  vers sa station** (message de fichier NIP-17 emballé NIP-59, relayé par la constellation).
- **Le reste du monde par un lien secret** : `https://station/s/<slug>#clé`, de 1 heure à 30 jours. La clé AES-256 est
  dans le **fragment** `#…` que le navigateur n'envoie jamais ; le serveur ne garde que des copies chiffrées avec cette
  clé, et les liens expirés sont supprimés chaque nuit (`astro-dav/share.rs`).
- **Films et séries** (copie privée, CPI L. 122-5) : publiés en NIP-71 (kind 21/22) et **livrés au cercle N¹ de l'auteur
  seulement**, jamais à tout le cercle N² (§10).
- **Les cercles N¹ et N²** sont ceux de Nostr : le relai de la station réserve ses contenus privés aux membres
  authentifiés (NIP-42) du cercle, et le N¹ d'un auteur se lit dans sa liste de contacts, même s'il vit sur **une autre
  station** de la constellation.

> **À savoir.** La reconnaissance faciale passe soit par un service HTTP d'inférence (`--face-engine`), soit, sur une
> station autonome, par une inférence **locale** (`--face-models`, binaire compilé avec `--features onnx` : SCRFD +
> ArcFace par ONNX Runtime, modèles à fournir). Sans l'un ni l'autre, uCloud fonctionne mais sans visages. Pas de partage
> « automatique » vers un ami qui ne vous suit pas en retour : c'est le lien secret qui sert alors.

Pas à pas : [étape 8 du guide](docs/GUIDE.md#etape-8).

---

## Démarrer en 5 minutes

```bash
# 1. Installer (vérifie l'empreinte SHA-256 de l'archive, sans droits administrateur)
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash

# 2. Préparer la station : clé Y-Level, choix ORIGIN ou ẐEN, dossiers (assistant en français)
astroportd init

# 3. Créer le MULTIPASS du Capitaine (secrets affichés UNE seule fois : notez-les)
astroportd multipass create capitaine@exemple.org --captain

# 4. Démarrer
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
# → http://127.0.0.1:18080 : l'application web ; la même adresse sert le relai Nostr

# 5. Les membres s'inscrivent sur http://127.0.0.1:18080/register (ou : astroportd multipass create <email>)
```

`astroportd init --mode origin` ou `--mode zen --swarm-key ./swarm.key` s'utilisent aussi sans questions (`--yes`).
`astroportd init --systemd` écrit un service utilisateur ; `astroportd doctor` vérifie le tout.

**Guide pas à pas illustré** (terminal *et* navigateur, accessible à tous) : [docs/index.html](docs/index.html)
· version texte : [docs/GUIDE.md](docs/GUIDE.md) · en profondeur : [docs/CONSTELLATIONS.md](docs/CONSTELLATIONS.md).

## Créer son MULTIPASS, directement sur sa station

![Création d'un MULTIPASS : de l'email aux clés](docs/img/multipass-creation.svg)

astroportd **crée les MULTIPASS lui-même**, sans passer par un autre service (reprise de `make_NOSTRCARD.sh`
d'Astroport.ONE) :

- **Au terminal** : `astroportd multipass create <email> [--captain] [--lang fr|en|es] [--json]` et
  `astroportd multipass list`. `--captain` désigne le **premier** compte comme Capitaine de la station (refusé si un
  autre Capitaine existe) ; les suivants **suivent automatiquement le Capitaine** (kind 3).
- **Au navigateur** : la page publique **`/register`** (lien « Pas de compte ? Créer mon MULTIPASS » dans la fenêtre de
  connexion) : email + langue, puis une **carte d'identité** à noter (SALT, PEPPER, nsec, npub, Ğ1, IPNS, code PASS,
  jeton), téléchargeable en `.txt`, et un bouton « Ouvrir mon espace ». API : `GET/POST /api/v1/register`.
- **Inscription web ouverte par défaut**, avec garde-fous : `--max-accounts N` (500 par défaut, `0` = illimité),
  `--register-per-hour N` (5 par adresse IP, `0` = illimité), `--no-register` pour la fermer.
- **Une seule constellation par compte** : SALT et PEPPER aléatoires (24 caractères), puis la formule **isolée**
  `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)`. Le même couple donne donc **d'autres clés** dans une autre constellation.
  La création signe un profil (kind 0), le suivi du Capitaine (kind 3) et un DID (kind 30800).

Les secrets ne sont affichés qu'**une seule fois**. [qo-op.com](https://qo-op.com) reste une option (une station ORIGIN
publique où s'inscrire), pas une obligation.

### La clé de secours : retrouver son compte après une panne, ou changer de station

Le secret du compte (SALT et PEPPER) est découpé en **3 parts, dont 2 suffisent** (partage de Shamir sur GF(2⁸)) :

| Part | Qui la garde | Seule, elle… |
|------|--------------|--------------|
| **P1** | le membre : QR imprimable (`astro-share:v1|…`) remis à la création | ne révèle **rien** (volée, photographiée : sans danger) |
| **P2** | la station d'accueil (`<compte>/.ssss/2`, `0600`) | sert à réimprimer P1 ou à renvoyer P3 |
| **P3** | **les amis qui hébergent la sauvegarde** (envoyée d'elle-même, chiffrée pour chacun) | ne révèle rien ; l'ami la montre à celui qui l'appelle |

**« J'appelle un ami »** : sur la nouvelle station, on saisit (ou scanne) P1 et la part que l'ami lit dans *Compte › Sauvegarde
privée*. Aucun mot de passe, aucun réseau à attendre pour cette étape ; la voix de l'ami est la vérification. L'ancienne carte
complète (`astro-secret:v1|…`) reste acceptée. Le SALT et le PEPPER ne sont plus affichés à la création.

- **Rétablir** : `/restore` sur la nouvelle station (scan des deux QR ou collage), ou `astroportd multipass recover -` au terminal.
  La station rétablie retrouve les trois parts d'origine (le P1 imprimé et le P3 des amis restent valables).
  Même clé, donc mêmes amis, contacts et fichiers ; un **nouveau PASS** est généré (l'ancien n'est pas récupérable).
- **Sans rien demander à vos amis** : chaque nuit, votre station range chez l'ami qui héberge votre sauvegarde vos fichiers
  privés **et le manifeste de votre uCloud**. Après un rétablissement, la nouvelle station republie votre DID avec **sa**
  station d'accueil, prévient vos proches (leurs stations mettent à jour votre adresse), et **leurs stations renvoient d'elles-mêmes**
  le manifeste, puis les fichiers sauvegardés ou partagés. Les fichiers qui manquent encore sont redemandés par leur CID.
- **Déménager** (l'ancienne station tourne encore) : la nouvelle écrit à l'ancienne un message `moved`, signé de la clé du
  compte ; l'ancienne cesse d'héberger ce compte et met son dossier de côté (`.moved/`, jamais effacé).
- **Garde-fous** : la carte d'une autre constellation, ou dont les secrets ne redonnent pas la clé annoncée, est refusée avant
  toute écriture ; aucun événement de naissance n'est republié (il écraserait votre vraie liste d'amis) ; aucun manifeste vide
  n'est publié tant que la restauration dure.
- **Réafficher une part** : `astroportd multipass card <email> [--part 1|2|3] [--svg]`.

Pas à pas : [guide, étape 5](docs/GUIDE.md#etape-5).

---

## ORIGIN ou ẐEN : deux constellations

![ORIGIN ou ẐEN](docs/img/origin-vs-zen.svg)

| | **UPlanet ORIGIN** | **UPlanet ẐEN** |
|---|---|---|
| Vocation | Bac à sable, apprentissage | Production, économie coopérative |
| `swarm.key` | **absente** | **présente** (clé du réseau privé) |
| `UPLANETNAME` | 64 zéros | la clé partagée |
| Réseau P2P | ouvert | privé, chiffré |
| 1 Ẑen vaut | 0,1 Ğ1 (jeu) | 1 € (coopérative) |
| Pour démarrer | `astroportd init --mode origin` | `astroportd init --mode zen --swarm-key ./swarm.key` |

**Pas de `swarm.key` = ORIGIN ; `swarm.key` présente = ẐEN.** Pour entrer en ẐEN, recevez la clé d'un Capitaine
(certification DRAGON) — ou **fondez votre constellation** : `astroportd swarm-key > swarm.key`, puis copiez-la sur
chaque station. Détails, avertissements et limites : [docs/CONSTELLATIONS.md](docs/CONSTELLATIONS.md).

## Y-Level : votre clé SSH *est* l'identité de votre station

![Y-Level : une clé, quatre identités](docs/img/y-level.svg)

`astroportd init` fait le rituel **Y-Level** d'Astroport.ONE : la clé SSH est hachée (sha512) en `SALT` et `PEPPER`, un
scrypt en tire **une graine ed25519**, et de cette graine viennent une **clé SSH jumelle**, le **PeerID IPFS** de la
station (`IPFSNODEID`), la clé publique **Ğ1** et la clé **Nostr**. Votre ancienne clé SSH est gardée (`~/.ssh/origin.key`).
`astroportd ylevel --verify` prouve que SSH et IPFS sont la même clé.

**Pourquoi c'est utile** : la clé SSH d'un Capitaine se déduit de son identité IPFS. Les Capitaines qui se suivent sur
Nostr peuvent donc s'ouvrir un accès SSH entre machines de confiance, à travers les tunnels P2P d'astroportd :
**individus ↔ Astroport ↔ territoire**.

![Individus, Astroports, territoire](docs/img/individu-astroport-territoire.svg)

*Ce qui est dans astroportd :* le rituel, la vérification, les tunnels P2P (`--p2p-expose ssh=…`, `--p2p-forward`).
*Ce qui reste dans Astroport.ONE :* le suivi automatique des Capitaines (kind 30850) et la mise à jour des clés SSH
autorisées.

---

## Télécharger

Les binaires sont publiés dans les [**Releases**](https://github.com/papiche/astroportd/releases), avec `SHA256SUMS` :

| Cible | Pour |
|---|---|
| `x86_64-unknown-linux-gnu` | PC, serveur, VM |
| `aarch64-unknown-linux-gnu` | Raspberry Pi 4/5, Pi Zero 2 W (64 bits) |
| `armv7-unknown-linux-gnueabihf` | Raspberry Pi 2/3, Pi OS 32 bits |
| `x86_64-unknown-linux-musl` | binaire statique, sans glibc |

Linux uniquement. Compiler soi-même : `./build-all.sh` (voir §4).

---

## 1. Le manifeste

### Souveraineté radicale

Vos données, vos clés et votre monnaie ne doivent dépendre d'aucune plateforme, d'aucun hébergeur, d'aucune autorité
de certification choisie pour vous. `astroportd` applique cette phrase à la lettre :

- **Vos clés restent chez vous.** Les secrets sont des fichiers `0600` (`astroportd doctor` vérifie), effacés de la
  mémoire à la destruction (`zeroize`), jamais écrits dans la base ni dans les journaux.
- **Vos fichiers sont chiffrés avant de quitter la machine** (UENC v2, AES-256-GCM ou ChaCha20-Poly1305, par blocs de
  64 Kio). Le clair ne touche jamais le disque : le téléversement est chiffré *en flux*. Un lien partagé porte sa clé
  dans le fragment `#…` de l'URL, qu'un navigateur n'envoie jamais au serveur.
- **Vos certificats sont les vôtres.** HTTPS automatique (Let's Encrypt ou ZeroSSL, défi HTTP-01) sans Nginx ni
  Certbot : le démon tient les ports 80 et 443.
- **Votre monnaie est comptable, pas spéculative.** Le **ẐEN** est l'unité de compte interne d'une coopérative, adossée
  à la Ğ1 (Duniter v2s). Tout montant est un **entier de centimes** : le flottant est banni de la monnaie.

### 100 % Nostr + IPFS : la vérité n'est pas dans la base

> *« Le stockage doit être fait à 100 % dans Nostr et IPFS. »*

Il n'existe qu'**une** source de vérité, répliquée par la constellation : les **événements Nostr** (identités, réseau
social, registre Ğ1-N², manifestes) et les **blobs IPFS** (fichiers chiffrés). `uplanet.db` n'est qu'un
**cache rejouable** : on peut la supprimer, puis `astroportd replay` la reconstruit à l'identique depuis les
événements. Corollaire testé : chaque projection est *indépendante de l'ordre d'arrivée*, donc le temps réel et le
replay convergent vers le même état.

### SQLite, projection et acteur d'écriture unique

SQLite (mode WAL, extension `sqlite-vec` pour la recherche vectorielle cosinus) sert d'index local rapide. Toutes les
écritures passent par **un acteur unique** (un thread, une file bornée, une transaction par message) : plus de
`SQLITE_BUSY`, contre-pression naturelle, et les lecteurs ne sont jamais bloqués. Les tables sont classées en
*projections* (rejouables) et *état local* (identités hébergées, files de tâches, verrous).

### Frugalité et horloge solaire (20h12)

Un nœud doit coûter peu d'énergie et peu d'attention. Les travaux d'entretien se calent sur **20h12 solaire vrai**
(équation du temps de Spencer, précise à ~30 s), pas sur l'horloge murale : ils suivent le soleil de la station.
`astro-core::solar` calcule le prochain 20h12 et refuse de fonctionner si l'horloge est antérieure à 2026 (un
Raspberry Pi sans pile RTC démarre en 1970 et ferait des échéances absurdes). `astroportd doctor` l'affiche.
Le démon (`src/solar_cron.rs`) dort jusqu'à ce soleil-là, puis joue, **dans cet ordre et chaque étape indépendante
des suivantes** : garde-fou RTC → verrou du jour (posé *avant* le travail) → snapshot atomique `VACUUM INTO`
(7 conservés) → purge des réclamations de plus de 7 jours et des liens d'invités expirés → balayage RÊVE → clôture
hebdomadaire de la régie 3×1/3 (toute semaine ISO terminée et pas encore close : le lundi, avec rattrapage de
5 semaines) → événement de santé économique (kind 30850, `d = economic-health`). Une étape qui échoue est dite sur
le bus d'activité et dans le journal ; la nuit continue. Il faut les coordonnées de la station (`--lat/--lon` ou la
position de son NODE) ; sans elles le démon le dit et reste inactif (`--no-nightly` pour le couper).
**Rattrapage au démarrage** : une station éteinte à 20h12 ne reste pas en retard. Au boot, une tâche de fond (le
serveur n'attend pas) joue la nuit du jour solaire courant si son 20h12 est passé et qu'elle n'a pas eu lieu
(`done nightly:<date>`), et la nuit de la veille si elle a été manquée alors que la station avait déjà fait une nuit.
Les nuits plus anciennes ne sont pas rejouées (la clôture hebdomadaire, elle, rattrape 5 semaines). Horloge < 2026 :
rien n'est fait, et le dit.

### Ni cloud, ni abonnement, ni destruction

Un compte en défaut de paiement n'est **jamais détruit** : 7 jours de loyer, 7 jours de grâce, puis *suspension* — le
compte, ses clés et ses données restent intacts et se réactivent dès que le loyer est réglé. Quand la trésorerie ne
couvre plus les charges, la station bascule en **Bénévolat Actif** (Love Ledger) plutôt que de s'arrêter.

---

## 2. Vue d'ensemble

```
        navigateur / mobile (PWA)        clients Nostr        WebDAV (Finder, Windows…)
                    │                         │ NIP-11/WS               │
                    ▼                         ▼                         ▼
   ┌────────────────────────────  astroportd  ───────────────────────────────────┐
   │  astro-ui  façade HTTPS (ACME, proxy, app web, /health, SSE)                 │
   │  astro-relay  relai Nostr, pare-feu social NIP-42   astro-dav  uCloud chiffré│
   │  astro-bro  mémoire, bus         astro-facecloud  visages   astro-plugins    │
   │  astro-economy  3×1/3, Love Ledger              astro-lifecycle  comptes     │
   │  astro-sync  projections & replay    astro-p2p  maillage libp2p, tunnels     │
   ├────────────┬───────────────────────────────┬─────────────────────────────────┤
   │ astro-db   │ astro-crypto · astro-core     │ astro-duniter  (Ğ1, subxt)      │
   │ SQLite+vec │ astro-nostr-proto             │ astro-g1-runtime                │
   └────────────┴───────────────────────────────┴─────────────────────────────────┘
          │ cache rejouable                │ vérité : événements Nostr + blobs IPFS (Kubo ou disque)
          ▼                                ▼
      uplanet.db                  relais de la constellation  ·  pairs libp2p
```

---

## 3. Cartographie des crates

Le workspace compte **16 crates** (+ le binaire `astroportd`, qui est aussi une bibliothèque testée de bout en bout).

| Crate | Rôle |
|---|---|
| **`astro-core`** | Socle pur : les 5 formules de crédentiels (`KeyFormula`), dérivations Ğ1/IPFS/Nostr (scrypt, PBKDF2), SS58 préfixe 4450, `Amount` (centimes entiers), semaine ISO, **horloge solaire 20h12**, **bus d'activité** (flux SSE). |
| **`astro-crypto`** | Chiffrement des fichiers **UENC v1 (lecture) / v2 (écriture)**, NIP-44 v2, NIP-59 (gift wrap), NIP-98 durci (anti-rejeu, liaison URL/méthode/`payload`), sources de clés. |
| **`astro-nostr-proto`** | Événements, filtres, messages NIP-01, kinds du projet, signature Schnorr (secp256k1), validation stricte. |
| **`astro-db`** | `uplanet.db` : schéma, acteur d'écriture unique, pool de lecteurs WAL, recherche vectorielle `sqlite-vec` (mémoire 1024D, visages 512D), stockage des événements (remplaçables, NIP-09, NIP-40), recherche plein texte NIP-50 par index FTS5 (préfixes, sans accents ni casse, classée par pertinence), migrations légères. Base créée en `0600`. |
| **`astro-relay`** | Relai Nostr WebSocket en **pare-feu social** : lecture publique des kinds publics, tout le reste réservé au cercle N1/N2 authentifié (NIP-42) ; NIP-11 ; anti-abus (débit, tailles). NIP-01, 09 (suppressions par id et par adresse, sans réinsertion), 11 (avec CORS), 17, 40, 42, 45 (`COUNT`), 50 (`search`), 59 et 70 (événements protégés). |
| **`astro-sync`** | `Projector` (événement → requêtes SQL, pur et indépendant de l'ordre), `replay`, `Crawler` N² clé par clé, client WebSocket (`ws://`, `wss://` rustls), envoi de messages chiffrés. |
| **`astro-dav`** | WebDAV classe 1+2 sur uCloud chiffré (GET/Range, PUT en flux, PROPFIND, MKCOL, MOVE, COPY, LOCK, PROPPATCH), manifeste ancré sur Nostr/IPFS, liens d'invité `/s/<slug>#clé` (lecture seule) et **invitations** `/i/<jeton>` (le destinataire crée son MULTIPASS et reçoit les photos), **miniatures WebP**, dépôt hors WebDAV (`ingest`), magasin de blobs (disque / Kubo), **CardDAV** (`carddav` : carnet `/Contacts` chiffré, vCard 3.0/4.0 avec `X-NOSTR-PUBKEY`/`X-G1-PUBKEY`), **CalDAV** (`caldav`, `ical` : agenda `/Calendrier` chiffré, objets `.ics`), `sync-collection` pour les deux (journal `dav_changes`), **films et séries** (`media` : métadonnées TMDB, sondage, plan H.264/AAC, événement NIP-71, attestation de copie privée). |
| **`astro-facecloud`** | Reconnaissance de visages : file de photos, moteur d'inférence (Brain HTTP ou ONNX local, commutable à chaud), seuils 0,82 / 0,55, partage automatique aux amis réciproques, validation « est-ce X ? ». |
| **`astro-bro`** | Agent personnel **BRO LifeOS** : mémoire sémantique `bge-m3` 1024D (4 emplacements), **cycle RÊVE** (condensation à 150 souvenirs), garde-fous anti-secrets, bus de commandes par messages privés. |
| **`astro-p2p`** | Maillage libp2p (TCP/QUIC, Noise, Yamux, réseau privé par `swarm.key`, circuit relay, DCUtR), tunnels DRAGON `/x/<service>-<PeerID>` avec **garde par flux et watchdog de session**, échange de blobs, **routage intelligent** (Power-Score, `power`/`closest`/`random`). |
| **`astro-duniter`** | Client Duniter v2s natif (subxt) : soldes, virements avec mémo, surveillance des transferts, bascule entre nœuds *avant* envoi seulement, signature hors ligne. |
| **`astro-g1-runtime`** | Métadonnées du runtime Ğ1, compilées à part pour isoler le coût de `subxt`. |
| **`astro-economy`** | Registre **Ğ1-N²** (kind 30852) validé intégralement, règle des **3×1/3**, **Love Ledger**. |
| **`astro-lifecycle`** | Détection des identités locales (NODE, Capitaine, MULTIPASS), machine d'état hebdomadaire (loyer → grâce → suspension), reconstruction au replay, déménagement `home:station`. |
| **`astro-plugins`** | Runtime **WebAssembly isolé** (wasmtime) : aucune capacité, carburant et mémoire bornés, appels sans état. |
| **`astro-ui`** | Façade web : ACME, TLS, reverse proxy, **application web embarquée** (HTML/CSS/JS sans framework, ≈ 39,5 Ko bruts, **13,0 Ko en Brotli, 14,6 Ko en Gzip**, PWA), API `/api/v1`, `/health`, flux SSE. |

---

## 4. Démarrage rapide

### Construire

```bash
# Prérequis : Rust ≥ 1.85, un compilateur C (SQLite et libwebp sont compilés depuis leurs sources)
cargo build --release
# → target/release/astroportd

# Toutes les plateformes supportées (x86_64, aarch64/Raspberry Pi 4-5, armv7/Pi 2-3, x86_64 statique musl) :
./build-all.sh --list      # état de la chaîne d'outils de chaque cible, et la commande qui installe ce qui manque
./build-all.sh             # construit ce qui est possible → dist/astroportd-<version>-<cible>.tar.gz + SHA256SUMS
./build-all.sh --strict    # échoue si une cible a dû être sautée (CI)
```

### Diagnostiquer avant de démarrer

```bash
astroportd doctor --db uplanet.db --game-dir ~/.zen/game
```

`doctor` ne modifie rien : il vérifie l'identité locale, les permissions `0600` des secrets et de la base, le mode WAL
et l'intégrité de SQLite, la validité des certificats ACME (alerte sous 30 jours) et l'horloge (prochain 20h12
solaire). Code de sortie `1` si une vérification échoue.

### Démarrer une station

```bash
# Essai local, sans domaine (HTTP en clair sur 127.0.0.1:18080) :
astroportd serve --db uplanet.db --blobs blobs

# Station publique : HTTPS automatique, maillage P2P, BRO, soldes Ğ1 :
astroportd serve --db uplanet.db --blobs blobs \
    --domain station.exemple.org --acme-email moi@exemple.org \
    --p2p-listen /ip4/0.0.0.0/tcp/4101 --swarm-key swarm.key \
    --p2p-expose comfyui=127.0.0.1:8188 \
    --ollama http://127.0.0.1:11434 --duniter-rpc wss://g1.axiom-team.fr/ws
```

Ouvrez ensuite `https://station.exemple.org/` : un navigateur reçoit l'application, un client Nostr reçoit le relai
(`Accept: application/nostr+json` ou WebSocket) — **même adresse**. Les clés de la station sont découvertes
automatiquement dans `~/.zen/game` (`--game-dir` ou `ASTRO_GAME_DIR`).

### Les autres commandes

| Commande | Rôle |
|---|---|
| `astroportd init [--mode origin\|zen]` | Assistant d'installation : rituel **Y-Level**, choix ORIGIN/ẐEN, dossiers de la station, service systemd (`--systemd`). |
| `astroportd ylevel [--verify]` | Affiche (ou vérifie, code 1 si faux) la correspondance SSH ↔ PeerID IPFS ↔ Ğ1 ↔ Nostr. |
| `astroportd multipass create <email> [--captain]` / `multipass list` | Crée (et liste) les MULTIPASS de la station ; secrets affichés une seule fois. |
| `astroportd token <email>` | Jeton de connexion d'un compte hébergé (interface web, clients WebDAV). |
| `astroportd media --account <email> consent` | Atteste la copie privée (CPI L. 122-5), une fois. `--show` affiche le texte. |
| `astroportd media --account <email> --db … --blobs … add --file film.mkv --series "Kaamelott" --season 2 --episode 3` | Dépose un film ou un épisode (≤ 1 024 Mo), encode en H.264/AAC (`--transcode auto\|notify`) et publie l'événement vidéo. |
| `astroportd replay --db uplanet.db [--from ws://relai --hosted <hex>] [--blobs blobs]` | Reconstruit la base depuis les événements (et restaure uCloud depuis les manifestes). |
| `astroportd route ollama --mode closest --lat 45.7 --lon 4.8` | Meilleur nœud du swarm pour un service. |
| `astroportd swarm-key` | Génère la clé d'un réseau privé de constellation. |
| `astroportd plugin [--dir plugins] list \| call <nom> '<json>'` | Gère les extensions Wasm. |
| `astroportd relay`, `astroportd dav` | Le relai seul, le WebDAV seul. |

### L'économie en une page

- **Loyer** : un MULTIPASS paie chaque semaine ; sociétaire : parts validées.
- **3×1/3** : chaque semaine, le surplus est ventilé (impôt sur les sociétés marginal 15 %/25 % au seuil de 42 500,
  prime du Capitaine 1 % du net, puis trois tiers égaux : trésorerie, R&D, actifs ; le résidu va à la trésorerie).
- **Ni location ni vente de ressources** : ni disque, ni GPU ne se louent. Le quota d'un compte est un réglage de sa
  formule (MULTIPASS ou ZenCard) ; le soutien entre Capitaines, GPU compris, passe par l'entraide.
- **Opex et Capex** : l'écran MULTIPASS sépare les **redevances d'usage** (Ẑen Opex : loyer) des **parts
  d'investissement matériel** (Ẑen Capex, ZenCard : capital, amorti).
- **Films et séries** : voir §10.

---

## 5. Sécurité

- **Pare-feu social** : le relai ne sert les contenus privés (messages, données applicatives) qu'aux membres
  authentifiés du cercle N1/N2 ; les enveloppes NIP-59 ne sont acceptées d'un inconnu que vers un MULTIPASS hébergé, avec
  limite de débit.
- **Authentification** : NIP-98 (anti-rejeu persistant, fenêtre de 120 s, liaison URL + méthode + empreinte du corps) ou
  jeton dérivé `HMAC(secret de station, hex du compte)`. Aucun cookie : l'API n'est pas exposée aux requêtes forgées.
- **SSRF** : le téléchargement d'un document demandé par `#upload <URL>` ne joint que des adresses publiques (résolues
  puis vérifiées *avant* connexion, redirections revérifiées, HTTPS validé).
- **Application web** : CSP stricte (`default-src 'none'`), aucun script ni style en ligne, aucun `innerHTML`.
- **Plugins** : voir §7 — bulle sans capacité, bornes dures.
- **Code** : `unsafe` interdit (hors la seule initialisation de `sqlite-vec`), `unwrap` interdit en production,
  `clippy` sans avertissement, documentation obligatoire.

---

## 6. Développer sur astroportd

```bash
cargo test --workspace      # ~ 570 tests : unitaires, intégration, bout en bout (vraie base, vrai WebDAV, vrais pairs libp2p)
cargo clippy --workspace --all-targets
```

Conventions : montants en centimes entiers ; une erreur n'est jamais avalée (elle remonte, ou elle est dite sur le bus
d'activité) ; les décisions qui dépendent de l'état se prennent **en SQL** pour que le replay soit indépendant de
l'ordre ; tout test hostile (SSRF, boucle infinie, dépassement mémoire) a son cas. Le dossier `tests/` du binaire
contient les scénarios complets : cycle de vie et économie, interface web, `#upload`.

---

## 7. Écrire une extension WebAssembly

Une extension est un fichier `.wasm` déposé dans `plugins/`. Elle s'exécute dans une **bulle** :

| Garantie | Mécanisme |
|---|---|
| Aucune capacité | Pas de WASI, aucune fonction fournie par l'hôte : un module qui *importe* quoi que ce soit est **refusé au chargement**. Ni fichier, ni réseau, ni horloge, ni aléa. |
| Calcul borné | Carburant : 50 millions d'instructions par appel (une boucle infinie finit en `OutOfFuel`). |
| Mémoire bornée | 16 Mio par instance ; entrée et sortie plafonnées à 1 Mio. |
| Sans état | Instance neuve à chaque appel : rien ne survit ni ne fuit entre appelants. |
| Déterministe | Sans horloge ni aléa : même entrée, même sortie. |

### L'interface (ABI v1)

JSON UTF-8 dans la mémoire linéaire du plugin. Il exporte :

| Export | Signature | |
|---|---|---|
| `memory` | mémoire | |
| `alloc` | `(len: i32) -> i32` | réserve `len` octets |
| `plugin_call` | `(ptr: i32, len: i32) -> i64` | rend `(adresse_sortie << 32) \| longueur` d'une enveloppe `{"ok": …}` ou `{"error": "…"}` |
| `dealloc` | `(ptr: i32, len: i32)` | *facultatif* |
| `plugin_manifest` | `() -> i64` | *facultatif* : `{"version","description"}` |

### Un plugin en Rust (extrait de `plugins/resonance`)

```toml
# Cargo.toml — hors du workspace, cible wasm32
[lib]
crate-type = ["cdylib"]
[dependencies]
serde_json = "1"
[profile.release]
opt-level = "z"
lto = true
panic = "abort"
```

```rust
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: i32) -> i32 { /* Layout::from_size_align(len, 1) */ }

#[unsafe(no_mangle)]
pub extern "C" fn plugin_call(ptr: i32, len: i32) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    pack(envelope(handle(input)))   // handle : JSON entrant → Result<Value, String>
}
```

Compiler et tester :

```bash
rustup target add wasm32-unknown-unknown
./plugins/build.sh                                   # produit plugins/resonance.wasm
astroportd plugin --dir plugins list
astroportd plugin --dir plugins call resonance '{"fn":"derive_key","seed":"graine","label":"uplanet/nostr"}'
```

Le plugin d'exemple fournit `derive_key` (une clé par étiquette, jamais la graine) et `resonance` (similarité de deux
textes, de 0 à 1000). Côté Rust, l'hôte s'utilise ainsi :

```rust
let (host, report) = astro_plugins::PluginHost::load_dir("plugins".as_ref(), Default::default())?;
let out = host.call_value("resonance", &serde_json::json!({ "fn": "resonance", "a": "pain", "b": "pain" }))?;
```

Un plugin peut aussi s'écrire directement en WAT pour les cas minimaux — `crates/astro-plugins/tests/runtime.rs`
en contient, dont des modules *hostiles* (imports interdits, boucle infinie, mémoire gloutonne) qui démontrent la bulle.

---

## 8. État du projet et limites connues

Livré et testé : relai, uCloud chiffré avec manifeste rejouable et miniatures, FaceCloud, BRO (mémoire, RÊVE, bus),
économie 3×1/3, cycle de vie, maillage P2P et tunnels gardés, plugins Wasm, application web PWA,
`doctor`.

À connaître :
- **MULTIPASS** : la création reprend l'essentiel de `make_NOSTRCARD.sh`, mais **pas** le découpage SSSS 2-sur-3 de
  DISCO (remplacé par la **clé de secours** en 3 parts, P1 en QR), l'import de la clé IPNS dans Kubo, l'envoi du zine par
  email, la ZenCard ni la primo-transaction. Retrouver un compte repose donc sur cette carte, remise **une seule fois** ;
- **une station = une constellation** (une `swarm.key`, ou aucune). Raccorder une même station à ORIGIN **et** à ẐEN en
  parallèle est *prévu* (deux réseaux P2P parallèles, même identité Y-Level, chaque MULTIPASS rattaché à une
  constellation), pas encore disponible ;
- le client Duniter (`subxt`) n'a été éprouvé que hors ligne : son comportement sur un nœud réel reste à valider ;
- une nuit de 20h12 manquée n'est rejouée qu'au démarrage et seulement pour la veille et le jour courant ;
- l'inférence faciale a deux moteurs : un « Brain » HTTP, ou l'inférence **locale** (SCRFD + ArcFace par ONNX Runtime, `--face-models`, binaire compilé avec `--features onnx`, `astroportd face-check` pour diagnostiquer). Le binaire publié sans la fonction `onnx`, et les binaires statiques (musl, `libonnxruntime` introuvable), n'ont que le Brain HTTP ; les modèles ne sont jamais embarqués dans le binaire ;
- **tunnels et accès** (`src/tunnel_policy.rs`) : chaque service partagé (`ssh`, `comfyui`, `ollama`, plus ceux de `--p2p-expose`) a un mode réglé par le Capitaine dans Options › Tunnels, à chaud : `off`, `listed` (liste nominative, durée facultative), `friends` (Capitaines amis réciproques + liste), `swarm` (tout le réseau). **SSH est fermé par défaut en ẐEN** et ouvert aux Capitaines amis en ORIGIN ; il est offert au maillage (`--p2p-ssh`, `--p2p-comfyui`, `--p2p-ollama` : `hôte:port`, ou `none` pour ne pas l'offrir ; le relai Nostr n'est pas un tunnel réglable, son pare-feu NIP-42 le protège) mais refusé tant que le Capitaine ne l'a pas ouvert. Le bloc balisé `astroportd` de `~/.ssh/authorized_keys` suit la politique (clé jumelle de chaque pair nommé : elle n'ouvre une session que pour un Capitaine au niveau Y) ; chaque session SSH est journalisée. Retirer un accès coupe les sessions ouvertes (vérification chaque minute) ;
- **assistance entre Capitaines amis** (`src/assist.rs`) : Options › Tunnels › Assistance. Le Capitaine demande de l'aide pour un service (SSH, ComfyUI, Ollama), pour une durée, à tous ses Capitaines amis ou à un seul (ou au premier qui accepte). **Rien n'est ouvert à l'envoi** : l'accès n'est accordé, à l'ami qui accepte seulement et jusqu'à expiration, que sur sa réponse. Messages NIP-59 entre Capitaines amis réciproques ; une demande expire sans réponse au bout de 48 h ;
- **console SSH dans l'interface** (`src/forwards.rs`, `src/console.rs`) : sur une aide acceptée, « Se connecter » ouvre à chaud un accès local `127.0.0.1:<port>` vers la station de l'ami (fermé à l'échéance, sans redémarrage ni `--p2p-forward`), puis « Ouvrir la console » lance un terminal xterm.js (MIT, embarqué, chargé à la demande) relié par WebSocket à un `ssh` que **la station** exécute dans un pseudo-terminal. Ticket de 30 s à usage unique délivré au seul Capitaine, `Origin` vérifiée, utilisateur validé, `ssh` sans configuration, agent, redirection ni mot de passe, clé d'hôte retenue par PeerID, session tuée à l'échéance de l'aide. Seule la page `/ui/console.html` assouplit la CSP, et seulement pour les styles (xterm crée des `<style>`).
- **bibliothèque** (`src/library.rs`, écran `#/library`) : « ce jour-là » (jour du navigateur), chronologie par mois chargée à la demande, recherche à filtres (nom et description, type, personne, dates, zone) paginée par curseur, doublons exacts (SHA-256 du clair) et visuels (dHash 64 bits calculé avec la miniature, distance de Hamming ≤ 6, rattrapé la nuit pour les photos déjà là) avec suppression **confirmée**, et `POST /api/v1/ucloud/have` pour qu'un client n'envoie que ce qui manque. Tout est par compte : un contenu présent chez un autre compte ne se devine jamais.
- **santé du stockage et des tâches** : à 20h12, `src/integrity.rs` relit par lots les blobs référencés (absents ou corrompus signalés au Capitaine, rien n'est réparé ni supprimé) ; Options › File de traitement montre les tâches en attente, en cours et abandonnées, avec relance.
- **code identique entre stations** (`src/build_attest.rs`) : chaque station hache son binaire et publie, toutes les 6 h, une attestation signée par son NODE (kind 30853 : version, SHA-256, cible, PeerID). La carte *Constellation* de l'écran du Capitaine compare chaque pair au binaire local : identique, empreinte différente (même version et même cible : à vérifier), autre version, autre plateforme ou non attesté. L'attestation est déclarative : elle repère une dérive honnête, pas une station qui mentirait.
- **sauvegarde privée** (`src/private_backup.rs`) : un MULTIPASS peut faire héberger ses fichiers non partagés et ses contacts, chiffrés, chez un ami **qui l'accepte** et fixe le volume (compté dans son quota). Envoi une fois par nuit à 20h12, retrait après 7 jours d'un blob dont le fichier a disparu. Le **manifeste** de l'uCloud part chez l'ami avec les fichiers. **Restauration automatique** (carte secrète, voir plus haut) : l'ami renvoie sans rien faire, mais seulement s'il est **joignable** et **réciproque** ; un fichier qu'un autre ami vous avait envoyé revient seulement si cet ami est en ligne. Sans manifeste retrouvé en 3 jours, le compte repart à neuf ;
- le registre du capital (**ẐEN Capex**) est projeté depuis `.assets[]` du DID du NODE (kind 30800, `d` = PeerID) :
  l'instantané le plus récent remplace le précédent, identique en temps réel et au replay ;

---

## 9. Contribuer

Les issues et les pull requests sont les bienvenues ([issues](https://github.com/papiche/astroportd/issues)). Avant de proposer un changement : `cargo test --workspace` et
`cargo clippy --workspace --all-targets` doivent passer sans avertissement. Les messages de commit s'écrivent sans
mention de co-auteur.

*Astroport est un bien commun : forkez-le, hébergez-le, faites-le vôtre.*

---

## 10. Films et séries : la copie privée

Héritage d'`ajouter_media.sh`, sans shell. La chaîne refuse chaque fois en le disant :

1. **Attestation** : le titulaire atteste une fois (écran MULTIPASS ou `astroportd media consent`) que ses dépôts sont des
   copies pour son usage strictement privé (article L. 122-5, 2° du CPI). C'est un événement **kind 31903**
   (`d = private_copy`) signé de sa clé, qui contient le texte exact accepté : il est projeté dans
   `private_copy_consent` et revient au replay. Sans lui, rien n'est déposé ni publié.
2. **Plafond frugal** : 1 024 Mo par fichier.
3. **Sondage** (`ffprobe`) et plan : un fichier déjà MP4/H.264/AAC n'est jamais réencodé. Sinon, mode `auto` : `ffmpeg`
   réencode en H.264/AAC, **en gardant la piste audio française** (la première si aucune n'est étiquetée, avec
   remarque) ; mode `notify` : dépôt tel quel et encodage recommandé sur le bus d'activité. Outils lancés sous délai,
   `kill_on_drop`, jamais par `fork` du démon.
4. **Dépôt chiffré** (UENC v2) dans `/Vidéos/Films` ou `/Vidéos/Séries/<série>/Saison NN`.
5. **Événement vidéo** NIP-71 : **kind 21** (> 60 s) ou **22** (≤ 60 s), `imeta` (NIP-94 : `url`, `m`, `x`, `dim`,
   `duration`, `bitrate`), genres en étiquettes **NIP-32**, `series_name` / `season_number` / `episode_number` /
   `episode_name`, tag NIP-70 `["-"]`. Les métadonnées viennent d'un document TMDB (`--tmdb-json`) ou des options.
6. **Cercle N¹** : le relai ne livre un kind 21/22 qu'à son **auteur** et à ceux qu'il **suit** (N¹), jamais au reste du
   cercle N². Le N¹ se lit dans le **dernier kind 3 de l'auteur** stocké sur la station, qu'il soit hébergé ici ou sur une
   station distante de la constellation ; un lecteur authentifié (NIP-42) qui y figure est servi même s'il est inconnu
   du cercle local. Le N¹ est rechargé à chaque rafraîchissement du cercle : une liste de contacts qui change prend
   effet à ce rythme.

Limite assumée : le fichier est chiffré dans l'uCloud, l'événement n'en publie ni la clé ni une URL lisible. Pour qu'un
ami regarde, le titulaire lui envoie un lien de partage (`/s/<slug>#clé`).
