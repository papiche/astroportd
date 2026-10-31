//! Tâches de fond et services du démon, sortis de `run` pour que chacun se lise seul. Chaque fonction prend ce dont elle a
//! besoin dans [`Parts`] et pousse ses tâches dans `background`, arrêtées ensemble à la fermeture.

use super::*;

/// Ce que les tâches de fond partagent : la base, le bus d'activité, WebDAV, le relai et l'identité de la station.
pub(super) struct Parts<'a> {
    pub a: &'a ServeArgs,
    pub db: Arc<Db>,
    pub activity: ActivityBus,
    pub stop_watch: tokio::sync::watch::Receiver<bool>,
    pub dav: Arc<Dav>,
    pub local: Arc<AutoBlobStore>,
    pub relay: Arc<Relay>,
    pub p2p_handle: Option<astro_p2p::P2pHandle>,
    pub services_peer: Option<String>,
    pub uplanet_name: String,
    pub secrets_dir: PathBuf,
    pub nostr_dir: Option<PathBuf>,
    pub notify: Option<Notify>,
    pub node_hex: String,
    pub node_secret: Option<[u8; 32]>,
    pub node_publisher: Option<Arc<EventPublisher>>,
}

type Background = Vec<tokio::task::JoinHandle<()>>;

/// Les accès temporaires expirent, les amis changent : `authorized_keys` suit la politique toutes les 5 minutes.
pub(super) fn ssh_sync(p: &Parts<'_>, tunnel_policy: &Arc<TunnelPolicy>, background: &mut Background) {
    let stop_watch = &p.stop_watch;
    {
        let (policy, mut stop) = (tunnel_policy.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            loop {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                if let Err(e) = policy.sync_ssh(at).await {
                    eprintln!("tunnels : authorized_keys non mis à jour : {e}");
                }
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(300)) => {}
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            return;
                        }
                    }
                }
            }
        }));
    }
}

/// Rattrapage du GPS et de la date de prise de vue des photos reçues avant que la station ne les lise : 25 fichiers par
/// compte et par passage, chacun lu une seule fois ; le rythme s'accélère tant qu'il en reste.
pub(super) fn exif_backfill(p: &Parts<'_>, background: &mut Background) {
    let (dav, db, activity, stop_watch) = (&p.dav, &p.db, &p.activity, &p.stop_watch);
    {
        let (dav, db, bus, mut stop) = (dav.clone(), db.clone(), activity.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            let mut wait = Duration::from_secs(30);
            loop {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    _ = stop.changed() => return,
                }
                let owners: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass','captain','zencard')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap_or_default();
                let (mut scanned, mut found, mut remaining) = (0, 0, 0);
                for owner in owners {
                    match astro_dav::thumb::backfill_exif(&dav, &owner, 25).await {
                        Ok(s) => (scanned, found, remaining) = (scanned + s.scanned, found + s.found, remaining + s.remaining),
                        Err(e) => {
                            bus.publish_to(astro_core::Audience::Captain, "alert", "exif", format!("rattrapage EXIF impossible : {e}"), None);
                        }
                    }
                }
                if scanned > 0 {
                    bus.publish_to(astro_core::Audience::Captain, "system", "exif", format!("rattrapage EXIF : {scanned} photo(s) lue(s), {found} avec position ou date, {remaining} restante(s)"), None);
                }
                wait = if remaining > 0 { Duration::from_secs(20) } else { Duration::from_secs(900) };
            }
        }));
    }
}

