# Plan : découper `ui_backend.rs` et le trait `Backend`

État : **phases 0 à 4 terminées** ; les alias de closures sont conservés par décision du mainteneur (voir « Avancement » en fin de document). Document de travail pour le mainteneur.

## 1. Le constat, chiffré

| Fichier | Lignes | Rôle |
|---|---|---|
| `crates/astro-ui/src/api.rs` | 2 925 | trait `Backend` (≈ 182 méthodes), tous les handlers HTTP, le routeur (≈ 164 appels `st.backend`) |
| `src/ui_backend.rs` | 2 418 | **l'unique** `impl Backend for StationUi<B>` ; `StationUi` porte une quarantaine de champs (`Option<Arc<…>>` et alias de closures `Fn`) |

Deux fichiers de la taille d'un module entier, couplés par un trait unique. Le « God Object » n'est donc pas seulement `StationUi` : c'est le couple **trait `Backend` + son implémentation**.

### Pourquoi on ne peut pas simplement « couper le fichier en quatre »

1. **Un seul bloc `impl Trait for Type` par trait.** Rust interdit de répartir l'`impl Backend` sur plusieurs fichiers. Il faut d'abord **scinder le trait**.
2. **Le trait utilise `async fn`** (RPITIT) : il n'est pas utilisable en `dyn Backend`. On ne peut donc pas passer un `Arc<dyn UcloudApi>` : la généricité `B` se propage dans chaque handler (`State<Arc<UiState<B>>>`).
3. **Les fonctionnalités optionnelles** renvoient aujourd'hui `503 « … non disponible sur cette station »` via des méthodes par défaut du trait. Des routes absentes donneraient `404` : c'est un changement de comportement visible des clients (coracle, zelkova) à traiter explicitement.
4. **Gain de compilation :** découper en modules du même crate améliore la lisibilité, **pas** la vitesse. La promesse du conseil initial (« vitesse de compilation ») ne se tient que si les domaines deviennent des **crates** distincts (phase 5, optionnelle).

## 2. Cible

Un routeur **par domaine**, chacun avec un `State` réduit et un trait étroit ; le routeur global n'est plus qu'un `.merge()`.

```text
astro-ui/src/api/
  mod.rs        UiState (activity, started, public_origin), router() = merge des domaines, erreurs, extracteurs
  auth.rs       trait Auth { authenticate }  — le seul trait partagé par tous les domaines
  ucloud.rs     trait UcloudApi   + handlers + routes
  faces.rs      trait FacesApi
  coop.rs       trait CoopApi      (économie, billets, charte)
  social.rs     trait SocialApi    (chat, présence, visio, contacts, liens, BRO, agents)
  account.rs    trait AccountApi   (compte, appareils, clés, restauration)
  onboarding.rs trait OnboardApi   (invitations, accueil, inscription, annuaire des stations)
  station.rs    trait StationApi   (Capitaine : cockpit, admin, domaine, plans, console, santé)
```

Côté binaire, `src/ui_backend.rs` devient `src/ui_backend/{mod,ucloud,faces,coop,social,account,onboarding,station}.rs`. Chaque fichier possède **sa propre structure** (`UcloudBackend`, `FacesBackend`…) qui ne détient que les services dont elle a besoin. `StationUi` disparaît : `serve/ui.rs` construit les domaines activés et les monte.

### Répartition proposée des 182 méthodes

