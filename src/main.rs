//! `astroportd` : point d'entrée du binaire unique.
//!
//! Sous-commandes :
//! - `replay` : reconstruit les tables de projection depuis `nostr_events` (et, avec `--from`, depuis un relai distant) ;
//!   avec `--blobs` et `--secrets`, restaure aussi le VFS et le trousseau uCloud depuis les manifestes chiffrés ;
//! - `relay` : sert le relai Nostr souverain (pare-feu social NIP-42) ;
//! - `dav` : sert le WebDAV personnel chiffré (UENC v2) et publie le manifeste uCloud ;
//! - `serve` : le démon complet (relai, WebDAV, FaceCloud, BRO, application web sur `/`, `/health`, flux SSE) ;
//! - `doctor` : diagnostic de la station (permissions 0600, base WAL, certificats ACME, horloge solaire) ;
//! - `token` : jeton de connexion d'un compte hébergé ; `route` : meilleur nœud du swarm pour un service.
//!
//! Les clés de la station sont découvertes automatiquement dans `~/.zen/game` (`--game-dir` ou `ASTRO_GAME_DIR`) :
//! NODE (`secret.nostr`, `secret.NODE.dunikey`), Capitaine et MULTIPASS hébergés. `ASTRO_STATION_SECRET` et
//! `ASTRO_P2P_SEED` ne sont plus que des surcharges facultatives, jamais passées en argument.

mod identity;
mod serve;

use astroportd::{doctor, init};

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use astro_crypto::{FileKeySource, KeySource};
use astro_dav::{AutoBlobStore, DavConfig, DavState, FsBlobStore, KuboBlobStore, ManifestPublisher};
use astro_facecloud::{FaceCloud, FaceConfig, HttpFaceEngine};
use astro_db::{Db, SqliteReplayStore};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::{CrawlConfig, Crawler, Projector, RelayOutbox, WsClient, replay_local, replay_with_ucloud};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "astroportd", version, about = "Astroport.ONE : binaire unique (Nostr + IPFS = vérité, SQLite = cache rejouable)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Démon complet : relai Nostr, WebDAV, FaceCloud, proxy, HTTPS automatique, maillage P2P.
    Serve(Box<serve::ServeArgs>),
    /// Assistant d'installation : rituel Y-Level (clé SSH = IPFSNODEID = Ğ1 = Nostr), choix de la constellation
    /// ORIGIN (ouverte) ou ẐEN (privée), dossiers de la station, unité systemd.
    Init(Box<init::InitArgs>),
    /// Affiche (et avec `--verify` contrôle) la correspondance SSH ↔ IPFSNODEID ↔ Ğ1 ↔ Nostr de la station.
    Ylevel(init::YlevelArgs),
    /// Vérifie l'inférence faciale locale (SCRFD + ArcFace par ONNX Runtime) : fonction compilée, modèles, bibliothèque, chargement.
    FaceCheck {
        /// Dossier des modèles `det_10g.onnx` et `w600k_r50.onnx` (défaut : les emplacements habituels).
        #[arg(long)]
        models: Option<PathBuf>,
    },
    /// Génère une clé de réseau privé `swarm.key` pour une constellation (à copier sur chaque station).
    SwarmKey,
    /// Reprend le uCloud d'Astroport.ONE d'un compte (`~/.zen/game/nostr/<email>/.ucloud`) : mêmes blobs chiffrés, mêmes clés,
    /// rien n'est recopié. L'ancienne base n'est jamais modifiée. Voir `--dry-run`.
    ImportUcloud(Box<ImportUcloudArgs>),
    /// Remplit le uCloud d'un compte depuis un dossier d'un autre serveur WebDAV (Nextcloud, ownCloud, une autre station) :
    /// mode « pull ». Rejouable : ce qui est déjà là n'est jamais écrasé ni dupliqué. Voir `pull-webdav <compte> add`.
    PullWebdav(Box<PullArgs>),
    /// Adopte **sans copie** des fichiers d'un plan dans un autre (ORIGIN / ZEN*) quand les deux astroportd partagent le même
    /// bassin de blobs (`--blobs` identique, `--pool-peer` croisés) : même blob, même clé, aucun octet de plus sur le disque.
    BridgeCopy(Box<BridgeCopyArgs>),
    /// État du bassin de blobs partagé entre plusieurs plans : poids, part de chaque plan, place économisée par le partage,
    /// blobs que plus personne ne référence (`--sweep` supprime ceux de plus de 24 h).
    PoolStatus(Box<PoolStatusArgs>),
    /// Diagnostic : permissions 0600, base WAL, certificats ACME, horloge solaire. Code 1 si une vérification échoue.
    Doctor(doctor::DoctorArgs),
    /// Affiche le jeton de connexion d'un compte hébergé (interface web et clients WebDAV).
    Token {
        /// Email ou clé Nostr (64 hex) du compte.
        account: String,
        /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Base de la station (défaut : `~/.zen/astroportd/uplanet.db`) : sert à connaître la génération du jeton après un changement.
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Extensions WebAssembly (`plugins/*.wasm`) : lister ou appeler un plugin dans sa bulle isolée.
    Plugin {
        /// Dossier des plugins.
        #[arg(long, default_value = "plugins")]
        dir: PathBuf,
        #[command(subcommand)]
        action: PluginAction,
    },
    /// MULTIPASS : créer un compte sur cette station (clés isolées par constellation) ou lister les comptes hébergés.
    Multipass {
        #[command(subcommand)]
        action: MultipassAction,
    },
    /// Films et séries : attestation de copie privée (CPI L. 122-5) et dépôt chiffré avec publication NIP-71 (kind 21/22)
    /// réservée au cercle N¹.
    Media(Box<MediaArgs>),
    /// Choisit le meilleur nœud du swarm pour un service (héritage d'`astrosystemctl connect`).
    Route {
        /// Service DRAGON recherché (`ollama`, `comfyui`, `qdrant`…).
        service: String,
        /// `power` (Power-Score), `closest` (Haversine) ou `random` (parmi les 3 plus puissants).
        #[arg(long, default_value = "power")]
        mode: astro_p2p::routing::Mode,
        /// Dossier des `12345.json` des pairs (défaut : `$HOME/.zen/tmp/swarm`).
        #[arg(long)]
        swarm_dir: Option<PathBuf>,
        /// Latitude de cette station (mode `closest`).
        #[arg(long, allow_hyphen_values = true, requires = "lon")]
        lat: Option<f64>,
        /// Longitude de cette station.
        #[arg(long, allow_hyphen_values = true, requires = "lat")]
        lon: Option<f64>,
    },
    /// Reconstruit les tables de projection depuis les événements Nostr.
    Replay {
        /// Base `uplanet.db`.
        #[arg(long)]
        db: PathBuf,
        /// Événements lus par transaction.
        #[arg(long, default_value_t = 1000)]
        page_size: usize,
        /// Relai à crawler avant le replay (`ws://…`), pour repartir d'une base vide.
        #[arg(long)]
        from: Option<String>,
        /// Clé (hex) d'un MULTIPASS hébergé ici, point de départ du crawl ; répétable.
        #[arg(long = "hosted")]
        hosted: Vec<String>,
        /// Tours de crawl au plus (N1 puis N2…).
        #[arg(long, default_value_t = 6)]
        rounds: usize,
        /// Dossier des blobs chiffrés (IPFS local) : active la restauration d'uCloud (avec `--secrets`).
        #[arg(long, requires = "secrets")]
        blobs: Option<PathBuf>,
        /// Dossier des comptes (`~/.zen/game/nostr`) contenant `<email>/.secret.nostr` (défaut : `<game-dir>/nostr`).
        #[arg(long, requires = "blobs")]
        secrets: Option<PathBuf>,
        /// Dossier `~/.zen/game` des identités locales (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
        #[arg(long)]
        game_dir: Option<PathBuf>,
    },
    /// Sert le WebDAV personnel chiffré et publie le manifeste uCloud.
    Dav {
        /// Base `uplanet.db`.
        #[arg(long)]
        db: PathBuf,
        /// Dossier des blobs chiffrés.
        #[arg(long)]
        blobs: PathBuf,
        /// Dossier des comptes (`<email>/.secret.nostr`) ; défaut : `<game-dir>/nostr`.
        #[arg(long)]
        secrets: Option<PathBuf>,
        /// Dossier `~/.zen/game` des identités locales (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Adresse d'écoute.
        #[arg(long, default_value = "127.0.0.1:54321")]
        listen: String,
        /// Préfixe de montage.
        #[arg(long, default_value = "/dav")]
        mount: String,
        /// URL publique de la station (liaison NIP-98 derrière un reverse proxy).
        #[arg(long)]
        public_url: Option<String>,
        /// Délai de regroupement des publications de manifeste, en secondes.
        #[arg(long, default_value_t = 2)]
        debounce_secs: u64,
        /// API RPC d'IPFS Kubo (`http://127.0.0.1:5001`) ; sans elle, ou si elle ne répond pas, les blobs restent sur disque.
        #[arg(long)]
        kubo: Option<String>,
        /// Active FaceCloud : URL du moteur d'inférence faciale (Brain GPU).
        #[arg(long)]
        face_engine: Option<String>,
        /// Relais (`ws://` ou `wss://`) où publier les partages FaceCloud ; répétable.
        #[arg(long = "relay")]
        relays: Vec<String>,
    },
    /// Sert le relai Nostr.
    Relay {
        /// Base `uplanet.db`.
        #[arg(long)]
        db: PathBuf,
        /// Adresse d'écoute.
        #[arg(long, default_value = "127.0.0.1:7777")]
        listen: String,
        /// URL publique (contrôle du tag `relay` des AUTH).
        #[arg(long)]
        relay_url: Option<String>,
        /// Lire l'IP cliente dans X-Forwarded-For (reverse proxy de confiance uniquement).
        #[arg(long)]
        trust_forwarded_for: bool,
        /// Période de rafraîchissement du cercle social, en secondes.
        #[arg(long, default_value_t = 15)]
        circle_refresh_secs: u64,
    },
}

