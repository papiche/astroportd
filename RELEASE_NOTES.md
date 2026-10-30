## astroportd 0.3.0-beta

Cette version rend la station **partageable avec le collectif sans rien ouvrir par défaut**, ajoute l'entraide entre Capitaines amis et une vraie **bibliothèque** de photos, et reprend l'interface écran par écran.

### Accès et entraide entre Capitaines
- **Politique de tunnels** : chaque service partagé (SSH, ComfyUI, Ollama) a son mode — fermé, liste, Capitaines amis, tout le réseau — réglable à chaud dans Options. Le SSH est **fermé par défaut** en ẐEN et ouvert aux Capitaines amis en ORIGIN ; `authorized_keys` n'est touché que dans un bloc balisé.
- **Demande d'assistance** : le Capitaine demande de l'aide à tous ses Capitaines amis ou à un seul (ou au premier qui accepte). Rien ne s'ouvre à l'envoi : l'accès n'est accordé qu'à l'ami qui accepte, pour la durée choisie, puis se referme seul.
- **Console SSH dans le navigateur** : l'ami aidant ouvre l'accès local à chaud puis un terminal (xterm.js) relié à un `ssh` lancé par la station dans un pseudo-terminal. Ticket de 30 s à usage unique, `Origin` vérifiée, session tuée à l'échéance de l'aide.
- **Code identique entre stations** : chaque station publie l'empreinte SHA-256 de son binaire (kind 30853) ; la carte Constellation la compare à la vôtre. L'attestation est déclarative : elle repère une dérive honnête, pas une station qui mentirait.
- Le relai `strfry` n'est plus exposé au maillage ; seuls `ssh`, `comfyui` et `ollama` sont partagés.
- Retrait du code d'achat et de location de GPU et des baux de disque.

### Données
- **Sauvegarde privée** chez un ami qui accepte (chiffrée, comptée dans son quota), synchronisée à 20h12, nettoyage nocturne avec 7 jours de grâce.
- **Y-Level** : une clé déjà Y n'est plus jamais re-dérivée sans le dire (`--rederive-y` explicite).
- **Relai Nostr complet** : COUNT (NIP-45), recherche NIP-50 par index FTS5 (préfixes, sans casse ni accents, classée par pertinence), suppressions NIP-09, expiration NIP-40, événements protégés NIP-70, filtres bornés.
- **Bibliothèque** (`#/library`) : « ce jour-là », chronologie par mois, recherche à filtres (nom, description, type, personne, dates, zone), doublons exacts et visuels (empreinte perceptuelle), suppression des doublons **après confirmation**, et `POST /api/v1/ucloud/have` pour n'envoyer que ce qui manque. Tout est par compte.
- **Santé du stockage** : à 20h12, relecture par lots des blobs référencés (absents ou corrompus signalés au Capitaine, rien n'est réparé ni supprimé) ; Options › File de traitement montre les tâches abandonnées et permet de les relancer.
- **FaceCloud** : un visage flou (score de détection < 0,70) ne forme plus de groupe sur la table de tri.

### Interface
- Options replié avec résumés, formulaires et cases à cocher corrigés, pastille « Aide » pour les demandes reçues, tableaux en fiches sur téléphone, raccourcis clavier (visionneuse : Début, Fin, `s` ; liste : Maj+clic, Ctrl+A).
- Écrans Options et Assistance en français, anglais et espagnol ; messages techniques reformulés (plus d'options de ligne de commande à l'écran).

### À savoir avant de mettre à jour
- La base gagne des tables et un index de recherche : ils se créent à la première ouverture. **Faites une copie de `uplanet.db`** avant la première mise à jour d'une station réelle.
- La console SSH et l'accès local à chaud ne fonctionnent que sous Unix.
- Budgets d'interface : écrans à la demande 48 Ko Gzip au total et 12 Ko par écran (un seul dictionnaire de langue compté).
