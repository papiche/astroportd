# Guide pas à pas : démarrer sa station astroportd

> Version texte du [guide illustré](index.html). Préversion **0.5.2-beta**. Chaque étape propose le **terminal (CLI)** et, quand c'est possible, le **navigateur (Web)**.

**Sommaire** : [0. Avant de commencer](#etape-0) · [1. Installer astroportd](#etape-1) · [2. Activer Y-Level](#etape-2) · [3. Choisir sa constellation](#etape-3) · [4. Démarrer la station](#etape-4) · [5. Créer mon MULTIPASS](#etape-5) · [6. Vérifier que tout va bien](#etape-6) · [7. Entrer dans l'interface web](#etape-7) · [7 bis. Le cockpit du Capitaine](#etape-7b) · [8. FaceCloud](#etape-8) · [8 bis. Contacts, agenda (CardDAV, CalDAV) et Mon compte](#etape-8b) · [8 bis-b. Mon coffre de mots de passe (KeePass)](#etape-8bis-b) · [8 bis-c. Reprendre son uCloud d'Astroport.ONE](#etape-8bis-c) · [8 bis-d. Remplir son uCloud depuis un autre serveur (Nextcloud, ownCloud…)](#etape-8bis-d) · [8 bis-e. Ranger son uCloud par type](#etape-8bis-e) · [8 bis-f. Un jeton par appareil (téléphones, tablettes, ordinateurs)](#etape-8bis-f) · [8 bis-g. Hébergement facturé, compte technique et règlement hebdomadaire](#etape-8bis-g) · [8 bis-h. Plusieurs astroportd sur une machine (ORIGIN + ZEN*)](#etape-8bis-h) · [8 bis-i. Plusieurs UPlanet dans un seul processus](#etape-8bis-i) · [8 bis-j. Messages privés et une personne dans plusieurs UPlanet](#etape-8bis-j) · [8 ter. Options du Capitaine](#etape-8c) · [8 quater. Inviter quelqu'un à recevoir des photos](#etape-8quater) · [8 quinquies. Sans email, connexion par PASS, signature déléguée](#etape-8quinquies) · [9. Relier sa station aux autres Capitaines](#etape-9) · [9 bis. Une adresse pour chaque station et chaque MULTIPASS (DNSLink)](#etape-9b) · [10. En cas de problème](#etape-10) · [Sur un smartphone](#sur-un-smartphone)

<a id="etape-0"></a>

## Étape 0 — Avant de commencer

**astroportd** est un programme unique qui fait tourner votre « Astroport » : un cloud personnel chiffré, vos photos avec reconnaissance des visages, le partage entre amis et un relais Nostr. Ce guide vous mène de zéro à une station qui fonctionne.

Pas à pas, chaque étape se fait au **terminal** (colonne de gauche) ou, quand c'est possible, au **navigateur** (colonne de droite).

### 🖥️ Terminal (CLI)

Il vous faut :

- un ordinateur **Linux** (PC, serveur) ou un **Raspberry Pi** (de préférence 64 bits : Pi 4, Pi 5, Pi Zero 2 W), ou un **Mac** Apple Silicon (bêta : voir [MACOS.md](MACOS.md)) ;
- une connexion Internet et environ 15 minutes ;
- une **horloge à l'heure** : un Raspberry Pi sans pile démarre en 1970, et astroportd refuse alors de calculer ses échéances ;
- facultatif, pour une station ouverte sur Internet : un nom de domaine et les ports 80 et 443 ouverts ;
- facultatif : de quoi reconnaître les visages (pour FaceCloud : un service d'inférence, ou les modèles locaux que l'installeur peut mettre en place) et `ffmpeg` (pour les films et séries).

Deux commandes pour vérifier votre machine :

```bash
uname -m   # x86_64, aarch64 ou armv7l
date       # l'année doit être 2026 ou plus
```

### 🌐 Navigateur (Web)

Il vous faut un navigateur récent (Firefox, Chrome, Safari…). L'interface de la station est une application web : elle s'installe aussi comme une application sur téléphone.

> **À savoir.** astroportd est en **préversion 0.5.2-beta** : il fonctionne et il est testé, mais il est jeune. Gardez une sauvegarde de vos clés.

Vous aurez aussi besoin d'un compte **MULTIPASS** (votre identité Nostr) : astroportd le crée pour vous à l'étape 5.

<a id="etape-1"></a>

## Étape 1 — Installer astroportd

Le plus simple : l'installeur. Il choisit le bon fichier pour votre machine et **vérifie son empreinte SHA-256** avant de l'installer. S'il y a le moindre doute, il s'arrête.

### 🖥️ Terminal (CLI)

```bash
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
```

Options utiles : `--version v0.5.2-beta`, `--static` (binaire statique, x86_64 et aarch64), `--prefix DIR`, `--no-init`, `--yes`. Ensuite :

```bash
astroportd --version
```

Si la commande est introuvable, ajoutez `~/.local/bin` à votre PATH : `export PATH="$HOME/.local/bin:$PATH"`.

> **À savoir.** L'installeur ne fonctionne que sous **Linux** (Android sous Termux compris, voir la fin du guide). Sur un **Mac**, téléchargez l'archive à la main (colonne Navigateur ci-contre, et [MACOS.md](MACOS.md)).

**L'installeur s'adapte à votre machine.** Reconnaître les visages et comprendre le sens des textes *dans* astroportd (sans service externe) coûte de la mémoire, du disque et du processeur. Avant de télécharger les modèles, l'installeur examine donc votre machine : architecture, nombre de cœurs, mémoire, place libre, **vitesse d'écriture du disque** (carte SD, disque mécanique, SSD, NVMe), carte graphique et présence d'Ollama. Il vous montre ce qu'il a trouvé et **recommande un profil** :

| Profil | Ce qui tourne sur la machine | Pour qui |
|---|---|---|
| **léger** | Rien en local : visages et mémoire passent par une autre machine (Brain GPU, Ollama) | Machine juste en mémoire (moins de 2 Go), en place ou en disque : Pi Zero, Pi 2/3, binaire statique |
| **standard** | Visages en local (≈ 190 Mo) et, si la machine a assez de mémoire (≈ 4 Go : au moins 3,5 Go utilisables) et de disque (≥ 2,5 Go libres), **embeddings locaux** (≈ 560 Mo) ; la moitié des cœurs au plus | Raspberry Pi 4/5 (4 Go et plus), mini-PC, NAS correct |
| **confort** | Tout en local, tous les cœurs | PC ou serveur : 8 Go de mémoire (7 Go utilisables) et 6 cœurs ou plus |

Au terminal, l'installeur propose la recommandation et vous laisse en choisir une autre ; avec `--yes`, il applique la recommandation. Il vous prévient quand la machine est juste (disque très lent, carte SD, pas d'accélération int8) mais il obéit si vous insistez.

```bash
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --check-hardware    # diagnostic seul, rien n'est installé
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --profile standard   # impose un profil : leger, standard ou confort
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --no-embed           # pas d'embeddings locaux (ni leur téléchargement de 560 Mo)
```

Chaque modèle est vérifié par SHA-256 avant d'être gardé ; un fichier incorrect est refusé. Les fils de calcul retenus sont inscrits dans `station.conf` et dans la commande de démarrage (`--face-threads`, `--embed-threads`) : un second `astroportd init` les conserve.

### 🌐 Navigateur (Web)

Ouvrez la page des versions : [https://github.com/papiche/astroportd/releases](https://github.com/papiche/astroportd/releases). Téléchargez l'archive qui correspond à votre machine :

- `x86_64-unknown-linux-gnu` : PC et serveurs ;
- `aarch64-unknown-linux-gnu` : Raspberry Pi 4/5 et Zero 2 W en 64 bits ;
- `armv7-unknown-linux-gnueabihf` : Raspberry Pi 2/3, Pi OS 32 bits ;
- `x86_64-unknown-linux-musl` : binaire statique, sans dépendance ;
- `aarch64-unknown-linux-musl` : binaire statique ARM 64 bits (Android sous Termux, Pi, NAS) ;
- `aarch64-apple-darwin` et `x86_64-apple-darwin` : **Mac** (Apple Silicon, Intel). *Bêta* : jamais compilé sur un Mac avant la 0.5.0, voir [MACOS.md](MACOS.md).

Téléchargez aussi `SHA256SUMS`. Décompressez l'archive avec votre gestionnaire de fichiers et copiez `astroportd` dans `~/.local/bin`. Comparez l'empreinte (ligne de commande : `sha256sum archive.tar.gz`) avec celle de `SHA256SUMS`.

<a id="etape-2"></a>

## Étape 2 — Activer Y-Level : une clé, quatre identités

Y-Level est le rituel qui relie **vous**, votre **machine** et le **réseau**. À partir de votre clé SSH, astroportd fabrique une graine unique. De cette graine viennent quatre identités qui partagent la même clé publique : une clé SSH « jumelle », l'identité **IPFS** de la station (`12D3Koo…`, appelée IPFSNODEID), votre portefeuille **Ğ1** et votre identité **Nostr**.

Résultat : on peut vérifier, de n'importe où, que l'accès SSH à une machine et son identité sur le réseau sont **la même clé**.

![Schéma Y-Level : de la clé SSH à quatre identités (SSH jumelle, PeerID IPFS, Ğ1, Nostr).](img/y-level.svg)

### 🖥️ Terminal (CLI)

L'assistant `init` fait ce rituel pour vous (c'est sa première étape) :

```bash
astroportd init
```

Il part de `~/.ssh/id_ed25519` (il en crée une si elle n'existe pas). **Votre ancienne clé SSH n'est pas détruite** : elle est conservée sous `~/.ssh/origin.key` et `~/.ssh/origin.pub`. Les secrets de la station sont écrits en `0600` dans `~/.zen/game`.

Pour voir la correspondance, ou la vérifier plus tard :

```bash
astroportd ylevel            # affiche SSH ↔ PeerID IPFS ↔ Ğ1 ↔ Nostr
astroportd ylevel --verify   # code de sortie 1 si ça ne correspond pas
```

> ⚠️ **Attention.** Votre clé SSH publique **change** : si vous l'aviez enregistrée ailleurs (GitHub, un serveur), il faudra y mettre la nouvelle. Pour ne pas toucher à SSH : `astroportd init --no-ssh` (le lien SSH ↔ IPFS n'est alors pas établi). Avec Kubo, `--kubo-config` aligne aussi son identité.

### 🌐 Navigateur (Web)

Il n'y a **volontairement pas d'écran web** pour cette étape : une clé privée ne doit jamais passer par un navigateur.

Une fois la station démarrée (étape 4), l'écran *Station* de l'interface web montre l'identité IPFS de votre station.

<a id="etape-3"></a>

## Étape 3 — Choisir sa constellation : ORIGIN ou ẐEN

Une **constellation** est un groupe de stations qui se parlent. Il y en a deux sortes.