#[derive(Subcommand)]
enum MultipassAction {
    /// Crée un MULTIPASS : SALT/PEPPER aléatoires, clés Nostr + Ğ1 + IPNS, fichiers du compte, événements de naissance.
    /// Les secrets ne sont affichés **qu'une fois**.
    Create {
        /// Adresse email du titulaire.
        email: String,
        /// Langue du compte (`fr`, `en`, `es`…).
        #[arg(long, default_value = "fr")]
        lang: String,
        /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Dossier de la station (`station.conf`, `uplanet.db`) ; défaut `~/.zen/astroportd`.
        #[arg(long)]
        home: Option<PathBuf>,
        /// `swarm.key` de la constellation (ẐEN) ; sinon `uplanetname` de station.conf, sinon ORIGIN (64 zéros).
        #[arg(long)]
        swarm_key: Option<PathBuf>,
        /// Base `uplanet.db` (défaut : `<home>/uplanet.db`).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Relai (`ws://` ou `wss://`) où publier les événements de naissance ; répétable.
        #[arg(long = "relay")]
        relays: Vec<String>,
        /// Désigne ce compte comme **Capitaine** de la station (le premier MULTIPASS, `players/.current/.player`) ;
        /// les MULTIPASS créés ensuite le suivent. Refusé si la station a déjà un autre Capitaine.
        #[arg(long)]
        captain: bool,
        /// Sortie JSON (scripts).
        #[arg(long)]
        json: bool,
    },
    /// Rétablit sur cette station un MULTIPASS **existant** depuis sa clé de secours : deux parts (P1 du membre + P3 d'un ami), ou l'ancienne carte complète (station en panne, ou déménagement) : même clé, donc
    /// mêmes amis et mêmes fichiers. La station retrouve ensuite ses événements, annonce sa nouvelle station d'accueil et demande à
    /// chaque proche ce qu'il détient : ses amis n'ont rien à faire. Un nouveau PASS est généré (l'ancien n'est pas récupérable).
    Recover {
        /// Parts de secours (`astro-share:v1|…`, deux, séparées par un espace) ou ancienne carte (`astro-secret:v1|…`), ou `-` pour la lire sur l'entrée standard, ou `@fichier` : à éviter en argument, il se lirait dans `ps`.
        card: String,
        /// Langue du compte (`fr`, `en`, `es`…).
        #[arg(long, default_value = "fr")]
        lang: String,
        /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Dossier de la station (`station.conf`, `uplanet.db`) ; défaut `~/.zen/astroportd`.
        #[arg(long)]
        home: Option<PathBuf>,
        /// `swarm.key` de la constellation (ẐEN) ; sinon `uplanetname` de station.conf, sinon ORIGIN (64 zéros).
        #[arg(long)]
        swarm_key: Option<PathBuf>,
        /// Base `uplanet.db` (défaut : `<home>/uplanet.db`).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Relai (`ws://` ou `wss://`) où rapatrier tout de suite les événements du compte ; répétable. Sans lui, c'est le démon qui s'en charge.
        #[arg(long = "relay")]
        relays: Vec<String>,
        /// Tours de crawl au plus (amis N1 puis N2…).
        #[arg(long, default_value_t = 6)]
        rounds: usize,
        /// Sortie JSON (scripts).
        #[arg(long)]
        json: bool,
    },
    /// Affiche de nouveau une part de la clé de secours d'un MULTIPASS hébergé ici (texte, ou QR en SVG avec `--svg`).
    /// P1 (défaut) est celle du membre, à imprimer et à garder hors ligne ; P3 est celle que gardent ses amis.
    Card {
        /// Email du compte.
        account: String,
        /// Numéro de la part : 1 (membre), 2 (station), 3 (amis).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
        part: u8,
        /// Dossier `~/.zen/game`.
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// Dossier de la station (`station.conf`) ; défaut `~/.zen/astroportd`.
        #[arg(long)]
        home: Option<PathBuf>,
        /// `swarm.key` de la constellation ; sinon `uplanetname` de station.conf, sinon ORIGIN.
        #[arg(long)]
        swarm_key: Option<PathBuf>,
        /// Écrit le QR en SVG au lieu du texte.
        #[arg(long)]
        svg: bool,
    },
    /// Liste les MULTIPASS hébergés (sans aucun secret).
    List {
        /// Dossier `~/.zen/game`.
        #[arg(long)]
        game_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Liste les plugins chargés (et ceux qui sont refusés, avec la raison).
    List,
    /// Appelle un plugin : `astroportd plugin call resonance '{"fn":"resonance","a":"x","b":"y"}'`.
    Call {
        /// Nom du plugin (fichier sans `.wasm`).
        name: String,
        /// Entrée JSON.
        input: String,
    },
}

/// Options de `pull-webdav`.
#[derive(clap::Args)]
struct PullArgs {
    /// Email ou clé Nostr (64 hex) du compte, hébergé ici.
    account: String,
    #[command(subcommand)]
    action: PullAction,
    /// Base `uplanet.db` d'astroportd (défaut : `~/.zen/astroportd/uplanet.db`).
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Dossier des blobs d'astroportd (défaut : `~/.zen/astroportd/blobs`).
    #[arg(long, global = true)]
    blobs: Option<PathBuf>,
    /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long, global = true)]
    game_dir: Option<PathBuf>,
    /// API RPC d'IPFS Kubo où ranger les fichiers repris ; sans Kubo joignable, ils restent sur disque.
    #[arg(long, global = true, default_value = "http://127.0.0.1:5001")]
    kubo: String,
}

