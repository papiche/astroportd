//! Station (Capitaine) : cockpit, domaine, énergie, réglages, constellations (plans).
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « station ».
pub struct StationBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    /// Ollama (avec `--ollama`).
    pub(super) ollama: Option<Arc<OllamaClient>>,
    /// FaceCloud est actif.
    pub(super) faces_enabled: bool,
    pub(super) admin: Option<AdminFn>,
    /// `station.conf` (domaine réglé depuis le cockpit).
    pub(super) conf_path: Option<std::path::PathBuf>,
    pub(super) probe: Mutex<Option<(Instant, Result<(), String>)>>,
    pub(super) planes_home: Option<std::path::PathBuf>,
    pub(super) unit_exe: Option<std::path::PathBuf>,
    /// Visio : port STUN de la station (UDP), secret du relais d'appel s'il est actif, serveurs supplémentaires.
    pub(super) ice: (Option<u16>, Option<[u8; 32]>, Vec<String>),
}

impl_auth!(StationBackend);

impl<B: BlobStore> StationBackend<B> {
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

    /// Port TCP du maillage (le premier écouté hors circuit de relais).
    fn p2p_port(&self) -> Option<u16> {
        let addrs = self.services.p2p.as_ref()?.listen_addrs();
        addrs.iter().map(ToString::to_string).filter(|a| !a.contains("/p2p-circuit")).find_map(|a| a.split("/tcp/").nth(1)?.split('/').next()?.parse().ok())
    }

    fn planes_home(&self) -> Result<&std::path::Path, ApiError> {
        self.planes_home.as_deref().ok_or_else(|| ApiError::unavailable("configuration des plans non disponible sur cette station"))
    }

    fn unit_exe(&self) -> std::path::PathBuf {
        self.unit_exe.clone().unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("astroportd")))
    }

    /// Port UDP de la visio dans la configuration donnée (`None` : visio coupée, ou pas de domaine).
    fn visio_port(&self, proxy: bool, has_domain: bool) -> Option<u16> {
        self.ice.0?;
        has_domain.then_some(if proxy { crate::station_domain::UI_PORT } else { 443 })
    }

    /// Comment l'interface est servie maintenant : `https` (certificat de la station), `proxy` (reverse proxy) ou `local`.
    fn web_mode(&self) -> &'static str {
        match (self.services.tls_status().is_some(), self.services.info.domain.is_some()) {
            (true, _) => "https",
            (false, true) => "proxy",
            (false, false) => "local",
        }
    }
}

