## astroportd 1.2.0 — PAF par machine, alerte « nouvelle machine », versements de Ẑen

Les versions 1.1.0 et 1.1.1 n'ont pas été publiées à part : cette version les reprend (voir plus bas).

- **PAF propre à chaque machine** : `paf = 9` dans `station.conf` (Ẑ par semaine, **par paliers de 3**, 9 par défaut). La station règle chaque semaine 3×PAF : 1×PAF au NODE, 2×PAF au Capitaine. `--weekly-costs` l'emporte s'il est donné. Chaque plan d'un processus `planes` a sa PAF.
- **Estimation** (`astroportd init`, option `--paf`, et carte « 💶 PAF de cette machine » du cockpit : `GET`/`POST /api/v1/station/paf`) : énergie (puissance mesurée par le plugin `energy`, sinon supposée d'après le profil) + capacité (Power-Score), arrondie au palier de 3. Un régulateur solaire (plugin `victron`) **hausse** la PAF : +3 Ẑ par tiers de la consommation du mois couverte (+6 au plus), et le **🎯 Bingo résilience** ajoute +3 Ẑ quand la production dépasse la consommation. Proposé, jamais appliqué seul.
- **Balise 30850** : nouvelle étiquette `paf:machine` ; l'annuaire expose `paf_cents`.
- **Alerte « nouvelle machine »** : toutes les 2 minutes, la station compare l'annuaire de son UPlanet aux machines déjà vues et prévient le Capitaine, une fois par machine (jamais au premier passage d'une base). Elle n'admet personne.
- **`planes`** reprend la position (`lat`, `lon`) de `station.conf` quand la commande n'en donne pas.
- **Versements de Ẑen** par le Capitaine et par les membres, **synchronisation OpenCollective → Ẑen** avec contrôle de collision, activation du MULTIPASS vide par le portefeuille maître, carte « Déposer » (adresse, QR) dans Constellation : voir le guide, sections « Les Ẑen » et « Relier une UPlanet à OpenCollective ».
- Le relai accepte les clés de station et de la coopérative dans son cercle social (Ğ1Billet : « author is outside the social circle »).

## astroportd 1.1.1 — correctif de CI

- Le test du relais d'appel qui écoute une nouvelle adresse de la machine (`127.0.0.2`) est ignoré sous macOS, dont la boucle locale ne porte que `127.0.0.1`. Aucun changement de comportement.

## astroportd 1.1.0 — images publiques adressées par leur contenu

Avatars, bandeaux et images envoyées depuis un client Nostr deviennent des **blobs publics en clair, adressés par leur CID** : n'importe quelle station amie les sert, et les rapatrie chez ses pairs si la station d'origine s'éteint.

- **`GET /ipfs/<cid>`** sert toute image inscrite dans la nouvelle table `public_blobs`, même passerelle chiffrée (`--ipfs-gateway`) coupée. Un CID inconnu est demandé aux pairs (8 Mo au plus, 10 s, empreinte vérifiée) et n'est gardé que si ses octets sont ceux d'un JPEG, d'un PNG ou d'un WebP ; le reste est retiré du disque. Les blobs chiffrés de l'uCloud ne sont jamais exposés par ce chemin.
- **Pare-feu des CID inconnus** : 4 rapatriements simultanés, 60 par minute (429 au-delà), un CID introuvable n'est pas redemandé pendant 2 minutes.
- **Profil** : `picture` et `banner` valent `<station>/ipfs/<cid>` ; le kind 0 porte aussi `picture_cid` et `banner_cid`. Les billets lisent le fond et l'icône par le CID, chez les pairs si besoin.
- **Envoi NIP-96** (`POST /api/upload/image`) : une seule copie, publique ; `url` = `<station>/ipfs/<cid>`, plus `cid` dans la réponse. Les nouveaux envois ne vont plus dans `/Public/uploads`.
- **Quota** : les images publiques d'un compte entament son quota d'uCloud (`vfs::used_bytes`, tableau de bord compris) ; l'inscription échoue en SQL quand il est atteint.
- **Mon compte › Moi › « Mes images publiques »** : liste, adresse à copier, retrait (`GET /api/v1/account/images`, `DELETE /api/v1/account/images/<cid>`). Une image en service comme photo de profil ou bandeau ne se retire pas.
- **Retraits** : la route `/api/v1/profile/<clé>/<picture|banner>.jpg` et la table `profile_images`. **Les profils déjà publiés avec l'ancienne adresse ne se chargent plus : le membre renvoie son image** pour obtenir un CID.
- Versement de Ẑen par le Capitaine et synchronisation OpenCollective (`zen_credit`, `oc_emission`) : voir le guide.

## astroportd 1.0.0 — fin des ponts avec Astroport.ONE

Aucun compte existant n'est repris : ces retraits **cassent la lecture des anciennes données**.

- **Retirés** : la commande `astroportd import-ucloud` ; les CID v0 `Qm…` (lecture Kubo, épinglage, passerelle `/ipfs/`) ; la lecture du format **UENC v1** (seul UENC v2 est lu et écrit) ; la migration des anciennes invitations ; les anciens emplacements `nostr/.moved/` et `nostr.ejected/` (la corbeille `trash/` les remplace).
- **`KeyFormula::Legacy`** (clés d'avant l'isolation par UPlanet, `(SALT, PEPPER)` bruts) est retirée : ces comptes ne sont plus retrouvés par `multipass recover` ni par la carte QR. `raw` produit les mêmes clés pour qui en a besoin. La contrainte SQL `key_formula` n'accepte plus `legacy`.
- **UPassport / `tools/uenc_codec.py`** écrit et lit désormais l'UENC v2 (même format que astroportd, vérifié dans les deux sens) ; `UENC_OVERHEAD` passe de 64 à 8192 octets.
- **Pièces jointes de messages** (`/api/v1/attachments`) : chiffrées côté client en UENC v2 (coracle, zelkova via UPassport) ; l'hébergeur exige l'en-tête v2 et ne déchiffre jamais. Une pièce jointe v1 déjà envoyée n'est plus lisible.

## astroportd 0.6.0-beta

Cette version **soigne l'installation** : un binaire qui tourne sur Debian 12 et Ubuntu 22.04, une station déjà en niveau Y qui n'est jamais redérivée, une constellation reprise depuis kubo, un résumé réseau adapté à chaque cas, et des plugins proposés à l'installation.

### Installer
- **Binaires gnu compilés sur Ubuntu 22.04** (glibc 2.35) : ils démarrent sur Debian 12, Ubuntu 22.04 et plus récent. Avant : `GLIBC_2.38 not found` sur une Debian 12.
- `install.sh` choisit seul le **binaire statique** si la glibc de la machine est plus ancienne que la 2.35, et **s'arrête** si le binaire installé ne démarre pas, au lieu de lancer `init` avec un binaire inutilisable.
- **Plugins** : `install.sh` propose de présenter les plugins à la fin. `install-plugins.sh` (dépôt `astroportd-plugins`) les présente, fait le `git clone`, puis lance l'`install.sh` de chacun ; `--list`, `--all`, `--yes` ou des noms.