/// Synchronisation continue N¹/N² : les profils et événements des amis et de leurs amis, depuis les relais des stations de
/// la constellation (joints par le maillage P2P, adresse publique en repli), et mesure de leur réplication.
pub(super) fn crawl(p: &Parts<'_>, relay_client: &Arc<astroportd::p2p_relay::P2pRelayClient>, background: &mut Background) {
    let Parts { a, db, activity, stop_watch, services_peer, uplanet_name, p2p_handle, .. } = p;
    if !a.no_crawl {
        let client = astroportd::p2p_relay::SharedRelayClient(relay_client.clone());
        let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
        let own = services_peer.clone();
        let source: astro_sync::RelaySource = Arc::new(move || {
            let peers = swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default();
            peers.iter().filter(|p| Some(&p.node_id) != own.as_ref()).map(astro_sync::Peer::target).collect()
        });
        let (hook_db, hook_bus, hook_client) = (db.clone(), activity.clone(), client.clone());
        let hook: astro_sync::PassHook = Arc::new(move |r| {
            match &r.result {
                Ok(s) => {
                    if s.inserted > 0 {
                        hook_bus.publish("relay", "crawl", format!("synchronisation N² : {} nouvel(s) événement(s) répliqué(s) ({} membres, {} relais)", s.inserted, s.keys, r.relays), None);
                    }
                    if let Some((relay, key, why)) = s.errors.first() {
                        hook_bus.publish_to(astro_core::Audience::Captain, "alert", "crawl", format!("synchronisation N² : {} échec(s), par exemple {relay} pour …{} : {why}", s.errors.len(), key.chars().take(8).collect::<String>()), None);
                    }
                }
                Err(e) => {
                    hook_bus.publish_to(astro_core::Audience::Captain, "alert", "crawl", format!("synchronisation N² interrompue : {e}"), None);
                }
            }
            let routes = serde_json::Value::Array(hook_client.0.routes().iter().map(astroportd::p2p_relay::RouteState::to_json).collect());
            let db = hook_db.clone();
            tokio::spawn(async move {
                let _ = db
                    .writer()
                    .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('crawl_status', 'routes', ?1, strftime('%s','now')) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![routes.to_string().into()])
                    .await;
            });
        });
        let config = astro_sync::CrawlConfig { relays: a.relays.clone(), ..astro_sync::CrawlConfig::default() };
        let coop_keys: Vec<String> = astroportd::coop_config::coop_keys(uplanet_name).map(|(_, hex)| vec![hex]).unwrap_or_default();
        let crawler = Arc::new(astro_sync::Crawler::new(db.clone(), client.clone(), config).with_relay_source(source).with_hook(hook).with_extra_keys(coop_keys));
        // Mesure de la réplication des données des membres hébergés : où et par qui (événements ; fichiers si kubo est joint).
        {
            let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
            let peers_fn: astro_sync::replication::PeersFn = Arc::new(move || swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default());
            // Qui détient un fichier : le maillage interne d'abord (sonde `HAS` du protocole de blobs, sans kubo), kubo pour les
            // CID qu'il ne sait pas échanger (`bafybei…` : fichiers en plusieurs blocs) quand `--kubo` est donné.
            let kubo = a.kubo.as_deref().map(|url| Arc::new(astro_dav::kubo::KuboBlobStore::new(url)));
            let mesh = p2p_handle.clone();
            let providers: Option<astro_sync::replication::ProviderFn> = (mesh.is_some() || kubo.is_some()).then(|| {
                let swarm = a.swarm_dir.clone().or_else(default_swarm_dir);
                let f: astro_sync::replication::ProviderFn = Arc::new(move |cid: String| {
                    let (kubo, mesh, swarm) = (kubo.clone(), mesh.clone(), swarm.clone());
                    Box::pin(async move {
                        if let (Some(handle), true) = (&mesh, cid.starts_with("bafkrei")) {
                            let peers = swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default();
                            let probes = peers.into_iter().filter_map(|p| p.node_id.parse::<astro_p2p::PeerId>().ok().map(|id| (p.node_id, id))).map(|(name, id)| {
                                let (handle, cid) = (handle.clone(), cid.clone());
                                async move { (name, astro_p2p::blob::has_blob(&handle, id, &cid, Duration::from_secs(4)).await) }
                            });
                            let mut found = Vec::new();
                            for (name, r) in futures_util::future::join_all(probes).await {
                                if matches!(r, Ok(Some(_))) {
                                    found.push(name);
                                }
                            }
                            return Ok(found);
                        }
                        match &kubo {
                            Some(k) => k.providers(&cid, 20, Duration::from_secs(8)).await.map_err(|e| e.to_string()),
                            None => Err("CID en plusieurs blocs : non échangeable par le maillage, et kubo n'est pas joint (--kubo)".to_owned()),
                        }
                    })
                });
                f
            });
            let measured = providers.is_some();
            let config = astro_sync::ReplicationConfig { own_node_id: services_peer.clone(), ..astro_sync::ReplicationConfig::default() };
            let replicator = Arc::new(astro_sync::Replicator::new(db.clone(), client.clone(), peers_fn, providers, config));
            let alert_bus = activity.clone();
            let on_pass: Arc<dyn Fn(Result<usize, String>) + Send + Sync> = Arc::new(move |r| {
                if let Err(e) = r {
                    alert_bus.publish_to(astro_core::Audience::Captain, "alert", "replication", format!("mesure de la réplication impossible : {e}"), None);
                }
            });
            println!("réplication : mesure par membre toutes les {} s ({})", (a.crawl_every * 2).max(60), if measured { "événements et fichiers" } else { "événements seulement : --kubo absent" });
            background.push(tokio::spawn(replicator.run_forever(Duration::from_secs(120), Duration::from_secs((a.crawl_every * 2).max(60)), on_pass, stop_watch.clone())));
        }
        println!("synchronisation N¹/N² : un passage toutes les {} s (premier dans 60 s)", a.crawl_every);
        background.push(tokio::spawn(crawler.run_forever_after(Duration::from_secs(60), Duration::from_secs(a.crawl_every.max(30)), stop_watch.clone())));
    }
}

