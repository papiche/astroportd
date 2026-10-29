//! Implémentation du [`Backend`] de l'interface web : câble la base, WebDAV, FaceCloud, BRO, Duniter et le P2P.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astro_bro::bus;
use astro_bro::{MemoryStore, Meta, OllamaClient, Slot};
use astro_core::Amount;
use astro_dav::{BlobStore, DavState};
use astro_p2p::routing::{Candidate, haversine_km};
use astro_ui::api::Applicant;
use astro_ui::{ApiError, Backend, Principal, Status, Subsystem};
use axum::http::{HeaderMap, Uri};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::station::StationServices;

/// Mémoire de BRO telle que câblée par le démon.
pub type BroMemory = MemoryStore<OllamaClient, OllamaClient>;

/// Semaines d'autonomie à pleine jauge.
const GAUGE_WEEKS: f64 = 12.0;
/// Les sondes de `/health` (public) ne déclenchent jamais plus d'un appel sortant par minute.
const PROBE_TTL: Duration = Duration::from_secs(60);

/// Interface web de la station.
pub struct StationUi<B: BlobStore> {
    /// Services partagés.
    pub services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub dav: Arc<DavState<B>>,
    /// Mémoire de BRO (avec `--ollama`).
    pub memory: Option<Arc<BroMemory>>,
    /// Ollama (avec `--ollama`).
    pub ollama: Option<Arc<OllamaClient>>,
    /// FaceCloud est actif.
    pub faces_enabled: bool,
    consent_fn: Option<ConsentFn>,
    thumbs_fn: Option<ThumbFn>,
    friends_fn: Option<FriendsFn>,
    coop: Option<(CoopGetFn, CoopSetFn)>,
    admin: Option<AdminFn>,
    account: Option<Arc<crate::account::AccountService>>,
    contacts: Option<Arc<crate::identity_links::IdentityLinks<B>>>,
    invites: Option<Arc<crate::invites::Invites<B>>>,
    send_fn: Option<SendFn>,
    board: astro_facecloud::board::BoardCache,
    registrar: Option<Arc<crate::register::Registrar>>,
    probe: Mutex<Option<(Instant, Result<(), String>)>>,
}

/// Vignettes de visages d'un titulaire : `(vignettes par visage, visages en échec avec leur raison)` ; fourni par le binaire,
/// qui détient le service FaceCloud (clés, blobs, décodage).
pub type ThumbFn = Arc<dyn Fn(String, Vec<i64>, u32) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(std::collections::HashMap<i64, String>, Vec<(i64, String)>), String>> + Send>> + Send + Sync>;

/// Lecture de la configuration coopérative : vue pour l'interface.
pub type CoopGetFn = Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Écriture de la configuration coopérative : `{ clé: valeur | null }` ; rend la vue mise à jour.
pub type CoopSetFn = Arc<dyn Fn(std::collections::BTreeMap<String, Option<String>>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Réglages d'administration : `(section, None)` lit, `(section, Some(corps))` modifie ; rend la vue de la section.
/// Une erreur est un message destiné au Capitaine (« section inconnue » = 404).
pub type AdminFn = Arc<dyn Fn(String, Option<Value>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Proches réciproques d'un titulaire ; fourni par le binaire (il détient la boîte d'envoi générique).
pub type FriendsFn = Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;
/// Envoi de fichiers d'un titulaire à des proches : `(titulaire, chemins, proches)`.
pub type SendFn = Arc<dyn Fn(String, Vec<String>, Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// Enregistre l'attestation de copie privée d'un compte (signature, stockage, publication) : fourni par le binaire.
pub type ConsentFn = Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync>;

impl<B: BlobStore> StationUi<B> {
    /// Assemble l'interface.
    pub fn new(services: Arc<StationServices>, dav: Arc<DavState<B>>, memory: Option<Arc<BroMemory>>, ollama: Option<Arc<OllamaClient>>, faces_enabled: bool) -> Self {
        Self { services, dav, memory, ollama, faces_enabled, consent_fn: None, thumbs_fn: None, friends_fn: None, send_fn: None, coop: None, admin: None, account: None, contacts: None, invites: None, board: astro_facecloud::board::BoardCache::new(), registrar: None, probe: Mutex::new(None) }
    }

    /// Active l'attestation de copie privée depuis l'interface.
    pub fn with_consent(mut self, f: ConsentFn) -> Self {
        self.consent_fn = Some(f);
        self
    }

    /// Invitations par lien : photos envoyées à quelqu'un qui n'a pas encore de MULTIPASS.
    pub fn with_invites(mut self, invites: Arc<crate::invites::Invites<B>>) -> Self {
        self.invites = Some(invites);
        self
    }

    fn invites_service(&self) -> Result<&Arc<crate::invites::Invites<B>>, ApiError> {
        self.invites.as_ref().ok_or_else(|| ApiError::unavailable("invitations non disponibles sur cette station"))
    }

    /// Active l'annuaire (CardDAV, rapprochement photo ↔ fiche ↔ MULTIPASS).
    pub fn with_contacts(mut self, links: Arc<crate::identity_links::IdentityLinks<B>>) -> Self {
        self.contacts = Some(links);
        self
    }

    /// Active « Mon compte » (profil, jeton, export des clés).
    pub fn with_account(mut self, service: Arc<crate::account::AccountService>) -> Self {
        self.account = Some(service);
        self
    }

    /// Active les réglages d'administration du Capitaine (moteur FaceID, matrice des kinds…).
    pub fn with_admin(mut self, f: AdminFn) -> Self {
        self.admin = Some(f);
        self
    }

    /// Active l'écran de configuration coopérative du Capitaine.
    pub fn with_coop(mut self, get: CoopGetFn, set: CoopSetFn) -> Self {
        self.coop = Some((get, set));
        self
    }

    /// Active l'envoi de fichiers à des proches.
    pub fn with_sharing(mut self, friends: FriendsFn, send: SendFn) -> Self {
        self.friends_fn = Some(friends);
        self.send_fn = Some(send);
        self
    }

    /// Active les vignettes de visages de la table de tri.
    pub fn with_face_thumbs(mut self, f: ThumbFn) -> Self {
        self.thumbs_fn = Some(f);
        self
    }

    /// Ouvre l'inscription d'un MULTIPASS depuis l'interface (`/register`).
    pub fn with_registrar(mut self, r: Arc<crate::register::Registrar>) -> Self {
        self.registrar = Some(r);
        self
    }

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
    }

    /// Ollama répond-il ? Résultat gardé 60 s.
    async fn ollama_probe(&self) -> Option<Result<(), String>> {
        let client = self.ollama.as_ref()?;
        if let Some((at, res)) = self.probe.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed() < PROBE_TTL {
                return Some(res.clone());
            }
        }
        let res = client.ping().await.map_err(|e| e.to_string());
        *self.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), res.clone()));
        Some(res)
    }
}