### Identité et constellation
- **`init` lit `~/.ipfs/config`** (ou `$IPFS_PATH`) sans qu'on le lui demande : une station dont la clé SSH est déjà la jumelle de l'IPFSNODEID n'est plus redérivée. `astroportd ylevel` vérifie aussi avec ce fichier.
- **`~/.ipfs/swarm.key`** : si elle existe et qu'aucune constellation n'est choisie, `init` propose de reprendre cette constellation ẐEN (reprise d'office avec `--yes`).
- **ORIGIN = candidature** : le texte de l'assistant explique que la machine est proposée au collectif, et que le Capitaine, une fois inscrit et reconnu, reçoit la `swarm.key` (`astroportd plane-setup add`, ou `init --join`).

### Réseau
- Nouvelle étape ④ de l'assistant : **ce qu'il faut ouvrir sur la box** selon le cas (HTTPS direct 80/443, derrière un reverse proxy, sans domaine), le maillage (le port 4101 n'a pas besoin d'être ouvert : relais et « hole punching ») et la visio (UDP).

## astroportd 0.5.8-beta

Cette version **recentre astroportd sur son rôle de serveur** : il garde, chiffre et partage les fichiers (WebDAV, P2P, N¹, N²) et **ne traite plus aucun média**. Tout le travail sur les films, séries et musiques passe dans le plugin `media`. Les **pièces jointes** des messages ont leur propre chemin, vérifiable et répliqué chez les amis.

### Pièces jointes des messages
- **Dépôt** : `POST /api/v1/attachments` (NIP-98) reçoit un fichier **déjà chiffré** par le client (UENC v1). Plafond de **20 Mo** et de 90 jours.
- **Lien servi par n'importe quelle passerelle** : l'adresse est le CID du contenu ; le destinataire le vérifie après téléchargement. Une pièce jointe ne peut pas être supprimée par un rejeu ni par la date d'expiration d'un tiers.
- **Copie chez les amis** : la station envoie la pièce jointe, en protocole de blobs P2P (`/astro/blob/1.0.0`), aux stations qui hébergent la part P3 du membre (3 au plus). Le plafond de réception est de 20 Mo, vérifié avant le premier octet reçu.
- **coracle** : envoi et affichage des images chiffrées par ce chemin ; les messages vocaux sont retirés.

### Médias : tout est dans le plugin
- **Retirés de la station** : la commande `astroportd media`, le pipeline `ffprobe`/`ffmpeg` et son branchement dans `serve`, la carte « Copie privée » de l'accueil et les routes `/api/v1/consent/private-copy`.
- **Plus de lecture de l'attestation** : la station ne lit plus le kind 31903 `d = private_copy` ; la table `private_copy_consent` disparaît du schéma (projection rejouable : rien à migrer).
- **Plugin `media`** (dépôt `astroportd-plugins`) : sonde, réencode, télécharge (`yt-dlp`), interroge TMDB, extrait les sous-titres, écrit par WebDAV dans `/Vidéos/Films`, `/Vidéos/Séries/<série>/Saison NN`, `/Vidéos/Autres`, `/Musique/<artiste>/<album>`, et signe puis publie lui-même l'événement vidéo (kind 21 ou 22) avec la clé du membre.
- **Doublon** : refusé avant tout réencodage. **Copie privée** : l'attestation est posée automatiquement, en local, au premier dépôt ; rien n'est publié.

### À savoir en mettant à jour
- `astroportd media` n'existe plus : installer le plugin `media`.
- Les tests d'interopérabilité (`tests/plugin_media_interop.rs`) vérifient que les événements signés par le plugin sont acceptés par la station.

## astroportd 0.5.7-beta

Cette version aide les futurs membres à **trouver une station** : chaque station publie une balise, et un annuaire les envoie vers la plus proche qui a encore de la place. La station fonctionne aussi **derrière un reverse proxy** et affiche son **énergie**. La **documentation publique** est refondue : une page d'accueil pour le grand public et les futurs Capitaines, et un guide du Capitaine en quatre parties.

### Trouver une station proche
- **Balise complète** (kind 30850, `d = economic-health`, signée par le NODE) : places libres, ouverture des inscriptions, position arrondie à la maille UMAP (≈ 1 km, seulement si `--lat`/`--lon` sont réglés), matériel, prix du MULTIPASS et adresse publique. Les noms de tags restent ceux d'`ECONOMY.broadcast.sh`.
- **Quand elle est publiée** : au démarrage, à chaque changement du nombre de places (au plus toutes les 2 minutes), toutes les 6 heures et après la nuit de 20h12.
- **Où elle est publiée** : sur le relai de la station, sur les relais `--relay` et sur le relai commun de la coopérative `wss://relay.<OVH_ZONE>`.
- **Annuaire** : la page `/stations` et `GET /api/v1/stations` (lisible depuis toute origine) listent les stations de la même UPlanet vues depuis moins de 8 jours. Avec « Me localiser », la position du visiteur reste dans son navigateur ; la station conseillée est revérifiée en direct.
- **Inscriptions** : une station complète ou fermée renvoie vers l'annuaire depuis `/register`.
- **Relai commun** : le relai accepte la balise d'une station inconnue seulement si elle porte le `swarm_id` de son UPlanet et un PeerID, avec un débit limité. Le reste du relai reste fermé.

### Derrière un reverse proxy
- **Détection par `astroportd init`** : quand les ports 80/443 sont déjà pris, la station écoute sur `127.0.0.1:8090` et demande le domaine que sert le proxy (`--public-domain`). La balise, les liens envoyés aux membres, NIP-98 et le relai annoncé utilisent ce domaine.
- **Adresse des visiteurs** : quand la connexion vient du proxy (boucle locale ou réseau privé), la station la lit dans `X-Forwarded-For`, dont seule la dernière adresse est crue. Les limites par adresse visent ainsi les visiteurs, pas le proxy.

### Énergie de la station
- **Lecture du plugin `energy`** (dépôt `astroportd-plugins` : consommation avec PowerJoular, production solaire avec les régulateurs Victron) : la station lit le fichier `energy/v1` qu'il dépose et n'exécute jamais son code.
- **Affichage et diffusion** : carte « Énergie de la station » dans le cockpit, `GET /api/v1/station/energy`, et tags `hw:energy_*` / `hw:solar_*` dans la balise.
- **Fichier périmé ou invalide** : un fichier de plus d'une heure ou hors contrat est ignoré, et le cockpit le signale.

### Documentation
- **Nouvelle page d'accueil** (`docs/index.html`) pour le grand public et les futurs Capitaines :
  - une promesse et un schéma de la constellation ;
  - un comparatif avec Google Photos et iCloud ;
  - deux parcours : rejoindre une station, ouvrir la sienne ;
  - la clé de secours en trois morceaux ;
  - « Où va l'argent » : 1×PAF à l'Armateur, 2×PAF au Capitaine, puis le surplus en 3×1/3.
- **Guide du Capitaine** (`docs/guide.html`) :
  - quatre parties : mettre en service en 9 étapes, ce que la station offre aux membres, l'argent, relier et administrer ;
  - les détails techniques sont repliés ;
  - les anciennes adresses `…/#etape-N` mènent toujours à la bonne section.
- **Constellations et Mac réécrits.** Corrections : le SSH est fermé par défaut en ẐEN et ouvert aux Capitaines amis en ORIGIN, et astroportd tient lui-même son bloc dans `authorized_keys`.
- **Charte** : le texte, signé par les Capitaines, est inchangé ; sa page web explique désormais ce qu'il est.

## astroportd 0.5.6-beta

Cette version rend l'interface **plus simple à prendre en main**, pour le membre comme pour le Capitaine, et transforme **« J'appelle un ami »** : le membre qui a perdu sa station prévient ses gardiens, l'ami confirme d'un geste après l'avoir appelé, et le compte renaît tout seul, sans QR à lire au téléphone. Elle ouvre aussi le **maillage entre Capitaines amis** (stations ajoutées à chaud, services partagés à la demande) et rend l'économie plus sûre (primale vérifiée par deux indexeurs, intrusions corrigées).

### « J'appelle un ami » sans QR : le secours par les gardiens
- Sur la nouvelle station, `/restore` : le membre scanne sa **carte imprimée** et appuie sur **« Prévenir mes gardiens »**. La station retrouve ses contacts sur le réseau (le crawl va chercher sa clé, réveillé tout de suite) et leur envoie une demande, **depuis sa clé NODE**.
- Seule la station d'un ami qui garde sa part y donne suite ; les autres contacts ne voient rien. L'ami est prévenu (accueil › *Pour vous*, *Mon compte › Ma sécurité*, message de BRO) ; il appelle, coche **« je l'ai reconnu(e) »** et confirme.
- Sa part arrive **directement** sur la nouvelle station, qui l'assemble à la carte : personne ne lit ni ne recopie rien. La page de restauration attend (relevé toutes les 10 s) et rétablit le compte d'elle-même ; la demande vit 24 h.
- Même sécurité qu'au téléphone (la carte + l'accord de l'ami) ; débit borné (3 demandes par jour et par compte, 10 comptes par heure et par station) ; un refus (« ce n'est pas lui ») **alerte le Capitaine** de l'ami. L'ancienne méthode reste en repli.
- Boîte d'envoi : une station d'accueil inconnue est cherchée dans l'essaim (seule la bonne station accepte l'enveloppe), et une clé NODE est routée vers sa station. Crawl : clés demandées par un module (`crawl_wanted`, avec expiration) et réveil immédiat.