#[derive(Subcommand)]
enum PullAction {
    /// Liste les sources du compte (sans leur mot de passe).
    List,
    /// Enregistre une source après avoir vérifié l'adresse et les identifiants. Le mot de passe se lit sur l'entrée standard.
    Add {
        /// Racine WebDAV, par exemple `https://nextcloud.example/remote.php/dav/files/fred`.
        #[arg(long)]
        url: String,
        /// Identifiant de connexion.
        #[arg(long)]
        user: String,
        /// Lit le mot de passe (d'application de préférence) sur la première ligne de l'entrée standard.
        #[arg(long)]
        password_stdin: bool,
        /// Libellé de la source.
        #[arg(long, default_value = "")]
        label: String,
        /// Dossier distant à reprendre, relatif à la racine (défaut : tout).
        #[arg(long, default_value = "")]
        remote_path: String,
        /// Dossier du uCloud où déposer la reprise, **telle qu'elle est sur le serveur** (défaut : `/Imports/<libellé>`).
        /// Aucun tri par dates : réorganiser le uCloud est le rôle de la fonction de rangement.
        #[arg(long)]
        dest: Option<String>,
        /// Admet une adresse privée (serveur sur le réseau local). Sans cette option, seules les adresses publiques sont jointes.
        #[arg(long)]
        allow_private: bool,
    },
    /// Retire une source (ce qui a déjà été repris reste dans le uCloud).
    Remove {
        /// Identifiant donné par `list`.
        id: String,
    },
    /// Reprend une source (ou toutes) : télécharge ce qui manque, chiffre, range.
    Run {
        /// Identifiant d'une source (défaut : toutes).
        id: Option<String>,
        /// Compte et n'écrit rien.
        #[arg(long)]
        dry_run: bool,
        /// Fichiers repris au plus pendant ce passage (0 : aucune borne).
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Plafond par fichier, en Mo.
        #[arg(long, default_value_t = 200)]
        max_file_mb: u64,
    },
}

/// Options de `pool-status`.
#[derive(clap::Args)]
struct PoolStatusArgs {
    /// Dossier de blobs commun.
    #[arg(long)]
    blobs: PathBuf,
    /// Base `uplanet.db` d'un plan (répétable : une par plan qui partage le bassin).
    #[arg(long = "db", required = true)]
    dbs: Vec<PathBuf>,
    /// Supprime les blobs orphelins de plus de 24 h (un blob tout récent peut attendre d'être enregistré).
    #[arg(long)]
    sweep: bool,
}

/// Options de `bridge-copy`.
#[derive(clap::Args)]
struct BridgeCopyArgs {
    /// Base `uplanet.db` du plan source (lue en lecture seule).
    #[arg(long)]
    from_db: PathBuf,
    /// Email ou clé Nostr (64 hex) du compte source.
    #[arg(long)]
    from: String,
    /// Base `uplanet.db` du plan de destination.
    #[arg(long)]
    to_db: PathBuf,
    /// Email ou clé Nostr (64 hex) du compte de destination.
    #[arg(long)]
    to: String,
    /// Bassin de blobs commun aux deux plans.
    #[arg(long)]
    blobs: PathBuf,
    /// Dossier (ou fichier) à adopter ; `/` pour tout le uCloud.
    #[arg(long, default_value = "/")]
    path: String,
    /// Dossier de destination, où le chemin source est reproduit.
    #[arg(long, default_value = "/Imports")]
    dest: String,
    /// Atteste que les deux comptes sont **la même personne** : ils partageront les blobs et leurs clés de fichier.
    #[arg(long)]
    same_person: bool,
    /// Compte et vérifie, sans rien écrire.
    #[arg(long)]
    dry_run: bool,
}

/// Options de `import-ucloud`.
#[derive(clap::Args)]
struct ImportUcloudArgs {
    /// Email ou clé Nostr (64 hex) du compte, hébergé ici.
    account: String,
    /// Dossier `.ucloud` d'Astroport.ONE (défaut : `<game-dir>/nostr/<email>/.ucloud`).
    #[arg(long)]
    legacy_dir: Option<PathBuf>,
    /// Base `uplanet.db` d'astroportd (défaut : `~/.zen/astroportd/uplanet.db`).
    #[arg(long)]
    db: Option<PathBuf>,
    /// Dossier des blobs d'astroportd (défaut : `~/.zen/astroportd/blobs`).
    #[arg(long)]
    blobs: Option<PathBuf>,
    /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long)]
    game_dir: Option<PathBuf>,
    /// API RPC d'IPFS Kubo, où vivent les blobs d'Astroport.ONE.
    #[arg(long, default_value = "http://127.0.0.1:5001")]
    kubo: String,
    /// Lit, contrôle et compte, sans rien écrire dans astroportd.
    #[arg(long)]
    dry_run: bool,
    /// Remplace les entrées que le compte aurait déjà dans astroportd.
    #[arg(long)]
    replace: bool,
    /// Fichiers relus, déchiffrés et comparés (taille, SHA-256) avant d'écrire ; 0 pour ne rien contrôler.
    #[arg(long, default_value_t = 60)]
    verify: usize,
    /// Ne publie pas le manifeste Nostr (sauvegarde et réplication) à la fin.
    #[arg(long)]
    no_publish: bool,
}

/// Options communes aux commandes `media`.
#[derive(clap::Args)]
struct MediaArgs {
    #[command(subcommand)]
    action: MediaAction,
    /// Email ou clé Nostr (64 hex) du compte titulaire.
    #[arg(long, global = true)]
    account: Option<String>,
    /// Base `uplanet.db`.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Dossier des blobs chiffrés.
    #[arg(long, global = true)]
    blobs: Option<PathBuf>,
    /// Dossier `~/.zen/game` (défaut : `$ASTRO_GAME_DIR`, sinon `$HOME/.zen/game`).
    #[arg(long, global = true)]
    game_dir: Option<PathBuf>,
    /// API RPC d'IPFS Kubo ; sans elle les blobs restent sur disque.
    #[arg(long, global = true)]
    kubo: Option<String>,
    /// Relais où publier (répétable) ; sans relais, l'événement reste sur le relai local de la base.
    #[arg(long = "relay", global = true)]
    relays: Vec<String>,
}

/// Les options d'un seul `Add` : une commande de ligne de commande, jamais dans une boucle chaude.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum MediaAction {
    /// Atteste, une fois, que les œuvres déposées sont des copies pour l'usage privé du titulaire (kind 31903).
    Consent {
        /// Affiche le texte légal et sort sans rien signer.
        #[arg(long)]
        show: bool,
    },
    /// Dépose un film ou un épisode, puis publie l'événement vidéo.
    Add {
        /// Fichier vidéo (plafond 1 024 Mo).
        #[arg(long)]
        file: PathBuf,
        /// Document TMDB (JSON : title/name, release_date, genres, series_name, season_number, episode_number…).
        #[arg(long)]
        tmdb_json: Option<PathBuf>,
        /// Titre (si pas de document TMDB).
        #[arg(long)]
        title: Option<String>,
        /// Année de sortie.
        #[arg(long)]
        year: Option<u32>,
        /// Genre (répétable).
        #[arg(long = "genre")]
        genres: Vec<String>,
        /// Nom de la série (fait de ce dépôt un épisode ; exige --season et --episode).
        #[arg(long)]
        series: Option<String>,
        /// Saison.
        #[arg(long)]
        season: Option<u32>,
        /// Épisode.
        #[arg(long)]
        episode: Option<u32>,
        /// Titre de l'épisode.
        #[arg(long)]
        episode_name: Option<String>,
        /// `auto` : réencode en H.264/AAC avant le dépôt ; `notify` : dépose tel quel et recommande l'encodage.
        #[arg(long, default_value = "auto")]
        transcode: String,
        /// Chemin de `ffmpeg`.
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: String,
        /// Chemin de `ffprobe`.
        #[arg(long, default_value = "ffprobe")]
        ffprobe: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erreur : {e}");
            ExitCode::FAILURE
        }
    }
}

type Failure = Box<dyn std::error::Error>;