**ORIGIN** est le bac à sable ouvert à tous : on apprend, on essaie, sans clé secrète (1 Ẑen = 0,1 Ğ1, un jeu). **ẐEN** est le réseau privé de production : il faut la clé du réseau, la `swarm.key` (1 Ẑen = 1 € dans la coopérative UPlanet ẐEN).

La règle est simple : **pas de `swarm.key` = ORIGIN ; une `swarm.key` présente = ẐEN.** La clé du réseau sert aussi de « nom » à la constellation (`UPLANETNAME`), dont dépend la dérivation des clés des nouveaux MULTIPASS.

![ORIGIN (réseau ouvert, bac à sable) et ẐEN (réseau privé, production).](img/origin-vs-zen.svg)

### 🖥️ Terminal (CLI)

**Parcours A — Démarrer une constellation ORIGIN** (le plus simple pour commencer)

```bash
astroportd init --mode origin
```

Aucune clé à fournir : le réseau est ouvert. Vous rejoignez les autres stations ORIGIN.

**Parcours B — Rejoindre une constellation ẐEN existante**

Un Capitaine de la constellation vous remet sa `swarm.key` (après la certification DRAGON, à demander à support@qo-op.com, objet « Candidature Capitaine DRAGON »). Gardez-la secrète.

```bash
chmod 600 swarm.key
astroportd init --mode zen --swarm-key ./swarm.key
```

**Parcours C — Fonder SA constellation ẐEN**

```bash
astroportd swarm-key > swarm.key
chmod 600 swarm.key
# Copiez ensuite swarm.key sur chaque station, par un canal sûr (scp, clé USB) :
astroportd init --mode zen --swarm-key ./swarm.key
```

> ⚠️ **Attention.** Sauvegardez `swarm.key` : la perdre, ou en changer, change le nom de la constellation (`UPLANETNAME`) et donc la dérivation des clés des futurs MULTIPASS. Ne la publiez jamais.

> **À savoir.** Fonder sa constellation crée un **réseau privé** dont vous fixez les règles. La valeur « 1 Ẑen = 1 € » est celle de la coopérative UPlanet ẐEN ; elle ne s'applique pas toute seule à une constellation que vous créez.

### 🌐 Navigateur (Web)

