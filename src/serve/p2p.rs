//! Maillage P2P de la station : démarrage du nœud, services partagés derrière la politique de tunnels, accès `--p2p-forward`
//! et offre du relai Nostr aux autres stations.

use super::*;

/// Gardiens des services P2P : les abandonner les arrête (tunnels, blobs, nœud).
pub(super) type Guards = Vec<Box<dyn std::any::Any + Send>>;

/// Démarre le maillage si une option le demande ; rend ses gardiens et sa poignée. `exposed_names` reçoit les services offerts.
pub(super) async fn start(a: &ServeArgs, p2p_seed: Option<[u8; 32]>, local: &Arc<AutoBlobStore>, tunnel_policy: &Arc<TunnelPolicy>, exposed_names: &mut Vec<String>) -> Result<(Guards, Option<astro_p2p::P2pHandle>), Failure> {
    let mut p2p_guards: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
    let mut p2p_handle = None;
    if !a.p2p_listen.is_empty() || !a.p2p_bootstrap.is_empty() || a.p2p_relay_server || !a.p2p_use_relay.is_empty() {
        let seed = p2p_seed.ok_or("graine P2P introuvable : NODE absent de --game-dir (secret.NODE.dunikey) et ASTRO_P2P_SEED non défini")?;
        let mut cfg = P2pConfig::local(seed);
        if !a.p2p_listen.is_empty() {
            cfg.listen = a.p2p_listen.clone();
        }
        cfg.bootstrap = a.p2p_bootstrap.clone();
        cfg.external = a.p2p_external.clone();
        cfg.relay_server = a.p2p_relay_server;
        if let Some(path) = &a.swarm_key {
            cfg.psk = Some(parse_swarm_key(&std::fs::read_to_string(path)?)?);
        }
        let node = P2pNode::spawn(cfg).await?;
        let handle = node.handle.clone();
        println!("P2P : PeerID {} ({})", handle.peer_id(), if a.swarm_key.is_some() { "réseau privé" } else { "réseau ouvert" });
        for relay in &a.p2p_use_relay {
            match handle.listen_via_relay(relay.clone()).await {
                Ok(addr) => println!("P2P : joignable derrière la box via {addr}"),
                Err(e) => eprintln!("P2P : relais {relay} indisponible : {e}"),
            }
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
    Ok((p2p_guards, p2p_handle))
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
