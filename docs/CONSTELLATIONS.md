# Constellations : ORIGIN, ẐEN, Y-Level et liens entre Capitaines

Ce document explique **ce qui se passe sous le capot** quand on démarre une station. Pour le pas-à-pas, voir le [guide](GUIDE.md) ([version illustrée](index.html)).

> **Comment lire ce document.** Chaque point est marqué : **astroportd** (implémenté dans ce dépôt et testé) ou **Astroport.ONE** (hérité : fait par les scripts bash du dépôt [papiche/Astroport.ONE](https://github.com/papiche/Astroport.ONE), pas encore réécrit en Rust). Les deux fonctionnent ensemble ; astroportd 0.2.0-beta est une **préversion**.

![Individus, Astroports et territoire](img/individu-astroport-territoire.svg)

---

## 1. Une constellation, c'est quoi ?

Une **constellation** est un ensemble de stations (des « Astroports ») qui se reconnaissent et se parlent, avec trois niveaux :

| Niveau | Ce que c'est | Identité |
|---|---|---|
| **Individu** | une personne, avec son **MULTIPASS** | une clé Nostr, des clés Ğ1 |
| **Astroport** | une station tenue par un **Capitaine** | `IPFSNODEID` (PeerID IPFS `12D3Koo…`), égale à sa clé SSH grâce à Y-Level |
| **Territoire** | les stations d'une zone géographique | des dalles **UMAP** de 0,01° (environ 1 km²), chacune avec sa propre identité Nostr |

Ce qui relie ces niveaux : **les clés** (Y-Level), **les suivis Nostr** entre personnes et entre Capitaines, et **le réseau privé** (`swarm.key`) pour la constellation ẐEN.

---

## 2. ORIGIN et ẐEN

![ORIGIN ou ẐEN](img/origin-vs-zen.svg)

| | **UPlanet ORIGIN** | **UPlanet ẐEN** |
|---|---|---|
| Vocation | Bac à sable : apprendre, essayer | Production : économie coopérative |
| `swarm.key` | **absente** | **présente** |
| `UPLANETNAME` | 64 zéros (`000…0`) | la clé partagée (dernière ligne de `swarm.key`) |
| Réseau P2P | **ouvert** | **privé**, chiffré par la clé pré-partagée |
| 1 Ẑen vaut | 0,1 Ğ1 (monnaie de jeu) | 1 € dans la coopérative UPlanet ẐEN |
| Qui y entre | tout le monde | les membres qui ont reçu la clé |

**La règle de détection est mécanique** : pas de `swarm.key` → ORIGIN ; `swarm.key` présente → ẐEN.

- *astroportd* : `serve --swarm-key FICHIER` active le réseau privé libp2p (clé pré-partagée) ; sans cette option le démarrage affiche « réseau ouvert » (`src/serve.rs`). `astroportd init --mode origin|zen` prépare la station dans ce sens.
- *Astroport.ONE* : `tools/my.sh` définit `UPLANETNAME` comme la dernière ligne de `~/.ipfs/swarm.key`, ou 64 zéros s'il n'y en a pas.

### Pourquoi `UPLANETNAME` compte

Les nouveaux MULTIPASS utilisent la formule de clés dite **isolated** : `(SALT, PEPPER_<sel>)` où `sel = sha256(UPLANETNAME)[:16]` (*astroportd* : `astro-core`, `KeyFormula::Isolated`). Une même personne obtient donc des clés **différentes** dans ORIGIN et dans une constellation ẐEN, et dans deux constellations ẐEN différentes. Conséquences pratiques :

- **sauvegardez votre `swarm.key`** ; la perdre ou en changer, c'est changer de monde ;
- les clés de la **station elle-même** (NODE) ne dépendent pas de `UPLANETNAME` : elles viennent de Y-Level (§4) et sont identiques d'une constellation à l'autre.

### Passer d'ORIGIN à ẐEN

La voie « officielle » est la **certification DRAGON** (une formation à la cybersécurité et à la cryptographie) : un Capitaine de la constellation ẐEN vous remet alors sa `swarm.key`. Candidature : support@qo-op.com, objet « Candidature Capitaine DRAGON ». Ce parcours de formation est celui d'*Astroport.ONE* (voir son README).

---

## 3. Démarrer une constellation

### 3.1 Rejoindre ORIGIN

```bash
astroportd init --mode origin
astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs
```

Rien de secret à fournir. Votre station est visible des autres stations ORIGIN. Pour avoir un compte, créez-le sur votre station : `astroportd multipass create <email> --captain` (voir §3.4). [qo-op.com](https://qo-op.com) reste une option (une station ORIGIN publique), pas une obligation.

### 3.2 Rejoindre une constellation ẐEN existante

```bash
chmod 600 swarm.key        # la clé reçue d'un Capitaine
astroportd init --mode zen --swarm-key ./swarm.key
astroportd serve ... --swarm-key ./swarm.key
```

Toutes les stations d'une même constellation doivent avoir **exactement la même** clé. Si une station ne voit pas les autres, c'est la première chose à vérifier (`astroportd doctor`).

### 3.3 Fonder sa propre constellation

```bash
astroportd swarm-key > swarm.key    # génère une clé de réseau privé (32 octets aléatoires)
chmod 600 swarm.key
```

Le fichier est au format `/key/swarm/psk/1.0.0/` · `/base16/` · 64 caractères hexadécimaux, le format des réseaux privés IPFS/libp2p (`astro_p2p::generate_swarm_key`). Copiez-le ensuite sur **chaque** station par un canal sûr, puis `astroportd init --mode zen --swarm-key ./swarm.key`.

> Une constellation que vous fondez est un **réseau privé dont vous fixez les règles**. L'économie « 1 Ẑen = 1 € » est celle de la coopérative UPlanet ẐEN (financement via OpenCollective, comptabilité des trois tiers) ; elle ne s'applique pas automatiquement à votre réseau.

### 3.4 Créer les MULTIPASS sur sa station

![Création d'un MULTIPASS](img/multipass-creation.svg)

*astroportd* crée les MULTIPASS lui-même (reprise de `make_NOSTRCARD.sh` d'*Astroport.ONE*) :

```bash
astroportd multipass create capitaine@exemple.org --captain   # le PREMIER compte = le Capitaine de la station
astroportd multipass create ami@exemple.org                   # les suivants suivent automatiquement le Capitaine
astroportd multipass list
```

Les membres peuvent aussi s'inscrire au navigateur sur la page **`/register`** de la station (inscription **ouverte par défaut** ; `--max-accounts`, `--register-per-hour`, `--no-register` pour la régler ou la fermer). Les secrets (SALT, PEPPER, nsec, code PASS, jeton web) ne sont affichés **qu'une seule fois**.

**Une constellation par compte.** Le SALT et le PEPPER sont tirés au hasard (24 caractères). Les crédentiels passés à scrypt sont `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` : la clé Nostr (sha256 de la graine), la clé Ğ1 et l'identité IPNS en découlent. Le **même** couple SALT/PEPPER donne donc **d'autres clés** dans une autre constellation (vérifié contre le `keygen` d'Astroport.ONE). Un MULTIPASS est rattaché à **une** constellation : ORIGIN s'il n'y a pas de `swarm.key`, sinon ẐEN.

**Ce qui est écrit** : `~/.zen/game/nostr/<email>/` (`.secret.nostr`, `.secret.dunikey`, `.keyformula`, `.pass`, `.multipass.json`, en `0600`, plus `HEX`, `NPUB`, `G1PUBNOSTR`, `LANG`, `NOSTRNS`) et trois événements signés par le compte : profil (kind 0), suivi du Capitaine (kind 3), DID W3C (kind 30800).

#### Limites

- **Non repris de `make_NOSTRCARD.sh`** : le découpage SSSS 2-sur-3 de DISCO chiffré vers le Capitaine, les QR codes, l'import de la clé IPNS dans Kubo, l'envoi du zine par email, la ZenCard et la primo-transaction. Retrouver un compte repose sur le SALT/PEPPER remis une fois au titulaire (et sur le `nsec`).
- **Une station = une constellation** (une `swarm.key`, ou aucune). Raccorder une même station à ORIGIN **et** à ẐEN en parallèle est **prévu** : deux réseaux P2P parallèles, la même identité Y-Level, chaque MULTIPASS rattaché à sa constellation. Ce n'est **pas encore disponible**.

---

## 4. Y-Level : une clé, quatre identités

![Y-Level](img/y-level.svg)

Y-Level est le « rituel de fusion » entre le Capitaine, sa machine et le réseau. Au lieu d'avoir une clé SSH d'un côté et une identité IPFS de l'autre, **les deux sont la même clé ed25519**.

### La chaîne de dérivation

```
~/.ssh/id_ed25519
   └─ sha512 du fichier ─→ SECRET1 (64 premiers caractères hex) + SECRET2 (64 suivants)
        └─ scrypt (SALT = SECRET1, PEPPER = SECRET2, N = 4096, r = 16, p = 1) ─→ graine ed25519 de 32 octets
             ├─ clé SSH « jumelle »   (cette même clé ed25519, au format OpenSSH)
             ├─ PeerID IPFS           (12D3Koo… = IPFSNODEID, multihash de la clé publique)
             ├─ clé publique Ğ1       (base58 de la même clé publique)
             └─ clé Nostr             (clé privée = sha256 de la graine)
```

Les paramètres scrypt sont ceux de Duniter. La partie « graine → identités » est dans `astro-core` (`kdf.rs`), validée par des vecteurs de test produits par `tools/keygen` d'Astroport.ONE ; le rituel complet est la reprise de `Astroport.ONE/tools/Ylevel.sh`.

L'ancienne clé SSH n'est **pas détruite** : elle est rangée en `~/.ssh/origin.key` et `origin.pub`.

### Vérifier le lien

```bash
astroportd ylevel            # affiche SSH ↔ PeerID IPFS ↔ Ğ1 ↔ Nostr
astroportd ylevel --verify   # code de sortie 1 si la clé SSH ne correspond pas à l'identité de la station
```

La preuve est arithmétique : la clé publique SSH `ssh-ed25519 AAAA…` contient les 32 octets de la clé publique ed25519 ; le PeerID `12D3Koo…` est l'encodage de ces **mêmes** 32 octets. L'outil `Astroport.ONE/tools/ssh_to_g1ipfs.py` fait la conversion dans les deux sens. Le champ `SSHPUB` du `12345.json` d'une station publie cette preuve.

### À savoir avant de lancer Y-Level

- Votre **clé SSH publique change**. Mettez la nouvelle à jour partout où l'ancienne est enregistrée.
- `--no-ssh` : n'y touche pas (le lien SSH ↔ IPFS n'est alors pas établi).
- `--kubo-config` : aligne aussi l'identité d'un démon IPFS Kubo local.

---

## 5. Les Capitaines qui se suivent : individus ↔ Astroport ↔ territoire

C'est ce qui relie les constellations entre elles, sans annuaire central.

1. **Y-Level** donne à chaque station une identité unique, vérifiable : `IPFSNODEID` = clé SSH.
2. Chaque station publie ses rapports économiques (événement Nostr **kind 30850**).
3. Un Capitaine **suit** les autres Capitaines qu'il découvre dans ces événements. *(Astroport.ONE : `DRAGON_p2p_ssh.sh` ; règles décrites dans `AUTOfollow.rules.md` : le Capitaine suit ses passagers, les passagers suivent leur Capitaine, les Capitaines d'un même essaim se suivent.)*
4. Comme la clé SSH d'un Capitaine **se déduit de son `IPFSNODEID`**, ce suivi suffit à savoir quelles clés SSH sont celles de machines de confiance : accès SSH pair à pair, pour le support technique mutuel. *(Astroport.ONE.)*
5. Le **transport** de cet accès est celui d'astroportd : un tunnel libp2p `/x/<service>-<PeerID>` entre les deux stations, sans port ouvert sur la box de l'hôte, avec une garde par flux et un watchdog (*astroportd* : `astro-p2p`, `src/serve.rs`).

### Ce qu'astroportd fait aujourd'hui

```bash
# L'hôte expose son SSH dans le maillage :
astroportd serve ... --p2p-listen /ip4/0.0.0.0/tcp/4101 --p2p-expose ssh=127.0.0.1:22 --swarm-key ./swarm.key

# L'autre Capitaine le joint depuis un port local :
astroportd serve ... --p2p-forward 127.0.0.1:2222=<PeerID de l'hôte>/ssh
ssh -p 2222 utilisateur@127.0.0.1
```

- Tout service exposé par `--p2p-expose` passe par une garde de flux. Pour l'instant c'est une garde provisoire qui **refuse tout** (`src/tunnel_gate.rs`) : la politique de tunnels du Capitaine la remplacera. Le partage d'un Brain GPU entre Capitaines relève de l'entraide, pas d'un paiement.
- `astroportd route <service>` choisit le meilleur nœud de l'essaim (Power-Score, distance, tirage).

### Ce qu'astroportd ne fait pas encore

- **Le suivi automatique** des Capitaines (kind 30850 → suivre) et la **mise à jour automatique** de `authorized_keys` restent dans *Astroport.ONE*.
- Le tunnel transporte des octets : **c'est `sshd` qui autorise**. Ne l'ouvrez en réseau **ORIGIN (ouvert à tous)** que si `PasswordAuthentication no` et que les clés autorisées sont celles que vous avez choisies.

---

## 6. FaceCloud et partages N¹ / N² (aperçu)

![FaceCloud](img/facecloud-partage.svg)

- **N¹** : les personnes que vous suivez *et* qui vous suivent (amis réciproques). **N²** : les amis de vos amis.
- FaceCloud **partage automatiquement** les photos d'un ami reconnu avec cet ami s'il est du cercle N¹ réciproque : copie dans `/Partages/De_<nom>/` s'il est hébergé sur la même station, sinon message de fichier chiffré (NIP-17 emballé en NIP-59) vers sa station (*astroportd* : `astro-facecloud`, `pipeline.rs`).
- Pour le reste du monde, un **lien secret** `/s/<slug>#clé` : la clé est dans le fragment de l'adresse, que le navigateur n'envoie jamais au serveur (*astroportd* : `astro-dav`, `share.rs`).
- Les **vidéos** (copie privée) ne sont livrées qu'à l'auteur et à son N¹, jamais à tout le cercle N².
- Le relai Nostr réserve ses contenus privés au cercle N¹/N² authentifié (NIP-42).

La reconnaissance faciale passe par un service HTTP (`--face-engine`) ou par une inférence locale (`--face-models`, voir le guide) ; aucun modèle n'est embarqué dans le binaire.

### Synchronisation N¹/N² et réplication

Une station en marche tire en tâche de fond (`--crawl-every`, 900 s par défaut ; `--no-crawl` pour la couper) les profils et événements publics de ses membres, de leurs amis (N¹) et des amis de leurs amis (N²), depuis les relais des autres stations de la constellation. Ces relais se joignent **par le maillage P2P** (le tunnel `/x/strfry-<PeerID>`, le même que celui de DRAGON) : aucun port public n'est nécessaire. Quand un pair annonce aussi une adresse publique (`myRELAY` dans son `12345.json`), elle ne sert que de **repli** après un échec de connexion, et l'écran « Réplication » dit par quelle voie chaque station a été jointe.

L'écran **Réplication** de l'interface montre, pour vos derniers événements et vos derniers fichiers, **où** ils existent aussi (quelles stations) et **par qui** (vos proches, vos proches de proches, ou d'autres stations). Une station qui ne répond pas est marquée « injoignable », jamais comptée comme « 0 copie ». La mesure des fichiers demande `--kubo` (recherche des fournisseurs d'un CID).

---

## 7. Résumé : qui fait quoi

| Fonction | astroportd 0.2.0-beta | Astroport.ONE |
|---|:---:|:---:|
| Rituel Y-Level (SSH → IPFS → Ğ1 → Nostr) | ✔ `init`, `ylevel` | ✔ `Ylevel.sh` |
| Réseau privé `swarm.key` (ẐEN), réseau ouvert (ORIGIN) | ✔ | ✔ |
| Tunnels P2P entre stations (dont SSH) | ✔ | ✔ `DRAGON_p2p_ssh.sh` |
| Suivi automatique entre Capitaines (kind 30850) | — | ✔ |
| Création d'un MULTIPASS | ✔ `multipass create`, page `/register` (version allégée) | ✔ `make_NOSTRCARD.sh` (complet : SSSS, QR, zine, ZenCard) |
| uCloud chiffré, FaceCloud, liens secrets | ✔ | ✔ (version bash/Python) |
| Relai Nostr avec pare-feu social N¹/N² | ✔ intégré | ✔ strfry + filtres NIP-101 |
| Économie Ẑen (3×1/3, Love Ledger) | ✔ | ✔ |