**ORIGIN** — [qo-op.com](https://qo-op.com) reste une **option** : c'est une station ORIGIN publique où l'on peut s'inscrire. Ce n'est pas une obligation : sur votre propre station, vous créez vos MULTIPASS vous-même (étape 5).

**ẐEN** — pour passer d'ORIGIN à ẐEN, la voie habituelle est la certification DRAGON, puis la réception de la `swarm.key`. Informations : le dépôt [Astroport.ONE](https://github.com/papiche/Astroport.ONE) et support@qo-op.com.

> **À savoir.** Une commande `astroportd serve` sert **une** constellation (une `swarm.key`, ou aucune). Pour raccorder **la même station** à ORIGIN **et** à une ou plusieurs UPlanet ẐEN, on utilise `astroportd planes` (étape 8 bis-i) : un seul processus, la **même identité Y-Level** (même `IPFSNODEID`), et pour chaque *plan* sa base, son dossier de comptes, son **réseau P2P privé** (sa `swarm.key`) et son port. Chaque MULTIPASS est rattaché à un plan et rien ne passe d'un plan à l'autre sans votre accord.

Le **premier** choix (ORIGIN ou ẐEN) se fait au terminal avec `astroportd init` : il engage une clé secrète. Ensuite, **ajouter** une UPlanet à une station existante (la fonder, ou la rejoindre avec une `swarm.key` reçue) se fait aussi dans l'interface web, écran *Station › Plans (UPlanet)* (étape 8 bis-i).

<a id="etape-4"></a>

## Étape 4 — Démarrer la station

`init` a préparé le dossier de la station (`~/.zen/astroportd` : la base `uplanet.db`, les fichiers chiffrés `blobs`, les sauvegardes `snapshots`) et vous a affiché la commande suivante. Voici les deux façons de démarrer.

### 🖥️ Terminal (CLI)

**Essai local** (HTTP simple, sur votre machine seulement) :

```bash
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
# → http://127.0.0.1:18080
```

**Station publique** (HTTPS automatique, réseau P2P, 20h12 solaire) — ajoutez vos coordonnées, elles règlent l'entretien de nuit sur le soleil :

```bash
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs \
  --domain station.exemple.org --acme-email moi@exemple.org \
  --p2p-listen /ip4/0.0.0.0/tcp/4101 \
  --lat 45.76 --lon 4.84
# en ẐEN, ajoutez : --swarm-key ./swarm.key
```

**Qu'elle démarre toute seule** (systemd, sans droits administrateur) :

```bash
astroportd init --systemd        # écrit ~/.config/systemd/user/astroportd.service
systemctl --user daemon-reload
systemctl --user enable --now astroportd
loginctl enable-linger "$USER"   # démarrer au boot sans session ouverte
journalctl --user -u astroportd -f   # suivre les journaux
```

### 🌐 Navigateur (Web)

Rien à cliquer pour démarrer : c'est le terminal qui lance le programme. Quand il tourne, ouvrez [http://127.0.0.1:18080](http://127.0.0.1:18080) (essai local) ou `https://votre-domaine/`.

Sur une **même adresse**, un navigateur reçoit l'application, et un client Nostr reçoit le relai de la station.

<a id="etape-5"></a>

## Étape 5 — Créer mon MULTIPASS

Votre **MULTIPASS** est votre identité sur la station : une clé Nostr, un portefeuille Ğ1 et une identité IPNS. astroportd le crée **directement sur votre station**, il n'y a pas besoin d'aller ailleurs.

On commence par le **Capitaine** (la personne qui tient la station), puis les autres membres s'inscrivent. Les secrets ne sont affichés qu'**une seule fois** : notez-les.

![Création d'un MULTIPASS : email, SALT/PEPPER, graine, clés Nostr, Ğ1 et IPNS isolées par constellation, carte à noter une seule fois.](img/multipass-creation.svg)

### 🖥️ Terminal (CLI)

**Le Capitaine d'abord** — le premier compte, désigné par `--captain` :

```bash
astroportd multipass create capitaine@exemple.org --captain --lang fr
```

`--captain` écrit aussi `~/.zen/game/players/.current/.player` et donne le rôle de Capitaine. C'est refusé si un autre Capitaine existe déjà. En ẐEN, ajoutez `--swarm-key ./swarm.key` : le compte est **rattaché à une seule constellation** (ORIGIN s'il n'y a pas de `swarm.key`, sinon ẐEN).

> ⚠️ **Attention.** Le programme affiche **une seule fois** : `SALT`, `PEPPER`, `nsec`, code `PASS` et jeton web. Notez-les maintenant (`--json` pour les récupérer par script). Le même SALT/PEPPER donne **d'autres clés** dans une autre constellation.

**Les membres ensuite** — ils suivent automatiquement le Capitaine (liste de contacts Nostr, kind 3) :

```bash
astroportd multipass create ami@exemple.org
astroportd multipass list
```

Options : `--lang fr|en|es`, `--relay ws://…` (publier les événements de naissance sur un relai), `--home DIR`, `--game-dir DIR`, `--db FICHIER`.

Ce que la création écrit : un dossier `~/.zen/game/nostr/<email>/` (`.secret.nostr`, `.secret.dunikey`, `.keyformula`, `.pass`, `.multipass.json` en `0600`, plus `HEX`, `NPUB`, `G1PUBNOSTR`, `LANG`, `NOSTRNS`) et trois événements signés par le compte : son **profil** (kind 0), le **suivi du Capitaine** (kind 3) et son **DID** (kind 30800).

> **À savoir.** **Limites.** La création reprend l'essentiel de `make_NOSTRCARD.sh` d'Astroport.ONE, mais pas : l'import de la clé IPNS dans Kubo, l'envoi du zine par email, la ZenCard ni la primo-transaction du compte. Le découpage SSSS du DISCO envoyé chiffré au Capitaine n'est pas repris tel quel : il est remplacé par la **clé de secours en trois parts** (voir plus bas), avec son QR (`astroportd multipass card <email> --svg`). Retrouver un compte repose donc sur cette clé de secours ; le SALT/PEPPER et le `nsec` ne sont affichés qu'une fois à la création (ils restent dans le dossier du compte et dans `--json`).

### 🌐 Navigateur (Web)

Les **membres** s'inscrivent depuis le navigateur (le Capitaine se crée au terminal, ci-contre). La station doit tourner (étape 4).

1. Ouvrez `https://votre-domaine/register` (ou, dans la fenêtre **Connexion**, le lien **« Pas de compte ? Créer mon MULTIPASS »**).
2. Saisissez votre **email** et choisissez la **langue**.
3. Une **carte d'identité** s'affiche : SALT, PEPPER, nsec, npub, Ğ1, IPNS, code PASS et jeton. **Elle ne s'affiche qu'une fois.**
4. Cliquez **« Télécharger ma carte (.txt) »** et gardez le fichier en lieu sûr.
5. Cliquez **« Ouvrir mon espace »** : vous êtes connecté directement.

L'inscription web est **ouverte par défaut**, avec des garde-fous réglables au démarrage : `--max-accounts N` (500 par défaut, `0` = illimité), `--register-per-hour N` (5 par adresse IP, `0` = illimité) et `--no-register` pour la **fermer**. Un email déjà inscrit est refusé (erreur 409), une adresse invalide aussi (400).

> **À savoir.** Sur une station publique, pensez à fermer l'inscription (`--no-register`) quand vos membres sont inscrits.

### 🔑 La clé de secours : retrouver son compte si la station tombe en panne

À la création, la carte d'identité contient une **clé de secours** : le secret de votre compte est découpé en **trois parts, dont deux suffisent**. Vous recevez la **part 1** (une ligne `astro-share:v1|…` et son **QR**). **Seule, elle ne révèle rien** : une photo ou une carte volée ne donne pas accès à votre compte. Imprimez-la et gardez-la hors ligne.

- **Part 2** : gardée par votre station (dossier du compte, `0600`).
- **Part 3** : confiée **automatiquement** aux amis qui hébergent votre sauvegarde (message chiffré pour chacun, une seule fois). Chacun la voit dans *Compte › Sauvegarde privée* (bouton *Montrer sa part de secours*), en texte et en QR.

**« J'appelle un ami ».** Station détruite, ou déménagement chez un Capitaine plus proche : sur la nouvelle station, ouvrez `/restore`, scannez votre QR (part 1), puis appelez un ami qui héberge votre sauvegarde : il vous montre sa part (part 3), vous la scannez ou la tapez. Ces deux parts rétablissent **le même compte** : mêmes amis, mêmes contacts, mêmes fichiers. La voix de l'ami est la vérification : ne lui demandez rien d'autre. Le SALT et le PEPPER ne sont plus jamais affichés. L'ancienne carte complète (`astro-secret:v1|…`) reste acceptée.

Tant qu'aucun ami n'a accepté d'héberger votre sauvegarde, la part 3 n'existe nulle part ailleurs que chez votre station : demandez à un proche d'héberger votre sauvegarde (*Compte › Sauvegarde privée*) pour que la clé de secours soit complète.

**Ce qui se passe, sans que vos amis aient quoi que ce soit à faire.** Chaque nuit, votre station range chez l'ami qui héberge votre sauvegarde vos fichiers privés **et le manifeste de votre uCloud** (la liste de vos fichiers et leurs clés, chiffrée). Après un rétablissement, la nouvelle station :

1. retrouve votre liste d'amis et vos événements Nostr par le crawl ;
2. **annonce qu'elle est votre nouvelle maison** (elle republie votre DID avec sa station d'accueil, et prévient chaque proche) : leurs stations mettent à jour votre adresse d'elles-mêmes ;
3. demande à chaque proche ce qu'il détient pour vous : leurs stations **renvoient d'elles-mêmes** le manifeste d'abord, puis les fichiers sauvegardés et ceux que vous leur aviez partagés ;
4. applique le manifeste : votre uCloud et son trousseau reviennent, les fichiers au fil des heures ; l'accord de sauvegarde est rétabli, rien n'est renvoyé inutilement ;
5. redemande par leur identifiant les fichiers qui manquent encore (ceux que d'autres amis vous avaient envoyés) : un ami ne rend que ce qu'il a lui-même partagé avec vous.

Vous êtes prévenu dans votre messagerie BRO quand c'est fait. Si rien n'est retrouvé en 3 jours, le compte repart à neuf (vos amis et contacts restent les vôtres) et le Capitaine est alerté.

### 🖥️ Terminal (CLI)

```bash
astroportd multipass card alice@exemple.org            # réaffiche la part 1 (celle du membre) d'un compte hébergé ici
astroportd multipass card alice@exemple.org --svg > carte.svg   # …en QR à imprimer
astroportd multipass card alice@exemple.org --part 3    # la part que gardent les amis
# Sur la NOUVELLE station (les deux parts, une par ligne, lues sur l'entrée standard : un argument se lirait dans `ps`) :
astroportd multipass recover - --relay wss://relais.ami.example < parts.txt
```

`--relay` (répétable) rapatrie tout de suite les événements du compte ; sans lui, le démon (`astroportd serve`) s'en charge au prochain crawl. Un **nouveau code PASS** est généré et affiché une fois : l'ancien est tiré au hasard et n'est pas récupérable.

### 🌐 Navigateur (Web)

Ouvrez `https://votre-nouvelle-station/restore`, **scannez le QR** (si votre navigateur sait le lire) ou **collez la carte**, puis **« Rétablir mon compte »**. Notez le nouveau PASS, puis **« Ouvrir mon espace »**.

> **À savoir.** La station refuse la carte d'une autre constellation, et toute carte dont les secrets ne redonnent pas la clé annoncée (faute de frappe). **Ne modifiez rien tant que la restauration n'est pas terminée** : tant qu'elle dure, la station ne publie pas de manifeste (un manifeste vide remplacerait celui que vos amis détiennent).
>
> **Déménagement volontaire.** Si l'ancienne station tourne encore, la nouvelle lui écrit un message `moved`, signé de la clé de votre compte : l'ancienne cesse d'héberger ce compte (plus de connexion ni de sauvegarde pour lui) et met son dossier de côté dans `nostr/.moved/`, sans rien effacer. Si elle est détruite, il n'y a personne à prévenir.
>
> **Limites actuelles.** (1) Un fichier qu'un ami vous avait envoyé revient seulement si cet ami est joignable. (2) Seuls les amis réciproques participent. (3) L'ancienne station doit être à jour pour comprendre `moved` ; sinon arrêtez-la.

<a id="etape-6"></a>

## Étape 6 — Vérifier que tout va bien

Trois vérifications, de la plus complète à la plus rapide. Aucune ne modifie rien.

### 🖥️ Terminal (CLI)

```bash
astroportd doctor --db ~/.zen/astroportd/uplanet.db --game-dir ~/.zen/game
astroportd ylevel --verify
curl http://127.0.0.1:18080/health
```

`doctor` contrôle l'identité, les permissions `0600` des secrets, la base, les certificats HTTPS et l'horloge (il affiche le prochain 20h12 solaire). Code de sortie `1` si une vérification échoue.

Pour voir le relai Nostr de la station :

```bash
curl -H 'Accept: application/nostr+json' http://127.0.0.1:18080/
```

### 🌐 Navigateur (Web)

Ouvrez `http://127.0.0.1:18080/health` : une page courte indique l'état de chaque partie de la station (OK ou en panne).

Si l'application se charge sur `http://127.0.0.1:18080/`, c'est que le serveur web fonctionne.

<a id="etape-7"></a>

## Étape 7 — Entrer dans l'interface web

L'application web s'ouvre sur l'**Accueil** : trois gestes (**Mes photos**, **Mes proches**, **Mon assistant** — ou **Mon smartphone** quand il n'y a pas d'assistant) et une liste **Bien démarrer**. Les autres écrans : **Station** (réservé au Capitaine, voir l'étape 7 bis), **uCloud**, **Personnes**, **Contacts**, **Messages**, **Réplication** et **Mon compte**.

Pour s'y connecter, il faut un compte hébergé par la station (étape 5) et son **jeton**.

![Schéma de l'écran MULTIPASS : cartes Solde, Autonomie, Empreinte, Souveraineté, Ẑen Opex et Ẑen Capex.](img/multipass-ecran.svg)

### 🖥️ Terminal (CLI)

Le jeton est affiché à la création du compte. Pour le retrouver (son email) :

```bash
astroportd token vous@exemple.org --game-dir ~/.zen/game
```

Le même jeton sert de mot de passe pour monter votre cloud en WebDAV (Finder, Explorateur Windows, application mobile) : adresse `https://votre-domaine/dav`, identifiant = votre email.

### 🌐 Navigateur (Web)

1. Ouvrez la station dans votre navigateur.
2. Une fenêtre **Connexion** s'affiche : saisissez votre email (ou votre clé) et le jeton.
3. L'**Accueil** vous propose d'envoyer vos premières photos, de **synchroniser votre smartphone** (photos, contacts, agenda : on quitte Google et Apple), de reconnaître un visage, de retrouver un proche, de mettre vos fichiers à l'abri chez un ami. Les étapes se cochent toutes seules ; la liste disparaît quand tout est fait. Une alerte n'apparaît que si votre espace dépasse 90 %.
4. En bas de l'accueil, **Mes chiffres** montre votre solde en Ẑen, votre autonomie, votre empreinte sur les communs, vos fichiers chiffrés, et deux cartes distinctes : **Ẑen Opex** (ce que vous dépensez : loyer) et **Ẑen Capex** (ce que vous possédez : parts de matériel, avec leur amortissement).
5. La carte **Copie privée** vous demande d'attester, une fois, que vos films et séries sont des copies pour votre usage privé.

![L'Accueil d'un membre : trois gestes (photos, proches, assistant ou smartphone) et la liste « Bien démarrer ».](img/ui-accueil.png)

Sur un téléphone, installez la page comme application (menu du navigateur : *Ajouter à l'écran d'accueil*).

**Style de l'interface.** Dans l'en-tête, le sélecteur propose trois styles : **Aurore** (par défaut : dégradés, icônes, liseré coloré sur les cartes), **Sobre** (sans décor) et **Kraft** (papier chaud, inspiré des pochettes de FaceCloud). Le bouton voisin (◐ ☀ ☾) choisit le mode : automatique (celui de l'appareil), clair ou sombre. Ces choix restent sur l'appareil et ne sont envoyés nulle part.

<a id="etape-7b"></a>

## Étape 7 bis — Le cockpit du Capitaine

Dans l'application web, l'écran **Station** est le **cockpit** : tout ce qu'un Capitaine doit voir, d'un seul regard.

### 🌐 Navigateur (Web)

1. En haut, **un verdict** : ✅ *Tout va bien*, 🟠 *N points à regarder* ou 🔴 *Intervention nécessaire*.
2. **À faire** : les trois actions les plus urgentes, chacune avec sa raison en clair et un bouton qui mène à l'écran qui la règle (sous-système en panne, P2P arrêté, SSH ouvert à tout le réseau, service réglé qui ne répond plus, tâches abandonnées, MULTIPASS presque pleins, comptes verrouillés ou en période de grâce, trésorerie entamée).
3. Sur une station neuve, **Mettre la station en service** : nom de domaine, maillage P2P, autres stations vues, adresses de la coopérative, premier membre invité. Les étapes se cochent d'après l'état réel.
4. **Les étages de la station**, chacun avec son voyant : Machine, Station, Constellation, Services et accès, Membres, Économie.
5. **L'essaim** : une carte par station (Power-Score, distance, en ligne ou vue, services partagés) et le nombre de *Brains* qui offrent Ollama.
6. La page se met à jour toute seule toutes les 30 secondes. Les écrans détaillés restent à un clic (*Détails techniques*).

![Le cockpit d'une station neuve : verdict, À faire, mise en service, étages et essaim.](img/ui-cockpit.png)

<a id="etape-8"></a>

## Étape 8 — FaceCloud : retrouver et partager vos photos entre amis

C'est l'application phare. Vous déposez vos photos dans votre **uCloud** (chiffré avant de quitter votre appareil). La station y repère les **visages**, les compare, et vous aide à les nommer. Les photos où figure un ami peuvent alors lui être **partagées automatiquement**.

Deux cercles : vos **amis N¹** (ceux que vous suivez *et* qui vous suivent) reçoivent le partage automatique ; pour tous les autres, vous créez un **lien secret**.

![Parcours FaceCloud : photos, visages, album, amis N¹, lien secret.](img/facecloud-partage.svg)

### 🖥️ Terminal (CLI)

FaceCloud s'active quand la station sait où faire l'inférence faciale : un service HTTP de reconnaissance (modèle InsightFace) :

```bash
astroportd serve ... --face-engine http://127.0.0.1:8000 --relay ws://127.0.0.1:7777
```

`--relay` indique où publier les partages vers les amis qui sont sur **une autre station** (message chiffré, enveloppé NIP-59). Sans `--face-engine` ni `--face-models`, uCloud fonctionne mais sans reconnaissance.

**Sans service HTTP : l'inférence locale (facultatif).** Une station autonome peut exécuter SCRFD (détection) et ArcFace (empreinte 512D) dans le binaire, par ONNX Runtime :

```bash
cargo build --release --features onnx                      # ou la version publiée compilée avec cette fonction
export ORT_DYLIB_PATH=/chemin/vers/libonnxruntime.so       # chargée à l'exécution : rien n'est téléchargé à la compilation
astroportd serve ... --face-models ~/modeles/buffalo_l     # contient det_10g.onnx et w600k_r50.onnx
```

Pour savoir ce qui manque, lancez `astroportd face-check` : il vérifie la fonction compilée, les modèles, la bibliothèque et le chargement. Si les deux modèles sont dans `~/.zen/astroportd/models` (ou `~/.insightface/models/buffalo_l`) et la bibliothèque aux emplacements habituels (dont le `onnxruntime` d'un venv pip), `serve` active l'inférence locale **tout seul**, sans option.

Les deux modèles ne sont **pas** fournis avec astroportd : ce sont ceux du paquet InsightFace `buffalo_l`. Leurs poids sont publiés pour la **recherche non commerciale** : vérifiez la licence avant tout usage commercial, ou utilisez des modèles compatibles dont la licence vous convient (mêmes noms de fichiers, mêmes entrées et sorties). Mesure sur un PC de bureau (build de développement, 2 fils) : environ 0,5 s par photo ; le binaire optimisé est nettement plus rapide.

**Mémoire et recherche : des embeddings calculés dans la station (facultatif).** La mémoire de BRO et la recherche par le sens transforment chaque texte en un vecteur de 1024 nombres (modèle `bge-m3`). Par défaut, c'est Ollama qui les calcule (`--ollama http://…`). Une station peut aussi les calculer **elle-même**, sans Ollama, avec la version quantifiée en entiers 8 bits (int8) de `bge-m3`, par le même ONNX Runtime que les visages :

```bash
astroportd embed-check                                     # fonction compilée, fichiers, bibliothèque, chargement, essai
astroportd serve ... --ollama http://127.0.0.1:11434       # les vecteurs sont calculés en local si les fichiers sont là
astroportd serve ... --embed-models ~/modeles/bge-m3       # ou impose le dossier (erreur claire s'il est incomplet)
astroportd serve ... --embed-threads 2                     # limite les fils de calcul (0 = tous les cœurs)
```

Les deux fichiers sont `bge-m3-int8.onnx` (≈ 540 Mo) et `bge-m3-tokenizer.json` (≈ 16 Mo), dans `~/.zen/astroportd/models` : l'installeur les met en place (profils *standard* et *confort*), vérifiés par SHA-256. Ils viennent de `Xenova/bge-m3` (licence MIT). Sans option, `serve` les utilise **tout seul** quand ils sont présents (et que `libonnxruntime` est trouvée) ; sinon Ollama reste le moteur, et si les fichiers sont là mais inutilisables, la station le dit à l'écran avant de se rabattre sur Ollama. Ce sont les mêmes 1024 dimensions que celles d'Ollama, et le calcul reproduit exactement celui d'`onnxruntime` sur ce même fichier (cosinus 1,000). **Attention** : le fichier est *quantifié* (int8) alors qu'Ollama sert une autre version du modèle ; leurs vecteurs sont très proches mais **non identiques**, et cet écart n'a pas été mesuré. Sur une mémoire déjà indexée par Ollama, gardez le même moteur ou ré-indexez avant de changer.

Ordres de grandeur sur un PC de bureau à 12 cœurs : chargement ≈ 2 s au démarrage, environ 20 ms pour une phrase courte et 170 ms pour 200 jetons (≈ 150 mots), **≈ 1,3 Go de mémoire au pic**. Ollama reste nécessaire pour les **résumés** du cycle RÊVE ; seuls les vecteurs sont calculés en local. Le binaire statique (musl) n'a pas cette fonction.

L'écran **Personnes** (« table de tri ») montre les visages sur une table lumineuse : chaque pastille est un visage, les visages inconnus qui se ressemblent sont regroupés en grappes, et on les **glisse** sur la pochette d'une personne (ou au lasso, ou avec la barre d'actions au clavier). Les vignettes sont recadrées par la station ; les empreintes ne quittent jamais la station.

Déposez vos photos par WebDAV, par exemple avec `curl` :

```bash
curl -u vous@exemple.org:JETON -T photo.jpg https://votre-domaine/dav/Photos/photo.jpg
```

### 🌐 Navigateur (Web)

1. Écran **uCloud** → bouton **Album** : vos photos, de la plus récente à la plus ancienne.
2. Écran **Visages & BRO** : les visages détectés. Un visage reconnu à plus de 82 % porte son nom ; entre 55 % et 82 %, la station demande « est-ce X ? » : vous acceptez ou vous nommez vous-même.
3. Entre amis N¹ réciproques, les photos sont copiées dans `/Partages/De_<nom>/` chez l'ami (même station), ou lui sont envoyées chiffrées (autre station).
4. Pour partager avec quelqu'un d'autre (cercle N², invités) : ouvrez une photo, bouton **Partager** → **Lien magique**. Durée de 1 heure à 30 jours (72 h par défaut). Envoyez le lien tel quel : la clé de déchiffrement est après le `#` et ne quitte **jamais** votre navigateur ni celui de l'invité.
5. Les liens expirés sont supprimés chaque nuit, avec leurs copies chiffrées.

> **À savoir.** Les films et séries déposés (copie privée) ne sont publiés que pour votre cercle N¹, jamais pour tout le cercle N².

<a id="etape-8b"></a>

## Étape 8 bis — Contacts, agenda (CardDAV, CalDAV) et Mon compte

**Carnet d'adresses (CardDAV).** Chaque compte a un carnet synchronisable avec « Contacts » d'Android (DAVx⁵) et d'iPhone. Les fiches sont des fichiers `.vcf` chiffrés du dossier `/Contacts` de votre uCloud (donc répliqués, sauvegardés et comptés dans votre quota), servis en CardDAV (RFC 6352) :

- **Serveur** : `https://<station>/dav/` (la découverte `/.well-known/carddav` est prise en charge) ; **identifiant** : votre email ; **mot de passe** : votre jeton (écran *Mon compte*).
- Limites connues : `sync-collection` est pris en charge (jeton = numéro du journal des changements, suppressions comprises, gardé 30 jours ; un jeton plus ancien renvoie le client à une synchronisation complète) ; `addressbook-query` renvoie tout le carnet sans filtrer.

**Agenda (CalDAV).** Même principe pour l'agenda : « Agenda » d'Android (DAVx⁵), d'iPhone ou Thunderbird se synchronise avec le dossier `/Calendrier` de votre uCloud. Chaque événement, tâche ou note est un fichier `.ics` **chiffré, répliqué avec vos sauvegardes et jamais publié sur Nostr** (rien de public par défaut).

- **Serveur** : le même `https://<station>/dav/` (découverte `/.well-known/caldav` et `calendar-home-set` du principal) ; mêmes identifiants. Un seul agenda par compte, nommé « Agenda ».
- Limites connues : `calendar-query` renvoie tout l'agenda sans appliquer le filtre de dates ni développer les récurrences (le client le fait) ; pas d'invitations entre participants ; pas de création d'agendas supplémentaires. `sync-collection` est pris en charge, avec le même journal que le carnet.

**Rapprochement photo ↔ fiche ↔ MULTIPASS.** Une fiche relie un visage à un MULTIPASS de deux façons : la fiche porte une clé Nostr (`X-NOSTR-PUBKEY` ou `X-NOSTR-NPUB`, en hexadécimal ou `npub1…`), ou son email est celui d'un compte **hébergé par cette station** (un email déclaré dans un DID distant n'est jamais une preuve). Les visages de même nom (sans accents ni casse) prennent alors cette clé ; deux fiches du même nom pour deux clés différentes restent ambiguës et ne sont pas rapprochées. Si vous êtes Proches réciproques, les photos déjà analysées de cette personne lui sont partagées aussitôt. L'écran **Contacts** montre les fiches, leur lien, les personnes reconnues sans fiche (*Créer la fiche*) et permet de relier à la main (email d'un MULTIPASS de la station, ou `npub`) ; une clé ajoutée à la main est écrite dans la fiche, donc elle suit le carnet sur votre téléphone.

**Mon compte.** Identité et clés publiques, profil Nostr (nom, présentation, image : un nouveau kind 0 signé par la station pour votre compte), langue de l'interface (français, English, Español), relais annoncés (NIP-65), **changement du jeton** (l'ancien cesse de fonctionner immédiatement ; la génération est conservée dans la base) et **export de vos clés** (case à cocher de confirmation, 3 exports par heure, chaque export est tracé et vous est signalé).

## Étape 8 bis-b — Mon coffre de mots de passe (KeePass)

Votre uCloud accueille un **coffre KeePass** (`.kdbx`) dans le dossier `/Coffre`. Le fichier est chiffré par **votre** mot de passe maître, que la station ne connaît pas : l'hébergeur, le Capitaine et les amis qui gardent votre sauvegarde n'en voient qu'un fichier opaque. Il est répliqué avec vos sauvegardes et comptabilisé dans votre quota. **Il n'est jamais partageable** : ni lien d'invité, ni envoi à un ami, même en désignant le dossier racine.

**Deux coffres, deux niveaux de confiance.**

- **`Coffre/perso.kdbx`** : tout ce qui compte (banque, impôts, santé). Il n'est lu que par vos applications.
- **`Coffre/agents.kdbx`** : seulement les comptes que vous choisissez de confier à un assistant (BRO ou un agent que vous déclarez). Créez d'abord `perso.kdbx`, puis **copiez-y les entrées** à déléguer vers `agents.kdbx` (dans KeePassXC, ouvrez les deux bases et déplacez ou copiez l'entrée de l'une à l'autre). Une entrée copiée n'est plus liée à l'original : changer un mot de passe se fait aux deux endroits.

*Les agents (BRO ou un autre) passent par le serveur MCP décrit plus bas : ils **demandent**, vous décidez. L'ouverture de `agents.kdbx` par la station n'est pas encore faite.*

**Brancher une application** (serveur et identifiants comme pour les contacts : `https://<station>/dav/`, votre email, votre jeton de *Mon compte*) :

| Plateforme | Application | Branchement |
|---|---|---|
| Android | Keepass2Android | *Ouvrir un fichier › WebDAV*, adresse `https://<station>/dav/Coffre/perso.kdbx` |
| iPhone | Strongbox | *Ajouter une base › WebDAV* |
| Ordinateur | KeePassXC | monter `/dav/` comme lecteur réseau (WebDAV du gestionnaire de fichiers), ou télécharger le fichier à la main |

À chaque enregistrement, l'application écrase le fichier (la station renvoie `204`) et relit la dernière version à l'ouverture. Si deux appareils modifient en même temps, KeePass propose de fusionner : acceptez.

**Agents et serveur MCP (demande d'accord).** La station parle MCP (JSON-RPC 2.0, `POST https://<station>/mcp`). Un agent s'y présente avec **son propre jeton**, jamais avec votre compte :

1. Vous déclarez l'agent (`POST /api/v1/agents` avec `{ "name": "BRO", "group": "Agents", "max_secs": 600 }`, authentifié comme pour le reste de l'API). La réponse contient son **jeton `ag1_…`, montré une seule fois** : la station n'en garde que l'empreinte SHA-256. Huit agents au plus par compte ; pour en retirer un : `POST /api/v1/agents/revoke`.
2. Le registre est un événement Nostr **kind 31903, `d = agents`**, signé de votre clé et **chiffré pour vous seul** (NIP-44) ; il suit vos sauvegardes et se reconstruit au `replay`.
3. L'agent appelle l'outil `demander_acces` (`site` = nom d'hôte seul, par exemple `impots.gouv.fr`, et une `raison` de 200 caractères) puis interroge `statut_acces`. Vous recevez la demande dans le flux d'activité et la tranchez par `POST /api/v1/agents/decide` (`{ "id", "approve", "secs" }`). Une demande non traitée expire au bout de 5 minutes ; la durée d'un accord est plafonnée par l'agent et par une heure.
4. Chaque demande, accord, refus et retrait est écrit dans le journal d'audit (`audit.agent`).

**Ce que cette étape ne fait pas :** un agent ne reçoit jamais de mot de passe (il reçoit « en attente », « accordé », « refusé » ou « expiré »), et pour l'instant l'accord n'ouvre pas non plus `agents.kdbx` : il est seulement enregistré et tracé. La remise d'un identifiant à un navigateur reste à construire. Pas d'écran dédié non plus : l'API et le flux d'activité suffisent aux premiers essais. Limites : 5 demandes en attente et 20 par heure et par agent.

**Quitter le coffre de Firefox.**

1. Dans Firefox : `about:logins` › menu **⋯** › *Exporter les identifiants…* (un mot de passe principal éventuel est demandé).
2. Dans KeePassXC : *Base de données › Importer › Fichier CSV*, vérifier l'association des colonnes (URL, identifiant, mot de passe), enregistrer sous `perso.kdbx`.
3. **Supprimer le fichier CSV tout de suite** : il contient tous vos mots de passe en clair. Idéalement, faites l'opération sur un disque chiffré.
4. Installer l'extension **KeePassXC-Browser** et désactiver l'enregistrement des mots de passe de Firefox, pour ne pas garder deux coffres.

Les codes à usage unique (TOTP) ne migrent pas : Firefox n'en stocke pas ; KeePass sait les générer, saisissez-les à la main.

**Si vous oubliez le mot de passe maître**, personne ne peut le retrouver, ni vous aider : ni la station, ni le Capitaine, ni vos amis (la clé de secours rétablit votre compte, pas ce mot de passe). Notez-le sur papier, ou ajoutez un fichier clé à votre coffre.

## Étape 8 bis-c — Reprendre son uCloud d'Astroport.ONE

`astroportd import-ucloud <email>` reprend le uCloud d'un compte (`~/.zen/game/nostr/<email>/.ucloud`) **sans rien recopier ni ré-encoder** : le nouveau VFS référence les **mêmes blobs chiffrés** (UENC v1, déjà dans IPFS) avec **les mêmes clés**. L'ancienne base n'est ouverte qu'en lecture seule et n'est jamais modifiée.

1. **À blanc d'abord** : `astroportd import-ucloud fred@example.org --dry-run`. Il lit l'ancien uCloud, dit ce qu'il reprendrait (fichiers, taille, dossiers, clés) et ce qu'il **écarterait avec la raison** (liste complète dans `import-ucloud-ecartes.tsv`), puis **relit, déchiffre et compare** un échantillon de fichiers (`--verify`, 60 par défaut) : taille et SHA-256 du clair doivent être ceux annoncés par l'ancienne base. Au moindre échec, rien n'est écrit.
2. **Pour de bon** : même commande sans `--dry-run`. Le compte ne doit pas déjà avoir de fichiers dans astroportd (sinon `--replace`). Le manifeste Nostr est publié à la fin (`--no-publish` pour s'en abstenir).
3. **`astroportd serve` doit tourner avec `--kubo http://127.0.0.1:5001`** : les anciens blobs n'existent que dans IPFS (CID v0 `Qm…`). Sans cela, les entrées s'affichent mais leur contenu est illisible.

Ce qui change pour ces fichiers : ils se lisent et se téléchargent comme les autres (`Range` compris) ; **les supprimer retire l'entrée sans jamais désépingler le blob** (l'ancienne station le référence encore) ; ils **ne sont pas** sauvegardés chez les amis ni répliqués par P2P tant qu'ils n'ont pas été ré-enregistrés au format natif, et le contrôle d'intégrité de nuit les laisse de côté. Non repris : empreintes de visages (Qdrant) et vignettes, qui se recalculent ; les index vectoriels de descriptions.

**Avant un import réel, mettez le démon à jour** : cette version migre le schéma de `uplanet.db` à l'ouverture, un démon d'une version antérieure qui tourne encore ne s'y retrouverait pas. Arrêtez-le, sauvegardez la base, lancez l'import, redémarrez.

## Étape 8 bis-d — Remplir son uCloud depuis un autre serveur (Nextcloud, ownCloud…)

Le uCloud peut se remplir en **tirant** (mode « pull ») un dossier d'un autre serveur WebDAV : un Nextcloud, un ownCloud, ou une autre station astroportd. À lancer depuis l'interface : **uCloud › Sources**.

1. **Ajouter une source** : adresse WebDAV du serveur (par exemple `https://nextcloud.example/remote.php/dav/files/votre-identifiant`), identifiant et **mot de passe d'application** (à créer côté serveur : vous pouvez le révoquer à tout moment). Dossier distant, libellé et dossier de dépôt sont facultatifs. L'adresse et les identifiants sont vérifiés avant l'enregistrement, et le mot de passe est rangé **chiffré pour vous seul**.
2. **Aperçu** : compte ce qui serait repris, sans rien écrire. **Reprendre** : lance le passage en arrière-plan, avec une barre de progression ; **Arrêter** l'interrompt (le fichier en cours s'achève). Un seul passage à la fois par compte.
3. **Les fichiers arrivent tels quels** sous le dossier de dépôt (`/Imports/<libellé>` par défaut), avec l'arborescence du serveur : **aucun tri par dates**. Les ranger comme un seul disque, alimenté par plusieurs sources, est le rôle d'une fonction de rangement à part.
4. **Rejouable** : un fichier déjà repris n'est ni repris ni écrasé, même si vous l'avez déplacé ailleurs dans le uCloud depuis (la station retient ce qui vient de quelle source). Un fichier modifié depuis sur le serveur est signalé, jamais dupliqué.

Garde-fous : seules les adresses **publiques** sont jointes depuis l'interface (un serveur sur votre réseau local se branche par la ligne de commande de l'opérateur, `--allow-private`) ; les identifiants ne suivent **jamais** une redirection vers un autre serveur ; un serveur qui désignerait des chemins hors du dossier demandé ne peut rien écrire ailleurs ; fichiers de 200 Mo au plus, 50 000 fichiers au plus par passage.

En ligne de commande : `astroportd pull-webdav <compte> add --url … --user … --password-stdin --remote-path Photos [--dest /Imports/X] [--allow-private]`, puis `list`, `run [id] [--dry-run] [--limit N]` et `remove <id>`. Le mot de passe se lit sur l'entrée standard, jamais en argument.

## Étape 8 bis-e — Ranger son uCloud par type

Le uCloud est alimenté par de nombreuses sources (dépôts, reprises WebDAV, téléphones). **uCloud › Ranger** le réorganise **comme un ordinateur de bureau** : Documents, Images, Musique, Vidéos, Archives.

1. **Voir ce qui serait rangé** : un aperçu par type (nombre de fichiers, taille, exemples de destination), et ce qui reste en place. Rien n'est écrit.
2. **Réglages** : le dossier à ranger (`/` pour tout le uCloud, ou par exemple `/Imports`), des sous-dossiers par date (aucun, par année, par année et mois : la date de prise de vue, sinon de modification) et les types à ranger.
3. **Ranger maintenant** : le rangement tourne en arrière-plan, avec une barre de progression et un bouton **Arrêter**. Les dossiers laissés vides disparaissent.
4. **Annuler ce rangement** : chaque passage est journalisé, et un bouton remet chaque fichier à sa place d'avant (les dossiers sont recréés ; si une place a été reprise entre-temps, le fichier revient sous un numéro).

Seuls les **chemins** changent : aucun fichier n'est recopié ni re-chiffré (le rangement de 30 000 photos est donc quasi instantané), vos **visages** et vos **partages** suivent leur photo. Jamais touchés : le coffre, les contacts, l'agenda, les partages reçus, les sauvegardes et tous les dossiers cachés. Un fichier de type inconnu reste où il est, un fichier déjà dans un dossier de type aussi, et un nom déjà pris reçoit un numéro (jamais d'écrasement).

## Étape 8 bis-f — Un jeton par appareil (téléphones, tablettes, ordinateurs)

**Mon compte › Appareils** donne à chaque appareil **son propre jeton WebDAV**, **confiné à un dossier** de votre uCloud : son envoi arrive dans le répertoire que vous choisissez (par exemple `/Photos/Téléphone de Léa`), et il ne voit ni ne touche rien d'autre.

1. **Ajouter un appareil** : un nom, un dossier (créé s'il manque ; par défaut `/Photos/<nom>`) et des droits : **envoi seul** (déposer, créer des dossiers, déplacer ; ni lire, ni supprimer : le bon choix pour une appli de sauvegarde de photos), **lecture et écriture**, ou **lecture seule** (un écran qui montre un album).
2. Le **jeton** (`dv_…`) est montré **une seule fois**, avec l'adresse du serveur et votre identifiant : à saisir dans l'application de l'appareil (adresse WebDAV, identifiant = votre email, mot de passe = ce jeton). La station n'en garde que l'empreinte.
3. L'appareil voit son dossier comme la **racine** du WebDAV : les liens renvoyés n'indiquent jamais le chemin au-dessus. Aucun `..` ne l'en fait sortir. Contacts et agenda lui sont fermés (utilisez le jeton principal pour synchroniser le carnet d'adresses et l'agenda).
4. **Révoquer** coupe le jeton tout de suite ; ce que l'appareil a déjà déposé reste dans votre uCloud. Le **dernier envoi** de chaque appareil est affiché. 16 appareils au plus. Un jeton d'appareil n'ouvre jamais l'interface : seulement ce dossier, en WebDAV.

Le jeton principal du compte (et NIP-98) fonctionne comme avant, sans confinement.

## Étape 8 bis-g — Hébergement facturé, compte technique et règlement hebdomadaire

Le Capitaine fixe, par MULTIPASS hébergé, un **quota** et un **prix hebdomadaire en ẑ** (Station › Comptes hébergés › *Hébergement*), et un **palier par défaut** pour les autres. Sans réglage, tout est gratuit.

1. **Prélèvement** : la machine d'états du cycle de vie décide ; tous les 7 jours depuis le dernier prélèvement réussi, le prix part du MULTIPASS sur la Ğ1 vers le **compte technique d'hébergement** (`<UPLANETNAME>.HOSTING.<PeerID du NODE>` : indépendant de l'email du Capitaine). Un prix tout juste fixé ne débite que dans une semaine. Solde insuffisant : *période de grâce* de 7 jours puis *suspension* — un **statut et un message**, jamais une coupure du service, jamais une suppression.
2. **Règlement hebdomadaire** (avec `--weekly-costs` = 3×PAF, en centimes de ẑ ; `0` désactive) : une fois la semaine clôturée à 20h12, si le montant reçu le permet, le compte technique vire 1×PAF au NODE, 2×PAF au MULTIPASS du Capitaine, puis le surplus ventilé par la clôture (`IMPOT`, prime de 1 %, `TREASURY`, `RND`, `ASSETS`).
3. **Transaction primale** : le portefeuille maître `UPLANETNAME_G1` envoie **1 Ğ1** à chaque portefeuille coopératif vide (`UPLANETNAME`, `SOCIETY`, `TREASURY`, `RND`, `ASSETS`, `IMPOT`, `INTRUSION`, `CAPITAL`, `AMORTISSEMENT`), au compte technique et au NODE : c'est leur *primal*, qui les marque comme appartenant à cette UPlanet (équivalent de `UPLANET.init.sh`). **Désactivé par défaut (`--primal-init` pour l'activer) : cela dépense de vrais Ğ1.** Le maître doit détenir au moins 1 Ğ1 par portefeuille en plus de son propre dépôt ; sinon la station le dit une fois par jour et reprend seule.

Chaque virement est noté **avant** d'être signé ; une issue inconnue n'est jamais rejouée (alerte au Capitaine : vérifier le solde du compte concerné).

<a id="etape-8bis-h"></a>

## Étape 8 bis-h — Plusieurs astroportd sur une machine (ORIGIN + ZEN*) : un seul bassin de blobs

Chaque plan est un `astroportd serve` à lui (ses `--db`, `--game-dir`, ports, `--swarm-key`). Pour **ne pas payer deux fois le disque**, tous pointent leur `--blobs` vers **le même dossier**, et chacun déclare les bases des autres avec `--pool-peer` :

```bash
astroportd serve --db ~/.zen/origin/uplanet.db --blobs ~/.zen/pool --pool-peer ~/.zen/zen1/uplanet.db --kubo http://127.0.0.1:5001 …
astroportd serve --db ~/.zen/zen1/uplanet.db   --blobs ~/.zen/pool --pool-peer ~/.zen/origin/uplanet.db …   # sans --kubo : Kubo n'appartient qu'à ORIGIN
```

- **Un blob n'est supprimé que si plus aucun plan ne le référence** : avant de supprimer, la station interroge la base de chaque pair en lecture seule (entrées, clés, miniatures, manifestes). Un pair illisible fait **garder** le blob (une place perdue vaut mieux qu'un fichier perdu).
- **Adopter sans copier** : `astroportd bridge-copy --from-db … --from alice@… --to-db … --to alice@… --blobs ~/.zen/pool --path /Photos --dest /Imports/ORIGIN --same-person` fait apparaître les fichiers du premier plan dans le second, **même blob, même clé, zéro octet de plus**. `--same-person` atteste l'accord explicite (les deux comptes partagent la clé de fichier). Rejouable, jamais d'écrasement, quota du compte de destination appliqué, `--dry-run` pour compter. Les fichiers de l'ancien format (CID `Qm…`, vivants dans Kubo) et ceux absents du bassin sont **signalés, jamais copiés en silence** : leur transfert demande un ré-chiffrement (non fait).
- **Contrôler le bassin** : `astroportd pool-status --blobs ~/.zen/pool --db …/origin/uplanet.db --db …/zen1/uplanet.db` affiche le poids, la part de chaque plan, la **place économisée par le partage** et les orphelins (`--sweep` supprime ceux de plus de 24 h).

<a id="etape-8bis-i"></a>

## Étape 8 bis-i — Plusieurs UPlanet dans un seul processus : assistant et `astroportd planes`

Un **plan** est une UPlanet vue de cette machine : ORIGIN (sans `swarm.key`) ou une UPlanet privée (ZEN1, ZEN2…, avec sa `swarm.key`). Une seule commande les sert tous : même NODE (même `IPFSNODEID`), même bassin de blobs, **une base, un dossier de comptes et un port par plan** ; l'étanchéité est structurelle.

**Configurer, au choix :**
- **En ligne de commande** : `astroportd plane-setup list` · `plane-setup add` (pose les questions : fonder une UPlanet neuve — la station crée sa `swarm.key` et vous la montre pour la donner aux autres stations — ou rejoindre avec la clé reçue) · `plane-setup add --name zen1 --found` · `plane-setup add --name zen2 --swarm-key reçue.key` · `plane-setup remove zen2`.
- **Sur le web** : Station › *Plans (UPlanet)* › *Gérer les plans* (Capitaine) : liste, état (servi / au prochain démarrage), ajout (fonder ou coller la `swarm.key` reçue) et retrait.

![Station › Plans : une station qui sert ORIGIN et une UPlanet privée (zen1) dans un seul processus, et le formulaire pour en ajouter une.](img/ui-plans.png)

Dans les deux cas : ports choisis libres, clé écrite en `0600` sous `~/.zen/game/<plan>/swarm.key` sans jamais écraser une clé existante, **ORIGIN reste dans sa base et son dossier actuels (rien n'est déplacé)**, et **retirer un plan ne supprime aucune donnée**. Les règles du démarrage sont vérifiées dès l'ajout : Kubo n'appartient qu'à ORIGIN, pas deux plans avec la même clé, la même base ou le même port.

**Servir** : `astroportd planes --check` valide `~/.zen/astroportd/planes.json` et prépare les dossiers (liens vers les fichiers du NODE) sans rien lancer ; `astroportd planes` démarre tous les plans, à la place de `serve` : chaque plan a son propre `serve` (donc son propre nœud P2P, avec sa `swarm.key`) dans le même processus. Ajouter un plan demande de relancer cette commande.

- **Un seul nom, plusieurs plans** : `astroportd planes --front 127.0.0.1:18000` écoute sur un port commun et route **par nom d'hôte** : `zen1.votre-domaine` ouvre le plan `zen1`, le nom nu ouvre ORIGIN (en local : `zen1.localhost`). HTTP en clair : à placer derrière le terminateur TLS (Nginx Proxy Manager).
- **Choisir le plan d'une commande** : `--plane zen1` (par exemple `astroportd --plane zen1 multipass create alice@exemple.org`, `token`, `doctor`, `pool-status`) remplit la base, les blobs, le dossier de comptes et la clé du plan choisi, sans remplacer ce que vous donnez vous-même. Un plan inconnu est refusé.
- **Dans l'interface** : quand la station route par nom d'hôte, le Capitaine a un **sélecteur de plan** dans l'en-tête ; l'écran *Station › Plans* dit si la station sert plusieurs plans dans un seul processus.

<a id="etape-8bis-j"></a>

## Étape 8 bis-j — Messages privés et une personne dans plusieurs UPlanet

**Messages** (`#/chat`) : conversations 1 à 1 chiffrées (NIP-17 : message kind 14, scellé kind 13, enveloppe kind 1059). La station ouvre toutes les 3 s les enveloppes nouvelles de chaque MULTIPASS hébergé (repère `seq` dans `kv`, car NIP-59 brouille les dates) et range les messages déchiffrés dans `chat_messages` (état local). API : `GET /api/v1/chat`, `GET /api/v1/chat/messages?peer=`, `POST /api/v1/chat/send`, `POST /api/v1/chat/read`.

**Une personne, plusieurs UPlanet.** Le MULTIPASS d'une personne a une clé différente dans chaque UPlanet : on ne peut pas la déduire de son email. La station la retrouve de deux façons :
1. **Annuaire de la station** : les plans voisins (`planes.json`) sont lus en lecture seule ; l'email d'un MULTIPASS hébergé donne sa clé dans chaque plan.
2. **Lien prouvé** : kind 31903 `d = plane-link`, signé par la clé A, tag `p` = clé B, contenu = événement `d = plane-link-proof` signé par B qui nomme A. Un lien sans les deux signatures est ignoré.

`GET /api/v1/me/links` liste vos clés ; `POST /api/v1/me/links` annonce vos comptes des autres plans (public, sur demande, à faire dans chaque UPlanet). La fiche d'un contact (`GET /api/v1/book/card`) rend `keys` ; le chat regroupe les clés liées en une conversation et écrit dans l'UPlanet commune.

<a id="etape-8c"></a>

## Étape 8 ter — Options du Capitaine : moteur FaceID et règles du relai

L'écran **Station → Options de la station** change sans redémarrer :

- **Tunnels et accès des autres Capitaines** : pour chaque service offert (SSH, ComfyUI, Ollama, plus ceux de `--p2p-expose`), *qui peut l'ouvrir* — fermé, la liste seulement, les Capitaines amis, ou tout le réseau — et la liste nominative avec une durée (« laisser un ami m'aider »). Un service qui ne répond pas sur la machine n'a pas de carte : une ligne *Non détectés* le nomme, jusqu'à ce qu'il démarre. Un réglage s'applique tout de suite, sessions ouvertes comprises.
- **Assistance entre Capitaines amis** : demander de l'aide pour un service, pour une durée ; rien ne s'ouvre avant qu'un ami accepte.
- **File de traitement** : les tâches en attente, en cours ou abandonnées, avec relance.

- **Reconnaissance des visages** : *ONNX local* (modèles `det_10g.onnx` + `w600k_r50.onnx`), *Brain GPU distant* (adresse `http://hôte:port/chemin`, testée avant d'être appliquée) ou *Arrêté* (les photos attendent, aucune tentative n'est consommée). L'écran montre ce qui manque (fonction `onnx` compilée, modèles, `libonnxruntime`). Le choix est conservé ; une option `--face-engine` / `--face-models` sur la ligne de commande l'impose et fige le réglage.
![Options de la station : assistance entre Capitaines, tunnels et accès (ComfyUI non détecté, SSH réglé sur « Capitaines amis + liste »).](img/ui-options.png)

- **Relai : lecture, écriture, projection par kind.** Lecture : *Tous*, *Auteur et destinataires*, *Auteur et ses Proches*, *Cercle social*, *Personne*. Écriture : *Cercle social*, *Tous (débit limité)*, *Personne*. Projection : l'événement alimente-t-il les tables dérivées (comptes, ledger…) ; il reste stocké et servi. Couches, de la plus faible à la plus forte : défauts du relai, `station.conf`, réglages du Capitaine. Dans `station.conf` :

```ini
kind.30078 = write=open project=off
kind.7     = read=circle write=circle project=on
```

Une règle invalide est refusée avant d'être enregistrée (ou signalée au démarrage, la politique par défaut s'applique). Le kind 22242 (authentification) n'a pas de règle ; le kind 1059 garde sa règle d'amorçage.

**NIP-65.** À sa création, un MULTIPASS publie sa liste de relais (kind 10002) avec le relai de la station (`--domain`). Pour écrire à un ami dont la station est inconnue, la station essaie ses relais de **lecture** annoncés avant les `--relay` fixes.

## Étape 8 quater — Inviter quelqu'un à recevoir des photos

Pour envoyer des photos à une personne qui n'a pas encore de MULTIPASS : écran **Contacts → Inviter** (sur une fiche, ou sur une personne reconnue sans fiche : la fiche est alors créée d'abord). La station retient la liste des photos où cette personne apparaît (200 au plus) et rend un **lien** `https://<station>/i/<jeton>` avec son **QR code**, à envoyer par le moyen de votre choix (le bouton *Partager* ouvre le menu de partage du téléphone).

