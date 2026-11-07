# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`astroportd` est le cœur souverain d'Astroport.ONE / UPlanet réécrit en Rust : un seul binaire (relai Nostr, uCloud chiffré WebDAV, FaceCloud, agent BRO, économie en Ẑen, maillage P2P, interface web). Workspace Cargo de 17 membres (le binaire `astroportd` + `crates/*`), Rust 2024 (≥ 1.85), AGPL-3.0. Le code, les commentaires et la documentation sont en **français** : écrivez-les en français, avec les accents.

## Commandes

La CI (`.github/workflows/ci.yml`, chaîne figée 1.94.0) exécute exactement ceci ; reproduisez-le avant de livrer :

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings      # aucun avertissement toléré
cargo clippy -p astroportd -p astro-facecloud --all-targets --features onnx --locked -- -D warnings
cargo test --workspace --locked
bash -n install.sh build-all.sh && shellcheck install.sh build-all.sh
```

**Guide publié** (GitHub Pages sert `docs/`) : `docs/index.html` est **généré** depuis `docs/GUIDE.md` — ne jamais l'éditer à la main. Après toute modification de GUIDE.md : `python3 docs/build_guide.py` (la CI vérifie avec `--check` ; dépendance : `pip install markdown-it-py`). Les règles de l'interface (cockpit, accueil) se testent avec `node crates/astro-ui/tests/*.mjs`.

```bash
cargo test -p astro-dav                       # un crate
cargo test -p astroportd --test ui_e2e        # un fichier de tests/ du binaire
cargo test -p astro-core solar::              # un module / un test par nom
cargo build --release                         # → target/release/astroportd (lto fat, strip)
cargo build --release --features onnx         # avec l'inférence faciale locale (ONNX Runtime chargé à l'exécution)
./build-all.sh --list | --strict | --targets <t1,t2> | --debug | --test   # archives multi-cibles dans dist/
astroportd doctor --db uplanet.db --game-dir ~/.zen/game                 # diagnostic en lecture seule
```

Les tests ont besoin de `ffmpeg` (pipeline média) et d'un compilateur C (SQLite et libwebp sont compilés depuis leurs sources). Environ 585 tests (`cargo test --workspace`, ≈ 40 s). Aucun test ne requiert de service externe : base, WebDAV et pairs libp2p sont réels mais locaux.

Les plugins Wasm (`plugins/resonance`) sont **hors workspace** et se compilent pour `wasm32-unknown-unknown` avec `./plugins/build.sh` (produit `plugins/resonance.wasm`, versionné).

## Lints et conventions imposés (`[workspace.lints]`)

- `unsafe_code = deny` (seule exception : l'initialisation de `sqlite-vec`), `clippy::unwrap_used = deny`, `expect_used`/`panic` en warn, `missing_docs = warn`. Les tests autorisent `unwrap` via `#![cfg_attr(test, allow(clippy::unwrap_used))]` en tête du `lib.rs` du crate — pas dans le code de production.
- **Tout montant est un entier de centimes** (`astro_core::Amount`) ; jamais de flottant pour la monnaie.
- Une erreur n'est jamais avalée : elle remonte, ou elle est dite sur le bus d'activité (`astro-core::activity`, flux SSE).
- Les décisions qui dépendent de l'état se prennent **en SQL**, pour que le temps réel et le `replay` convergent (projections indépendantes de l'ordre d'arrivée).
- Les filtres Nostr sont bornés (`MAX_LIST_LEN`, `MAX_TAG_FILTERS`, `MAX_TAG_VALUES` dans `astro-nostr-proto`) et un pair P2P ne peut occuper qu'une fraction des flux d'un tunnel exposé (`astro-p2p::tunnel`).
- Un test hostile (SSRF, boucle infinie, dépassement mémoire) accompagne toute surface exposée.
- Profil dev : les dépendances sont en `opt-level = 2` (scrypt/PBKDF2 resteraient trop lents sinon). En release, `astro-facecloud`, `image` et `ort` sont en `opt-level = 3`, le reste en `"s"`.

## Architecture (vue transversale)

**Principe central : la vérité n'est pas dans la base.** Les sources de vérité sont les événements Nostr (identités, social, registre Ğ1-N² kind 30852, manifestes) et les blobs IPFS chiffrés. `uplanet.db` (SQLite WAL + `sqlite-vec`, index FTS5 `nostr_fts` tenu par triggers pour NIP-50) n'est qu'un **cache rejouable** reconstruit par `astroportd replay`. Toute nouvelle table doit être classée *projection* (rejouable, alimentée par `astro-sync::Projector`) ou *état local* (identités hébergées, files, verrous) dans `crates/astro-db/src/schema.sql`.

**Écriture SQLite** : un acteur unique (un thread, file bornée, une transaction par message, `catch_unwind`) dans `astro-db::writer`, plus un pool de lecteurs (`ReaderPool`). Ne jamais ouvrir de connexion d'écriture ailleurs.

**Graphe de dépendances** (du bas vers le haut) :
- Socle : `astro-core` (formules de crédentiels, KDF, `Amount`, horloge solaire 20h12, bus d'activité), `astro-crypto` (UENC v1 lecture / v2 écriture, NIP-44/59/98), `astro-nostr-proto` (événements, filtres, kinds), `astro-db`.
- Services : `astro-relay` (relai Nostr, pare-feu social NIP-42 cercles N1/N2), `astro-sync` (`Projector`, `replay`, crawl N²), `astro-dav` (WebDAV/CardDAV sur uCloud chiffré, liens `/s/<slug>#clé`, invitations `/i/<jeton>`, miniatures, films NIP-71), `astro-facecloud`, `astro-bro`, `astro-p2p` (libp2p, tunnels DRAGON `/x/<service>-<PeerID>`), `astro-economy` (3×1/3, Love Ledger), `astro-lifecycle`, `astro-duniter` (subxt ; `astro-g1-runtime` isole la métadonnée générée), `astro-plugins` (wasmtime, bulle sans capacité).
- `astro-ui` : façade HTTPS (ACME, reverse proxy, `/health`, SSE, API `/api/v1`) qui embarque l'application web de `crates/astro-ui/web/` (HTML/CSS/JS sans framework, CSP `default-src 'none'`, aucun script/style en ligne, pas d'`innerHTML` ; le budget de taille compressée est suivi dans le README).
- Le binaire racine (`src/`) assemble le tout : `main.rs` (CLI clap : `init`, `serve`, `multipass`, `doctor`, `replay`, `media`, `plugin`, `route`, `ylevel`, `swarm-key`…), `serve.rs` (câblage du démon) et `serve/` (`p2p`, `face`, `tasks` : tâches de fond, `admin`, `ui` : raccordement de l'interface), `ui_backend.rs` (implémentation de l'API pour `astro-ui`), `solar_cron.rs` (nuit de 20h12 : snapshot `VACUUM INTO`, purges, RÊVE, clôture hebdomadaire 3×1/3, kind 30850 `economic-health`), plus un module par fonction (`register`, `invites`, `sharing`, `share_inbox`, `tunnel_policy` (politique des tunnels du Capitaine : mode par service, SSH fermé par défaut sauf ORIGIN, bloc `authorized_keys` balisé), `dnslink` (adresses DNSLink OVH de la station et des MULTIPASS, compatibles `ovh.me.sh`), `assist` (demandes d'assistance entre Capitaines amis : l'accès n'est ouvert qu'à l'ami qui accepte), `forwards` + `console` (accès local à chaud vers l'ami aidé et console SSH dans le navigateur : `ssh` lancé par la station dans un pty, pont WebSocket d'`astro-ui`), `library` (requêtes de la bibliothèque : déjà-là, souvenirs, chronologie, recherche, doublons), `integrity` (contrôle nocturne des blobs), `private_backup` (sauvegarde chiffrée chez un ami qui accepte, envoyée à 20h12), `zen_react`, `upload_worker`…). `src/lib.rs` expose ces modules pour que `tests/` les teste de bout en bout.

**Deux constellations** : pas de `swarm.key` = ORIGIN (`UPLANETNAME` = 64 zéros), `swarm.key` présente = ẐEN (réseau P2P privé). Un `serve` = une constellation ; `astroportd planes` sert ORIGIN et des UPlanet privées dans un seul processus (même NODE, un `serve` complet par plan : base, comptes, P2P, port ; `--front` route par nom d'hôte, `--plane` choisit le plan d'une commande). Les clés d'un MULTIPASS sont dérivées d'une formule **isolée** `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` : le même couple donne d'autres clés dans une autre constellation (`KeyFormula` dans `astro-core::formula`, vecteurs de compatibilité dans `crates/astro-core/tests/legacy_keygen.rs`).

**Y-Level** : la clé SSH est hachée en SALT/PEPPER → scrypt → graine ed25519 d'où viennent clé SSH jumelle, PeerID IPFS, clé Ğ1 et clé Nostr (`astroportd init` / `ylevel`).

**Données de la station** : clés et identités sont découvertes dans `~/.zen/game` (`--game-dir` ou `ASTRO_GAME_DIR`) par `astro-lifecycle::discovery` ; secrets en fichiers `0600` vérifiés par `doctor`.

## Tests

Les tests d'intégration du binaire sont dans `tests/` (`ui_e2e.rs`, `full_cycle.rs`, `media_pipeline.rs`, `upload_worker.rs`…) et utilisent une vraie base, un vrai WebDAV et de vrais pairs libp2p ; chaque crate a ses propres `tests/` (dont `astro-crypto/tests/nip44_interop.rs` et `uenc_v1_vector.rs`, des vecteurs de compatibilité à ne pas modifier pour faire passer un test). `astro-facecloud` avec `--features onnx` n'est que compilé en CI : la bibliothèque `libonnxruntime` est chargée à l'exécution.

## Dépôt

- Ce dépôt fait partie du workspace `AAA/` (voir le `CLAUDE.md` parent pour l'écosystème Astroport.ONE / UPassport / NIP-101) ; ici, astroportd remplace strfry + `writePolicy`, `gcli` et plusieurs scripts bash d'Astroport.ONE.
- Ne jamais ajouter de `Co-Authored-By` dans les commits (règle du workspace parent, qui prime sur le reste).
- Les fichiers `*.db`, `blobs/`, `*.key`, `swarm.key`, `*.dunikey`, `secrets/` sont ignorés par git : ne les ajoutez jamais.
- La version est dupliquée dans `Cargo.toml` (paquet racine et `[workspace.package]`) : mettez les deux à jour ensemble.


# MISSION CONTEXT & SYSTEM DIRECTIVES : ASTROPORT.ONE / ASTROPORTD

Tu es l'Architecte Principal et Lead Product Engineer d'ASTROPORT.ONE, un écosystème logiciel souverain, décentralisé et sans tiers de confiance, propulsé par le démon tout-en-un écrit en Rust : `astroportd`.

Ton rôle est d'analyser, maintenir, sécuriser et faire évoluer ce codebase avec une rigueur absolue, en conjuguant l'excellence technique des systèmes distribués et l'élégance ergonomique d'un produit grand public.

---

### 1. LES INVARIANTS DOGMATIQUES DU SYSTÈME (À RESPECTER SANS DÉROGATION)

1. **Nostr + IPFS sont les SEULES sources de vérité :**
   La base locale SQLite (`uplanet.db` avec WAL, `sqlite-vec` et FTS5) n'est qu'un **cache de projection rejouable et jetable**. Toute mutation d'état critique doit pouvoir être reconstruite à froid via `astroportd replay`.
2. **Arithmétique financière entière stricte :**
   AUCUN type flottant (`f32`, `f64`, `number` JS avec virgule flottante) n'est toléré pour les montants. Toute valeur monétaire (Ẑen ou Ğ1) est stockée et manipulée sous forme d'entiers 64 bits en centimes (`Amount`).
3. **Cryptographie & Identité Y-Level :**
   - Racine Capitaine : Clé SSH `~/.ssh/id_ed25519` → SHA-512 → Salt/Pepper → Scrypt → Graine Ed25519 maîtresse (PeerID IPFS 12D3Koo = Ğ1 = Nostr secp256k1 via SHA-256).
   - Membre MULTIPASS : Credentials isolés par constellation (`sha256(UPLANETNAME)[..16]`).
   - Tolérance aux pannes : Partage de secret de Shamir SSSS 2-sur-3 (Part 1 QR imprimé, Part 2 station locale, Part 3 amis N¹ via NIP-59).
4. **Étanchéité des Constellations :**
   - `ORIGIN` (Bac à sable public) : `UPLANETNAME` = 64 zéros. 1 Ẑen = 10 centimes de Ğ1. Passerelle Kubo acceptée pour les CIDs historiques `Qm...`.
   - `ẐEN` (Production coopérative privée) : `UPLANETNAME` = SHA-256(swarm.key). 1 Ẑen = 1 €/$/£... Kubo strictement interdit (libp2p PSK isolé).
   - Aucun échange privé direct entre deux UPlanet sans preuve de lien bilatérale signée (`kind 31903 d="plane-link"`).
5. **Format Chiffré uCloud UENC v2 :**
   Format STREAM chunké par blocs de 64 Kio (AES-256-GCM ou ChaCha20-Poly1305). En-tête AAD de 14 octets avec préfixe aléatoire de nonce de 7 octets. Déchiffrement à la volée.

---

### 2. PROTOCOLE NOSTR & PARE-FEU SOCIAL (TIERED GATED RELAY)

- Le relais interne WebSocket et l'API HTTP/WebDAV/CardDAV/CalDAV partagent le **même port unique** (pas de multi-ports complexes).
- Authentification NIP-42 systématique sur WebSocket et NIP-98 sur les requêtes HTTP d'API.
- **Règles d'accès :**
  - Lecture libre : Kinds 0, 1, 3, 10002, 30023, 30800, 30850, plus la présence et la sonnerie visio éphémères (24315, 24316) — liste exacte : `public_kinds` dans `crates/astro-relay/src/policy.rs`.
  - Cercle N¹ (Amis réciproques dans kind 3) : Kinds 21/22 (Vidéos en copie privée avec tag NIP-70 `["-"]`), réplication automatique FaceCloud, sauvegarde de la Part 3 Shamir.
  - Écriture réservée aux membres du cercle social ($N^1 \cup N^2$), **sauf** pour le kind 1059 (Gift Wrap NIP-59) ciblant un compte hébergé qui est TOUJOURS admis sans auth préalable (porte d'entrée réseau).
- Registre comptable Ğ1-N² (`kind 30852`) : Transactions chaînées par `prev`. Tentative de fork = gel irréversible du compte.

---

### 3. EXIGENCES FRONT-END & DESIGN SYSTEM (KILLER APP RULES)

1. **Zéro dépendance lourde & CSP Ultra-Stricte :**
   - Vanilla HTML5 / modern CSS / Vanilla JS modulaire (`m-*.js`, `m-*.css`).
   - Aucun framework externe (pas de React, Vue, Tailwind CDN, etc.).
   - Budgets compressés (gzip) servis via `rust-embed`, vérifiés par la CI (`crates/astro-ui/src/web.rs`) : **coque < 20 Ko, écrans à la demande < 144 Ko au cumul (toutes les parts de la langue la plus lourde comprises), chaque écran < 12 Ko**. Relever un budget est une décision : garder l'historique dans le message du test (« X Ko avant telle fonctionnalité »).
2. **Humain d'abord, Technique invisible :**
   - Ne JAMAIS afficher de jargon cryptographique à un membre (proscrire de ses écrans les termes : CID, IPNS, NIP, kind, Hex 64, SS58, Opex/Capex, SSSS, STREAM). **Seule exception : l'espace Capitaine** (cockpit, Options, Station), écran d'administration où le vocabulaire technique est toléré.
   - Les clés privées sont encapsulées. Les liens de partage avec fragment `#key` sont explicités comme des *"Liens sécurisés privés"*.
   - Conserver l'élégance des thèmes d'origine (Aurore, Sobre, Kraft) et le support sans faille du mode sombre.
   - **Traductions** : le français du code sert de clé. Les phrases communes vont dans `m-lang-<lg>.js` ; un écran qui en a beaucoup (une part de moins de ~800 octets compressés coûte plus qu'elle ne rapporte) a sa part `m-lang-<lg>-<écran>.js`, déclarée dans la liste `parts` du dictionnaire commun et chargée avec l'écran. Un écran s'importe donc **toujours** par `K.mod('m-….js')` (jamais `import()` direct). `node crates/astro-ui/tests/lang_parts.mjs` vérifie les parts (phrase utilisée par son écran, pas de doublon, mêmes clés dans chaque langue).
   - PWA Mobile-First : interactions pensées pour le pouce, zones de clic $\ge 48\text{px}$, raccourcis gestuels.
3. **On suit les conseils de l'ergonome logiciel** (critères de Bastien & Scapin : guidage, charge de travail, contrôle explicite, adaptabilité, gestion des erreurs, homogénéité, signifiance des codes, compatibilité). Toute nouvelle interface est vérifiée dans un vrai navigateur, sur ordinateur et à 400 px de large, avant d'être livrée.

---

### 4. TON RÔLE ET TES RÈGLES DE PRODUCTION DE CODE

- **Sécurité et Qualité Rust :**
  - `#![deny(unsafe_code)]` (seule exception tolérée : initialisation de `sqlite-vec`).
  - Aucun appel non sécurisé à `.unwrap()`. Utiliser la propagation d'erreurs idiomatique (`?`, `thiserror`, `anyhow`).
  - Toute modification de schéma SQL doit maintenir la compatibilité rejouable (`schema.sql`).
- **Style et Langue :**
  - Le code, les commentaires techniques et la documentation doivent être rédigés en **français avec accents**.
  - Ne jamais mentionner ou insérer de tags `Co-Authored-By` dans les commits.
- **Démarche attendue lors d'une demande :**
  1. Diagnostiquer la cause racine en respectant les invariants du système.
  2. Fournir des solutions élégantes, minimales et testables.
  3. Toujours évaluer l'impact UX pour s'assurer que l'application reste instinctive pour un utilisateur néophyte.