impl<B: BlobStore> StationApi for StationBackend<B> {
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
        let (balance, weeks) = match svc.balance_quick(&who.hex).await {
            Ok((b, stale)) => (json!({ "g1_cents": b.g1_cents, "zen_cents": b.zen_cents, "error": null, "stale": stale }), (svc.weekly_rent_cents > 0).then(|| b.zen_cents as f64 / svc.weekly_rent_cents as f64)),
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
    async fn planes(&self, _who: &Principal) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (file, existed) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let info = plane::describe(&file, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let multi = !plane::running().is_empty();
        let pending: Vec<&str> = info.iter().filter(|p| !p.running).map(|p| p.name.as_str()).collect();
        // Démarrage automatique : une unité qui lance encore `serve` ne servira que ORIGIN, même avec plusieurs plans configurés.
        let unit = crate::systemd_unit::unit_path(home);
        let state = crate::systemd_unit::state_of(&unit);
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("astroportd"));
        let service = json!({
            "unit": state.as_str(), "path": unit.display().to_string(), "command": crate::systemd_unit::planes_exec(&exe, None),
            "mismatch": info.len() > 1 && state == crate::systemd_unit::UnitState::Serve,
            "restart": crate::station_domain::restart_available(),
        });
        Ok(json!({ "configured": existed, "multi_process": multi, "front": plane::front(), "planes": info, "restart_needed": !pending.is_empty(), "pending": pending, "path": path.display().to_string(), "service": service }))
    }
    async fn plane_add(&self, _who: &Principal, name: String, swarm_key: Option<String>) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (mut file, _) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        let added = plane::add(&mut file, &plane::NewPlane { name: name.clone(), swarm_key, ..plane::NewPlane::default() }, home).map_err(|e| ApiError::bad_request(e.to_string()))?;
        plane::save(&path, &file).map_err(|e| ApiError::internal(format!("planes.json non écrit : {e}")))?;
        let node_game = file.game_dir.as_deref().map_or_else(|| home.join(".zen/game"), |g| g.strip_prefix("~/").map_or_else(|| std::path::PathBuf::from(g), |r| home.join(r)));
        if let Ok(planes) = plane::plan(&file, home) {
            if let Some(p) = planes.iter().find(|p| p.name == name.trim()) {
                plane::prepare(p, &node_game).map_err(|e| ApiError::internal(format!("dossier du plan : {e}")))?;
            }
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:planes", format!("plan « {} » ajouté (redémarrage de `astroportd planes` nécessaire)", name.trim()), None);
        // Comme `plane-setup add` : une unité qui lance encore `serve` passe à `planes`, sinon le nouveau plan ne démarrerait jamais.
        // Un échec ici n'annule pas l'ajout : il est dit, et le bouton de l'écran permet de réessayer.
        let service = match crate::systemd_unit::apply(home, &self.unit_exe(), &path, &mut file, unix_now()) {
            Ok(r) => r.to_json(),
            Err(e) => json!({ "state": "error", "error": e }),
        };
        Ok(json!({ "name": name.trim(), "http": added.http_listen, "p2p": added.p2p_listen, "swarm_key_path": added.swarm_key_path.display().to_string(), "created_key": added.created_key, "service": service }))
    }
    async fn plane_service(&self, _who: &Principal) -> Result<Value, ApiError> {
        let home = self.planes_home()?;
        let path = crate::plane::default_path(home);
        let (mut file, existed) = crate::plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        if !existed {
            return Err(ApiError::bad_request("aucune constellation supplémentaire n'est configurée : rien à basculer"));
        }
        let rep = crate::systemd_unit::apply(home, &self.unit_exe(), &path, &mut file, unix_now()).map_err(ApiError::internal)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:planes", format!("démarrage automatique : {}", rep.state), None);
        Ok(rep.to_json())
    }
    async fn plane_remove(&self, _who: &Principal, name: String) -> Result<Value, ApiError> {
        use crate::plane;
        let home = self.planes_home()?;
        let path = plane::default_path(home);
        let (mut file, existed) = plane::load(&path, home).map_err(|e| ApiError::internal(e.to_string()))?;
        if !existed {
            return Err(ApiError::bad_request("aucun planes.json : rien à retirer"));
        }
        plane::remove(&mut file, name.trim()).map_err(|e| ApiError::bad_request(e.to_string()))?;
        plane::save(&path, &file).map_err(|e| ApiError::internal(format!("planes.json non écrit : {e}")))?;
        Ok(json!({ "removed": name.trim() }))
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
    async fn domain_info(&self, _who: &Principal) -> Result<Value, ApiError> {
        use crate::station_domain as sd;
        let conf = self.conf_path.as_ref().ok_or_else(|| ApiError::unavailable("station.conf inconnu : réglage du domaine indisponible"))?;
        let saved = crate::init::read_conf(conf).into_iter().find(|(k, _)| k == "domain").map(|(_, v)| v).filter(|v| !v.is_empty());
        let saved_proxy = sd::proxy_from_conf(conf);
        let running = self.services.info.domain.clone();
        let mode = self.web_mode();
        // Les gestes réseau de la configuration enregistrée (celle qui vaudra après redémarrage), sinon de l'actuelle.
        let proxy = if saved.is_some() { saved_proxy } else { mode == "proxy" };
        let domain = saved.clone().or_else(|| running.clone());
        Ok(json!({
            "domain": running, "saved": saved, "mode": mode, "proxy": proxy,
            "pending": saved.is_some() && (saved != running || (mode != "local" && proxy != (mode == "proxy"))),
            "steps": sd::network_steps(proxy, domain.as_deref(), self.p2p_port(), self.visio_port(proxy, domain.is_some())),
            "restart": sd::restart_available(), "restart_command": "systemctl --user restart astroportd",
        }))
    }
    async fn domain_set(&self, _who: &Principal, domain: String, proxy: Option<bool>) -> Result<Value, ApiError> {
        use crate::station_domain as sd;
        let conf = self.conf_path.clone().ok_or_else(|| ApiError::unavailable("station.conf inconnu : réglage du domaine indisponible"))?;
        let domain = sd::normalize_domain(&domain).map_err(ApiError::bad_request)?;
        // Le DNS d'abord : sans lui, ni la station ni un proxy n'obtiennent de certificat.
        let lookup = tokio::time::timeout(Duration::from_secs(8), tokio::net::lookup_host((domain.as_str(), 443))).await;
        let ips: Vec<String> = match lookup {
            Ok(Ok(addrs)) => {
                let mut v: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                v.sort();
                v.dedup();
                v
            }
            _ => Vec::new(),
        };
        if ips.is_empty() {
            return Err(ApiError::bad_request(format!("{domain} ne pointe encore nulle part : chez votre fournisseur de domaine, créez un enregistrement A vers l'adresse publique de votre box, puis réessayez (la propagation prend parfois une heure)")));
        }
        let mut warnings = Vec::new();
        let mode = self.web_mode();
        let proxy = match proxy {
            Some(p) => p,
            None if mode == "https" => false,
            None if mode == "proxy" => true,
            None => {
                // Station en HTTP local : qui tient les ports 80/443 ici, et qui répond au nom de domaine ?
                let (p80, p443) = (sd::port_state(80), sd::port_state(443));
                if p80 == "busy" || p443 == "busy" {
                    warnings.push("les ports 80/443 de cette machine sont tenus par un autre programme : un reverse proxy (Nginx Proxy Manager…), qui servira ce domaine".to_owned());
                    true
                } else if p80 == "free" {
                    let d = domain.clone();
                    match tokio::task::spawn_blocking(move || sd::who_answers(&d)).await.unwrap_or(sd::Answer::Unknown) {
                        sd::Answer::Other(server) => {
                            warnings.push(format!("http://{domain}/ arrive sur un autre serveur web ({server}), pas sur cette machine : un reverse proxy d'une autre machine sert ce domaine"));
                            true
                        }
                        _ => sd::proxy_from_conf(&conf),
                    }
                } else {
                    sd::proxy_from_conf(&conf)
                }
            }
        };
        if !proxy && mode != "https" {
            // La station prendra elle-même les ports 80 et 443 au redémarrage.
            let (p80, p443) = (sd::port_state(80), sd::port_state(443));
            if p80 == "busy" || p443 == "busy" {
                return Err(ApiError::conflict("les ports 80/443 de cette machine sont déjà pris (Nginx Proxy Manager ?) : cochez « Un reverse proxy sert ce domaine »"));
            }
            if p80 == "privileged" || p443 == "privileged" {
                let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "astroportd".into());
                warnings.push(format!("la station n'a pas le droit d'ouvrir les ports 80 et 443 : sur cette machine, lancez une fois « sudo setcap 'cap_net_bind_service=+ep' {exe} » avant de redémarrer"));
            }
        }
        sd::set_conf_value(&conf, "domain", &domain).map_err(|e| ApiError::internal(format!("{} : {e}", conf.display())))?;
        sd::set_conf_value(&conf, sd::WEB_KEY, if proxy { "proxy" } else { "" }).map_err(|e| ApiError::internal(format!("{} : {e}", conf.display())))?;
        Ok(json!({
            "domain": domain, "ips": ips, "proxy": proxy, "warnings": warnings,
            "steps": sd::network_steps(proxy, Some(&domain), self.p2p_port(), self.visio_port(proxy, true)),
            "restart": sd::restart_available(), "restart_command": "systemctl --user restart astroportd", "url": format!("https://{domain}/"),
        }))
    }
    async fn restart(&self, _who: &Principal) -> Result<Value, ApiError> {
        if !crate::station_domain::restart_available() {
            return Err(ApiError::unavailable("la station ne tourne pas sous systemd : relancez-la vous-même (systemctl --user restart astroportd, ou la commande de démarrage)"));
        }
        // Une unité réécrite (par `init --systemd` ou `plane-setup add`) n'est prise en compte qu'après `daemon-reload` : sans lui,
        // systemd relancerait l'ancienne commande.
        let reload = tokio::time::timeout(Duration::from_secs(5), tokio::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status()).await;
        if !matches!(reload, Ok(Ok(st)) if st.success()) {
            self.services.activity.publish_to(astro_core::Audience::Captain, "warn", "station", "systemctl --user daemon-reload a échoué : si l'unité vient d'être modifiée, lancez-le vous-même avant de redémarrer".to_owned(), None);
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "info", "station", "redémarrage demandé par le Capitaine".to_owned(), None);
        crate::station_domain::request_restart(Duration::from_millis(500));
        Ok(json!({ "restarting": true }))
    }
    async fn paf(&self, _who: &Principal) -> Result<Value, ApiError> {
        let conf = self.conf_path.as_deref().ok_or_else(|| ApiError::unavailable("dossier de la station inconnu"))?;
        let home = conf.parent().ok_or_else(|| ApiError::unavailable("dossier de la station inconnu"))?;
        let now = u64::try_from(crate::private_backup::now()).unwrap_or(0);
        let (measured, solar) = match crate::energy::read(home, now) {
            crate::energy::Energy::Fresh(r) | crate::energy::Energy::Stale { reading: r, .. } => {
                let solar = r.consumption.as_ref().zip(r.production.as_ref()).map(|(c, p)| crate::paf::Solar { production_wh: p.month_wh, consumption_wh: c.month_kwh * 1000.0 });
                (r.consumption.map(|c| c.avg_w_recent), solar)
            }
            _ => (None, None),
        };
        let coop = crate::coop_config::setting(&self.services.db, &self.services.uplanet_name, "PAF").await;
        Ok(crate::paf::view(conf, self.services.weekly_costs_cents, &self.services.hardware, measured, solar, coop.as_deref()))
    }
    async fn paf_set(&self, _who: &Principal, cents: u64) -> Result<Value, ApiError> {
        let conf = self.conf_path.as_deref().ok_or_else(|| ApiError::unavailable("dossier de la station inconnu"))?;
        crate::paf::save(conf, cents).map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "info", "station", format!("PAF de la machine : {} Ẑ par semaine (prise en compte au redémarrage)", crate::paf::text_of(cents)), None);
        self.paf(_who).await
    }
    async fn energy(&self, _who: &Principal) -> Result<Value, ApiError> {
        let home = self.conf_path.as_deref().and_then(std::path::Path::parent).ok_or_else(|| ApiError::unavailable("dossier de la station inconnu"))?;
        Ok(crate::energy::view(&crate::energy::read(home, u64::try_from(crate::private_backup::now()).unwrap_or(0))))
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
            (None, _) => Subsystem::new("duniter", Status::Disabled, "non reliée à la Ğ1"),
            (Some(_), None) => Subsystem::new("duniter", Status::Ok, "configuré, pas encore sollicité"),
            (Some(_), Some((age, true))) => Subsystem::new("duniter", Status::Ok, format!("dernière lecture réussie il y a {} s", age.as_secs())),
            (Some(_), Some((age, false))) => Subsystem::new("duniter", Status::Degraded, format!("dernière lecture en échec il y a {} s", age.as_secs())),
        });
        out.push(match self.ollama_probe().await {
            None => Subsystem::new("bro", Status::Disabled, "mémoire désactivée : pas de moteur d'IA (Ollama)"),
            Some(Ok(())) => Subsystem::new("bro", Status::Ok, "Ollama répond"),
            Some(Err(e)) => Subsystem::new("bro", Status::Degraded, e),
        });
        out.push(if self.faces_enabled { Subsystem::new("facecloud", Status::Ok, "actif") } else { Subsystem::new("facecloud", Status::Disabled, "non activé") });
        out.push(match (svc.tls_status(), crate::station_domain::refused()) {
            (None, Some(why)) => Subsystem::new("https", Status::Down, why.to_owned()),
            (tls, _) => tls_subsystem(tls, now_secs()),
        });
        out
    }
}