async fn run(cli: Cli) -> Result<(), Failure> {
    match cli.command {
        Command::Serve(args) => serve::run(*args).await,
        Command::Doctor(args) => doctor::run(args),
        Command::Init(args) => init::init(*args),
        Command::Ylevel(args) => init::ylevel_cmd(args),
        Command::Token { account, game_dir, db } => {
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let found = station.discovered.as_ref().ok_or("identités locales introuvables (--game-dir)")?;
            let m = found.multipasses.iter().find(|m| m.email.eq_ignore_ascii_case(&account) || m.hex == account.to_ascii_lowercase()).ok_or_else(|| format!("compte « {account} » non hébergé ici"))?;
            let db_path = db.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zen/astroportd/uplanet.db"))).filter(|p| p.is_file());
            let token = match db_path {
                Some(path) => astro_dav::auth::account_token(&Db::open(&path, 1)?, &station.secret, &m.hex).await,
                None => astro_dav::auth::derive_dav_token(&station.secret, &m.hex), // aucune base : génération 0
            };
            println!("identifiant : {}\njeton       : {token}", m.email);
            Ok(())
        }
        Command::Plugin { dir, action } => {
            let (host, report) = astro_plugins::PluginHost::load_dir(&dir, astro_plugins::Limits::default())?;
            for (path, why) in &report.rejected {
                eprintln!("plugin refusé : {} : {why}", path.display());
            }
            match action {
                PluginAction::List => {
                    for p in host.list() {
                        println!("{:<20} v{:<8} {:>7} octets  {}", p.name, p.version, p.size, p.description);
                    }
                    Ok(())
                }
                PluginAction::Call { name, input } => {
                    println!("{}", host.call(&name, &input)?);
                    Ok(())
                }
            }
        }
        Command::Media(args) => media_cmd(*args).await,
        Command::ImportUcloud(args) => import_ucloud_cmd(*args).await,
        Command::PullWebdav(args) => pull_webdav_cmd(*args).await,
        Command::BridgeCopy(args) => bridge_copy_cmd(*args).await,
        Command::PoolStatus(args) => pool_status_cmd(*args),
        Command::Multipass { action } => multipass_cmd(action).await,
        Command::Route { service, mode, swarm_dir, lat, lon } => route(&service, mode, swarm_dir, lat.zip(lon)),
        Command::SwarmKey => {
            print!("{}", astro_p2p::generate_swarm_key()?);
            Ok(())
        }
        Command::FaceCheck { models } => face_check(models),
        Command::Replay { db, page_size, from, hosted, rounds, blobs, secrets, game_dir } => {
            let db = Arc::new(Db::open(&db, 2)?);
            if let Some(relay) = from {
                for key in &hosted {
                    if !astro_nostr_proto::is_hex64(key) {
                        return Err(format!("clé hébergée invalide : {key}").into());
                    }
                    db.writer().batch(vec![Projector::mark_hosted(key, "multipass")]).await?;
                }
                let mut client = WsClient::new(Duration::from_secs(30));
                if let Some(signer) = identity::resolve(game_dir.as_deref(), false)?.signer {
                    client = client.with_signer(signer);
                }
                let config = CrawlConfig { relays: vec![relay], ..CrawlConfig::default() };
                let crawl = Crawler::new(db.clone(), client, config).run_until_stable(&hosted, rounds).await?;
                println!("crawl : {} clés, {} reçus, {} nouveaux, {} doublons, {} refusés", crawl.keys, crawl.fetched, crawl.inserted, crawl.duplicates, crawl.rejected);
                for (relay, key, reason) in &crawl.errors {
                    eprintln!("  échec {relay} / {key} : {reason}");
                }
            } else if !hosted.is_empty() {
                return Err("--hosted n'a de sens qu'avec --from".into());
            }
            let stats = match (blobs, secrets) {
                (Some(blobs), secrets) => {
                    let secrets = match secrets {
                        Some(dir) => dir,
                        None => identity::resolve(game_dir.as_deref(), true)?.nostr_dir.ok_or("dossier des comptes introuvable : indiquer --secrets")?,
                    };
                    let store = FsBlobStore::open(blobs)?;
                    replay_with_ucloud(&db, page_size, &store, &FileKeySource::new(secrets)).await?
                }
                _ => replay_local(&db, page_size).await?,
            };
            println!("replay : {} examinés, {} projetés, {} ignorés", stats.scanned, stats.projected, stats.skipped);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let restored = astro_lifecycle::rebuild::rebuild_all(&db, now).await?;
            if restored > 0 {
                println!("cycle de vie : {restored} comptes rétablis d'après leurs loyers constatés");
            }
            if stats.manifests + stats.manifests_skipped > 0 {
                println!("uCloud : {} manifestes restaurés, {} ignorés", stats.manifests, stats.manifests_skipped);
            }
            for (id, reason) in &stats.errors {
                eprintln!("  {id} : {reason}");
            }
            db.writer().shutdown().await?;
            Ok(())
        }
        Command::Dav { db, blobs, secrets, game_dir, listen, mount, public_url, debounce_secs, kubo, face_engine, relays } => {
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let secret = station.secret.clone();
            let secrets = secrets.or(station.nostr_dir.clone()).ok_or("dossier des comptes introuvable : indiquer --secrets")?;
            let replay = Arc::new(SqliteReplayStore::open(&db)?);
            let db = Arc::new(Db::open(&db, 4)?);
            let blobs = Arc::new(AutoBlobStore::new(kubo.as_deref().map(KuboBlobStore::new), FsBlobStore::open(blobs)?));
            let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(secrets));
            let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(debounce_secs));
            let mut config = DavConfig::new(secret);
            config.mount = mount;
            config.public_base_url = public_url;
            let (stop, stop_rx) = tokio::sync::watch::channel(false);
            let mut on_upload = None;
            let mut worker = None;
            if let Some(engine_url) = face_engine {
                let mut client = WsClient::new(Duration::from_secs(30));
                if let Some(signer) = station.signer.clone() {
                    client = client.with_signer(signer);
                }
                let engine = Arc::new(HttpFaceEngine::new(&engine_url, Duration::from_secs(120)));
                let outbox = Arc::new(RelayOutbox::new(client, relays));
                let face = Arc::new(FaceCloud::new(db.clone(), blobs.clone(), engine, outbox, keys, Some(publisher.clone()), FaceConfig::default()));
                on_upload = Some(face.upload_hook());
                worker = Some(tokio::spawn(face.run_worker(Duration::from_secs(5), stop_rx)));
                println!("FaceCloud actif (moteur {engine_url})");
            }
            let state = Arc::new(DavState { db: db.clone(), blobs, replay, publisher: Some(publisher.clone()), config, on_upload });
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            println!("WebDAV à l'écoute sur http://{}", listener.local_addr()?);
            tokio::select! {
                served = axum::serve(listener, astro_dav::router(state)) => served?,
                _ = tokio::signal::ctrl_c() => {}
            }
            let _ = stop.send(true);
            if let Some(w) = worker {
                let _ = w.await;
            }
            let failures = publisher.flush().await; // dernière publication avant l'arrêt
            for (owner, reason) in failures {
                eprintln!("manifeste non publié pour {owner} : {reason}");
            }
            db.writer().shutdown().await?;
            Ok(())
        }
        Command::Relay { db, listen, relay_url, trust_forwarded_for, circle_refresh_secs } => {
            let db = Arc::new(Db::open(&db, 4)?);
            let circle = Arc::new(DbCircle::new());
            circle.refresh(&db).await?;
            let _refresher = circle.spawn_refresher(db.clone(), Duration::from_secs(circle_refresh_secs.max(1)));
            let config = RelayConfig {
                relay_url,
                trust_forwarded_for,
                projection: Some(Arc::new(Projector::plan_lossy)),
                ..RelayConfig::default()
            };
            let relay = Relay::new(db.clone(), circle, config);
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            println!("relai à l'écoute sur ws://{}", listener.local_addr()?);
            tokio::select! {
                served = serve(listener, relay) => served?,
                _ = tokio::signal::ctrl_c() => {}
            }
            db.writer().shutdown().await?;
            Ok(())
        }
    }
}

