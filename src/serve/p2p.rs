//! Maillage P2P de la station : démarrage du nœud, services partagés derrière la politique de tunnels, accès `--p2p-forward`
//! et offre du relai Nostr aux autres stations.

use super::*;

/// Gardiens des services P2P : les abandonner les arrête (tunnels, blobs, nœud).
pub(super) type Guards = Vec<Box<dyn std::any::Any + Send>>;

/// Rôle de la station dans le maillage.
#[derive(Default)]
pub(super) struct P2pRole {
    /// Elle sert de relais (`--p2p-relay-server`, ou listée dans `P2P_RELAYS`).
    pub relay_server: bool,
    /// Adresses publiques qu'elle annonce (`--p2p-external`, son entrée de `P2P_RELAYS`).
    pub announce: Vec<Multiaddr>,
}

/// Démarre le maillage si une option le demande ; rend ses gardiens et sa poignée. `exposed_names` reçoit les services offerts.
/// Le maillage est actif par défaut (`--no-p2p` le coupe) : sans `--p2p-listen`, écoute sur 4101 ou le premier port libre
/// après — chaque plan d'un `astroportd planes` reçoit le sien. Sans graine (NODE absent), le défaut s'efface avec un message ;
/// une option P2P explicite, elle, reste une erreur.
pub(super) async fn start(a: &ServeArgs, p2p_seed: Option<[u8; 32]>, coop_relays: &[(Multiaddr, PeerId)], local: &Arc<AutoBlobStore>, tunnel_policy: &Arc<TunnelPolicy>, exposed_names: &mut Vec<String>) -> Result<(Guards, Option<astro_p2p::P2pHandle>, P2pRole), Failure> {
    let mut p2p_guards: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
    let mut p2p_handle = None;
    let mut role = P2pRole::default();
    let explicit = !a.p2p_listen.is_empty() || !a.p2p_bootstrap.is_empty() || a.p2p_relay_server || !a.p2p_use_relay.is_empty();
    if a.no_p2p {
        if explicit {
            return Err("--no-p2p coupe le maillage : retirez les options --p2p-listen/--p2p-bootstrap/--p2p-relay-server/--p2p-use-relay".into());
        }
        println!("P2P : maillage coupé (--no-p2p)");
    } else if !explicit && p2p_seed.is_none() {
        println!("P2P : maillage non démarré (NODE absent de --game-dir : pas de graine) ; lancez `astroportd init` pour l'activer");
    } else {
        let seed = p2p_seed.ok_or("graine P2P introuvable : NODE absent de --game-dir (secret.NODE.dunikey) et ASTRO_P2P_SEED non défini")?;
        let mut cfg = P2pConfig::local(seed);
        if a.p2p_listen.is_empty() {
            let port = astroportd::mesh_admin::free_port().ok_or("aucun port libp2p libre entre 4101 et 4200 : précisez --p2p-listen ou --no-p2p")?;
            cfg.listen = vec![format!("/ip4/0.0.0.0/tcp/{port}").parse()?];
            println!("P2P : écoute par défaut sur le port {port} (--p2p-listen pour un autre, --no-p2p pour couper)");
        } else {
            cfg.listen = a.p2p_listen.clone();
        }
        // Les stations gardées depuis l'écran du Capitaine sont jointes en tâche de fond (`mesh_admin::Mesh::keep_alive`) :
        // une station éteinte ne retarde jamais le démarrage.
        cfg.bootstrap = a.p2p_bootstrap.clone();
        cfg.external = a.p2p_external.clone();
        cfg.relay_server = a.p2p_relay_server;
        cfg.mdns = !a.no_mdns;
        // La coopérative désigne cette station comme relais : elle en rend le service et annonce l'adresse publiée.
        let me = astro_p2p::keypair_from_seed(&seed)?.public().to_peer_id();
        if let Some((addr, _)) = coop_relays.iter().find(|(_, p)| *p == me) {
            cfg.relay_server = true;
            let mut public = addr.clone();
            public.pop();
            cfg.external.push(public);
            role.announce.push(addr.clone());
            println!("P2P : station relais de la constellation (P2P_RELAYS de la configuration coopérative) : {addr}");
        }
        role.relay_server = cfg.relay_server;
        for x in &a.p2p_external {
            role.announce.push(x.clone());
        }
        if let Some(path) = &a.swarm_key {
            cfg.psk = Some(parse_swarm_key(&std::fs::read_to_string(path)?)?);
        }
        let node = P2pNode::spawn(cfg).await?;
        let handle = node.handle.clone();
        println!("P2P : PeerID {} ({})", handle.peer_id(), if a.swarm_key.is_some() { "réseau privé" } else { "réseau ouvert" });
        // Les places chez les relais (`--p2p-use-relay`, `P2P_RELAYS`) sont prises et refaites par `mesh_admin::Mesh::keep_alive`.
        if !a.no_mdns {
            println!("P2P : stations du réseau local cherchées par mDNS (--no-mdns pour couper)");
        }
        p2p_guards.push(Box::new(serve_blobs(&handle, local.clone(), ServeOpts::default())?));
        let mut specs: Vec<String> = a.p2p_expose.clone();
        // Les trois services partagés entre Capitaines : SSH, ComfyUI, Ollama (chacun derrière la politique des tunnels).
        for (name, target) in [("ssh", &a.p2p_ssh), ("comfyui", &a.p2p_comfyui), ("ollama", &a.p2p_ollama)] {
            if target != "none" && !specs.iter().any(|s| s.split_once('=').is_some_and(|(n, _)| n == name)) {
                specs.push(format!("{name}={target}"));
                exposed_names.push(name.to_owned());
            }
        }
        for spec in &specs {
            let (name, target) = spec.split_once('=').ok_or("--p2p-expose attend nom=hôte:port")?;
            // Chaque flux passe par la politique du Capitaine (voir l'écran Options › Tunnels) : rien n'est ouvert sans elle.
            let opts = ExposeOpts { gate: Some(Arc::new(PolicyGate::new(tunnel_policy.clone()))), ..ExposeOpts::default() };
            let mode = tunnel_policy.rule(name).mode;
            println!("P2P : {name} exposé, accès « {mode:?} » (réglable dans l'interface du Capitaine)");
            p2p_guards.push(Box::new(expose(&handle, name, target.to_owned(), opts)?));
        }
        // Les services que le Capitaine partage à chaud depuis l'écran Options (gardés en base) rejoignent le maillage.
        let fixed: Vec<String> = specs.iter().filter_map(|s| s.split_once('=').map(|(n, _)| n.to_owned())).collect();
        tunnel_policy.attach(&handle, &fixed);
        for spec in &a.p2p_forward {
            let (bind, rest) = spec.split_once('=').ok_or("--p2p-forward attend bind=PeerID/nom")?;
            let (peer, name) = rest.split_once('/').ok_or("--p2p-forward attend bind=PeerID/nom")?;
            let fwd = forward(&handle, bind.parse()?, peer.parse::<PeerId>()?, name).await?;
            println!("P2P : {} → {name} de {peer}", fwd.local_addr);
            p2p_guards.push(Box::new(fwd));
        }
        p2p_guards.push(Box::new(node));
        p2p_handle = Some(handle);
    }
    Ok((p2p_guards, p2p_handle, role))
}

