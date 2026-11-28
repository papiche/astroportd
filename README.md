# astroportd

[![Licence AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue)](LICENSE)
[![Release](https://img.shields.io/github/v/release/papiche/astroportd?include_prereleases&label=release)](https://github.com/papiche/astroportd/releases)
[![CI](https://github.com/papiche/astroportd/actions/workflows/ci.yml/badge.svg)](https://github.com/papiche/astroportd/actions/workflows/ci.yml)

**Votre cloud, vos photos, vos amis et votre économie coopérative, sur une machine qui est à vous.**

> 🧪 **astroportd est en bêta.** Il s'améliore chaque semaine, avec ceux qui l'utilisent, et **chacun peut participer à le rendre
> meilleur** : signalez un souci ou proposez une idée depuis l'accueil de l'application (« 🐞 Signaler un souci », « 💡 Proposer une
> idée »), ouvrez un [ticket](https://github.com/papiche/astroportd/issues), ou proposez une modification (voir
> [docs/DEVELOPPER.md](docs/DEVELOPPER.md)). Gardez une sauvegarde de ce qui vous est précieux.

astroportd est une **station personnelle** : un seul programme, du Raspberry Pi au serveur GPU. Il garde vos fichiers
**chiffrés**, retrouve les visages dans vos photos et les partage avec vos amis, vous fait **sauvegarder par vos
proches**, et tient une **comptabilité coopérative en Ẑen** adossée à la monnaie libre Ğ1. Sans plateforme, sans
abonnement, sans serveur central. C'est le cœur d'[Astroport.ONE](https://github.com/papiche/Astroport.ONE) et
d'UPlanet, écrit en Rust.

![Roman-photo : Alice rejoint une station, et ses photos trouvent Léa](docs/img/roman-membre.webp)

## Quickstart

```bash
curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash   # sans sudo, archive vérifiée
astroportd init                       # identité de la station, compte du Capitaine (notez le code PASS et la clé de secours)
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
```

Ouvrez **http://127.0.0.1:8090** : c'est votre station. Vos proches s'inscrivent sur `/register`.
En cas de doute : `astroportd doctor`.

Pour la mettre en ligne (HTTPS, nom de domaine, ports, service au démarrage) : [guide du Capitaine, partie 1](docs/guide.html).

## Par où commencer ?

| Vous êtes… | Lisez |
|---|---|
| **Curieux**, sans rien installer | [La page d'accueil](docs/index.html) · [le comparatif](docs/COMPARATIF.md) |
| **Membre** d'une station | [Bienvenue sur notre station](docs/BIENVENUE.md) (une page) |
| **Capitaine** : vous ouvrez une station | [Le guide du Capitaine illustré](docs/guide.html) ([version texte](docs/GUIDE.md)) |
| **Fondateur** d'une coopérative | [Les constellations](docs/CONSTELLATIONS.md) · [la configuration coopérative](docs/CONFIG_COOP.md) · [le manuel des membres](docs/UPLANET_ManuelDesMembresEtDesContributeurs.md) · [la charte](docs/CHARTE_UPLANET.md) |
| **Développeur** | [La référence technique](docs/DEVELOPPER.md) : architecture, commandes, extensions, **écrire un client compatible** |
| Sur **macOS** | [docs/MACOS.md](docs/MACOS.md) |

---

## Ce que vous pouvez faire

| Je veux… | astroportd… |
|---|---|
| **Garder mes fichiers chez moi**, accessibles partout | un cloud personnel **chiffré avant de quitter l'appareil**, monté comme un disque réseau (Finder, Windows, téléphone) ; contacts et agenda synchronisés avec le téléphone. |
| **Retrouver mes photos par visage** et les partager | reconnaît les visages et envoie les photos, chiffrées, à la station de l'ami reconnu. Une photo de groupe attend votre accord. |
| **Chercher dans mes souvenirs** | une seule case : une personne, une ville, une date, un mot lu sur une photo ou dans un PDF. Tout se calcule sur la station. |
| **Faire entrer quelqu'un** | un lien ou un QR d'invitation : la personne crée son compte en un geste, ses photos arrivent. |
| **Ne rien perdre si ma machine tombe** | un ami choisi héberge une copie chiffrée qu'il ne peut pas lire ; avec votre carte de secours et son aide, tout revient sur n'importe quelle station. |
| **Parler entre amis**, sans plateforme | messages privés chiffrés et appels visio de navigateur à navigateur. |
| **Choisir jusqu'où circulent mes données** | trois modes de station : **Forteresse**, **Village**, **Place publique**. |
| **Faire vivre une coopérative** | loyer, partage des revenus, micro-dons et billets papier en Ẑen. |
| **Avoir un assistant qui me connaît** | BRO, une mémoire personnelle locale, avec **votre** IA (Ollama ou une clé que vous déclarez). |

## Comment ça marche, en quatre idées

**1. Une identité, pas un compte chez quelqu'un.** Votre **MULTIPASS** est une clé : elle signe vos messages (Nostr),
tient votre portefeuille (Ğ1) et adresse vos fichiers (IPFS). La station d'un Capitaine l'héberge, mais vous pouvez
déménager. Vous vous connectez depuis n'importe quelle station ; votre code PASS ne se tape que chez vous.

**2. Personne ne lit vos fichiers, pas même le Capitaine.** Ils sont chiffrés avant de quitter votre appareil. Un lien
de partage porte sa clé après le `#`, partie de l'adresse que le navigateur n'envoie jamais.

**3. Vos amis sont votre sauvegarde.** Le secret de votre compte est coupé en trois parts, dont deux suffisent : votre
carte imprimée, votre station, vos amis. Une part seule ne révèle rien.

![Roman-photo : j'appelle un ami, Alice retrouve son compte](docs/img/roman-secours.webp)

**4. La station appartient à son réseau, pas l'inverse.** La vérité vit dans le réseau (événements Nostr, fichiers
IPFS), la base locale n'est qu'un cache qu'on peut reconstruire. Les stations se parlent directement, en pair à pair,
sans ouvrir de port sur la box.

![Les écluses : du coffre au public, les portes s'ouvrent selon le mode de la station](docs/img/ecluses.svg)

## ORIGIN ou ẐEN

![ORIGIN ou ẐEN](docs/img/origin-vs-zen.svg)

- **ORIGIN** : le réseau ouvert, pour découvrir. 1 Ẑen = 0,1 Ğ1.
- **ẐEN** : une constellation privée de stations qui se font confiance (clé partagée `swarm.key`). 1 Ẑen = 1 €.
  On y entre parrainé par un Capitaine (`astroportd init --join <jeton>`) ou on fonde la sienne
  (`astroportd init --mode zen --found-constellation`).

Une même machine peut servir les deux. Détails : [docs/CONSTELLATIONS.md](docs/CONSTELLATIONS.md).

## L'économie, en bref

Chaque semaine, les membres paient un loyer en Ẑen. La station règle **1×PAF** (participation aux frais) à la machine et
**2×PAF** au Capitaine ; le surplus est réparti en **trois tiers** : trésorerie, R&D, actifs. Un compte en retard de
paiement est **suspendu, jamais détruit**. Le Ẑen est une unité de compte de la coopérative, pas une monnaie
spéculative. Détails : [guide, partie 3](docs/GUIDE.md#partie-3).

## Ce qui le distingue

Beaucoup d'outils font une partie de cela (Nextcloud, Immich, Signal, Matrix, Syncthing, Tailscale…). astroportd les
réunit **dans un seul programme que vous possédez**, chiffré de bout en bout, en pair à pair, et financé par sa
coopérative. Le comparatif détaillé : [docs/COMPARATIF.md](docs/COMPARATIF.md).

---

## Télécharger

Les binaires sont publiés dans les [Releases](https://github.com/papiche/astroportd/releases), avec `SHA256SUMS`.
`install.sh` choisit l'archive, vérifie son empreinte, examine la machine et propose un profil (*léger*, *standard*,
*confort*). `install.sh --check-hardware` montre seulement ce diagnostic. `sudo bash install_sudo.sh` ajoute, si besoin,
les outils du système (ffmpeg, poppler, Kubo en ORIGIN, démarrage avec la machine).

| Cible | Pour |
|---|---|
| `x86_64-unknown-linux-gnu` | PC, serveur, VM (inférence locale incluse) |
| `aarch64-unknown-linux-gnu` | Raspberry Pi 4/5, Pi Zero 2 W (inférence locale incluse) |
| `armv7-unknown-linux-gnueabihf` | Raspberry Pi 2/3, 32 bits |
| `x86_64-unknown-linux-musl` · `aarch64-unknown-linux-musl` | binaires statiques (Termux, NAS) ; visages et mémoire calculés par une autre machine |
| `aarch64-apple-darwin` · `x86_64-apple-darwin` | macOS, au mieux ([docs/MACOS.md](docs/MACOS.md)) |

Compiler soi-même : `cargo build --release` (Rust ≥ 1.85 et un compilateur C) ; voir [docs/DEVELOPPER.md](docs/DEVELOPPER.md#4-construire-et-tester).

## Limites connues

Chaque limite est suivie de ce qu'on peut faire en attendant.

- **Restauration** : seuls les amis joignables renvoient vos fichiers ; après 3 jours le compte repart, et les amis absents
  sont relancés chaque jour pendant 30 jours (ce qui revient est fusionné sans rien écraser). Les fichiers sont répartis
  entre les gardiens (`k` parts sur `n` suffisent) : il faut que `k` d'entre eux répondent. La sûreté et le débit n'ont
  pas encore été mesurés sur un vrai réseau ni sur Raspberry Pi. *Ayez trois gardiens ou plus dont la station reste
  allumée (Moi › Sauvegarde affiche la sécurité estimée face à l'objectif de 99,9 %).*
- **Binaires statiques (musl)** : pas d'inférence locale. *Visages et mémoire se calculent sur une autre machine (`--face-engine`, Ollama).*
- **Mémoire de BRO** : un seul moteur d'embeddings par mémoire. *Changer de moteur demande de ré-indexer.*
- **Entre deux UPlanet d'une même machine** : copie à la main, sans miroir automatique. *`astroportd bridge-copy`.*
- **Nuit de 20h12** : une station éteinte ne rattrape que la veille. *Activez le service au démarrage (`astroportd init --systemd`).*
- **MULTIPASS** : ni zine par email ni ZenCard. *Astroport.ONE s'en charge.*
- **Ğ1 réelle et macOS** : moins éprouvés (test manuel, CI non bloquante).

## Contribuer

Issues et pull requests bienvenues ([issues](https://github.com/papiche/astroportd/issues)). Avant de proposer un
changement, les vérifications de la CI doivent passer sans avertissement : voir
[docs/DEVELOPPER.md](docs/DEVELOPPER.md#4-construire-et-tester). Code et documentation en français ; messages de commit
sans mention de co-auteur.

Version 2.3.0 · Licence **AGPL-3.0** · Auteur : Fred (support@qo-op.com). Notes de version : [RELEASE_NOTES.md](RELEASE_NOTES.md).

*Astroport est un bien commun : forkez-le, hébergez-le, faites-le vôtre.*
