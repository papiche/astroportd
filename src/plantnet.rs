//! Reconnaissance **manuelle** d'une plante sur une photo, par l'API Pl@ntNet, quand la coopérative a publié sa clé
//! (`PLANTNET_API_KEY` de la configuration coopérative, kind 30800 : [`crate::coop_config`]).
//!
//! Le membre la déclenche lui-même, photo par photo (« Reconnaître la plante » dans la visionneuse) : rien ne part seul.
//! Ce qui part : une copie **réduite et ré-encodée** de la photo (petit côté 1 280 px au plus, JPEG 90 %, donc sans EXIF :
//! ni lieu, ni date, ni appareil), vers un hôte fixe ([`API`]). La clé reste sur la station (jamais dans le navigateur, ni
//! dans une ligne de commande, ni dans un journal).
//!
//! Ce qui revient entre dans la **fiche plante** de la photo ([`crate::plant_fiche`]) : propositions, espèce retenue,
//! rubriques préremplies depuis Wikipédia. Pl@ntNet ne dit ni usages ni dangers ; la station ajoute seulement, d'après une
//! courte liste embarquée ([`TOXIC`]), l'avertissement des plantes très toxiques d'Europe connues pour être confondues avec des
//! plantes comestibles. Une reconnaissance peut se tromper : l'écran le dit toujours.
//!
//! Quota : l'offre gratuite de Pl@ntNet compte 500 reconnaissances par jour et par clé (`remainingIdentificationRequests` de
//! la réponse), pour toute la constellation qui partage la clé de la coopérative.

use std::io::Cursor;
use std::time::Duration;

use astro_db::Db;
use serde_json::Value;
use sha2::Digest;

use crate::fetch::{FetchLimits, request};

/// Point d'entrée de l'API (projet « all » : toute la flore). Hôte fixe : la station ne joint jamais une autre adresse.
pub const API: &str = "https://my-api.plantnet.org/v2/identify/all";
/// Propositions gardées, de la plus probable à la moins probable.
pub const MAX_RESULTS: usize = 5;
/// Photo acceptée au plus avant réduction (le fichier du membre).
pub const MAX_PHOTO: u64 = 40 << 20;
/// Petit côté envoyé au plus : l'API réduit elle-même à 1 280 px, inutile d'envoyer plus.
pub const SIDE: u32 = 1280;
/// Réponse de l'API au plus.
const MAX_REPLY: u64 = 512 << 10;

/// Plantes très toxiques (genre entier quand tout le genre l'est, sinon espèce), avec la confusion classique.
/// Liste volontairement courte : ce ne sont que des avertissements, jamais un feu vert.
pub const TOXIC: &[(&str, &str)] = &[
    ("Aconitum", "Aconit : mortel, même au toucher prolongé."),
    ("Atropa belladonna", "Belladone : baies mortelles."),
    ("Colchicum", "Colchique : mortelle, confondue avec l'ail des ours."),
    ("Conium", "Grande ciguë : mortelle, confondue avec le persil ou le cerfeuil."),
    ("Cicuta", "Ciguë vireuse : mortelle, confondue avec le céleri ou le panais."),
    ("Aethusa cynapium", "Petite ciguë : toxique, confondue avec le persil."),
    ("Oenanthe crocata", "Œnanthe safranée : mortelle, confondue avec le céleri ou le panais."),
    ("Digitalis", "Digitale : toxique pour le cœur, confondue avec la consoude ou la bourrache."),
    ("Veratrum", "Vératre : mortel, confondu avec la gentiane jaune."),
    ("Convallaria majalis", "Muguet : toxique, feuilles confondues avec l'ail des ours."),
    ("Taxus", "If : feuillage et graines mortels."),
    ("Datura", "Datura : toxique, hallucinogène."),
    ("Hyoscyamus niger", "Jusquiame noire : toxique."),
    ("Ricinus communis", "Ricin : graines mortelles."),
    ("Nerium oleander", "Laurier-rose : toute la plante est toxique."),
    ("Laburnum", "Cytise : graines toxiques, surtout pour les enfants."),
    ("Arum maculatum", "Gouet tacheté : baies toxiques, feuilles confondues avec l'oseille ou l'ail des ours."),
];

/// Avertissement de la liste embarquée pour ce nom latin (sans auteur), s'il y en a un.
pub fn danger_of(scientific: &str) -> Option<&'static str> {
    let genus = scientific.split_whitespace().next().unwrap_or_default();
    TOXIC.iter().find(|(name, _)| if name.contains(' ') { scientific.starts_with(name) } else { genus == *name }).map(|(_, why)| *why)
}

