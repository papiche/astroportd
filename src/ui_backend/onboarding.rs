//! Accueil : invitations, liens d'accueil, inscription, restauration, annuaire des stations.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « onboarding ».
pub struct OnboardBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    pub(super) backup: Option<BackupFns>,
    pub(super) invites: Option<Arc<crate::invites::Invites<B>>>,
    /// Liens d'accueil de la station (plusieurs arrivées par lien).
    pub(super) welcome: Option<Arc<crate::welcome::Welcome<B>>>,
    pub(super) registrar: Option<Arc<crate::register::Registrar>>,
    pub(super) stations: Option<Arc<crate::beacon::Stations>>,
    /// Code d'installation d'une station sans Capitaine (page `/welcome`).
    pub(super) setup: Option<Arc<crate::first_run::SetupCode>>,
}

impl_auth!(OnboardBackend);

impl<B: BlobStore> OnboardBackend<B> {
    fn invites_service(&self) -> Result<&Arc<crate::invites::Invites<B>>, ApiError> {
        self.invites.as_ref().ok_or_else(|| ApiError::unavailable("invitations non disponibles sur cette station"))
    }

    async fn is_hosted(&self, hex: &str) -> bool {
        use rusqlite::OptionalExtension;
        let h = hex.to_owned();
        self.services.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }

