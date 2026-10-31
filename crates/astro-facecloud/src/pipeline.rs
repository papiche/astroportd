//! Traitement d'une photo : détection, appariement vectoriel, catalogage, partage.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use astro_crypto::uenc;
use astro_crypto::{KeySource, UencKey};
use astro_crypto::nip59::{Rumor, gift_wrap};
use astro_dav::vfs::{self, VfsError};
use astro_dav::{BlobStore, ManifestPublisher};
use astro_db::{Db, DbError, VectorTable};
use astro_nostr_proto::{Event, kind};
use rusqlite::{OptionalExtension, params};
use tokio::sync::Semaphore;

use crate::engine::{DetectedFace, EngineError, FaceEngine, embedding_hash, l2_normalize, unknown_name};

/// Erreurs du pipeline.
#[derive(Debug, thiserror::Error)]
pub enum FaceError {
    /// Base de données.
    #[error("base : {0}")]
    Db(#[from] DbError),
    /// SQLite.
    #[error("sqlite : {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// VFS.
    #[error("vfs : {0}")]
    Vfs(#[from] VfsError),
    /// Blob.
    #[error("blob : {0}")]
    Blob(#[from] astro_dav::BlobError),
    /// Déchiffrement UENC.
    #[error("UENC : {0}")]
    Uenc(#[from] uenc::UencError),
    /// Inférence.
    #[error("inférence : {0}")]
    Engine(#[from] EngineError),
    /// Entrée absente ou inadaptée.
    #[error("{0}")]
    Invalid(&'static str),
}

/// Erreur d'envoi d'une enveloppe.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OutboxError(pub String);

/// Envoi d'une enveloppe NIP-59 (kind 1059) vers le relai du destinataire.
pub trait Outbox: Send + Sync + 'static {
    /// Publie `wrap` pour `recipient_hex`.
    fn send(&self, recipient_hex: &str, wrap: Event) -> impl Future<Output = Result<(), OutboxError>> + Send;
}

/// Réglages.
#[derive(Debug, Clone)]
pub struct FaceConfig {
    /// Score cosinus à partir duquel un visage est reconnu.
    pub match_threshold: f64,
    /// Score minimal d'une suggestion « est-ce X ? ».
    pub maybe_min: f64,
    /// Confiance minimale de détection.
    pub det_min: f32,
    /// Voisins examinés.
    pub knn_k: usize,
    /// Taille maximale d'une image analysée.
    pub max_image_bytes: u64,
    /// Partager automatiquement avec les amis réciproques.
    pub share_enabled: bool,
    /// Écart aléatoire (min, max) en ms entre deux envois NIP-59 vers des stations distantes : lisse les rafales et
    /// brouille le profil temporel vu des relais.
    pub send_jitter_ms: (u64, u64),
    /// Détection minimale (visage net, de face) pour un partage sans confirmation.
    pub share_det_min: f32,
    /// Score cosinus minimal pour un partage sans confirmation ; entre `match_threshold` et ce seuil, le partage attend un clic.
    pub share_auto_min: f64,
}

impl Default for FaceConfig {
    fn default() -> Self {
        Self { match_threshold: 0.82, maybe_min: 0.55, det_min: 0.60, knn_k: 5, max_image_bytes: 50 << 20, share_enabled: true, send_jitter_ms: (200, 800), share_det_min: 0.80, share_auto_min: 0.88 }
    }
}

/// Décision d'appariement d'un visage.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Score ≥ seuil : visage connu.
    Recognized {
        /// Score cosinus.
        score: f64,
    },
    /// Zone grise : à valider par l'humain.
    Suggestion {
        /// Score cosinus.
        score: f64,
        /// Personne suggérée.
        candidate: Option<String>,
    },
    /// Aucun visage proche.
    Unknown,
    /// Déjà catalogué (retraitement de la même photo).
    AlreadyKnown,
}

/// Résultat pour un visage.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceReport {
    /// Ligne `faces`.
    pub face_id: i64,
    /// Nom attribué.
    pub name: String,
    /// Clé de la personne si elle est identifiée.
    pub pubkey: Option<String>,
    /// Décision.
    pub decision: Decision,
    /// Confiance de détection du visage dans la photo traitée.
    pub det_score: f32,
}

/// Mode de livraison d'un partage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareVia {
    /// Copie dans le VFS d'un ami hébergé ici.
    Local,
    /// Message fichier NIP-17 emballé en NIP-59.
    Nip17,
}

/// Partage effectué.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareReport {
    /// Destinataire.
    pub recipient: String,
    /// Mode.
    pub via: ShareVia,
}

/// Partage non effectué.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Personne reconnue.
    pub recipient: String,
    /// Raison.
    pub reason: String,
}

