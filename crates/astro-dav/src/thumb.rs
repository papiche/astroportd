//! Miniatures WebP (320 × 320 au plus) générées à l'ingestion des images.
//!
//! La miniature est un **blob à part**, chiffré en UENC v2 avec sa propre clé ; son CID, ses dimensions et celles de
//! l'original sont inscrits dans l'entrée (`data.thumb`), sa clé dans le trousseau : tout part dans le manifeste
//! ancré sur Nostr/IPFS, donc survit à un `replay`. Les copies d'un fichier (même CID) partagent la même miniature.
//!
//! Le décodage d'une image envoyée par un utilisateur est un travail à risque : plafonds de dimensions et de
//! mémoire, deux décodages simultanés au plus, hors du runtime asynchrone, et un échec n'empêche jamais le dépôt
//! (le fichier est là, seule la miniature manque ; l'interface retombe sur l'original).

use std::io::Cursor;
use std::sync::Arc;

use astro_crypto::uenc::{self, Cipher};
use astro_crypto::UencKey;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::blob::{BlobError, BlobStore, BlobWriter, is_cid};
use crate::server::DavState;

/// Côté maximal d'une miniature, en pixels.
pub const MAX_EDGE: u32 = 320;
/// Taille maximale d'un original pris en charge (au-delà : pas de miniature).
pub const MAX_SOURCE_BYTES: u64 = 48 << 20;
/// Qualité WebP avec perte.
const QUALITY: f32 = 75.0;
/// Décodages simultanés au plus : une photo de 48 Mpx pèse ~200 Mo décodée.
static SLOTS: Semaphore = Semaphore::const_new(2);

/// Erreurs de génération.
#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    /// Image illisible ou refusée (trop grande, format inconnu).
    #[error("image : {0}")]
    Image(String),
    /// Encodage WebP.
    #[error("encodage WebP : {0}")]
    Encode(&'static str),
    /// Blob.
    #[error("blob : {0}")]
    Blob(#[from] BlobError),
    /// Chiffrement.
    #[error("UENC : {0}")]
    Uenc(#[from] uenc::UencError),
    /// Base.
    #[error("base : {0}")]
    Db(#[from] astro_db::DbError),
    /// Entrée d'entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] std::io::Error),
}

/// Miniature enregistrée dans `ucloud_entries.data` (`thumb`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbInfo {
    /// CID du blob WebP chiffré.
    pub cid: String,
    /// Largeur de la miniature.
    pub w: u32,
    /// Hauteur de la miniature.
    pub h: u32,
    /// Largeur de l'original (après rotation EXIF).
    pub ow: u32,
    /// Hauteur de l'original.
    pub oh: u32,
    /// Octets du WebP.
    pub size: u64,
}

/// Une miniature rendue, avant stockage.
#[derive(Debug)]
pub struct Rendered {
    /// Fichier WebP.
    pub webp: Vec<u8>,
    /// Largeur.
    pub w: u32,
    /// Hauteur.
    pub h: u32,
    /// Largeur de l'original.
    pub ow: u32,
    /// Hauteur de l'original.
    pub oh: u32,
}

/// Les types d'images dont on sait faire une miniature.
pub fn is_thumbable(mime: &str) -> bool {
    matches!(mime.split(';').next().unwrap_or("").trim(), "image/jpeg" | "image/png" | "image/gif" | "image/webp")
}

/// Décode `src`, applique l'orientation EXIF, réduit à [`MAX_EDGE`] et encode en WebP. Synchrone et coûteux :
/// à appeler depuis `spawn_blocking`.
pub fn render(src: &[u8]) -> Result<Rendered, ThumbError> {
    let img_err = |e: &dyn std::fmt::Display| ThumbError::Image(e.to_string());
    let mut reader = ImageReader::new(Cursor::new(src)).with_guessed_format().map_err(|e| img_err(&e))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| img_err(&e))?;
    let orientation = decoder.orientation().map_err(|e| img_err(&e))?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| img_err(&e))?;
    img.apply_orientation(orientation);
    let (ow, oh) = (img.width(), img.height());
    let small = if ow.max(oh) > MAX_EDGE { img.thumbnail(MAX_EDGE, MAX_EDGE) } else { img };
    let (w, h) = (small.width(), small.height());
    let rgba = small.to_rgba8();
    let webp = webp::Encoder::from_rgba(&rgba, w, h).encode(QUALITY).to_vec();
    if webp.is_empty() {
        return Err(ThumbError::Encode("sortie vide"));
    }
    Ok(Rendered { webp, w, h, ow, oh })
}

