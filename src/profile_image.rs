//! Images de profil d'un compte (kind 0 : `picture` et `banner`) : réduites, rangées comme blobs publics (CID) et servies par toute station, pour que
//! le profil pointe vers une adresse qui marche (et que le fond d'un Ğ1Billet puisse s'en servir) sans dépendre d'un site tiers.
//!
//! Tout ce qui entre est **relu et réencodé** en JPEG de taille bornée : un fichier piégé n'est jamais servi tel quel. Une image
//! d'un autre site (adresse `https://` d'un profil) n'est lue que par le chargeur borné (pas d'adresse privée, taille limitée).

use std::sync::Arc;

use astro_db::Db;
use astro_dav::blob::is_cid;
use astro_dav::{BlobStore, PublicFetch};
use base64::Engine as _;
use image::ImageReader;
use image::imageops::FilterType;
use tokio::io::AsyncReadExt;

use crate::fetch::{FetchLimits, fetch};

/// Octets au plus d'une image reçue (avant réduction).
pub const MAX_INPUT: usize = 4 * 1024 * 1024;
/// Octets au plus d'une image lue chez un tiers.
const MAX_EXTERNAL: u64 = 3 * 1024 * 1024;

/// Nature de l'image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Icône du profil : carré.
    Picture,
    /// Bandeau (fond) : large.
    Banner,
}

impl Kind {
    /// Lit `picture` ou `banner`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "picture" => Some(Self::Picture),
            "banner" => Some(Self::Banner),
            _ => None,
        }
    }

    /// Nom du champ du kind 0 et de la ligne.
    pub fn name(self) -> &'static str {
        match self {
            Self::Picture => "picture",
            Self::Banner => "banner",
        }
    }

    fn size(self) -> (u32, u32) {
        match self {
            Self::Picture => (320, 320),
            Self::Banner => (1200, 400),
        }
    }
}

