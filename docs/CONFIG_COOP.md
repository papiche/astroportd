# Configuration coopérative : où trouver chaque valeur

L'écran **Configuration coopérative** (`#/coop`, espace Capitaine) publie un seul réglage pour **toute la constellation** : chaque station le reçoit et l'applique. Il est rangé par tâche : *Mettre en service*, *Recevoir les dons*, *Règles communes*, *Retours des utilisateurs*, et un dernier bloc pour les stations Astroport.ONE. Cette page dit, section par section, à quoi sert chaque champ et **où aller chercher sa valeur**. Le « ? » à côté du titre de chaque section de l'écran mène ici.

> **À savoir.** Les champs dont le nom contient `KEY`, `TOKEN`, `SECRET`, `API` ou `PRIVATE` sont **chiffrés** avant publication et ne sont plus jamais affichés : l'écran dit seulement « renseigné ». En **ORIGIN**, n'importe qui peut les déchiffrer : n'y mettez une vraie clé que dans une constellation **ẐEN** (avec `swarm.key`).

## Nom et charte de la constellation {#uplanet}

| Champ | Ce qu'il faut y mettre |
|---|---|
| `UPLANET_NAME` | Le nom affiché de votre UPlanet (48 caractères au plus), choisi en coopérative. |
| `CHARTER_VERSION` | Un numéro (`1.0`, `1.1`…), à augmenter à chaque changement du texte. |
| `CHARTER_TEXT` | Le texte que chaque Capitaine signe. **Vide** : la [charte type](CHARTE_UPLANET.md) est proposée. |

