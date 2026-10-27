//! Déménagement souverain (`home:station`) : le titulaire emporte son MULTIPASS vers une autre station.
//!
//! - [`export`] : conteneur chiffré (UENC v2) des fichiers d'identité du MULTIPASS — clés `.secret.*`, `.keyformula`,
//!   fichiers `.next.*` — avec une clé de conteneur aléatoire que l'appelant remet au titulaire (jamais stockée ici) ;
//! - [`import`] / [`restore_into`] : lecture du conteneur sur la station d'accueil, noms de fichiers assainis ;
//! - [`residual_cashback`] et [`restore_cashback`] : solde résiduel à rembourser, avec les garde-fous du legacy
//!   (`nostr_DESTROY_TW.sh` / `nostr_RESTORE_TW.sh`).
//!
//! Les fichiers d'uCloud ne sont pas dans le conteneur : ils vivent sur IPFS, retrouvés par le manifeste kind 30078.

use std::collections::BTreeMap;
use std::path::Path;

use astro_core::Amount;
use astro_crypto::uenc::{Cipher, decrypt_to_vec, encrypt_to_vec};
use astro_crypto::{UencError, UencKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use zeroize::Zeroizing;

/// Dépôt existentiel de la Ğ1 v2s (1 Ğ1) : retenu sur le cashback.
pub const EXISTENTIAL_DEPOSIT: Amount = Amount(100);
/// Au-delà, l'ancien wallet détient encore les fonds : un cashback serait un double crédit (1,5 Ğ1).
pub const STILL_FUNDED_ABOVE: Amount = Amount(150);

const FORMAT: &str = "astroportd-migration-v1";

/// Erreurs de migration.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// Lecture/écriture de fichier.
    #[error("fichier : {0}")]
    Io(#[from] std::io::Error),
    /// Chiffrement ou déchiffrement du conteneur.
    #[error("conteneur : {0}")]
    Crypto(#[from] UencError),
    /// Contenu qui n'est pas un conteneur de migration.
    #[error("conteneur invalide : {0}")]
    Format(String),
    /// Aucun fichier d'identité à emporter.
    #[error("aucun fichier d'identité dans {0}")]
    Empty(String),
}

/// `home_station` : `<IPFSNODEID>:<NODE_HEX>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeStation {
    /// PeerID de la station.
    pub ipfs_node_id: String,
    /// Clé Nostr de la station (64 hex).
    pub node_hex: String,
}

impl HomeStation {
    /// Lit `<IPFSNODEID>:<NODE_HEX>`.
    pub fn parse(s: &str) -> Option<Self> {
        let (id, hex) = s.trim().split_once(':')?;
        (!id.is_empty() && astro_nostr_proto::is_hex64(hex)).then(|| Self { ipfs_node_id: id.to_owned(), node_hex: hex.to_owned() })
    }
}

impl std::fmt::Display for HomeStation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.ipfs_node_id, self.node_hex)
    }
}

/// Conteneur prêt à transmettre.
pub struct Export {
    /// Octets UENC v2 (publiables sur IPFS : sans la clé, illisibles).
    pub container: Vec<u8>,
    /// Clé du conteneur, à remettre au titulaire par un canal séparé.
    pub key: UencKey,
    /// Noms des fichiers emportés.
    pub files: Vec<String>,
}

fn portable(name: &str) -> bool {
    name == ".keyformula" || name.starts_with(".secret.") || name.starts_with(".next.")
}

/// Exporte les fichiers d'identité de `dir` (dossier `nostr/<email>`) pour `email`, vers la station `to`.
pub async fn export(dir: &Path, email: &str, hex: &str, from: Option<&HomeStation>, to: &HomeStation, now: u64) -> Result<Export, MigrationError> {
    let mut files: BTreeMap<String, Zeroizing<Vec<u8>>> = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() && portable(&name) {
            files.insert(name, Zeroizing::new(std::fs::read(entry.path())?));
        }
    }
    if files.is_empty() {
        return Err(MigrationError::Empty(dir.display().to_string()));
    }
    let names: Vec<String> = files.keys().cloned().collect();
    let body = serde_json::json!({
        "format": FORMAT, "email": email, "hex": hex, "created_at": now,
        "from": from.map(ToString::to_string), "to": to.to_string(),
        "files": files.iter().map(|(k, v)| (k.clone(), STANDARD.encode(v.as_slice()))).collect::<BTreeMap<_, _>>(),
    });
    let plain = Zeroizing::new(serde_json::to_vec(&body).map_err(|e| MigrationError::Format(e.to_string()))?);
    let key = UencKey::generate()?;
    let container = encrypt_to_vec(&key, Cipher::detect(), &plain).await?;
    Ok(Export { container, key, files: names })
}

