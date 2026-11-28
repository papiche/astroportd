## astroportd 2.3.0 — Embarquement des Capitaines sans DNS, UPlanet reconnaissable, domaines et HTTPS des constellations

- **Deux stations sans nom de domaine se trouvent et leurs Capitaines peuvent devenir amis.** Les machines se découvrent seules ; les personnes ne se suivent que d'un clic.
  - **Annonce du Capitaine** (`Captains::keep_announced`, `src/captains.rs`) : au démarrage puis toutes les 10 minutes, le DID du Capitaine (kind 30800 `d = did`) est republié avec `t = captain` et sa station d'attache `home_station = <PeerID>:<NODE_HEX>` (rien de republié s'il le dit déjà).
  - **Pont maillage → Nostr** (`swarm_cards::captain_keys`, `Crawler::with_keys_source`) : le `captainHEX` des cartes de station reçues par le maillage devient une clé du crawl, demandée chez sa station par le tunnel du relai. Nouveau cercle `Circle::Mesh` : kinds publics seulement (profil, contacts, relais, DID), sans curseur, pour que le privé soit rapatrié depuis le début le jour où la personne est suivie ; 256 clés au plus par passage.
  - **Rôle `captain` projeté depuis un DID signé** (`crates/astro-sync/src/projector.rs`) : un compte distant dont le DID porte `t = captain` et une station d'attache devient Capitaine ; un DID plus récent sans la marque le rend simple membre. Jamais pour un compte hébergé ici, et `mark_hosted` rend membre un Capitaine distant qui vient s'héberger. Avant, un Capitaine distant n'était jamais reconnu : l'écran Capitaines restait vide.
  - **Écran Capitaines** : un Capitaine n'apparaît que si **la carte de sa station le nomme** (champ `verified`, la carte arrive par une connexion authentifiée par le PeerID) ou s'il y a déjà un suivi ; un DID qui se dit Capitaine ne suffit pas. Aucun suivi automatique : « Suivre » signe le kind 3, et l'amitié réciproque ouvre les services en mode « amis ».
  - **Crawl : repli sur le public** (`CrawlConfig::public_kinds`, compteur `public_only`) : un relai refuse toute une requête dès qu'elle contient un kind privé et que la clé de la station (son NODE) n'est pas dans son cercle — c'est le cas entre deux stations amies, où c'est le Capitaine qui est suivi. Le lot est alors relu en kinds publics, sans curseur, et le refus est compté dans le bilan. Limite connue : les kinds privés d'un ami sur une autre station ne sont pas encore rapatriés.
  - Test d'acceptation : `tests/captains_onboarding.rs` (deux vraies stations libp2p sans domaine, relais joints par le maillage seulement, échange réel des cartes, vrai crawler authentifié par le NODE : B apparaît sur A, A suit, B voit « vous suit », B suit, amis, le service de B s'ouvre à A) et un test hostile (DID qui se dit Capitaine sans carte, membre hébergé qui se dit Capitaine).
- **UPlanet reconnaissable** : `GET /api/v1/look` (public) règle style, mode et nom de l'UPlanet pour l'application et les pages autonomes ; Kraft réservé à ORIGIN ; section « Monnaie et apparence » (`CURRENCY_NAME`, `CURRENCY_SYMBOL`, `UI_STYLE`), ignorée sur ORIGIN ; nom de l'UPlanet à côté du logo et dans le sélecteur de constellation.
- **Domaines et HTTPS des constellations** : domaine public par plan (vérifié par le DNS, `station.conf` du plan), carte Certificats HTTPS ; `planes.json` `"https"` / `planes --tls` : front HTTPS ACME pour tous les domaines, routage par domaine exact ; redémarrage depuis le cockpit aussi en mode `planes`.
- **Blog** : « Répondre » mène à `/repondre/<adresse>` (compte ici, autre station, pas de compte, autre client Nostr) ; pied de page : se connecter, créer son MULTIPASS, visites des 30 jours, aucun cookie.
- **Clé Pl@ntNet propre à la station** (`STATION_KEYS`, `kv` `coop_local`), prioritaire et jamais publiée.
- **Champs hors connexion** : plus de `type=password` (`secret: true`), `autocomplete=off` et marques anti-gestionnaires.
- Guide régénéré (version 2.3.0).

## astroportd 2.2.0 — Partage N¹ en chaîne, cascade de Ẑen Voie A, remixes recette et atlas botanique

- **Moteur de partage en cascade et DAG de remix (`crates/astro-economy/src/cascade.rs`)** : arithmétique entière exacte en `DeciZen` (`1 DeciZen = 0,10 Ẑen = 0,01 Ğ1`), signatures BIP-340 indépendantes (`CommonsBlock`, sauts `ShareChain`, contresignature de générique `Credit::attest`), aplatissement récursif du DAG (`flatten_dag`, plancher `remix_min_bps`, fusion des diamants) et filtrage WoT anti-Sybil des relais éligibles à la tranche Diffusion.
- **Transport NIP-59, liste officielle WoT et remixes « recette » (`src/cascade_share.rs`, `src/sharing.rs`, `src/share_inbox.rs`, `src/media_share.rs`)** : la lignée `ShareChain` circule chiffrée dans les enveloppes NIP-59 (y compris pour les fiches recette sans duplication de blob via `fiche_work_sha`), compteur public facultatif `kind 30005` filtré par la toile des rencontres prouvées (`POST /api/v1/ucloud/media/chain/register`), contresignature co-auteur (`POST .../credit/attest`), sous-titres `.vtt`/`.srt` en recette et prise en charge des communs sur photos, plantes, documents (PDF) et articles.
- **Paiement Voie A atomique, retombées, merci NIP-59 et ardoise groupée (`crates/astro-duniter`, `src/zen_credit.rs`, `src/cascade_features.rs`)** :
  - Lot atomique `utility.batch_all` (`Chain::transfer_batch`) sur Duniter v2s (ou promesses en mode île), contrôle préalable du dépôt existentiel Ğ1 (`1 Ğ1`) et de la frontière de plan, reçu Nostr `kind 30852`.
  - **Mes retombées** (`GET /api/v1/ucloud/media/earnings`) : synthèse créations / relais / œuvres soutenues / derniers soutiens reçus.
  - **Gratitude chiffrée** : mot de merci facultatif (signé ou anonyme) envoyé en NIP-59 aux bénéficiaires distants et notifié en local.
  - **Ardoise groupée** (`GET/POST /api/v1/ucloud/media/slate`, `POST .../slate/settle`) : empilage rapide `♡ +0,50 Ẑ`, fusion de tous les bénéficiaires et règlement en un seul lot atomique.
