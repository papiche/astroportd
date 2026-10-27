//! Rituel **Y-Level** : la clé SSH du Capitaine, la clé IPFS du nœud, le portefeuille Ğ1 et la clé Nostr de la
//! station sont **la même racine**.
//!
//! Chaîne canonique (`Astroport.ONE/tools/Ylevel.sh`) :
//!
//! ```text
//! ~/.ssh/id_ed25519 ── sha512 ──► SECRET1 (hex 1‑64) + SECRET2 (hex 65‑128)
//!        SALT = SECRET1, PEPPER = SECRET2 ── scrypt (Duniter) ──► graine ed25519
//!        graine ed25519 ─┬─► clé SSH jumelle  (même clé publique que le PeerID)
//!                        ├─► PeerID IPFS 12D3Koo…  (= IPFSNODEID)
//!                        ├─► clé publique Ğ1 (base58) et portefeuille `secret.NODE.dunikey`
//!                        └─► clé Nostr sha256(graine) → `secret.nostr`
//! ```
//!
//! Conséquence vérifiable par n'importe qui (`tools/ssh_to_g1ipfs.py`) : la clé publique SSH d'un Capitaine
//! **donne** le PeerID de sa station, donc son accès P2P/SSH de confiance (voir [`ssh_pub_to_peer_id`]).
//!
//! Différence assumée avec le script bash : les secrets sont dérivés **une seule fois**, de la clé SSH d'origine.
//! `Ylevel.sh` les recalcule à chaque exécution à partir de la clé SSH *courante* ; après la transmutation, celle-ci
//! est la clé jumelle, et un second passage réécrit `secret.june` avec d'autres secrets (qui ne correspondent plus au
//! PeerID du nœud). Ici, une fois `secret.june` écrit, il fait foi : on le relit, on ne le recalcule pas.

use std::path::{Path, PathBuf};

use astro_core::kdf::{self, KdfError, Seed};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::{Digest, Sha256, Sha512};
use zeroize::Zeroizing;

use crate::dunikey;