/// `astroportd route` : lit les `12345.json` des pairs et affiche le nœud retenu.
fn route(service: &str, mode: astro_p2p::routing::Mode, swarm_dir: Option<PathBuf>, origin: Option<(f64, f64)>) -> Result<(), Failure> {
    use astro_p2p::routing::{Candidate, random_u64, select};
    let dir = match swarm_dir {
        Some(d) => d,
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME introuvable : indiquer --swarm-dir")?).join(".zen/tmp/swarm"),
    };
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{} : {e}", dir.display()))?.flatten() {
        let file = entry.path().join("12345.json");
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(json) => candidates.push(Candidate::from_station_json(&entry.file_name().to_string_lossy(), &json)),
            Err(e) => eprintln!("route : {} ignoré : {e}", file.display()),
        }
    }
    let Some(sel) = select(&candidates, service, mode, origin, random_u64()) else {
        return Err(format!("aucun nœud ne fournit « {service} » ({} pair(s) lus dans {})", candidates.len(), dir.display()).into());
    };
    println!("{}", sel.node.node_id);
    let extra = sel.distance_km.map(|d| format!(", à {d:.0} km")).unwrap_or_default();
    eprintln!("route : {service} → Power-Score {:.1}{extra} (mode {:?})", sel.node.power_score, sel.mode);
    if sel.fell_back {
        eprintln!("route : ⚠ mode {mode:?} inapplicable (coordonnées inconnues, --lat/--lon) : repli sur le Power-Score");
    }
    Ok(())
}

/// `astroportd multipass …`.
async fn multipass_cmd(action: MultipassAction) -> Result<(), Failure> {
    match action {
        MultipassAction::Recover { card, lang, game_dir, home, swarm_key, db, relays, rounds, json } => {
            let home = station_home(home)?;
            let uplanet_name = station_uplanet_name(&home, swarm_key)?;
            let card = astro_lifecycle::share::Recovery::parse(&read_card_text(&card)?)?;
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let nostr_dir = station.nostr_dir.clone().ok_or("dossier des comptes introuvable (--game-dir)")?;
            let db_path = db.unwrap_or_else(|| home.join("uplanet.db"));
            let db = Arc::new(Db::open(&db_path, 1)?);
            let secret = station.secret.clone();
            let registrar = astroportd::register::Registrar::new(db.clone(), nostr_dir, uplanet_name, Arc::new(move |hex| astro_dav::auth::derive_dav_token(&secret, hex)), astroportd::register::Limits { open: true, max_accounts: 0, per_ip_per_hour: 0 });
            let reg = registrar.recover(None, &card, &lang).await?;
            // Rapatriement immédiat des événements du compte (contacts, DID, manifeste) ; sinon le crawl du démon s'en charge.
            if !relays.is_empty() {
                let mut client = WsClient::new(Duration::from_secs(30));
                if let Some(signer) = station.signer {
                    client = client.with_signer(signer);
                }
                let crawl = Crawler::new(db.clone(), client, CrawlConfig { relays, ..CrawlConfig::default() }).run_until_stable(std::slice::from_ref(&reg.account.hex), rounds).await?;
                eprintln!("crawl : {} clés, {} reçus, {} nouveaux, {} doublons, {} refusés", crawl.keys, crawl.fetched, crawl.inserted, crawl.duplicates, crawl.rejected);
                for (relay, key, reason) in &crawl.errors {
                    eprintln!("  échec {relay} / {key} : {reason}");
                }
            }
            db.writer().shutdown().await?;
            let a = &reg.account;
            if json {
                println!("{}", serde_json::json!({ "email": a.email, "token": reg.token, "pass": a.pass, "npub": a.npub, "hex": a.hex, "g1pub": a.g1_ss58, "nostrns": a.nostrns, "constellation": reg.constellation, "restoring": true }));
            } else {
                println!("MULTIPASS {} rétabli sur la constellation {} (même clé : {})\n", a.email, reg.constellation, a.hex);
                println!("  Identifiant web : {}\n  Jeton web       : {}\n  NOUVEAU PASS    : {}", a.email, reg.token, a.pass);
                println!("\n⚠ Notez ce nouveau PASS : l'ancien n'est pas récupérable.");
                println!("Le démon (`astroportd serve`) va maintenant retrouver vos amis, annoncer cette station comme votre nouvelle maison et récupérer vos fichiers chez vos proches : ils n'ont rien à faire.");
                println!("Ne modifiez rien tant que la restauration n'est pas terminée ; vous serez prévenu.");
            }
            Ok(())
        }
        MultipassAction::Card { account, part, game_dir, home, swarm_key, svg } => {
            let home = station_home(home)?;
            let uplanet_name = station_uplanet_name(&home, swarm_key)?;
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let nostr_dir = station.nostr_dir.ok_or("dossier des comptes introuvable (--game-dir)")?;
            let email = astro_lifecycle::multipass::normalize_email(&account)?;
            let dir = nostr_dir.join(&email);
            // La part demandée est lue dans le dossier du compte ; un compte plus ancien n'en a pas encore : on la crée une fois (le
            // même découpage sert ensuite, pour que P1 imprimé et P3 chez les amis restent compatibles).
            if astro_lifecycle::share::read_share(&dir, 1).is_none() {
                let shares = astro_lifecycle::share::split_card(&astro_lifecycle::multipass::SecretCard::from_dir(&dir, &uplanet_name)?)?;
                astro_lifecycle::share::write_shares(&dir, &shares)?;
            }
            let text = astro_lifecycle::share::read_share(&dir, part).ok_or_else(|| format!("part P{part} introuvable pour {email}"))?.to_text();
            if svg {
                println!("{}", astro_ui::qr::svg(&text).ok_or("carte trop longue pour un QR")?);
            } else {
                println!("{text}");
            }
            Ok(())
        }
        MultipassAction::List { game_dir } => {
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let found = station.discovered.as_ref().ok_or("identités locales introuvables (--game-dir)")?;
            for m in &found.multipasses {
                println!("{:<40} {}  {}", m.email, m.hex, if found.captain_email.as_deref() == Some(m.email.as_str()) { "Capitaine" } else { "" });
            }
            for p in &found.problems {
                eprintln!("⚠ {} : {}", p.path.display(), p.what);
            }
            println!("{} MULTIPASS hébergé(s)", found.multipasses.len());
            Ok(())
        }
        MultipassAction::Create { email, lang, game_dir, home, swarm_key, db, relays, captain: make_captain, json } => {
            let home = station_home(home)?;
            let uplanet_name = station_uplanet_name(&home, swarm_key)?;
            let station = identity::resolve(game_dir.as_deref(), true)?;
            let nostr_dir = station.nostr_dir.clone().ok_or("dossier des comptes introuvable (--game-dir)")?;
            let db_path = db.unwrap_or_else(|| home.join("uplanet.db"));
            let db = Arc::new(Db::open(&db_path, 1)?);
            let secret = station.secret.clone();
            let captain = station.discovered.as_ref().and_then(|d| d.captain()).map(|m| m.hex.clone());
            let player_file = nostr_dir.parent().map(|g| g.join("players/.current/.player"));
            if make_captain {
                let wanted = astro_lifecycle::multipass::normalize_email(&email)?;
                let current = player_file.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty());
                if current.as_deref().is_some_and(|c| c != wanted) {
                    return Err(format!("la station a déjà un Capitaine ({}) : --captain refusé", current.unwrap_or_default()).into());
                }
            }
            let registrar = astroportd::register::Registrar::new(db.clone(), nostr_dir, uplanet_name, Arc::new(move |hex| astro_dav::auth::derive_dav_token(&secret, hex)), astroportd::register::Limits { open: true, max_accounts: 0, per_ip_per_hour: 0 }).with_captain(captain);
            let reg = registrar.register(None, &email, &lang).await?;
            if make_captain {
                let file = player_file.ok_or("dossier de la station introuvable")?;
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&file, format!("{}\n", reg.account.email))?;
                db.writer().execute("UPDATE identities SET role = 'captain' WHERE hex = ?1", vec![rusqlite::types::Value::Text(reg.account.hex.clone())]).await?;
                println!("⚓ {} est le Capitaine de cette station ({}).", reg.account.email, file.display());
            }
            // Publication directe (pas de démon ici) : les événements de naissance partent vers les relais demandés.
            let mut failures = 0;
            if !relays.is_empty() {
                let mut client = WsClient::new(Duration::from_secs(30));
                if let Some(signer) = station.signer {
                    client = client.with_signer(signer);
                }
                let events = stored_birth_events(&db, &reg.account.hex).await?;
                for relay in &relays {
                    for e in &events {
                        match client.publish(relay, e).await {
                            Ok((true, _)) => {}
                            Ok((false, why)) => {
                                failures += 1;
                                eprintln!("refusé par {relay} : {why}");
                            }
                            Err(err) => {
                                failures += 1;
                                eprintln!("échec sur {relay} : {err}");
                            }
                        }
                    }
                }
            }
            db.writer().shutdown().await?;
            let a = &reg.account;
            if json {
                println!("{}", serde_json::json!({ "email": a.email, "token": reg.token, "pass": a.pass, "salt": &*a.salt, "pepper": &*a.pepper, "nsec": &*a.nsec, "npub": a.npub, "hex": a.hex, "g1pub": a.g1_ss58, "nostrns": a.nostrns, "constellation": reg.constellation, "card": reg.card }));
            } else {
                println!("MULTIPASS {} créé sur la constellation {}\n", a.email, reg.constellation);
                println!("  Identifiant web : {}\n  Jeton web       : {}\n  Code PASS       : {}", a.email, reg.token, a.pass);
                println!("  nsec            : {}\n  npub            : {}\n  Ğ1              : {}\n  IPNS            : {}", &*a.nsec, a.npub, a.g1_ss58, a.nostrns);
                println!("  Clé de secours  : {}", reg.card);
                println!("\n⚠ Imprimez la clé de secours en QR (`astroportd multipass card {} --svg`) et gardez-la hors ligne : seule, elle ne révèle rien ; avec la part de l'un de vos amis qui héberge votre sauvegarde, elle rétablit le compte sur une autre station. Le SALT et le PEPPER ne sont plus affichés (ils restent dans le dossier du compte, sous `0600`, et dans `--json`).", a.email);
                println!("  Dossier du compte : {}", a.dir.display());
            }
            if failures > 0 { Err(format!("{failures} publication(s) en échec (le compte est créé)").into()) } else { Ok(()) }
        }
    }
}