/// Une proposition de Pl@ntNet.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Match {
    /// Nom latin sans auteur (`Urtica dioica`).
    pub scientific: String,
    /// Auteur du nom (`L.`).
    pub author: String,
    /// Noms communs dans la langue demandée.
    pub common: Vec<String>,
    /// Genre.
    pub genus: String,
    /// Famille.
    pub family: String,
    /// Confiance de l'API, en pour cent (0 à 100).
    pub score: u8,
    /// Identifiant GBIF.
    pub gbif: Option<String>,
    /// Identifiant POWO (Kew).
    pub powo: Option<String>,
    /// Avertissement de la liste embarquée, s'il y en a un.
    pub danger: Option<String>,
}

fn id_of(v: &Value) -> Option<String> {
    match v.get("id")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Lit la réponse de `POST /v2/identify/{project}` : les `MAX_RESULTS` premières propositions de `results`.
pub fn parse(body: &[u8]) -> Result<Vec<Match>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("réponse de Pl@ntNet illisible : {e}"))?;
    let results = v.get("results").and_then(Value::as_array).ok_or("réponse de Pl@ntNet sans résultats")?;
    let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    Ok(results
        .iter()
        .filter_map(|r| {
            let sp = r.get("species")?;
            let scientific = s(sp, "scientificNameWithoutAuthor");
            if scientific.is_empty() {
                return None;
            }
            // Le score est une probabilité (0 à 1) : arrondie au pour cent entier pour l'affichage.
            let score = r.get("score").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0);
            let common = sp.get("commonNames").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).take(6).collect()).unwrap_or_default();
            Some(Match {
                danger: danger_of(&scientific).map(str::to_owned),
                author: s(sp, "scientificNameAuthorship"),
                genus: sp.get("genus").map(|g| s(g, "scientificNameWithoutAuthor")).unwrap_or_default(),
                family: sp.get("family").map(|f| s(f, "scientificNameWithoutAuthor")).unwrap_or_default(),
                common,
                score: (score * 100.0).round() as u8,
                gbif: r.get("gbif").and_then(id_of),
                powo: r.get("powo").and_then(id_of),
                scientific,
            })
        })
        .take(MAX_RESULTS)
        .collect())
}

/// Copie réduite (petit côté ≤ [`SIDE`]) et ré-encodée en JPEG 90 % : aucune métadonnée de l'original ne part.
pub fn prepare(photo: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::ImageReader::new(Cursor::new(photo)).with_guessed_format().map_err(|e| e.to_string())?.decode().map_err(|_| "cette image ne se lit pas (JPEG, PNG ou WebP attendu)".to_owned())?;
    let small = img.width().min(img.height());
    let img = if small > SIDE {
        let k = f64::from(SIDE) / f64::from(small);
        img.resize((f64::from(img.width()) * k).round() as u32, (f64::from(img.height()) * k).round() as u32, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img.to_rgb8()).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Corps `multipart/form-data` : une image, organe `auto`. Rend `(type de contenu, corps)`.
fn multipart(jpeg: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("astro{}", hex::encode(&sha2::Sha256::digest(jpeg)[..12]));
    let mut b = Vec::with_capacity(jpeg.len() + 512);
    b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"organs\"\r\n\r\nauto\r\n").as_bytes());
    b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"images\"; filename=\"photo.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n").as_bytes());
    b.extend_from_slice(jpeg);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), b)
}

/// Envoie `jpeg` (déjà préparé) à `api` et rend les propositions. `Ok(vec![])` : Pl@ntNet n'y voit aucune plante (404).
pub async fn identify(api: &str, key: &str, lang: &str, jpeg: &[u8], limits: &FetchLimits) -> Result<Vec<Match>, String> {
    let mut url = url::Url::parse(api).map_err(|e| e.to_string())?;
    url.query_pairs_mut().append_pair("api-key", key).append_pair("lang", lang).append_pair("nb-results", &MAX_RESULTS.to_string()).append_pair("include-related-images", "false");
    let (ctype, body) = multipart(jpeg);
    let mut once = limits.clone();
    once.max_redirects = 0; // un envoi n'est jamais rejoué ailleurs (la clé est dans l'adresse)
    let got = match request("POST", url.as_str(), &[("content-type", ctype)], Some(bytes::Bytes::from(body)), &once).await {
        Ok(g) => g,
        Err(crate::fetch::FetchError::Status(404)) => return Ok(Vec::new()),
        // L'erreur ne recopie jamais l'adresse : elle porte la clé.
        Err(crate::fetch::FetchError::Status(401 | 403)) => return Err("la clé Pl@ntNet est refusée (celle de la station si le Capitaine en a mis une, sinon celle de la coopérative)".into()),
        Err(crate::fetch::FetchError::Status(429)) => return Err("quota Pl@ntNet épuisé pour aujourd'hui : le Capitaine peut donner la clé de sa station (Configuration coopérative › Pl@ntNet)".into()),
        Err(e) => return Err(format!("Pl@ntNet injoignable : {e}")),
    };
    let mut out = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut { got.body }, &mut out).await.map_err(|e| format!("Pl@ntNet : {e}"))?;
    if out.len() as u64 > limits.max_bytes {
        return Err("réponse de Pl@ntNet trop longue".into());
    }
    parse(&out)
}

