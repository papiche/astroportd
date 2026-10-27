# Annonce d'astroportd à la communauté Nostr — brouillons

> **⚠ Rien n'est publié.** Ces textes sont des **brouillons** : aucun événement n'a été signé ni envoyé.
> Ils sont à **relire, ajuster et signer avec la clé de Fred** (la sienne, pas celle d'une station), puis à
> publier depuis un client Nostr de son choix. Vérifier d'abord que le dépôt et la release sont publics et que les liens
> ci-dessous répondent.

Liens utilisés : dépôt https://github.com/papiche/astroportd · releases https://github.com/papiche/astroportd/releases ·
guide `docs/index.html` (à remplacer par l'adresse publique du guide, une fois publiée).

---

## 1. Note courte (kind 1)

### Version française (≈ 500 caractères)

```
astroportd 0.1 (préversion) : le cœur d'Astroport.ONE réécrit en Rust, en un seul binaire.

Relai Nostr, cloud personnel chiffré, reconnaissance de visages et partage entre amis (N¹) ou par lien secret, maillage P2P. Tourne sur un Raspberry Pi.

Votre clé SSH devient l'identité IPFS de votre station (Y-Level) ; deux constellations : ORIGIN (ouverte) et ẐEN (privée).

Code AGPL-3.0, guide pas à pas :
https://github.com/papiche/astroportd

#nostr #opensource #selfhosted #ipfs #rust #g1 #UPlanet
```

### English version (≈ 470 characters)

```
astroportd 0.1 (preview): the core of Astroport.ONE rewritten in Rust, as a single binary.

Nostr relay, encrypted personal cloud, face recognition with sharing to friends (N¹) or via secret link, P2P mesh. Runs on a Raspberry Pi.

Your SSH key becomes your station's IPFS identity (Y-Level); two constellations: ORIGIN (open) and ẐEN (private).

AGPL-3.0, step-by-step guide:
https://github.com/papiche/astroportd

#nostr #opensource #selfhosted #ipfs #rust #g1 #UPlanet
```

---

## 2. Fil de 4 notes

**Note 1/4 — FaceCloud et partages N¹/N²**

```
1/4 FaceCloud, dans astroportd : vos photos sont chiffrées avant de quitter votre appareil, puis la station repère les visages.

Reconnu au-dessus de 0,82 de ressemblance ; entre 0,55 et 0,82, elle demande « est-ce X ? » et c'est vous qui validez.

Les photos d'un ami N¹ (réciproque) lui arrivent seules : copie locale, ou message chiffré vers sa station. Pour les autres : un lien secret dont la clé reste dans le # de l'adresse.
```

**Note 2/4 — Y-Level**

```
2/4 Y-Level : votre clé SSH ed25519 est hachée, étirée (scrypt), et donne une graine unique.

De cette graine : une clé SSH jumelle, le PeerID IPFS de la station, la clé publique Ğ1, la clé Nostr.

SSH = IPFS : la même clé publique. Les Capitaines qui se suivent sur Nostr peuvent s'ouvrir un accès SSH entre machines de confiance. `astroportd ylevel --verify` le prouve.
```

**Note 3/4 — ORIGIN / ẐEN**

```
3/4 Deux constellations. ORIGIN : bac à sable ouvert, sans clé secrète, 1 Ẑen = 0,1 Ğ1 (un jeu). ẐEN : réseau privé, il faut la swarm.key ; 1 Ẑen = 1 € dans la coopérative UPlanet ẐEN.

Pas de swarm.key = ORIGIN ; swarm.key présente = ẐEN. Vous pouvez aussi fonder la vôtre : `astroportd swarm-key`.
```

**Note 4/4 — Comment essayer**

```
4/4 Essayer (Linux, Raspberry Pi) :

curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
astroportd init
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs

Préversion 0.1.0 : elle héberge des MULTIPASS mais n'en crée pas encore en ligne de commande. Retours bienvenus : https://github.com/papiche/astroportd/issues
```

---

## 3. Texte long (NIP-23, kind 30023)

Tags suggérés : `d` = `astroportd-presentation`, `title` = « astroportd : une station Astroport en un seul binaire »,
`t` = `nostr`, `opensource`, `selfhosted`, `ipfs`, `rust`, `g1`, `uplanet`, `published_at` = date de publication.

```markdown
# astroportd : une station Astroport en un seul binaire

**astroportd** est la réécriture en Rust du cœur d'[Astroport.ONE](https://github.com/papiche/Astroport.ONE) et
d'UPlanet. Il remplace une pile de scripts et de services (relai Nostr, API, cloud, visages…) par **un exécutable**,
qui tient sur un Raspberry Pi. C'est une **préversion 0.1.0**, testée de bout en bout mais jeune.

## Le principe

Il n'y a qu'une source de vérité : les **événements Nostr** et les **blobs IPFS**. La base SQLite locale n'est qu'un
cache : on peut la supprimer, `astroportd replay` la reconstruit. Les fichiers sont chiffrés avant de quitter la
machine.

## FaceCloud et partages entre amis

Vous déposez vos photos dans votre uCloud (WebDAV). La station détecte et compare les visages (seuil de
reconnaissance 0,82 ; entre 0,55 et 0,82 elle demande confirmation). Les photos d'un ami **N¹** (réciproque) lui sont
partagées automatiquement, sur la même station ou vers sa station par message chiffré. Pour tout autre destinataire,
un **lien secret** `/s/<slug>#clé` : la clé reste dans le fragment de l'adresse et n'est jamais envoyée au serveur.
Les films et séries déposés au titre de la copie privée ne sont montrés qu'au cercle N¹ de l'auteur. La reconnaissance
faciale s'appuie sur un service d'inférence HTTP : aucun modèle n'est embarqué dans le binaire.

## Y-Level : SSH = IPFS

La clé SSH est hachée en SALT et PEPPER, un scrypt en tire une graine ed25519 ; de cette graine viennent une clé SSH
jumelle, le PeerID IPFS de la station, la clé Ğ1 et la clé Nostr. L'identité réseau d'une machine est donc **la même clé**
que son accès SSH. Les Capitaines qui se suivent sur Nostr peuvent ainsi s'ouvrir, à travers des tunnels P2P, un accès
entre machines de confiance (le suivi automatique reste pour l'instant dans Astroport.ONE).

## ORIGIN et ẐEN

ORIGIN est un bac à sable ouvert (pas de clé de réseau, 1 Ẑen = 0,1 Ğ1). ẐEN est un réseau privé protégé par une
`swarm.key` (1 Ẑen = 1 € dans la coopérative UPlanet ẐEN). Pas de clé : ORIGIN. Clé présente : ẐEN. On peut aussi
fonder sa propre constellation avec `astroportd swarm-key`.

## Essayer

    curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
    astroportd init
    astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs

Binaires pour x86_64, Raspberry Pi (aarch64, armv7) et x86_64 statique. Code sous licence AGPL-3.0 :
https://github.com/papiche/astroportd — retours et contributions bienvenus.
```

---

## 4. Où publier

À choisir par Fred, sans sur-publier (une seule fois, puis répondre aux questions) :

- **Relais** : ceux que Fred utilise déjà pour l'écosystème UPlanet ; le relai public de la constellation ; un ou deux
  relais généralistes francophones. Les relais exacts sont à confirmer par Fred.
- **Hashtags** : `#nostr #opensource #selfhosted #ipfs #rust #g1 #UPlanet` (FR) ; `#selfhosting #privacy #raspberrypi` (EN).
- **Communautés Nostr** (NIP-72 ou groupes, à vérifier qu'elles existent et accueillent ce sujet) : développeurs de
  relais et de clients, auto-hébergement, monnaie libre Ğ1 / Duniter, francophonie.
- **Hors Nostr** : le fil de releases du dépôt, le dépôt [Astroport.ONE](https://github.com/papiche/Astroport.ONE)
  (lien dans son README), le forum de la monnaie libre.
- **Étiqueter** l'annonce comme **préversion** et rappeler la limite principale : pas de création de MULTIPASS en ligne
  de commande dans la 0.1.

## 5. Avant de publier

- [ ] Le dépôt est public, la release `v0.1.0` existe, `install.sh` fonctionne sur une machine neuve.
- [ ] Les liens des notes répondent (dépôt, releases, guide).
- [ ] Les affirmations restent exactes (seuils 0,82 / 0,55, tailles de lien, limites de la préversion).
- [ ] Signé avec la clé de Fred ; aucune clé de station utilisée.