fn db_err(e: impl std::fmt::Display) -> ApiError {
    eprintln!("interface : erreur de base : {e}");
    ApiError::internal("erreur interne")
}

fn entry_json(e: &astro_dav::vfs::Entry) -> Value {
    json!({
        "name": astro_dav::vfs::name_of(&e.path), "path": e.path,
        "type": if e.kind == astro_dav::vfs::EntryKind::Dir { "dir" } else { "file" },
        "size": e.size, "mime": e.mime, "mtime": e.mtime,
    })
}

/// Compte hébergé : clé, rôle, email, contrat, état du cycle de vie (JSON), octets utilisés, fichiers.
type AccountRow = (String, String, Option<String>, Option<String>, Option<String>, i64, i64);

/// Miniature d'une entrée, sans son CID ni sa taille : l'interface n'a besoin que des dimensions.
fn thumb_of(json: Option<String>) -> Value {
    let Some(v) = json.and_then(|j| serde_json::from_str::<Value>(&j).ok()) else { return Value::Null };
    json!({ "w": v["w"], "h": v["h"], "ow": v["ow"], "oh": v["oh"] })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn db_bytes(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))
}

impl<B: BlobStore> Backend for StationUi<B> {
    async fn authenticate(&self, headers: &HeaderMap, method: &str, uri: &Uri) -> Option<Principal> {
        let account = self.dav.account_of(headers, method, uri).await?;
        let hex = account.owner.clone();
        let (email, role): (Option<String>, Option<String>) = self
            .services
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT email, role FROM identities WHERE hex = ?1", [hex], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or((None, None))))
            .await
            .ok()?;
        Some(Principal { hex: account.owner, email, captain: role.as_deref() == Some("captain") })
    }