Changer le texte demande une **nouvelle signature** à chaque Capitaine (la signature porte l'empreinte exacte du texte). Détails : guide, [Ma UPlanet](GUIDE.md#etape-8c).

## Monnaie et apparence {#look}

Pour qu'on sache d'un coup d'œil **sur quelle UPlanet** on se trouve. Le nom de l'UPlanet (`UPLANET_NAME`) s'affiche dans l'en-tête de l'application, à côté du logo, et dans le sélecteur de constellation du Capitaine.

| Champ | Ce qu'il faut y mettre |
|---|---|
| `CURRENCY_NAME` | Le nom de l'unité de compte (16 caractères au plus, sans chiffre), par exemple `Sol` ou `Graine`. **Vide** : « Ẑen ». Il remplace « Ẑen » partout dans l'application. |
| `CURRENCY_SYMBOL` | Son symbole (3 caractères au plus), par exemple `☉`. **Vide** : « Ẑ ». |
| `UI_STYLE` | Le style proposé par défaut aux membres : Aurore (vide) ou `sobre`. Chacun garde le choix de son appareil. |

Le style **Kraft est réservé à ORIGIN**, le réseau public d'essai : ses membres le voient tout de suite. Sur ORIGIN, ces trois réglages sont **ignorés**, et la monnaie reste « Ẑen ». La configuration d'ORIGIN est signée par une clé que tout le monde peut calculer, donc n'importe qui pourrait la renommer. Seul l'affichage change : les montants, la règle 1 Ẑen = 1 € et la comptabilité restent les mêmes.

## Domaine de la coopérative (OVH) {#ovh}

Sert à donner une adresse en `.<votre domaine>` à chaque station et à chaque MULTIPASS, et fixe le relai commun `wss://relay.<domaine>`. Il faut un **nom de domaine dont la zone DNS est chez OVH**.

1. `OVH_ZONE` : votre domaine, tel qu'il apparaît dans *Noms de domaine* du Manager OVH (ex. `copylaradio.com`).
2. Ouvrez **https://eu.api.ovh.com/createToken/** et connectez-vous avec le compte OVH qui gère ce domaine.
3. Remplissez : nom de l'application `uplanet-dnslink`, validité **Unlimited**, et ces droits :

```text
GET  /domain/zone/*
POST /domain/zone/*
PUT  /domain/zone/*
```

4. OVH affiche **une seule fois** trois valeurs : copiez-les tout de suite dans `OVH_APP_KEY` (*Application Key*), `OVH_APP_SECRET` (*Application Secret*) et `OVH_CONSUMER_KEY` (*Consumer Key*).

Puis cliquez **Tester la connexion OVH** sous la section : l'essai est en lecture seule. Clé perdue : OVH ne la redonne pas ; créez-en une nouvelle et supprimez l'ancienne (Manager OVH › votre compte › *Gestion des API*). Détails : guide, [DNSLink](GUIDE.md#etape-9b).

## Stations relais du maillage {#mesh}

**Le problème.** Deux stations derrière deux box différentes ne peuvent pas s'appeler directement : aucune n'accepte de connexion entrante. Sans chemin, pas de partage de photos, pas de synchronisation, pas de tunnel.

**La solution.** Une **station relais** : une station de la constellation dont le **port TCP 4101 est ouvert** sur sa box. Les autres y réservent une place et deviennent joignables à travers elle (tout reste chiffré). Il en faut au moins une ; deux ou trois, c'est mieux.

| Champ | Ce qu'il faut y mettre |
|---|---|
| `P2P_RELAYS` | Une adresse de station relais par ligne (8 au plus). |
| `ORIGIN_RELAY` | Les relais Nostr où une station ẐEN ouverte (Village, Place publique) annonce aussi sa balise, pour apparaître sur la carte des UPlanet : un par ligne, 4 au plus. Chaque Capitaine peut y **ajouter le relai de sa station** à côté de celui de la coopérative. **Vide** : `wss://relay.copylaradio.com`. `none` seul : ne rien annoncer hors de la constellation. |

**Le plus simple** : sur la station dont le port est ouvert, *Réseau* (`#/mesh`) › *Ma station* › **Devenir station relais** construit l'adresse et l'ajoute à la liste. Le port proposé est le **vrai port d'écoute de l'UPlanet affichée** : 4101 par défaut, mais 4102, 4103… quand plusieurs UPlanet partagent la machine (`astroportd planes`). Si la box redirige un **autre port public** vers lui, corrigez le champ *port*.

**À la main**, l'adresse a la forme :

```text
/dns4/<nom ou DynDNS>/tcp/4101/p2p/<PeerID>
/ip4/<IP publique>/tcp/4101/p2p/<PeerID>
```

1. **Le PeerID** (`12D3Koo…`) : sur la station relais, *Réseau* (`#/mesh`) › *Ma station* ; c'est la fin de l'adresse, après `/p2p/`.
2. **Le nom public** : le nom par lequel vous ouvrez l'interface de cette station depuis internet, un **nom DynDNS**, ou à défaut l'IP publique de la box. Les adresses `192.168.…`, `10.…` ou `127.…` ne marchent que sur le réseau local : ne les mettez pas ici.
3. **Le port** : celui de l'UPlanet (4101 par défaut ; *Ma station* l'affiche dans l'adresse, après `/tcp/`), ouvert en **TCP** sur la box et redirigé vers la machine de la station. Vérifiez depuis un autre réseau (un téléphone en 4G) : `nc -zv <nom> <port>`.

La station inscrite devient relais à son prochain démarrage ; les autres y réservent leur place dans la minute. L'écran montre sous le titre de la section combien de relais ont une place réservée.

> ⚠️ **Attention.** **Le premier contact.** Une station neuve reçoit cette liste… par une station à laquelle elle est déjà reliée. Il lui faut donc **un** premier lien : voir *Réseau* › **Rejoindre ma constellation**, qui accepte un jeton de parrainage ou une adresse de station. Détails : guide, [stations derrière une box](GUIDE.md#etape-9).

## OpenCollective {#oc}

L'entrée en euros : un don devient des Ẑen sur le MULTIPASS qui a la même adresse e-mail. L'écran dit sous le titre si le collectif est lu et combien de dons sont à créditer.

| Champ | Où le trouver |
|---|---|
| `OCSLUG` | La fin de l'adresse de votre collectif : `opencollective.com/<OCSLUG>`. |
| `OCAPIKEY` | OpenCollective › votre compte › *For developers* › *Personal tokens* › nouveau jeton, portée **`email`**. Il faut être administrateur du collectif. |
| `OC_AUTO_CREDIT` | *Je confirme chaque crédit* (par défaut) ou *La station crédite seule*. |
| `OC_URL_MEMBRE` | Page du collectif › *Contribute* › le palier d'adhésion › l'adresse du bouton (`…/contribute/<palier>-<numéro>`). Proposée aux nouveaux membres. |
| `OC_URL_CLOUD`, `OC_URL_SATELLITE`, `OC_URL_CONSTELLATION` | Même méthode, pour chaque palier. Ce sont les boutons de la page publique **Contribuer** d'UPlanet (`contribute.html`), qui les lit dans cette configuration. |

Détails : guide, [OpenCollective](GUIDE.md#relier-une-uplanet-opencollective).

## Routage des paliers {#tiers}

Dit à quel usage va un don, selon le **nom court du palier** OpenCollective (la partie avant le numéro dans `…/contribute/<palier>-<numéro>`). Plusieurs motifs séparés par des virgules ; `*` remplace n'importe quoi ; un mot sans `*` doit former un segment entier entre tirets.

| Champ | Le don devient | Valeur par défaut |
|---|---|---|
| `TIER_SLUG_SATELLITE` | une part sociale | `*parrainage*128*,extension-128,satellite,*love-box*claude*` |
| `TIER_SLUG_CONSTELLATION` | une part sociale (GPU) | `*parrainage*gpu*,module-gpu,constellation,*love-box*deluxe*,*love-box*gpu*` |
| `TIER_SLUG_LABO` | un don à la R&D | `infrastructure,labo,genereux-donateur,r-d,recherche` |

Un don qui ne correspond à aucun motif devient un crédit d'usage (1 € = 1 Ẑ). Laissez vide pour garder les valeurs par défaut.

## Règle 3×1/3 + 1 % {#3x13}

`TREASURY_PERCENT`, `RND_PERCENT`, `ASSETS_PERCENT`, `CAPTAIN_BONUS_PERCENT` : chaque semaine, après l'impôt, la prime du Capitaine est prise sur le surplus, puis le reste est partagé entre trésorerie, R&D et actifs **dans ces proportions**. Défaut 33 / 33 / 33 / 1. **Le total doit faire 100 %**, sinon l'écran refuse de publier ces champs. Décision de l'assemblée de la coopérative ; elle vaut dès la clôture suivante, **en ẐEN seulement** : en ORIGIN, la configuration peut être signée par n'importe qui, la station garde donc ses valeurs par défaut.

## Impôt sur les sociétés {#fiscal}

`IS_RATE_REDUCED`, `IS_RATE_NORMAL`, `IS_THRESHOLD` : la provision d'impôt prise sur le surplus de chaque semaine, au taux réduit jusqu'au seuil annuel, au taux normal au-delà. Valeurs françaises par défaut : 15 %, 25 %, 42 500 Ẑ. À faire confirmer par votre comptable ; elles valent dès la clôture suivante, en ẐEN seulement (comme la règle 3×1/3).

## PAF suggérée {#shares}

`PAF` : la participation aux frais de la machine **proposée** aux stations, en Ẑ par semaine. Ce n'est pas un ordre : chaque Capitaine fixe la PAF de sa station dans *Économie*, en partant de cette valeur. Détails : guide, [la PAF de la machine](GUIDE.md#la-paf-de-la-machine-et-les-nouvelles-machines).

## Retours des utilisateurs (Git) {#api}

Les bogues et idées envoyés depuis les applications (coracle, zelkova…) deviennent des tickets dans `<GIT_OWNER>/<application>`. Sans forge, ils restent dans le journal de la station.

| Champ | Où le trouver |
|---|---|
| `GIT_HOST` | Vide ou `https://github.com` : GitHub. Sinon l'adresse de votre GitLab (`https://gitlab.exemple.org`). |
| `GIT_OWNER` | L'organisation ou le compte qui possède les dépôts des applications (`papiche` par exemple). |
| `GIT_TOKEN` | **GitHub** : *Settings › Developer settings › Fine-grained tokens*, propriétaire = `GIT_OWNER`, droit **Issues : Read and write**. **GitLab** : *Préférences › Jetons d'accès*, portée `api`. |

## Reconnaissance des plantes (Pl@ntNet) {#plantnet}

Chaque photo a sa **fiche plante** (bouton « 🌿 Fiche plante » de la visionneuse) : l'espèce, puis *Comestible ?*, *Préparation*, *Propriétés médicinales*, *Dangers* et des notes personnelles, préremplies depuis Wikipédia dans la langue du membre (extraits des sections qui en parlent, source citée, CC BY-SA) et toujours modifiables. La fiche est un fichier chiffré de son uCloud (`/_/Plantes/`), emporté par sa sauvegarde, et tout ce qu'elle contient entre dans sa recherche (« plante », le nom, « comestible », « médicinale »…). Les plantes très toxiques souvent confondues avec des plantes comestibles (colchique, ciguës, vératre, muguet, digitale…) reçoivent un avertissement d'une liste embarquée. Sans clé, le membre écrit lui-même le nom de la plante.

Quand la clé est renseignée, la fiche propose en plus **« Reconnaître la plante »**. Rien ne part seul : c'est le membre qui la demande, photo par photo. La station envoie une copie réduite (petit côté 1 280 px) et ré-encodée de la photo, **sans lieu ni date** (aucun EXIF), à `my-api.plantnet.org` ; la clé ne quitte jamais la station.

| Champ | Où le trouver |
|---|---|
| `PLANTNET_API_KEY` | Compte sur **my.plantnet.org** › *Your API key*. L'offre gratuite compte 500 reconnaissances par jour et par clé (vérifié : la réponse de Pl@ntNet donne le reste du jour), pour **toute** la constellation ; au-delà, la station dit que le quota est épuisé. Les stations Astroport.ONE lisent la même clé. |
| *Clé de cette station seulement* | Votre propre clé Pl@ntNet, quand celle de la coopérative ne suffit plus. Elle **passe avant** celle de la coopérative, pour vos seuls membres. Elle n'est **jamais publiée** et reste sur votre station. Cochez « revenir à la clé de la coopérative » pour la retirer. |

## Stations Astroport.ONE {#legacy}

astroportd publie et garde ces clés **sans s'en servir** : seules les stations Astroport.ONE (les scripts bash) de la constellation les lisent. Une constellation 100 % astroportd peut les laisser vides.

| Champ | Rôle | Où le trouver |
|---|---|---|
| `TVA_RATE` | Taux de TVA (20 % par défaut). | Votre comptable. |
| `ZENCARD_SATELLITE`, `ZENCARD_CONSTELLATION` | Prix des parts sociales (50 € et 540 € par défaut). | Décision de la coopérative. |
| `NCARD`, `ZCARD` | Loyers hebdomadaires d'un MULTIPASS et d'une ZEN Card. | Décision de la coopérative. |
| `TIER_SLUG_CLOUD` | Paliers OpenCollective « cloud » de `oc2uplanet.sh`. | Voir [Routage des paliers](#tiers). |
| `MJ_APIKEY_PUBLIC`, `MJ_APIKEY_PRIVATE` | Envoi des courriels. | **app.mailjet.com** › *Paramètres du compte* › *Gestion des clés API* : *API Key* et *Secret Key*. |
| `MJ_SENDER_EMAIL` | Expéditeur des courriels. | Une adresse **validée** dans Mailjet (*Adresses et domaines d'expéditeur*). |