- **Atlas botanique collaboratif et ergonomie web (`m-media.js`, `m-media-cascade.js`, `m-plant.js`, `m-photos.js`, `m-files.js`, traductions `en`/`es`)** : bouton *Publier en commun* en un geste depuis Photos et Fichiers, détection automatique de la fiche parente dans Fiche plante (sans notes ni GPS fin), onglet *Atlas du cercle* (`GET /api/v1/ucloud/plants/atlas`, maille UMAP `0,01°` et carte SVG hors ligne), budget des écrans à la demande : 920 Ko bruts, 360 Ko compressés (< 12 Ko par écran).
- Guide régénéré (version 2.2.0).

## astroportd 2.1.1 — vidéos à soi partageables, navigateur des agents sur serveur, conception du partage en chaîne

- **Créer la fiche d'une vidéo** déposée sans fiche (Médiathèque › Créer la fiche), avec le choix d'une licence (CC BY, BY-SA, BY-NC, BY-NC-SA, CC0) : la vidéo se corrige puis se partage avec les amis (étiquette de licence comprise).
- **Navigateur des agents sur un serveur sans écran** : lancé avec les options d'un serveur (sans GPU, `/dev/shm` ménagé, sans bac à sable en root) ; ce qu'il écrit en s'arrêtant est désormais dit au journal (« agents : le navigateur s'est arrêté : … »), même chose pour Naviguer ensemble.
- **Conception du partage en chaîne et de la cascade de Ẑen** : `docs/design/PARTAGE_CASCADE.md` (voie A retenue : le spectateur connaît sa lignée et paie chaque maillon directement ; rien n'est encore codé).
- Guide régénéré (version 2.1.1).

## astroportd 2.1.0 — Membres refondus, plantes, vidéos dans la Médiathèque, sections pliables

- **Membres, une liste qui se règle sur place** : quota, prix, Ğ1Billets, lecture et IA enregistrés aussitôt, aide « ? » sur chaque colonne, tri par activité, disque ou ancienneté, filtre « Inactifs +30 j ». Chaque ligne montre le logo, le bandeau et le site du membre (champs de son profil). Le Capitaine retire un compte inactif depuis plus d'un mois, confirmé en deux temps, et peut **supprimer complètement un MULTIPASS et ses données** (`src/member_purge.rs`, `POST /api/v1/station/accounts/{hex}/delete`) : solde rendu, profil vidé, fichiers (seuls les blobs que personne d'autre ne référence), clés.
- **Mes Ẑen** : le solde est dans l'en-tête et ouvre l'**historique des transactions** (`GET /api/v1/me/zen/history`, lu auprès de l'indexeur Ğ1 ; non éprouvé contre un indexeur réel).
- **Plantes (Pl@ntNet)** : la clé se règle dans la configuration coopérative (*Services aux membres*). Une photo de plante a sa **fiche** (comestible ?, préparation, propriétés médicinales, dangers, notes), prérempli depuis Wikipédia avec la source citée, modifiable, rangée chiffrée dans l'uCloud du membre et cherchable. **Plantes** apparaît à côté de Médiathèque quand la clé est réglée ou que le membre a déjà des fiches. Un avertissement de toxicité est toujours affiché ; la liste des plantes toxiques connues est en français seulement.
- **Réseau** : stations cliquables avec leur fiche, Capitaines réunis dans le même encart, « Suivre » son Capitaine, stockage libre total de l'essaim (les autres stations l'annoncent après leur mise à jour).
- **Écluses** : nom de l'UPlanet affiché, ORIGIN reconnu aussi avec une `swarm.key` de zéros, réglages du relai déplacés ici depuis Options.
- **Économie** : la PAF se règle dans « Régie coopérative 3×1/3 » (Constellation), plus dans le cockpit ; « Constellation » reste dans le sous-menu.
- **Sections pliables** : chaque carte titrée se plie d'un geste sur son titre, et l'état est retenu par écran (`m-fold.js`).
- **Médiathèque** : les vidéos déposées par WebDAV **sans fiche** y apparaissent (onglet Vidéos, titre tiré du nom du fichier) et ne polluent plus Photos, sauf celles d'un dossier d'appareil photo (`DCIM`, `Camera`).
- **Mes agents** : l'écran dit que brancher un agent par MCP n'ouvre aucun navigateur ; la page n'apparaît dans l'application qu'après acceptation de la demande de mode navigateur. La cause du « rien ne s'ouvre » n'a pas été reproduite : aucun jeton d'agent n'existe sur cette station (à confirmer avec le client MCP utilisé).
- **Chat** : le « ⋮ » est calé à droite.
- **Plugin `corpus`** (astroportd-plugins) : le corpus d'essai s'installe en option (`install-plugins.sh corpus`).
- Budget des écrans à la demande : 884 Ko bruts, 344 Ko compressés (876 Ko avant cette version).

## astroportd 2.0.0 — BRO parle, l'IA de la station se partage, vos données restent chez vous

