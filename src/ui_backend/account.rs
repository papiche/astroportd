//! Compte : profil, site, clés, jetons, appareils, retour d'usage.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « account ».
pub struct AccountBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    pub(super) billet: Option<BilletFn>,
    pub(super) site: Option<Arc<crate::public_site::Site<B>>>,
    pub(super) account: Option<Arc<crate::account::AccountService>>,
    pub(super) registrar: Option<Arc<crate::register::Registrar>>,
    pub(super) feedback: Option<Arc<crate::feedback::Feedback>>,
    pub(super) ai: Option<Arc<crate::member_ai::MemberAi>>,
}

impl_auth!(AccountBackend);

impl<B: BlobStore> AccountBackend<B> {
    fn account_service(&self) -> Result<&Arc<crate::account::AccountService>, ApiError> {
        self.account.as_ref().ok_or_else(|| ApiError::unavailable("Mon compte n'est pas disponible sur cette station"))
    }

    /// Écran MULTIPASS : redevances courantes d'usage (**Opex**) distinctes des parts d'investissement matériel
    /// (**Capex**, ZenCard). Les parts de matériel sont comptées à part des loyers.
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
                    "SELECT COALESCE(SUM(CASE WHEN at >= ?2 THEN amount_cents END), 0), COALESCE(SUM(amount_cents), 0) FROM rents WHERE owner_hex = ?1",
                    rusqlite::params![owner, since],
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
                    "weekly_rent_cents": weekly_rent, "rent_28d_cents": rent_28, "rent_total_cents": rent_total, "total_28d_cents": rent_28,
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

impl<B: BlobStore> AccountApi for AccountBackend<B> {
    async fn feedback(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        use crate::feedback::FeedbackError as E;
        let f = self.feedback.as_ref().ok_or_else(|| ApiError::unavailable("retours non activés sur cette station"))?;
        f.submit(&who.hex, &body).await.map_err(|e| match e {
            E::Invalid(m) => ApiError::bad_request(m),
            E::Limited(m) => ApiError::too_many(m).with_code("rate_limited"),
            E::Internal(m) => {
                eprintln!("interface : retour : {m}");
                ApiError::internal("retour non enregistré : voir les journaux de la station")
            }
        })
    }
    async fn devices_list(&self, who: &Principal) -> Result<Value, ApiError> {
        let list = astro_dav::devices::list(&self.services.db, &who.hex).await.map_err(device_err)?;
        Ok(json!({ "devices": list.iter().map(|d| json!({ "id": d.id, "label": d.label, "root": d.root, "mode": d.mode.as_str(), "created_at": d.created_at, "last_used": d.last_used })).collect::<Vec<_>>(), "max": astro_dav::devices::MAX_DEVICES }))
    }
    async fn devices_add(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let mode = astro_dav::devices::Mode::parse(&text("mode")).ok_or_else(|| ApiError::bad_request("mode attendu : write, readwrite ou read"))?;
        let (d, token) = astro_dav::devices::create(&self.services.db, &who.hex, &text("label"), &text("root"), mode).await.map_err(device_err)?;
        self.dav.publisher.as_ref().inspect(|p| p.notify(&who.hex));
        Ok(json!({ "id": d.id, "label": d.label, "root": d.root, "mode": d.mode.as_str(), "token": token, "note": "Ce jeton ne sera plus jamais affiché : copiez-le maintenant dans l'application de l'appareil." }))
    }
    async fn devices_revoke(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        astro_dav::devices::revoke(&self.services.db, &who.hex, &id).await.map_err(device_err)?;
        Ok(json!({ "revoked": id }))
    }
    async fn footprint(&self, who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let account = astro_dav::auth::lookup_by_hex(&svc.db, &who.hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let limits = self.dav.config.limits_for(&account);
        let unlimited = limits.quota_total == u64::MAX;
        // Sans quota (Capitaine), la mesure utile est le disque de la station.
        let quota = if unlimited { self.dav.blobs.disk_space().map_or(0, |(_, total)| total) } else { limits.quota_total };
        let owner = who.hex.clone();
        let (opex, capex) = self.opex_capex(&who.hex, &account).await?;
        let hosting = {
            let h = who.hex.clone();
            svc.db.readers().read(move |c| Ok(json!({ "weekly_zen": crate::hosting_fee::weekly_price(c, &h)?, "lifecycle": crate::hosting_fee::lifecycle_summary(c, &h) }))).await.map_err(db_err)?
        };
        let (used, files) = svc
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT COALESCE(SUM(size), 0) + COALESCE((SELECT SUM(size) FROM public_blobs WHERE owner = ?1), 0), COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [&owner], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?)
            })
            .await
            .map_err(db_err)?;
        Ok(json!({
            "disk": { "used_bytes": used, "files": files, "quota_bytes": quota, "unlimited": unlimited, "tier": format!("{:?}", account.tier) },
            "opex": opex, "capex": capex,
            "hosting": hosting,
        }))
    }
    async fn account(&self, who: &Principal) -> Result<Value, ApiError> {
        self.account_service()?.view(&who.hex).await.map_err(account_error)
    }
    async fn account_site(&self, who: &Principal) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'adresse : pas de site public"))?;
        Ok(site.info(&who.hex, email).await)
    }
    async fn account_site_example(&self, who: &Principal, force: bool) -> Result<Value, ApiError> {
        let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
        let written = site.install_example(&who.hex, force).await.map_err(|e| if e.contains("existe déjà") { ApiError::conflict(e) } else { ApiError::internal(e) })?;
        Ok(json!({ "written": written }))
    }
    async fn account_image(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        use base64::Engine as _;
        let svc = self.account_service()?;
        let kind = body["kind"].as_str().unwrap_or("");
        // Une image déjà publiée dans /Public : la station la lit elle-même (aucun autre dossier de l'uCloud n'est accepté).
        let from_public;
        let data = match body["public_path"].as_str() {
            Some(path) => {
                let site = self.site.as_ref().ok_or_else(|| ApiError::unavailable("site public non activé sur cette station"))?;
                from_public = base64::engine::general_purpose::STANDARD.encode(site.read_public(&who.hex, path).await.map_err(ApiError::bad_request)?);
                from_public.as_str()
            }
            None => body["data"].as_str().unwrap_or(""),
        };
        svc.set_image(&who.hex, kind, data, body["origin"].as_str()).await.map_err(account_error)
    }
    async fn public_images(&self, who: &Principal) -> Result<Value, ApiError> {
        let (db, hex) = (self.services.db.clone(), who.hex.clone());
        let account = astro_dav::auth::lookup_by_hex(&db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        let quota = self.dav.config.limits_for(&account).quota_total;
        let h = hex.clone();
        let (rows, used, profile) = db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT cid, mime, size, added_at FROM public_blobs WHERE owner = ?1 ORDER BY added_at DESC, cid LIMIT 500")?;
                let rows = st.query_map([&h], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let used = astro_dav::vfs::used_bytes(c, &h).map_err(|_| astro_db::DbError::Aborted)?;
                let profile: Option<String> = rusqlite::OptionalExtension::optional(c.query_row("SELECT content FROM nostr_events WHERE kind = 0 AND pubkey = ?1 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get(0)))?;
                Ok((rows, used, profile))
            })
            .await
            .map_err(db_err)?;
        let profile: Value = profile.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or(Value::Null);
        let in_use = |cid: &str| -> Option<&'static str> {
            [("picture", "picture_cid"), ("banner", "banner_cid")].into_iter().find(|(url, cidf)| profile[*cidf].as_str() == Some(cid) || profile[*url].as_str().and_then(crate::profile_image::cid_of) == Some(cid)).map(|(k, _)| k)
        };
        let images: Vec<Value> = rows.iter().map(|(cid, mime, size, at)| json!({ "cid": cid, "mime": mime, "size": size, "added_at": at, "in_use": in_use(cid) })).collect();
        Ok(json!({ "images": images, "used": used, "quota": quota }))
    }
    async fn public_image_delete(&self, who: &Principal, cid: String) -> Result<Value, ApiError> {
        if !astro_dav::blob::is_cid(&cid) {
            return Err(ApiError::bad_request("image inconnue"));
        }
        let listing = self.public_images(who).await?;
        let Some(image) = listing["images"].as_array().and_then(|a| a.iter().find(|i| i["cid"] == cid.as_str())) else { return Err(ApiError::not_found("image inconnue")) };
        if let Some(role) = image["in_use"].as_str() {
            return Err(ApiError::bad_request(if role == "banner" { "cette image est votre bandeau : changez d'abord de bandeau" } else { "cette image est votre photo de profil : changez d'abord de photo" }));
        }
        // La ligne d'abord (le quota est libéré et la passerelle cesse de la servir), puis les octets.
        let done = self.services.db.writer().execute("DELETE FROM public_blobs WHERE cid = ?1 AND owner = ?2", vec![cid.clone().into(), who.hex.clone().into()]).await.map_err(db_err)?;
        if done.changes == 0 {
            return Err(ApiError::not_found("image inconnue"));
        }
        if let Err(e) = self.dav.blobs.remove(&cid).await {
            eprintln!("interface : image publique retirée mais octets non supprimés : {e}");
        }
        Ok(json!({ "removed": cid }))
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
    async fn account_position(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let pos = match (body["lat"].as_f64(), body["lon"].as_f64()) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            (None, None) if body.get("lat").is_none() => None,
            _ => return Err(ApiError::bad_request("position attendue : {lat, lon} en degrés")),
        };
        let umap = r.set_position(&who.hex, pos).await.map_err(register_error)?;
        Ok(json!({ "umap": umap }))
    }
    async fn account_eject(&self, _who: &Principal, hex: String) -> Result<Value, ApiError> {
        use astro_duniter::Chain;
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let email = r.ejectable(&hex).await.map_err(register_error)?;
        // Ẑen éventuels (reçus d'un ami, d'un don…) : reversés au portefeuille primal UPLANETNAME_G1 avant l'éjection, comme
        // le « CASH-BACK » de nostr_DESTROY_TW.sh ; ils seront restitués si le membre revient. Solde illisible = pas d'éjection.
        let dunikey = r.nostr_root().join(&email).join(".secret.dunikey");
        let mut cashback = 0u64;
        if let Ok(text) = std::fs::read_to_string(&dunikey) {
            let chain = self.services.chain.clone().ok_or_else(|| ApiError::unavailable("station non reliée à la Ğ1 : impossible de vérifier le solde du compte avant de l'éjecter"))?;
            let key = astro_lifecycle::dunikey::Dunikey::parse(&text).map_err(|e| ApiError::internal(format!("portefeuille du compte illisible : {e}")))?;
            let signer = astro_duniter::Ed25519Signer::from_seed(*key.ed25519_seed());
            let balance = chain.balance(&signer.account()).await.map_err(|e| ApiError::unavailable(format!("solde du compte illisible ({e}) : éjection reportée")))?;
            if balance.total.0 > 0 {
                let master = crate::coop_wallets::Wallets::new(r.uplanet_name().to_owned(), self.services.db.clone()).master().await.map_err(ApiError::internal)?;
                let user = email.split('@').next().unwrap_or(&email);
                let memo = format!("MULTIPASS:{user}:PRIMAL:CASH-BACK");
                match chain.transfer(&signer, &master.account(), balance.total, astro_duniter::TransferMode::AllowDeath, Some(&memo)).await {
                    Ok(out) => {
                        cashback = balance.total.0;
                        let record = json!({ "hex": hex, "email": email, "from": signer.account().ss58(), "to": master.account().ss58(), "g1_cents": cashback, "block": hex::encode(out.block_hash) });
                        let _ = self.services.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('eject_cashback', ?1, ?2)", vec![now_secs().into(), record.to_string().into()]).await;
                    }
                    Err(e) => return Err(ApiError::unavailable(format!("reversement des {} Ğ1 du compte impossible ({e}) : éjection annulée, rien n'a changé", balance.total))),
                }
            }
        }
        let email = r.eject(&hex).await.map_err(register_error)?;
        if cashback > 0 {
            self.services.activity.publish_to(astro_core::Audience::Captain, "system", "register", format!("{email} : {} Ğ1 reversés au portefeuille primal (CASH-BACK) avant l'éjection", Amount(cashback)), None);
        }
        Ok(json!({ "ejected": email, "cashback_g1_cents": cashback }))
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
    async fn accounts(&self, _who: &Principal) -> Result<Value, ApiError> {
        let rows: Vec<AccountRow> = self
            .services
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT i.hex, i.role, i.email, i.contract, (SELECT value FROM kv WHERE scope = 'lifecycle' AND key = i.hex),                             COALESCE((SELECT SUM(size) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file'), 0) + COALESCE((SELECT SUM(size) FROM public_blobs p WHERE p.owner = i.hex), 0),                             (SELECT COUNT(*) FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.type = 'file')                      FROM identities i WHERE i.hosted = 1 AND i.role IN ('multipass', 'captain', 'zencard') ORDER BY i.email, i.hex",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        for (hex, role, email, contract, state, used, files) in rows {
            let (quota, unlimited) = match astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await {
                Some(a) => {
                    let q = self.dav.config.limits_for(&a).quota_total;
                    (if q == u64::MAX { 0 } else { q }, q == u64::MAX)
                }
                None => (0, false),
            };
            let life: Value = state.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
            let status = life["status"].as_str().map(str::to_owned).or_else(|| contract.clone());
            let grace_ends = (status.as_deref() == Some("grace_period")).then(|| life["status_since"].as_i64().map(|s| s + astro_lifecycle::state::GRACE_SECS as i64)).flatten();
            let pass_locked_until = match &self.account {
                Some(a) => a.pass_locked_until(&hex).await,
                None => None,
            };
            let h = hex.clone();
            let (own_quota, own_price, price, fee): (Option<i64>, Option<u64>, u64, Option<Value>) = self
                .services
                .db
                .readers()
                .read(move |c| {
                    let own_quota = c.query_row("SELECT quota_bytes FROM identities WHERE hex = ?1", [&h], |r| r.get::<_, Option<i64>>(0))?;
                    let own_price = c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![crate::hosting_fee::PRICE_SCOPE, h], |r| r.get::<_, String>(0)).ok().and_then(|v| v.trim().parse().ok());
                    let price = crate::hosting_fee::weekly_price(c, &h)?;
                    let fee = crate::hosting_fee::lifecycle_summary(c, &h);
                    Ok((own_quota, own_price, price, fee))
                })
                .await
                .map_err(db_err)?;
            let issuer: bool = self.services.db.readers().read({ let h = hex.clone(); move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT 1 FROM kv WHERE scope = 'billet_issuer' AND key = ?1", [&h], |_| Ok(())))?.is_some()) }).await.unwrap_or(false);
            let onboarding: Value = self.services.db.readers().read({ let h = hex.clone(); move |c| Ok(crate::member_primo::onboarding(c, &h)?) }).await.ok().flatten().map_or(Value::Null, |o| o.to_json(now_secs()));
            out.push(json!({
                "onboarding": onboarding, "billet_issuer": issuer, "own_quota_bytes": own_quota, "own_weekly_zen": own_price, "weekly_zen": price, "fee": fee,
                "pass_locked_until": pass_locked_until,
                "hex": hex, "role": role, "email": email, "contract": contract, "status": status, "tracked": !life.is_null(),
                "last_ok_at": life["last_ok_at"], "status_since": life["status_since"], "grace_ends_at": grace_ends,
                "used_bytes": used, "files": files, "quota_bytes": quota, "unlimited": unlimited,
            }));
        }
        Ok(json!({ "accounts": out }))
    }
    async fn account_quota(&self, who: &Principal, hex: String, quota_bytes: Option<u64>) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        if account.tier == astro_dav::auth::Tier::Captain && quota_bytes.is_some() {
            return Err(ApiError::bad_request("le Capitaine n'a pas de quota : seul le disque de la station le borne"));
        }
        let value: rusqlite::types::Value = quota_bytes.map_or(rusqlite::types::Value::Null, |q| rusqlite::types::Value::Integer(i64::try_from(q).unwrap_or(i64::MAX)));
        self.services.db.writer().execute("UPDATE identities SET quota_bytes = ?2 WHERE hex = ?1 AND hosted = 1", vec![hex.clone().into(), value]).await.map_err(db_err)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Quota de {} : {}", hex.get(..8).unwrap_or(&hex), quota_bytes.map_or_else(|| "celui de sa formule".to_owned(), |q| format!("{} Go", q >> 30))), None);
        let _ = who;
        Ok(json!({ "hex": hex, "quota_bytes": quota_bytes }))
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
    async fn billet_issuer(&self, who: &Principal) -> bool {
        match &self.billet {
            Some(f) => f(None, "is_issuer", who_body(who, Value::Null)).await.map(|v| v["issuer"] == true).unwrap_or(false),
            None => false,
        }
    }
    async fn ai_declared(&self, who: &Principal) -> bool {
        match &self.ai {
            Some(ai) => ai.declared(&who.hex).await,
            None => false,
        }
    }
}