Ce que voit la personne, sans compte : qui l'invite, combien de fichiers, de petites vignettes **floutées** (16 pixels de côté : on reconnaît une scène, pas un visage). Elle saisit **son** adresse email ; la station crée alors son MULTIPASS, lui remet ses identifiants une seule fois, la rend **Proche réciproque** de l'expéditeur (chacun suit l'autre, listes de contacts signées par chacune des deux clés et publiées), range les photos dans `/Partages/De_<expéditeur>/` (même blob chiffré, même clé : aucune copie) et **relie la fiche de contact** à son nouveau MULTIPASS. Les autres photos où elle apparaît lui sont partagées aussitôt, comme pour tout Proche relié.

Garde-fous :

- **Usage unique.** La réclamation est prise de façon atomique avant toute création ; si la création échoue (adresse invalide, déjà inscrite), le lien reste valable.
- **Une adresse qui a déjà un MULTIPASS ici est refusée** : lier deux comptes sur la seule foi d'une adresse saisie permettrait de s'imposer comme proche de n'importe qui. La personne se connecte, puis demande à être ajoutée comme proche.
- Le lien expire (14 jours par défaut, 90 au plus), se retire d'un clic (*Retirer*), et chaque compte garde au plus 50 invitations en attente. La page est servie avec `X-Robots-Tag: noindex` et sans en-tête `Referer`.
- Le débit d'inscription par adresse IP s'applique aussi aux réclamations (`--register-per-hour`) ; `--no-register` ferme les invitations avec l'inscription.
- Les clés des fichiers ne quittent pas la station avant la réclamation. Quand le lien est créé pour **une personne** (« toutes les photos de Léa »), la liste est **recalculée au moment où le lien est ouvert et accepté** : les photos traitées entre-temps sont incluses, celles supprimées depuis ne le sont plus. Un lien créé pour des chemins précis garde ces chemins.

**Accepter avec son compte (même station).** Si le visiteur est déjà connecté à la station, la page propose d'abord « Bonjour Léa, ajouter ces N fichiers à votre compte ? » : un clic (`POST /api/v1/invite/<jeton>/accept`) accepte, **sans formulaire et sans nouveau compte**. L'identité est prouvée par la **session**, jamais par une adresse saisie. Le double suivi, le rangement des photos, la liaison de la fiche et la prévenance de l'expéditeur sont les mêmes qu'à la création d'un compte ; l'expéditeur ne peut pas accepter son propre lien. « Utiliser un autre compte » rouvre le formulaire d'inscription. La session de l'application est conservée dans le `localStorage` du navigateur (elle survit à la fermeture de la PWA ; *Déconnexion* l'efface) : c'est ce qui permet à la page d'invitation, ouverte dans un autre onglet, de la reconnaître.

**Envoi du lien.** La station n'envoie **jamais** de courrier : le lien se transmet par la messagerie de l'expéditeur. Après *Inviter*, les boutons **Email** (`mailto:` prérempli avec l'adresse de la fiche), **SMS**, **WhatsApp** et **Partager** (menu du téléphone) ouvrent son application avec le message prêt.

**Erreurs à code stable.** Toute erreur d'invitation porte `{ "error": "…", "code": "invite_used" | "invite_expired" | "invite_revoked" | "email_taken" | "rate_limited" | "registration_closed" | … }` ; la page traduit d'après le code (français, anglais, espagnol), jamais d'après le texte.

**Limites actuelles.** « Accepter avec mon compte » ne fonctionne que pour un compte hébergé sur **cette** station : une personne inscrite sur une autre station n'est pas encore redirigée vers la sienne (acceptation entre stations à faire). Une session ouverte par une extension Nostr (NIP-07) n'est pas reconnue par la page : seule la session par identifiant et jeton ou PASS l'est. Les codes d'erreur ne couvrent pas encore tous les écrans de l'application, seulement les invitations et le PASS. Ce parcours n'a pas été essayé sur un vrai téléphone.

### Compte sur une autre station

Sur la page du lien, **« Mon compte est sur une autre station »** : la personne saisit l'adresse de sa station et y est envoyée (`/claim`). Le jeton voyage dans le **fragment** de l'adresse (`#origin=…&token=…`), que le navigateur n'envoie à aucun serveur ; la page le retire de l'adresse dès qu'elle l'a lu. Elle demande le PASS s'il n'y a pas de session dans ce navigateur (la session obtenue reste en mémoire), puis nomme la station d'origine et le compte qui recevra, et attend un clic.

Ensuite c'est **la station du destinataire** qui travaille, sans que le navigateur parle à l'autre station (pas de CORS) :

1. `GET /api/v1/invite/<jeton>` signé NIP-98 : l'aperçu public ne contient jamais la clé de l'expéditeur, la réponse signée la donne.
2. Le destinataire suit l'expéditeur (liste de contacts signée avec sa clé, qui ne quitte pas sa station).
3. `POST /api/v1/invite/<jeton>/accept` signé NIP-98 (le corps est lié à la signature) : la station d'origine vérifie, marque le lien utilisé par cette clé, suit le destinataire, relie la fiche de contact et rend la liste de contacts de l'expéditeur, que la station du destinataire range **avant** toute photo (un fichier n'est accepté que d'un proche réciproque ; une enveloppe arrivée trop tôt est écartée).
4. `POST /api/v1/invite/<jeton>/deliver` : les photos partent par le maillage (NIP-17 emballé en NIP-59, puis rapatriement du blob chiffré par `/astro/blob/1.0.0`) vers `/Partages/De_<expéditeur>/`.

Limites : le lien reste à usage unique (`invite_gone` si la station d'origine répond 404/410) ; l'envoi est réservé à celui qui a accepté, pendant une heure et 3 fois au plus ; l'adresse saisie doit être celle que la station d'origine annonce publiquement (la signature est liée à l'URL). Sans station d'accueil connue pour le destinataire, c'est son `home_station` (envoyé dans le corps signé) qui sert de route.

## Étape 8 quinquies — Sans email, connexion par PASS, signature déléguée

**L'email tient lieu de `npub`.** Chaque MULTIPASS en a un, unique : un index de la base refuse deux MULTIPASS hébergés de même adresse (casse comprise) ; la ZenCard et le Capitaine d'un même membre, eux, peuvent porter la même adresse que son MULTIPASS. L'unicité sur **toute** la constellation ne peut pas être garantie par une base centrale : c'est la station qui crée le compte qui la contrôle.

**Je préfère ne pas donner mon email.** Sur `/register` comme sur la page d'une invitation, une case crée un compte **invité**. La station génère `<prénom ou pseudo>-<6 caractères>@<domaine>` (par exemple `lea-x7k2p9@copylaradio.com`), en vérifiant qu'elle est libre. Le domaine est celui de l'**API OVH** : `OVH_ZONE` de la configuration coopérative, sinon le `--domain` de la station, sinon `uplanet.local`. La station **ne crée aucune boîte aux lettres** pour cette adresse : c'est un identifiant UPlanet (Nostr et IPFS). Comme le domaine est celui de la zone OVH, une messagerie SMTP/IMAP pourra la servir plus tard sans rien changer au compte. Un compte invité voit, dans **Mon compte**, un bandeau qui rappelle que la carte (SALT, PEPPER) et le PASS sont ses seuls moyens de récupération. Changer plus tard d'adresse n'est **pas** encore possible.

**Connexion par PASS.** Un nouveau MULTIPASS reçoit un code **PASS à 6 chiffres** (les comptes existants gardent le leur, à 4 chiffres ; les deux sont acceptés). Dans la fenêtre **Connexion**, saisissez votre email, puis votre PASS à la place du long jeton : la station le vérifie (`POST /api/v1/account/unlock`) et rend le jeton de session, que l'application conserve. Le PASS n'a que 10⁶ valeurs : sa défense est la limite d'essais **de la station**, jamais un secret chiffré avec lui.

| Échecs consécutifs | Réponse |
|---|---|
| 1 à 4 | « code PASS incorrect : N essai(s) restant(s) » |
| 5ᵉ | compte **verrouillé 15 min** ; le Capitaine reçoit une alerte, le titulaire aussi dans son flux |
| chaque échec suivant, délai écoulé | le verrou **double** (30 min, 1 h, 2 h… 24 h au plus) |

Un succès remet tout à zéro, et une erreur isolée s'oublie après 24 h. Le PASS n'est **jamais détruit** par des échecs : n'importe qui connaissant une adresse pourrait sinon l'invalider à distance. Le Capitaine lève un verrou dans **Station → Comptes → Déverrouiller le PASS**. Une même adresse IP est en outre limitée à 30 tentatives par quart d'heure.

**`window.nostr` sans extension.** Après la connexion, l'application fournit `window.nostr` (NIP-07) aux scripts de **sa propre origine** : `getPublicKey()` rend la clé du compte, `signEvent()` envoie l'événement non signé à `POST /api/v1/account/sign` et reçoit l'événement signé. **La clé secrète ne quitte jamais la station** : un téléphone volé se révoque en changeant le jeton (Mon compte), alors qu'une clé Nostr ne se change pas. La station refuse de signer les kinds 30800 (configuration coopérative) et 30852 (grand livre), les enveloppes NIP-59 et les réactions `+N` (elles déclenchent un paiement) ; la signature est limitée à 120 par minute et par compte, et tracée dans le journal d'audit (kind seulement). Une extension de navigateur déjà installée n'est jamais remplacée. Les clients d'une **autre** origine (Coracle…) ne voient pas ce `window.nostr` : un signataire distant NIP-46 le permettra, il n'est pas encore fait.

<a id="etape-9"></a>

## Étape 9 — Relier sa station aux autres Capitaines

Grâce à Y-Level, la clé publique SSH d'une station **se déduit de son identité IPFS** (`12D3Koo…`) : c'est la même clé. Quand deux Capitaines se suivent sur Nostr, ils peuvent donc s'ouvrir un accès SSH entre machines de confiance : de l'**individu**, à l'**Astroport**, au **territoire**.

Qui fait quoi ? Le **suivi automatique** entre Capitaines (lecture des rapports économiques kind 30850, suivi mutuel) est fait par **Astroport.ONE** (`DRAGON_p2p_ssh.sh`). **astroportd**, lui, fournit le **tunnel** P2P entre les stations, sans ouvrir de port sur la box ; et il fait suivre le Capitaine par chaque MULTIPASS qu'il crée.

![Individus, Astroports et territoire ; les Capitaines qui se suivent.](img/individu-astroport-territoire.svg)

### 🖥️ Terminal (CLI)

Quand le maillage P2P est actif, la station **offre aussi son relai Nostr** aux autres stations (service `strfry`, tunnel `/x/strfry-<PeerID>`) : c'est par là que la synchronisation N¹/N² et l'écran « Réplication » joignent les relais des autres stations, sans port public.

Sur la station qui **offre** son accès SSH :

```bash
astroportd serve ... --p2p-listen /ip4/0.0.0.0/tcp/4101 [--swarm-key ./swarm.key]   # le SSH est offert par défaut, mais fermé : le Capitaine l'ouvre dans Options › Tunnels
```

Sur la station qui **rejoint** (le PeerID est celui de l'hôte, visible avec `astroportd ylevel`) :

```bash
astroportd serve ... --p2p-forward 127.0.0.1:2222=12D3Koo…/ssh
ssh -p 2222 utilisateur@127.0.0.1
```

> ⚠️ **Attention.** Le tunnel transporte les octets : c'est toujours `sshd` qui décide. Ajoutez volontairement la clé publique de l'autre Capitaine dans `~/.ssh/authorized_keys`, et en réseau **ORIGIN (ouvert)** n'exposez SSH que si `PasswordAuthentication no`.

Pour choisir le meilleur nœud du réseau pour un service (GPU, Ollama…) :

```bash
astroportd route ollama --mode closest --lat 45.7 --lon 4.8
```

### 🌐 Navigateur (Web)

Le Capitaine voit dans l'écran **Station** (le cockpit, étape 7 bis) l'état de la constellation, l'économie de la station et les comptes hébergés ; les cartes **Ollama** et **ComfyUI** de *Options › Tunnels* n'apparaissent que si le service répond sur la machine.

Le territoire se lit sur la carte UPlanet : chaque dalle de 0,01° (environ 1 km²) a sa propre identité, et les stations voisines forment le territoire.

<a id="etape-9b"></a>

## Étape 9 bis — Une adresse pour chaque station et chaque MULTIPASS (DNSLink)

La coopérative peut donner à **chaque station** et à **chaque MULTIPASS** une adresse lisible dans sa zone DNS : `_dnslink.<nom>.<zone>` pointe vers l'espace IPNS (`/ipns/k51…`), et n'importe quelle passerelle IPFS résout alors `<nom>.<zone>`.

### 🖥️ Terminal (CLI)

La station s'en charge seule dès que le Capitaine a renseigné les clés OVH de la coopérative (`OVH_APP_KEY`, `OVH_APP_SECRET`, `OVH_CONSUMER_KEY`, `OVH_ZONE`, `astroport.one` par défaut). Elle écrit les **mêmes** enregistrements que l'outil `Astroport.ONE/admin/system/ovh.me.sh`, que l'on peut garder pour inspecter la zone :

```bash
~/.zen/Astroport.ONE/admin/system/ovh.me.sh list
```

### 🌐 Navigateur (Web)

1. Dans *Station › Configuration coopérative*, renseignez les clés OVH (elles sont chiffrées, jamais ré-affichées).
2. Dans le cockpit, la carte **Adresses de la coopérative** montre l'adresse de la station, combien d'adresses sont à jour, en attente ou en échec ; **Enregistrer maintenant** lance un passage tout de suite.
3. Un MULTIPASS s'appelle `<partie locale à l'envers>.<domaine>` (`geg-la@exemple.org` devient `la.geg.exemple.org`) ; une station `<nom de machine>-<6 derniers caractères du PeerID>`.

> **À savoir.** Un passage a lieu 90 secondes après le démarrage puis toutes les 15 minutes, et n'appelle OVH que pour ce qui a changé. La station ne supprime jamais un enregistrement. Un échec est affiché et le Capitaine en est averti une fois.

<a id="etape-10"></a>

## Étape 10 — En cas de problème

Les réponses aux questions les plus fréquentes. Dans tous les cas, `astroportd doctor` est le premier réflexe.

### 🖥️ Terminal (CLI)

- **« L'horloge est antérieure à 2026 »** : réglez l'heure (`sudo timedatectl set-ntp true`). Sur Raspberry Pi sans pile, c'est normal au premier démarrage.
- **Le port 80 ou 443 est refusé** : un utilisateur ordinaire ne peut pas ouvrir les ports sous 1024. Autorisez-le : `sudo setcap 'cap_net_bind_service=+ep' ~/.local/bin/astroportd`.
- **`doctor` signale un secret lisible par d'autres** : `chmod 600 ~/.zen/game/secret.*` (et `chmod 600` sur la base).
- **`ylevel --verify` échoue** : la clé SSH a été remplacée après Y-Level (ou une autre machine l'a écrasée). Relancez `astroportd init`, ou restaurez `~/.ssh/id_ed25519` depuis vos sauvegardes.
- **La reconnaissance de visages ne démarre pas** : vérifiez `--face-engine` ou `--face-models` (voir étape 8). Avec `--face-models`, le binaire doit avoir été compilé avec `--features onnx`, `ORT_DYLIB_PATH` doit désigner `libonnxruntime.so` et le dossier doit contenir `det_10g.onnx` et `w600k_r50.onnx`.
- **Les embeddings locaux ne démarrent pas** : lancez `astroportd embed-check`, il dit ce qui manque (fonction `onnx` compilée, `bge-m3-int8.onnx` et `bge-m3-tokenizer.json` dans `~/.zen/astroportd/models`, `libonnxruntime`). Sans option, des fichiers inutilisables font dire pourquoi à la station, qui se rabat alors sur Ollama ; avec `--embed-models`, un dossier incomplet ou une bibliothèque introuvable est une **erreur** et la station ne démarre pas. Sur une petite machine, le démarrage charge ≈ 560 Mo de modèle : si la mémoire manque, relancez l'installeur avec `--check-hardware` et choisissez le profil *léger* ou `--no-embed`.
- **L'installeur recommande « léger » alors que je veux tout en local** : il a vu un manque de mémoire, de place ou un disque très lent (chaque raison est affichée). Forcez avec `--profile standard` ou `--profile confort` si vous assumez la lenteur, ou changez de disque (un SSD USB ou NVMe vaut mieux qu'une carte SD).
- **Un virement Ğ1 n'apparaît pas en Ẑen** : l'écoute des virements compte un virement comme Ẑen seulement s'il vient de cette UPlanet (contrôle « primal »). Les nœuds Duniter et les indexeurs squid sont **découverts automatiquement** au démarrage (amorces, pairs, fichier réseau officiel, annonces on-chain ; sondés puis triés par latence ; cache d'une heure) : `--duniter-rpc` et `--squid-url` ne servent qu'à les imposer, `--no-duniter` à ne pas joindre la Ğ1. Si aucun indexeur ne répond, l'origine reste « non vérifiée » et le titulaire est prévenu sans que le montant soit compté.
- **Photos : GPS et date de prise de vue.** À la réception d'une photo, la station lit son EXIF (position GPS, date de prise de vue) avant que la miniature n'en efface les métadonnées, et les note dans l'uCloud (jamais d'écrasement d'une valeur déjà saisie ; une position « 0, 0 » ou hors du globe est ignorée). L'**orientation** EXIF est appliquée partout : la détection des visages, leurs vignettes et la visionneuse partagent le même repère. Les photos reçues **avant** cette version ne sont pas rattrapées.
- **La station ne voit pas les autres** : en ẐEN, toutes les stations doivent avoir **exactement la même** `swarm.key`.
- **Passer d'ORIGIN à ẐEN** : ajoutez `--swarm-key ./swarm.key` au démarrage et redémarrez ; la station ne verra plus que les stations de la même clé.

Une question, un bug ? Ouvrez un ticket : [https://github.com/papiche/astroportd/issues](https://github.com/papiche/astroportd/issues). Pour la coopérative : support@qo-op.com.

### 🌐 Navigateur (Web)

- **Impossible de se connecter** : le compte doit être hébergé par la station (dossier `~/.zen/game/nostr/<email>/`) ; le jeton s'obtient avec `astroportd token`. Pas de compte ? `astroportd multipass create <email>` ou la page `/register`.
- **La page ne se charge pas** : essayez `/health` ; si elle répond, videz le cache du navigateur.
- **Le lien magique ne s'ouvre pas** : l'adresse doit être copiée en entier, avec le `#` et ce qui le suit.
- **Le guide en ligne** : [Astroport.ONE](https://github.com/papiche/Astroport.ONE) pour le suivi automatique des Capitaines, la ZenCard et les QR codes.

---

Pour comprendre en profondeur ORIGIN, ẐEN et Y-Level : [CONSTELLATIONS.md](CONSTELLATIONS.md). Licence AGPL-3.0.

## Sur un smartphone

Deux façons de s'en servir, de la plus simple à la plus aventureuse.

**1. Le téléphone comme client (recommandé, rien à installer).** Faites tourner astroportd sur un PC, une box ou un Raspberry Pi, puis utilisez le téléphone pour s'y connecter :
- l'**interface web** est une application installable (PWA) : ouvrez l'adresse de votre station dans le navigateur, puis « Ajouter à l'écran d'accueil » ;
- le **disque réseau** : tout gestionnaire de fichiers qui sait parler WebDAV (Material Files, Solid Explorer, Cx Explorer sous Android ; Fichiers sous iOS) se connecte à `https://votre-station/dav` avec votre adresse e-mail et le jeton (`astroportd token <email>`). Vos photos s'y déposent et la station les chiffre.

**2. astroportd directement sur le téléphone (Android seulement, expérimental).** Le binaire statique ARM 64 bits (`aarch64-unknown-linux-musl`) tourne sous [Termux](https://f-droid.org/packages/com.termux/) (installez-le depuis F-Droid, pas depuis le Play Store). L'installeur le choisit tout seul :

```bash
pkg install curl openssh            # ssh-keygen sert au rituel Y-Level
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash   # bash, pas sh : l'installeur en a besoin
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs \
  --game-dir ~/.zen/game --http-listen 127.0.0.1:18080
```

Puis ouvrez `http://127.0.0.1:18080` dans le navigateur du téléphone (« localhost » est un contexte sécurisé : l'application s'installe) et, dans votre gestionnaire de fichiers, ajoutez un serveur WebDAV `http://127.0.0.1:18080/dav`. Le téléphone devient alors une station : ses photos sont chiffrées sur place, et vos proches les voient par le maillage.

Limites à connaître (non vérifiées sur un appareil réel) :
- **DNS** : un binaire statique musl lit `/etc/resolv.conf`, que Android n'a pas. Si la station n'arrive pas à joindre un nom d'hôte (nœuds Duniter, pairs), lancez-la dans `termux-chroot` (paquet `proot`), qui fournit ce fichier.
- **Veille** : Android coupe les applications en arrière-plan. Lancez `termux-wake-lock` et retirez Termux de l'optimisation de batterie, sans quoi la station s'endort.
- **Reconnaissance de visages et embeddings locaux** : ils demandent `libonnxruntime` (glibc), indisponible pour un binaire statique. Pointez `--face-engine` vers le service d'une autre station, et `--ollama` vers une machine qui héberge Ollama pour les vecteurs de la mémoire.
- **Réseau** : un téléphone est derrière un NAT d'opérateur ; pour être joignable par ses pairs, utilisez `--p2p-use-relay` avec une station qui sert de relais. iOS n'est pas supporté.