### Membres : moins de technique, plus de gestes
- **Inscription** : l'écran final ne montre plus que l'identifiant, le **code PASS en grand** et la **carte de secours** (« 3 morceaux, 2 suffisent ») ; les clés sont repliées dans « Détails techniques ». L'impression ne porte plus ni identifiant ni PASS : une feuille volée n'ouvre pas le compte.
- **Mon compte en trois onglets** : *Moi*, *Mes appareils*, *Ma sécurité* (lien direct `#/account?tab=…`). *Ma sécurité* explique **« Si je perds tout »** pas à pas.
- **Accueil « Pour vous »** : ce qui attend un geste aujourd'hui (un ami en secours, des photos à confirmer avant envoi, des visages à confirmer ou à nommer), à un tap.
- **Trier en grand au pouce** : glisser la photo → oui, ← non.
- **Visio** : passage automatique en **voix seule** quand plus de 15 % des paquets se perdent pendant 5 s (une fois par appel ; 📻 rend l'image).
- **Quartier** (facultatif, à l'inscription) : la station ne garde qu'une position approchée (maille UMAP, ≈ 1 km).

### Capitaine
- Cockpit : un bouton **« Débloquer »** par PASS verrouillé ; le carburant devient une **« Réserve de démarrage »** en jauge, l'adresse et le QR repliés.
- **Espace Capitaine en quatre onglets** (Cockpit, Essaim et pairs, Économie, Options et tunnels).
- **Maillage et Capitaines amis** (`#/mesh`) : maillage actif par défaut, stations ajoutées **à chaud** (gardées ou non comme bootstrap), Capitaines suivis ou non, et pour chaque ami réciproque les services **que sa station nous ouvre** (question posée par le maillage, rien n'est publié) avec « Se connecter ».
- **Partage à chaud de services** de la machine (Options › Tunnels : nom et port, machine ou réseau local seulement), par exemple un bureau à distance VNC/RDP pour un ami nommé.
- **Bail géographique** : une seule station d'une UPlanet tient les clés d'un quartier (UMAP, SECTOR, REGION) — la plus proche, le PeerID départageant.

### Économie
- **Primale vérifiée par deux indexeurs** : un désaccord ou un retard vaut « non vérifié », jamais un mouvement de fonds.
- **Correction des intrusions** : un virement reçu d'un émetteur à primale étrangère **prouvée** repart vers le portefeuille `INTRUSION` ; la vigilance (Économie) ne montre que ces émetteurs.
- **Transaction primale après preuve** : le 1 Ğ1 d'un MULTIPASS part à sa première connexion authentifiée ; un compte jamais connecté en 24 h peut être éjecté (fonds reversés, clés dans la **corbeille** de la station).

### Interface et performances
- Miniatures JPEG décodées à échelle réduite (≈ 1,5 Mo de mémoire au lieu de 36 Mo pour une photo de 12 Mpx).
- Boutons qui restaient grisés après une erreur (redémarrage, adresses DNS, relance de tâches) : corrigés.
- Budget des écrans : 144 Ko gzip (136 Ko avant cette version).

## astroportd 0.5.5-beta

Cette version rend la station **robuste** sur ses points sensibles (argent, sonnerie, mises à jour de l'application) et transforme **l'arrivée d'un Capitaine** : plus de terminal une fois la station démarrée. Elle inclut aussi la 0.5.4-beta, jamais publiée (Personnes et Contacts réunis, visio sans tiers).

### Robustesse
- **Personne ne s'inscrit avant le Capitaine** : tant que la station n'en a pas, `/register` et l'inscription invitée sont fermés (un inconnu ne peut plus prendre l'email du Capitaine). Les membres suivent le Capitaine même s'il est créé pendant que la station tourne.
- **Solde réellement dépensable** : toute dépense (réactions payantes, hébergement, clôture, transactions primales, billets) s'appuie sur le solde *transférable* de la chaîne (fonds gelés exclus), plus jamais sur le total.
- **Un virement à la fois par portefeuille** : deux signatures simultanées ne lisent plus le même nonce.
- **Sonnerie instantanée** : une enveloppe privée acceptée par le relai réveille le chat tout de suite (la poignée de main visio ne perd plus jusqu'à 6 s) ; le passage toutes les 3 s reste en secours.
- **Application installée (PWA) toujours cohérente** : le cache porte l'empreinte de tous les fichiers web ; une nouvelle version recharge la page au prochain écran (jamais pendant un appel).
- **Certificat HTTPS visible** : `/health` a un sous-système `https` ; un échec Let's Encrypt arrive au cockpit avec le geste qui le règle (box, DNS, quota, autre serveur sur le port 80).
- **Relais d'appel** : une nouvelle adresse de la machine (DHCP, Wi-Fi, VPN) est écoutée sans redémarrer.
- **Invitations de l'ancien format** migrées au démarrage : elles suivent le uCloud vivant (un fichier supprimé n'est plus promis).

### Arrivée du Capitaine
- `astroportd init` **crée le compte du Capitaine** quand on lui donne l'email, et affiche son code PASS.
- Sans email : la station écrit un **code d'installation** dans son journal ; la page **`/welcome`** crée le compte du Capitaine avec ce code (usage unique, renouvelé après 5 essais faux).
- **Carte « Carburant de la station »** (avec `--primal-init`) : QR et adresse du portefeuille maître, montant exact qui manque.
- **Nom de domaine depuis le cockpit** : vérification (forme, DNS, ports), enregistrement dans `station.conf`, bouton **« Redémarrer la station »** (systemd la relance). Filet : si les ports 80/443 sont indisponibles, la station reste joignable en local et le cockpit dit pourquoi.
- **Configuration coopérative en trois niveaux** : Essentiel (nom, charte, OVH), Services (OpenCollective, Mailjet, API), Réglages experts repliés (fiscalité, parts, 3×1/3).

### Faire venir du monde
- **Liens d'accueil** `/w/<jeton>` (cockpit › « Inviter votre premier membre ») : un lien et son QR pour **plusieurs** personnes. Qui l'ouvre crée son MULTIPASS (email ou simple pseudo), devient ami réciproque du Capitaine et arrive dans la discussion avec lui. Places limitées, 90 jours, retirable.
- **Station marraine** : `astroportd parrainer` donne un jeton à usage unique (24 h) ; `astroportd init --join <jeton>` rejoint la constellation (swarm.key reçue **chiffrée**, amorçage P2P sur la marraine), sans recopier de fichier.
- Chat : **brouillons** gardés le temps de l'onglet ; visio : bouton **« Voix seule »** (la vidéo se coupe dans les deux sens, l'appel tient en 3G).

### Installation
- **`install.sh` relancé met à jour** : il montre l'installation en place (version, constellation, domaine, Capitaine, service), met à jour le binaire et les modèles déjà choisis, redémarre le service, et ne repropose jamais `init`. `--force` réinstalle la même version.

## astroportd 0.5.4-beta

Cette version réunit **Personnes et Contacts** (un visage appartient à une fiche, plus jamais à un nom), apporte la **visio sans tiers** (appels à deux et en groupe, relais d'appel de la station, présence « en ligne ») et allège le premier chargement de l'interface.

### Personnes : fiches et visages réunis
- Un seul écran **Personnes** (menu allégé, `#/contacts` reste un alias) : en tête, le sas **« N visages à ranger »** mène à **Trier en grand** (plein écran, une question à la fois : « Est-ce Léa ? », « Qui est-ce ? », raccourcis O, N, Espace, Échap) ou à la table de tri. Chaque fiche montre ses visages ; un clic ouvre la **photo d'origine, le visage entouré**.
- **Lien par UID vCard** (`faces.contact_uid`, migration automatique) : deux homonymes sont deux personnes, renommer une fiche renomme ses visages, supprimer une fiche les délie. Une fiche sans UID en reçoit un. Les visages d'avant sont rattachés une fois, par leur clé ou par un nom qu'une seule fiche porte. Si plusieurs fiches portent le nom tapé, l'interface demande laquelle.
- **Fusion des doublons exacts** en un geste (même nom, société, emails, téléphones), par lots, avec aperçu, progression et arrêt ; rien n'est perdu.

### Visio sans tiers
- Bouton 📹 dans Messages et sur une fiche reliée ; carte d'appel sur tout écran (sonnerie, vibration, titre qui clignote, notification système sur option), écran d'appel au pouce (Micro, Caméra, Raccrocher), « Appel manqué » + Rappeler dans la conversation, « Appel pris sur un autre écran ».
- **Appels de groupe** jusqu'à 4 (maillage de navigateur à navigateur) : l'hôte ajoute des proches en cours d'appel ; un invité n'est admis par la station d'un autre que dans la salle ouverte par un ami.
- Signalisation chiffrée comme un message privé (NIP-59, enveloppe qui expire) ; seul un proche fait sonner.
- **STUN et relais d'appel (TURN)** servis par la station en UDP sur le port de l'interface, réservés aux membres et à leurs amis N¹, relais **uniquement entre appels de la station** (jamais un relais ouvert), une prise par adresse locale. Options `--stun-listen`, `--no-turn`, `--no-stun`, `--ice-server`.
- **Présence** sur option (Mon compte → Visio) : vos amis réciproques voient « en ligne » ou « en appel » ; battements NIP-101 (kind 24315) vers les autres stations ; rien n'est stocké.
- La visio demande une adresse `https` (le navigateur l'exige pour la caméra) ; l'écran le dit.

### Interface
- **Traductions par écran** : le dictionnaire chargé au démarrage passe de 12,5 à 5 Ko ; les phrases d'un écran viennent avec lui. Texte « compte invité » de nouveau traduit.
- Jargon retiré des écrans des membres (NIP-65, kind 0, NIP-05) ; l'index A–Z du carnet ne masque plus la liste sur téléphone.
- Budgets : coque < 20 Ko, écrans < 128 Ko au cumul (toutes les parts d'une langue comprises), chaque écran < 12 Ko.

### Station
- `init` ajoute `--kubo` quand l'uCloud repris d'Astroport.ONE en dépend (ORIGIN seulement) et `serve` le signale s'il manque ; `serve` refuse `--kubo` avec une `swarm.key` (étanchéité des constellations).
- Historique des portefeuilles : les virements repris de la Ğ1 v1 (blocs négatifs) sont lus.
- Mon compte ne plante plus à l'ouverture.

## astroportd 0.5.3-beta

Cette version donne à chaque UPlanet un **nom, une charte et un argent commun visible**, ajoute les **Ğ1Billets** (vérifier, encaisser, créer), un **site public** par membre et plusieurs outils pour le Capitaine.


### Ğ1Billet : vérifier et encaisser (page publique `/scan`)
- Page **`/scan`** sans compte : scanner le billet à la caméra (`BarcodeDetector`, ou jsQR chargé à la demande), dire où envoyer la valeur, encaisser. Les deux QR se lisent **dans n'importe quel ordre**. Routes publiques `GET /api/v1/billet/check`, `/destination` et `POST /identify`, `/redeem` (limites de débit par adresse, réponses `no-store`).
- Circuit legacy conservé : billet → `UPLANETNAME_G1` → destinataire (`solde − 1 Ğ1`), billet et destinataire vérifiés « de cette UPlanet » par le primal. Découpage Shamir compatible avec `billet_gen.sh` (vecteur de test tiré de l'algorithme). État écrit avant chaque virement, reprise toutes les 5 min d'un transfert final échoué, alerte au Capitaine.
- Date de fin d'un billet (événement kind 31903 `d = billet:<adresse>` signé par la coopérative) : passée, le porteur ne l'encaisse plus.
- Budget : la page `/scan` et le décodeur QR ont chacun le leur, hors de la coque de l'application.
- **Création de billets** : écran « Mes Ğ1Billets » pour le Capitaine et les MULTIPASS qu'il autorise (dans la même action que le quota et le prix en ẑ/semaine). Planche A4 paysage de 6 billets, valeur en ẑ, durée de validité, financement billet par billet (activation du maître puis portefeuille de l'émetteur, nonce réessayé). Option **retour automatique** : P2 et P3 gardées chiffrées pour le NODE dans le témoin ; à la date de fin la valeur revient à l'émetteur. `/scan` traduit en fr/en/es.
- Réglage de l'hébergement dans un **panneau de la page** (plus de fenêtres de saisie) : quota, prix en ẑ/semaine et droit de créer des Ğ1Billets. Retour automatique des billets expirés **une fois par jour à 20h12** (étape « billets » de la nuit) ; la reprise d'un encaissement à terminer reste toutes les 5 minutes.
- **Images de profil** : envoi d'une image ou d'un **bandeau** depuis Mon compte (réduit, réencodé, servi par la station, kind 0 `picture`/`banner`) ; ils deviennent l'icône et le fond des Ğ1Billets.
- **Ma UPlanet** : suivi des Ğ1Billets (créés, en circulation et valeur, encaissés, renvoyés, expirés) et point « billets » sur la carte animée.
- **Site public** : le dossier `/Public` de l'uCloud est servi en clair, sans authentification, à `/site/<nom>/` et à `https://<nom>.<zone>/` (mêmes noms que les adresses DNSLink). Publier = déplacer un fichier dans `/Public`. Page par défaut (profil + fichiers), site d'exemple en un clic dans Mon compte, politique `sandbox` sur chaque réponse, `..` et fichiers cachés refusés.
- Site public : bouton **Publier / Retirer de Public** dans l'explorateur (déplacement avec confirmation dans la page), **image de profil choisie dans /Public**, **compteur de visites** sans adresse ni identifiant de visiteur, et **redirection vers la station qui héberge** un site quand le DNS (tourniquet) mène à une autre (adresse annoncée par chaque station dans son résumé de santé, tag `url`).
- `/scan` : connexion par email et PASS (iPhone dont l'application installée a sa propre session) pour le raccourci « Mon compte ».
- Essais réels sur la Ğ1 en ORIGIN : `tests/billet_live.rs` (ignoré, `ASTRO_LIVE_G1`).

### Ma UPlanet : portefeuilles, charte et carte animée
- Écran **Ma UPlanet** (`#/constellation`) visible de tous les membres : portefeuilles de la coopérative (`UPLANETNAME_G1` et ceux de la transaction primale) avec soldes Ğ1/Ẑen et état primal, charte signée par la coopérative, et **carte animée en SVG** (impulsions dans des tuyaux, mode « rejouer » l'historique). Routes `GET /api/v1/constellation` et `/api/v1/constellation/ledger`.
- L'historique des virements est lu chez l'indexeur squid (table `coop_ledger`, toutes les 15 min) ; sans indexeur : « historique partiel ». Aucune adresse de membre n'est rendue.
- **Acceptation de la charte par les Capitaines** : événement Nostr public kind 31903 `d = charter-accept` (empreinte SHA-256 du texte, version, clé de la coopérative), signé de la clé du Capitaine ; une nouvelle version demande une nouvelle signature. Route `POST /api/v1/constellation/charter/accept` (Capitaine).
- Configuration coopérative : nouvelle section « Nom et charte de l'UPlanet » (`UPLANET_NAME`, `CHARTER_VERSION`, `CHARTER_TEXT`) ; charte type `docs/CHARTE_UPLANET.md`.
- Budget de l'interface : écrans à la demande portés à 240 Ko bruts / 92 Ko gzip.
- Guide : CONSTELLATIONS, MACOS, la charte et le manuel sont publiés en HTML.

## astroportd 0.5.2-beta

Cette version fait calculer les **vecteurs de la mémoire de BRO dans la station**, sans Ollama, et rend l'installeur **adaptatif**.

### Embeddings locaux (bge-m3 int8)
- Avec la fonctionnalité `onnx`, `serve` calcule les embeddings de la mémoire avec `bge-m3` quantifié (ONNX Runtime chargé à l'exécution, tokeniseur `tokenizers`), 1024 dimensions, pooling CLS. Les deux fichiers (`bge-m3-int8.onnx`, `bge-m3-tokenizer.json`) sont découverts dans `~/.zen/astroportd/models/` ; options `--embed-models DIR` et `--embed-threads N`. Sans eux, ou sans `libonnxruntime`, la station se replie visiblement sur Ollama (erreur franche si `--embed-models` est donné).
- Nouvelle commande `astroportd embed-check` : vérifie la fonctionnalité, les fichiers, la bibliothèque, charge le modèle et chronomètre une phrase.
- Les vecteurs locaux sont proches de ceux d'Ollama sans leur être identiques (écart non mesuré) : réindexer, ou garder un seul moteur. Ollama reste requis pour les résumés RÊVE.

### Installeur adaptatif
- `install.sh` détecte l'architecture, les cœurs, la RAM, l'espace libre, le type et la vitesse du disque, le GPU et Ollama, puis propose un profil **léger / standard / confort** (`--check-hardware`, `--profile`, `--embed/--no-embed`). `init` mémorise `face_threads` et `embed_threads` dans `station.conf`.

### Guide
- Relecture complète : étape 8 « Mémoire et recherche », section installeur, notes Termux (`| bash`) et Mac, encadré « Limites » de l'étape 5 recalé sur le code (clé de secours en trois parts et QR existent ; restent absents : import IPNS dans Kubo, zine par email, ZenCard, primo-transaction du compte), chiffres corrigés.

## astroportd 0.5.1-beta

Cette version donne un **cockpit** au Capitaine (un verdict, ce qui demande une action, six étages, l'essaim), une **mise en service guidée**, un **accueil simple** pour les membres (avec la synchronisation du smartphone en avant quand il n'y a pas d'assistant), des **adresses DNSLink** pour la station et chaque MULTIPASS, et une comparaison de code entre stations **retirée**.

### Mise en service du Capitaine
- Le cockpit propose, tant qu'elle n'est pas finie, une liste **« Mettre la station en service »** cochée d'après l'état réel : nom de domaine, maillage P2P, autres stations vues, adresses de la coopérative, premier membre invité. Pendant ce temps, les informations qu'elle redit ne sont pas répétées dans « À faire ». L'accueil du Capitaine affiche en tête une carte **⚓ Votre station** (état en un mot, lien vers le cockpit).

### Adresses de la coopérative (DNSLink OVH)
- La station **s'inscrit en tant qu'Astroport** et inscrit **chaque MULTIPASS hébergé** (Capitaine compris) dans la zone DNS OVH de la coopérative : `_dnslink.<nom>.<zone>` → `/ipns/<espace>`. Compatible avec `ovh.me.sh` d'Astroport.ONE (mêmes noms, mêmes cibles, TTL 0) : les deux outils se relaient sans doublon. Identifiants lus dans la configuration coopérative. Passage 90 s après le démarrage puis toutes les 15 min, **seulement pour ce qui a changé** ; aucune suppression ; échec dans le bilan, alerte unique au Capitaine ; carte *Adresses de la coopérative* dans le cockpit (« Enregistrer maintenant »). Nouvelle dépendance : `sha1`.

### Accueil des membres
- L'onglet **Accueil** (ex-« Tableau de bord ») devient simple : **trois gestes** (Mes photos, Mes proches, Mon assistant), une liste **Bien démarrer** cochée d'après l'état réel (premières photos, un visage reconnu, un proche, sauvegarde chez un ami acceptée, assistant branché) qui disparaît une fois terminée, une alerte **seulement** si l'espace dépasse 90 %, et — **sans assistant (pas de Brain)** — la synchronisation du **smartphone** mise en avant à la place de BRO (« Quitter Google et Apple : photos, contacts, agenda »). Le Capitaine y trouve un lien vers son cockpit. Solde, autonomie, loyer, Opex/Capex et copie privée sont à un clic : **Mes chiffres** (`#/chiffres`), inchangés.
- Règles testées par `crates/astro-ui/tests/home_steps.mjs` (CI). Correction : un écran qui recevait une section vide affichait le texte « null » (cockpit calme).

### Comparaison du code entre stations : retirée
- La colonne « Code » de la carte Constellation, l'avertissement correspondant et l'**attestation de build** (kind 30853, `src/build_attest.rs`) sont supprimés : le verdict (identique, empreinte différente, autre version) informait trop peu pour justifier un événement signé toutes les 6 h par station. Une station ne publie plus ni ne projette ce kind ; les événements 30853 déjà présents sur les relais sont simplement ignorés.

### Cockpit du Capitaine
- Nouvel écran d'accueil de l'onglet **Station** : un verdict (✅/🟠/🔴), **ce qui demande une action** (3 au plus, chacune avec son bouton et sa raison en clair), **six étages** avec voyant (Machine, Station, Constellation, Services et accès, Membres, Économie) et l'**essaim** de stations. Rafraîchi toutes les 30 s. Détecte : sous-système en panne, P2P arrêté, station seule, SSH ouvert à tout le réseau, service réglé qui ne répond plus, tâches abandonnées, MULTIPASS presque pleins, comptes verrouillés ou en période de grâce, trésorerie entamée. Les anciens écrans sont à un clic (*Détails techniques*).
- Budget des écrans à la demande relevé (200 → 216 Ko bruts, 76 → 80 Ko en gzip) ; le cockpit seul fait 4,6 Ko en gzip, sous la limite de 12 Ko par écran. Règles testées par `crates/astro-ui/tests/cockpit_rules.mjs` (CI).

### Corrections
- **Documentation sur les plans** : « une station = une constellation, raccorder ORIGIN et ẐEN est prévu » n'était plus vrai. `astroportd planes` sert ORIGIN et des UPlanet privées dans un seul processus (même identité Y-Level, un `serve` complet par plan : base, comptes, réseau P2P privé, port), avec routage par nom d'hôte (`--front`), `--plane` sur les commandes et sélecteur de plan ; seul le **miroir entre plans** reste à faire. Guide, README, CONSTELLATIONS, plan de conception et annonce corrigés. Captures d'écran de la vraie interface ajoutées au guide.
- **Interface** : les écrans *Plans* et *Configuration coopérative* affichaient le texte « null » ; le formulaire d'ajout de plan range maintenant chaque case à cocher devant son libellé.

### Options › Tunnels : seuls les services qui répondent ont une carte
- La station sonde (TCP, 600 ms) l'adresse locale de chaque service offert. Ollama ou ComfyUI absents n'ont plus de carte : une ligne « Non détectés : … » les nomme. SSH reste toujours visible ; un service réglé ou ayant des pairs nommés garde sa carte même arrêté, avec la pastille « ne répond pas ». Un port ouvert ne prouve pas qu'un modèle soit chargé : c'est un « démarré ? », pas une garantie.

## astroportd 0.5.0-beta

Cette version fait de la **clé de secours** un secret qu'on peut perdre sans danger, et ajoute l'**agenda** : « Agenda » du téléphone se synchronise avec la station, comme « Contacts ».

### macOS (bêta), crawl plus rapide, documentation recalée
- **macOS** : la station détecte la RAM par `sysctl hw.memsize` et compte la **mémoire unifiée** d'un Mac Apple Silicon comme VRAM (66 %, `ASTRO_VRAM_GB` pour corriger) : un Mac peut être un Brain GPU ; la bibliothèque ONNX Runtime `.dylib` est reconnue (Homebrew compris) ; le nom de la machine se lit aussi par `hostname`. Nouveau : [docs/MACOS.md](docs/MACOS.md), CI macOS et archives `aarch64-apple-darwin` / `x86_64-apple-darwin`, toutes **au mieux** (jamais compilées sur un Mac avant cette version).
- **Crawl N²** : les couples clé × relai se synchronisent **8 à la fois** (`CrawlConfig::concurrency`) au lieu d'un par un ; curseurs, filtres et contrôles d'auteur inchangés.
- **Documentation** : la garde de flux « provisoire qui refuse tout » (`tunnel_gate.rs`, disparu) est remplacée par la politique des tunnels ; la limite « une station = une constellation » dit maintenant que `astroportd planes` en sert plusieurs ; tableau des commandes complété.

### Clé de secours en 3 parts (remplace la carte complète)
- Le secret du compte (SALT et PEPPER) est découpé en **3 parts, dont 2 suffisent** (partage de Shamir). **P1** va au membre (QR imprimable `astro-share:v1|…`), **P2** reste sur la station (`<compte>/.ssss/2`, `0600`), **P3** est confiée d'elle-même, chiffrée pour chacun, aux amis qui hébergent la sauvegarde. Une part seule ne révèle rien : un QR volé ou photographié ne donne pas accès au compte.
- **« J'appelle un ami »** : sur la nouvelle station (`/restore`, ou `astroportd multipass recover -`), on saisit ou scanne P1 et la part que l'ami montre dans *Compte › Sauvegarde privée* (texte et QR). La station rétablie recalcule les trois parts d'origine : le P1 imprimé et le P3 des amis restent valables.
- `astroportd multipass card <email> [--part 1|2|3] [--svg]` réaffiche une part ; un compte plus ancien reçoit son découpage à la première demande.
- **Le SALT et le PEPPER ne sont plus affichés** à l'inscription (page, fichier `.txt`, texte du terminal ; `--json` les garde). Le `nsec` reste affiché (clients Nostr à extension de navigateur). L'**ancienne carte complète** (`astro-secret:v1|…`) reste acceptée à la restauration.
- Tant qu'aucun ami n'héberge la sauvegarde, la part P3 n'existe que sur la station.

### Agenda (CalDAV) et synchronisation incrémentale
- **CalDAV** (RFC 4791) : agenda `/dav/calendars/<email>/default/`, événements, tâches et notes `.ics` **chiffrés** dans `/Calendrier` de votre uCloud (donc répliqués avec la sauvegarde, jamais publiés sur Nostr). Découverte `/.well-known/caldav`, `calendar-multiget`, `calendar-query`, validation iCalendar (un seul `UID` par objet, 512 Ko au plus).
- **`sync-collection`** (RFC 6578) pour l'agenda **et** le carnet d'adresses : un journal des changements (`dav_changes`, alimenté par des triggers SQL, suppressions comprises) ne renvoie au téléphone que ce qui a changé ; un jeton plus ancien que 30 jours renvoie le client à une synchronisation complète.
- Limites : `calendar-query` renvoie tout l'agenda sans filtre de dates ni développement des récurrences (le client s'en charge) ; pas d'invitations entre participants ; un seul agenda par compte. **Non testé avec de vrais clients** (DAVx⁵, iOS, Thunderbird) : tests réels à faire.

### Coffre de mots de passe et agents (MCP)
- **`/Coffre`** accueille un coffre KeePass (`perso.kdbx`, `agents.kdbx`) synchronisable par WebDAV avec Keepass2Android, Strongbox ou KeePassXC. Chiffré par un mot de passe maître que la station ne connaît pas ; sauvegardé chez les amis hébergeurs ; **jamais partageable** (ni lien d'invité, ni envoi à un ami).
- **Registre des agents** : événement kind 31903 `d = agents`, signé du membre et chiffré pour lui seul ; jetons `ag1_…` montrés une fois (seule l'empreinte est gardée). Routes `/api/v1/agents`, `/revoke`, `/requests`, `/decide`.
- **Serveur MCP** `POST /mcp` (jeton d'agent en `Bearer`) : outils `demander_acces` et `statut_acces`. Le membre accorde ou refuse ; un agent ne reçoit jamais de mot de passe. Tout est tracé (`audit.agent`).
- Pas encore : l'ouverture de `agents.kdbx` par la station à l'accord, la remise d'un identifiant à un navigateur, un écran dédié.

### Reprise du uCloud d'Astroport.ONE
- **`astroportd import-ucloud <email> [--dry-run]`** reprend l'ancien uCloud (`.ucloud/ucloud.db`) : mêmes blobs UENC v1 dans IPFS, mêmes clés, rien de recopié ; l'ancienne base n'est jamais modifiée. Contrôle préalable par échantillon (déchiffrement, taille, SHA-256) ; une entrée écartée est toujours dite et listée.
- Les CID v0 (`Qm…`) se lisent par Kubo (GET, `Range`, miniatures) ; leur suppression ne désépingle jamais le blob. Ils sont laissés de côté par la sauvegarde chez les amis, le P2P et le contrôle d'intégrité.

### Interface
- **Trois styles** au choix dans l'en-tête : **Aurore** (par défaut), **Sobre** (l'ancien rendu) et **Kraft** ; mode automatique, clair ou sombre. Bandeau d'identité sur « Mon compte », tuiles de statistiques, icônes d'écran, chat de BRO avec message d'accueil, une seule ligne d'en-tête à 1280 px.
- **Correction** : l'écran **Personnes** ne s'ouvrait plus (`IMG_CAP is not defined`, introduit avec le plafond de miniatures) ; « Mon compte » affichait le texte « null » sous son titre.
- Budget de la coque relevé à 52 Ko bruts et 20 Ko compressés (48 et 15 avant).
- Le test de fumée navigateur (`tests/browser/smoke_ui.py`) passe de nouveau en entier et couvre le sélecteur de style.

### Remplir son uCloud depuis un autre serveur WebDAV (mode « pull »)
- **uCloud › Sources** (et `astroportd pull-webdav`) : tire un dossier d'un Nextcloud, d'un ownCloud ou d'une autre station astroportd, en arrière-plan, avec progression, aperçu et arrêt. Les fichiers arrivent **tels quels** (aucun tri par dates) sous `/Imports/<libellé>` ; le passage est rejouable et ne reprend jamais deux fois un fichier, même déplacé ensuite par un rangement (table `pull_imported`, provenance `origin = pull:<source>`).
- Mot de passe d'application rangé **chiffré pour son titulaire seul** ; adresses privées refusées depuis l'interface ; identifiants jamais envoyés après une redirection vers un autre serveur ; chemins hors dossier ignorés.
- `ingest` crée désormais aussi les dossiers intermédiaires manquants. Testé de bout en bout contre un second astroportd (3 Mo, noms accentués, rejeu).

- Le dossier `.moved/` d'un compte déménagé (il contient encore ses clés) est **effacé après 30 jours** par la nuit de 20h12.

### Ranger son uCloud par type
- **uCloud › Ranger** : réorganise le uCloud comme un ordinateur de bureau (Documents, Images, Musique, Vidéos, Archives), avec aperçu, sous-dossiers par date facultatifs, progression, arrêt et **annulation** de chaque passage (`organize_runs`, `organize_moves`). Ne déplace que des entrées ; coffre, contacts, agenda, partages, sauvegardes et dossiers cachés intouchés ; jamais d'écrasement.
- **Correction** : déplacer ou renommer une photo (par WebDAV comme par le rangement) fait maintenant **suivre ses visages** (`faces.ucloud_path`, `source_path`) ; avant, le lien avec la photo était perdu.
- **Correction** : l'attribut `hidden` n'était plus respecté par les boutons (la refonte visuelle les affichait : « Extension Nostr » sur l'écran de connexion, « Scanner le QR »…).
- Les pages d'inscription, de restauration et d'invitation suivent le style choisi (Aurore, Sobre, Kraft ; clair ou sombre).

### Un jeton WebDAV par appareil
- **Mon compte › Appareils** : un jeton `dv_…` par téléphone ou ordinateur, **confiné à un dossier du uCloud** que vous choisissez, avec des droits (envoi seul, lecture et écriture, lecture seule). Montré une seule fois (seule l'empreinte est gardée), révocable, dernier usage affiché, 16 appareils au plus.
- Le client voit son dossier comme la racine du WebDAV (liens sans préfixe, `..` sans effet, destinations de `MOVE`/`COPY` replacées sous la racine) ; contacts et agenda lui sont fermés ; le jeton n'ouvre jamais l'interface.
- Budgets de l'interface : écrans à la demande relevés à 160 Ko bruts et 56 Ko compressés (128 et 48 avant les écrans Sources et Ranger).

### Un fichier déposé deux fois n'occupe qu'une place
- Au dépôt (WebDAV, interface, reprise), un fichier dont le clair (SHA-256 et taille) existe déjà dans le **même compte**, en UENC v2, **réutilise le blob et la clé** existants ; le blob qu'on vient d'écrire est jeté. Le blob n'est libéré que quand plus aucune entrée ne le référence (suppression d'une copie sur deux : il reste).
- Deux comptes ne partagent jamais un blob (clés distinctes) : le partage entre comptes viendra avec l'accord explicite prévu au plan double UPlanet. Les fichiers UENC v1 (Kubo) ne servent jamais de cible et ne sont jamais dépinglés. Pas de nettoyage rétroactif : sur le uCloud repris de `fred@qwantic.com`, les doublons ne pèsent que 4 Mo sur 49 Go.

### Hébergement facturé par le Capitaine (ẑ)
- **Le Capitaine fixe, pour chaque MULTIPASS hébergé, un quota et un prix hebdomadaire en ẑ** (ex. 128 Go à 0 ẑ pour l'un, 128 Go à 3 ẑ pour l'autre) : écran Station › Comptes hébergés › **Hébergement**. Une réponse vide retire le réglage propre au compte. (ẑ minuscule : les Ẑen du MULTIPASS, côté Opex ; le Ẑ majuscule de la ZenCard, côté Capex, n'est pas touché.)
- **Palier par défaut** (même écran) : quota et prix des MULTIPASS sans réglage propre. Sans rien régler, **tout est gratuit** et rien n'est débité.
- **Prélèvement piloté par le cycle de vie** : tous les 7 jours depuis le dernier prélèvement réussi, le prix est viré sur la Ğ1 du MULTIPASS vers un **compte technique d'hébergement** (sel `<UPLANETNAME>.HOSTING.<PeerID du NODE>` : il ne dépend pas de l'email du Capitaine, qui peut changer, et il est distinct du `CAPTAIN_DEDICATED` du legacy). Un prix fixé aujourd'hui ne débite que dans une semaine. Solde insuffisant : **période de grâce** de 7 jours (régularisable à tout moment), puis **suspension** ; le virement qui passe rétablit le compte. **Grâce et suspension sont un statut affiché et un message : elles n'affectent jamais le service** (accès, fichiers, restauration) et rien n'est supprimé. Un virement par cycle, noté avant l'envoi ; une issue inconnue n'est jamais rejouée (alerte au Capitaine). Les virements comptent comme loyers (vue `rents`) pour la clôture hebdomadaire.
- **Règlement hebdomadaire du compte technique** (semaine clôturée à 20h12, **seulement si `--weekly-costs` > 0**, comme `PAF = 0` désactive l'économie du legacy) : `weekly_costs` = 3×PAF. **Si le montant reçu le permet** : 1×PAF au NODE, 2×PAF au MULTIPASS du Capitaine, puis le surplus tel que la clôture l'a ventilé (provision d'impôt `IMPOT`, prime de gestion 1 % au Capitaine, puis `TREASURY`, `RND`, `ASSETS`). Jamais plus que les recettes de la semaine ni que le solde du compte technique ; chaque virement est noté avant l'envoi. Un déficit reste celui de la clôture (trésorerie ou bénévolat actif) : aucun prélèvement sur `TREASURY`, `RND` ni `ASSETS`.
- Le membre voit son prix, son dernier prélèvement et son statut dans « Empreinte sur les communs ».

### Transaction primale des portefeuilles coopératifs
- Le portefeuille maître `UPLANETNAME_G1` envoie **1 Ğ1** à chaque portefeuille coopératif vide (`UPLANETNAME`, `SOCIETY`, `TREASURY`, `RND`, `ASSETS`, `IMPOT`, `INTRUSION`, `CAPITAL`, `AMORTISSEMENT`), au compte technique d'hébergement et au NODE : leur **primal** les marque comme appartenant à cette UPlanet (équivalent de `UPLANET.init.sh`, indispensable aussi pour qu'ils reçoivent de petits montants). **Option `--primal-init`, désactivée par défaut** (cela dépense de vrais Ğ1). Un envoi par portefeuille, noté avant la signature ; maître trop pauvre : alerte quotidienne et reprise seule.

### Plusieurs astroportd, un seul bassin de blobs
- **`--pool-peer`** : plusieurs `astroportd` (ORIGIN, ZEN*) partagent le même dossier `--blobs` ; un blob n'est supprimé que si **aucun** plan ne le référence (lecture seule des bases des pairs ; pair illisible = blob gardé).
- **`bridge-copy`** : adoption **sans copie** de fichiers d'un plan dans l'autre (même blob, même clé), sur accord explicite `--same-person`, rejouable, sans écrasement, quota de destination appliqué. L'ancien format (Kubo) et les blobs absents du bassin sont signalés, jamais copiés en silence.
- **`pool-status`** : poids du bassin, part de chaque plan, place économisée, orphelins (`--sweep`).

### Table de tri et Bibliothèque
- **Supprimer les photos des visages choisis** (barre de sélection, ou pochette « 🗑 Supprimer les photos ») : la confirmation donne le nombre exact de photos, l'action est irréversible. Une photo supprimée — ici comme par WebDAV ou le rangement — emporte désormais **ses visages et son analyse en attente** (avant, la table gardait des visages sans photo) ; les visages de référence des contacts ne sont jamais touchés.
- **Trier par dossier** : un sélecteur (`/Images/2024/05 · 120 visages, 80 à ranger`) restreint la table à un dossier ; les groupes se calculent alors sur ce sous-ensemble, même avec des dizaines de milliers de visages au total (plafond de 8 000 non identifiés). Les personnes restent celles de tout le compte.
- **Doublons** : seulement photos et vidéos par défaut (les documents identiques ne polluent plus la liste) ; une case ajoute les autres fichiers.
- **Bibliothèque en onglets** : *Temps* (fil continu, jour par jour, du plus récent au plus ancien, avec saut au mois), *Carte*, *Recherche*, *Doublons*.
- **Carte du monde hors ligne** : contour embarqué (Natural Earth, domaine public, 6 Ko), aucun serveur de tuiles appelé. Les photos géolocalisées sont regroupées par la station (jamais 30 000 points dans le navigateur), zoom à la molette ou aux boutons, glisser pour se déplacer, clic sur un groupe pour voir ses photos, période par dates, préréglages et histogramme par mois.

### Plusieurs UPlanet dans un seul processus (phase 6, étapes a et assistant)
- **`astroportd planes`** sert ORIGIN et des UPlanet privées dans **un seul processus** (`planes.json`) : même NODE et même `IPFSNODEID`, un bassin de blobs commun, une base, un dossier de comptes et un port par plan. Invariants vérifiés avant tout démarrage : Kubo réservé à ORIGIN, un seul ORIGIN, jamais deux plans avec la même `swarm.key`, base, dossier de comptes ou port. `--check` valide et prépare sans lancer. Essayé pour de bon : deux plans, un processus, même PeerID, réseau privé pour le second.
- **Assistant en ligne de commande** (`plane-setup list | add | remove`, avec questions si on n'a rien précisé) **et sur le web** (Station › Plans) : fonder une UPlanet (clé créée et montrée pour être copiée) ou rejoindre avec une `swarm.key` reçue ; ports libres, clé en `0600` jamais écrasée, ORIGIN intact, rien de supprimé en retirant un plan.
- Reste de la phase 6 : routage par nom d'hôte sur un port commun, `--plane` sur les autres commandes, sélecteur de plan dans l'en-tête, test automatisé « plusieurs plans, un processus ».

### Paramètres du MULTIPASS : IA déclarée, BRO conditionnel
- **Mon compte › Intelligence artificielle** : chaque MULTIPASS déclare **un Ollama** (adresse) **ou une clé d'API** (Anthropic, Gemini). Stocké dans un événement 31903 `d = ai` **chiffré pour lui seul** (NIP-44) : la clé n'est jamais en clair dans la base, sur le relai ni dans une réponse de l'API (l'interface ne reçoit que « une clé est enregistrée »). Rejouable (`replay`). Retirer le fournisseur retire aussi sa clé.
- **L'onglet BRO n'apparaît que si le MULTIPASS a déclaré une IA** ; ouvrir `#/bro` sans IA renvoie vers Mon compte.
- Le Capitaine fixe toujours les défauts de la station (quota et prix par défaut, par compte) ; chaque MULTIPASS ajoute ensuite ses propres paramètres applicatifs. Le bouton « Hébergement » de l'écran Station, qui ne s'affichait pas (cellule de tableau mal aplatie), fonctionne de nouveau.

### Carnet d'adresses et « Ma fiche »
- **Contacts devient un vrai carnet** : liste avec **recherche instantanée** (nom, société, emails, chiffres d'un téléphone, notes, groupes ; sans accents ni casse), **index A–Z**, **groupes** (renommer ou retirer un groupe sur toutes les fiches), **fiche détaillée modifiable** (société, fonction, téléphones et emails typés, adresses, anniversaire, sites, notes, groupes), **import d'un .vcf** (doublons ignorés, fiches refusées expliquées), **doublons** (même nom, email ou téléphone) à **fusionner** (tout s'additionne, rien ne se perd). Les fiches restent des vCard chiffrées synchronisables avec un téléphone ; modifier une fiche venue d'un téléphone **conserve** sa photo, sa clé Nostr et tout ce que le carnet ne gère pas. L'ancien écran (synchronisation du téléphone, invitations) est l'onglet « Synchroniser et inviter ».
- **Fiche d'un ami ↔ son profil Nostr (kind 0)** : la fiche et le profil public se comparent champ par champ (nom, présentation, image, site, NIP-05, adresse Lightning) ; on **prend chaque valeur à part**, jamais en bloc.
- **« Ma fiche »** : votre profil Nostr et votre propre fiche sont **réunis dans un seul formulaire**. Le profil publié fait foi pour les champs publics ; l'enregistrement **publie le kind 0** (champs « public ») **et écrit la fiche chiffrée** ; téléphones, adresses, anniversaire et notes **ne quittent jamais la fiche**. Elle est toujours en tête, ne se supprime ni ne se fusionne.
- **Personnes ↔ Contacts** : nommer des visages inconnus (table de tri) **crée la fiche de la personne si aucune ne porte ce nom**, sinon les relie à la fiche existante (comparaison sans accents ni casse) ; le dialogue « Qui est-ce ? » propose les noms de vos contacts.
- Le profil publié accepte aussi le site web, le NIP-05 et l'adresse Lightning.

### Messages privés 1 à 1 et une personne dans plusieurs UPlanet
- **Écran « Messages »** : conversations 1 à 1 **chiffrées de bout en bout (NIP-17, enveloppes NIP-59)**, non lus avec pastille dans le menu, envoi par Entrée, arrivée en direct. Chaque message est aussi emballé pour son auteur : l'historique se retrouve sur toute station qui héberge le compte. Seuls comptent les messages d'un proche que vous suivez (ou d'un compte hébergé ici) ; un message non remis reste dans le fil, marqué « non remis ».
- **Une personne a une clé Nostr par UPlanet** (formule isolée) : sa clé dans une autre UPlanet ne se déduit **pas** de son email. Elle se retrouve par deux voies, jamais par un email auto-déclaré : (1) **l'annuaire de la station** : les plans de `planes.json` sont lus en lecture seule, l'email d'un MULTIPASS hébergé donne sa clé dans chaque plan ; (2) **les liens prouvés** (kind 31903 `d = plane-link`) : un lien n'existe que si **les deux clés l'ont signé** (l'événement d'une clé porte la preuve signée de l'autre). Table `key_links` (projection).
- **La fiche contact** liste les clés de la personne, une par UPlanet, avec leur origine ; « Écrire » est actif pour l'UPlanet courante, et renvoie vers l'autre UPlanet sinon.
- **Le chat parle à la personne** : une conversation par personne (clés liées regroupées, historique commun), canal choisi dans l'UPlanet commune ; les conversations des plans voisins (même email) s'affichent en lecture seule avec leur UPlanet. Si la personne n'a pas de clé dans l'UPlanet courante, l'erreur dit où lui écrire.
- **Annoncer ses autres comptes** (`POST /api/v1/me/links`) ne part que **sur demande** : le lien est public (il révèle que deux clés vont ensemble). À faire dans chaque UPlanet.

### Base de données : schéma allégé (mesuré)
- **`nostr_events`** passe à une table à `rowid` (clé interne `seq`) ; `nostr_tags` et l'index plein texte s'y rattachent par cet entier au lieu de l'identifiant de 64 caractères. Sur 200 000 événements : base **28 % plus petite**, insertion **2× plus rapide**, jointure par tag **≈ 9× plus rapide**.
- **`ucloud_entries`** passe aussi à une table à `rowid` (unicité de `(owner_hex, path)` conservée) : base **26 % plus petite**, insertion **1,6× plus rapide**, liste d'un dossier **≈ 100× plus rapide** (150 000 entrées).
- Les opérations sur une arborescence (liste, suppression, déplacement, copie) utilisent une plage d'index au lieu d'un `LIKE` : ≈ 100× plus rapide, et **sensible à la casse** (l'ancien `LIKE` confondait `/Docs` et `/docs` : supprimer l'un emportait l'autre).
- Une base d'une version précédente est **migrée à l'ouverture** (reprenable si le processus s'arrête en route) ; rien à faire.
- **« Ce jour-là » et les mois** suivent la date que le membre voit : `taken_at` est désormais l'**heure murale** de la prise de vue (l'EXIF lu tel quel, sans fuseau). Une photo prise à 23 h 30 le 14 juillet reste au 14 juillet.

### Corrections
- La connexion par PASS ne distingue plus une adresse inconnue d'un compte sans PASS (même réponse, même travail côté station).
- Le point de contrôle du journal WAL de la nuit signale un échec quand un lecteur l'a empêché (il était ignoré en silence).
- Un accès temporaire ouvert pour un ami aidant se referme à son échéance, même si personne ne rouvre l'écran d'assistance.
- La table de tri des visages borne les vignettes gardées en mémoire du navigateur.

### À savoir
- Le smoke navigateur complet (`tests/browser/smoke_ui.py`) échoue sur `input[name=fo-mode]` depuis avant cette version ; les pages inscription et restauration passent avec ce bloc sauté.
- La restauration par plusieurs amis (k parts sur n) reste à venir.