/// Dossier de la station : `--home`, sinon `~/.zen/astroportd`.
fn station_home(home: Option<PathBuf>) -> Result<PathBuf, Failure> {
    Ok(home.map_or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zen/astroportd")).ok_or("HOME introuvable : indiquer --home"), Ok)?)
}

/// `UPLANETNAME` de la station : `swarm.key` donné, sinon `uplanetname` de `station.conf`, sinon ORIGIN (64 zéros).
fn station_uplanet_name(home: &std::path::Path, swarm_key: Option<PathBuf>) -> Result<String, Failure> {
    Ok(match swarm_key {
        Some(path) => std::fs::read_to_string(&path).map_err(|e| format!("{} : {e}", path.display()))?.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned).ok_or("swarm.key vide")?,
        None => std::fs::read_to_string(home.join("station.conf"))
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("uplanetname").and_then(|v| v.trim_start().strip_prefix('=')).map(|v| v.trim().to_owned())))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| astroportd::init::ORIGIN_UPLANETNAME.to_owned()),
    })
}

/// Texte d'une carte secrète : donné tel quel, lu sur l'entrée standard (`-`) ou dans un fichier (`@chemin`).
fn read_card_text(arg: &str) -> Result<String, Failure> {
    if arg == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        Ok(text)
    } else if let Some(path) = arg.strip_prefix('@') {
        Ok(std::fs::read_to_string(path).map_err(|e| format!("{path} : {e}"))?)
    } else {
        Ok(arg.to_owned())
    }
}

