## astroportd 0.5.0-beta

Cette version fait de la **clé de secours** un secret qu'on peut perdre sans danger, et ajoute l'**agenda** : « Agenda » du téléphone se synchronise avec la station, comme « Contacts ».

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