/// Ligne lue pour un fichier : CID, type MIME, taille, clé de fichier.
type FileRow = (Option<String>, Option<String>, Option<i64>, Option<String>);

/// Miniature déjà connue pour ce CID chez ce compte (une copie du fichier la partage).
fn existing(conn: &rusqlite::Connection, owner: &str, cid: &str) -> rusqlite::Result<Option<ThumbInfo>> {
    let json: Option<String> = conn
        .query_row("SELECT json_extract(data, '$.thumb') FROM ucloud_entries WHERE owner_hex = ?1 AND cid = ?2 AND json_extract(data, '$.thumb.cid') IS NOT NULL LIMIT 1", params![owner, cid], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
}

/// Génère (ou rattache) la miniature du fichier `path` de `owner`. `Ok(None)` : rien à faire (pas une image prise
/// en charge, trop gros, déjà fait, ou le fichier a changé entre-temps).
pub async fn generate<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, path: &str) -> Result<Option<ThumbInfo>, ThumbError> {
    let (o, p) = (owner.to_owned(), path.to_owned());
    let found = st
        .db
        .readers()
        .read(move |c| {
            let row: Option<FileRow> = c
                .query_row(
                    "SELECT cid, mime, size, (SELECT key_hex FROM ucloud_keyring k WHERE k.owner_hex = e.owner_hex AND k.cid = e.cid) FROM ucloud_entries e WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'",
                    params![o, p],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((Some(cid), mime, size, key)) = row else { return Ok(None) };
            let known = existing(c, &o, &cid)?;
            Ok(Some((cid, mime.unwrap_or_default(), u64::try_from(size.unwrap_or(0)).unwrap_or(0), key, known)))
        })
        .await?;
    let Some((cid, mime, size, key_hex, known)) = found else { return Ok(None) };
    if !is_thumbable(&mime) || size > MAX_SOURCE_BYTES || !is_cid(&cid) {
        return Ok(None);
    }
    let info = match known {
        Some(info) => info, // une copie de ce fichier a déjà sa miniature : on la rattache
        None => {
            let Some(key_hex) = key_hex else { return Ok(None) };
            let key = UencKey::from_hex(&key_hex)?;
            let _permit = SLOTS.acquire().await.map_err(|_| ThumbError::Image("file d'attente fermée".into()))?;
            let (mut reader, _) = st.blobs.open(&cid).await?;
            let mut plain = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            uenc::decrypt_stream(&key, &mut reader, &mut plain).await?;
            // Le clair est déjà en main : on y lit aussi le GPS et la date de prise de vue (l'EXIF ne survit pas à la miniature).
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let (rendered, exif_meta) = tokio::task::spawn_blocking(move || {
                let found = crate::exif::read(&plain, now);
                render(&plain).map(|r| (r, found))
            })
            .await
            .map_err(|e| ThumbError::Image(e.to_string()))??;
            let tkey = UencKey::generate()?;
            let mut blob = st.blobs.begin().await?;
            uenc::encrypt_stream(&tkey, Cipher::detect(), &mut rendered.webp.as_slice(), &mut blob).await?;
            let meta = blob.commit().await?;
            let info = ThumbInfo { cid: meta.cid.clone(), w: rendered.w, h: rendered.h, ow: rendered.ow, oh: rendered.oh, size: rendered.webp.len() as u64 };
            let (o, c, i, k) = (owner.to_owned(), cid.clone(), serde_json::to_string(&info).unwrap_or_default(), tkey.to_hex());
            let attached = st
                .db
                .writer()
                .transaction(move |tx| {
                    // Le fichier a-t-il été remplacé ou supprimé pendant le décodage ? Alors la miniature est orpheline.
                    let alive: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND cid = ?2)", params![o, c], |r| r.get(0))?;
                    if !alive {
                        return Ok(false);
                    }
                    tx.execute("INSERT OR REPLACE INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", params![o, meta.cid, k])?;
                    tx.execute("UPDATE ucloud_entries SET data = json_set(data, '$.thumb', json(?3)) WHERE owner_hex = ?1 AND cid = ?2", params![o, c, i])?;
                    // Jamais d'écrasement : une position ou une date déjà notées (import, saisie) gardent la priorité.
                    tx.execute(
                        "UPDATE ucloud_entries SET lat = COALESCE(lat, ?3), lon = COALESCE(lon, ?4), taken_at = COALESCE(taken_at, ?5), data = json_set(data, '$.exif_scanned', 1) WHERE owner_hex = ?1 AND cid = ?2",
                        params![o, c, exif_meta.lat, exif_meta.lon, exif_meta.taken_at],
                    )?;
                    Ok(true)
                })
                .await?;
            if !attached {
                let _ = st.blobs.remove(&info.cid).await;
                return Ok(None);
            }
            if let Some(p) = &st.publisher {
                p.notify(owner);
            }
            return Ok(Some(info));
        }
    };
    // Rattachement d'une miniature existante à ce chemin (même CID) : le GPS et la date déjà lus pour une autre copie suivent.
    let (o, p, i) = (owner.to_owned(), path.to_owned(), serde_json::to_string(&info).unwrap_or_default());
    st.db.writer().execute("UPDATE ucloud_entries SET data = json_set(data, '$.thumb', json(?3)) WHERE owner_hex = ?1 AND path = ?2", vec![o.clone().into(), p.clone().into(), i.into()]).await?;
    st.db
        .writer()
        .execute(
            "UPDATE ucloud_entries SET lat = COALESCE(lat, s.lat), lon = COALESCE(lon, s.lon), taken_at = COALESCE(taken_at, s.taken_at), data = json_set(data, '$.exif_scanned', 1) \
             FROM (SELECT lat, lon, taken_at FROM ucloud_entries WHERE owner_hex = ?1 AND cid = ?3 AND (lat IS NOT NULL OR taken_at IS NOT NULL) LIMIT 1) s \
             WHERE ucloud_entries.owner_hex = ?1 AND ucloud_entries.path = ?2",
            vec![o.into(), p.into(), cid.clone().into()],
        )
        .await?;
    if let Some(p) = &st.publisher {
        p.notify(owner);
    }
    Ok(Some(info))
}