/// Contenu déchiffré d'un conteneur.
pub struct Imported {
    /// Email du MULTIPASS.
    pub email: String,
    /// Clé Nostr (64 hex).
    pub hex: String,
    /// Station de destination déclarée.
    pub to: Option<HomeStation>,
    /// Fichiers (nom → contenu).
    pub files: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

/// Déchiffre et lit un conteneur. Les noms de fichiers non portables (chemins, `..`) sont refusés.
pub async fn import(container: &[u8], key: &UencKey) -> Result<Imported, MigrationError> {
    let plain = Zeroizing::new(decrypt_to_vec(key, container).await?);
    let v: serde_json::Value = serde_json::from_slice(&plain).map_err(|e| MigrationError::Format(e.to_string()))?;
    if v["format"] != FORMAT {
        return Err(MigrationError::Format("format inconnu".into()));
    }
    let text = |k: &str| v[k].as_str().map(str::to_owned).ok_or_else(|| MigrationError::Format(format!("{k} absent")));
    let mut files = BTreeMap::new();
    for (name, b64) in v["files"].as_object().ok_or_else(|| MigrationError::Format("files absent".into()))? {
        if !portable(name) || name.contains(['/', '\\']) || name.contains("..") {
            return Err(MigrationError::Format(format!("nom de fichier refusé : {name}")));
        }
        let data = STANDARD.decode(b64.as_str().unwrap_or_default()).map_err(|_| MigrationError::Format(format!("{name} : base64 invalide")))?;
        files.insert(name.clone(), Zeroizing::new(data));
    }
    Ok(Imported { email: text("email")?, hex: text("hex")?, to: v["to"].as_str().and_then(HomeStation::parse), files })
}

/// Écrit les fichiers dans `dir` (créé si besoin) avec les droits `0600`. Refuse d'écraser un compte existant.
pub fn restore_into(imported: &Imported, dir: &Path) -> Result<(), MigrationError> {
    if dir.join(".secret.nostr").exists() {
        return Err(MigrationError::Format(format!("{} existe déjà : restauration refusée", dir.display())));
    }
    std::fs::create_dir_all(dir)?;
    for (name, data) in &imported.files {
        let path = dir.join(name);
        std::fs::write(&path, data.as_slice())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// Solde résiduel à rembourser au départ : le solde moins le dépôt existentiel, jamais négatif.
pub fn residual_cashback(raw_balance: Amount, existential_deposit: Amount) -> Amount {
    raw_balance.checked_sub(existential_deposit).unwrap_or_default()
}

/// Pourquoi un cashback est versé, réduit ou refusé à l'arrivée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CashbackDecision {
    /// Versement de ce montant.
    Pay(Amount),
    /// Autre UPlanet : pas de cashback.
    DifferentUplanet,
    /// Solde de l'ancien wallet illisible : on ne verse rien dans le doute.
    OldWalletUnreadable,
    /// L'ancien wallet détient encore les fonds : double crédit évité.
    OldWalletStillFunded,
    /// Rien au-dessus du dépôt existentiel.
    NothingToRefund,
}

/// Décide du cashback à l'arrivée sur la station d'accueil.
///
/// `recorded` : solde enregistré au départ ; `old_wallet_now` : solde actuel de l'ancien wallet (`None` si illisible).
/// Après un cashback réussi, l'ancien wallet ne contient plus que le dépôt existentiel (~1 Ğ1).
pub fn restore_cashback(recorded: Amount, same_uplanet: bool, old_wallet_now: Option<Amount>) -> CashbackDecision {
    if !same_uplanet {
        return CashbackDecision::DifferentUplanet;
    }
    match old_wallet_now {
        None => CashbackDecision::OldWalletUnreadable,
        Some(b) if b > STILL_FUNDED_ABOVE => CashbackDecision::OldWalletStillFunded,
        Some(_) => match residual_cashback(recorded, EXISTENTIAL_DEPOSIT) {
            Amount(0) => CashbackDecision::NothingToRefund,
            a => CashbackDecision::Pay(a),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::discovery::fixtures::{secret_nostr, write_secret};

    static N: AtomicU32 = AtomicU32::new(0);

    fn account_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("astro-mig-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        write_secret(&dir.join(".secret.nostr"), &secret_nostr(&[3; 32]));
        write_secret(&dir.join(".secret.dunikey"), &crate::dunikey::render(&[3; 32]));
        std::fs::write(dir.join(".keyformula"), "nostr=isolated").unwrap();
        std::fs::write(dir.join(".next.nostr"), "prochaine-cle").unwrap();
        std::fs::write(dir.join("HEX"), "pas-portable").unwrap();
        std::fs::create_dir_all(dir.join(".ucloud")).unwrap();
        dir
    }

    fn home(c: char) -> HomeStation {
        HomeStation { ipfs_node_id: format!("12D3Koo{c}"), node_hex: c.to_string().repeat(64) }
    }

    #[tokio::test]
    async fn the_container_round_trips_only_with_the_key_and_carries_only_identity_files() {
        let dir = account_dir();
        let hex = astro_nostr_proto::pubkey_of(&[3; 32]).unwrap();
        let ex = export(&dir, "alice@example.org", &hex, Some(&home('a')), &home('b'), 1_800_000_000).await.unwrap();
        assert_eq!(ex.files, [".keyformula", ".next.nostr", ".secret.dunikey", ".secret.nostr"]);
        assert!(!ex.container.windows(8).any(|w| w == b"nsec1" || w == b"NSEC="), "rien de lisible dans le conteneur");
        assert!(import(&ex.container, &UencKey::generate().unwrap()).await.is_err(), "mauvaise clé");
        let imp = import(&ex.container, &ex.key).await.unwrap();
        assert_eq!((imp.email.as_str(), imp.hex.as_str(), imp.to.clone()), ("alice@example.org", hex.as_str(), Some(home('b'))));
        let target = std::env::temp_dir().join(format!("astro-mig-dest-{}", N.fetch_add(1, Ordering::SeqCst))).join("alice@example.org");
        restore_into(&imp, &target).unwrap();
        assert_eq!(std::fs::read(target.join(".secret.nostr")).unwrap(), std::fs::read(dir.join(".secret.nostr")).unwrap());
        assert!(!target.join("HEX").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(target.join(".secret.nostr")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(restore_into(&imp, &target).is_err(), "jamais d'écrasement d'un compte existant");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }

    #[tokio::test]
    async fn an_empty_account_and_a_forged_container_are_refused() {
        let empty = std::env::temp_dir().join(format!("astro-mig-empty-{}", N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(export(&empty, "x", "y", None, &home('b'), 0).await, Err(MigrationError::Empty(_))));
        let key = UencKey::generate().unwrap();
        for body in [
            serde_json::json!({"format": "autre"}),
            serde_json::json!({"format": FORMAT, "email": "a", "hex": "b", "files": {"../evil": "AA=="}}),
            serde_json::json!({"format": FORMAT, "email": "a", "hex": "b", "files": {".secret.x/y": "AA=="}}),
            serde_json::json!({"format": FORMAT, "email": "a", "hex": "b", "files": {".secret.x": "***"}}),
        ] {
            let blob = encrypt_to_vec(&key, Cipher::detect(), body.to_string().as_bytes()).await.unwrap();
            assert!(matches!(import(&blob, &key).await, Err(MigrationError::Format(_))), "{body}");
        }
        let _ = std::fs::remove_dir_all(empty);
    }

    #[test]
    fn home_station_parses_and_prints_the_legacy_format() {
        let h = home('a');
        assert_eq!(HomeStation::parse(&h.to_string()), Some(h));
        assert_eq!(HomeStation::parse("sans-deux-points"), None);
        assert_eq!(HomeStation::parse("12D3:court"), None);
    }

    #[test]
    fn cashback_follows_the_legacy_guards() {
        assert_eq!(residual_cashback(Amount(1_250), EXISTENTIAL_DEPOSIT), Amount(1_150));
        assert_eq!(residual_cashback(Amount(80), EXISTENTIAL_DEPOSIT), Amount(0));
        let rec = Amount(1_250);
        assert_eq!(restore_cashback(rec, true, Some(Amount(100))), CashbackDecision::Pay(Amount(1_150)), "il ne reste que le dépôt : cashback versé");
        assert_eq!(restore_cashback(rec, true, Some(Amount(150))), CashbackDecision::Pay(Amount(1_150)), "1,5 Ğ1 pile : toujours versé");
        assert_eq!(restore_cashback(rec, true, Some(Amount(151))), CashbackDecision::OldWalletStillFunded);
        assert_eq!(restore_cashback(rec, true, None), CashbackDecision::OldWalletUnreadable);
        assert_eq!(restore_cashback(rec, false, Some(Amount(100))), CashbackDecision::DifferentUplanet);
        assert_eq!(restore_cashback(Amount(100), true, Some(Amount(100))), CashbackDecision::NothingToRefund);
    }
}