/// Virements Ğ1 reçus (le titulaire est prévenu dès le bloc), réactions `+N` et renouvellement horaire des nœuds découverts.
pub(super) fn funds(p: &Parts<'_>, funds_chain: Option<Arc<RpcPool<SubxtConnector>>>, squid_urls: &[String], background: &mut Background) {
    let Parts { a, db, activity, stop_watch, uplanet_name, secrets_dir, nostr_dir, notify, relay, .. } = p;
    let squid_urls = squid_urls.to_vec();
    if let Some(chain) = funds_chain {
        match astroportd::funds::master_wallet(uplanet_name) {
            Ok(master) => {
                let indexer = (!squid_urls.is_empty()).then(|| Arc::new(astroportd::funds::SquidPrimal::new(squid_urls.clone())));
                // Les listes issues de la découverte se renouvellent toutes les heures : nœuds RPC (le pool garde sa connexion qui
                // marche et suit la nouvelle liste à la prochaine bascule) et indexeurs squid. Une liste imposée par option ne bouge pas.
                let (auto_rpc, auto_squid) = (a.duniter_rpc.is_empty(), a.squid_url.is_none());
                if !a.no_duniter && (auto_rpc || (auto_squid && indexer.is_some())) {
                    let (pool, handle, db, mut stop) = (chain.clone(), indexer.as_ref().map(|ix| ix.urls_handle()), db.clone(), stop_watch.clone());
                    background.push(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                () = tokio::time::sleep(Duration::from_secs(astroportd::duniter_nodes::CACHE_TTL_SECS.unsigned_abs() + 60)) => {}
                                _ = stop.changed() => return,
                            }
                            let fresh = astroportd::duniter_nodes::nodes(&db, Arc::new(astroportd::duniter_nodes::RealHttp::default())).await;
                            if fresh.degraded {
                                continue; // aucune réponse : on garde les listes qui marchent
                            }
                            if auto_rpc {
                                pool.set_urls(fresh.rpc_urls());
                            }
                            if let (true, Some(h)) = (auto_squid, &handle) {
                                *h.write().unwrap_or_else(|e| e.into_inner()) = fresh.squid_urls();
                            }
                        }
                    }));
                }
                // Réactions `+N` : le MULTIPASS hébergé ici qui réagit envoie N Ẑen (virement Ğ1) à l'auteur, sans toucher au relai.
                if !a.no_zen_reactions {
                    let zen = Arc::new(astroportd::zen_react::ZenReactions::new(
                        db.clone(),
                        chain.clone(),
                        secrets_dir.clone(),
                        indexer.clone(),
                        master.clone(),
                        activity.clone(),
                        notify.clone(),
                        astroportd::zen_react::ZenLimits { per_reaction: a.zen_per_reaction, per_day: a.zen_per_day },
                    ));
                    background.push(tokio::spawn(zen.run(relay.subscribe(), stop_watch.clone())));
                    println!("Ẑen : les réactions +N des MULTIPASS hébergés ici envoient N Ẑen (plafonds : {} par réaction, {} par jour)", a.zen_per_reaction, a.zen_per_day);
                }
                let watch = Arc::new(astroportd::funds::FundsWatch::new(db.clone(), activity.clone(), master, indexer, notify.clone(), nostr_dir.clone()));
                background.push(tokio::spawn(watch.run(chain, stop_watch.clone())));
            }
            Err(e) => eprintln!("Ğ1 : portefeuille maître de la constellation introuvable ({e}) : écoute des virements inactive"),
        }
    }
}

/// Balayage RÊVE de la mémoire sémantique de BRO, toutes les 30 minutes.
pub(super) fn bro_sweep(p: &Parts<'_>, memory: &Option<Arc<Memory>>, background: &mut Background) {
    let (a, stop_watch) = (p.a, &p.stop_watch);
    if let Some(m) = &memory {
        println!("BRO : mémoire sémantique active (Ollama, {}) ; cycle RÊVE à {} souvenirs", a.embed_model, astro_bro::REVE_THRESHOLD);
        let (m, mut stop) = (m.clone(), stop_watch.clone());
        background.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30 * 60));
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = stop.changed() => return,
                }
                if let Err(e) = m.sweep().await {
                    eprintln!("BRO : balayage RÊVE impossible : {e}");
                }
            }
        }));
    }
}