/// Décode, recadre (couvre le cadre) et réencode en JPEG. Une image qui n'en est pas une est refusée.
pub fn normalize(bytes: &[u8], kind: Kind) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_INPUT {
        return Err("image trop lourde (4 Mo au plus)".into());
    }
    let img = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?.decode().map_err(|_| "ce fichier n'est pas une image lisible (JPEG, PNG, GIF ou WebP)".to_owned())?;
    let (w, h) = kind.size();
    let fitted = img.resize_to_fill(w, h, FilterType::Lanczos3).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82).encode_image(&fitted).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Magasin des images publiques de la station : range les octets réencodés comme **blobs publics** (CID brut `bafkrei…`, en clair,
/// inscrits dans `public_blobs`) et les relit, au besoin chez les pairs du maillage. Une image de profil n'est donc plus une ligne
/// de SQLite attachée à une station : n'importe quelle station amie la sert à `/ipfs/<cid>`.
pub struct PublicImages {
    put: PutFn,
    get: GetFn,
}

/// Quota (octets) d'un compte hébergé ici, par sa clé (hex).
pub type QuotaFn = Arc<dyn Fn(String) -> BoxFut<u64> + Send + Sync>;
type PutFn = Box<dyn Fn(String, Vec<u8>) -> BoxFut<Result<String, String>> + Send + Sync>;
type GetFn = Box<dyn Fn(String) -> BoxFut<Option<Vec<u8>>> + Send + Sync>;
type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

impl PublicImages {
    /// `store` : le magasin de blobs de la station (celui qui rapatrie chez les pairs) ; `fetch` : le rapatriement borné d'un blob
    /// public inconnu (voir [`astro_dav::public::pull`]), absent sans maillage.
    /// `quota` : quota d'un compte (octets), que ses images publiques entament comme ses fichiers ; sans lui, aucune limite.
    pub fn new<S: BlobStore>(db: Arc<Db>, store: Arc<S>, fetch: Option<PublicFetch>, quota: Option<QuotaFn>) -> Self {
        let (put_db, put_store) = (db.clone(), store.clone());
        let put: PutFn = Box::new(move |owner: String, jpeg: Vec<u8>| {
            let (db, store, quota) = (put_db.clone(), put_store.clone(), quota.clone());
            Box::pin(async move {
                let quota = match &quota {
                    Some(q) => q(owner.clone()).await,
                    None => u64::MAX,
                };
                astro_dav::public::put(&db, store.as_ref(), &jpeg, "image/jpeg", Some(astro_dav::public::Owner { hex: &owner, quota })).await
            })
        });
        let get: GetFn = Box::new(move |cid: String| {
            let (db, store, fetch) = (db.clone(), store.clone(), fetch.clone());
            Box::pin(async move {
                if astro_dav::public::mime_of(&db, &cid).await.is_none() && !(fetch?)(cid.clone()).await {
                    return None;
                }
                let (mut reader, len) = store.open(&cid).await.ok()?;
                if len > astro_dav::public::MAX_PUBLIC_BYTES {
                    return None;
                }
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.ok()?;
                Some(buf)
            })
        });
        Self { put, get }
    }

    /// Range le JPEG déjà normalisé du compte `owner` (ses octets entament son quota) ; rend son CID.
    pub async fn put(&self, owner: &str, jpeg: Vec<u8>) -> Result<String, String> {
        (self.put)(owner.to_owned(), jpeg).await
    }

    /// Octets du blob public `cid` : en local, sinon chez les pairs. `None` s'il n'existe nulle part ou n'est pas une image publique.
    pub async fn get(&self, cid: &str) -> Option<Vec<u8>> {
        (self.get)(cid.to_owned()).await
    }
}

/// Adresse publique d'une image sur la station `base` : `<base>/ipfs/<cid>`.
pub fn url_for(base: &str, cid: &str) -> String {
    format!("{}/ipfs/{cid}", base.trim_end_matches('/'))
}

/// CID d'une adresse `…/ipfs/<cid>` (avec ou sans paramètres), quelle que soit la station qui la sert.
pub fn cid_of(url: &str) -> Option<&str> {
    let cid = url.split("/ipfs/").nth(1)?.split(['?', '#']).next()?;
    is_cid(cid).then_some(cid)
}

async fn external(url: &str) -> Result<Vec<u8>, String> {
    let mut limits = FetchLimits::new(MAX_EXTERNAL);
    limits.timeout = std::time::Duration::from_secs(10);
    let got = fetch(url, &limits).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    got.body.take(MAX_EXTERNAL + 1).read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_EXTERNAL {
        return Err("image trop lourde".into());
    }
    Ok(buf)
}

/// Image du profil `url`, réduite pour `kind`, en `data:` URI (ce que la CSP laisse passer et ce que l'impression garde).
/// Une adresse `…/ipfs/<cid>` est lue par le CID (ici ou chez les pairs) : elle survit à l'arrêt de la station qui l'a publiée ; si le
/// CID reste introuvable, l'adresse elle-même est lue. `None` si l'adresse manque, n'est pas `https://` ou si l'image est illisible : un
/// fond manquant ne bloque jamais un billet.
pub async fn data_uri(images: Option<&PublicImages>, url: &str, kind: Kind) -> Option<String> {
    let by_cid = match (images, cid_of(url)) {
        (Some(images), Some(cid)) => images.get(cid).await,
        _ => None,
    };
    let raw = match by_cid {
        Some(raw) => raw,
        None if url.starts_with("https://") => external(url).await.ok()?,
        None => return None,
    };
    let jpeg = normalize(&raw, kind).ok()?;
    Some(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn an_image_is_recut_to_the_frame_and_reencoded() {
        let banner = normalize(&png(2000, 900), Kind::Banner).unwrap();
        let img = image::load_from_memory(&banner).unwrap();
        assert_eq!((img.width(), img.height()), (1200, 400));
        assert_eq!(&banner[..3], &[0xFF, 0xD8, 0xFF], "JPEG");
        let icon = image::load_from_memory(&normalize(&png(100, 300), Kind::Picture).unwrap()).unwrap();
        assert_eq!((icon.width(), icon.height()), (320, 320));
        assert!(normalize(b"<?php echo 1; ?>", Kind::Picture).unwrap_err().contains("pas une image"));
        assert!(normalize(&vec![0u8; MAX_INPUT + 1], Kind::Picture).unwrap_err().contains("trop lourde"));
    }

    #[test]
    fn the_cid_is_read_from_any_stations_ipfs_address() {
        let cid = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
        assert_eq!(cid_of(&url_for("https://s.exemple/", cid)), Some(cid));
        assert_eq!(cid_of(&format!("https://autre.exemple/ipfs/{cid}?v=3")), Some(cid));
        for bad in ["https://x.exemple/photo.jpg", "https://x.exemple/ipfs/pas-un-cid", "https://x.exemple/ipfs/../x"] {
            assert_eq!(cid_of(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_stored_image_comes_back_as_a_small_data_uri_and_a_foreign_http_address_is_ignored() {
        let dir = std::env::temp_dir().join(format!("astroportd-profile-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let images = PublicImages::new(db, Arc::new(astro_dav::FsBlobStore::open(dir.join("blobs")).unwrap()), None, None);
        let cid = images.put(&"ab".repeat(32), normalize(&png(900, 300), Kind::Banner).unwrap()).await.unwrap();
        let uri = data_uri(Some(&images), &url_for("https://s.exemple", &cid), Kind::Banner).await.unwrap();
        assert!(uri.starts_with("data:image/jpeg;base64,") && uri.len() < 400_000);
        assert!(data_uri(Some(&images), "http://x.exemple/a.jpg", Kind::Banner).await.is_none(), "pas de http");
        let unknown = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
        assert!(images.get(unknown).await.is_none(), "inconnu et sans pairs");
        assert!(data_uri(None, &url_for("http://s.exemple", unknown), Kind::Picture).await.is_none(), "rien de rangé");
        let _ = std::fs::remove_dir_all(dir);
    }
}
