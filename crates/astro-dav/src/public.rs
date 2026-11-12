//! **Blobs publics** : des images en clair (avatar, bandeau…) que n'importe quelle station peut servir à `GET /ipfs/<cid>`.
//!
//! Le CID est l'empreinte des octets (CIDv1 `raw` + sha2-256) : un lecteur peut vérifier ce qu'une passerelle lui rend, et une station
//! qui n'a pas l'image la rapatrie auprès de ses pairs (`/astro/blob/1.0.0`, empreinte vérifiée) avant de la servir. Si la station
//! d'origine s'éteint, l'image reste servie par toutes celles qui l'ont vue passer.
//!
//! **Liste explicite** : seuls les CID inscrits dans `public_blobs` sont servis en clair. Les blobs chiffrés de l'uCloud, les parts
//! de sauvegarde et les manifestes ne sont jamais exposés par ce chemin. Rapatrier un CID inconnu n'inscrit que ce qui est vraiment
//! une image (octets magiques JPEG, PNG ou WebP) et de taille bornée ; le reste est retiré du disque aussitôt.

use std::future::Future;
use std::pin::Pin;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use astro_db::Db;
use rusqlite::OptionalExtension;
use tokio::io::AsyncReadExt;

use crate::blob::{BlobStore, is_cid};

/// Octets au plus d'un blob public (une image de profil réduite en pèse quelques dizaines de Ko, une photo de post réduite à 2 048 px quelques centaines).
pub const MAX_PUBLIC_BYTES: u64 = 8 * 1024 * 1024;

/// Rapatrie `cid` chez les pairs s'il est inconnu (voir [`pull`]) ; vrai si le CID est ensuite un blob public de cette station.
pub type PublicFetch = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Type MIME d'une image reconnue à ses octets magiques ; `None` pour tout le reste (SVG, HTML, blob chiffré `UENC`…).
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Compte qui dépose un blob public, et plafond de son occupation totale.
#[derive(Debug, Clone, Copy)]
pub struct Owner<'a> {
    /// Clé (hex) du compte.
    pub hex: &'a str,
    /// Quota du compte : ses fichiers d'uCloud **et** ses blobs publics y comptent (voir `vfs::used_bytes`).
    pub quota: u64,
}

/// Inscrit `cid` comme blob public de type `mime`. `owner` : le compte qui le dépose, dont le quota d'uCloud paie le blob (décidé en
/// SQL, dans la même instruction que l'inscription) ; `None` pour un blob rapatrié chez les pairs, qui ne coûte rien à un compte.
/// Un CID déjà inscrit ne compte qu'une fois.
pub async fn register(db: &Db, cid: &str, mime: &str, size: u64, owner: Option<Owner<'_>>) -> Result<(), String> {
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (size_i, at_i) = (i64::try_from(size).unwrap_or(0), i64::try_from(at).unwrap_or(0));
    let quota = owner.map_or(i64::MAX, |o| i64::try_from(o.quota).unwrap_or(i64::MAX));
    let done = db
        .writer()
        .execute(
            "INSERT OR IGNORE INTO public_blobs(cid, mime, size, owner, added_at) \
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE ?4 IS NULL OR \
               (SELECT COALESCE(SUM(size), 0) FROM ucloud_entries WHERE owner_hex = ?4 AND type = 'file') \
             + (SELECT COALESCE(SUM(size), 0) FROM public_blobs WHERE owner = ?4) + ?3 <= ?6",
            vec![cid.to_owned().into(), mime.to_owned().into(), size_i.into(), owner.map(|o| o.hex.to_owned()).into(), at_i.into(), quota.into()],
        )
        .await
        .map_err(|e| e.to_string())?;
    if done.changes == 0 && mime_of(db, cid).await.is_none() {
        return Err("quota de stockage atteint : l'image n'a pas été enregistrée".into());
    }
    Ok(())
}

/// Range `data` (une image déjà réencodée, de type `mime`) dans `store` et l'inscrit comme blob public ; rend son CID.
pub async fn put<S: BlobStore>(db: &Db, store: &S, data: &[u8], mime: &str, owner: Option<Owner<'_>>) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    if sniff(data) != Some(mime) || data.len() as u64 > MAX_PUBLIC_BYTES {
        return Err("image publique : type ou taille refusés".into());
    }
    let mut w = store.begin().await.map_err(|e| e.to_string())?;
    w.write_all(data).await.map_err(|e| e.to_string())?;
    let meta = crate::blob::BlobWriter::commit(w).await.map_err(|e| e.to_string())?;
    register(db, &meta.cid, mime, meta.size, owner).await?;
    Ok(meta.cid)
}