    async fn register_info(&self) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let limits = r.limits();
        let accounts = r.hosted_accounts().await.map_err(|e| ApiError::internal(e.to_string()))?;
        let open = limits.open && (limits.max_accounts == 0 || accounts < limits.max_accounts);
        Ok(json!({ "open": open, "constellation": r.constellation(), "accounts": accounts, "max_accounts": limits.max_accounts }))
    }

    async fn register(&self, ip: Option<std::net::IpAddr>, email: String, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        r.register(ip, &email, &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
    }

    async fn register_guest(&self, ip: Option<std::net::IpAddr>, alias: Option<String>, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        r.register_guest(ip, alias.as_deref(), &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
    }

    async fn invite_create(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let req: crate::invites::CreateRequest = serde_json::from_value(body).map_err(|e| ApiError::bad_request(format!("demande illisible : {e}")))?;
        self.invites_service()?.create(&who.hex, req).await.map_err(invite_error)
    }

    async fn invites(&self, who: &Principal) -> Result<Value, ApiError> {
        self.invites_service()?.list(&who.hex).await.map_err(invite_error)
    }

    async fn invite_revoke(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.revoke(&who.hex, &token).await.map_err(invite_error)
    }

    async fn invite_preview(&self, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.preview(&token).await.map_err(invite_error)
    }

    async fn invite_thumb(&self, token: String, index: usize) -> Result<Vec<u8>, ApiError> {
        self.invites_service()?.thumb(&token, index).await.map_err(invite_error)
    }

    async fn invite_claim(&self, ip: Option<std::net::IpAddr>, token: String, who: Applicant, lang: String) -> Result<Value, ApiError> {
        let svc = self.invites_service()?;
        let claimed = match who {
            Applicant::Email(email) => svc.claim(&token, ip, &email, &lang).await,
            Applicant::Guest(alias) => svc.claim_guest(&token, ip, alias.as_deref(), &lang).await,
        }
        .map_err(invite_error)?;
        let mut out = registered_json(&claimed.registered);
        out["from"] = json!(claimed.from);
        out["shared"] = json!(claimed.shared);
        out["warnings"] = json!(claimed.warnings);
        Ok(out)
    }

    async fn dashboard(&self, who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let owner = who.hex.clone();
        let (events, last_event, size, files, bytes, manifest) = svc
            .db
            .readers()
            .read(move |c| {
                let (events, last): (i64, i64) = c.query_row("SELECT COUNT(*), COALESCE(MAX(created_at), 0) FROM nostr_events", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let (files, bytes): (i64, i64) = c.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL",
                    [&owner],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let manifest: Option<(String, i64, String)> = c
                    .query_row("SELECT id, created_at, tags FROM nostr_events WHERE kind = 30078 AND pubkey = ?1 AND d_tag = ?2", params![owner, astro_dav::manifest::D_TAG], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok();
                Ok((events, last, db_bytes(c)?, files, bytes, manifest))
            })
            .await
            .map_err(db_err)?;
        let (m_at, m_event, m_rev) = match manifest {
            Some((id, at, tags)) => {
                let rev = serde_json::from_str::<Vec<Vec<String>>>(&tags).ok().and_then(|t| t.into_iter().find(|t| t.first().map(String::as_str) == Some("rev")).and_then(|t| t.get(1).cloned()));
                (Some(at), Some(id.chars().take(12).collect::<String>()), rev)
            }
            None => (None, None, None),
        };
        let (balance, weeks) = match svc.balance_of(&who.hex).await {
            Ok(b) => (json!({ "g1_cents": b.g1_cents, "zen_cents": b.zen_cents, "error": null }), (svc.weekly_rent_cents > 0).then(|| b.zen_cents as f64 / svc.weekly_rent_cents as f64)),
            Err(e) => (json!({ "g1_cents": null, "zen_cents": null, "error": e }), None),
        };
        let p2p = match &svc.p2p {
            Some(h) => json!({ "enabled": true, "peers": h.connected_peers().len(), "listen": h.listen_addrs().len(), "score": (svc.hardware.score() * 10.0).round() / 10.0, "tier": svc.hardware.tier().label() }),
            None => json!({ "enabled": false, "score": (svc.hardware.score() * 10.0).round() / 10.0, "tier": svc.hardware.tier().label() }),
        };
        Ok(json!({
            "station": { "node": svc.info.node_hex.chars().take(12).collect::<String>(), "peer_id": svc.info.peer_id, "domain": svc.info.domain, "version": env!("CARGO_PKG_VERSION"), "uptime_secs": svc.info.started.elapsed().as_secs(), "captain": who.captain },
            "relay": { "events": events, "last_event_at": (last_event > 0).then_some(last_event), "db_bytes": size },
            "p2p": p2p,
            "balance": balance,
            "energy": { "weeks": weeks, "max_weeks": GAUGE_WEEKS, "weekly_rent_cents": svc.weekly_rent_cents },
            "sovereignty": { "encrypted_files": files, "encrypted_bytes": bytes, "last_manifest_at": m_at, "last_manifest_event": m_event, "last_manifest_rev": m_rev },
        }))
    }

    async fn ucloud_list(&self, who: &Principal, path: String) -> Result<Value, ApiError> {
        let path = astro_dav::vfs::normalize_path(&path).map_err(|_| ApiError::bad_request("chemin invalide"))?;
        let (owner, p) = (who.hex.clone(), path.clone());
        let listing = self
            .services
            .db
            .readers()
            .read(move |c| {
                let Some(dir) = astro_dav::vfs::get_entry(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)? else { return Ok(None) };
                if dir.kind != astro_dav::vfs::EntryKind::Dir {
                    return Ok(Some(Vec::new()));
                }
                Ok(Some(astro_dav::vfs::list_children(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)?))
            })
            .await
            .map_err(db_err)?;
        let Some(entries) = listing else { return Err(ApiError::not_found("dossier introuvable")) };
        let mut entries: Vec<Value> = entries.iter().map(entry_json).collect();
        // Dossiers d'abord, puis par nom.
        entries.sort_by(|a, b| (a["type"] != "dir", a["name"].as_str()).cmp(&(b["type"] != "dir", b["name"].as_str())));
        Ok(json!({ "path": path, "dav": self.dav.config.mount, "entries": entries }))
    }

    async fn ucloud_photos(&self, who: &Principal, before: Option<i64>, limit: usize) -> Result<Value, ApiError> {
        let owner = who.hex.clone();
        let limit_i = i64::try_from(limit).unwrap_or(60);
        let (items, next) = self
            .services
            .db
            .readers()
            .read(move |c| {
                const COLS: &str = "path, mime, size, mtime, taken_at, COALESCE(taken_at, mtime) AS t, json_extract(data, '$.thumb')";
                const KIND: &str = "owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND (mime LIKE 'image/%' OR mime LIKE 'video/%')";
                let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, Value)> {
                    let t: i64 = r.get(5)?;
                    Ok((t, json!({ "path": r.get::<_, String>(0)?, "mime": r.get::<_, Option<String>>(1)?, "size": r.get::<_, Option<i64>>(2)?, "mtime": r.get::<_, Option<i64>>(3)?, "taken_at": r.get::<_, Option<i64>>(4)?, "thumb": thumb_of(r.get::<_, Option<String>>(6)?) })))
                };
                let mut stmt = c.prepare_cached(&format!("SELECT {COLS} FROM ucloud_entries WHERE {KIND} AND (?2 IS NULL OR COALESCE(taken_at, mtime) < ?2) ORDER BY t DESC, path LIMIT ?3"))?;
                let mut rows = stmt.query_map(params![owner, before, limit_i], map)?.collect::<Result<Vec<_>, _>>()?;
                let mut next = None;
                if rows.len() as i64 == limit_i {
                    // Page pleine : on y ajoute les photos de la même seconde que la dernière (ordre par chemin),
                    // sinon la page suivante (`t < dernier`) les sauterait.
                    if let Some((t, last)) = rows.last().cloned() {
                        let last_path = last["path"].as_str().unwrap_or("").to_owned();
                        let mut tie = c.prepare_cached(&format!("SELECT {COLS} FROM ucloud_entries WHERE {KIND} AND COALESCE(taken_at, mtime) = ?2 AND path > ?3 ORDER BY path"))?;
                        rows.extend(tie.query_map(params![owner, t, last_path], map)?.collect::<Result<Vec<_>, _>>()?);
                        next = Some(t);
                    }
                }
                Ok((rows.into_iter().map(|r| r.1).collect::<Vec<_>>(), next))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "dav": self.dav.config.mount, "items": items, "next": next }))
    }

    async fn ucloud_share(&self, who: &Principal, paths: Vec<String>, ttl_secs: i64, origin: String) -> Result<Value, ApiError> {
        use astro_dav::share::{ShareError, create_guest_link};
        let title = who.email.clone().unwrap_or_else(|| "uCloud".into());
        let link = create_guest_link(&self.dav.db, self.dav.blobs.as_ref(), &who.hex, &paths, &title, ttl_secs, Self::now()).await.map_err(|e| match e {
            ShareError::Nothing(p) => ApiError::not_found(format!("rien à partager : {p}")),
            ShareError::TooMany => ApiError::bad_request("trop de fichiers pour un seul lien"),
            other => {
                eprintln!("interface : partage impossible : {other}");
                ApiError::internal("création du lien impossible")
            }
        })?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "ucloud", "ui:share", format!("lien magique créé ({} fichier(s))", link.items), None);
        Ok(json!({ "url": format!("{origin}{}#{}", link.path, link.key_hex), "expires_at": link.expires_at, "items": link.items }))
    }

    async fn faces(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "enabled": false, "dav": self.dav.config.mount, "people": [], "unknown": 0, "suggestions": [] }));
        }
        let owner = who.hex.clone();
        let (people, unknown, suggestions) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let bbox = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<Value> { Ok(json!([r.get::<_, f64>(i)?, r.get::<_, f64>(i + 1)?, r.get::<_, f64>(i + 2)?, r.get::<_, f64>(i + 3)?])) };
                // La ligne de meilleur score de détection représente la personne (colonnes nues de MAX()).
                let mut p = c.prepare_cached(
                    "SELECT name, COUNT(DISTINCT cid), face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2, MAX(det_score), \
                            (SELECT json_extract(e.data, '$.thumb') FROM ucloud_entries e WHERE e.owner_hex = faces.owner_hex AND e.path = faces.ucloud_path) FROM faces \
                     WHERE owner_hex = ?1 AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL AND name NOT LIKE 'Inconnu\\_%' ESCAPE '\\' GROUP BY name ORDER BY COUNT(DISTINCT cid) DESC, name LIMIT 200",
                )?;
                let people = p
                    .query_map([&owner], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?, "sample": { "face_id": r.get::<_, i64>(2)?, "name": r.get::<_, String>(0)?, "path": r.get::<_, String>(3)?, "bbox": bbox(r, 4)?, "thumb": thumb_of(r.get::<_, Option<String>>(9)?) } })))?
                    .collect::<Result<Vec<_>, _>>()?;
                let unknown: i64 = c.query_row("SELECT COUNT(*) FROM faces WHERE owner_hex = ?1 AND name LIKE 'Inconnu\\_%' ESCAPE '\\' AND suggest_name IS NULL", [&owner], |r| r.get(0))?;
                let mut s = c.prepare_cached(
                    "SELECT face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2, suggest_name, suggest_score, \
                            (SELECT json_extract(e.data, '$.thumb') FROM ucloud_entries e WHERE e.owner_hex = faces.owner_hex AND e.path = faces.ucloud_path) FROM faces \
                     WHERE owner_hex = ?1 AND suggest_name IS NOT NULL AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL ORDER BY suggest_score DESC LIMIT 50",
                )?;
                let suggestions = s
                    .query_map([&owner], |r| Ok(json!({ "face_id": r.get::<_, i64>(0)?, "path": r.get::<_, String>(1)?, "bbox": bbox(r, 2)?, "suggest_name": r.get::<_, String>(6)?, "score": r.get::<_, Option<f64>>(7)?.unwrap_or(0.0), "thumb": thumb_of(r.get::<_, Option<String>>(8)?) })))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((people, unknown, suggestions))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "enabled": true, "dav": self.dav.config.mount, "people": people, "unknown": unknown, "suggestions": suggestions }))
    }

    async fn face_accept(&self, who: &Principal, face_id: i64) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("FaceCloud n'est pas activé (--face-engine)"));
        }
        match astro_facecloud::review::accept_suggestion(&self.services.db, &who.hex, face_id).await {
            Ok(true) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", "suggestion validée", None);
                Ok(json!({ "accepted": true }))
            }
            Ok(false) => Err(ApiError::not_found("aucune suggestion à valider pour ce visage")),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn face_name(&self, who: &Principal, face_id: i64, name: String) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("FaceCloud n'est pas activé (--face-engine)"));
        }
        match astro_facecloud::review::name_face(&self.services.db, &who.hex, face_id, None, &name).await {
            Ok(()) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", "visage nommé", None);
                Ok(json!({ "named": name }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::not_found(m)),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn consent(&self, who: &Principal) -> Result<Value, ApiError> {
        let owner = who.hex.clone();
        let row: Option<(i64, i64)> = self.services.db.readers().read(move |c| Ok(c.query_row("SELECT version, accepted_at FROM private_copy_consent WHERE owner_hex = ?1", [owner], |r| Ok((r.get(0)?, r.get(1)?))).ok())).await.map_err(db_err)?;
        let current = i64::from(astro_dav::media::CONSENT_VERSION);
        Ok(json!({
            "accepted": row.is_some_and(|(v, _)| v >= current), "version": row.map(|r| r.0), "accepted_at": row.map(|r| r.1),
            "current_version": current, "article": "CPI L. 122-5", "text": astro_dav::media::CONSENT_TEXT, "available": self.consent_fn.is_some(),
        }))
    }

    async fn consent_accept(&self, who: &Principal, version: u32) -> Result<Value, ApiError> {
        let Some(f) = &self.consent_fn else { return Err(ApiError::unavailable("dépôt de médias non disponible sur cette station")) };
        if version != astro_dav::media::CONSENT_VERSION {
            return Err(ApiError::bad_request("le texte d'attestation a changé : rechargez la page avant d'attester"));
        }
        f(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : attestation de copie privée : {e}");
            ApiError::internal("attestation impossible : clé du compte absente de cette station ?")
        })?;
        self.consent(who).await
    }

    async fn footprint(&self, who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let account = astro_dav::auth::lookup_by_hex(&svc.db, &who.hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let base = self.dav.config.limits_without_leases(account.tier).quota_total;
        let limits = self.dav.limits_for(&account).await;
        let (owner, node, now) = (who.hex.clone(), svc.info.node_hex.clone(), now_secs());
        let (opex, capex) = self.opex_capex(&who.hex, &account).await?;
        let (used, files, leases, tariffs) = svc
            .db
            .readers()
            .read(move |c| {
                let (used, files): (i64, i64) = c.query_row("SELECT COALESCE(SUM(size), 0), COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [&owner], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut stmt = c.prepare_cached("SELECT src_event, node, resource, start_at, end_at, units, paid_cents, status FROM resource_leases WHERE beneficiary = ?1 ORDER BY start_at DESC LIMIT 50")?;
                let leases = stmt
                    .query_map([&owner], |r| {
                        let (start, end, status): (i64, i64, String) = (r.get(3)?, r.get(4)?, r.get(7)?);
                        let resource: String = r.get(2)?;
                        Ok(json!({
                            "id": r.get::<_, String>(0)?.chars().take(12).collect::<String>(), "mine": r.get::<_, String>(1)? == node, "resource": resource,
                            "start": start, "end": end, "units": r.get::<_, i64>(5)?, "paid_cents": r.get::<_, i64>(6)?, "status": status,
                            "active": status == "active" && start <= now && now < end,
                            "used_secs": if status == "active" { (now.min(end) - start).max(0) } else { 0 },
                        }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut t = c.prepare_cached("SELECT resource, price_cents, unit FROM tariffs WHERE node_hex = ?1 ORDER BY resource")?;
                let tariffs = t.query_map([&node], |r| Ok(json!({ "resource": r.get::<_, String>(0)?, "price_cents": r.get::<_, i64>(1)?, "unit": r.get::<_, String>(2)? })))?.collect::<Result<Vec<_>, _>>()?;
                Ok((used, files, leases, tariffs))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({
            "disk": { "used_bytes": used, "files": files, "quota_bytes": limits.quota_total, "base_quota_bytes": base, "leased": limits.quota_total > base, "tier": format!("{:?}", account.tier) },
            "leases": leases, "tariffs": tariffs, "opex": opex, "capex": capex,
        }))
    }

    async fn faces_board(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "enabled": false, "faces": [], "people": [], "unknown": 0 }));
        }
        let b = self.board.board(&self.services.db, &who.hex).await.map_err(db_err)?;
        let faces: Vec<Value> = b
            .faces
            .iter()
            .map(|f| {
                json!({
                    "id": f.face_id, "name": f.name, "pubkey": f.pubkey, "named": f.named, "path": f.path, "bbox": f.bbox,
                    "x": (f64::from(f.x) * 10_000.0).round() / 10_000.0, "y": (f64::from(f.y) * 10_000.0).round() / 10_000.0,
                    "group_id": f.group.as_ref().map(|g| g.0.clone()), "group_size": f.group.as_ref().map(|g| g.1),
                    "suggest": f.suggest.as_ref().map(|(name, score)| json!({ "name": name, "score": score })),
                })
            })
            .collect();
        let people: Vec<Value> = b.people.iter().map(|p| json!({ "name": p.name, "pubkey": p.pubkey, "faces": p.faces, "photos": p.photos })).collect();
        Ok(json!({ "enabled": true, "dav": self.dav.config.mount, "faces": faces, "people": people, "unknown": b.unknown, "groups_skipped": b.groups_skipped, "truncated": b.truncated }))
    }

    async fn faces_thumbs(&self, who: &Principal, ids: Vec<i64>, size: u32) -> Result<Value, ApiError> {
        let f = self.thumbs_fn.as_ref().ok_or_else(|| ApiError::unavailable("vignettes de visages non activées (--face-engine)"))?;
        let (thumbs, failed) = f(who.hex.clone(), ids, size).await.map_err(|e| {
            eprintln!("interface : vignettes de visages : {e}");
            ApiError::internal("vignettes indisponibles")
        })?;
        let map: serde_json::Map<String, Value> = thumbs.into_iter().map(|(id, url)| (id.to_string(), Value::String(url))).collect();
        Ok(json!({ "thumbs": map, "failed": failed.into_iter().map(|(id, why)| json!([id, why])).collect::<Vec<_>>() }))
    }

    async fn faces_assign(&self, who: &Principal, ids: Vec<i64>, name: String, pubkey: Option<String>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("FaceCloud n'est pas activé (--face-engine)"));
        }
        match astro_facecloud::review::name_faces(&self.services.db, &who.hex, &ids, pubkey.as_deref(), &name).await {
            Ok(changed) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{changed} visage(s) rangé(s) dans « {name} »"), None);
                // Une fiche de contact du même nom relie aussitôt la personne à son MULTIPASS (et partage ses photos).
                if let Some(links) = &self.contacts {
                    let (links, owner) = (links.clone(), who.hex.clone());
                    tokio::spawn(async move {
                        if let Err(e) = links.reconcile(&owner).await {
                            eprintln!("annuaire : rapprochement impossible : {e:?}");
                        }
                    });
                }
                Ok(json!({ "assigned": changed, "name": name }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::bad_request(m)),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn faces_forget(&self, who: &Principal, ids: Vec<i64>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("FaceCloud n'est pas activé (--face-engine)"));
        }
        match astro_facecloud::review::forget_faces(&self.services.db, &who.hex, &ids).await {
            Ok(gone) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{gone} visage(s) oublié(s)"), None);
                Ok(json!({ "forgotten": gone }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::bad_request(m)),
            Err(e) => Err(db_err(e)),
        }
    }

    async fn contacts(&self, who: &Principal) -> Result<Value, ApiError> {
        let links = self.contacts_service()?;
        let mut view = links.list(&who.hex).await.map_err(link_error)?;
        let login = crate::account_login(&self.services.db, &who.hex).await;
        let base = self.dav.config.public_base_url.clone().unwrap_or_default();
        view["carddav"] = json!({ "url": format!("{}{}/addressbooks/{login}/contacts/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "server": format!("{}{}/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "login": login });
        Ok(view)
    }

    async fn contacts_sync(&self, who: &Principal) -> Result<Value, ApiError> {
        let r = self.contacts_service()?.sync_owner(&who.hex).await.map_err(link_error)?;
        Ok(json!({ "indexed": r.indexed, "removed": r.removed, "linked": r.linked.iter().map(|(n, _)| n).collect::<Vec<_>>() }))
    }

    async fn contacts_create(&self, who: &Principal, name: String, email: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.create_from_person(&who.hex, &name, email.as_deref()).await.map_err(link_error)
    }

    async fn contacts_link(&self, who: &Principal, path: String, target: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.link(&who.hex, &path, target.as_deref()).await.map_err(link_error)
    }

    async fn account(&self, who: &Principal) -> Result<Value, ApiError> {
        self.account_service()?.view(&who.hex).await.map_err(account_error)
    }

    async fn account_profile(&self, who: &Principal, patch: Value) -> Result<Value, ApiError> {
        self.account_service()?.update_profile(&who.hex, &patch).await.map_err(account_error)
    }

    async fn account_unlock(&self, ip: Option<std::net::IpAddr>, email: String, pass: String) -> Result<Value, ApiError> {
        self.account_service()?.unlock(ip, &email, &pass).await.map_err(account_error)
    }

    async fn account_sign(&self, who: &Principal, event: Value) -> Result<Value, ApiError> {
        self.account_service()?.sign(&who.hex, &event).await.map_err(account_error)
    }

    async fn pass_unlock(&self, _who: &Principal, hex: String) -> Result<Value, ApiError> {
        let v = self.account_service()?.clear_pass_lock(&hex).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "pass", format!("verrouillage du PASS levé pour {}", &hex[..8]), None);
        Ok(v)
    }

    async fn account_token_rotate(&self, who: &Principal) -> Result<Value, ApiError> {
        let v = self.account_service()?.rotate_token(&who.hex).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "system", "ui:token", "jeton de connexion changé : l'ancien ne fonctionne plus".to_owned(), None);
        Ok(v)
    }

    async fn account_export_keys(&self, who: &Principal, confirm: bool) -> Result<Value, ApiError> {
        let v = self.account_service()?.export_keys(&who.hex, confirm).await.map_err(account_error)?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "alert", "ui:keys", "vos clés secrètes viennent d'être exportées".to_owned(), None);
        Ok(v)
    }

    async fn admin_get(&self, _who: &Principal, section: &str) -> Result<Value, ApiError> {
        let f = self.admin.as_ref().ok_or_else(|| ApiError::not_found("section d'administration inconnue"))?;
        f(section.to_owned(), None).await.map_err(admin_error)
    }

    async fn admin_set(&self, _who: &Principal, section: &str, body: Value) -> Result<Value, ApiError> {
        let f = self.admin.as_ref().ok_or_else(|| ApiError::not_found("section d'administration inconnue"))?;
        let view = f(section.to_owned(), Some(body)).await.map_err(admin_error)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:admin", format!("réglage « {section} » modifié par le Capitaine"), None);
        Ok(view)
    }

    async fn coop_config(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (get, _) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        get().await.map_err(|e| {
            eprintln!("interface : configuration coopérative : {e}");
            ApiError::internal("configuration coopérative illisible")
        })
    }

    async fn coop_config_set(&self, who: &Principal, patch: std::collections::BTreeMap<String, Option<String>>) -> Result<Value, ApiError> {
        let (_, set) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        let keys: Vec<String> = patch.keys().cloned().collect();
        let view = set(patch).await.map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:coop", format!("configuration coopérative modifiée : {}", keys.join(", ")), None);
        let _ = who;
        Ok(view)
    }

    async fn friends(&self, who: &Principal) -> Result<Value, ApiError> {
        let f = self.friends_fn.as_ref().ok_or_else(|| ApiError::unavailable("envoi à des proches non activé"))?;
        f(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : proches : {e}");
            ApiError::internal("liste des proches indisponible")
        })
    }

    async fn ucloud_send(&self, who: &Principal, paths: Vec<String>, friends: Vec<String>) -> Result<Value, ApiError> {
        let f = self.send_fn.as_ref().ok_or_else(|| ApiError::unavailable("envoi à des proches non activé"))?;
        f(who.hex.clone(), paths, friends).await.map_err(ApiError::bad_request)
    }

    async fn replication(&self, who: &Principal) -> Result<Value, ApiError> {
        let hex = who.hex.clone();
        let (mine, crawl, routes) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let get = |scope: &str, key: &str| -> Option<Value> {
                    rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [scope, key], |r| r.get::<_, String>(0))).ok().flatten().and_then(|t| serde_json::from_str(&t).ok())
                };
                Ok((get("replication", &hex), get("crawl_status", "last"), get("crawl_status", "routes")))
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "mine": mine, "crawl": crawl, "routes": routes.unwrap_or_else(|| json!([])) }))
    }

    async fn constellation(&self, _who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let origin = match svc.origin {
            Some(o) => Some(o),
            None => svc
                .db
                .readers()
                .read(|c| Ok(c.query_row("SELECT lat, lon FROM identities WHERE lat IS NOT NULL AND lon IS NOT NULL ORDER BY (role = 'node') DESC, hosted DESC LIMIT 1", [], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok()))
                .await
                .map_err(db_err)?,
        };
        let dir = svc.swarm_dir.clone();
        let peers: Vec<Candidate> = tokio::task::spawn_blocking(move || dir.map(|d| astro_p2p::routing::read_swarm_dir(&d)).unwrap_or_default())
        .await
        .map_err(db_err)?;
        let prices: Vec<(String, String, i64, String)> = svc
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT i.peer_id, t.resource, t.price_cents, t.unit FROM tariffs t JOIN identities i ON i.hex = t.node_hex WHERE i.peer_id IS NOT NULL ORDER BY t.resource")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(db_err)?;
        let connected: Vec<String> = svc.p2p.as_ref().map(|h| h.connected_peers().iter().map(ToString::to_string).collect()).unwrap_or_default();
        let me = svc.info.peer_id.clone().unwrap_or_default();
        let describe = |node_id: &str, power: f64, pos: Option<(f64, f64)>, services: Vec<String>, is_self: bool| {
            let distance = match (origin, pos) {
                (Some(o), Some(p)) => Some((haversine_km(o, p) * 10.0).round() / 10.0),
                _ => None,
            };
            let tariffs: Vec<Value> = prices.iter().filter(|p| p.0 == node_id).map(|p| json!({ "resource": p.1, "price_cents": p.2, "unit": p.3 })).collect();
            json!({
                "node_id": node_id, "self": is_self, "power_score": (power * 10.0).round() / 10.0, "tier": astro_p2p::routing::Tier::of(power).label(),
                "distance_km": distance, "lat": pos.map(|p| p.0), "lon": pos.map(|p| p.1),
                "status": if is_self { "local" } else if connected.iter().any(|c| c == node_id) { "connected" } else { "known" },
                "services": services, "tariffs": tariffs,
            })
        };
        let mut nodes = vec![describe(&me, svc.hardware.score(), origin, svc.exposed.clone(), true)];
        let mut others: Vec<&Candidate> = peers.iter().filter(|p| p.node_id != me).collect();
        others.sort_by(|a, b| b.power_score.total_cmp(&a.power_score).then_with(|| a.node_id.cmp(&b.node_id)));
        nodes.extend(others.iter().map(|p| describe(&p.node_id, p.power_score, p.lat.zip(p.lon), p.services.clone(), false)));
        Ok(json!({ "origin": origin.map(|o| json!({ "lat": o.0, "lon": o.1 })), "nodes": nodes, "swarm_dir_configured": svc.swarm_dir.is_some() }))
    }

    async fn economy(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (latest, love, recent) = self
            .services
            .db
            .readers()
            .read(|c| {
                let latest: Option<(String, i64, String)> = c
                    .query_row("SELECT stream, at, data FROM journal WHERE stream IN ('economy.3x13', 'economy.drawdown', 'economy.volunteer') ORDER BY at DESC, id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok();
                let love: (i64, i64) = c.query_row(astro_economy::store::love_totals(), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut stmt = c.prepare("SELECT at, kind, amount_cents, detail FROM love_ledger ORDER BY at DESC, id DESC LIMIT 8")?;
                let recent = stmt.query_map([], |r| Ok(json!({ "at": r.get::<_, i64>(0)?, "kind": r.get::<_, Option<String>>(1)?, "amount_cents": r.get::<_, i64>(2)?, "detail": r.get::<_, Option<String>>(3)?.and_then(|d| serde_json::from_str::<Value>(&d).ok()) })))?.collect::<Result<Vec<_>, _>>()?;
                Ok((latest, love, recent))
            })
            .await
            .map_err(db_err)?;
        let cents = |v: &Value, k: &str| v[k].as_str().and_then(|s| s.parse::<Amount>().ok()).map(|a| a.0);
        let (kind, at, week, allocation, shares, gap) = match latest {
            Some((stream, at, data)) => {
                let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                match stream.as_str() {
                    "economy.3x13" => {
                        let (cash, rnd, assets) = (cents(&v, "cash").unwrap_or(0), cents(&v, "rnd").unwrap_or(0), cents(&v, "assets").unwrap_or(0));
                        let total = (cash + rnd + assets).max(1);
                        let share = |name: &str, c: u64| json!({ "name": name, "cents": c, "pct": (c as f64 * 1000.0 / total as f64).round() / 10.0 });
                        let alloc = json!({ "surplus_cents": cents(&v, "surplus"), "is_provision_cents": cents(&v, "is_provision"), "net_cents": cents(&v, "net"), "captain_bonus_cents": cents(&v, "captain_bonus") });
                        ("split", Some(at), v["week"].as_str().map(str::to_owned), Some(alloc), vec![share("Trésorerie", cash), share("R&D", rnd), share("Actifs", assets)], None)
                    }
                    "economy.drawdown" => ("drawdown", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "gap")),
                    _ => ("volunteer", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "deficit")),
                }
            }
            None => ("none", None, None, None, Vec::new(), None),
        };
        Ok(json!({
            "kind": kind, "at": at, "week": week, "allocation": allocation, "shares": shares, "gap_cents": gap,
            "love": { "total_cents": love.0, "weeks": love.1, "recent": recent },
        }))
    }

    async fn accounts(&self, _who: &Principal) -> Result<Value, ApiError> {
        let rows: Vec<AccountRow> = self
            .services
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT i.hex, i.role, i.email, i.contract, (SELECT value FROM kv WHERE scope = 'lifecycle' AND key = i.hex),                             COALESCE((SELECT SUM(size) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file'), 0),                             (SELECT COUNT(*) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file')                      FROM identities i WHERE i.hosted = 1 AND i.role IN ('multipass', 'captain', 'zencard') ORDER BY i.email, i.hex",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        for (hex, role, email, contract, state, used, files) in rows {
            let quota = match astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await {
                Some(a) => self.dav.limits_for(&a).await.quota_total,
                None => 0,
            };
            let life: Value = state.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
            let status = life["status"].as_str().map(str::to_owned).or_else(|| contract.clone());
            let grace_ends = (status.as_deref() == Some("grace_period")).then(|| life["status_since"].as_i64().map(|s| s + astro_lifecycle::state::GRACE_SECS as i64)).flatten();
            let pass_locked_until = match &self.account {
                Some(a) => a.pass_locked_until(&hex).await,
                None => None,
            };
            out.push(json!({
                "pass_locked_until": pass_locked_until,
                "hex": hex, "role": role, "email": email, "contract": contract, "status": status, "tracked": !life.is_null(),
                "last_ok_at": life["last_ok_at"], "status_since": life["status_since"], "grace_ends_at": grace_ends,
                "used_bytes": used, "files": files, "quota_bytes": quota,
            }));
        }
        Ok(json!({ "accounts": out }))
    }

    async fn account_token(&self, who: &Principal, hex: String) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let login: Option<String> = self.services.db.readers().read({
            let hex = hex.clone();
            move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [hex], |r| r.get::<_, Option<String>>(0)).ok().flatten())
        }).await.map_err(db_err)?;
        let token = astro_dav::auth::account_token(&self.services.db, &self.dav.config.station_secret, &account.owner).await;
        // Trace : un jeton remis par le Capitaine est une opération sensible.
        let audit = json!({ "by": who.hex, "for": hex }).to_string();
        let _ = self.services.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.token', ?1, ?2)", vec![now_secs().into(), audit.into()]).await;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:token", format!("jeton de secours remis pour {}", login.as_deref().unwrap_or(&hex[..8])), None);
        Ok(json!({ "login": login.unwrap_or_else(|| hex.clone()), "token": token }))
    }

    /// Console de BRO : `#commande` exécute la commande (comme par message privé) ; un autre texte est noté dans le
    /// journal (emplacement 15) après avoir cherché les souvenirs proches. Pas de génération de réponse par un LLM.
    async fn bro_say(&self, who: &Principal, text: String) -> Result<Value, ApiError> {
        if let Some(command) = bus::parse(&text) {
            let out = bus::execute(self.services.as_ref(), &command, &who.hex).await;
            return Ok(json!({ "reply": out.unwrap_or_else(|e| format!("⚠️ {e}")) }));
        }
        let Some(memory) = &self.memory else { return Err(ApiError::unavailable("la mémoire de BRO demande un moteur d'embeddings (--ollama)")) };
        let related = memory.recall(&who.hex, &text, 3, None).await.map_err(|e| ApiError::unavailable(format!("mémoire indisponible : {e}")))?;
        let meta = Meta { source: "console".into(), ..Meta::default() };
        let mut reply = match memory.remember(&who.hex, Slot::Journal, &text, meta).await {
            Ok(r) if r.mem_id.is_some() => "📝 Noté dans votre journal.".to_owned(),
            Ok(_) => "📝 Je le savais déjà.".to_owned(),
            Err(e @ astro_bro::BroError::Unstorable(_)) => format!("🔒 {e} : je ne le garde pas."),
            Err(e) => return Err(ApiError::unavailable(format!("mémoire indisponible : {e}"))),
        };
        if !related.is_empty() {
            reply.push_str("\nJe me souviens que :");
            for r in related {
                reply.push_str(&format!("\n• {}", r.memory.content));
            }
        }
        Ok(json!({ "reply": reply }))
    }

    async fn health(&self) -> Vec<Subsystem> {
        let svc = &self.services;
        let mut out = Vec::new();
        out.push(match svc.db.readers().read(|c| Ok((c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?, c.query_row("SELECT COUNT(*), COALESCE(MAX(created_at), 0) FROM nostr_events", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?))).await {
            Ok((mode, _)) if mode != "wal" => Subsystem::new("db", Status::Degraded, format!("journal {mode} (WAL attendu)")),
            Ok(_) => Subsystem::new("db", Status::Ok, "SQLite WAL"),
            Err(e) => Subsystem::new("db", Status::Down, e.to_string()),
        });
        out.push(Subsystem::new("relay", Status::Ok, "relais Nostr à l'écoute"));
        out.push(match &svc.p2p {
            None => Subsystem::new("p2p", Status::Disabled, "maillage non activé"),
            Some(h) => {
                let n = h.connected_peers().len();
                if n == 0 { Subsystem::new("p2p", Status::Degraded, "aucun pair connecté") } else { Subsystem::new("p2p", Status::Ok, format!("{n} pair(s)")) }
            }
        });
        out.push(match (&svc.chain, svc.last_chain_call()) {
            (None, _) => Subsystem::new("duniter", Status::Disabled, "nœud Duniter non configuré"),
            (Some(_), None) => Subsystem::new("duniter", Status::Ok, "configuré, pas encore sollicité"),
            (Some(_), Some((age, true))) => Subsystem::new("duniter", Status::Ok, format!("dernière lecture réussie il y a {} s", age.as_secs())),
            (Some(_), Some((age, false))) => Subsystem::new("duniter", Status::Degraded, format!("dernière lecture en échec il y a {} s", age.as_secs())),
        });
        out.push(match self.ollama_probe().await {
            None => Subsystem::new("bro", Status::Disabled, "sans moteur d'embeddings (--ollama)"),
            Some(Ok(())) => Subsystem::new("bro", Status::Ok, "Ollama répond"),
            Some(Err(e)) => Subsystem::new("bro", Status::Degraded, e),
        });
        out.push(if self.faces_enabled { Subsystem::new("facecloud", Status::Ok, "actif") } else { Subsystem::new("facecloud", Status::Disabled, "non activé") });
        out
    }
}