- **BRO discute, en flux.** Il répond avec l'IA qui vous est attribuée, mot à mot, et dit **où** chaque réponse a été calculée (🏠 chez vous, 🌐 sur Internet) et avec quel modèle. Il **cherche dans vos fichiers** (mots, personnes reconnues, villes, dates, et ce que montrent vos photos), lit **votre solde**, relit **ce que vous lui avez confié** (sa mémoire) et **retient** ce que vous lui demandez (« retiens que… », dans votre journal privé, seule écriture). Un modèle de 8B ou plus est conseillé pour des outils fiables (essai réel : `qwen3:14b` réussit, `llama3.2` 3B non). Chaque message se copie, se partage ou se supprime (il sort alors de ce que BRO relit). `src/bro_chat.rs`, `POST /api/v1/bro/stream` (SSE) et `/bro/say`.
- **Routage hybride : la confidentialité d'abord.** Dès qu'un outil a lu vos données, la conversation est marquée 🔒 et ne va qu'à une IA de votre station ou de la constellation ; sur Internet, seulement avec votre accord, donné pour une fois. Les outils ne sont jamais offerts à une IA cloud sans cet accord. Une demande ouverte et lourde (long texte, code) va d'abord au cloud.
- **Deux familles d'IA, dites comme telles** (*Moi › Appareils*, carte IA) : **sur votre station** (l'Ollama de la station, ou l'Ollama / vLLM d'un ami par tunnel — toujours en boucle locale, et pour un membre seulement parmi les offres de la station : sinon la station appellerait pour lui l'API de Kubo…) ou **par Internet** (OpenRouter, **MixRoute**, Mistral, Gemini, Anthropic : un seul client au format OpenAI, `src/ai_client.rs`). **Tester** mesure ce qu'elle sait faire (réponse et délai, outils, vision, mémoire, images, crédit), et la carte dit **ce que votre IA permet**. ComfyUI est à part (images).
- **Le Capitaine prête son IA et règle chaque membre** (*Membres*, carte « IA de vos membres » et colonne « IA ») : partage de sa clé (jamais montrée), droits (aucune IA / celle de la station / la station et la leur), **quota** de jetons par semaine (table `ai_usage`), **prix en ẑ par semaine** prélevé sur le MULTIPASS **avec l'hébergement** (un virement, détail au journal `hosting_fee`) et donc compté dans les loyers et la clôture 3×1/3. **Débit** réglable par clé (50 requêtes par minute par défaut). **Liens de parrainage** de la coopérative chez les fournisseurs (MixRoute par défaut), présentés comme tels. `src/ai_rights.rs`, `GET/POST /api/v1/station/ai`. Conception : `docs/design/IA.md`.
- **Mes agents : aucune demande ne passe sans trace.** « Récemment » (rendue par l'agent, expirée sans réponse, refusée, reprise), une demande attend 30 min au lieu de 5, l'expiration est inscrite au journal, l'Accueil dit « OpenClaw (Claude Code 2.1) demande l'accès à … » (le programme tel qu'il se présente au serveur MCP), une demande refusée ne s'affiche plus comme accordée.
- **À propos** : le titre « Astroport » ouvre ce qu'est le projet, ses crédits (Ğ1FabLab, logiciels libres, modèles et licences) et comment contribuer.
- **Journal du démon gardé sur disque** : `install_sudo.sh` pose `Storage=persistent` ; `doctor` avertit d'un journal en mémoire seulement.
- Vérifié contre les vrais services : MixRoute (modèles gratuits, flux, outils) et un Ollama local (flux, outils, vecteurs) — essais `#[ignore]` lancés à la main.
- Budget des écrans à la demande : 832 Ko bruts, 320 Ko compressés (800 et 312 Ko avant cette version).

## astroportd 1.11.0 — Moi en une page par question, Partages du Capitaine, interface au pouce

- **Moi, une seule rangée de sous-onglets** : *Compte · Mes Ẑen · Mon blog · Appareils · Sauvegarde*, une question chacun. Plus d'onglets dans les onglets. *Compte* : profil, préférences appliquées tout de suite (langue, style, visio), mon espace, et, replié, ce que je publie. *Appareils* : un jeton par appareil (le formulaire s'ouvre seul s'il n'y en a aucun), accès complet WebDAV, puis les assistants (IA, lecture des documents, agents).
- **Sauvegarde : « Suis-je à l'abri ? »** Un bilan en tête — mon compte, mes fichiers, mes publications, ma carte de secours — dont chaque ligne mène à la carte qui l'améliore. Gardiens, ami hébergeur, « Si je perds tout », ce que je garde pour mes amis, copies chez les autres stations et export des clés sur une seule page (avant : trois endroits). Demander à un ami de garder sa sauvegarde ne se fait plus qu'à un endroit. « Bien démarrer » n'est plus que sur l'accueil, avec l'étape « Rendre vos documents cherchables ». Les anciennes adresses (`#/account?tab=…`, `#/replication`) mènent à la nouvelle page.
- **Partages du Capitaine** (*Réseau › Partages*, `m-services.js`) : un seul écran pour tous les services entre stations, dans les deux sens. *Chez mes amis* (Utiliser, adresse à copier), *Sur cette machine* (qui peut utiliser chaque service, amis ajoutés pour une durée, chips révocables) et *S'entraider* (accepter une demande puis ouvrir l'accès, sans ligne de commande). **Les ports TCP qui écoutent sur la machine** sont listés (`src/local_ports.rs`, lecture de `/proc/net/tcp{,6}` ; ceux de la station sont exclus, pas de tunnel vers un tunnel) et se partagent d'un clic ; un port ou `IP:port` du réseau local se saisit à la main. *Réglages* passe sous *Santé*.
- **Passe téléphone (400 px), vérifiée à l'œil écran par écran** : en-tête sur une ligne, accueil en tuiles, Proches avec « ⋯ Plus », Fichiers avec barre d'actions contextuelle, Essaim en rangées, sous-onglets qui centrent l'onglet actif, titres en double retirés (Photos, Mes Ẑen), dates sans majuscule fautive.
- **Traductions** : une centaine de phrases restées en français en anglais et en espagnol (accueil, appareils, sauvegarde, Fichiers, page de secours `/restore`) sont traduites ; la bulle d'aide « ? » est commune à tous les écrans (`K.tip`, `K.head`).
- **La fiche contact, pivot de la personne** : Écrire, Appeler, **Payer** (dialogue rempli), **Ses articles** (Mon blog › Chez mes amis filtré sur elle) et **son cercle**, réglable d'un geste. Un article d'ami mène à la conversation (« Lui écrire »). Carnet et fiche traduits en entier (anglais, espagnol).
- **Payer, Recevoir, Mes cercles plus simples** : sans caméra, plus de cadre noir, le choix dans les proches prend sa place ; le montant à demander tient sur une ligne sous le QR ; seuls les cercles habités ont une carte, les vides tiennent en une ligne, la toile de confiance est repliée en bas avec son état dans le titre.
- **Corpus d'essai pour le Capitaine** (`tests/corpus/fabriquer_corpus.py`) : 45 fichiers pour vérifier qu'une station cherche bien — photos libres de Wikimedia Commons (un chat, la tour Eiffel… et des portraits d'Einstein, Marie Curie, John Conway, Alan Turing, Katherine Johnson, Thomas Pesquet, Chaplin, Monroe… pour FaceCloud), panneau et ticket à lire, PDF avec et sans texte, vidéos, dates et lieux fictifs ; `LISEZMOI.md` dit quoi taper et ce qui doit sortir ; `--envoyer` les dépose par WebDAV.
- Budget des écrans à la demande : 800 Ko bruts, 312 Ko compressés (768 et 296 Ko avant cette version).

## astroportd 1.10.0 — payer et se rencontrer face à face, cercles de proches, toile de confiance, Ğ1 en différé

- **Payer et recevoir par QR, de téléphone à téléphone.** *Mes Ẑen › 📲 Recevoir* montre un QR (montant facultatif, aucun nom dedans) ; *📷 Payer* le lit à la caméra et ouvre le dialogue d'envoi habituel, destinataire et montant remplis, avec le nom que **sa** station connaît : « Léa, votre proche » ou « vérifiez son nom ». L'écran Recevoir annonce « Paiement reçu ». npub accepté ; lecteur partagé avec `/scan` (`scan-cam.js`). Route `GET /api/v1/me/zen/receive`.
- **Se rencontrer** (*Proches › 🤝*) : mon code, scanner son code, ou chercher dans la constellation (jamais d'email complet). L'amitié n'existe que si les deux font le geste (scan ou demande acceptée). **Rencontre prouvée ≠ nomination** : le code porte la signature, par la personne montrée, de la tranche de 2 minutes en cours (valable 6 minutes : une photo du code expire) ; attestation kind 30855 `how=qr|named`, revérifiée par chaque station (`astro_economy::meet`, projection `meetings`). Conception : `docs/design/PAYER_QR.md`.
- **Mes cercles** : Famille, Proche, Ami, Connaissance, Contact. Par défaut un ami réciproque est Ami, une personne suivie sans retour Contact ; on ne range que les exceptions. **Privé** : un seul kind 30078 chiffré pour soi (`src/proximity.rs`), jamais dans la liste de contacts. Branché sur le paiement (le cercle rassure ou avertit).
- **Toile de confiance des rencontres** (carte dans *Mes cercles*) : la mécanique de la toile Ğ1 (Duniter v2s : 5 certifications, distance 5, 80 % des référents, 2 ans, 100 par émetteur) appliquée aux seules rencontres prouvées, racines = Capitaines de la constellation. Calcul pur et rejouable (`astro_economy::wot`, `GET /api/v1/me/wot`). Le dividende universel qu'elle préparera n'est pas actif.
- **Gardiens conseillés** (*Ma sécurité*) : d'abord un proche sur un **autre Astroport**, puis du cercle le plus proche, puis rencontré en personne ; si tous les proches sont sur la station, l'écran le dit et propose *Se rencontrer*.
- **ISLAND-1 « Ğ1 en différé »** : pendant une coupure de Duniter, « Envoyer des Ẑen » devient une promesse (même écran, mêmes deux clics), plafonnée au dernier solde connu plus les DU, réglée en vraie Ğ1 au retour du réseau (30852 `t=island`, crédit 30854). Ouverture à la main ou automatique par le Capitaine (`#/ile`), suivi des promesses (`#/island`). **À n'activer que sur une station de test** : la confiance entre stations reste à traiter (`docs/design/ISLAND.md`).
- **Corrigé** : limitation de débit par source bornée (une IPv6 compte par /64, table plafonnée, débordement commun) ; un virement Ğ1-N² signé par le membre seul ne compte plus comme loyer payé (seul le loyer attesté par la station, kind 30851) ; *Fichiers* : changer la sélection désarme « Supprimer » ; *Médiathèque* : « suivant » pressé plusieurs fois ne superpose plus deux sons.
- **Conception (rien d'activé)** : UPlanet **10101** et le **LOVE**, dividende universel compté en LOVE, entretien des lieux (UMAP) et Vœux, d'après le jeu « 3 2 1 DU » de la Forêt Enchantée ; règles, simulations et spécification de mise en œuvre : `docs/design/DU_G1N2.md`, `docs/design/sim_du/`.
- Budgets relevés : écrans à la demande 768 Ko bruts / 296 Ko compressés, page `/scan` 35 Ko / 14 Ko (historique dans les tests). Tests navigateur : `smoke_pay.py`, `smoke_island.py`.

## astroportd 1.9.0 — agents qui agissent pour vous sans votre mot de passe, naviguer ensemble avec un ami

- **Naviguer ensemble : se faire aider par un ami sur un site.** Depuis une conversation (⋮ › Naviguer ensemble) ou depuis *Partager* du téléphone, le membre ouvre un site dans le navigateur jetable de sa station et invite des amis réciproques : ils voient la même page en direct, et chacun a un niveau que le membre change d'un geste — regarder, montrer (pointeur nommé), prendre la main (clics et frappe, mots de passe compris ; un seul à la fois). Visio à un bouton, une heure au plus, tout effacé à la fin. Un ami hébergé sur une autre station passe par la sienne (service `cosurf`, demandes signées de sa clé, NIP-98, vérifiées). Le navigateur ne va jamais sur le réseau local. Écran `m-cosurf.js`, module d'écran commun `m-screen.js`, service `src/cosurf.rs`.
- **Navigateur prêté à un agent (MCP, mode `navigateur`), écran *Mes agents*.** Un agent (OpenClaw, Claude Code, un script Playwright…) demande à agir sur un site ; le membre accepte dans *Moi › Mes appareils › Mes agents*, se connecte lui-même dans un Chromium jetable **montré dans l'application** (téléphone ou ordinateur : la page est diffusée, clics et frappe rejoués ; aucun écran nécessaire sur la station), puis le **prête** pour la durée choisie et peut **reprendre la main** à tout moment. `statut_acces` rend `browser_ws_endpoint` (`ws://127.0.0.1:<port>/cdp/<jeton>`) pour `connectOverCDP`.
  - **Sécurité** : Chromium n'ouvre aucun port (tube CDP) ; porte à jeton de 256 bits, un seul client, cookies, fichiers et navigation explicite hors site refusés, en-têtes `Cookie` nettoyés ; **garde de navigation** qui bloque, dès le prêt, tout chargement de la page principale hors du site (clic, formulaire, JavaScript) et ferme les fenêtres ouvertes ailleurs ; sites liés (connexion via Google, FranceConnect) permis par le membre. Échéance, reprise, `rendre_acces` ou fermeture : groupe de processus tué, profil effacé ; balayage au démarrage. Audit `session_open` / `session_ready` / `session_end`.
  - **Agents du nuage** (ChatGPT, claude.ai) : outils `naviguer`, `lire_page`, `cliquer`, `saisir`, `capture`, exécutés par la station dans le navigateur prêté.
  - **Agent sur une autre station de la constellation** : déclaré « sur <station> », il parle à sa station (`/mcp?station=<PeerID>`), qui relaie par le maillage (service `agents-mcp`, station reconnue par le maillage) ; la porte lui est ouverte par un tunnel DRAGON réservé à cette station, et l'agent reçoit une adresse en boucle locale chez lui.
  - Multiplexeur CDP (la station et l'agent partagent le navigateur sans se voir). Nouveau module `src/agent_browser/` ; écran `m-agents.js` ; `install_sudo.sh --chromium`, signalé par `install.sh`. Conception : `docs/design/SESSION_AGENT.md`.

## astroportd 1.8.0 — souvenirs, recherche dans tous les échanges, mise à jour depuis l'interface, chat complet, déménagement Google/iCloud, blog

- **Souvenirs.** L'accueil montre « Il y a 1 an » (photos prises ce jour-là les années passées). *Photos › Voyages* repère seul les voyages : photos à plus de 80 km de chez soi plusieurs jours de suite, « chez soi » étant la position déclarée par le MULTIPASS (sinon déduite des jours photographiés) ; nom tiré du dossier où les photos sont rangées, personnes présentes, lien vers toutes les photos. Une **page par personne reconnue** (`#/person`) : ses photos année par année, avec qui elle apparaît, les voyages partagés ; ouverte depuis sa fiche du carnet (visages rattachés à la fiche, même nommés autrement). API : `/api/v1/ucloud/trips`, `/api/v1/ucloud/person`.
- **Une seule case de recherche pour tout.** En plus des fichiers : les conversations (déchiffrées, groupes compris, sans accents ni casse), ses notes, articles et brouillons, ceux des amis réciproques, les fiches du carnet ; chaque résultat mène au bon écran (conversation, article dans « Chez mes amis », « Répondre » à une note, fiche). Un article réservé (NIP-70) n'est montré qu'à son auteur et à ses destinataires. API : `/api/v1/me/find`.
- **Rien d'important ne vit plus seulement dans la base.** Visages nommés, historique du chat et mémoire de BRO sont écrits dans l'uCloud du membre (`/_/Visages`, `/_/Messages`, `/_/Memoire`), emportés par la sauvegarde chez les amis et relus après une restauration (fusion idempotente, effacements compris).
- **Mon espace : où sont mes données.** Anneau par zone (privé, partagé, Public, Coffre, visages/messages/mémoire, index, carnet), jauge « à l'abri chez vos amis » avec ce que garde chaque gardien, jauges de ce que l'on garde pour ses amis face au volume accordé.
- **Mise à jour depuis l'interface du Capitaine** (*Ma station › Détails techniques*) : version publiée, modèles proposés, état des plugins, confirmation, journal qui défile ; install.sh tourne hors du service (`systemd-run`) puis la station redémarre. Le cockpit annonce une nouvelle version.
- **install.sh revoit modèles et plugins à chaque passage**, même quand le binaire est à jour : modèles de recherche mis à jour (empreinte), nouveaux modèles du catalogue proposés selon la machine (`astroportd models update`, `models list --absent`), plugins mis à jour et nouveaux plugins proposés (`install-plugins.sh --update`, version installée suivie).
- **Réplication et replay.** Le miroir Kubo ne rapatrie plus les films des amis et n'insiste plus 5 s sur un fichier introuvable ; le cercle N² est recalculé une fois en fin de replay (plus à chaque liste) et n'est plus vidé pendant le replay ; crawl : suppressions (kind 5) répliquées, une transaction par page, N2 limités à ce qui sert au pare-feu, purge nocturne des notes d'auteurs sortis du cercle ; loyers arrivés avant l'identité du payeur rattrapés ; index de recherche découpé au-delà de 96 Mio au lieu d'être abandonné en silence ; sauvegarde : les gros fichiers anciens n'empêchent plus les petits de partir ; `CidGate` avec cache court et index des miniatures.
- **Chat au niveau des grandes messageries** : nouveau message vers proches et carnet, recherche, citer et répondre, ✓ remis / ✓✓ lu, pièces jointes et vocaux, aperçus de liens, notifications Web Push sans contenu, groupes (participants et nom modifiables), réactions, modification, « écrit… », épingler, archiver, sourdine.
- **Déménager depuis Google ou Apple** (*Fichiers › Déménager*) : archives Takeout / iCloud en plusieurs parties lues dans le navigateur, doublons évités, albums, dates, lieux, descriptions et personnes, contacts, agenda, documents ; reprise sans renvoi.
- **Blog et site public** rendus par la station (articles, notes, flux Atom, trois thèmes), *Mon blog* (articles réservés aux amis ou à un groupe, brouillons, programmation), « Chez mes amis », partage depuis le téléphone, Favoris.
- **Capitaine** : alertes enregistrées (`/alerts`), virements à l'issue inconnue ; Ẑen vers tout MULTIPASS de la constellation.
- **Corrigé** : erreurs clippy qui faisaient échouer la CI (Linux et macOS) ; étiquettes d'état non traduites ; phrases « Public », « Coffre », « Groupe », « Brouillon », « Avec » non traduites hors de leur écran.
- Budgets des écrans relevés (700 Ko bruts, 272 Ko compressés), historique dans le test.

## astroportd 1.7.0 — sauvegarde répartie entre gardiens, secours nominatif, services de la constellation, photos publiques nettoyées

- **Sauvegarde répartie (Reed-Solomon k-sur-n).** Les fichiers privés ne sont plus copiés en entier chez chaque ami : ils sont regroupés en paquets de 16 Mio, rechiffrés, puis découpés en fragments répartis entre les gardiens (un par station), dont quelques-uns suffisent à tout retrouver — un « RAID 6 » entre stations amies. Le nombre de fragments nécessaires est choisi pour viser 99,9 % de chances de restauration, d'après la présence réelle de chaque gardien. Exemple : 3 fragments sur 5 donnent environ 99 % pour 1,7 fois la place, là où trois copies complètes en prenaient 3 fois. Nouveau crate `astro-erasure` (GF(2⁸) partagé avec la clé de secours, code de Cauchy, fragments ARSF v1, vecteurs figés) ; paquets et index dans `astro-dav` (index chiffré par une clé dérivée du secret du compte : la carte de secours suffit à le retrouver). Conception : `docs/design/SAUVEGARDE_RS.md`.
  - **Réparation chaque nuit** : contrôle que les gardiens détiennent bien leurs fragments (lecture d'un échantillon comparée au recalcul local ; deux plages fausses pour condamner, jamais une erreur réseau), remplacement d'un gardien absent depuis 14 jours, alerte au membre quand un paquet n'a plus de marge. Un gardien ajouté reçoit sa part au fil des nuits.
  - **Compactage** en quatre étapes sans perte ; les anciennes copies complètes ne sont retirées qu'une fois les paquets sûrs, avec marge.
  - **Fichiers partagés** : la station compte leurs **copies vérifiées** chez des amis réciproques (stations distinctes, sonde de moins de 7 jours) et ne les écarte de la sauvegarde que si ces copies suffisent ; un lien de partage ou une invitation ne compte plus comme une copie.
  - **Médiathèque** : « 🟢 En sécurité : 3 copies chez vos amis · vérifié il y a 3 h ». *Ma sécurité* montre la sécurité estimée et la place économisée ; le Capitaine a un résumé dans *Ma station › Santé*.
  - **Corrigé** : les nouveaux fichiers ne partaient qu'au dernier gardien choisi.
  - Compatibilité : un gardien dont la station n'annonce pas cette version reste en copie complète.
- **Secours du MULTIPASS.** *Ma sécurité* nomme les gardiens et donne un niveau (aucun, fragile, bien, solide) ; *Pour vous* alerte tant que personne, hors de la station, ne garde la part. La page `/restore` montre les noms publics des contacts sollicités et leur état — elle pouvait auparavant afficher l'**adresse email** du gardien qui confirmait. Après une restauration, le compte repart au bout de 3 jours mais les amis absents sont relancés chaque jour pendant 30 jours ; un manifeste revenu tard est **fusionné sans rien écraser** (un conflit garde les deux versions).
- **Services de la constellation** (*Ma station › Réseau › Essaim*) : la carte « Services ouverts par la constellation » liste ce que les stations vous ouvrent (SSH, Ollama, ComfyUI…), l'ouvre d'un clic et donne l'adresse locale à copier. **Anti-cascade** : une station n'annonce ni ne sert un service qui mène à l'un de ses propres accès vers un ami, ni un service dont la cible ne répond pas. **Ports stables** : port habituel s'il est libre, sinon un port de 24000 à 24999 dérivé du service et de la station amie, le même à chaque ouverture.
- **Site public : photos nettoyées à la sortie.** JPEG, PNG et WebP de `/Public` sont servis sans position GPS ni identité de l'appareil (EXIF, XMP, IPTC…), orientation gardée ; l'original reste intact. HEIC, AVIF et TIFF ne sont pas servis au public ; dans le doute, refus plutôt que fichier brut. La vigie de 20h12 ne signale plus la position des photos, et signale les formats non servis.
- **Installation** : correctifs d'`install.sh`, `install_sudo.sh` et de `astroportd doctor`.
- **Documentation** : README recentré (quickstart, parcours par public), `docs/DEVELOPPER.md` (dont « écrire un client compatible »), `docs/COMPARATIF.md`.
- Reste à mesurer sur un vrai réseau et sur Raspberry Pi : débit et mémoire de la sauvegarde répartie, durée d'une première sauvegarde et d'une restauration (`docs/design/SAUVEGARDE_RS.md`, § 8.4).

## astroportd 1.6.1 — binaires macOS

- **macOS : les archives Apple sont enfin publiées.** La compilation réussissait, mais `build-all.sh` échouait juste après, dans son récapitulatif : le bash 3.2 de macOS tient un tableau vide pour une variable non définie sous `set -u`. Le script parcourt maintenant ses listes d'une façon que bash 3.2 accepte.

## astroportd 1.6.0 — le disque d'abord (Kubo en miroir), install_sudo.sh, recherche par mots entiers

- **ORIGIN : le disque d'abord, Kubo en miroir.** Les fichiers s'écrivent toujours sur le disque de la station, même quand Kubo répond : un Kubo arrêté (ou un `stop.sh` d'Astroport.ONE qui l'arrête) ne rend plus rien illisible. Kubo devient un **miroir de publication** sur le réseau IPFS des stations ORIGIN (swarm.key à zéros, pas l'IPFS mondial ; `src/kubo_mirror.rs`) : il rapatrie sur le disque, après vérification, ce qui était rangé dans Kubo seulement ; il publie le reste **par référence au fichier** (*filestore*, `nocopy` : aucune place en plus), ou par copie si Kubo ne peut pas relire le disque (le Capitaine est prévenu) ; il désépingle ce qui n'est plus référencé. Les gros fichiers gardent le CID `raw` du fichier entier : vérifiables chaque nuit, et échangeables par le maillage entre stations. `init` donne `--kubo` à ORIGIN, comme `planes`.
- **`install_sudo.sh`** : `install.sh` reste sans `sudo`, mais dit ce qui manque et propose ce script, qui n'installe que cela : ffmpeg, poppler, python3-pip, git, **kubo 0.40.0** pour ORIGIN (archive officielle vérifiée par SHA-512 figé ; dépôt neuf sur le réseau d'ORIGIN : swarm.key à zéros, amorçage sur les stations ORIGIN, AutoConf coupée — sinon Kubo 0.40 refuse de démarrer en réseau privé —, `LIBP2P_FORCE_PNET=1` ; filestore activé ; service sous l'utilisateur ; un kubo existant est gardé) et le démarrage avec la machine (`loginctl enable-linger`). `--dry-run` montre ce qu'il ferait.
- **Recherche : un mot se cherche entier** (sans accents ni majuscules, au pluriel près), dans le nom, le dossier, la description et le texte lu : « chat » ne trouve plus « achat », ni « 2025 » une photo de 2026 dont le nom porte l'heure 20:25 (fonction SQL `has_word` d'`astro-db`, même règle pour l'index FTS). Une photo **trouvée par sa description** compte parmi les meilleurs résultats et dit pourquoi (pastille 👁, extrait de la description). Relevé sur une vraie station de 30 000 photos.
- **`init` ne prend plus la swarm.key d'ORIGIN pour une constellation ẐEN** : le kubo d'une station ORIGIN porte la clé à zéros, et `init` proposait de « reprendre » cette constellation ẐEN (oui par défaut, et d'office avec `--yes`). Cette clé est maintenant reconnue comme celle d'ORIGIN et ignorée.
- **`pull-webdav`** range les fichiers repris sur le disque, dans le dossier de blobs du plan de la base (le bassin commun avec `planes`, qu'il ignorait), et ne parle plus à Kubo (le miroir de `serve` publie ensuite). L'option `--kubo` disparaît.

## astroportd 1.5.0 — recherche dans tout le uCloud, Médiathèque, gros fichiers entre amis

- **Une seule recherche pour tout le uCloud** (*Fichiers*, sous Dossiers / Photos / Médiathèque) : noms, texte lu dans les documents, ce que montrent les photos et les vidéos, personnes reconnues, villes, mois et années. « Léa plage 2024 » : les photos de Léa, prises en 2024, qui montrent une plage ; la station dit ce qu'elle a compris. Chaque mot peut être trouvé dans une source différente ; une interprétation ajoute une façon d'être trouvé, sans jamais retirer le mot. **Les plus proches** d'abord, puis *Documents*, *Photos et vidéos*, *Films et musique* ; **Peut-être** quand rien n'est sûr ; type et période dans *Affiner*. Villes de plus de 15 000 habitants embarquées (GeoNames, CC BY 4.0, `src/data/cities.tsv.gz`), retenues seulement là où le membre a des photos (`src/search_query.rs`, écran `m-find.js`). L'ancien onglet *Photos › Recherche* disparaît.
- **Lire le texte des documents** (crate `astro-index`, PP-OCRv6 small par ONNX Runtime, PDF par poppler) : scans, PDF et photos de tickets se retrouvent par un mot qu'ils contiennent (`search_text`, FTS5 sans accents) ; « Copier le texte » dans la visionneuse ; la vigie de `/Public` repère les papiers d'identité et relevés bancaires. Une station légère peut **confier la lecture** à une ou plusieurs stations de Capitaines amis, avec l'accord explicite du membre (*Moi › Documents et photos cherchables*) : service P2P `ai-index`, fermé par défaut, en boucle locale, file équitable par station (`503` + `Retry-After`), la moins chargée d'abord.
- **Chercher une photo ou un moment de vidéo par ce qu'il montre** (SigLIP 2 base int8), sur les stations équipées seulement, jamais à distance : un vecteur par photo et par image clé de vidéo (12 au plus ; ffmpeg lit la vidéo en boucle locale, sans clair sur le disque, format imposé par le type). Requête corrigée de l'attirance des images, puis seules les photos qui sortent nettement du lot (z ≥ 3,5, 14 photos au moins) : 12 bonnes réponses sur 15, aucun intrus, mesuré. « ▶ 1:23 » sur la vignette, la visionneuse ouvre la vidéo à ce moment. JPEG décodé directement réduit (une photo 5K passe de 1 s à 0,47 s).
- **Paquet d'index** : ce qui a été calculé pour un membre est rangé dans son uCloud (`/_/Recherche/index.astroidx`), emporté par la sauvegarde chez un ami et relu après une restauration au lieu d'être recalculé.
- **Le membre garde la main** : pause de l'analyse de ses fichiers, effacement de ce qui a été calculé (paquet compris, confirmé dans la page), place de son index affichée. **Le Capitaine décide au-dessus** (*Ma station › Membres*) : station, défaut des comptes, chaque MULTIPASS, prêt aux amis.
- **`astroportd models`** (`list`, `add`, `remove`, `bench`) : modèles figés, épinglés dans IPFS, vitesse mesurée sur la machine. Budget des écrans : 568 Ko bruts / 216 Ko gzip (536 / 204 avant la recherche unique, 512 / 196 avant « Documents et photos cherchables »).

- **Reprise d'un rapatriement** : un gros fichier coupé (pair parti, station redémarrée) reprend **au premier segment manquant** au lieu de repartir de zéro. La partie reçue est gardée dans `.tmp/part-<cid>` (segments entiers seulement, mise sur le disque avant de rendre la main), relue une fois pour son empreinte, et jetée si l'empreinte finale ne correspond pas ; deux rapatriements du même blob n'écrivent jamais dans le même fichier ; une partie abandonnée depuis 7 jours est effacée à l'ouverture du magasin.
- **Lire un film pendant qu'il arrive** : un blob absent ouvert par la station (lecture `/play`, WebDAV, copie d'un ami) n'attend plus le fichier entier. Le rapatriement part en tâche de fond (une fois par blob, il continue si le lecteur s'en va), le lecteur reçoit la main dès que la taille est connue et suit la partie reçue ; une lecture au-delà attend les octets suivants, un échec du rapatriement l'interrompt. Rien n'entre dans le magasin avant la vérification de l'empreinte complète, chaque bloc UENC v2 est authentifié au déchiffrement, et la **fin de lecture attend le verdict** : un blob dont l'empreinte est fausse se termine par une erreur, jamais par une fin normale. Une copie de médiathèque (clic « Lire ») est annoncée dès son arrivée **commencée** ; un partage ordinaire reste annoncé une fois entièrement reçu (sinon réessayé, puis signalé). **Un saut dans le film est servi en priorité** : le lecteur dit quel segment il attend (et le suivant, pour une lecture suivie) ; le rapatriement le demande avant ceux de l'ordre du fichier et le garde en mémoire (8 segments, 32 Mio par film) jusqu'à ce que l'écriture, qui reste dans l'ordre (empreinte et reprise inchangées), le reprenne sans le redemander.
- **Chat** : les messages de service entre stations (sauvegarde chez un ami, assistance), qui voyagent aussi en kind 14, n'apparaissent plus dans les conversations, ni comme questions à l'agent BRO (`Rumor::is_station_protocol`, étiquette `t = astro-…`).
- **Déduplication sociale** (plugin `media` 0.3.0) : avant de télécharger, réencoder ou déposer, le plugin demande à la station si un proche réciproque a déjà **la même œuvre** — même film TMDB, même vidéo ou morceau YouTube (lu dans l'adresse, avant tout téléchargement, y compris pour chaque morceau d'un mix), même épisode (série par son identifiant TMDB ou son nom, saison, numéro). Si oui, il demande sa copie — le **même** fichier chiffré, sans nouveau CID — au lieu de déposer ; s'il est déjà chez le membre, il le dit. `--own-copy` dépose quand même ; une station muette ou ancienne laisse le dépôt se faire comme avant. Route `GET /api/v1/ucloud/media/friends/match` (bornée, membre authentifié). Un épisode porte désormais la série dans son événement (`i tmdb-tv:<id>`, plugin et station). Le chiffrement dérivé du contenu a été écarté : il ne fusionnerait que des fichiers identiques à l'octet, et permettrait à quiconque détient un film de savoir qui l'a aussi.
- **Un seul blob par film** : une copie demandée part vers une source du blob **le plus répandu** chez les amis (même empreinte `x`, même CID), un partage ne rechiffre jamais : un film circule de N¹ en N¹ sous le même CID et une station n'en garde qu'un fichier, quel que soit le nombre de membres qui l'ont.
- **« Partager avec mes amis »** dans la Médiathèque (film, épisode, morceau ; une **série** ou un **album** se partage comme une liste NIP-51 kind 30005 de ses épisodes ou morceaux, dans l'ordre) : la station signe l'événement avec la clé du membre, **les mêmes étiquettes que le plugin `media`** (test de parité octet pour octet), retrait par NIP-09. **Trace** : qui l'a copié et quand, de qui on le tient, quels amis le repartagent (`src/media_share.rs`).
- **Negentropy (NIP-77)** : réconciliation d'ensembles par plages dans `astro-nostr-proto` (messages identiques octet pour octet à l'implémentation de référence de strfry, vecteurs dans `tests/fixtures/negentropy_vectors.json`), côté relai (`NEG-OPEN`/`NEG-MSG`/`NEG-CLOSE`, limité à ce que le lecteur a le droit de lire, 100 000 enregistrements, 4 réconciliations par connexion) et côté client (`WsClient::sync_as` : seuls les événements manquants sont demandés ; lecture complète si le relai ne connaît pas NIP-77). « Chez mes amis » s'en sert : seules les nouveautés d'un ami voyagent, ses suppressions sont vues.
- **Moins de trafic de synchronisation** : le **crawl** réconcilie chaque lot de clés par Negentropy (ensemble local = tout ce que la base a déjà pour ces clés, quel que soit le relai d'origine) — la fenêtre de recouvrement ne retélécharge plus rien ; repli sur les pages si le relai ne parle pas NIP-77, se tait 5 s, ou si la fenêtre dépasse 100 000 événements. La **mesure de réplication** réconcilie son échantillon d'événements au lieu de les télécharger, et sonde les **fichiers** en un seul flux par station (`HASN` dans `/astro/blob/1.1.0`, 4 096 empreintes, un bit par fichier, écluse respectée) au lieu d'un `HAS` par fichier. Les inventaires de blobs ne sont **pas** réconciliés par Negentropy : la réconciliation révélerait au demandeur les CID qu'il n'a pas.
- **Chez mes amis** (*Médiathèque*) : les films et morceaux partagés par les proches réciproques, lus chez leur station au nom du membre (NIP-42 : le relai de l'ami décide), avec le nombre de copies et leurs sources ; **copie à la demande au clic « Lire »**, rangée au même dossier que chez l'ami (fiche et affiche comprises), annoncée à l'arrivée. Côté ami : réciprocité et publication vérifiées, refus renvoyé. Caches `friend_media`, `friend_media_sync`, état `media_requests`. Budget des écrans : 512 Ko bruts / 196 Ko gzip (504 / 192 avant).
- **Médiathèque** (*Fichiers › Médiathèque*) : films et vidéos, séries par saison, musique par album ou par mix, d'après les fiches `media/v1`. Recherche sans accents sur titre, artiste, genre, acteur, réalisateur, mots-clés. **Lecture en flux** par `/play/<jeton>` (jeton signé, 12 h, un fichier d'un compte ; `Range` et déchiffrement à la volée) : film en grand avec sous-titres, musique dans une barre qui continue d'un écran à l'autre (file d'écoute, commandes de l'écran verrouillé). **Correction des fiches** depuis l'écran (champs selon le type), réécrites dans l'uCloud. Index en cache local rejouable (`media_index`). Fiches, affiches, pochettes et sous-titres ne s'affichent plus dans **Dossiers** ni dans **Photos**. Budget des écrans relevé à 504 Ko bruts / 192 Ko gzip (480 / 184 avant). Test navigateur `tests/browser/smoke_media.py`.

- **Protocole de blobs `/astro/blob/1.1.0`** (remplace 1.0.0, incompatible : aucune station n'était encore en ligne). Requêtes `HAS <cid>` et `GET <cid> <début> <longueur>`. Le rapatriement sonde les pairs en parallèle, découpe le blob en segments de 4 Mio demandés à plusieurs pairs à la fois (mémoire bornée à 16 Mio quel que soit le fichier), reprend **au même endroit** chez un autre pair un segment coupé ou trop lent, et vérifie toujours l'empreinte SHA-256 avant de valider. Le délai n'est plus total mais un **silence toléré** (plus un débit plancher de 32 Kio/s) : un film de plusieurs Go ne s'arrête plus au bout de deux minutes. La place disque (réserve comprise) est vérifiée avant le premier octet.
- **Playlists réservées au cercle N¹** : le kind 30005 (liste de vidéos NIP-51, `kind::VIDEO_SET`) suit la règle des kinds 21/22 — l'auteur et ceux qu'il suit seulement. Le plugin `media` 0.2.0 y publie les mix de DJ (un morceau = un kind 21 à `imeta` audio) ; test d'interopérabilité étendu (`tests/plugin_media_interop.rs`).
- **Plafonds relevés** : 4 Gio par fichier pour un MULTIPASS (1 avant), 16 Gio pour une ZEN Card et le Capitaine (4 avant) ; partages, sauvegardes chez un ami et échange P2P acceptent jusqu'à 16 Gio. Les quotas totaux ne changent pas.

## astroportd 1.4.0 — écluses, UPlanet visibles sur la carte, guide en images

- **Écluses** (*Ma station › Écluses*) : trois modes de station, réglés d'un geste. **Forteresse** (hébergés et amis réciproques seulement, aucune inscription), **Village** (défaut : amis et amis d'amis), **Place publique** (messages et profils ouverts à tous). Le mode est lu en SQL par le relai, le crawl et FaceCloud.
- **Tiroir système** : Coffre, Contacts et Calendrier rangés sous `/_/` (migration au démarrage).
- **FaceCloud** : une photo n'est envoyée d'office à un ami que s'il y est **seul avec vous**.
- **Vigie de `/Public`** (20h12 et accueil du membre) : prévient — sans rien modifier — des photos géolocalisées, des photos dont l'EXIF porte le **numéro de série de l'appareil ou le nom de son propriétaire**, et des fichiers sensibles (coffre, contacts, agenda, clés).
- **Balise de station** (kind 30850) : porte le mode, le nom de l'UPlanet, la version et l'empreinte de sa charte, la clé de sa coopérative. Une station ẐEN en Village ou Place publique l'annonce aussi sur le relai ORIGIN (`ORIGIN_RELAY` dans la configuration coopérative, `none` pour s'abstenir) ; une Forteresse jamais. Le relai ORIGIN accepte ces balises ouvertes, et elles seules.
- **Carte des stations** (G1v1map) : chaque UPlanet ouverte devient un choix de constellation, avec sa charte dépliable ; les Forteresses n'y figurent pas.
- **« Mes Ẑen »** remplace « Mon argent » (accueil, Moi ; en anglais *My Ẑen*, en espagnol *Mis Ẑen*).
- **Téléphone** : la barre d'en-tête passe à la ligne au lieu de rogner ; le nom de la constellation n'est plus tronqué.
- **Guide** : trois schémas animés (écluses, connexion en deux temps, plans), trois romans-photos tirés de vraies captures (`docs/roman/roman_photo.py`), texte allégé de moitié, détails techniques repliés. README et limites connues alignés sur le code.

## astroportd 1.3.1 — Ğ1Billet par UPlanet, imports WebDAV triés, test navigateur réparé

- **Ğ1Billet** : « ORIGIN » sur les billets de l'UPlanet ORIGIN, nom de l'UPlanet sur les autres. Nouveau choix « Fond et logo » : profil Nostr (défaut), Eugro ou Astroport ; Eugro remplace le profil s'il n'a pas d'images. Images embarquées dans le binaire.
- **Sources WebDAV externes** : filtre par type (images, musique, vidéos, documents, archives) et rangement automatique (`/Images`, `/Musique`…), en web et en CLI (`pull-webdav add --kinds … --sort`). Les fichiers non repris sont comptés « laissés ».
- **Test navigateur** (`tests/browser/smoke_ui.py`) remis d'équerre avec la navigation et la connexion actuelles.

## astroportd 1.3.0 — navigation refondue, fiche de bienvenue, guide mis à jour

- **Navigation par espaces** : table de routes unique (`m-routes.js`). Mon espace : Accueil · Fichiers · Proches · Moi (barre du bas sur téléphone). Ma station (Capitaine, bouton en haut à droite) : Santé · Membres · Réseau · Économie. Sous-onglets et retour calculés depuis la table ; les anciennes adresses (`#/station`, `#/chiffres`) redirigent.
- **Écrans déplacés sans perte** : comptes hébergés, jeton de secours, hébergement → *Membres* ; chiffres du membre → *Moi › Mon argent* (sans Opex/Capex) ; nœud, relais, sous-systèmes → *Détails techniques*. L'onglet « Album » du uCloud est retiré (doublon de *Fichiers › Photos*).
- **Proches** réunit carnet et conversations ; la fiche d'une personne porte *Écrire* et *Appeler*. Mon compte : réglages avancés repliés.
- **Liens corrigés** : « Trier en 1 clic » ouvre le tri ; alertes du cockpit vers l'objet exact (`#/members?f=locked`) ; alertes « À faire » en tête du cockpit ; « Tout relancer » pour les tâches abandonnées.
- **Console SSH du navigateur supprimée** : l'accès à un service d'un ami passe par `astroportd tunnel connect`.
- **Documentation** : rôles des sites (astroport.one, map.astroport.one, qo-op.com), mots du guide en clair, dépannage de la vie réelle, nouvelle fiche **BIENVENUE** à remettre aux membres ; la page d'accueil mène à la carte des stations.
- **Budgets** : coque 46 Ko (57 avant), écrans à la demande relevés à 440 Ko bruts / 176 Ko gzip.
- Test `crates/astro-ui/tests/routes_rules.mjs` : chaque lien `#/…` tombe sur une route.

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