| Domaine | Méthodes (extraits) |
|---|---|
| **auth** | `authenticate` |
| **ucloud** | `ucloud_list/photos/share/send`, `public_move`, `public_images`, `library`, `organize_*`, `pull_*`, `backup_*`, `replication`, `upload_image`, `attach`, `friends` |
| **faces** | `faces`, `face_accept/name`, `faces_board/thumbs/assign/folders/delete_photos/forget`, `face_shares_*`, `rotate_photos`, `profile_image` |
| **coop** | `constellation`, `economy`, `fuel`, `coop_config(_set)`, `cooperative_view`, `charter_accept`, `billet_*`, `account_billet_issuer`, `uplanet_hex`, `check_balance`, `account_oc`, `hosting_defaults`, `account_hosting` |
| **social** | `chat`, `presence`, `call_ice`, `book`, `contacts*`, `my_links`, `announce_links`, `bro_say`, `ai_*`, `agents_*`, `mcp` |
| **account** | `account`, `account_site*`, `account_image/position/profile/unlock/sign`, `pass_unlock`, `account_eject`, `account_token(_rotate)`, `account_quota`, `account_export_keys`, `accounts`, `devices_*`, `restore(_ask)`, `feedback` |
| **onboarding** | `welcome_*`, `join_take`, `invite_*`, `register*`, `register_info`, `stations`, `station_card` |
| **station** | `dashboard`, `footprint`, `energy`, `domain_info/set`, `restart`, `admin_get/set`, `console_open`, `planes`, `plane_add/remove`, `public_origin`, `health` |

À affiner à la lecture du code : quelques méthodes (`friends`, `replication`, `backup_*`) touchent deux domaines.

## 3. Phases (chacune livrable seule, `cargo test --workspace` vert entre chaque)

