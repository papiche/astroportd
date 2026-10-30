-- astro-db : schéma de uplanet.db
--
-- RÈGLE FONDATRICE : Nostr et IPFS sont les seules sources de vérité.
-- Ce fichier est un CACHE ET UN INDEX DE RECHERCHE reconstruisible par `astroportd replay`.
--   * Section 1 « PROJECTION » : tout est rejouable depuis des événements Nostr et des CID IPFS.
--     `src_event` = id de l'événement Nostr d'origine, `src_cid` = CID IPFS du blob d'origine.
--   * Section 2 « LOCAL » : état volontairement non rejouable (anti-rejeu, files, journaux).
--     Sa perte est sans conséquence au-delà de la fenêtre en cours.
-- Aucune clé racine (nsec, dunikey, SSSS) n'est stockée ici.

PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 30000;

CREATE TABLE IF NOT EXISTS schema_info (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT, WITHOUT ROWID;
INSERT OR IGNORE INTO schema_info(key, value) VALUES ('schema_version', '1');

-- ═══════════════════════════ 1. PROJECTION (rejouable) ═══════════════════════════

-- Curseur de rejeu par source (relai, filtre, backup CID) : permet de reprendre `replay`.
CREATE TABLE IF NOT EXISTS replay_state (
  source     TEXT PRIMARY KEY,         -- ex. 'relay:wss://…', 'backup:<hex>'
  cursor     TEXT,                     -- since=<ts> ou dernier event id
  updated_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Événements du relai local (copie de travail ; la vérité reste le relai/la constellation).
CREATE TABLE IF NOT EXISTS nostr_events (
  id         TEXT PRIMARY KEY,
  pubkey     TEXT NOT NULL,
  kind       INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  content    TEXT NOT NULL,
  sig        TEXT NOT NULL,
  tags       TEXT NOT NULL,            -- JSON
  d_tag      TEXT,                     -- kinds remplaçables paramétrés (30000-39999)
  expires_at INTEGER                   -- NIP-40
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ne_kind_ts ON nostr_events(kind, created_at DESC);
CREATE INDEX IF NOT EXISTS ne_author  ON nostr_events(pubkey, kind, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS ne_replaceable
  ON nostr_events(pubkey, kind, d_tag) WHERE d_tag IS NOT NULL;

-- Recherche plein texte NIP-50 : index FTS5 « sans contenu » (le texte reste dans `nostr_events`), tenu à jour par
-- triggers pour que toute insertion, remplacement, suppression NIP-09 ou purge d'expiration le suive. Les messages
-- chiffrés (kinds 4 et 1059) ne sont pas indexés. `nostr_fts_ids` fait le lien entre l'identifiant d'événement et
-- le rowid de l'index. Reconstructible : `astro_db::events::rebuild_search_index`.
CREATE TABLE IF NOT EXISTS nostr_fts_ids (
  rowid INTEGER PRIMARY KEY,
  id    TEXT NOT NULL UNIQUE
) STRICT;
CREATE VIRTUAL TABLE IF NOT EXISTS nostr_fts USING fts5(content, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS nostr_fts_ai AFTER INSERT ON nostr_events WHEN new.kind NOT IN (4, 1059) BEGIN
  INSERT INTO nostr_fts_ids(id) VALUES (new.id);
  INSERT INTO nostr_fts(rowid, content) VALUES (last_insert_rowid(), new.content);
END;
CREATE TRIGGER IF NOT EXISTS nostr_fts_ad AFTER DELETE ON nostr_events BEGIN
  DELETE FROM nostr_fts WHERE rowid IN (SELECT rowid FROM nostr_fts_ids WHERE id = old.id);
  DELETE FROM nostr_fts_ids WHERE id = old.id;
END;

CREATE TABLE IF NOT EXISTS nostr_tags (
  event_id TEXT NOT NULL REFERENCES nostr_events(id) ON DELETE CASCADE,
  name     TEXT NOT NULL,              -- d, p, e, t, g
  value    TEXT NOT NULL,
  PRIMARY KEY (event_id, name, value)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS nt_lookup ON nostr_tags(name, value);

-- Identités : DID kind 30800 (+ kind 0 pour home_station). `key_formula` est détectée par
-- astro-lifecycle en comparant G1PUBNOSTR ; elle n'entraîne JAMAIS de destruction de compte.
CREATE TABLE IF NOT EXISTS identities (
  hex          TEXT PRIMARY KEY,       -- clé Nostr x-only (64 hex)
  role         TEXT NOT NULL CHECK (role IN ('node','captain','multipass','zencard','love')),
  email        TEXT,
  npub         TEXT,
  g1pub_nostr  TEXT,
  g1pub        TEXT,
  ipns         TEXT,
  lang         TEXT,
  lat          REAL,
  lon          REAL,
  umap         TEXT,
  key_formula  TEXT NOT NULL DEFAULT 'isolated'
               CHECK (key_formula IN ('isolated','legacy','raw','concat_swarm','coop_prefix')),
  home_station TEXT,                   -- '<IPFSNODEID>:<NODE_HEX>'
  peer_id      TEXT,                   -- PeerID libp2p d'un NODE (flux P2P → NODE)
  contract     TEXT CHECK (contract IN ('active_rental','cooperative_member') OR contract IS NULL),
  status       TEXT,                   -- contractStatus brut du DID
  hosted       INTEGER NOT NULL DEFAULT 0 CHECK (hosted IN (0,1)),  -- 1 = hébergé ici (état LOCAL, jamais écrasé par un DID)
  quota_bytes  INTEGER,
  created_at   INTEGER,
  todate       TEXT,
  src_event    TEXT                    -- DID 30800
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS identities_email ON identities(email) WHERE email IS NOT NULL;
-- Un email = un MULTIPASS hébergé ici (l'email tient lieu de npub). Seuls les MULTIPASS : la ZenCard et le Capitaine d'un
-- même membre portent légitimement la même adresse que son MULTIPASS.
CREATE UNIQUE INDEX IF NOT EXISTS identities_email_multipass ON identities(lower(email)) WHERE email IS NOT NULL AND hosted = 1 AND role = 'multipass';
CREATE INDEX IF NOT EXISTS identities_hosted ON identities(hex) WHERE hosted = 1;

-- Graphe social N1/N2, projection des kind 3.
CREATE TABLE IF NOT EXISTS social_graph (
  owner_hex      TEXT NOT NULL,
  pubkey         TEXT NOT NULL,
  level          INTEGER NOT NULL CHECK (level IN (0,1,2)),
  via            TEXT,
  certified_a4l  INTEGER NOT NULL DEFAULT 0,
  updated_at     INTEGER NOT NULL,
  src_event      TEXT,
  PRIMARY KEY (owner_hex, pubkey)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS sg_pubkey ON social_graph(pubkey);

-- Marqueurs d'idempotence (paiements, cérémonies) : rejouables depuis les événements économiques.
CREATE TABLE IF NOT EXISTS done (
  key       TEXT PRIMARY KEY,
  value     TEXT,
  at        INTEGER NOT NULL,
  src_event TEXT
) STRICT, WITHOUT ROWID;

-- uCloud : l'index chiffré (manifest) est un blob IPFS référencé par un événement 30078 ;
-- ces lignes en sont la projection.
CREATE TABLE IF NOT EXISTS ucloud_entries (
  owner_hex     TEXT NOT NULL,
  path          TEXT NOT NULL,
  parent        TEXT NOT NULL,
  type          TEXT NOT NULL,
  cid           TEXT,
  mime          TEXT,
  size          INTEGER,
  mtime         INTEGER,
  taken_at      INTEGER,
  sha_orig      TEXT,
  sha_plain     TEXT,
  faceid_status TEXT CHECK (faceid_status IN ('faces','no_face','pending','timeout','unreadable') OR faceid_status IS NULL),
  origin        TEXT,
  phash         INTEGER,               -- dHash 64 bits, entier signé
  lat           REAL,
  lon           REAL,
  desc_text     TEXT,
  data          TEXT NOT NULL,         -- entrée JSON complète du manifest
  src_cid       TEXT,                  -- CID du manifest d'où provient la ligne
  PRIMARY KEY (owner_hex, path)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ue_parent     ON ucloud_entries(owner_hex, parent);
CREATE INDEX IF NOT EXISTS ue_cid        ON ucloud_entries(cid)       WHERE cid IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_sha_orig   ON ucloud_entries(sha_orig)  WHERE sha_orig IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_sha_plain  ON ucloud_entries(sha_plain) WHERE sha_plain IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_taken      ON ucloud_entries(owner_hex, COALESCE(taken_at, mtime) DESC)
  WHERE type = 'file' AND cid IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_phash      ON ucloud_entries(phash)     WHERE phash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_geo        ON ucloud_entries(lat, lon)  WHERE lat IS NOT NULL;

-- Trousseau des clés AES de fichier. Projection d'un blob chiffré NIP-44 vers soi-même
-- (publié sur IPFS, CID dans un 30078) : jamais la seule copie de ces clés.
CREATE TABLE IF NOT EXISTS ucloud_keyring (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  key_hex   TEXT NOT NULL CHECK (length(key_hex) = 64),
  enc       TEXT NOT NULL DEFAULT 'uenc-aes256gcm',
  src_cid   TEXT,
  PRIMARY KEY (owner_hex, cid)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS ucloud_embeddings (
  emb_id INTEGER PRIMARY KEY,
  cid    TEXT NOT NULL UNIQUE,
  model  TEXT NOT NULL,
  dsrc   TEXT
) STRICT;

-- Visages : remplace les collections Qdrant `faces_<hex64>`.
CREATE TABLE IF NOT EXISTS faces (
  face_id      INTEGER PRIMARY KEY,
  owner_hex    TEXT NOT NULL,
  cid          TEXT,
  source_path  TEXT,
  name         TEXT,                   -- 'Inconnu_<hash8>' tant que non nommé
  pubkey       TEXT,
  ref          TEXT CHECK (ref IN ('contact','shared') OR ref IS NULL),
  email        TEXT,
  det_score    REAL,
  bbox_x1      REAL, bbox_y1 REAL, bbox_x2 REAL, bbox_y2 REAL,  -- repère de l'image originale
  embed_hash   TEXT NOT NULL,          -- id déterministe (sha256 de l'embedding arrondi à 6 décimales)
  ts           INTEGER NOT NULL,
  src_cid      TEXT,
  ucloud_path  TEXT,                   -- photo d'origine dans le VFS
  suggest_pubkey TEXT,                 -- zone grise [0,55 ; 0,82[ : « est-ce X ? », à valider par l'humain
  suggest_name   TEXT,
  suggest_score  REAL,
  UNIQUE (owner_hex, embed_hash)
) STRICT;
CREATE INDEX IF NOT EXISTS faces_owner_name ON faces(owner_hex, name);

-- Annuaire des contacts : index des fiches vCard de `/Contacts` (le chiffré reste la source de vérité ; table rebuildable
-- par un balayage). `pubkey` = clé Nostr rattachée à la fiche : donnée par la fiche (X-NOSTR-*) ou déduite de l'email d'un
-- MULTIPASS HÉBERGÉ ici (un email auto-déclaré dans un DID n'est jamais une preuve). `cid` = état indexé (étiquette de la fiche).
CREATE TABLE IF NOT EXISTS identity_links (
  owner_hex  TEXT NOT NULL,
  path       TEXT NOT NULL,            -- /Contacts/<fichier>.vcf
  uid        TEXT,
  name       TEXT NOT NULL,            -- FN
  name_key   TEXT NOT NULL,            -- FN comparable (minuscules, sans accents) : rapproche les visages nommés
  emails     TEXT NOT NULL DEFAULT '[]',
  pubkey     TEXT,
  via        TEXT CHECK (via IN ('vcard','email') OR via IS NULL),
  g1pub      TEXT,
  warning    TEXT,                     -- X-NOSTR-* illisible, fiche invalide…
  cid        TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, path)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS identity_links_name ON identity_links(owner_hex, name_key);

-- Partages FaceCloud déjà effectués (un par photo et destinataire) : état LOCAL, évite les doublons au retraitement.
CREATE TABLE IF NOT EXISTS face_shares (
  owner_hex     TEXT NOT NULL,
  cid           TEXT NOT NULL,
  recipient_hex TEXT NOT NULL,
  via           TEXT NOT NULL CHECK (via IN ('local','nip17')),
  at            INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid, recipient_hex)
) STRICT, WITHOUT ROWID;

-- Invitations à rejoindre la constellation en recevant des photos (lien `/i/<jeton>`). Le jeton (128 bits aléatoires) est la
-- seule preuve de possession du lien. `items` : fichiers choisis à la création (chemin, CID, nom, type, taille, empreinte) ;
-- leurs clés restent dans `ucloud_keyring` et ne quittent jamais la station avant la réclamation.
CREATE TABLE IF NOT EXISTS invites (
  token        TEXT PRIMARY KEY,
  owner_hex    TEXT NOT NULL,
  label        TEXT NOT NULL DEFAULT '',   -- nom du destinataire, tel que l'expéditeur le connaît
  email        TEXT,                       -- adresse de la fiche de contact, si connue
  contact_path TEXT,                       -- /Contacts/<fiche>.vcf à relier au MULTIPASS créé
  items        TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL,
  opened_at    INTEGER,                    -- première ouverture de la page publique
  claimed_at   INTEGER,                    -- réclamation en cours ou faite (usage unique)
  claimed_by   TEXT,                       -- clé du MULTIPASS créé ; NULL tant que la création n'est pas finie
  revoked_at   INTEGER
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS invites_owner ON invites(owner_hex, created_at);

-- Mémoire BRO : remplace les collections Qdrant `memory_<hex16>`.
CREATE TABLE IF NOT EXISTS memory (
  mem_id       INTEGER PRIMARY KEY,
  point_id     TEXT NOT NULL UNIQUE,   -- _stable_id(user_id, content_hash)
  owner_hex    TEXT NOT NULL,
  content      TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  ts           INTEGER NOT NULL,
  event_id     TEXT,
  season       TEXT,
  importance   REAL,
  kind         TEXT CHECK (kind IN ('fact','episode','trait','person')),
  source       TEXT,
  visibility   TEXT CHECK (visibility IN ('private','love','public')),
  confidence   REAL,
  status       TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','superseded','deleted')),
  embed_model  TEXT,
  schema_v     INTEGER,
  origin       TEXT,
  slot         INTEGER,                -- seuls 0, 13, 14, 15 subsistent (SLOT_SCHEMA legacy)
  src_cid      TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS memory_owner ON memory(owner_hex, status, ts DESC);

-- Économie : projections des kinds 30852 (ledger Ğ1-N²), 30800 (DID Node, capex) et 30850.
CREATE TABLE IF NOT EXISTS ledger_n2 (
  event_id   TEXT PRIMARY KEY,
  author     TEXT NOT NULL,
  dest       TEXT NOT NULL,
  amount     TEXT NOT NULL,            -- décimal exact en texte, jamais flottant
  prev       TEXT NOT NULL,            -- 'genesis' ou hex64
  d          TEXT NOT NULL,
  is_mint    INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (author, d)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ln2_author ON ledger_n2(author, created_at);
CREATE INDEX IF NOT EXISTS ln2_dest   ON ledger_n2(dest);

CREATE TABLE IF NOT EXISTS capital_assets (
  id                    TEXT PRIMARY KEY,
  label                 TEXT,
  asset_type            TEXT,
  email                 TEXT,
  value_zen             REAL,
  amortized_zen         REAL,
  depreciable           INTEGER,
  depreciation_weeks    INTEGER,
  date                  TEXT,
  created_at            INTEGER,
  last_depreciated_week TEXT,
  status                TEXT,
  src_event             TEXT,          -- DID Node (kind 30800, d-tag = IPFSNODEID)
  node_hex              TEXT           -- auteur du DID : un instantané par NODE
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS love_ledger (
  id         INTEGER PRIMARY KEY,
  at         INTEGER NOT NULL,
  kind       TEXT,
  amount_cents INTEGER NOT NULL CHECK (amount_cents >= 0),  -- centimes de ẐEN : jamais de flottant
  detail     TEXT,
  src_event  TEXT
) STRICT;

-- Loyers constatés (projection des kinds 30852 et 30851 `type=rent`) : le dernier de chaque compte reconstruit son
-- statut de cycle de vie lors d'un replay (voir `astro_lifecycle::rebuild`).
CREATE TABLE IF NOT EXISTS rent_payments (
  event_id     TEXT PRIMARY KEY,
  owner_hex    TEXT NOT NULL,           -- MULTIPASS qui paie
  at           INTEGER NOT NULL,
  amount_cents INTEGER NOT NULL DEFAULT 0,
  src_kind     INTEGER NOT NULL CHECK (src_kind IN (30851, 30852))
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS rent_owner ON rent_payments(owner_hex, at DESC);

-- Attestation de copie privée (CPI L. 122-5) : kind 31903, d = private_copy, signée du titulaire. Sans elle, aucun
-- film ni série n'est déposé ni publié.
CREATE TABLE IF NOT EXISTS private_copy_consent (
  owner_hex   TEXT PRIMARY KEY,
  version     INTEGER NOT NULL,
  accepted_at INTEGER NOT NULL,
  src_event   TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Permis Oracle (kinds 30500-30504).
CREATE TABLE IF NOT EXISTS permit_defs (
  id                  TEXT PRIMARY KEY,
  name                TEXT,
  description         TEXT,
  issuer_did          TEXT,
  min_attestations    INTEGER,
  required_license    TEXT,
  revocable           INTEGER,
  valid_duration_days INTEGER,
  verification_method TEXT,
  metadata            TEXT,
  src_event           TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS permit_requests (
  request_id           TEXT PRIMARY KEY,
  applicant_did        TEXT,
  applicant_npub       TEXT,
  permit_definition_id TEXT,
  attestations         TEXT,
  evidence             TEXT,
  statement            TEXT,
  status               TEXT,
  nostr_event_id       TEXT,
  created_at           INTEGER,
  updated_at           INTEGER
) STRICT, WITHOUT ROWID;

-- Recherche vectorielle (sqlite-vec). Cosinus partout ; `owner_hex` est une clé de partition.
-- Visages : InsightFace buffalo_l, 512 dimensions.
CREATE VIRTUAL TABLE IF NOT EXISTS face_vectors USING vec0(
  face_id   INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[512] DISTANCE_METRIC=cosine
);
-- Mémoire BRO : bge-m3, 1024 dimensions (EMBED_DIM).
CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
  mem_id    INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[1024] DISTANCE_METRIC=cosine
);
-- Descriptions de photos uCloud : bge-m3, 1024 dimensions.
CREATE VIRTUAL TABLE IF NOT EXISTS ucloud_desc_vectors USING vec0(
  emb_id    INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[1024] DISTANCE_METRIC=cosine
);
-- Tags DREAM (love_dream_vectors) : vecteur de 768 dimensions obtenu par hachage de tags, pas un embedding sémantique.
CREATE VIRTUAL TABLE IF NOT EXISTS dream_vectors USING vec0(
  owner_hex TEXT PARTITION KEY,
  dream_id  INTEGER PRIMARY KEY,
  embedding FLOAT[768] DISTANCE_METRIC=cosine
);

-- ═══════════════════════════ 2. LOCAL (non rejouable) ═══════════════════════════

-- Anti-rejeu NIP-98 : persistant entre redémarrages, utile seulement pendant la fenêtre de 2×120 s.
CREATE TABLE IF NOT EXISTS nip98_seen (
  event_id   TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS nip98_expiry ON nip98_seen(expires_at);

-- File de tâches (DM entrants, gift wraps, FaceID) avec backpressure et ré-essais.
CREATE TABLE IF NOT EXISTS tasks (
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','done','failed')),
  not_before INTEGER,
  attempts   INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  ttl        INTEGER
) STRICT;
CREATE INDEX IF NOT EXISTS tasks_ready ON tasks(status, not_before);

-- Déduplication des événements déjà traités (remplace les marqueurs O_EXCL), rétention 7 jours.
CREATE TABLE IF NOT EXISTS claimed (
  owner    TEXT NOT NULL,
  event_id TEXT NOT NULL,
  at       INTEGER NOT NULL,
  PRIMARY KEY (owner, event_id)
) STRICT, WITHOUT ROWID;

-- Suppressions NIP-09 : empêchent de réinsérer un événement supprimé par son auteur. `ref` est un identifiant d'événement
-- (`until` = i64::MAX) ou l'adresse `kind:pubkey:d` d'un événement remplaçable (bloque les versions jusqu'à `until`).
CREATE TABLE IF NOT EXISTS nostr_deleted (
  pubkey TEXT NOT NULL,
  ref    TEXT NOT NULL,
  until  INTEGER NOT NULL,
  PRIMARY KEY (pubkey, ref)
) STRICT, WITHOUT ROWID;

-- Demandes d'assistance entre Capitaines amis (état LOCAL). `direction` : `out` (je demande de l'aide, une ligne par ami
-- sollicité) ou `in` (un ami me la demande). `status` : pending, accepted, declined, closed (déjà aidé ou annulée), expired.
CREATE TABLE IF NOT EXISTS assist_requests (
  id         TEXT NOT NULL,
  direction  TEXT NOT NULL CHECK (direction IN ('out','in')),
  captain    TEXT NOT NULL,          -- mon Capitaine (clé Nostr)
  other      TEXT NOT NULL,          -- l'ami (clé Nostr)
  service    TEXT NOT NULL,
  hours      INTEGER NOT NULL,
  note       TEXT NOT NULL DEFAULT '',
  exclusive  INTEGER NOT NULL DEFAULT 0,
  station    TEXT,                   -- PeerID de la station de celui qui demande (sens `in`)
  status     TEXT NOT NULL CHECK (status IN ('pending','accepted','declined','closed','expired')),
  created_at INTEGER NOT NULL,
  decided_at INTEGER,
  PRIMARY KEY (id, direction, other)
) STRICT, WITHOUT ROWID;

-- Niveau de partage des fichiers du uCloud (état LOCAL) : un fichier absent de cette table, des invitations, des partages
-- FaceCloud et du dossier /Partages n'a jamais quitté son propriétaire : il est « privé » et sauvegardable chez un ami.
CREATE TABLE IF NOT EXISTS ucloud_sharing (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  kind      TEXT NOT NULL CHECK (kind IN ('friend','link','invite')),
  at        INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid, kind)
) STRICT, WITHOUT ROWID;

-- Sauvegarde privée chez un ami (état LOCAL, une ligne par couple propriétaire/hébergeur, tenue par chaque station concernée).
-- `status` : décision de l'hébergeur, qui fixe aussi le volume (`quota_bytes`) ; `enabled` : choix du propriétaire.
CREATE TABLE IF NOT EXISTS backup_pacts (
  owner_hex   TEXT NOT NULL,
  host_hex    TEXT NOT NULL,
  status      TEXT NOT NULL CHECK (status IN ('pending','accepted','declined','revoked')),
  quota_bytes INTEGER NOT NULL DEFAULT 0,
  enabled     INTEGER NOT NULL DEFAULT 0,
  asked_at    INTEGER NOT NULL,
  decided_at  INTEGER,
  PRIMARY KEY (owner_hex, host_hex)
) STRICT, WITHOUT ROWID;

-- Blobs chiffrés déjà envoyés (côté propriétaire) ou rangés (côté hébergeur) ; `size` en octets de clair. Un blob dont le
-- fichier a disparu chez le propriétaire est retiré de chez l'ami après 7 jours (nettoyage de 20h12).
CREATE TABLE IF NOT EXISTS backup_items (
  owner_hex TEXT NOT NULL,
  host_hex  TEXT NOT NULL,
  cid       TEXT NOT NULL,
  size      INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  missing_since INTEGER,                    -- côté propriétaire : depuis quand le fichier n'existe plus chez lui
  PRIMARY KEY (owner_hex, host_hex, cid)
) STRICT, WITHOUT ROWID;

-- Journaux bornés (activité, audit ARBOR, requêtes d'outils).
CREATE TABLE IF NOT EXISTS journal (
  id     INTEGER PRIMARY KEY,
  stream TEXT NOT NULL,
  at     INTEGER NOT NULL,
  data   TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS journal_stream ON journal(stream, at);

-- Caches et états mutables divers (jamais purgés par la maintenance nocturne).
CREATE TABLE IF NOT EXISTS kv (
  scope      TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (scope, key)
) STRICT, WITHOUT ROWID;