/// Erreurs du rituel.
#[derive(Debug, thiserror::Error)]
pub enum YLevelError {
    /// Dérivation impossible.
    #[error("dérivation : {0}")]
    Kdf(#[from] KdfError),
    /// Fichier illisible ou non inscriptible.
    #[error("{path} : {why}")]
    Io {
        /// Fichier concerné.
        path: PathBuf,
        /// Cause.
        why: String,
    },
    /// Clé publique SSH illisible (attendu : `ssh-ed25519 AAAA… [commentaire]`).
    #[error("clé publique SSH illisible : {0}")]
    BadSshKey(&'static str),
    /// `secret.june` présent mais illisible.
    #[error("secret.june illisible : {0}")]
    BadSecretJune(&'static str),
    /// Hasard système indisponible.
    #[error("hasard système indisponible")]
    Random,
}

fn io(path: &Path, e: impl ToString) -> YLevelError {
    YLevelError::Io { path: path.to_owned(), why: e.to_string() }
}

/// Les deux secrets (`SALT`, `PEPPER`) d'où tout découle.
pub struct Secrets {
    /// `SALT` = 64 premiers caractères hexadécimaux du sha512.
    pub salt: Zeroizing<String>,
    /// `PEPPER` = 64 suivants.
    pub pepper: Zeroizing<String>,
}

impl Secrets {
    /// Secrets tirés du **contenu** du fichier de clé privée SSH (comme `cat id_ed25519 | sha512sum`).
    pub fn from_ssh_private_file(bytes: &[u8]) -> Self {
        let hex = hex::encode(Sha512::digest(bytes));
        Self { salt: Zeroizing::new(hex[..64].to_owned()), pepper: Zeroizing::new(hex[64..].to_owned()) }
    }

    /// Secrets lus dans `secret.june` (`SALT=…; PEPPER=…`).
    pub fn parse_secret_june(text: &str) -> Result<Self, YLevelError> {
        let mut salt = None;
        let mut pepper = None;
        for part in text.split([';', '\n']) {
            match part.trim().split_once('=') {
                Some(("SALT", v)) => salt = Some(v.trim().to_owned()),
                Some(("PEPPER", v)) => pepper = Some(v.trim().to_owned()),
                _ => {}
            }
        }
        match (salt, pepper) {
            (Some(s), Some(p)) if !s.is_empty() && !p.is_empty() => Ok(Self { salt: Zeroizing::new(s), pepper: Zeroizing::new(p) }),
            _ => Err(YLevelError::BadSecretJune("SALT ou PEPPER absent")),
        }
    }

    /// Contenu de `secret.june`.
    pub fn render_secret_june(&self) -> String {
        format!("SALT={}; PEPPER={}\n", &*self.salt, &*self.pepper)
    }

    /// Dérive la chaîne de clés.
    pub fn twin(&self) -> Result<Twin, YLevelError> {
        Ok(Twin { seed: kdf::duniter_seed(&self.salt, &self.pepper)? })
    }
}

/// Les clés jumelles d'une même graine ed25519.
pub struct Twin {
    seed: Seed,
}

impl Twin {
    /// Jumeau d'une graine connue (`secret.NODE.dunikey`).
    pub fn from_seed(seed: Seed) -> Self {
        Self { seed }
    }

    /// PeerID IPFS (`IPFSNODEID`).
    pub fn peer_id(&self) -> String {
        self.seed.ipfs_peer_id()
    }

    /// Clé publique Ğ1 (base58).
    pub fn g1_pubkey(&self) -> String {
        self.seed.g1_pubkey()
    }

    /// Clé publique ed25519 brute.
    pub fn public(&self) -> [u8; 32] {
        self.seed.ed25519_public()
    }

    /// `HEX`, `npub` et contenu de `secret.nostr` (`NSEC=…; NPUB=…; HEX=…`).
    pub fn nostr(&self) -> Result<(String, String, Zeroizing<String>), YLevelError> {
        let keys = self.seed.nostr_keys()?;
        let nsec = keys.nsec()?;
        let npub = keys.npub()?;
        let hex = keys.pubkey_hex();
        let file = Zeroizing::new(format!("NSEC={}; NPUB={npub}; HEX={hex}\n", &*nsec));
        Ok((hex, npub, file))
    }

    /// Contenu de `secret.NODE.dunikey`.
    pub fn dunikey(&self) -> Zeroizing<String> {
        Zeroizing::new(dunikey::render(&self.seed.ed25519_secret_seed()))
    }

    /// Ligne de clé publique OpenSSH (`ssh-ed25519 AAAA… commentaire`).
    pub fn ssh_public(&self, comment: &str) -> String {
        ssh_public_line(&self.public(), comment)
    }

    /// Clé privée OpenSSH (`-----BEGIN OPENSSH PRIVATE KEY-----`), non chiffrée, que `ssh` accepte telle quelle.
    pub fn ssh_private(&self, comment: &str) -> Zeroizing<String> {
        let seed = self.seed.ed25519_secret_seed();
        Zeroizing::new(openssh_private(&seed, &self.public(), comment))
    }

    /// `(PeerID, PrivKey)` au format de `~/.ipfs/config` (`Identity`) : protobuf libp2p `{Type=Ed25519, Data=seed‖pub}`.
    pub fn kubo_identity(&self) -> (String, Zeroizing<String>) {
        let seed = self.seed.ed25519_secret_seed();
        let mut pb = Zeroizing::new(vec![0x08, 0x01, 0x12, 0x40]);
        pb.extend_from_slice(&*seed);
        pb.extend_from_slice(&self.public());
        (self.peer_id(), Zeroizing::new(B64.encode(&*pb)))
    }
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn ssh_public_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(51);
    put_string(&mut blob, b"ssh-ed25519");
    put_string(&mut blob, public);
    blob
}

/// `ssh-ed25519 AAAA… commentaire`.
pub fn ssh_public_line(public: &[u8; 32], comment: &str) -> String {
    let line = format!("ssh-ed25519 {}", B64.encode(ssh_public_blob(public)));
    if comment.is_empty() { line } else { format!("{line} {comment}") }
}

/// Format `openssh-key-v1` sans chiffrement. Le champ de contrôle (`checkint`) est dérivé de la clé : le fichier est
/// reproductible, `ssh-keygen -y` en tire la même clé publique.
fn openssh_private(seed: &[u8; 32], public: &[u8; 32], comment: &str) -> String {
    let check = u32::from_be_bytes(Sha256::digest(public)[..4].try_into().unwrap_or([0; 4]));
    let mut private = Vec::new();
    private.extend_from_slice(&check.to_be_bytes());
    private.extend_from_slice(&check.to_be_bytes());
    put_string(&mut private, b"ssh-ed25519");
    put_string(&mut private, public);
    let mut pair = Zeroizing::new(Vec::with_capacity(64));
    pair.extend_from_slice(seed);
    pair.extend_from_slice(public);
    put_string(&mut private, &pair);
    put_string(&mut private, comment.as_bytes());
    let mut pad = 1u8;
    while private.len() % 8 != 0 {
        private.push(pad);
        pad += 1;
    }
    let mut body = Vec::new();
    body.extend_from_slice(b"openssh-key-v1\0");
    put_string(&mut body, b"none");
    put_string(&mut body, b"none");
    put_string(&mut body, b"");
    body.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut body, &ssh_public_blob(public));
    put_string(&mut body, &private);
    private.fill(0);
    let b64 = B64.encode(&body);
    body.fill(0);
    let mut out = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(70) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    out
}

/// Clé publique ed25519 contenue dans une ligne `ssh-ed25519 AAAA…`.
pub fn ssh_pub_to_ed25519(line: &str) -> Result<[u8; 32], YLevelError> {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("ssh-ed25519") {
        return Err(YLevelError::BadSshKey("seul ssh-ed25519 est reconnu"));
    }
    let blob = B64.decode(parts.next().ok_or(YLevelError::BadSshKey("blob absent"))?).map_err(|_| YLevelError::BadSshKey("base64 invalide"))?;
    // `string "ssh-ed25519"` puis `string pub` : la clé est dans les 32 derniers octets (comme ssh_to_g1ipfs.py).
    if blob.len() != 51 || !blob.starts_with(&[0, 0, 0, 11]) {
        return Err(YLevelError::BadSshKey("blob de longueur inattendue"));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&blob[blob.len() - 32..]);
    Ok(out)
}

/// PeerID IPFS correspondant à une clé publique SSH (équivalent de `tools/ssh_to_g1ipfs.py`).
pub fn ssh_pub_to_peer_id(line: &str) -> Result<String, YLevelError> {
    let public = ssh_pub_to_ed25519(line)?;
    let mut mh = Vec::with_capacity(38);
    mh.extend_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
    mh.extend_from_slice(&public);
    Ok(bs58::encode(mh).into_string())
}

/// Clé publique Ğ1 (base58) correspondant à une clé publique SSH.
pub fn ssh_pub_to_g1(line: &str) -> Result<String, YLevelError> {
    Ok(bs58::encode(ssh_pub_to_ed25519(line)?).into_string())
}

/// Emplacements du rituel.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.zen/game`.
    pub game: PathBuf,
    /// `~/.ssh`.
    pub ssh: PathBuf,
}

impl Paths {
    fn secret_june(&self) -> PathBuf {
        self.game.join("secret.june")
    }
    fn node_dunikey(&self) -> PathBuf {
        self.game.join("secret.NODE.dunikey")
    }
    fn node_nostr(&self) -> PathBuf {
        self.game.join("secret.nostr")
    }
    fn id_ssh_pub(&self) -> PathBuf {
        self.game.join("id_ssh.pub")
    }
    fn ssh_key(&self) -> PathBuf {
        self.ssh.join("id_ed25519")
    }
    fn ssh_pub(&self) -> PathBuf {
        self.ssh.join("id_ed25519.pub")
    }
}

/// D'où viennent les secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `secret.june` existait : il fait foi.
    SecretJune,
    /// Dérivés de la clé SSH existante.
    SshKey,
    /// Aucune clé SSH : une clé d'origine a été tirée au hasard.
    Fresh,
}