impl<B: BlobStore> StationUi<B> {
    /// Écran MULTIPASS : redevances courantes d'usage (**Opex**) distinctes des parts d'investissement matériel
    /// (**Capex**, ZenCard). Un virement est soit un loyer, soit un bail : jamais compté deux fois.
    async fn opex_capex(&self, owner: &str, account: &astro_dav::auth::Account) -> Result<(Value, Value), ApiError> {
        let (owner, now) = (owner.to_owned(), now_secs());
        let tier = format!("{:?}", account.tier);
        let weekly_rent = self.services.weekly_rent_cents;
        let (opex, capex) = self
            .services
            .db
            .readers()
            .read(move |c| {
                let since = now - 28 * 86_400;
                let (rent_28, rent_total): (i64, i64) = c.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN at >= ?2 THEN amount_cents END), 0), COALESCE(SUM(amount_cents), 0) FROM rent_payments WHERE owner_hex = ?1",
                    rusqlite::params![owner, since],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let (usage_28, usage_week): (i64, i64) = c.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN start_at >= ?2 THEN paid_cents END), 0), \
                            COALESCE(SUM(CASE WHEN start_at <= ?3 AND end_at > ?3 THEN unit_price_cents END), 0) \
                     FROM resource_leases WHERE beneficiary = ?1 AND status = 'active'",
                    rusqlite::params![owner, since, now],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let (contract, email): (Option<String>, Option<String>) = c.query_row("SELECT contract, email FROM identities WHERE hex = ?1", [&owner], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut assets = Vec::new();
                let (mut value, mut amortized) = (0i64, 0i64);
                if let Some(email) = email {
                    let mut stmt = c.prepare("SELECT id, label, asset_type, COALESCE(value_zen, 0), COALESCE(amortized_zen, 0), status FROM capital_assets WHERE lower(email) = lower(?1) ORDER BY date, id")?;
                    for row in stmt.query_map([&email], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?, r.get::<_, Option<String>>(5)?)))? {
                        let (id, label, kind, v, a, status) = row?;
                        let (v, a) = ((v * 100.0).round() as i64, (a * 100.0).round() as i64);
                        value += v;
                        amortized += a;
                        assets.push(json!({ "id": id, "label": label, "asset_type": kind, "value_cents": v, "amortized_cents": a, "net_cents": (v - a).max(0), "status": status }));
                    }
                }
                let opex = json!({
                    "weekly_rent_cents": weekly_rent, "rent_28d_cents": rent_28, "rent_total_cents": rent_total,
                    "usage_28d_cents": usage_28, "usage_weekly_cents": usage_week, "total_28d_cents": rent_28 + usage_28,
                });
                Ok((opex, contract, assets, value, amortized))
            })
            .await
            .map_err(db_err)
            .map(|(opex, contract, assets, value, amortized)| {
                let zencard = tier == "ZenCard";
                (opex, json!({ "zencard": zencard, "tier": tier, "contract": contract, "assets": assets, "value_cents": value, "amortized_cents": amortized, "net_cents": (value - amortized).max(0) }))
            })?;
        Ok((opex, capex))
    }
}