/// Bilan d'une photo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhotoReport {
    /// `no_face`, `faces`, ou `skipped` (copie reçue, jamais analysée).
    pub status: String,
    /// Visages catalogués.
    pub faces: Vec<FaceReport>,
    /// Partages réalisés.
    pub shares: Vec<ShareReport>,
    /// Partages non réalisés.
    pub skipped: Vec<Skipped>,
}

pub(crate) struct Photo {
    pub cid: String,
    pub mime: String,
    pub size: u64,
    pub sha_plain: String,
    pub origin: String,
    pub key_hex: String,
}

struct Candidate {
    score: f64,
    name: Option<String>,
    pubkey: Option<String>,
}

/// Service FaceCloud.
pub struct FaceCloud<B: BlobStore, E: FaceEngine, O: Outbox> {
    pub(crate) db: Arc<Db>,
    pub(crate) blobs: Arc<B>,
    pub(crate) engine: Arc<E>,
    pub(crate) outbox: Arc<O>,
    pub(crate) keys: Arc<dyn KeySource>,
    pub(crate) publisher: Option<ManifestPublisher>,
    pub(crate) config: FaceConfig,
    /// Une seule inférence à la fois : le matériel hôte ne doit jamais être saturé.
    gpu: Semaphore,
    /// Vignettes de visages déjà calculées (mémoire seulement : jamais de clair sur disque).
    pub(crate) thumbs: crate::thumbs::ThumbCache,
    /// Décodages de photos simultanés au plus (la mémoire d'une petite station est comptée).
    pub(crate) decode: Semaphore,
}

pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl<B: BlobStore, E: FaceEngine, O: Outbox> FaceCloud<B, E, O> {
    /// Assemble le service.
    pub fn new(db: Arc<Db>, blobs: Arc<B>, engine: Arc<E>, outbox: Arc<O>, keys: Arc<dyn KeySource>, publisher: Option<ManifestPublisher>, config: FaceConfig) -> Self {
        Self { db, blobs, engine, outbox, keys, publisher, config, gpu: Semaphore::new(1), thumbs: crate::thumbs::ThumbCache::new(), decode: Semaphore::new(2) }
    }

    pub(crate) async fn photo(&self, owner: &str, path: &str) -> Result<Photo, FaceError> {
        let (o, p) = (owner.to_owned(), path.to_owned());
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT e.cid, coalesce(e.mime,''), coalesce(e.size,0), coalesce(e.sha_plain,''), coalesce(e.origin,''), k.key_hex \
                     FROM ucloud_entries e JOIN ucloud_keyring k ON k.owner_hex = e.owner_hex AND k.cid = e.cid \
                     WHERE e.owner_hex = ?1 AND e.path = ?2 AND e.type = 'file'",
                    params![o, p],
                    |r| Ok(Photo { cid: r.get(0)?, mime: r.get(1)?, size: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0), sha_plain: r.get(3)?, origin: r.get(4)?, key_hex: r.get(5)? }),
                )
                .optional()?)
            })
            .await?
            .ok_or(FaceError::Invalid("photo introuvable (entrée ou clé absente)"))
    }

    pub(crate) async fn read_plain(&self, photo: &Photo) -> Result<Vec<u8>, FaceError> {
        let key = UencKey::from_hex(&photo.key_hex)?;
        let (mut reader, _) = self.blobs.open(&photo.cid).await?;
        let mut plain = Vec::with_capacity(usize::try_from(photo.size).unwrap_or(0));
        uenc::decrypt_stream(&key, &mut reader, &mut plain).await?;
        Ok(plain)
    }

    async fn detect(&self, image: &[u8]) -> Result<Vec<DetectedFace>, FaceError> {
        let _permit = self.gpu.acquire().await.map_err(|_| FaceError::Invalid("file d'inférence fermée"))?;
        let mut faces = self.engine.detect(image).await?;
        faces.retain(|f| f.det_score >= self.config.det_min);
        for f in &mut faces {
            l2_normalize(&mut f.embedding)?;
        }
        Ok(faces)
    }

    async fn set_status(&self, owner: &str, path: &str, status: &str, count: usize) -> Result<(), FaceError> {
        let info = serde_json::json!({ "status": status, "count": count, "checked_at": now() }).to_string();
        self.db
            .writer()
            .execute(
                "UPDATE ucloud_entries SET faceid_status = ?3, data = json_set(data, '$.faceid', json(?4)) WHERE owner_hex = ?1 AND path = ?2",
                vec![owner.to_owned().into(), path.to_owned().into(), status.to_owned().into(), info.into()],
            )
            .await?;
        Ok(())
    }

    /// Les plus proches visages du compte, du plus proche au plus lointain.
    async fn nearest(&self, owner: &str, emb: &[f32]) -> Result<Vec<Candidate>, FaceError> {
        let blob = VectorTable::Face.encode(emb)?;
        let (o, k) = (owner.to_owned(), i64::try_from(self.config.knn_k).unwrap_or(5));
        Ok(self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT v.distance, f.name, f.pubkey FROM \
                       (SELECT face_id, distance FROM face_vectors WHERE embedding MATCH ?1 AND owner_hex = ?2 AND k = ?3) v \
                     JOIN faces f ON f.face_id = v.face_id ORDER BY v.distance",
                )?;
                let rows = s.query_map(params![blob, o, k], |r| Ok(Candidate { score: 1.0 - r.get::<_, f64>(0)?, name: r.get(1)?, pubkey: r.get(2)? }))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    /// Catalogue un visage. `forced` : identité imposée (enrôlement supervisé), sinon appariement.
    async fn catalog(&self, owner: &str, path: &str, photo: &Photo, face: &DetectedFace, forced: Option<(&str, &str)>) -> Result<FaceReport, FaceError> {
        let hash = embedding_hash(&face.embedding);
        let (o, h) = (owner.to_owned(), hash.clone());
        let existing = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT face_id, name, pubkey FROM faces WHERE owner_hex = ?1 AND embed_hash = ?2", params![o, h], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))).optional()?))
            .await?;
        if let (Some((face_id, name, pubkey)), None) = (&existing, forced) {
            return Ok(FaceReport { face_id: *face_id, name: name.clone(), pubkey: pubkey.clone(), decision: Decision::AlreadyKnown, det_score: face.det_score });
        }

        let (name, pubkey, decision, suggest, reference) = if let Some((pk, nm)) = forced {
            (nm.to_owned(), Some(pk.to_owned()), Decision::Recognized { score: 1.0 }, None, Some("contact"))
        } else {
            let best = self.nearest(owner, &face.embedding).await?.into_iter().next();
            match best {
                Some(b) if b.score >= self.config.match_threshold => {
                    (b.name.clone().unwrap_or_else(|| unknown_name(&hash)), b.pubkey.clone(), Decision::Recognized { score: b.score }, None, None)
                }
                Some(b) if b.score >= self.config.maybe_min => {
                    let decision = Decision::Suggestion { score: b.score, candidate: b.name.clone() };
                    (unknown_name(&hash), None, decision, Some((b.pubkey.clone(), b.name.clone(), b.score)), None)
                }
                _ => (unknown_name(&hash), None, Decision::Unknown, None, None),
            }
        };

        let blob = VectorTable::Face.encode(&face.embedding)?;
        let (o, p, cid, n, pk, h, bb, ds, rf) = (owner.to_owned(), path.to_owned(), photo.cid.clone(), name.clone(), pubkey.clone(), hash, face.bbox, f64::from(face.det_score), reference);
        let sg = suggest;
        let face_id = self
            .db
            .writer()
            .transaction(move |tx| {
                // Enrôlement d'une empreinte déjà vue : on met l'identité à jour plutôt que de dupliquer.
                if let Some(id) = tx.query_row("SELECT face_id FROM faces WHERE owner_hex = ?1 AND embed_hash = ?2", params![o, h], |r| r.get::<_, i64>(0)).optional()? {
                    tx.execute("UPDATE faces SET name = ?2, pubkey = ?3, ref = ?4, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL WHERE face_id = ?1", params![id, n, pk, rf])?;
                    return Ok(id);
                }
                tx.execute(
                    "INSERT INTO faces(owner_hex, cid, source_path, name, pubkey, ref, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, ucloud_path, suggest_pubkey, suggest_name, suggest_score) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?3,?14,?15,?16)",
                    params![o, cid, p, n, pk, rf, ds, f64::from(bb.x1), f64::from(bb.y1), f64::from(bb.x2), f64::from(bb.y2), h, now(), sg.as_ref().and_then(|s| s.0.clone()), sg.as_ref().and_then(|s| s.1.clone()), sg.as_ref().map(|s| s.2)],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute("INSERT INTO face_vectors(face_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![id, o, blob])?;
                Ok(id)
            })
            .await?;
        Ok(FaceReport { face_id, name, pubkey, decision, det_score: face.det_score })
    }

    /// Analyse une photo du VFS : catalogue ses visages et partage avec les amis reconnus.
    pub async fn process_photo(&self, owner: &str, path: &str) -> Result<PhotoReport, FaceError> {
        let photo = self.photo(owner, path).await?;
        if photo.origin == "facecloud-share" {
            return Ok(PhotoReport { status: "skipped".into(), ..PhotoReport::default() }); // copie reçue : jamais ré-analysée
        }
        if !photo.mime.starts_with("image/") {
            return Err(FaceError::Invalid("ce n'est pas une image"));
        }
        if photo.size > self.config.max_image_bytes {
            self.set_status(owner, path, "unreadable", 0).await?;
            return Ok(PhotoReport { status: "unreadable".into(), ..PhotoReport::default() });
        }
        let plain = self.read_plain(&photo).await?;
        let detected = self.detect(&plain).await?;
        if detected.is_empty() {
            self.set_status(owner, path, "no_face", 0).await?;
            return Ok(PhotoReport { status: "no_face".into(), ..PhotoReport::default() });
        }
        let mut report = PhotoReport { status: "faces".into(), ..PhotoReport::default() };
        for face in &detected {
            report.faces.push(self.catalog(owner, path, &photo, face, None).await?);
        }
        self.set_status(owner, path, "faces", report.faces.len()).await?;
        if self.config.share_enabled {
            self.dispatch_shares(owner, path, &photo, &mut report).await?;
        }
        Ok(report)
    }

    /// Envoie tout de suite aux amis reconnus avec certitude (visage net, score ≥ `share_auto_min`) ; les cas ambigus
    /// attendent la confirmation de l'utilisateur dans `face_share_pending`.
    async fn dispatch_shares(&self, owner: &str, path: &str, photo: &Photo, report: &mut PhotoReport) -> Result<(), FaceError> {
        // ami → (envoi direct ?, meilleure détection, meilleur score, visage déjà catalogué ?)
        let mut friends: BTreeMap<String, (bool, f32, f64, bool)> = BTreeMap::new();
        for f in &report.faces {
            let Some(pk) = f.pubkey.clone().filter(|pk| pk != owner) else { continue };
            let (score, known) = match f.decision {
                Decision::Recognized { score } => (score, false),
                Decision::AlreadyKnown => (1.0, true),
                _ => continue,
            };
            let auto = !known && f.det_score >= self.config.share_det_min && score >= self.config.share_auto_min;
            let e = friends.entry(pk).or_insert((false, f.det_score, score, known));
            e.0 |= auto;
            e.1 = e.1.max(f.det_score);
            e.2 = e.2.max(score);
            e.3 &= known;
        }
        for (friend, (auto, det, score, known)) in friends {
            // Visage déjà catalogué : décidé à un passage précédent (envoi, attente ou nom donné par l'humain). Une
            // photo déjà en attente reste en attente ; sinon c'est une identité validée à la main, on envoie.
            let direct = auto || (known && !self.is_pending(owner, &friend, &photo.cid).await?);
            if direct {
                self.share_with(owner, path, photo, &friend, report).await?;
            } else if !known {
                self.queue_share(owner, path, photo, &friend, det, score, report).await?;
            }
        }
        Ok(())
    }

    async fn is_pending(&self, owner: &str, friend: &str, cid: &str) -> Result<bool, FaceError> {
        let (o, f, c) = (owner.to_owned(), friend.to_owned(), cid.to_owned());
        Ok(self
            .db
            .readers()
            .read(move |conn| Ok(conn.query_row("SELECT 1 FROM face_share_pending WHERE owner_hex = ?1 AND recipient_hex = ?2 AND cid = ?3", params![o, f, c], |r| r.get::<_, i64>(0)).optional()?.is_some()))
            .await?)
    }

    /// Met un envoi en attente de confirmation (sauf ami non réciproque ou déjà servi : rien à confirmer).
    #[allow(clippy::too_many_arguments)]
    async fn queue_share(&self, owner: &str, path: &str, photo: &Photo, friend: &str, det: f32, score: f64, report: &mut PhotoReport) -> Result<(), FaceError> {
        let (o, c, f) = (owner.to_owned(), photo.cid.clone(), friend.to_owned());
        let done = self.db.readers().read(move |conn| Ok(conn.query_row("SELECT 1 FROM face_shares WHERE owner_hex = ?1 AND cid = ?2 AND recipient_hex = ?3", params![o, c, f], |r| r.get::<_, i64>(0)).optional()?.is_some())).await?;
        if done || !self.reciprocal(owner, friend).await? {
            return Ok(());
        }
        self.db
            .writer()
            .execute(
                "INSERT OR IGNORE INTO face_share_pending(owner_hex, recipient_hex, path, cid, det_score, score, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![owner.to_owned().into(), friend.to_owned().into(), path.to_owned().into(), photo.cid.clone().into(), f64::from(det).into(), score.into(), now().into()],
            )
            .await?;
        report.skipped.push(Skipped { recipient: friend.to_owned(), reason: "en attente de confirmation".into() });
        Ok(())
    }

    /// Réponse de l'utilisateur au lot en attente pour `friend`. `accept` : envoie les photos ; sinon les oublie.
    /// Un envoi qui échoue (relais injoignable) reste en attente pour un nouvel essai. Rend `(envoyées, retirées sans envoi)`.
    pub async fn resolve_pending(&self, owner: &str, friend: &str, accept: bool) -> Result<(usize, usize), FaceError> {
        let (o, f) = (owner.to_owned(), friend.to_owned());
        let rows: Vec<(i64, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT id, path FROM face_share_pending WHERE owner_hex = ?1 AND recipient_hex = ?2 ORDER BY id")?;
                Ok(stmt.query_map(params![o, f], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)
            })
            .await?;
        let (mut sent, mut dropped) = (0_usize, 0_usize);
        for (id, path) in rows {
            let mut keep = false;
            if accept {
                match self.photo(owner, &path).await {
                    Ok(photo) if photo.origin != "facecloud-share" => {
                        let mut report = PhotoReport::default();
                        self.share_with(owner, &path, &photo, friend, &mut report).await?;
                        sent += report.shares.len();
                        // Panne d'envoi : on garde la ligne ; amitié perdue ou déjà partagé : rien à réessayer.
                        keep = report.skipped.iter().any(|s| s.reason.starts_with("envoi impossible") || s.reason.starts_with("clé de l'expéditeur"));
                    }
                    _ => {}
                }
            }
            if keep {
                continue;
            }
            self.db.writer().execute("DELETE FROM face_share_pending WHERE id = ?1", vec![id.into()]).await?;
            dropped += 1;
        }
        Ok((sent, dropped.saturating_sub(sent)))
    }

    /// Enrôlement supervisé : chaque visage de `image` est catalogué sous l'identité `(pubkey, name)`.
    pub async fn enroll(&self, owner: &str, pubkey: &str, name: &str, image: &[u8]) -> Result<usize, FaceError> {
        if !astro_nostr_proto::is_hex64(pubkey) {
            return Err(FaceError::Invalid("clé publique invalide"));
        }
        let detected = self.detect(image).await?;
        let reference = Photo { cid: String::new(), mime: String::new(), size: 0, sha_plain: String::new(), origin: "enroll".into(), key_hex: String::new() };
        for face in &detected {
            self.catalog(owner, "", &reference, face, Some((pubkey, name))).await?;
        }
        Ok(detected.len())
    }

    /// Nomme un visage (validation humaine) ; efface la suggestion.
    pub async fn name_face(&self, owner: &str, face_id: i64, pubkey: Option<&str>, name: &str) -> Result<(), FaceError> {
        crate::review::name_face(&self.db, owner, face_id, pubkey, name).await
    }

    /// Valide la suggestion « est-ce X ? » d'un visage : il prend le nom et la clé suggérés.
    pub async fn accept_suggestion(&self, owner: &str, face_id: i64) -> Result<bool, FaceError> {
        crate::review::accept_suggestion(&self.db, owner, face_id).await
    }

    /// Partage les photos déjà analysées où figure `friend` (visages rangés avec sa clé) : appelé quand une personne vient
    /// d'être reliée à un MULTIPASS (fiche contact, rapprochement). Chaque envoi reste soumis à la réciprocité N¹ et au
    /// registre des partages (jamais deux fois). Rend `(partagées, écartées)`.
    pub async fn share_backlog(&self, owner: &str, friend: &str) -> Result<(usize, usize), FaceError> {
        if !self.config.share_enabled || owner == friend {
            return Ok((0, 0));
        }
        let (o, f) = (owner.to_owned(), friend.to_owned());
        let paths: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT DISTINCT ucloud_path FROM faces WHERE owner_hex = ?1 AND pubkey = ?2 AND ref IS NULL AND ucloud_path IS NOT NULL ORDER BY ucloud_path LIMIT 500")?;
                let rows = stmt.query_map(params![o, f], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        let mut report = PhotoReport::default();
        for path in &paths {
            let Ok(photo) = self.photo(owner, path).await else { continue };
            if photo.origin == "facecloud-share" {
                continue; // copie reçue d'un ami : jamais renvoyée
            }
            self.share_with(owner, path, &photo, friend, &mut report).await?;
        }
        Ok((report.shares.len(), report.skipped.len()))
    }

    /// Pause aléatoire après un envoi distant (bornes de `send_jitter_ms`).
    async fn pace(&self) {
        let (lo, hi) = self.config.send_jitter_ms;
        if hi == 0 {
            return;
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::from(d.subsec_nanos()));
        tokio::time::sleep(std::time::Duration::from_millis(lo + nanos % (hi.saturating_sub(lo) + 1))).await;
    }

    /// `owner` et `friend` se suivent-ils mutuellement ?
    async fn reciprocal(&self, owner: &str, friend: &str) -> Result<bool, FaceError> {
        let (o, f) = (owner.to_owned(), friend.to_owned());
        Ok(self
            .db
            .readers()
            .read(move |c| {
                let follows: Option<i64> = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", params![o, f], |r| r.get(0)).optional()?;
                let back: Option<i64> = c
                    .query_row(
                        "SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.id WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1",
                        params![f, o],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(follows.is_some() && back.is_some())
            })
            .await?)
    }

    async fn share_with(&self, owner: &str, path: &str, photo: &Photo, friend: &str, report: &mut PhotoReport) -> Result<(), FaceError> {
        let skip = |report: &mut PhotoReport, reason: &str| report.skipped.push(Skipped { recipient: friend.to_owned(), reason: reason.to_owned() });
        let (o, cid, f) = (owner.to_owned(), photo.cid.clone(), friend.to_owned());
        let (done, hosted, label) = self
            .db
            .readers()
            .read(move |c| {
                let done: Option<i64> = c.query_row("SELECT 1 FROM face_shares WHERE owner_hex = ?1 AND cid = ?2 AND recipient_hex = ?3", params![o, cid, f], |r| r.get(0)).optional()?;
                let hosted: Option<i64> = c.query_row("SELECT hosted FROM identities WHERE hex = ?1", [&f], |r| r.get(0)).optional()?;
                let email: Option<String> = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&o], |r| r.get(0)).optional()?.flatten();
                Ok((done.is_some(), hosted == Some(1), email))
            })
            .await?;
        if done {
            skip(report, "déjà partagé");
            return Ok(());
        }
        if !self.reciprocal(owner, friend).await? {
            skip(report, "pas un ami réciproque (N1)");
            return Ok(());
        }
        let via = if hosted {
            let sender = sanitize_label(label.as_deref(), owner);
            self.share_local(owner, path, photo, friend, &sender).await?;
            ShareVia::Local
        } else {
            let Some(secret) = self.keys.secret(owner) else {
                skip(report, "clé de l'expéditeur indisponible");
                return Ok(());
            };
            let name = vfs::name_of(path).to_owned();
            let rumor = Rumor {
                pubkey: owner.to_owned(),
                created_at: u64::try_from(now()).unwrap_or(0),
                kind: kind::FILE,
                tags: vec![
                    vec!["p".into(), friend.to_owned()],
                    vec!["file-type".into(), photo.mime.clone()],
                    vec!["encryption-algorithm".into(), "uenc-v2".into()],
                    vec!["decryption-key".into(), photo.key_hex.clone()],
                    vec!["cid".into(), photo.cid.clone()],
                    vec!["ox".into(), photo.sha_plain.clone()],
                    vec!["size".into(), photo.size.to_string()],
                    vec!["name".into(), name],
                ],
                content: format!("ipfs://{}", photo.cid),
            };
            let wrap = gift_wrap(&secret, friend, &rumor, rumor.created_at).map_err(|_| FaceError::Invalid("emballage NIP-59 impossible"))?;
            if let Err(e) = self.outbox.send(friend, wrap).await {
                skip(report, &format!("envoi impossible : {e}"));
                return Ok(());
            }
            self.pace().await;
            ShareVia::Nip17
        };
        let (o, cid, f) = (owner.to_owned(), photo.cid.clone(), friend.to_owned());
        let via_str = if via == ShareVia::Local { "local" } else { "nip17" };
        self.db
            .writer()
            .execute(
                "INSERT OR IGNORE INTO face_shares(owner_hex, cid, recipient_hex, via, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![o.into(), cid.into(), f.into(), via_str.to_owned().into(), now().into()],
            )
            .await?;
        report.shares.push(ShareReport { recipient: friend.to_owned(), via });
        Ok(())
    }

    /// Copie silencieuse dans `/Partages/De_<nom>/` d'un ami hébergé ici : même blob, même clé, aucun ré-encodage.
    async fn share_local(&self, owner: &str, path: &str, photo: &Photo, friend: &str, sender_label: &str) -> Result<(), FaceError> {
        let file = astro_dav::adopt::SharedFile { cid: photo.cid.clone(), key_hex: photo.key_hex.clone(), mime: photo.mime.clone(), size: photo.size, sha_plain: photo.sha_plain.clone(), name: vfs::name_of(path).to_owned() };
        astro_dav::adopt::adopt_shared_file(&self.db, friend, owner, sender_label, &file, "facecloud-share").await?;
        if let Some(p) = &self.publisher {
            p.notify(friend);
        }
        Ok(())
    }
}

/// Nom de dossier sûr : partie locale de l'email, sinon 8 premiers hexadécimaux de la clé.
fn sanitize_label(email: Option<&str>, owner: &str) -> String {
    let local: String = email.and_then(|e| e.split('@').next()).unwrap_or("").chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')).take(40).collect();
    if local.is_empty() || local == "." || local == ".." { owner[..8.min(owner.len())].to_owned() } else { local }
}

#[cfg(test)]
mod tests {
    use super::sanitize_label;

    #[test]
    fn labels_are_safe_directory_names() {
        assert_eq!(sanitize_label(Some("alice.dupont@example.org"), "ab12cd34ef"), "alice.dupont");
        assert_eq!(sanitize_label(Some("../../etc@x.org"), "ab12cd34ef"), "....etc", "aucune barre : jamais de remontée de dossier");
        assert_eq!(sanitize_label(None, "ab12cd34ef56"), "ab12cd34");
        assert_eq!(sanitize_label(Some("@x.org"), "ab12cd34ef56"), "ab12cd34");
        assert_eq!(sanitize_label(Some("..@x.org"), "ab12cd34ef56"), "ab12cd34");
    }
}