/// Ce que `init` a fait.
#[derive(Debug)]
pub struct InitReport {
    /// Origine des secrets.
    pub source: Source,
    /// PeerID de la station (`IPFSNODEID`).
    pub peer_id: String,
    /// Clé publique Ğ1.
    pub g1_pubkey: String,
    /// Clé Nostr de la station (hex).
    pub nostr_hex: String,
    /// Ligne publique SSH jumelle.
    pub ssh_public: String,
    /// Fichiers écrits ou remplacés.
    pub written: Vec<PathBuf>,
    /// Anciennes clés SSH mises à l'abri (jamais détruites).
    pub kept: Vec<PathBuf>,
}

/// Options de [`init`].
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Ne touche pas à `~/.ssh` (la correspondance SSH ↔ IPFS ne sera pas établie).
    pub no_ssh: bool,
    /// Commentaire de la clé SSH jumelle.
    pub comment: String,
    /// `~/.ipfs/config` de kubo à aligner sur le PeerID (sauvegarde `config.bkp` faite avant).
    pub kubo_config: Option<PathBuf>,
}

fn write_secret(path: &Path, content: &str, mode: u32) -> Result<(), YLevelError> {
    let dir = path.parent().ok_or_else(|| io(path, "pas de dossier parent"))?;
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
        let mut f = options.open(&tmp).map_err(|e| io(&tmp, e))?;
        f.write_all(content.as_bytes()).map_err(|e| io(&tmp, e))?;
        f.sync_all().map_err(|e| io(&tmp, e))?;
    }
    #[cfg(unix)]
    std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(mode)).map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))
}