impl<B: BlobStore> StationUi<B> {
    fn contacts_service(&self) -> Result<&Arc<crate::identity_links::IdentityLinks<B>>, ApiError> {
        self.contacts.as_ref().ok_or_else(|| ApiError::unavailable("l'annuaire n'est pas disponible sur cette station"))
    }

    fn account_service(&self) -> Result<&Arc<crate::account::AccountService>, ApiError> {
        self.account.as_ref().ok_or_else(|| ApiError::unavailable("Mon compte n'est pas disponible sur cette station"))
    }
}

/// Ce que l'inscription remet au nouveau titulaire, une seule fois.
fn register_error(e: crate::register::RegisterError) -> ApiError {
    use crate::register::RegisterError as E;
    match e {
        E::Closed | E::Full => ApiError::unavailable(e.to_string()),
        E::RateLimited(_) => ApiError::too_many(e.to_string()),
        E::Invalid(m) => ApiError::bad_request(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::Internal(m) => {
            eprintln!("inscription : {m}");
            ApiError::internal("inscription impossible : voir les journaux de la station")
        }
    }
}

fn registered_json(reg: &crate::register::Registered) -> Value {
    let a = &reg.account;
    json!({
        "email": a.email, "token": reg.token, "pass": a.pass, "salt": &*a.salt, "pepper": &*a.pepper, "nsec": &*a.nsec,
        "npub": a.npub, "hex": a.hex, "g1pub": a.g1_ss58, "nostrns": a.nostrns, "constellation": reg.constellation, "lang": a.lang, "guest": reg.guest,
    })
}

fn invite_error(e: crate::invites::InviteError) -> ApiError {
    use crate::invites::InviteError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound(m) => ApiError::not_found(m),
        E::Gone(m) => ApiError::gone(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::RateLimited(m) => ApiError::too_many(m),
        E::Unavailable(m) => ApiError::unavailable(m),
        E::Internal(m) => {
            eprintln!("invitations : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

fn link_error(e: crate::identity_links::LinkError) -> ApiError {
    use crate::identity_links::LinkError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound(m) => ApiError::not_found(m),
        E::Conflict(m) => ApiError::conflict(m),
        E::Internal(m) => {
            eprintln!("interface : annuaire : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

fn account_error(e: crate::account::AccountError) -> ApiError {
    use crate::account::AccountError as E;
    match e {
        E::Invalid(m) => ApiError::bad_request(m),
        E::NotFound => ApiError::not_found("compte non hébergé ici"),
        E::Limited(m) => ApiError::too_many(m),
        E::Denied(m) => ApiError::denied(m),
        E::Internal(m) => {
            eprintln!("interface : Mon compte : {m}");
            ApiError::internal("opération impossible : voir les journaux de la station")
        }
    }
}

fn admin_error(message: String) -> ApiError {
    if message.starts_with("section inconnue") { ApiError::not_found(message) } else { ApiError::bad_request(message) }
}