/// Événements de naissance déjà stockés pour `hex`, à publier tels quels (mêmes identifiants que dans la base).
async fn stored_birth_events(db: &Db, hex: &str) -> Result<Vec<astro_nostr_proto::Event>, Failure> {
    let hex = hex.to_owned();
    let rows: Vec<(String, String, i64, u32, String, String, String)> = db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare("SELECT id, pubkey, created_at, kind, tags, content, sig FROM nostr_events WHERE pubkey = ?1 ORDER BY created_at, kind")?;
            let rows = stmt.query_map([&hex], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    rows.into_iter()
        .map(|(id, pubkey, at, kind, tags, content, sig)| Ok(astro_nostr_proto::Event { id, pubkey, created_at: u64::try_from(at).unwrap_or(0), kind, tags: serde_json::from_str(&tags)?, content, sig }))
        .collect()
}

/// `astroportd media …` : copie privée et dépôt d'un film ou d'un épisode.
fn pool_status_cmd(args: PoolStatusArgs) -> Result<(), Failure> {
    let PoolStatusArgs { blobs, dbs, sweep } = args;
    let st = astro_dav::pool::status(&blobs, &dbs)?;
    let mo = |n: u64| format!("{} Mo", n >> 20);
    println!("bassin {} : {} blob(s), {}", blobs.display(), st.blobs, mo(st.bytes));
    for (db, n, bytes) in &st.per_plane {
        println!("  {} : {n} blob(s) référencé(s), {}", db.display(), mo(*bytes));
    }
    println!("partagés entre plusieurs plans : {} blob(s), {} (place économisée : ces octets ne sont écrits qu'une fois)", st.shared_blobs, mo(st.shared_bytes));
    let orphan_bytes: u64 = st.orphans.iter().map(|o| o.1).sum();
    println!("orphelins (à aucun plan) : {} blob(s), {}", st.orphans.len(), mo(orphan_bytes));
    if sweep {
        let mut freed = 0u64;
        let mut kept = 0u64;
        for (path, size, age) in &st.orphans {
            if *age >= 86_400 {
                std::fs::remove_file(path).map_err(|e| format!("{} : {e}", path.display()))?;
                freed += size;
            } else {
                kept += 1;
            }
        }
        println!("nettoyage : {} libérés, {kept} orphelin(s) trop récent(s) gardé(s)", mo(freed));
    }
    Ok(())
}

async fn bridge_copy_cmd(args: BridgeCopyArgs) -> Result<(), Failure> {
    let BridgeCopyArgs { from_db, from, to_db, to, blobs, path, dest, same_person, dry_run } = args;
    if !same_person {
        return Err("partager des blobs et leurs clés entre deux comptes demande un accord explicite : ajoutez --same-person pour attester que ce sont les deux comptes d'une même personne".into());
    }
    let (from_owner, to_owner) = (astroportd::bridge::resolve_owner(&from_db, &from)?, astroportd::bridge::resolve_owner(&to_db, &to)?);
    let dst = Db::open(&to_db, 2)?;
    let pool = astro_dav::FsBlobStore::open(&blobs)?;
    let config = astro_dav::DavConfig::new(Vec::new());
    let opts = astroportd::bridge::Options { from_owner, to_owner, path, dest, dry_run };
    let r = astroportd::bridge::adopt(&from_db, &dst, &pool, &config, &opts).await?;
    println!("{} : {} fichier(s) adopté(s) sans copie ({} Mo), {} déjà là, {} en conflit, {} de l'ancien format (Kubo), {} absent(s) du bassin, {} refusé(s) par le quota", if dry_run { "à blanc" } else { "adoption" }, r.adopted, r.bytes >> 20, r.same, r.conflicts, r.legacy, r.missing, r.quota);
    if r.missing > 0 || r.legacy > 0 {
        println!("les fichiers de l'ancien format ou absents du bassin ne sont jamais copiés en silence : leur transfert demande un ré-chiffrement");
    }
    Ok(())
}

async fn pull_webdav_cmd(args: PullArgs) -> Result<(), Failure> {
    use astroportd::webdav_pull as pull;
    let PullArgs { account, action, db, blobs, game_dir, kubo } = args;
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("$HOME introuvable")?;
    let (db_path, blobs_path) = (db.unwrap_or_else(|| home.join(".zen/astroportd/uplanet.db")), blobs.unwrap_or_else(|| home.join(".zen/astroportd/blobs")));
    let station = identity::resolve(game_dir.as_deref(), true)?;
    let found = station.discovered.as_ref().ok_or("identités locales introuvables (--game-dir)")?;
    let owner = found.multipasses.iter().find(|m| m.email.eq_ignore_ascii_case(&account) || m.hex == account.to_ascii_lowercase()).ok_or_else(|| format!("compte « {account} » non hébergé ici"))?;
    let secrets_dir = station.nostr_dir.clone().ok_or("dossier des comptes introuvable (--game-dir)")?;
    let db = Arc::new(Db::open(&db_path, 2)?);
    db.writer().batch(astro_lifecycle::store::identity_statements(found)).await?;
    let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(&secrets_dir));
    let sources = pull::Sources::new(db.clone(), keys.clone());

    match action {
        PullAction::List => {
            let all = sources.list(&owner.hex).await?;
            if all.is_empty() {
                println!("aucune source pour {}", owner.email);
            }
            for s in &all {
                println!("{}  {}  {} (identifiant {}) dossier distant « {} » → {}{}", s.id, s.label, s.url, s.username, s.remote_path, s.dest_prefix, if s.allow_private { "  [adresses privées admises]" } else { "" });
            }
        }
        PullAction::Add { url, user, password_stdin, label, remote_path, dest, allow_private } => {
            if !password_stdin {
                return Err("le mot de passe se donne sur l'entrée standard : ajoutez --password-stdin (jamais en argument, il serait visible dans la liste des processus)".into());
            }
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let password = line.trim_end_matches(['\r', '\n']).to_owned();
            let name = if label.trim().is_empty() { url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| "source".into()) } else { label.clone() };
            let dest = dest.unwrap_or_else(|| format!("/Imports/{}", astro_dav::ingest::sanitize_name(&name).unwrap_or_else(|| "source".into())));
            let draft = pull::Source { id: String::new(), label, url, username: user, password, remote_path, dest_prefix: dest, allow_private, created_at: 0 };
            let mut probe = draft.clone();
            probe.dest_prefix = "/Verification".into();
            let seen = pull::check(&probe).await.map_err(|e| format!("source refusée : {e}"))?;
            let saved = sources.add(&owner.hex, draft).await?;
            println!("source {} enregistrée : {seen} entrée(s) au premier niveau du dossier distant. Lancer : astroportd pull-webdav {} run {}", saved.id, owner.email, saved.id);
        }
        PullAction::Remove { id } => {
            sources.remove(&owner.hex, &id).await?;
            println!("source {id} retirée");
        }
        PullAction::Run { id, dry_run, limit, max_file_mb } => {
            let all = sources.list(&owner.hex).await?;
            let chosen: Vec<_> = all.iter().filter(|s| id.as_deref().is_none_or(|i| s.id == i)).collect();
            if chosen.is_empty() {
                return Err(match id {
                    Some(i) => format!("source {i} inconnue (voir `pull-webdav {} list`)", owner.email),
                    None => format!("aucune source pour {} : en ajouter une avec `pull-webdav {} add`", owner.email, owner.email),
                }
                .into());
            }
            let local = Arc::new(AutoBlobStore::new(Some(KuboBlobStore::new(&kubo)), FsBlobStore::open(&blobs_path)?));
            let manifests = ManifestPublisher::spawn(db.clone(), local.clone(), keys.clone(), Duration::from_secs(3600));
            let dav = Arc::new(DavState { db: db.clone(), blobs: local, replay: Arc::new(SqliteReplayStore::open(&db_path)?), publisher: Some(manifests.clone()), config: DavConfig::new(station.secret.clone()), on_upload: None });
            let opts = pull::PullOptions { dry_run, limit, max_file: max_file_mb << 20, stop: None };
            let mut failed = false;
            for s in chosen {
                println!("== {} ({}) → {}{}", s.label, s.url, s.dest_prefix, if dry_run { "  [à blanc]" } else { "" });
                let r = pull::pull(&dav, &owner.hex, s, &opts, |p| {
                    if p.done == 0 && p.total > 0 {
                        println!("   {} fichiers sur le serveur", p.total);
                    } else if p.done > 0 && (p.done % 100 == 0 || p.done == p.total) {
                        println!("   {} / {} : {} repris, {} déjà repris, {} échecs", p.done, p.total, p.imported, p.skipped, p.errors);
                    }
                })
                .await?;
                println!("   bilan : {} vus, {} {}, {} déjà repris ({} modifiés depuis sur le serveur, laissés tels quels), {} trop gros, {} échecs, {:.2} Go{}", r.remote, r.imported, if dry_run { "à reprendre" } else { "repris" }, r.skipped, r.changed, r.too_large, r.errors.len(), r.bytes as f64 / 1e9, if r.capped { " ; parcours borné" } else { "" });
                if !r.errors.is_empty() {
                    failed = true;
                    let list = db_path.parent().map_or_else(|| PathBuf::from("pull-webdav-erreurs.tsv"), |p| p.join("pull-webdav-erreurs.tsv"));
                    let text: String = r.errors.iter().map(|(p, why)| format!("{p}\t{why}\n")).collect();
                    tokio::fs::write(&list, text).await?;
                    println!("   ÉCHECS : liste complète dans {}", list.display());
                    for (p, why) in r.errors.iter().take(10) {
                        println!("     {p} : {why}");
                    }
                }
            }
            for (who, why) in manifests.flush().await {
                eprintln!("manifeste de {who} non publié : {why}");
            }
            if failed {
                return Err("certains fichiers n'ont pas pu être repris (rejouer la commande pour réessayer ceux qui manquent)".into());
            }
        }
    }
    Ok(())
}

async fn import_ucloud_cmd(args: ImportUcloudArgs) -> Result<(), Failure> {
    use astroportd::legacy_ucloud as legacy;
    let ImportUcloudArgs { account, legacy_dir, db, blobs, game_dir, kubo, dry_run, replace, verify, no_publish } = args;
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("$HOME introuvable")?;
    let (db_path, blobs_path) = (db.unwrap_or_else(|| home.join(".zen/astroportd/uplanet.db")), blobs.unwrap_or_else(|| home.join(".zen/astroportd/blobs")));
    let station = identity::resolve(game_dir.as_deref(), true)?;
    let found = station.discovered.as_ref().ok_or("identités locales introuvables (--game-dir)")?;
    let owner = found.multipasses.iter().find(|m| m.email.eq_ignore_ascii_case(&account) || m.hex == account.to_ascii_lowercase()).ok_or_else(|| format!("compte « {account} » non hébergé ici"))?;
    let secrets_dir = station.nostr_dir.clone().ok_or("dossier des comptes introuvable (--game-dir)")?;
    let legacy_dir = legacy_dir.unwrap_or_else(|| secrets_dir.join(&owner.email).join(".ucloud"));

    println!("compte {} ({}), ancien uCloud : {}", owner.email, owner.hex, legacy_dir.display());
    let (manifest, report) = legacy::read_legacy(&legacy_dir.join("ucloud.db"), &owner.hex)?;
    println!("lu : {} fichiers ({:.1} Go), {} dossiers dont {} à créer, {} clés au trousseau", report.files, report.bytes as f64 / 1e9, report.dirs, report.dirs_created, manifest.keyring.len());
    if !report.skipped.is_empty() {
        let list = db_path.parent().map_or_else(|| PathBuf::from("import-ucloud-ecartes.tsv"), |p| p.join("import-ucloud-ecartes.tsv"));
        legacy::write_skipped(&list, &report.skipped).await?;
        println!("ÉCARTÉES : {} entrées (liste complète : {})", report.skipped.len(), list.display());
        for (p, why) in report.skipped.iter().take(10) {
            println!("  {p} : {why}");
        }
    }

    let local = Arc::new(AutoBlobStore::new(Some(KuboBlobStore::new(&kubo)), FsBlobStore::open(&blobs_path)?));
    if verify > 0 {
        let v = legacy::verify_sample(&*local, &manifest, verify).await;
        println!("contrôle : {} fichiers relus, déchiffrés et comparés sur {} demandés", v.checked, verify.min(report.files));
        if !v.failures.is_empty() {
            for (p, why) in v.failures.iter().take(10) {
                println!("  ÉCHEC {p} : {why}");
            }
            return Err(format!("{} fichiers du contrôle ne sont pas lisibles : rien n'a été écrit (kubo atteignable sur {kubo} ?)", v.failures.len()).into());
        }
    }
    if dry_run {
        println!("--dry-run : rien n'a été écrit dans astroportd.");
        return Ok(());
    }

    let db = Arc::new(Db::open(&db_path, 2)?);
    db.writer().batch(astro_lifecycle::store::identity_statements(found)).await?;
    legacy::apply(&db, &manifest, replace).await?;
    println!("importé : {} entrées et {} clés dans {}", manifest.entries.len(), manifest.keyring.len(), db_path.display());
    if no_publish {
        println!("manifeste non publié (--no-publish) : le démon le publiera à la prochaine modification du uCloud.");
    } else {
        let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(&secrets_dir));
        let published = astro_dav::manifest::publish(&db, &*local, &*keys, &owner.hex).await?;
        println!("manifeste publié : révision {}, {} fichiers (événement {})", published.pointer.rev, published.pointer.files, published.event.id);
    }
    println!("Pour lire ces fichiers, `astroportd serve` doit tourner avec --kubo {kubo}.");
    Ok(())
}