/// Clé Pl@ntNet : celle de la station si le Capitaine en a mis une, sinon celle publiée par la coopérative.
pub async fn api_key(db: &Db, uplanet_name: &str) -> Option<String> {
    crate::coop_config::setting(db, uplanet_name, "PLANTNET_API_KEY").await
}

/// Photo `path` du membre : `(empreinte, type MIME, taille)`, ou un message si ce n'est pas une photo qui se reconnaît.
pub async fn photo(db: &Db, owner: &str, path: &str) -> Result<(String, String, i64), String> {
    let (o, p) = (owner.to_owned(), path.to_owned());
    let row: Option<(Option<String>, Option<String>, i64)> = db
        .readers()
        .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT sha_plain, mime, size FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL", [&o, &p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?))
        .await
        .map_err(|e| e.to_string())?;
    let (sha, mime, size) = row.ok_or("photo introuvable")?;
    let mime = mime.unwrap_or_default();
    if !matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp") {
        return Err("seules les photos JPEG, PNG et WebP se reconnaissent".into());
    }
    Ok((sha.filter(|s| !s.is_empty()).ok_or("photo pas encore prête : réessayez dans un instant")?, mime, size))
}

/// Réglages de l'appel en production : hôte public seulement, délai et réponse bornés.
pub fn limits() -> FetchLimits {
    let mut l = FetchLimits::new(MAX_REPLY);
    l.timeout = Duration::from_secs(30);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"{"query":{"project":"all","images":["x"],"organs":["auto"]},"language":"fr","preferedReferential":"k-world-flora",
      "bestMatch":"Urtica dioica L.","results":[
        {"score":0.873,"species":{"scientificNameWithoutAuthor":"Urtica dioica","scientificNameAuthorship":"L.","genus":{"scientificNameWithoutAuthor":"Urtica"},"family":{"scientificNameWithoutAuthor":"Urticaceae"},"commonNames":["Grande ortie","Ortie dioïque"],"scientificName":"Urtica dioica L."},"gbif":{"id":"5361866"},"powo":{"id":"30001234-2"}},
        {"score":0.05,"species":{"scientificNameWithoutAuthor":"Digitalis purpurea","scientificNameAuthorship":"L.","genus":{"scientificNameWithoutAuthor":"Digitalis"},"family":{"scientificNameWithoutAuthor":"Plantaginaceae"},"commonNames":["Digitale pourpre"]},"gbif":{"id":3172309}},
        {"score":0.01,"species":{"scientificNameWithoutAuthor":""}}],
      "version":"2025-01-17 (7.3)","remainingIdentificationRequests":487}"#;

    #[test]
    fn the_reply_gives_names_score_ids_and_dangers() {
        let m = parse(REPLY.as_bytes()).unwrap();
        assert_eq!(m.len(), 2, "une proposition sans nom est ignorée");
        assert_eq!((m[0].scientific.as_str(), m[0].family.as_str(), m[0].score, m[0].gbif.as_deref(), m[0].powo.as_deref()), ("Urtica dioica", "Urticaceae", 87, Some("5361866"), Some("30001234-2")));
        assert_eq!(m[0].common, ["Grande ortie", "Ortie dioïque"]);
        assert!(m[0].danger.is_none());
        assert_eq!(m[1].gbif.as_deref(), Some("3172309"), "un identifiant numérique est lu aussi");
        assert!(m[1].danger.as_deref().unwrap().contains("Digitale"));
        assert!(parse(b"<html>").is_err() && parse(b"{}").is_err());
    }

    #[test]
    fn dangers_match_a_whole_genus_or_one_species() {
        assert!(danger_of("Colchicum autumnale").is_some() && danger_of("Taxus baccata").is_some() && danger_of("Atropa belladonna").is_some());
        assert!(danger_of("Allium ursinum").is_none() && danger_of("Atropa acuminata").is_none() && danger_of("Conringia orientalis").is_none());
    }

    #[test]
    fn the_sent_copy_is_small_and_carries_no_metadata() {
        let img = image::RgbImage::from_pixel(3000, 2000, image::Rgb([30, 140, 40]));
        let mut src = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut src), image::ImageFormat::Png).unwrap();
        let out = prepare(&src).unwrap();
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!((back.width(), back.height()), (1920, 1280));
        assert_eq!(&out[..2], &[0xFF, 0xD8]);
        assert!(!out.windows(4).any(|w| w == b"Exif"), "aucun EXIF ne part");
        assert!(prepare(b"pas une image").is_err());
    }

    async fn fake_plantnet(status: u16) -> String {
        use axum::http::StatusCode;
        let app = axum::Router::new().route(
            "/v2/identify/all",
            axum::routing::post(move |q: axum::extract::Query<std::collections::HashMap<String, String>>, h: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                assert_eq!(q.get("api-key").map(String::as_str), Some("clé-coop"));
                assert_eq!(q.get("lang").map(String::as_str), Some("fr"));
                let ct = h["content-type"].to_str().unwrap().to_owned();
                assert!(ct.starts_with("multipart/form-data; boundary=astro"));
                let text = String::from_utf8_lossy(&body);
                assert!(text.contains("name=\"organs\"\r\n\r\nauto") && text.contains("name=\"images\"; filename=\"photo.jpg\""));
                (StatusCode::from_u16(status).unwrap(), REPLY)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v2/identify/all", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn identify_sends_the_photo_and_reads_the_answer() {
        let mut l = limits();
        l.allow_private = true;
        let url = fake_plantnet(200).await;
        let m = identify(&url, "clé-coop", "fr", b"\xFF\xD8jpeg", &l).await.unwrap();
        assert_eq!(m[0].scientific, "Urtica dioica");
        assert!(identify(&fake_plantnet(404).await, "clé-coop", "fr", b"x", &l).await.unwrap().is_empty(), "404 : aucune plante vue");
        let e = identify(&fake_plantnet(401).await, "clé-coop", "fr", b"x", &l).await.unwrap_err();
        assert!(e.contains("refusée") && !e.contains("clé-coop"), "l'erreur ne dit jamais la clé : {e}");
        // Production : une adresse locale est refusée (SSRF), la clé ne part pas.
        assert!(identify(&url, "clé-coop", "fr", b"x", &limits()).await.is_err());
    }

    /// Essai réel, à la main : `PLANTNET_KEY_FILE=<fichier 0600> PLANTNET_PHOTO=<photo> cargo test -p astroportd --lib
    /// plantnet::tests::real -- --ignored --nocapture`. La clé est lue dans le fichier, jamais passée en argument.
    #[tokio::test]
    #[ignore = "appel réel à Pl@ntNet (clé et photo données par variables d'environnement)"]
    async fn real_plantnet_call() {
        let key = std::fs::read_to_string(std::env::var("PLANTNET_KEY_FILE").unwrap()).unwrap().trim().to_owned();
        let photo = std::fs::read(std::env::var("PLANTNET_PHOTO").unwrap()).unwrap();
        let jpeg = prepare(&photo).unwrap();
        let mut url = url::Url::parse(API).unwrap();
        url.query_pairs_mut().append_pair("api-key", &key).append_pair("lang", "fr").append_pair("nb-results", "5").append_pair("include-related-images", "false");
        let (ctype, body) = multipart(&jpeg);
        let got = request("POST", url.as_str(), &[("content-type", ctype)], Some(bytes::Bytes::from(body)), &limits()).await.unwrap();
        let mut raw = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut { got.body }, &mut raw).await.unwrap();
        let v: Value = serde_json::from_slice(&raw).unwrap();
        eprintln!("clés : {:?}", v.as_object().unwrap().keys().collect::<Vec<_>>());
        eprintln!("restantes aujourd'hui : {}", v["remainingIdentificationRequests"]);
        eprintln!("premier résultat : {}", serde_json::to_string_pretty(&v["results"][0]).unwrap());
        let m = parse(&raw).unwrap();
        eprintln!("{m:#?}");
        assert!(!m.is_empty());
    }

}