    /// Clé qui a signé la requête (NIP-98, corps lié à la signature, anti-rejeu). L'URL attendue est celle que la station annonce
    /// publiquement (`public_base_url`), à défaut celle de l'en-tête `Host`.
    async fn nostr_caller(&self, headers: &HeaderMap, method: &str, uri: &Uri, body: &[u8]) -> Result<String, ApiError> {
        let value = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or_else(|| ApiError::denied("signature NIP-98 attendue"))?.to_owned();
        let base = match &self.dav.config.public_base_url {
            Some(b) => b.trim_end_matches('/').to_owned(),
            None => format!("http://{}", headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost")),
        };
        let url = format!("{base}{}", uri.path_and_query().map_or("/", |p| p.as_str()));
        let (method, body, store) = (method.to_owned(), body.to_vec(), self.dav.replay.clone());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        tokio::task::spawn_blocking(move || astro_crypto::nip98::verify_authorization(&value, &astro_crypto::nip98::Nip98Request { method: &method, url: &url, body: Some(&body) }, now, store.as_ref()))
            .await
            .map_err(|_| ApiError::internal("vérification interrompue"))?
            .map(|v| v.pubkey)
            .map_err(|e| ApiError::denied(format!("NIP-98 refusé : {e}")))
    }

    fn welcome_service(&self) -> Result<&Arc<crate::welcome::Welcome<B>>, ApiError> {
        self.welcome.as_ref().ok_or_else(|| ApiError::unavailable("liens d'accueil non disponibles sur cette station"))
    }
}

impl<B: BlobStore> OnboardApi for OnboardBackend<B> {
    async fn register_info(&self) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("inscription non disponible sur cette station"))?;
        let limits = r.limits();
        let accounts = r.hosted_accounts().await.map_err(|e| ApiError::internal(e.to_string()))?;
        let waiting = r.awaiting_captain().await;
        let open = limits.open && !waiting && (limits.max_accounts == 0 || accounts < limits.max_accounts);
        let mut out = json!({ "open": open, "constellation": r.constellation(), "accounts": accounts, "max_accounts": limits.max_accounts });
        if waiting {
            out["error"] = json!(crate::register::RegisterError::NoCaptain.to_string());
            out["captain_setup"] = json!(self.setup.is_some());
        }
        Ok(out)
    }
    async fn station_card(&self) -> Result<Value, ApiError> {
        let svc = &self.services;
        let keys = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("carte de station non disponible"))?.public_keys().await.map_err(ApiError::internal)?;
        let captain: Option<String> = svc
            .db
            .readers()
            .read(|c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND role = 'captain' ORDER BY hex LIMIT 1", [], |r| r.get::<_, String>(0)))?))
            .await
            .map_err(db_err)?;
        // Seulement ce que la station publie déjà ailleurs (balise 30850, carte de maillage) : jamais d'e-mail, de secret ni de solde.
        let mut card = json!({
            "_source": "astroportd", "version": env!("CARGO_PKG_VERSION"), "hostname": crate::init::hostname(),
            "NODEHEX": svc.info.node_hex, "UPLANETG1PUB": keys.g1pub, "UPLANETNAME_G1": keys.name_g1,
        });
        if let Some(id) = &svc.info.peer_id {
            card["ipfsnodeid"] = json!(id);
        }
        if let Some(c) = captain {
            card["captainHEX"] = json!(c);
        }
        if let Some(d) = &svc.info.domain {
            card["myRELAY"] = json!(format!("wss://{d}"));
            card["uSPOT"] = json!(format!("https://{d}"));
            if self.dav.config.ipfs_gateway {
                card["myIPFS"] = json!(format!("https://{d}"));
            }
        }
        if let Some((lat, lon)) = svc.origin {
            card["STATION_LAT"] = json!(crate::beacon::umap_coord(lat));
            card["STATION_LON"] = json!(crate::beacon::umap_coord(lon));
        }
        Ok(card)
    }
    async fn stations(&self) -> Result<Value, ApiError> {
        let s = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des stations non disponible"))?;
        s.list(u64::try_from(crate::private_backup::now()).unwrap_or(0)).await.map_err(ApiError::internal)
    }
    async fn register_captain(&self, ip: Option<std::net::IpAddr>, code: String, email: String, lang: String) -> Result<Value, ApiError> {
        let (Some(r), Some(setup)) = (self.registrar.as_ref(), self.setup.as_ref()) else { return Err(ApiError::unavailable("cette station n'attend pas de Capitaine")) };
        if !r.awaiting_captain().await {
            return Err(ApiError::conflict("cette station a déjà un Capitaine"));
        }
        if !setup.redeem(&code) {
            return Err(ApiError::forbidden("code d'installation incorrect : il est écrit dans le journal de la station (terminal ou journalctl)"));
        }
        r.register_captain(ip, &email, &lang).await.map(|reg| registered_json(&reg)).map_err(register_error)
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
        self.invites_service()?.preview(&token, false).await.map_err(invite_error)
    }
    async fn invite_preview_signed(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String) -> Result<Value, ApiError> {
        self.nostr_caller(headers, method, uri, &[]).await?;
        self.invites_service()?.preview(&token, true).await.map_err(invite_error)
    }
    async fn invite_thumb(&self, token: String, index: usize) -> Result<Vec<u8>, ApiError> {
        self.invites_service()?.thumb(&token, index).await.map_err(invite_error)
    }
    async fn invite_accept(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        let a = self.invites_service()?.accept(&token, &who.hex).await.map_err(invite_error)?;
        Ok(json!({ "from": a.from, "shared": a.shared, "warnings": a.warnings }))
    }
    async fn invite_accept_nostr(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String, body: Vec<u8>) -> Result<Value, ApiError> {
        let pubkey = self.nostr_caller(headers, method, uri, &body).await?;
        let svc = self.invites_service()?;
        let hosted = self.is_hosted(&pubkey).await;
        if hosted {
            let a = svc.accept(&token, &pubkey).await.map_err(invite_error)?;
            return Ok(json!({ "from": a.from, "shared": a.shared, "warnings": a.warnings }));
        }
        let proof: crate::invites::RemoteProof = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("preuve illisible : {e}")))?;
        svc.accept_remote(&token, &pubkey, proof).await.map_err(invite_error)
    }
    async fn invite_deliver(&self, headers: &HeaderMap, method: &str, uri: &Uri, token: String, body: Vec<u8>) -> Result<Value, ApiError> {
        let pubkey = self.nostr_caller(headers, method, uri, &body).await?;
        self.invites_service()?.deliver_remote(&token, &pubkey).await.map_err(invite_error)
    }
    async fn invite_accept_elsewhere(&self, who: &Principal, origin: String, token: String) -> Result<Value, ApiError> {
        self.invites_service()?.accept_elsewhere(&who.hex, &origin, &token).await.map_err(invite_error)
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
    async fn join_take(&self, id: String) -> Result<Value, ApiError> {
        match crate::sponsor::take(&self.services.db, &id).await.map_err(ApiError::internal)? {
            Some(sealed) => {
                self.services.activity.publish_to(astro_core::Audience::Captain, "info", "parrainage", "une station filleule a retiré son offre de parrainage".to_owned(), None);
                Ok(json!({ "sealed": sealed }))
            }
            None => Err(ApiError::not_found("offre de parrainage introuvable, déjà utilisée ou expirée : demandez un nouveau jeton à la station marraine")),
        }
    }
    async fn welcome_list(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(json!({ "links": self.welcome_service()?.list(&who.hex).await.map_err(invite_error)? }))
    }
    async fn welcome_create(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let n = |k: &str| body[k].as_u64().map(|v| u32::try_from(v).unwrap_or(u32::MAX));
        self.welcome_service()?.create(&who.hex, body["label"].as_str().unwrap_or_default(), n("max_uses"), n("days")).await.map_err(invite_error)
    }
    async fn welcome_revoke(&self, who: &Principal, token: String) -> Result<Value, ApiError> {
        self.welcome_service()?.revoke(&who.hex, &token).await.map_err(invite_error)?;
        Ok(json!({ "revoked": true }))
    }
    async fn welcome_preview(&self, token: String) -> Result<Value, ApiError> {
        self.welcome_service()?.preview(&token).await.map_err(invite_error)
    }
    async fn welcome_arrive(&self, ip: Option<std::net::IpAddr>, token: String, email: Option<String>, alias: Option<String>, lang: String) -> Result<Value, ApiError> {
        let lang = if lang.is_empty() { "fr".to_owned() } else { lang };
        let a = self.welcome_service()?.arrive(&token, ip, email.as_deref(), alias.as_deref(), &lang).await.map_err(invite_error)?;
        let mut out = registered_json(&a.registered);
        out["chat_peer"] = json!(a.host);
        if !a.warnings.is_empty() {
            out["warnings"] = json!(a.warnings);
        }
        Ok(out)
    }
    async fn restore(&self, ip: Option<std::net::IpAddr>, card: String, lang: String) -> Result<Value, ApiError> {
        let r = self.registrar.as_ref().ok_or_else(|| ApiError::unavailable("restauration non disponible sur cette station"))?;
        // Carte imprimée seule : la part qu'un gardien a rendue (secours, 24 h) la complète, sans passer par le navigateur.
        let single = astro_lifecycle::share::SecretShare::parse(card.trim()).ok().filter(|s| s.index == 1);
        let rescued = match (&single, &self.backup) {
            (Some(s), Some(b)) => (b.rescue_share)(s.hex.clone(), false).await,
            _ => None,
        };
        let text = match &rescued {
            Some(p3) => format!("{}\n{p3}", card.trim()),
            None => card,
        };
        let card = astro_lifecycle::share::Recovery::parse(&text).map_err(|e| ApiError::bad_request(e.to_string()))?;
        let lang = if lang.is_empty() { "fr".to_owned() } else { lang };
        let reg = r.recover(ip, &card, &lang).await.map_err(register_error)?;
        if let (Some(_), Some(b)) = (&rescued, &self.backup) {
            (b.rescue_share)(card.card.hex.clone(), true).await;
        }
        let mut out = registered_json(&reg);
        out["restoring"] = json!(true);
        Ok(out)
    }
    async fn restore_ask(&self, card: String, send: bool) -> Result<Value, ApiError> {
        let b = self.backup.as_ref().ok_or_else(|| ApiError::unavailable("secours par les gardiens non disponible sur cette station"))?;
        (b.rescue_ask)(card, send).await.map_err(ApiError::bad_request)
    }
}