/// Bilan d'un passage de rattrapage EXIF.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillStats {
    /// Fichiers (CID distincts) lus pendant ce passage.
    pub scanned: usize,
    /// Dont une position GPS ou une date de prise de vue a été trouvée.
    pub found: usize,
    /// Fichiers qui restent à lire (ce compte).
    pub remaining: usize,
}

const BACKFILL_WHERE: &str = "e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL AND e.mime LIKE 'image/%' AND e.lat IS NULL AND e.taken_at IS NULL \
                              AND json_extract(e.data, '$.exif_scanned') IS NULL AND COALESCE(e.size, 0) <= ?2";

/// Rattrape le GPS et la date de prise de vue des photos reçues **avant** que la station ne les lise à l'envoi : au plus
/// `limit` fichiers par passage (déchiffrés un à un). Chaque fichier lu est marqué (`exif_scanned`), qu'il porte un EXIF ou
/// non : il n'est jamais relu. Les valeurs déjà notées ne sont jamais écrasées.
pub async fn backfill_exif<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, limit: usize) -> Result<BackfillStats, ThumbError> {
    let (o, lim) = (owner.to_owned(), i64::try_from(limit).unwrap_or(i64::MAX));
    let todo: Vec<(String, String, u64)> = st
        .db
        .readers()
        .read(move |c| {
            let sql = format!(
                "SELECT e.cid, (SELECT key_hex FROM ucloud_keyring k WHERE k.owner_hex = e.owner_hex AND k.cid = e.cid), MAX(COALESCE(e.size, 0)) FROM ucloud_entries e WHERE {BACKFILL_WHERE} GROUP BY e.cid ORDER BY MAX(e.mtime) DESC LIMIT ?3"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![o, i64::try_from(MAX_SOURCE_BYTES).unwrap_or(i64::MAX), lim], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0))))?;
            Ok(rows.filter_map(Result::ok).filter_map(|(cid, key, size)| key.map(|k| (cid, k, size))).collect())
        })
        .await?;
    let mut stats = BackfillStats::default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    for (cid, key_hex, size) in todo {
        let meta = async {
            if !is_cid(&cid) {
                return Ok(crate::exif::ExifMeta::default());
            }
            let key = UencKey::from_hex(&key_hex)?;
            let _permit = SLOTS.acquire().await.map_err(|_| ThumbError::Image("file d'attente fermée".into()))?;
            let (mut reader, _) = st.blobs.open(&cid).await?;
            let mut plain = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            uenc::decrypt_stream(&key, &mut reader, &mut plain).await?;
            tokio::task::spawn_blocking(move || crate::exif::read(&plain, now)).await.map_err(|e| ThumbError::Image(e.to_string()))
        }
        .await;
        // Un blob illisible (absent, clé fausse) n'est PAS marqué : il sera retenté plus tard, sans bloquer les autres.
        let Ok(meta) = meta else { continue };
        let (o, c) = (owner.to_owned(), cid.clone());
        st.db
            .writer()
            .execute(
                "UPDATE ucloud_entries SET lat = COALESCE(lat, ?3), lon = COALESCE(lon, ?4), taken_at = COALESCE(taken_at, ?5), data = json_set(data, '$.exif_scanned', 1) WHERE owner_hex = ?1 AND cid = ?2",
                vec![o.into(), c.into(), meta.lat.into(), meta.lon.into(), meta.taken_at.into()],
            )
            .await?;
        stats.scanned += 1;
        stats.found += usize::from(!meta.is_empty());
    }
    let (o, max) = (owner.to_owned(), i64::try_from(MAX_SOURCE_BYTES).unwrap_or(i64::MAX));
    stats.remaining = st
        .db
        .readers()
        .read(move |c| Ok(c.query_row(&format!("SELECT COUNT(DISTINCT e.cid) FROM ucloud_entries e WHERE {BACKFILL_WHERE}"), params![o, max], |r| r.get::<_, i64>(0))?))
        .await
        .map(|n| usize::try_from(n).unwrap_or(0))?;
    if stats.found > 0 {
        if let Some(p) = &st.publisher {
            p.notify(owner);
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use image::{ImageFormat, Rgb, RgbImage};

    use super::*;

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img).write_to(&mut out, ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_large_photo_becomes_a_small_webp_that_keeps_its_aspect_ratio() {
        let src = jpeg(1600, 1200);
        let t = render(&src).unwrap();
        assert_eq!((t.ow, t.oh), (1600, 1200));
        assert_eq!((t.w, t.h), (320, 240));
        assert_eq!(&t.webp[..4], b"RIFF");
        assert_eq!(&t.webp[8..12], b"WEBP");
        assert!(t.webp.len() < src.len() / 4, "{} octets pour {} d'original", t.webp.len(), src.len());
        // Le résultat est une image WebP valide de la bonne taille.
        let back = image::load_from_memory_with_format(&t.webp, ImageFormat::WebP).unwrap();
        assert_eq!((back.width(), back.height()), (320, 240));
    }

    #[test]
    fn a_small_image_is_not_enlarged_and_garbage_is_refused() {
        let t = render(&jpeg(100, 60)).unwrap();
        assert_eq!((t.w, t.h, t.ow, t.oh), (100, 60, 100, 60));
        assert!(matches!(render(b"ceci n'est pas une image"), Err(ThumbError::Image(_))));
        assert!(matches!(render(&[]), Err(ThumbError::Image(_))));
    }

    #[test]
    fn only_decodable_formats_are_thumbable() {
        assert!(is_thumbable("image/jpeg") && is_thumbable("image/png; charset=x") && is_thumbable("image/webp"));
        assert!(!is_thumbable("image/svg+xml") && !is_thumbable("video/mp4") && !is_thumbable("application/pdf"));
    }
}