/// Met l'ancienne clé de côté : `origin.key`/`origin.pub` si libres, sinon `<nom>.<horodatage>.bkp`. Rend le nouveau chemin.
fn keep(path: &Path, origin: &Path, stamp: u64) -> Result<PathBuf, YLevelError> {
    let target = if origin.exists() { path.with_file_name(format!("{}.{stamp}.bkp", path.file_name().and_then(|n| n.to_str()).unwrap_or("key"))) } else { origin.to_owned() };
    std::fs::rename(path, &target).map_err(|e| io(&target, e))?;
    Ok(target)
}

fn random_seed() -> Result<Zeroizing<[u8; 32]>, YLevelError> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *seed).map_err(|_| YLevelError::Random)?;
    Ok(seed)
}

/// Exécute le rituel. Idempotent : refait sans effet une station déjà conforme.
///
/// 1. secrets : `secret.june` s'il existe, sinon sha512 de `~/.ssh/id_ed25519`, sinon clé d'origine tirée au hasard ;
/// 2. écrit `secret.june`, `secret.NODE.dunikey`, `secret.nostr` (0600) ;
/// 3. clé SSH jumelle dans `~/.ssh/id_ed25519` ; l'ancienne est conservée (`origin.key`) ;
/// 4. facultatif : identité de kubo alignée sur le PeerID.
pub fn init(paths: &Paths, opts: &InitOptions, now: u64) -> Result<InitReport, YLevelError> {
    let mut written = Vec::new();
    let mut kept = Vec::new();

    let (secrets, source) = if paths.secret_june().is_file() {
        let text = Zeroizing::new(std::fs::read_to_string(paths.secret_june()).map_err(|e| io(&paths.secret_june(), e))?);
        (Secrets::parse_secret_june(&text)?, Source::SecretJune)
    } else if paths.ssh_key().is_file() {
        let bytes = Zeroizing::new(std::fs::read(paths.ssh_key()).map_err(|e| io(&paths.ssh_key(), e))?);
        (Secrets::from_ssh_private_file(&bytes), Source::SshKey)
    } else {
        // Pas de clé SSH : on en crée une d'origine (hasard), exactement comme `ssh-keygen` puis relance de Ylevel.sh.
        let origin = Twin::from_seed(Seed::from_ed25519_seed(*random_seed()?));
        let pem = origin.ssh_private(&opts.comment);
        if !opts.no_ssh {
            write_secret(&paths.ssh_key(), &pem, 0o600)?;
            write_secret(&paths.ssh_pub(), &format!("{}\n", origin.ssh_public(&opts.comment)), 0o644)?;
        }
        (Secrets::from_ssh_private_file(pem.as_bytes()), Source::Fresh)
    };
    let twin = secrets.twin()?;

    if source != Source::SecretJune {
        write_secret(&paths.secret_june(), &secrets.render_secret_june(), 0o600)?;
        written.push(paths.secret_june());
    }
    let (nostr_hex, _, nostr_file) = twin.nostr()?;
    for (path, content) in [(paths.node_dunikey(), twin.dunikey().to_string()), (paths.node_nostr(), nostr_file.to_string())] {
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            write_secret(&path, &content, 0o600)?;
            written.push(path);
        }
    }

    let ssh_public = twin.ssh_public(&opts.comment);
    if !opts.no_ssh {
        let current = std::fs::read_to_string(paths.ssh_pub()).ok().and_then(|l| ssh_pub_to_ed25519(&l).ok());
        if current != Some(twin.public()) {
            // La clé SSH d'origine (si elle existe) est mise à l'abri avant d'être remplacée.
            if paths.ssh_key().is_file() {
                kept.push(keep(&paths.ssh_key(), &paths.ssh.join("origin.key"), now)?);
            }
            if paths.ssh_pub().is_file() {
                kept.push(keep(&paths.ssh_pub(), &paths.ssh.join("origin.pub"), now)?);
            }
            write_secret(&paths.ssh_key(), &twin.ssh_private(&opts.comment), 0o600)?;
            write_secret(&paths.ssh_pub(), &format!("{ssh_public}\n"), 0o644)?;
            written.push(paths.ssh_key());
            written.push(paths.ssh_pub());
        }
        // Lien `~/.zen/game/id_ssh.pub` → clé publique SSH (preuve lue par `_12345.sh` d'Astroport.ONE).
        #[cfg(unix)]
        {
            let link = paths.id_ssh_pub();
            if std::fs::read_link(&link).ok().as_deref() != Some(paths.ssh_pub().as_path()) {
                let _ = std::fs::remove_file(&link);
                std::os::unix::fs::symlink(paths.ssh_pub(), &link).map_err(|e| io(&link, e))?;
                written.push(link);
            }
        }
    }

    if let Some(config) = &opts.kubo_config {
        if apply_kubo_identity(config, &twin)? {
            written.push(config.clone());
        }
    }

    Ok(InitReport { source, peer_id: twin.peer_id(), g1_pubkey: twin.g1_pubkey(), nostr_hex, ssh_public, written, kept })
}

