//! Images de profil d'un compte (kind 0 : `picture` et `banner`) : réduites, rangées par la station et servies publiquement, pour que
//! le profil pointe vers une adresse qui marche (et que le fond d'un Ğ1Billet puisse s'en servir) sans dépendre d'un site tiers.
//!
//! Tout ce qui entre est **relu et réencodé** en JPEG de taille bornée : un fichier piégé n'est jamais servi tel quel. Une image
//! d'un autre site (adresse `https://` d'un profil) n'est lue que par le chargeur borné (pas d'adresse privée, taille limitée).

use std::sync::Arc;

use astro_db::Db;
use base64::Engine as _;
use image::ImageReader;
use image::imageops::FilterType;
use rusqlite::OptionalExtension;
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

/// Range l'image `kind` du compte `hex`.
pub async fn store(db: &Db, hex: &str, kind: Kind, jpeg: Vec<u8>, at: u64) -> Result<(), String> {
    db.writer()
        .execute("INSERT OR REPLACE INTO profile_images(hex, kind, data, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![hex.to_owned().into(), kind.name().to_owned().into(), jpeg.into(), i64::try_from(at).unwrap_or(0).into()])
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Image rangée du compte, si elle existe.
pub async fn load(db: &Db, hex: &str, kind: Kind) -> Option<Vec<u8>> {
    let (h, k) = (hex.to_owned(), kind.name());
    db.readers().read(move |c| Ok(c.query_row("SELECT data FROM profile_images WHERE hex = ?1 AND kind = ?2", rusqlite::params![h, k], |r| r.get::<_, Vec<u8>>(0)).optional()?)).await.ok().flatten()
}

/// Adresse publique de l'image d'un compte sur cette station (`<base>/api/v1/profile/<hex>/<kind>.jpg?v=<date>`).
pub fn url_for(base: &str, hex: &str, kind: Kind, at: u64) -> String {
    format!("{}/api/v1/profile/{hex}/{}.jpg?v={at}", base.trim_end_matches('/'), kind.name())
}

/// L'adresse d'un profil est-elle une image de **ce** compte rangée ici ? (`…/api/v1/profile/<hex>/<kind>.jpg`)
fn own_kind(url: &str, hex: &str) -> Option<Kind> {
    let rest = url.split("/api/v1/profile/").nth(1)?;
    let (h, file) = rest.split('?').next()?.split_once('/')?;
    (h == hex).then(|| Kind::parse(file.strip_suffix(".jpg")?)).flatten()
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

/// Image du profil `url` du compte `hex`, réduite pour `kind`, en `data:` URI (ce que la CSP laisse passer et ce que l'impression garde).
/// `None` si l'adresse manque, n'est pas `https://`, ou si l'image est illisible : un fond manquant ne bloque jamais un billet.
pub async fn data_uri(db: &Arc<Db>, hex: &str, url: &str, kind: Kind) -> Option<String> {
    let raw = if let Some(own) = own_kind(url, hex) { load(db, hex, own).await? } else if url.starts_with("https://") { external(url).await.ok()? } else { return None };
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
    fn only_the_accounts_own_stored_image_is_recognised() {
        let hex = "ab".repeat(32);
        assert_eq!(own_kind(&url_for("https://s.exemple/", &hex, Kind::Banner, 5), &hex), Some(Kind::Banner));
        assert_eq!(own_kind(&url_for("https://s.exemple", &"cd".repeat(32), Kind::Banner, 5), &hex), None, "image d'un autre compte");
        assert_eq!(own_kind("https://x.exemple/photo.jpg", &hex), None);
    }

    #[tokio::test]
    async fn a_stored_image_comes_back_as_a_small_data_uri_and_a_foreign_http_address_is_ignored() {
        let dir = std::env::temp_dir().join(format!("astroportd-profile-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let hex = "ab".repeat(32);
        store(&db, &hex, Kind::Banner, normalize(&png(900, 300), Kind::Banner).unwrap(), 7).await.unwrap();
        let uri = data_uri(&db, &hex, &url_for("https://s.exemple", &hex, Kind::Banner, 7), Kind::Banner).await.unwrap();
        assert!(uri.starts_with("data:image/jpeg;base64,") && uri.len() < 400_000);
        assert!(data_uri(&db, &hex, "http://x.exemple/a.jpg", Kind::Banner).await.is_none(), "pas de http");
        assert!(data_uri(&db, &hex, &url_for("https://s.exemple", &hex, Kind::Picture, 7), Kind::Picture).await.is_none(), "rien de rangé");
        let _ = std::fs::remove_dir_all(dir);
    }
}
