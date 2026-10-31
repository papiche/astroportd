## astroportd 0.4.0-beta

Cette version fait de la **panne de sa station** un incident ordinaire : on retrouve son compte, ses amis et ses fichiers sur une autre station avec une simple carte imprimée, sans que personne n'ait rien à faire. Elle précise aussi, dans le README, ce qu'est astroportd et ce qui le distingue.

### Retrouver son compte : la carte secrète
- **Carte secrète** (`astro-secret:v1|…`) remise à la création, en texte et en **QR imprimable** (page `/register`, fichier `.txt`, `astroportd multipass create`). Elle contient SALT, PEPPER, la formule de clé et l'empreinte de la constellation : **qui la possède a la clé du compte**, à garder hors ligne. Elle remplace le découpage SSSS 2-sur-3, volontairement abandonné.
- **Rétablir un compte** sur n'importe quelle station de la même constellation : page `/restore` (scan du QR ou collage), `POST /api/v1/restore`, ou `astroportd multipass recover -`. Même clé, donc mêmes amis, contacts et fichiers ; un **nouveau PASS** est généré (l'ancien n'est pas récupérable hors ligne). Refus avant toute écriture si la carte vient d'une autre constellation ou si les secrets ne redonnent pas la clé annoncée.
- **`astroportd multipass card <email> [--svg]`** réaffiche la carte d'un compte existant ; la formule de clé est retrouvée par la clé elle-même, pas sur parole.
- **Les amis restituent d'eux-mêmes** : la station du membre rétabli publie sa nouvelle station d'accueil (DID) et le dit à chaque proche ; leurs stations mettent à jour son adresse et renvoient le manifeste, puis les fichiers sauvegardés ou partagés. Le VFS et le trousseau reviennent, l'accord de sauvegarde est rétabli, les fichiers manquants sont redemandés par leur CID. Sans manifeste retrouvé en 3 jours, le compte repart à neuf et le Capitaine est alerté.
- **Correctif de fond** : le **blob du manifeste** de l'uCloud n'était pas envoyé aux amis (seul l'événement 30078 voyageait) ; il part désormais en premier chaque nuit et le nettoyage ne le retire plus.
- **Déménager** : la nouvelle station écrit à l'ancienne un message `moved`, signé de la clé du compte ; l'ancienne cesse d'héberger ce compte et met son dossier de côté dans `.moved/`, sans rien effacer. Ignoré pour un Capitaine, pour une station qui est elle-même la nouvelle Home, ou s'il s'agit du rejeu d'un message antérieur.
- **Garde-fous** : aucun événement de naissance n'est republié lors d'une récupération (il écraserait la vraie liste d'amis) ; aucun manifeste vide n'est publié tant que la restauration dure.

### FaceCloud, stockage et exploitation
- **Zone de précaution** : un visage flou ou à score limite n'est plus envoyé tout seul ; les photos attendent une validation groupée par ami (`/api/v1/faces/pending-shares`). Un écart aléatoire sépare les envois NIP-59 vers des stations distantes.
- **Garde d'espace disque** : un dépôt est refusé avant tout fichier temporaire si la partition passerait sous une réserve (le plus grand de 500 Mo et 5 %).
- **Journal WAL** tronqué chaque nuit, et pause entre les étapes lourdes du 20h12 pour laisser passer le trafic sur un petit processeur.

### Port par défaut : 18080
Sans domaine, l'interface écoute désormais sur **`127.0.0.1:18080`** (et non plus 8080, souvent pris par la passerelle IPFS). `--http-listen` reste libre.

### README
Nouvelle ouverture : ce qu'astroportd permet de faire en clair, et un tableau de ce qui le distingue, rédigé d'après le code.

### À savoir avant de mettre à jour
- **Port** : si vous utilisiez l'adresse par défaut, mettez à jour vos favoris et votre proxy (`127.0.0.1:18080`) ou passez `--http-listen 127.0.0.1:8080`.
- La base gagne une table (`face_share_pending`) : elle se crée à la première ouverture. **Faites une copie de `uplanet.db`** avant la première mise à jour d'une station réelle.
- Une station qui doit **céder** un compte (déménagement) doit être à jour pour comprendre `moved` ; sinon arrêtez-la.
- Limites : un fichier qu'un ami vous avait envoyé revient seulement si cet ami est joignable ; seuls les amis réciproques participent.