/// Aligne `Identity.PeerID`/`PrivKey` de kubo sur la station. Sauvegarde `config.bkp` (une fois). Rend `true` si modifié.
fn apply_kubo_identity(config: &Path, twin: &Twin) -> Result<bool, YLevelError> {
    let text = Zeroizing::new(std::fs::read_to_string(config).map_err(|e| io(config, e))?);
    let mut json: serde_json::Value = serde_json::from_str(&text).map_err(|e| io(config, e))?;
    let (peer, key) = twin.kubo_identity();
    let identity = json.get_mut("Identity").and_then(serde_json::Value::as_object_mut).ok_or_else(|| io(config, "section Identity absente"))?;
    if identity.get("PeerID").and_then(serde_json::Value::as_str) == Some(peer.as_str()) {
        return Ok(false);
    }
    let backup = config.with_extension("bkp");
    if !backup.exists() {
        write_secret(&backup, &text, 0o600)?;
    }
    identity.insert("PeerID".into(), peer.into());
    identity.insert("PrivKey".into(), (*key).clone().into());
    let out = serde_json::to_string_pretty(&json).map_err(|e| io(config, e))?;
    write_secret(config, &out, 0o600)?;
    Ok(true)
}

/// Une vérification de conformité.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Intitulé.
    pub name: &'static str,
    /// Conforme ?
    pub ok: bool,
    /// Détail (jamais de secret).
    pub detail: String,
}