/// Le relai est aussi offert au maillage par une écoute locale (`/x/strfry-<PeerID>`, le nom de DRAGON) : c'est le transport
/// que les autres stations, même derrière une box, utilisent pour le lire, sans port public ni domaine. Ce n'est pas un
/// tunnel réglable : la politique de lecture (pare-feu social NIP-42) reste celle du relai.
pub(super) async fn offer_relay(a: &ServeArgs, handle: &astro_p2p::P2pHandle, relay: &Arc<Relay>, p2p_guards: &mut Guards) -> Result<(), Failure> {
    if a.p2p_expose.iter().any(|s| s.split_once('=').is_some_and(|(n, _)| n == astro_sync::peers::RELAY_SERVICE)) {
        println!("P2P : le service « {} » est déjà exposé par --p2p-expose", astro_sync::peers::RELAY_SERVICE);
    } else {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local = listener.local_addr()?;
        tokio::spawn(astro_relay::serve(listener, relay.clone()));
        p2p_guards.push(Box::new(expose(handle, astro_sync::peers::RELAY_SERVICE, local.to_string(), ExposeOpts::default())?));
        println!("P2P : relai Nostr offert au maillage (/x/{}-{})", astro_sync::peers::RELAY_SERVICE, handle.peer_id());
    }
    Ok(())
}