async fn media_cmd(args: MediaArgs) -> Result<(), Failure> {
    use astro_nostr_proto::Event;
    let MediaArgs { action, account, db, blobs, game_dir, kubo, relays } = args;
    if let MediaAction::Consent { show: true } = &action {
        println!("{}", astro_dav::media::CONSENT_TEXT);
        return Ok(());
    }
    let account = account.ok_or("--account (email ou clé Nostr) obligatoire")?;
    let (db_path, blobs_path) = (db.ok_or("--db obligatoire")?, blobs.ok_or("--blobs obligatoire")?);
    let station = identity::resolve(game_dir.as_deref(), true)?;
    let found = station.discovered.as_ref().ok_or("identités locales introuvables (--game-dir)")?;
    let owner = found.multipasses.iter().find(|m| m.email.eq_ignore_ascii_case(&account) || m.hex == account.to_ascii_lowercase()).ok_or_else(|| format!("compte « {account} » non hébergé ici"))?.hex.clone();
    let secrets_dir = station.nostr_dir.clone().ok_or("dossier des comptes introuvable (--game-dir)")?;

    let db = Arc::new(Db::open(&db_path, 2)?);
    db.writer().batch(astro_lifecycle::store::identity_statements(found)).await?;
    let local = Arc::new(AutoBlobStore::new(kubo.as_deref().map(KuboBlobStore::new), FsBlobStore::open(&blobs_path)?));
    let keys: Arc<dyn KeySource> = Arc::new(FileKeySource::new(&secrets_dir));
    let manifests = ManifestPublisher::spawn(db.clone(), local.clone(), keys.clone(), Duration::from_secs(3600));
    let dav = Arc::new(DavState { db: db.clone(), blobs: local, replay: Arc::new(SqliteReplayStore::open(&db_path)?), publisher: Some(manifests.clone()), config: DavConfig::new(station.secret.clone()), on_upload: None });
    let activity = astro_core::ActivityBus::new();
    let mut client = WsClient::new(Duration::from_secs(30));
    if let Some(s) = station.signer.clone() {
        client = client.with_signer(s);
    }
    let (ffmpeg, ffprobe, mode) = match &action {
        MediaAction::Add { ffmpeg, ffprobe, transcode, .. } => (ffmpeg.clone(), ffprobe.clone(), match transcode.as_str() {
            "auto" => astroportd::media::Transcode::Auto,
            "notify" => astroportd::media::Transcode::Notify,
            other => return Err(format!("--transcode : « auto » ou « notify », pas « {other} »").into()),
        }),
        MediaAction::Consent { .. } => ("ffmpeg".into(), "ffprobe".into(), astroportd::media::Transcode::Notify),
    };
    let pipeline = astroportd::media::MediaPipeline {
        dav,
        keys,
        publisher: Some(Arc::new(astroportd::media::RelaysPublisher { client, relays })),
        activity: Some(activity.clone()),
        tools: astroportd::media::Tools { ffmpeg, ffprobe, ..Default::default() },
        mode,
        work_dir: db_path.parent().map_or_else(|| PathBuf::from("work"), |p| p.join("work")),
    };
    let mut feed = activity.subscribe(0).1;
    let printer = tokio::spawn(async move {
        while let Ok(a) = feed.recv().await {
            println!("· {}", a.message);
        }
    });
    let result: Result<(), Failure> = async {
        match action {
            MediaAction::Consent { .. } => {
                let ev: Event = pipeline.record_consent(&owner).await?;
                println!("attestation de copie privée enregistrée (événement {})", &ev.id[..12]);
            }
            MediaAction::Add { file, tmdb_json, title, year, genres, series, season, episode, episode_name, .. } => {
                let doc = match tmdb_json {
                    Some(p) => serde_json::from_slice(&std::fs::read(&p).map_err(|e| format!("{} : {e}", p.display()))?)?,
                    None => serde_json::json!({ "title": title, "year": year, "genres": genres, "series_name": series, "season_number": season, "episode_number": episode, "episode_name": episode_name }),
                };
                let meta = astro_dav::media::MediaMeta::from_tmdb(&doc)?;
                let r = pipeline.submit(&owner, &file, &meta).await?;
                println!("déposé : {} ({} octets{}), événement kind {} {}", r.path, r.size, if r.transcoded { ", réencodé H.264/AAC" } else { "" }, r.kind, &r.event_id[..12]);
                for n in &r.notes {
                    println!("note : {n}");
                }
            }
        }
        Ok(())
    }
    .await;
    for (who, why) in manifests.flush().await {
        eprintln!("manifeste non publié pour {who} : {why}");
    }
    printer.abort();
    db.writer().shutdown().await?;
    result
}

/// `astroportd face-check` : dit ce qui manque pour activer l'inférence faciale locale, et le comment.
#[cfg(feature = "onnx")]
fn face_check(models: Option<PathBuf>) -> Result<(), Failure> {
    use astro_facecloud::onnx;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data = home.as_ref().map(|h| h.join(".zen/astroportd"));
    println!("Inférence faciale locale (SCRFD + ArcFace, ONNX Runtime)");
    println!("  ✔ fonction `onnx` compilée dans ce binaire");
    let dir = models.or_else(|| onnx::find_models(data.as_deref(), home.as_deref()));
    match &dir {
        Some(d) => println!("  ✔ modèles : {}", d.display()),
        None => {
            println!("  ✘ modèles introuvables ({} et {} attendus)", onnx::DETECTOR_FILE, onnx::RECOGNIZER_FILE);
            for d in onnx::default_model_dirs(data.as_deref(), home.as_deref()) {
                println!("      cherché : {}", d.display());
            }
            println!("      → placez-les dans {} ou passez --face-models DIR (paquet InsightFace buffalo_l : licence de recherche non commerciale)", data.as_ref().map_or_else(|| "~/.zen/astroportd/models".into(), |d| d.join("models").display().to_string()));
        }
    }
    let lib = onnx::find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), home.as_deref());
    match &lib {
        Some(l) => println!("  ✔ bibliothèque ONNX Runtime : {}", l.display()),
        None => println!("  ✘ libonnxruntime.so introuvable → installez-la (pip install onnxruntime, ou le paquet de votre distribution) ou définissez ORT_DYLIB_PATH"),
    }
    let (Some(d), Some(_)) = (dir, lib) else { return Err("l'inférence locale n'est pas prête (voir ci-dessus)".into()) };
    let started = std::time::Instant::now();
    onnx::OrtFaceEngine::load(&d, 0)?;
    println!("  ✔ modèles chargés en {:.1} s : `astroportd serve ...` les utilise tout seul (sans --face-engine), ou --face-models {}", started.elapsed().as_secs_f32(), d.display());
    Ok(())
}

#[cfg(not(feature = "onnx"))]
fn face_check(_: Option<PathBuf>) -> Result<(), Failure> {
    println!("Inférence faciale locale (SCRFD + ArcFace, ONNX Runtime)");
    println!("  ✘ ce binaire est compilé sans la fonction `onnx` → cargo build --release --features onnx");
    println!("    (ou utilisez un service d'inférence : serve --face-engine http://…)");
    Err("l'inférence locale n'est pas disponible dans ce binaire".into())
}