/// Vérifie la correspondance SSH ↔ PeerID ↔ Ğ1 ↔ Nostr d'une station, sans rien modifier.
pub fn verify(paths: &Paths, kubo_config: Option<&Path>) -> Vec<Check> {
    let mut out = Vec::new();
    let mut check = |name: &'static str, ok: bool, detail: String| out.push(Check { name, ok, detail });

    let dunikey = std::fs::read_to_string(paths.node_dunikey()).map_err(|e| e.to_string()).and_then(|t| dunikey::Dunikey::parse(&t).map_err(|e| e.to_string()));
    let Ok(dk) = dunikey else {
        check("NODE", false, format!("{} illisible : lance `astroportd init`", paths.node_dunikey().display()));
        return out;
    };
    let twin = Twin::from_seed(dk.seed());
    check("NODE", true, format!("IPFSNODEID {}  ·  Ğ1 {}", twin.peer_id(), twin.g1_pubkey()));

    match std::fs::read_to_string(paths.secret_june()).map_err(|e| e.to_string()).and_then(|t| Secrets::parse_secret_june(&t).map_err(|e| e.to_string())).and_then(|s| s.twin().map_err(|e| e.to_string())) {
        Ok(j) if j.peer_id() == twin.peer_id() => check("secret.june", true, "secret.june redonne bien le NODE".into()),
        Ok(j) => check("secret.june", false, format!("secret.june donne {} ≠ NODE {} (rituel rejoué à tort ?)", j.peer_id(), twin.peer_id())),
        Err(e) => check("secret.june", false, e),
    }

    match twin.nostr() {
        Ok((hex, _, _)) => {
            let declared = std::fs::read_to_string(paths.node_nostr()).ok().and_then(|t| t.split([';', '\n']).find_map(|p| p.trim().strip_prefix("HEX=").map(|v| v.trim().to_owned())));
            match declared {
                Some(h) if h == hex => check("Nostr", true, format!("secret.nostr = clé du NODE ({}…)", &hex[..12])),
                Some(h) => check("Nostr", false, format!("secret.nostr ({}…) ≠ clé dérivée du NODE ({}…)", &h[..h.len().min(12)], &hex[..12])),
                None => check("Nostr", false, format!("{} illisible", paths.node_nostr().display())),
            }
        }
        Err(e) => check("Nostr", false, e.to_string()),
    }

    match std::fs::read_to_string(paths.ssh_pub()) {
        Ok(line) => match ssh_pub_to_peer_id(&line) {
            Ok(p) if p == twin.peer_id() => check("SSH", true, "la clé SSH du Capitaine donne l'IPFSNODEID : accès SSH ⇔ identité réseau".into()),
            Ok(p) => check("SSH", false, format!("la clé SSH donne {p}, le NODE est {} : lance `astroportd init` (l'ancienne clé est conservée dans origin.key)", twin.peer_id())),
            Err(e) => check("SSH", false, e.to_string()),
        },
        Err(_) => check("SSH", false, format!("{} absent : lien SSH ⇔ IPFS non établi", paths.ssh_pub().display())),
    }

    if let Some(config) = kubo_config {
        let current = std::fs::read_to_string(config).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|j| j["Identity"]["PeerID"].as_str().map(str::to_owned));
        match current {
            Some(p) if p == twin.peer_id() => check("kubo", true, "l'identité de kubo est celle du NODE".into()),
            Some(p) => check("kubo", false, format!("kubo annonce {p}, le NODE est {} (--kubo-config à l'init)", twin.peer_id())),
            None => check("kubo", false, format!("{} illisible", config.display())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vecteur produit par le legacy : `printf 'fake-openssh-private-key-for-test\n' | sha512sum`, puis
    /// `keygen -t ipfs/duniter/nostr`, `keygen -t ssh` et `ssh_to_g1ipfs.py`.
    const FILE: &[u8] = b"fake-openssh-private-key-for-test\n";
    const SALT: &str = "5cc487d505584c0840d6745b61a43d5ed03cc072922a45541a4d10269f1acc25";
    const PEPPER: &str = "0a3d038212abc881d46e1fffadb6832246b54b3519a644c71a8cda4567c0a3e4";
    const PEER: &str = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";
    const G1: &str = "H9LWyDKDeoiKmDQYmmqsSD8GT2SUYC2SaxCDi9U6hCrL";
    const NPUB: &str = "npub1zd0slxp5p28pwn5ec75vxrylv8n48l0xfcfr3ek6htht4j8fqw9spqdmd2";
    const SSH: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO/dtHTi68xbFKA7GdLNvmGseC/uCK9gVCgY4U6v3Nmp";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astroportd-ylevel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_chain_matches_the_legacy_keygen_vectors() {
        let s = Secrets::from_ssh_private_file(FILE);
        assert_eq!((&**s.salt, &**s.pepper), (SALT, PEPPER));
        let t = s.twin().unwrap();
        assert_eq!(t.peer_id(), PEER);
        assert_eq!(t.g1_pubkey(), G1);
        assert_eq!(t.nostr().unwrap().1, NPUB);
        assert_eq!(t.ssh_public(""), SSH);
        // La clé SSH jumelle donne le PeerID (ssh_to_g1ipfs.py) et la clé Ğ1.
        assert_eq!(ssh_pub_to_peer_id(SSH).unwrap(), PEER);
        assert_eq!(ssh_pub_to_g1(SSH).unwrap(), G1);
        assert!(ssh_pub_to_peer_id("ssh-rsa AAAA").is_err());
    }

    #[test]
    fn the_private_key_file_is_a_valid_openssh_key_for_the_real_ssh_keygen() {
        let Ok(probe) = std::process::Command::new("ssh-keygen").arg("-?").output() else { return }; // ssh-keygen absent : sauté
        let _ = probe;
        let dir = tmp("sshkeygen");
        let t = Secrets::from_ssh_private_file(FILE).twin().unwrap();
        let path = dir.join("k");
        write_secret(&path, &t.ssh_private("station"), 0o600).unwrap();
        let out = std::process::Command::new("ssh-keygen").args(["-y", "-f"]).arg(&path).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), t.ssh_public("station"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kubo_identity_has_the_libp2p_protobuf_shape() {
        let t = Secrets::from_ssh_private_file(FILE).twin().unwrap();
        let (peer, key) = t.kubo_identity();
        assert_eq!(peer, PEER);
        let raw = B64.decode(&*key).unwrap();
        assert_eq!(&raw[..4], &[0x08, 0x01, 0x12, 0x40]);
        assert_eq!(raw.len(), 68);
        assert!(key.starts_with("CAESQ"), "préfixe kubo habituel : {}", &*key);
    }

    #[test]
    fn init_is_idempotent_keeps_the_old_ssh_key_and_does_not_rederive_from_the_twin() {
        let dir = tmp("init");
        let paths = Paths { game: dir.join("game"), ssh: dir.join("ssh") };
        std::fs::create_dir_all(&paths.ssh).unwrap();
        std::fs::write(paths.ssh.join("id_ed25519"), FILE).unwrap();
        std::fs::write(paths.ssh.join("id_ed25519.pub"), "ssh-ed25519 AAAAorigin old@host\n").unwrap();
        let opts = InitOptions { comment: "station".into(), ..InitOptions::default() };

        let r = init(&paths, &opts, 1_800_000_000).unwrap();
        assert_eq!((r.source, r.peer_id.as_str()), (Source::SshKey, PEER));
        assert_eq!(std::fs::read(paths.ssh.join("origin.key")).unwrap(), FILE, "l'ancienne clé est conservée");
        assert!(std::fs::read_to_string(paths.ssh.join("origin.pub")).unwrap().contains("old@host"));
        assert_eq!(ssh_pub_to_peer_id(&std::fs::read_to_string(paths.ssh.join("id_ed25519.pub")).unwrap()).unwrap(), PEER);
        assert!(verify(&paths, None).iter().all(|c| c.ok), "{:?}", verify(&paths, None));

        // Second passage : la clé SSH est désormais la jumelle (son sha512 est autre) ; secret.june fait foi, rien ne change.
        let before = std::fs::read(paths.game.join("secret.june")).unwrap();
        let again = init(&paths, &opts, 1_800_000_100).unwrap();
        assert_eq!((again.source, again.peer_id.as_str()), (Source::SecretJune, PEER));
        assert!(again.written.is_empty() && again.kept.is_empty(), "{again:?}");
        assert_eq!(std::fs::read(paths.game.join("secret.june")).unwrap(), before);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["secret.june", "secret.nostr", "secret.NODE.dunikey"] {
                assert_eq!(std::fs::metadata(paths.game.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
            }
            assert_eq!(std::fs::metadata(paths.ssh.join("id_ed25519")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_station_without_ssh_key_gets_a_fresh_origin_and_a_twin() {
        let dir = tmp("fresh");
        let paths = Paths { game: dir.join("game"), ssh: dir.join("ssh") };
        let r = init(&paths, &InitOptions { comment: "c".into(), ..InitOptions::default() }, 1).unwrap();
        assert_eq!(r.source, Source::Fresh);
        assert!(r.peer_id.starts_with("12D3Koo"));
        assert!(verify(&paths, None).iter().all(|c| c.ok));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_flags_a_ssh_key_that_does_not_match_and_no_ssh_leaves_it_alone() {
        let dir = tmp("mismatch");
        let paths = Paths { game: dir.join("game"), ssh: dir.join("ssh") };
        std::fs::create_dir_all(&paths.ssh).unwrap();
        std::fs::write(paths.ssh.join("id_ed25519"), FILE).unwrap();
        let other = Secrets::from_ssh_private_file(b"autre").twin().unwrap();
        std::fs::write(paths.ssh.join("id_ed25519.pub"), format!("{}\n", other.ssh_public("x"))).unwrap();
        init(&paths, &InitOptions { no_ssh: true, ..InitOptions::default() }, 1).unwrap();
        assert!(!paths.ssh.join("origin.key").exists(), "--no-ssh : ~/.ssh est intact");
        let checks = verify(&paths, None);
        let ssh = checks.iter().find(|c| c.name == "SSH").unwrap();
        assert!(!ssh.ok && ssh.detail.contains("origin.key"), "{ssh:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