/// Orchestrateur de nuit : 20h12 solaire vrai de la station.
pub(super) async fn nightly(p: &Parts<'_>, memory: &Option<Arc<Memory>>, backup: &Arc<Backup>, background: &mut Background) -> Result<(), Failure> {
    let Parts { a, db, activity, stop_watch, dav, local, services_peer, node_hex, node_secret, node_publisher, .. } = p;
    if !a.no_nightly {
        let origin = match a.lat.zip(a.lon) {
            Some(c) => Some(c),
            None => {
                let hex = node_hex.clone();
                db.readers().read(move |c| Ok(c.query_row("SELECT lat, lon FROM identities WHERE hex = ?1 AND lat IS NOT NULL AND lon IS NOT NULL", [hex], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok())).await?
            }
        };
        match origin.map(|(lat, lon)| astro_core::solar::Coordinates::new(lat, lon)) {
            Some(Ok(coords)) => {
                let dream: Option<astroportd::solar_cron::Hook> = memory.clone().map(|m| -> astroportd::solar_cron::Hook {
                    Arc::new(move |_| {
                        let m = m.clone();
                        Box::pin(async move { m.sweep().await.map(|done| format!("{} emplacement(s) examiné(s)", done.len())).map_err(|e| e.to_string()) })
                    })
                });
                let purge_shares: astroportd::solar_cron::Hook = {
                    let dav = dav.clone();
                    Arc::new(move |now| {
                        let dav = dav.clone();
                        Box::pin(async move { astro_dav::share::purge_expired(&dav.db, &*dav.blobs, i64::try_from(now).unwrap_or(0)).await.map(|n| format!("{n} lien(s) d'invité expiré(s) supprimé(s)")).map_err(|e| e.to_string()) })
                    })
                };
                let backup_night: astroportd::solar_cron::Hook = {
                    let backup = backup.clone();
                    Arc::new(move |_| {
                        let backup = backup.clone();
                        Box::pin(async move { backup.sync_night().await })
                    })
                };
                let integrity_night: astroportd::solar_cron::Hook = {
                    let integrity = Arc::new(astroportd::integrity::Integrity::new(db.clone(), local.clone(), Some(activity.clone())));
                    let dav = dav.clone();
                    Arc::new(move |now| {
                        let (integrity, dav) = (integrity.clone(), dav.clone());
                        Box::pin(async move {
                            // Rattrapage des empreintes visuelles des photos déjà là (doublons), puis contrôle des blobs.
                            let hashed = astro_dav::thumb::backfill_phash(&dav.db, &*dav.blobs, 500).await.map_err(|e| format!("empreintes visuelles : {e}"))?;
                            let report = integrity.run(now).await?;
                            Ok(if hashed > 0 { format!("{report} ; {hashed} empreinte(s) visuelle(s) calculée(s)") } else { report })
                        })
                    })
                };
                let snapshot_dir = a.snapshot_dir.clone().unwrap_or_else(|| a.db.parent().map_or_else(|| PathBuf::from("snapshots"), |p| p.join("snapshots")));
                let nightly = Arc::new(astroportd::solar_cron::Nightly {
                    db: db.clone(),
                    snapshot_dir,
                    node_hex: node_hex.clone(),
                    node_secret: *node_secret,
                    peer_id: services_peer.clone(),
                    weekly_costs: astro_core::Amount(a.weekly_costs),
                    split: astro_economy::split::SplitConfig::default(),
                    purge_shares: Some(purge_shares),
                    dream,
                    backup: Some(backup_night),
                    integrity: Some(integrity_night),
                    publisher: node_publisher.clone(),
                    activity: Some(activity.clone()),
                    pause: Duration::from_secs(1),
                });
                let next = astro_core::solar::system_now_checked().ok().and_then(|n| astro_core::solar::next_20h12(n, &coords).ok());
                println!("20h12 solaire : orchestrateur de nuit actif (prochaine échéance : {})", next.map_or_else(|| "horloge invalide".to_owned(), |t| format!("{} {:02}:{:02} UTC", astro_core::week::date_string(t), t % 86_400 / 3_600, t % 3_600 / 60)));
                // Rattrapage des nuits manquées pendant l'arrêt, en tâche de fond : le démarrage du serveur n'attend pas.
                {
                    let (n, c) = (nightly.clone(), coords);
                    background.push(tokio::spawn(async move {
                        let Ok(now) = astro_core::solar::system_now_checked() else { return };
                        for r in n.catch_up(now, &c).await {
                            println!("20h12 solaire : rattrapage de la nuit du {} ({} étapes, {})", r.date, r.steps.len(), if r.all_ok() { "ok" } else { "avec échec" });
                        }
                    }));
                }
                background.push(tokio::spawn(nightly.run(coords, stop_watch.clone())));
            }
            Some(Err(e)) => eprintln!("20h12 solaire : {e} : orchestrateur de nuit inactif"),
            None => eprintln!("20h12 solaire : coordonnées de la station inconnues (--lat/--lon, ou position du NODE en base) : orchestrateur de nuit inactif"),
        }
    }
    Ok(())
}
