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
- Le binaire racine (`src/`) assemble le tout : `main.rs` (CLI clap : `init`, `serve`, `multipass`, `doctor`, `replay`, `media`, `plugin`, `route`, `ylevel`, `swarm-key`…), `serve.rs` (câblage du démon et de ses tâches de fond), `ui_backend.rs` (implémentation de l'API pour `astro-ui`), `solar_cron.rs` (nuit de 20h12 : snapshot `VACUUM INTO`, purges, RÊVE, clôture hebdomadaire 3×1/3, kind 30850 `economic-health`), plus un module par fonction (`register`, `invites`, `sharing`, `share_inbox`, `tunnel_policy` (politique des tunnels du Capitaine : mode par service, SSH fermé par défaut sauf ORIGIN, bloc `authorized_keys` balisé), `assist` (demandes d'assistance entre Capitaines amis : l'accès n'est ouvert qu'à l'ami qui accepte), `build_attest` (attestation de build kind 30853 : version + SHA-256 du binaire, comparée entre stations sur la carte Constellation), `forwards` + `console` (accès local à chaud vers l'ami aidé et console SSH dans le navigateur : `ssh` lancé par la station dans un pty, pont WebSocket d'`astro-ui`), `library` (requêtes de la bibliothèque : déjà-là, souvenirs, chronologie, recherche, doublons), `integrity` (contrôle nocturne des blobs), `private_backup` (sauvegarde chiffrée chez un ami qui accepte, envoyée à 20h12), `zen_react`, `upload_worker`…). `src/lib.rs` expose ces modules pour que `tests/` les teste de bout en bout.

**Deux constellations** : pas de `swarm.key` = ORIGIN (`UPLANETNAME` = 64 zéros), `swarm.key` présente = ẐEN (réseau P2P privé). Une station = une constellation. Les clés d'un MULTIPASS sont dérivées d'une formule **isolée** `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` : le même couple donne d'autres clés dans une autre constellation (`KeyFormula` dans `astro-core::formula`, vecteurs de compatibilité dans `crates/astro-core/tests/legacy_keygen.rs`).

**Y-Level** : la clé SSH est hachée en SALT/PEPPER → scrypt → graine ed25519 d'où viennent clé SSH jumelle, PeerID IPFS, clé Ğ1 et clé Nostr (`astroportd init` / `ylevel`).

**Données de la station** : clés et identités sont découvertes dans `~/.zen/game` (`--game-dir` ou `ASTRO_GAME_DIR`) par `astro-lifecycle::discovery` ; secrets en fichiers `0600` vérifiés par `doctor`.

## Tests

Les tests d'intégration du binaire sont dans `tests/` (`ui_e2e.rs`, `full_cycle.rs`, `media_pipeline.rs`, `upload_worker.rs`…) et utilisent une vraie base, un vrai WebDAV et de vrais pairs libp2p ; chaque crate a ses propres `tests/` (dont `astro-crypto/tests/nip44_interop.rs` et `uenc_v1_vector.rs`, des vecteurs de compatibilité à ne pas modifier pour faire passer un test). `astro-facecloud` avec `--features onnx` n'est que compilé en CI : la bibliothèque `libonnxruntime` est chargée à l'exécution.

## Dépôt

- Ce dépôt fait partie du workspace `AAA/` (voir le `CLAUDE.md` parent pour l'écosystème Astroport.ONE / UPassport / NIP-101) ; ici, astroportd remplace strfry + `writePolicy`, `gcli` et plusieurs scripts bash d'Astroport.ONE.
- Ne jamais ajouter de `Co-Authored-By` dans les commits (règle du workspace parent, qui prime sur le reste).
- Les fichiers `*.db`, `blobs/`, `*.key`, `swarm.key`, `*.dunikey`, `secrets/` sont ignorés par git : ne les ajoutez jamais.
- La version est dupliquée dans `Cargo.toml` (paquet racine et `[workspace.package]`) : mettez les deux à jour ensemble.
