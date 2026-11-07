# Constellations : essayer avec ORIGIN, produire avec ẐEN

Une **constellation**, c'est l'ensemble des stations qui se reconnaissent et s'entraident : elles se gardent mutuellement des copies chiffrées et se prêtent leurs services. Avant de démarrer votre station, vous choisissez dans laquelle elle entre : **ORIGIN**, le bac à sable ouvert à tous, ou **ẐEN**, un réseau privé de production.

Cette page vous aide à faire ce choix, puis explique ce qui relie les stations entre elles. Pour les commandes pas à pas, suivez le [guide du Capitaine](GUIDE.md) (le choix de constellation est à l'[étape 4](GUIDE.md#etape-3)).

![ORIGIN ou ẐEN](img/origin-vs-zen.svg)

---

## ORIGIN ou ẐEN : le comparatif

| | **ORIGIN** | **ẐEN** |
|---|---|---|
| **À quoi ça sert** | Apprendre, tester, se tromper sans conséquence. | Rendre un vrai service à de vrais membres, avec une économie coopérative. |
| **Qui peut entrer** | Tout le monde, sans rien demander. | Seulement les stations qui ont reçu la clé du réseau (`swarm.key`, un petit fichier secret que les Capitaines se transmettent). |
| **Le réseau entre stations** | Ouvert. | Privé et chiffré par cette clé. |
| **Ce que vaut 1 Ẑen** | 0,1 Ğ1 (la monnaie libre) : c'est une monnaie de jeu. | 1 € dans la coopérative UPlanet ẐEN. |
| **Qui fixe les règles** | Personne en particulier : c'est un terrain d'essai. | Dans la coopérative UPlanet ẐEN, les membres adhèrent à la [charte](CHARTE_UPLANET.md) et les comptes sont publics. Dans une constellation que vous fondez vous-même, c'est vous et vos Capitaines. |

> **À savoir.** Le taux « 1 Ẑen = 1 € » est celui de la coopérative UPlanet ẐEN (financement via OpenCollective, comptabilité en trois tiers). Il **ne s'applique pas tout seul** à une constellation ẐEN que vous créez : vous fondez un réseau privé dont vous fixez les règles.

La station fait la différence toute seule : **pas de `swarm.key` → ORIGIN ; une `swarm.key` → ẐEN.**

---

## Comment choisir

- **Vous découvrez, vous voulez voir comment ça marche** : commencez en ORIGIN. Rien de secret à fournir, rien à perdre.
- **Un Capitaine d'une constellation ẐEN vous accueille** : il vous remet sa `swarm.key`, vous rejoignez son réseau.
- **Vous voulez monter votre propre réseau avec des proches** (une association, un village, une famille) : fondez votre constellation ẐEN, avec votre propre clé et vos propres règles.

> ⚠️ **Attention.** Changer de constellation, c'est changer de monde : un même compte n'a **pas les mêmes clés** en ORIGIN et en ẐEN, ni dans deux constellations ẐEN différentes. Un MULTIPASS (le compte d'une personne) appartient à **une seule** constellation. **Sauvegardez votre `swarm.key`** : la perdre ou en changer revient à quitter votre constellation.

### Passer d'ORIGIN à la coopérative UPlanet ẐEN

La voie « officielle » est la **certification DRAGON**, une formation à la cybersécurité et à la cryptographie. À son issue, un Capitaine de la constellation ẐEN vous remet sa `swarm.key`. Candidature : support@qo-op.com, objet « Candidature Capitaine DRAGON ». Ce parcours de formation est celui d'Astroport.ONE (voir son README).

---

## Démarrer ou rejoindre une constellation

Dans chaque cas, vous préparez la station avec `astroportd init`, puis vous la lancez avec `astroportd serve` (détails à l'[étape 5 du guide](GUIDE.md#etape-4)).

### Rejoindre ORIGIN

```bash
astroportd init --mode origin
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
```

Rien de secret à fournir. Votre station est visible des autres stations ORIGIN. [qo-op.com](https://qo-op.com) reste une option (une station ORIGIN publique), pas une obligation : vous créez les comptes sur votre propre station (voir plus bas).

### Rejoindre une constellation ẐEN existante

```bash
chmod 600 swarm.key        # la clé reçue d'un Capitaine, lisible par vous seul
astroportd init --mode zen --swarm-key ./swarm.key
astroportd serve ... --swarm-key ./swarm.key
```

Toutes les stations d'une même constellation doivent avoir **exactement la même** clé. Si votre station ne voit pas les autres, c'est la première chose à vérifier, avec `astroportd doctor` (le diagnostic de la station).

### Fonder votre propre constellation

```bash
astroportd swarm-key > swarm.key    # fabrique une nouvelle clé de réseau privé
chmod 600 swarm.key
```

Copiez ensuite ce fichier sur **chaque** station de votre réseau, par un canal sûr (clé USB, copie chiffrée), puis lancez sur chacune `astroportd init --mode zen --swarm-key ./swarm.key`.

### Créer les comptes de vos membres

![Création d'un MULTIPASS](img/multipass-creation.svg)

Le premier compte créé sur une station est celui du **Capitaine**. Les suivants suivent automatiquement le Capitaine.

```bash
astroportd multipass create capitaine@exemple.org --captain   # le PREMIER compte = le Capitaine de la station
astroportd multipass create ami@exemple.org                   # les suivants suivent automatiquement le Capitaine
astroportd multipass list
```

Vos membres peuvent aussi s'inscrire eux-mêmes dans leur navigateur, sur la page **`/register`** de votre station. L'inscription est **ouverte par défaut** ; vous la réglez avec `--max-accounts` (nombre maximal de comptes) et `--register-per-hour` (inscriptions par heure et par adresse), ou vous la fermez avec `--no-register`.

> ⚠️ **Attention.** Les secrets d'un compte (SALT, PEPPER, nsec, code PASS, jeton web) ne sont affichés **qu'une seule fois**. Le membre doit les noter tout de suite. Détails à l'[étape 6 du guide](GUIDE.md#etape-5).

### Une station dans plusieurs constellations

Un `astroportd serve` sert **une** constellation (une `swarm.key`, ou aucune). Pour raccorder la même machine à ORIGIN **et** à une ou plusieurs UPlanet ẐEN, utilisez `astroportd planes` : un seul programme, la même identité de station, et pour chaque constellation (chaque « plan ») son réseau, sa base, ses comptes et son port. Voir, dans le guide, [Plusieurs UPlanet dans un seul processus](GUIDE.md#etape-8bis-i).

---

## Y-Level : une seule clé, quatre identités

![Y-Level](img/y-level.svg)

Imaginez un trousseau où une seule clé ouvre quatre portes. Avec **Y-Level**, la clé de votre machine sert à la fois :

1. de **clé SSH** (la clé qui permet de se connecter à distance à la machine) ;
2. d'**identité de la station** sur le réseau (son PeerID IPFS, une adresse qui commence par `12D3Koo…`) ;
3. de **portefeuille Ğ1** (la clé publique de la monnaie libre) ;
4. d'**identité Nostr** (le réseau social décentralisé où la station publie).

Ce que vous y gagnez : n'importe qui peut **vérifier** que la station `12D3Koo…` est bien la machine dont il connaît la clé SSH, sans annuaire ni tiers de confiance. C'est la base de la confiance entre Capitaines.

`astroportd init` lance Y-Level pour vous (voir l'[étape 3 du guide](GUIDE.md#etape-2)). Pour vérifier le lien ensuite :

```bash
astroportd ylevel            # affiche SSH ↔ PeerID IPFS ↔ Ğ1 ↔ Nostr
astroportd ylevel --verify   # code de sortie 1 si la clé SSH ne correspond pas à l'identité de la station
```

> ⚠️ **Attention.** Votre **clé SSH publique change**. Mettez la nouvelle à jour partout où l'ancienne était enregistrée (serveurs, GitHub…). L'ancienne n'est **pas détruite** : elle est rangée dans `~/.ssh/origin.key` et `~/.ssh/origin.pub`.

> **À savoir.** Deux options de `astroportd init` règlent Y-Level : `--no-ssh` ne touche pas à votre clé SSH (le lien SSH ↔ IPFS n'est alors pas établi) ; `--kubo-config` aligne aussi l'identité d'un logiciel IPFS Kubo installé sur la machine.

Les clés de la station ne dépendent pas de la constellation : elles sont identiques en ORIGIN et en ẐEN. Seules les clés des comptes (MULTIPASS) changent d'une constellation à l'autre.

---

## Individus, stations, territoire

![Individus, Astroports et territoire](img/individu-astroport-territoire.svg)

Une constellation s'organise sur trois niveaux :

| Niveau | Ce que c'est | Son identité |
|---|---|---|
| **Individu** | une personne, avec son **MULTIPASS** | une clé Nostr, des clés Ğ1 |
| **Astroport** | une station tenue par un **Capitaine** | son identifiant de station (PeerID IPFS `12D3Koo…`), égal à sa clé SSH grâce à Y-Level |
| **Territoire** | les stations d'une zone géographique | des dalles **UMAP** de 0,01° de côté (environ 1 km²), chacune avec sa propre identité Nostr |

Ce qui relie ces niveaux : **les clés** (Y-Level), **les suivis Nostr** entre personnes et entre Capitaines, et, en ẐEN, **le réseau privé** (`swarm.key`).

---

## Les liens entre Capitaines

C'est ce qui relie les stations entre elles, sans annuaire central.

1. **Y-Level** donne à chaque station une identité unique et vérifiable.
2. Chaque station publie ses rapports économiques sur Nostr (événement de type **kind 30850**).
3. Un Capitaine **suit** les autres Capitaines. Deux Capitaines qui se suivent l'un l'autre sont des **Capitaines amis**. Dans l'interface, l'écran *Capitaines* montre qui suit qui et propose **Suivre / Ne plus suivre** (voir, dans le guide, [Relier sa station aux autres Capitaines](GUIDE.md#etape-9)).
4. Comme la clé SSH d'un Capitaine se déduit de l'identité de sa station, ce suivi suffit à savoir quelles machines sont dignes de confiance : un Capitaine peut **aider un autre à distance**, pour du support technique mutuel.
5. Cet accès passe par un **tunnel** (un passage chiffré) entre les deux stations, à travers le maillage P2P : aucun port n'est à ouvrir sur la box internet de l'hôte.

### Ce que vous décidez : qui ouvre quoi

Votre station offre au maillage trois services : **SSH** (l'accès à distance), **Ollama** (un moteur d'IA) et **ComfyUI** (un générateur d'images), plus ceux que vous ajoutez. Pour chacun, vous choisissez dans *Options › Tunnels* qui peut l'ouvrir :

| Réglage | Qui peut ouvrir le service |
|---|---|
| `off` | personne |
| `listed` | seulement les stations que vous avez nommées (éventuellement pour une durée limitée) |
| `friends` | vos Capitaines amis, plus la liste |
| `swarm` | tout membre du réseau |

Réglages de départ : Ollama et ComfyUI sont sur `friends`. Le SSH est **fermé** (`off`) en ẐEN, et sur `friends` en ORIGIN. Un service que vous ajoutez vous-même n'est ouvert à personne (`listed`, liste vide) tant que vous ne choisissez pas. Le partage d'un ordinateur puissant (un « Brain GPU ») entre Capitaines relève de l'entraide, pas d'un paiement.

> ⚠️ **Attention.** Le tunnel ne fait que transporter les connexions : **c'est le serveur SSH de votre machine (`sshd`) qui autorise ou refuse**. Avant d'ouvrir le SSH, et surtout en ORIGIN (ouvert à tous), vérifiez que la connexion par mot de passe est désactivée (`PasswordAuthentication no`) et que seules les clés que vous avez choisies sont autorisées.

---

## FaceCloud : partager ses photos entre amis

![FaceCloud](img/facecloud-partage.svg)

FaceCloud reconnaît les visages sur vos photos et les partage avec les personnes concernées, dans le cercle de confiance :

- **N¹**, ce sont vos amis réciproques : les personnes que vous suivez *et* qui vous suivent. **N²**, ce sont les amis de vos amis.
- Quand FaceCloud reconnaît un ami N¹ sur une photo, il la **lui partage automatiquement** : copie dans son dossier `/Partages/De_<nom>/` s'il est sur la même station, sinon message chiffré vers sa station.
- Pour quelqu'un hors du cercle, vous créez un **lien secret** `/s/<slug>#clé`. La clé est après le `#` : le navigateur ne l'envoie jamais au serveur, la station ne peut donc pas lire le fichier.
- Les **vidéos** ne sont livrées qu'à leur auteur et à ses amis N¹, jamais à tout le cercle N².
- Le relai Nostr de la station réserve les contenus privés aux membres authentifiés du cercle N¹/N².

La reconnaissance des visages se fait soit par un service externe (`--face-engine`), soit sur la station elle-même (`--face-models`, voir [FaceCloud dans le guide](GUIDE.md#etape-8)). Aucun modèle n'est livré avec le programme.

### Vos copies chez les autres : la réplication

Votre station va chercher régulièrement les profils et messages publics de ses membres, de leurs amis (N¹) et des amis de leurs amis (N²), auprès des autres stations de la constellation. Par défaut, un passage toutes les 15 minutes (`--crawl-every`, 900 secondes ; `--no-crawl` pour couper). Elle passe **par le maillage P2P**, sans port public. Si une station annonce aussi une adresse publique, celle-ci ne sert que de **solution de repli** quand le maillage échoue.

L'écran **Réplication** de l'interface vous montre, pour vos derniers messages et vos derniers fichiers, **où** ils existent aussi (sur quelles stations) et **chez qui** (vos proches, les proches de vos proches, ou d'autres stations). Il indique aussi par quelle voie chaque station a été jointe. Une station qui ne répond pas est marquée « injoignable », jamais comptée comme « 0 copie ». La mesure des fichiers demande l'option `--kubo`.

---

## L'économie d'une station

Chaque semaine, la station règle ses frais à partir des loyers d'hébergement reçus. La charge hebdomadaire vaut **3×PAF** (PAF : participation aux frais). L'ordre est fixe :

1. **1×PAF au NODE**, c'est-à-dire à l'**Armateur** (celui qui fournit la machine) : c'est le loyer du matériel ;
2. **2×PAF au Capitaine** (sur son MULTIPASS) : c'est la rétribution de son travail ;
3. le **surplus** éventuel : d'abord une provision d'impôt, puis une prime de gestion de **1 %** au Capitaine, puis le reste réparti en **trois tiers** : **TREASURY** (la trésorerie), **R&D** (la recherche) et **ASSETS** (le matériel).

S'il manque de l'argent (recettes inférieures à la PAF), le manque est couvert par la trésorerie ou par le bénévolat déclaré (Love Ledger), **jamais** par un prélèvement sur TREASURY, R&D ou ASSETS.

---

## Qui fait quoi : astroportd et Astroport.ONE

astroportd (version 0.5.7-beta, une préversion) remplace peu à peu les scripts bash d'[Astroport.ONE](https://github.com/papiche/Astroport.ONE). Les deux fonctionnent ensemble ; voici ce que chacun fait aujourd'hui.

| Fonction | astroportd | Astroport.ONE |
|---|:---:|:---:|
| Y-Level (SSH → IPFS → Ğ1 → Nostr) | ✔ `init`, `ylevel` | ✔ `Ylevel.sh` |
| Réseau privé `swarm.key` (ẐEN), réseau ouvert (ORIGIN) | ✔ | ✔ |
| Tunnels P2P entre stations (dont SSH) | ✔ politique par service, bloc géré dans `authorized_keys` | ✔ `DRAGON_p2p_ssh.sh` |
| Suivi entre Capitaines (kind 30850) | ✔ manuel (écran *Capitaines*) | ✔ automatique |
| Création d'un MULTIPASS | ✔ `multipass create`, page `/register` (version allégée) | ✔ `make_NOSTRCARD.sh` (complet : SSSS, QR, zine, ZenCard) |
| uCloud chiffré, FaceCloud, liens secrets | ✔ | ✔ (version bash/Python) |
| Relai Nostr avec pare-feu social N¹/N² | ✔ intégré | ✔ strfry + filtres NIP-101 |
| Économie Ẑen (règlement hebdomadaire, 3×1/3, Love Ledger) | ✔ | ✔ |

---

## Pour aller plus loin (technique)

### Détection ORIGIN / ẐEN

- *astroportd* : `serve --swarm-key FICHIER` active le réseau privé libp2p (clé pré-partagée, ou PSK) ; sans cette option, le démarrage affiche « réseau ouvert » (`src/serve.rs`). `astroportd init --mode origin|zen` prépare la station dans ce sens.
- *Astroport.ONE* : `tools/my.sh` définit `UPLANETNAME` comme la dernière ligne de `~/.ipfs/swarm.key`, ou 64 zéros (`000…0`) s'il n'y en a pas.

| | ORIGIN | ẐEN |
|---|---|---|
| `swarm.key` | absente | présente |
| `UPLANETNAME` | 64 zéros | la clé partagée (dernière ligne de `swarm.key`) |
| Réseau P2P | ouvert | privé, chiffré par la clé pré-partagée |

### Format de la `swarm.key`

`astroportd swarm-key` tire 32 octets au hasard et les écrit au format des réseaux privés IPFS/libp2p : `/key/swarm/psk/1.0.0/`, puis `/base16/`, puis 64 caractères hexadécimaux (`astro_p2p::generate_swarm_key`).

### Pourquoi un compte change de clés d'une constellation à l'autre

Les nouveaux MULTIPASS utilisent la formule de clés dite **isolated** : `(SALT, PEPPER_<sel>)` où `sel = sha256(UPLANETNAME)[:16]` (*astroportd* : `astro-core`, `KeyFormula::Isolated`). Le SALT et le PEPPER sont tirés au hasard (24 caractères). Ces crédentiels passent par scrypt ; la clé Nostr (sha256 de la graine), la clé Ğ1 et l'identité IPNS en découlent. Le **même** couple SALT/PEPPER donne donc **d'autres clés** dans une autre constellation (vérifié contre le `keygen` d'Astroport.ONE).

### Ce que `multipass create` écrit

`~/.zen/game/nostr/<email>/` : `.secret.nostr`, `.secret.dunikey`, `.keyformula`, `.pass`, `.multipass.json` (en `0600`, lisibles par le seul propriétaire), plus `HEX`, `NPUB`, `G1PUBNOSTR`, `LANG`, `NOSTRNS`. Et trois événements signés par le compte : profil (kind 0), suivi du Capitaine (kind 3), DID W3C (kind 30800).

**Non repris de `make_NOSTRCARD.sh`** : le découpage SSSS 2-sur-3 de DISCO chiffré vers le Capitaine, les QR codes, l'import de la clé IPNS dans Kubo, l'envoi du zine par email, la ZenCard et la primo-transaction. Retrouver un compte repose sur le SALT/PEPPER remis une fois au titulaire (et sur le `nsec`).

### Plusieurs plans dans un processus

`astroportd planes` : un seul processus, la même identité Y-Level, et par plan un réseau P2P privé, une base, un dossier de comptes et un port ; chaque MULTIPASS est rattaché à son plan. Le routage par nom d'hôte (`--front`) et `--plane` sont disponibles ; le miroir entre plans (règles de synchronisation, charte) ne l'est pas encore.

### La chaîne de dérivation Y-Level

```
~/.ssh/id_ed25519
   └─ sha512 du fichier ─→ SECRET1 (64 premiers caractères hex) + SECRET2 (64 suivants)
        └─ scrypt (SALT = SECRET1, PEPPER = SECRET2, N = 4096, r = 16, p = 1) ─→ graine ed25519 de 32 octets
             ├─ clé SSH « jumelle »   (cette même clé ed25519, au format OpenSSH)
             ├─ PeerID IPFS           (12D3Koo… = IPFSNODEID, multihash de la clé publique)
             ├─ clé publique Ğ1       (base58 de la même clé publique)
             └─ clé Nostr             (clé privée = sha256 de la graine)
```

Les paramètres scrypt sont ceux de Duniter. La partie « graine → identités » est dans `astro-core` (`kdf.rs`), validée par des vecteurs de test produits par `tools/keygen` d'Astroport.ONE ; le rituel complet reprend `Astroport.ONE/tools/Ylevel.sh`.

La preuve est arithmétique : la clé publique SSH `ssh-ed25519 AAAA…` contient les 32 octets de la clé publique ed25519 ; le PeerID `12D3Koo…` est l'encodage de ces **mêmes** 32 octets. L'outil `Astroport.ONE/tools/ssh_to_g1ipfs.py` fait la conversion dans les deux sens. Le champ `SSHPUB` du `12345.json` d'une station publie cette preuve.

### Tunnels DRAGON

Le transport est un tunnel libp2p `/x/<service>-<PeerID>` entre les deux stations, avec une garde par flux et un watchdog de session (*astroportd* : `astro-p2p`, `src/serve.rs`). La politique est dans `src/tunnel_policy.rs`. Le SSH offert au maillage est `127.0.0.1:22` (`--p2p-ssh`, `none` pour ne pas l'offrir), ComfyUI `127.0.0.1:8188` (`--p2p-comfyui`), Ollama `127.0.0.1:11434` (`--p2p-ollama`).

```bash
# L'hôte expose son SSH dans le maillage :
astroportd serve ... --p2p-listen /ip4/0.0.0.0/tcp/4101 --p2p-expose ssh=127.0.0.1:22 --swarm-key ./swarm.key

# L'autre Capitaine le joint depuis un port local :
astroportd serve ... --p2p-forward 127.0.0.1:2222=<PeerID de l'hôte>/ssh
ssh -p 2222 utilisateur@127.0.0.1
```

astroportd tient dans `~/.ssh/authorized_keys` (ou le dossier de `--ssh-dir`) un **bloc balisé `astroportd`** aligné sur la politique du SSH : les stations nommées et les Capitaines amis y figurent sous la forme de leur clé SSH jumelle. Rien n'est modifié hors de ce bloc.

`astroportd route <service>` choisit le meilleur nœud de l'essaim (Power-Score, distance, tirage).

Côté Astroport.ONE, le suivi automatique des Capitaines est fait par `DRAGON_p2p_ssh.sh`, selon les règles de `AUTOfollow.rules.md` : le Capitaine suit ses passagers, les passagers suivent leur Capitaine, les Capitaines d'un même essaim se suivent.

### FaceCloud et réplication

- Partage automatique N¹ : `astro-facecloud`, `pipeline.rs` ; hors station, message de fichier chiffré NIP-17 emballé en NIP-59.
- Liens secrets : `astro-dav`, `share.rs`.
- Relai réservé au cercle N¹/N² authentifié : NIP-42.
- Synchronisation N¹/N² : passe par le tunnel `/x/strfry-<PeerID>` (le même que celui de DRAGON) ; l'adresse publique de repli est le champ `myRELAY` du `12345.json` d'un pair. La mesure des fichiers par `--kubo` recherche les fournisseurs d'un CID.

### Règlement hebdomadaire

`src/settlement.rs` : `weekly_costs` est la charge hebdomadaire totale 3×PAF (PAF = un tiers). Ordre des virements : NODE (1×PAF), Capitaine (2×PAF), `IMPOT`, prime de gestion (1 % → MULTIPASS du Capitaine), `TREASURY`, `RND`, `ASSETS` (règle 3×1/3, `astro_economy::split`). Le déficit éventuel est celui de la clôture : trésorerie ou bénévolat actif (Love Ledger), jamais un prélèvement sur `TREASURY`, `RND` ou `ASSETS`.
