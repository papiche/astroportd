//! FaceCloud : visages, dossiers, attributions, partages de visages en attente.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « faces ».
pub struct FacesBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    /// FaceCloud est actif.
    pub(super) faces_enabled: bool,
    pub(super) thumbs_fn: Option<ThumbFn>,
    pub(super) pending_fn: Option<PendingShareFn>,
    pub(super) contacts: Option<Arc<crate::identity_links::IdentityLinks<B>>>,
    pub(super) board: astro_facecloud::board::BoardCache,
    pub(super) book: Option<Arc<crate::address_book::AddressBook<B>>>,
}

impl_auth!(FacesBackend);

impl<B: BlobStore> FacesApi for FacesBackend<B> {
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
                     WHERE owner_hex = ?1 AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL AND name NOT LIKE 'Inconnu\\_%' ESCAPE '\\' GROUP BY coalesce('fiche:' || contact_uid, 'nom:' || name) ORDER BY COUNT(DISTINCT cid) DESC, name LIMIT 200",
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
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
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
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // Même chemin que la table : la fiche de la personne d'abord, le visage s'y relie par son UID.
        let r = self.faces_assign(who, vec![face_id], AssignTarget { name, pubkey: None, contact: None, new: false }).await?;
        if r["assigned"] == 0 {
            return Err(ApiError::not_found("visage introuvable"));
        }
        Ok(json!({ "named": r["name"] }))
    }
    async fn faces_board(&self, who: &Principal, dir: Option<String>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "enabled": false, "faces": [], "people": [], "unknown": 0 }));
        }
        let b = self.board.board(&self.services.db, &who.hex, dir.as_deref()).await.map_err(db_err)?;
        let faces: Vec<Value> = b
            .faces
            .iter()
            .map(|f| {
                json!({
                    "id": f.face_id, "name": f.name, "pubkey": f.pubkey, "contact": f.contact, "named": f.named, "path": f.path, "bbox": f.bbox,
                    "x": (f64::from(f.x) * 10_000.0).round() / 10_000.0, "y": (f64::from(f.y) * 10_000.0).round() / 10_000.0,
                    "group_id": f.group.as_ref().map(|g| g.0.clone()), "group_size": f.group.as_ref().map(|g| g.1),
                    "suggest": f.suggest.as_ref().map(|(name, score)| json!({ "name": name, "score": score, "contact": f.suggest_contact })),
                })
            })
            .collect();
        let people: Vec<Value> = b.people.iter().map(|p| json!({ "name": p.name, "contact": p.contact, "pubkey": p.pubkey, "faces": p.faces, "photos": p.photos })).collect();
        Ok(json!({ "enabled": true, "dav": self.dav.config.mount, "faces": faces, "people": people, "unknown": b.unknown, "groups_skipped": b.groups_skipped, "truncated": b.truncated }))
    }
    async fn faces_thumbs(&self, who: &Principal, ids: Vec<i64>, size: u32) -> Result<Value, ApiError> {
        let f = self.thumbs_fn.as_ref().ok_or_else(|| ApiError::unavailable("Les vignettes de visages ne sont pas disponibles : la reconnaissance des visages n'est pas activée."))?;
        let (thumbs, failed) = f(who.hex.clone(), ids, size).await.map_err(|e| {
            eprintln!("interface : vignettes de visages : {e}");
            ApiError::internal("vignettes indisponibles")
        })?;
        let map: serde_json::Map<String, Value> = thumbs.into_iter().map(|(id, url)| (id.to_string(), Value::String(url))).collect();
        Ok(json!({ "thumbs": map, "failed": failed.into_iter().map(|(id, why)| json!([id, why])).collect::<Vec<_>>() }))
    }
    async fn faces_assign(&self, who: &Principal, ids: Vec<i64>, target: AssignTarget) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // La fiche d'abord : son UID relie les visages (jamais le nom) et son nom fait foi. Un nom ambigu est refusé avec le
        // nombre de fiches en cause : l'interface fait choisir, elle ne devine pas.
        let contact = match &self.book {
            Some(book) => Some(book.person_card(&who.hex, &target.name, target.contact.as_deref(), target.new).await.map_err(book_error)?),
            None => None,
        };
        let uid = contact.as_ref().and_then(|c| c["uid"].as_str()).map(str::to_owned);
        let name = contact.as_ref().and_then(|c| c["name"].as_str()).map_or_else(|| target.name.clone(), str::to_owned);
        match astro_facecloud::review::name_faces(&self.services.db, &who.hex, &ids, uid.as_deref(), target.pubkey.as_deref(), &name).await {
            Ok(changed) => {
                self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{changed} visage(s) rangé(s) dans « {name} »"), None);
                // La fiche reliée donne aussitôt sa clé Nostr aux visages (et partage leurs photos, amis réciproques).
                if let Some(links) = &self.contacts {
                    let (links, owner) = (links.clone(), who.hex.clone());
                    tokio::spawn(async move {
                        if let Err(e) = links.reconcile(&owner).await {
                            eprintln!("annuaire : rapprochement impossible : {e:?}");
                        }
                    });
                }
                Ok(json!({ "assigned": changed, "name": name, "contact": contact }))
            }
            Err(astro_facecloud::FaceError::Invalid(m)) => Err(ApiError::bad_request(m)),
            Err(e) => Err(db_err(e)),
        }
    }
    async fn face_shares_pending(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        let batches = astro_facecloud::review::pending_batches(&self.services.db, &who.hex).await.map_err(db_err)?;
        let pending: Vec<Value> = batches.into_iter().map(|b| json!({ "recipient_hex": b.recipient_hex, "name": b.name, "count": b.count, "sample_path": b.sample_path })).collect();
        Ok(json!({ "pending": pending }))
    }
    async fn face_shares_confirm(&self, who: &Principal, recipient: String, accept: bool) -> Result<Value, ApiError> {
        let f = self.pending_fn.as_ref().ok_or_else(|| ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station."))?;
        let (sent, dropped) = f(who.hex.clone(), recipient, accept).await.map_err(|e| {
            eprintln!("interface : envois de photos en attente : {e}");
            ApiError::internal("envoi impossible, réessayez plus tard")
        })?;
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", if accept { format!("{sent} photo(s) partagée(s) après confirmation") } else { "partage de photos refusé".to_owned() }, None);
        Ok(json!({ "sent": sent, "dropped": dropped }))
    }
    async fn faces_forget(&self, who: &Principal, ids: Vec<i64>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
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
    async fn faces_folders(&self, who: &Principal) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Ok(json!({ "folders": [] }));
        }
        let owner = who.hex.clone();
        let rows: Vec<(String, i64, i64)> = self
            .services
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT e.parent, COUNT(*), COALESCE(SUM(f.name LIKE 'Inconnu%' AND f.pubkey IS NULL), 0) FROM faces f \
                     JOIN ucloud_entries e ON e.owner_hex = f.owner_hex AND e.path = f.ucloud_path \
                     WHERE f.owner_hex = ?1 AND f.ref IS NULL AND f.bbox_x1 IS NOT NULL GROUP BY e.parent ORDER BY e.parent DESC LIMIT 2000",
                )?;
                Ok(stmt.query_map([&owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "folders": rows.into_iter().map(|(dir, faces, todo)| json!({ "dir": dir, "faces": faces, "todo": todo })).collect::<Vec<_>>() }))
    }
    async fn faces_delete_photos(&self, who: &Principal, ids: Vec<i64>) -> Result<Value, ApiError> {
        if !self.faces_enabled {
            return Err(ApiError::unavailable("La reconnaissance des visages n'est pas activée sur cette station (le Capitaine l'active dans Options)."));
        }
        // Les photos où apparaissent ces visages (les visages de référence des contacts n'ont pas de photo à nous).
        let (owner, wanted) = (who.hex.clone(), ids);
        let paths: Vec<String> = self
            .services
            .db
            .readers()
            .read(move |c| {
                let mut out = std::collections::BTreeSet::new();
                let mut stmt = c.prepare("SELECT ucloud_path FROM faces WHERE owner_hex = ?1 AND face_id = ?2 AND ref IS NULL AND ucloud_path IS NOT NULL")?;
                for id in wanted {
                    if let Ok(p) = stmt.query_row(rusqlite::params![owner, id], |r| r.get::<_, String>(0)) {
                        out.insert(p);
                    }
                }
                Ok(out.into_iter().collect())
            })
            .await
            .map_err(db_err)?;
        let (mut deleted, mut missing, mut bytes) = (0u64, 0u64, 0u64);
        for chunk in paths.chunks(200) {
            let (owner, chunk) = (who.hex.clone(), chunk.to_vec());
            let done = astro_dav::vfs::vfs_tx(&self.dav.db, move |tx| {
                let mut out = Vec::new();
                for p in chunk {
                    let size: i64 = tx.query_row("SELECT COALESCE(size, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", rusqlite::params![owner, p], |r| r.get(0)).unwrap_or(0);
                    match astro_dav::vfs::delete_tree_tx(tx, &owner, &p) {
                        Ok(cids) => out.push((cids, u64::try_from(size).unwrap_or(0), true)),
                        Err(astro_dav::vfs::VfsError::NotFound) => out.push((Vec::new(), 0, false)),
                        Err(e) => return Err(e),
                    }
                }
                Ok(out)
            })
            .await
            .map_err(|e| ApiError::internal(format!("suppression impossible : {e}")))?;
            for (cids, size, found) in done {
                if found {
                    deleted += 1;
                    bytes += size;
                } else {
                    missing += 1;
                }
                astro_dav::server::release_blobs(&self.dav, cids).await;
            }
        }
        if let Some(p) = &self.dav.publisher {
            p.notify(&who.hex);
        }
        self.services.activity.publish_to(astro_core::Audience::Owner(who.hex.clone()), "facecloud", "ui:faces", format!("{deleted} photo(s) supprimée(s) ({} Mo)", bytes >> 20), None);
        Ok(json!({ "deleted": deleted, "missing": missing, "bytes": bytes }))
    }
}
