# Guide pas à pas : démarrer sa station astroportd

> Version texte du [guide illustré](index.html). Préversion **0.2.0-beta**. Chaque étape propose le **terminal (CLI)** et, quand c'est possible, le **navigateur (Web)**.

**Sommaire** : [0. Avant de commencer](#etape-0) · [1. Installer astroportd](#etape-1) · [2. Activer Y-Level ](#etape-2) · [3. Choisir sa constellation ](#etape-3) · [4. Démarrer la station](#etape-4) · [5. Créer mon MULTIPASS](#etape-5) · [6. Vérifier que tout va bien](#etape-6) · [7. Entrer dans l'interface web](#etape-7) · [8. FaceCloud ](#etape-8) · [9. Relier sa station aux autres Capitaines](#etape-9) · [10. En cas de problème](#etape-10)

<a id="etape-0"></a>

## Étape 0 — Avant de commencer

**astroportd** est un programme unique qui fait tourner votre « Astroport » : un cloud personnel chiffré, vos photos avec reconnaissance des visages, le partage entre amis et un relais Nostr. Ce guide vous mène de zéro à une station qui fonctionne.

Pas à pas, chaque étape se fait au **terminal** (colonne de gauche) ou, quand c'est possible, au **navigateur** (colonne de droite).

### 🖥️ Terminal (CLI)

Il vous faut :

- un ordinateur **Linux** (PC, serveur) ou un **Raspberry Pi** (de préférence 64 bits : Pi 4, Pi 5, Pi Zero 2 W) ;
- une connexion Internet et environ 15 minutes ;
- une **horloge à l'heure** : un Raspberry Pi sans pile démarre en 1970, et astroportd refuse alors de calculer ses échéances ;
- facultatif, pour une station ouverte sur Internet : un nom de domaine et les ports 80 et 443 ouverts ;
- facultatif : un service d'inférence faciale (pour FaceCloud) et `ffmpeg` (pour les films et séries).

Deux commandes pour vérifier votre machine :

```bash
uname -m   # x86_64, aarch64 ou armv7l
date       # l'année doit être 2026 ou plus
```

### 🌐 Navigateur (Web)

Il vous faut un navigateur récent (Firefox, Chrome, Safari…). L'interface de la station est une application web : elle s'installe aussi comme une application sur téléphone.

> **À savoir.** astroportd est en **préversion 0.2.0-beta** : il fonctionne et il est testé, mais il est jeune. Gardez une sauvegarde de vos clés.

Vous aurez aussi besoin d'un compte **MULTIPASS** (votre identité Nostr) : astroportd le crée pour vous à l'étape 5.

<a id="etape-1"></a>

## Étape 1 — Installer astroportd

Le plus simple : l'installeur. Il choisit le bon fichier pour votre machine et **vérifie son empreinte SHA-256** avant de l'installer. S'il y a le moindre doute, il s'arrête.

### 🖥️ Terminal (CLI)

```bash
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
```

Options utiles : `--version v0.2.0-beta`, `--static` (binaire statique, x86_64), `--prefix DIR`, `--no-init`, `--yes`. Ensuite :

```bash
astroportd --version
```

Si la commande est introuvable, ajoutez `~/.local/bin` à votre PATH : `export PATH="$HOME/.local/bin:$PATH"`.

### 🌐 Navigateur (Web)

Ouvrez la page des versions : [https://github.com/papiche/astroportd/releases](https://github.com/papiche/astroportd/releases). Téléchargez l'archive qui correspond à votre machine :

- `x86_64-unknown-linux-gnu` : PC et serveurs ;
- `aarch64-unknown-linux-gnu` : Raspberry Pi 4/5 et Zero 2 W en 64 bits ;
- `armv7-unknown-linux-gnueabihf` : Raspberry Pi 2/3, Pi OS 32 bits ;
- `x86_64-unknown-linux-musl` : binaire statique, sans dépendance.

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

> **À savoir.** Une station appartient à **une seule constellation** à la fois (une `swarm.key`, ou aucune). Raccorder une même station à deux constellations en parallèle (ORIGIN et ẐEN) est **prévu**, pas encore disponible : deux réseaux P2P parallèles, la même identité Y-Level, chaque MULTIPASS rattaché à une constellation.

Il n'y a pas d'assistant web pour choisir sa constellation : ce choix engage une clé secrète, il se fait au terminal.

<a id="etape-4"></a>

## Étape 4 — Démarrer la station

`init` a préparé le dossier de la station (`~/.zen/astroportd` : la base `uplanet.db`, les fichiers chiffrés `blobs`, les sauvegardes `snapshots`) et vous a affiché la commande suivante. Voici les deux façons de démarrer.

### 🖥️ Terminal (CLI)

**Essai local** (HTTP simple, sur votre machine seulement) :

```bash
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
# → http://127.0.0.1:8080
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

Rien à cliquer pour démarrer : c'est le terminal qui lance le programme. Quand il tourne, ouvrez [http://127.0.0.1:8080](http://127.0.0.1:8080) (essai local) ou `https://votre-domaine/`.

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

> **À savoir.** **Limites.** La création reprend l'essentiel de `make_NOSTRCARD.sh` d'Astroport.ONE, mais pas : le découpage SSSS 2-sur-3 de DISCO chiffré vers le Capitaine, les QR codes, l'import de la clé IPNS dans Kubo, l'envoi du zine par email, la ZenCard et la primo-transaction. Retrouver un compte repose sur le SALT/PEPPER remis une fois au titulaire (et sur le `nsec`).

### 🌐 Navigateur (Web)

Les **membres** s'inscrivent depuis le navigateur (le Capitaine se crée au terminal, ci-contre). La station doit tourner (étape 4).

1. Ouvrez `https://votre-domaine/register` (ou, dans la fenêtre **Connexion**, le lien **« Pas de compte ? Créer mon MULTIPASS »**).
2. Saisissez votre **email** et choisissez la **langue**.
3. Une **carte d'identité** s'affiche : SALT, PEPPER, nsec, npub, Ğ1, IPNS, code PASS et jeton. **Elle ne s'affiche qu'une fois.**
4. Cliquez **« Télécharger ma carte (.txt) »** et gardez le fichier en lieu sûr.
5. Cliquez **« Ouvrir mon espace »** : vous êtes connecté directement.

L'inscription web est **ouverte par défaut**, avec des garde-fous réglables au démarrage : `--max-accounts N` (500 par défaut, `0` = illimité), `--register-per-hour N` (5 par adresse IP, `0` = illimité) et `--no-register` pour la **fermer**. Un email déjà inscrit est refusé (erreur 409), une adresse invalide aussi (400).

> **À savoir.** Sur une station publique, pensez à fermer l'inscription (`--no-register`) quand vos membres sont inscrits.

<a id="etape-6"></a>

## Étape 6 — Vérifier que tout va bien

Trois vérifications, de la plus complète à la plus rapide. Aucune ne modifie rien.

### 🖥️ Terminal (CLI)

```bash
astroportd doctor --db ~/.zen/astroportd/uplanet.db --game-dir ~/.zen/game
astroportd ylevel --verify
curl http://127.0.0.1:8080/health
```

`doctor` contrôle l'identité, les permissions `0600` des secrets, la base, les certificats HTTPS et l'horloge (il affiche le prochain 20h12 solaire). Code de sortie `1` si une vérification échoue.

Pour voir le relai Nostr de la station :

```bash
curl -H 'Accept: application/nostr+json' http://127.0.0.1:8080/
```

### 🌐 Navigateur (Web)

Ouvrez `http://127.0.0.1:8080/health` : une page courte indique l'état de chaque partie de la station (OK ou en panne).

Si l'application se charge sur `http://127.0.0.1:8080/`, c'est que le serveur web fonctionne.

<a id="etape-7"></a>

## Étape 7 — Entrer dans l'interface web

L'application web a quatre écrans : **Tableau de bord**, **Station** (réservé au Capitaine), **uCloud** (vos fichiers et vos photos) et **Visages & BRO**.

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
3. Le **Tableau de bord** montre votre solde en Ẑen, votre autonomie, votre empreinte sur les communs, vos fichiers chiffrés.
4. Deux cartes distinctes : **Ẑen Opex** (ce que vous dépensez : loyer, baux) et **Ẑen Capex** (ce que vous possédez : parts de matériel, avec leur amortissement).
5. La carte **Copie privée** vous demande d'attester, une fois, que vos films et séries sont des copies pour votre usage privé.

Sur un téléphone, installez la page comme application (menu du navigateur : *Ajouter à l'écran d'accueil*).

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

`--relay` indique où publier les partages vers les amis qui sont sur **une autre station** (message chiffré, enveloppé NIP-59). Sans `--face-engine`, uCloud fonctionne mais sans reconnaissance.

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

<a id="etape-9"></a>

## Étape 9 — Relier sa station aux autres Capitaines

Grâce à Y-Level, la clé publique SSH d'une station **se déduit de son identité IPFS** (`12D3Koo…`) : c'est la même clé. Quand deux Capitaines se suivent sur Nostr, ils peuvent donc s'ouvrir un accès SSH entre machines de confiance : de l'**individu**, à l'**Astroport**, au **territoire**.

Qui fait quoi ? Le **suivi automatique** entre Capitaines (lecture des rapports économiques kind 30850, suivi mutuel) est fait par **Astroport.ONE** (`DRAGON_p2p_ssh.sh`). **astroportd**, lui, fournit le **tunnel** P2P entre les stations, sans ouvrir de port sur la box ; et il fait suivre le Capitaine par chaque MULTIPASS qu'il crée.

![Individus, Astroports et territoire ; les Capitaines qui se suivent.](img/individu-astroport-territoire.svg)

### 🖥️ Terminal (CLI)

Sur la station qui **offre** son accès SSH :

```bash
astroportd serve ... --p2p-listen /ip4/0.0.0.0/tcp/4101 --p2p-expose ssh=127.0.0.1:22 [--swarm-key ./swarm.key]
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

Le Capitaine voit dans l'écran **Station** l'état de la constellation, l'économie de la station et la liste des comptes hébergés.

Le territoire se lit sur la carte UPlanet : chaque dalle de 0,01° (environ 1 km²) a sa propre identité, et les stations voisines forment le territoire.

<a id="etape-10"></a>

## Étape 10 — En cas de problème

Les réponses aux questions les plus fréquentes. Dans tous les cas, `astroportd doctor` est le premier réflexe.

### 🖥️ Terminal (CLI)

- **« L'horloge est antérieure à 2026 »** : réglez l'heure (`sudo timedatectl set-ntp true`). Sur Raspberry Pi sans pile, c'est normal au premier démarrage.
- **Le port 80 ou 443 est refusé** : un utilisateur ordinaire ne peut pas ouvrir les ports sous 1024. Autorisez-le : `sudo setcap 'cap_net_bind_service=+ep' ~/.local/bin/astroportd`.
- **`doctor` signale un secret lisible par d'autres** : `chmod 600 ~/.zen/game/secret.*` (et `chmod 600` sur la base).
- **`ylevel --verify` échoue** : la clé SSH a été remplacée après Y-Level (ou une autre machine l'a écrasée). Relancez `astroportd init`, ou restaurez `~/.ssh/id_ed25519` depuis vos sauvegardes.
- **La reconnaissance de visages ne démarre pas** : vérifiez `--face-engine` (voir étape 8).
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