/// Type MIME du blob public `cid`, s'il est inscrit.
pub async fn mime_of(db: &Db, cid: &str) -> Option<String> {
    let cid = cid.to_owned();
    db.readers().read(move |c| Ok(c.query_row("SELECT mime FROM public_blobs WHERE cid = ?1", [cid], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
}

/// Rend `cid` disponible comme blob public. `local` : le magasin **local** (jamais celui qui rapatrie tout seul) ; `fetch` : le
/// rapatriement borné chez les pairs, appelé seulement si le CID est absent du disque.
///
/// Vrai si `cid` est (ou devient) un blob public. Un blob déjà présent mais non inscrit est privé (chiffré) : jamais promu, jamais
/// retiré. Un blob rapatrié qui n'est pas une image, ou qui est trop gros, est supprimé.
pub async fn pull<L: BlobStore, F: Future<Output = bool>>(db: &Db, local: &L, cid: &str, fetch: impl FnOnce() -> F) -> bool {
    if !is_cid(cid) {
        return false;
    }
    if mime_of(db, cid).await.is_some() {
        return true;
    }
    if local.open(cid).await.is_ok() || !fetch().await {
        return false;
    }
    let Ok((mut reader, len)) = local.open(cid).await else { return false };
    let mut head = [0u8; 16];
    let n = if len <= MAX_PUBLIC_BYTES { reader.read(&mut head).await.unwrap_or(0) } else { 0 };
    match sniff(&head[..n]) {
        Some(mime) if register(db, cid, mime, len, None).await.is_ok() => true,
        _ => {
            let _ = local.remove(cid).await;
            false
        }
    }
}

/// Pare-feu du rapatriement : `GET /ipfs/<cid>` est public, et chaque CID inconnu coûte une question à des pairs. Au plus
/// [`MAX_CONCURRENT_PULLS`] rapatriements à la fois, [`MAX_PULLS_PER_MINUTE`] par minute, et un CID qui n'a rien donné n'est pas
/// redemandé avant [`MISS_TTL`] (mémoire bornée à [`MAX_MISSES`] CID).
pub struct PullGate {
    slots: Arc<tokio::sync::Semaphore>,
    state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    misses: HashMap<String, Instant>,
    starts: VecDeque<Instant>,
}

/// Rapatriements simultanés au plus.
pub const MAX_CONCURRENT_PULLS: usize = 4;
/// Rapatriements au plus par minute.
pub const MAX_PULLS_PER_MINUTE: usize = 60;
/// Durée pendant laquelle un CID introuvable n'est pas redemandé.
pub const MISS_TTL: Duration = Duration::from_secs(120);
/// CID introuvables retenus au plus.
pub const MAX_MISSES: usize = 2048;

/// Décision du pare-feu.
pub enum Admit {
    /// Rapatriement permis ; le jeton se garde pendant toute sa durée.
    Go(tokio::sync::OwnedSemaphorePermit),
    /// Ce CID a déjà été cherché sans succès il y a peu : 404 sans solliciter personne.
    KnownMiss,
    /// Trop de rapatriements en cours ou sur la dernière minute : 429.
    Busy,
}

impl Default for PullGate {
    fn default() -> Self {
        Self { slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PULLS)), state: Mutex::default() }
    }
}

impl PullGate {
    /// Demande l'autorisation de chercher `cid` chez les pairs.
    pub fn admit(&self, cid: &str) -> Admit {
        let now = Instant::now();
        let Ok(mut st) = self.state.lock() else { return Admit::Busy };
        st.misses.retain(|_, at| now.duration_since(*at) < MISS_TTL);
        if st.misses.contains_key(cid) {
            return Admit::KnownMiss;
        }
        while st.starts.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
            st.starts.pop_front();
        }
        if st.starts.len() >= MAX_PULLS_PER_MINUTE {
            return Admit::Busy;
        }
        let Ok(permit) = self.slots.clone().try_acquire_owned() else { return Admit::Busy };
        st.starts.push_back(now);
        Admit::Go(permit)
    }

    /// Note que `cid` est introuvable chez les pairs.
    pub fn miss(&self, cid: &str) {
        let Ok(mut st) = self.state.lock() else { return };
        if st.misses.len() >= MAX_MISSES {
            // Table pleine : on oublie le plus ancien, la mémoire reste bornée.
            if let Some(oldest) = st.misses.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone()) {
                st.misses.remove(&oldest);
            }
        }
        st.misses.insert(cid.to_owned(), Instant::now());
    }
}