**Phase 0 — filet de sécurité (1 PR, ~½ jour).**
Un test qui **liste toutes les routes** (méthode, chemin, niveau d'accès : anonyme / membre / Capitaine) et les compare à un instantané versionné. Il garantit qu'aucune route ne disparaît ni ne change de protection pendant le découpage. Compléter par un test « les routes Capitaine répondent 403 à un membre » sur l'ensemble de l'instantané (la protection `captain_only` est aujourd'hui répétée handler par handler).

**Phase 1 — déplacer sans rien changer (1 PR, ~1 jour).**
Découper `api.rs` en modules `api/*.rs` **en gardant le trait `Backend` intact**. Pur déplacement de code. Le diff est énorme mais mécanique ; il se relit avec `git diff --color-moved`.

**Phase 2 — scinder le trait (1 PR, ~1–2 jours).**
Créer les sept sous-traits. `Backend` devient `trait Backend: Auth + UcloudApi + FacesApi + …` (supertrait vide) : `StationUi` compile **sans changement**. Chaque module de handlers se borne à son sous-trait (`fn routes<B: UcloudApi>`), ce qui fait apparaître les couplages cachés.

**Phase 3 — scinder l'implémentation (1 PR par domaine, ~½ jour chacune).**
Déplacer les méthodes de `impl Backend for StationUi` vers `impl UcloudApi for UcloudBackend`, etc. Les champs de `StationUi` migrent vers la structure qui s'en sert. On commence par le domaine **le plus isolé** (`faces`, puis `coop`) pour valider la méthode, on finit par `account` (le plus connecté).

**Phase 4 — retirer `StationUi` et les closures (1 PR, ~1 jour).**
- `router()` prend les domaines activés : `Router::new().merge(ucloud::router(u)).merge(…)`.
- Un domaine désactivé monte un **routeur de repli** qui répond `503` avec le message actuel : le comportement visible ne change pas (point 3 du constat).
- Les alias de closures (`ThumbFn`, `CoopGetFn`, `BilletFn`, `AdminFn`…) sont remplacés par de petits traits implémentés par les services du binaire (inversion de dépendance) : plus d'`Arc<dyn Fn(…) -> Pin<Box<dyn Future…>>>` illisibles.

**Phase 5 — optionnelle : un crate par domaine lourd.**
Seulement si la durée de compilation reste un problème après la phase 4 : extraire `astro-ui-faces`, `astro-ui-coop`. À mesurer **avant** (`cargo build --timings`) pour décider.

## 4. Critères d'acceptation

- `tests/ui_e2e.rs` et l'instantané de routes passent **sans modification** pendant les phases 1 à 3.
- Aucun fichier de `src/ui_backend/` ou `crates/astro-ui/src/api/` ne dépasse ~600 lignes.
- Un `StationUi` ou un `State` ne porte jamais plus de ~8 champs.
- Les budgets de taille du front (`web.rs`) sont inchangés : le front n'est pas touché.
- `cargo clippy -D warnings` (avec et sans `onnx`) vert à chaque PR.

## 5. Risques

| Risque | Parade |
|---|---|
| Une route change de protection par oubli (`captain_only`) | Phase 0 : test d'accès sur l'instantané complet |
| `404` au lieu de `503` pour un domaine désactivé | Routeur de repli (phase 4), vérifié par le test de routes |
| Conflits avec le travail en cours sur `api.rs` / `ui_backend.rs` | Phase 1 en une seule PR, fusionnée vite, sur une branche sans autre chantier |
| Les sous-traits se révèlent trop couplés (méthodes à cheval sur deux domaines) | Tolérer un petit trait partagé (`Auth`, éventuellement `Notify`) plutôt que dupliquer |

## 6. Décision attendue

1. Valider les **sept domaines** et leur nom (ou en proposer d'autres).
2. Accepter le **repli `503`** (recommandé) plutôt que `404` pour un domaine désactivé.
3. Dire si la **phase 5** (crates séparés) est souhaitée ou à reporter.

## 7. Avancement

| Phase | État | Ce qui a été fait |
|---|---|---|
| 0 — filet | fait | `every_route_keeps_its_access_level` (`tests/ui_e2e.rs`) : 163 routes lues dans les sources, statut anonyme et membre comparés à `tests/fixtures/route_access.txt` ; garde-fou « toute route `station/*` est fermée au membre » (la console passe par un ticket). Régénérer l'instantané : `UPDATE_ROUTE_SNAPSHOT=1`. |
| 1 — déplacer | fait | `api.rs` → `api/{mod,ucloud,faces,coop,social,account,onboarding,station}.rs`. Aucune ligne perdue (comparaison avant/après) ; le test de routes est passé sans modification. |
| 2 — scinder le trait | fait | `Auth` (socle : `authenticate`, `public_origin`) + sept sous-traits ; `Backend` n'est plus qu'un supertrait avec implémentation générale. Trois couplages révélés par le compilateur : `public_origin` (rangé dans `Auth`), `restore`/`restore_ask` (accueil), `ai_declared`/`billet_issuer` (ils servent `/api/v1/me`, rangés dans `AccountApi`). |
| 3 — scinder l'implémentation | fait | `src/ui_backend/{mod,…}.rs` : une structure par domaine (`UcloudBackend`, `FacesBackend`, …) de 8 à 10 champs, au lieu des 41 de `StationUi`. Aucune aide n'était partagée entre domaines ; les champs communs (`ai`, `book`, `site`, `backup`…) sont des `Arc` clonés. |
| 4 — retirer `StationUi` | fait (closures conservées) | `astro_ui::api::Routes` monte un routeur par domaine, chacun avec **son** backend (`Routes::new(..).ucloud(a).faces(b)…`). `StationUi::into_router` consomme le plan de câblage au démarrage. **Décision du mainteneur :** les alias de closures (`ThumbFn`, `CoopGetFn`, `BilletFn`, `AdminFn`…) restent en l'état, plutôt que d'être remplacés par de petits traits (une vingtaine de câblages à migrer pour un gain de lisibilité modeste). |

### Écarts avec les critères d'acceptation

- `StationUi` garde ses 41 champs : c'est désormais un **plan de câblage** (constructeur `with_*`) consommé par `into_router`, plus un objet d'exécution. Les structures d'exécution, elles, respectent la borne (≤ 10 champs).
- `src/ui_backend/mod.rs` fait 661 lignes et `crates/astro-ui/src/api/mod.rs` 912 (dont ≈ 400 de tests) : au-dessus des ~600 visés. Les modules de domaine restent sous 430 lignes.
- Aucun `404` nouveau : chaque domaine est toujours monté, et ses fonctions optionnelles renvoient le même `503` qu'avant. Le « routeur de repli » prévu en phase 4 n'est donc pas nécessaire.
- Le découpage en modules ne change pas la durée de compilation (voir §1, point 4) ; la phase 5 reste à décider après mesure.
