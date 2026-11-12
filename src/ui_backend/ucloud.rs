//! uCloud : listing, photos, bibliothèque, partage, sauvegarde chez un ami, organisation, reprise WebDAV.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « ucloud ».
pub struct UcloudBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    pub(super) friends_fn: Option<FriendsFn>,
    pub(super) backup: Option<BackupFns>,
    pub(super) replicate_fn: Option<ReplicateFn>,
    pub(super) site: Option<Arc<crate::public_site::Site<B>>>,
    pub(super) pull: Option<Arc<crate::webdav_pull::PullService<B>>>,
    pub(super) organize: Option<Arc<crate::organize::OrganizeService<B>>>,
    pub(super) send_fn: Option<SendFn>,
}

impl_auth!(UcloudBackend);

impl<B: BlobStore> UcloudBackend<B> {
    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
    }

    fn organize_service(&self) -> Result<&Arc<crate::organize::OrganizeService<B>>, ApiError> {
        self.organize.as_ref().ok_or_else(|| ApiError::unavailable("rangement non disponible sur cette station"))
    }

    fn pull_service(&self) -> Result<&Arc<crate::webdav_pull::PullService<B>>, ApiError> {
        self.pull.as_ref().ok_or_else(|| ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station"))
    }
}

impl<B: BlobStore> UcloudApi for UcloudBackend<B> {
    async fn upload_image(&self, who: &Principal, data: Vec<u8>, host: String) -> Result<Value, ApiError> {
        use sha2::{Digest, Sha256};
        // Deux images décodées à la fois au plus : le décodage est du processeur, et la station a d'autres métiers.
        static BUSY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let email = who.email.clone().ok_or_else(|| ApiError::bad_request("ce compte n'a pas de dossier public : envoi impossible"))?;
        if self.site.is_none() {
            return Err(ApiError::unavailable("site public non activé sur cette station"));
        }
        let _permit = BUSY.try_acquire().map_err(|_| ApiError::too_many("des images sont en cours de traitement : réessayez dans un instant"))?;
        let original = hex::encode(Sha256::digest(&data));
        let prepared = tokio::task::spawn_blocking(move || crate::upload_image::normalize(&data)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::bad_request)?;
        let name = format!("{}.{}", &prepared.sha256[..32], prepared.ext);
        let size = prepared.data.len();
        astro_dav::ingest::replace(&self.dav, &who.hex, "/Public/uploads", &name, Some(prepared.mime), std::io::Cursor::new(prepared.data)).await.map_err(|e| match e {
            astro_dav::ingest::IngestError::TooLarge(m) => ApiError::bad_request(m),
            other => ApiError::internal(other.to_string()),
        })?;
        let url = format!("{}/site/{}/uploads/{name}", self.public_origin(&host), crate::public_site::name_of(&email));
        // Réponse NIP-96 (`nip94_event`), plus `url` à la racine : c'est ce que lisent les clients historiques.
        Ok(json!({
            "status": "success", "message": "Upload successful.", "url": url,
            "nip94_event": { "tags": [["url", url], ["ox", original], ["x", prepared.sha256], ["m", prepared.mime], ["size", size.to_string()]], "content": "" },
        }))
    }
    async fn attach(&self, who: &Principal, data: Vec<u8>, days: i64, origin: String) -> Result<Value, ApiError> {
        use astro_dav::share::{ShareError, store_attachment};
        if !self.dav.config.ipfs_gateway {
            return Err(ApiError::unavailable("passerelle /ipfs non activée sur cette station (--ipfs-gateway) : pièces jointes impossibles"));
        }
        let a = store_attachment(&self.dav.db, self.dav.blobs.as_ref(), &who.hex, &data, days * 86_400, Self::now()).await.map_err(|e| match e {
            ShareError::Refused(m) => ApiError::bad_request(m),
            other => {
                eprintln!("interface : pièce jointe impossible : {other}");
                ApiError::internal("dépôt de la pièce jointe impossible")
            }
        })?;
        if let Some(replicate) = self.replicate_fn.clone() {
            let (owner, cid, size, expires_at) = (who.hex.clone(), a.cid.clone(), a.size, a.expires_at);
            tokio::spawn(async move { replicate(owner, cid, size, expires_at).await });
        }
        // L'adresse est celle d'un contenu : n'importe quelle passerelle de la constellation qui l'a peut le servir à la même
        // adresse ; `gateway` n'est que celle où il se trouve aujourd'hui.
        Ok(json!({ "cid": a.cid, "size": a.size, "expires_at": a.expires_at, "gateway": origin, "url": format!("{origin}/ipfs/{}", a.cid) }))
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
    async fn library(&self, who: &Principal, q: astro_ui::api::LibraryQuery) -> Result<Value, ApiError> {
        use astro_ui::api::LibraryQuery as Q;
        let owner = who.hex.clone();
        let now = now_secs();
        self.services
            .db
            .readers()
            .read(move |c| {
                Ok(match q {
                    Q::Have(shas) => Ok(json!({ "have": crate::library::have(c, &owner, &shas)? })),
                    Q::Memories(day) => {
                        if let Some((md, y)) = day {
                            return Ok(Ok(json!({ "memories": crate::library::memories(c, &owner, &md, y)? })));
                        }
                        let (y, md): (i64, String) = c.query_row("SELECT CAST(strftime('%Y', ?1, 'unixepoch') AS INTEGER), strftime('%m-%d', ?1, 'unixepoch')", [now], |r| Ok((r.get(0)?, r.get(1)?)))?;
                        Ok(json!({ "memories": crate::library::memories(c, &owner, &md, y)? }))
                    }
                    Q::Timeline(None) => Ok(json!({ "months": crate::library::timeline_buckets(c, &owner)? })),
                    Q::Timeline(Some(ym)) => crate::library::timeline_month(c, &owner, &ym)?.map(|items| json!({ "ym": ym, "items": items })).ok_or_else(|| ApiError::bad_request("mois attendu au format AAAA-MM")),
                    Q::Search(s) => {
                        let bbox = match s.bbox.as_deref() {
                            None => None,
                            Some(b) => {
                                let v: Vec<f64> = b.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                                match v[..] {
                                    [s, w, n, e] if (-90.0..=90.0).contains(&s) && (-90.0..=90.0).contains(&n) && s <= n && (-180.0..=180.0).contains(&w) && (-180.0..=180.0).contains(&e) => Some((s, w, n, e)),
                                    _ => return Ok(Err(ApiError::bad_request("zone attendue : sud,ouest,nord,est en degrés"))),
                                }
                            }
                        };
                        let after = s.after_t.zip(s.after_p.clone());
                        let query = crate::library::Search { q: s.q, person: s.person, from: s.from, to: s.to, bbox, kind: s.kind, after, limit: s.limit.unwrap_or(60) };
                        Ok(crate::library::search(c, &owner, &query)?)
                    }
                    Q::Map(m) => {
                        let bbox = m.bbox.as_deref().and_then(|b| {
                            let v: Vec<f64> = b.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
                        });
                        let area = crate::library::MapArea { from: m.from, to: m.to, bbox, cell: m.cell.unwrap_or(5.0), items: m.items == Some(1) };
                        Ok(crate::library::map(c, &owner, &area)?)
                    }
                    Q::Duplicates(all) => Ok(crate::library::duplicates(c, &owner, all)?),
                })
            })
            .await
            .map_err(db_err)?
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
    async fn organize_status(&self, who: &Principal) -> Result<Value, ApiError> {
        self.organize_service()?.status(&who.hex).await.map_err(organize_err)
    }
    async fn organize_preview(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.organize_service()?.preview(&who.hex, &organize_options(body)?).await.map_err(organize_err)
    }
    async fn organize_run(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.organize_service()?.run(&who.hex, organize_options(body)?).await.map_err(organize_err)?;
        Ok(json!({ "started": true }))
    }
    async fn organize_stop(&self, who: &Principal) -> Result<Value, ApiError> {
        self.organize_service()?.stop(&who.hex).map_err(organize_err)?;
        Ok(json!({ "stopping": true }))
    }
    async fn organize_undo(&self, who: &Principal, run: String) -> Result<Value, ApiError> {
        let n = self.organize_service()?.undo(&who.hex, &run).await.map_err(organize_err)?;
        Ok(json!({ "restored": n }))
    }
    async fn pull_list(&self, who: &Principal) -> Result<Value, ApiError> {
        self.pull_service()?.list(&who.hex).await.map_err(pull_err)
    }
    async fn pull_add(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let draft: crate::webdav_pull::Draft = serde_json::from_value(body).map_err(|_| ApiError::bad_request("adresse, identifiant et mot de passe attendus"))?;
        self.pull_service()?.add(&who.hex, draft).await.map_err(pull_err)
    }
    async fn pull_remove(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.pull_service()?.remove(&who.hex, &id).await.map_err(pull_err)?;
        Ok(json!({ "removed": id }))
    }
    async fn pull_run(&self, who: &Principal, id: String, dry_run: bool) -> Result<Value, ApiError> {
        self.pull_service()?.run(&who.hex, &id, dry_run).await.map_err(pull_err)?;
        Ok(json!({ "started": id, "dry_run": dry_run }))
    }
    async fn pull_stop(&self, who: &Principal) -> Result<Value, ApiError> {
        self.pull_service()?.stop(&who.hex).map_err(pull_err)?;
        Ok(json!({ "stopping": true }))
    }
    async fn rotate_photos(&self, who: &Principal, paths: Vec<String>, turn: i8) -> Result<Value, ApiError> {
        let (mut rotated, mut failed) = (0, Vec::new());
        for p in paths {
            match crate::rotate::rotate_entry(&self.dav, &who.hex, &p, turn).await {
                Ok(()) => rotated += 1,
                Err(error) => failed.push(json!({ "path": p, "error": error })),
            }
        }
        Ok(json!({ "rotated": rotated, "failed": failed }))
    }
    async fn public_move(&self, who: &Principal, paths: Vec<String>, publish: bool) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        if paths.is_empty() {
            return Err(ApiError::bad_request("aucun fichier choisi"));
        }
        let out = site.set_public(&who.hex, &paths, publish).await;
        let n = out["moved"].as_array().map_or(0, Vec::len);
        if n > 0 {
            self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "system", "public", format!("{n} élément(s) {} Public", if publish { "publié(s) dans" } else { "retiré(s) de" }), None);
        }
        Ok(out)
    }
    async fn public_images(&self, who: &Principal) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        Ok(site.images(&who.hex).await)
    }
    async fn friends(&self, who: &Principal) -> Result<Value, ApiError> {
        let f = self.friends_fn.as_ref().ok_or_else(|| ApiError::unavailable("envoi à des proches non activé"))?;
        f(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : proches : {e}");
            ApiError::internal("liste des proches indisponible")
        })
    }
    async fn backup_status(&self, who: &Principal) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.status)(who.hex.clone()).await.map_err(|e| {
            eprintln!("interface : sauvegarde privée : {e}");
            ApiError::internal("état de la sauvegarde indisponible")
        })
    }
    async fn backup_rescue(&self, who: &Principal, owner: String, accept: bool) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.rescue_answer)(who.hex.clone(), owner, accept).await.map_err(ApiError::bad_request)
    }
    async fn backup_set(&self, who: &Principal, enabled: bool, host: Option<String>) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        (b.set)(who.hex.clone(), enabled, host).await.map_err(ApiError::bad_request)
    }
    async fn backup_respond(&self, who: &Principal, owner: String, accept: bool, quota_bytes: u64) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("sauvegarde privée non activée"))?;
        // Volume maximal : ce qui reste du quota de l'hébergeur, sauvegarde déjà hébergée comprise dans son occupation.
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &who.hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let quota = self.dav.config.limits_for(&account).quota_total;
        let hex = who.hex.clone();
        let used = self.services.db.readers().read(move |c| astro_dav::vfs::used_bytes(c, &hex).map_err(|_| astro_db::DbError::Aborted)).await.map_err(db_err)?;
        (b.respond)(who.hex.clone(), owner, accept, quota_bytes, quota.saturating_sub(used)).await.map_err(ApiError::bad_request)
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
}
