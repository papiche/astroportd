//! Films et séries déposés dans l'uCloud : métadonnées, sondage, plan d'encodage et événement vidéo NIP-71.
//!
//! Héritage d'`ajouter_media.sh` et de `publish_nostr_video.sh`, sans shell : tout ce qui se décide est ici, en
//! fonctions pures et testables ; l'exécution des outils externes (`ffprobe`, `ffmpeg`) et la publication sont
//! dans le binaire.
//!
//! - **Copie privée** : l'exception de l'article L. 122-5 du Code de la propriété intellectuelle ne couvre que la
//!   copie réservée à l'usage privé du copiste. Le titulaire du compte l'atteste **une fois** (kind 31903,
//!   `d = private_copy`, signé de sa clé) ; sans cette attestation, rien n'est déposé ni publié ;
//! - **Plafond frugal** : 1 024 Mo par fichier ;
//! - **Encodage** : H.264 + AAC en MP4, piste audio française conservée. Un fichier déjà conforme n'est jamais
//!   réencodé ;
//! - **Publication** : kind 21 (> 60 s) ou 22 (≤ 60 s), tag NIP-70 `["-"]`, réservé au cercle N¹ de l'auteur
//!   (voir la politique du relai). Les genres sont des étiquettes NIP-32 ; la série, la saison et l'épisode
//!   des tags de premier niveau.

use serde::Deserialize;
use serde_json::Value;

/// Plafond de taille d'un média (frugalité) : 1 024 Mo.
pub const MAX_MEDIA_BYTES: u64 = 1024 << 20;
/// Durée au-delà de laquelle une vidéo est « normale » (kind 21) ; en deçà ou égale, « courte » (kind 22).
pub const SHORT_VIDEO_SECS: u64 = 60;
/// `d`-tag de l'attestation de copie privée (kind 31903).
pub const CONSENT_D_TAG: &str = "private_copy";
/// Version du texte d'attestation.
pub const CONSENT_VERSION: u32 = 1;
/// Texte légal attesté (affiché à l'onboarding, hashé dans l'événement).
pub const CONSENT_TEXT: &str = "J'atteste que les œuvres que je dépose dans mon uCloud sont des copies réalisées pour mon usage strictement privé (article L. 122-5, 2° du Code de la propriété intellectuelle), que je les possède légitimement, et qu'elles ne seront partagées qu'avec mon cercle d'amis de confiance (N¹).";

/// Erreurs du pipeline média (pures).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaError {
    /// Fichier au-delà du plafond.
    #[error("fichier de {size} Mo : au-delà du plafond de {max} Mo (frugalité)")]
    TooLarge {
        /// Taille en Mo.
        size: u64,
        /// Plafond en Mo.
        max: u64,
    },
    /// Sortie de `ffprobe` inexploitable.
    #[error("sondage illisible : {0}")]
    BadProbe(String),
    /// Aucune piste vidéo.
    #[error("le fichier ne contient aucune piste vidéo")]
    NoVideo,
    /// Métadonnées inutilisables.
    #[error("métadonnées invalides : {0}")]
    BadMeta(String),
}

/// Vérifie le plafond de 1 024 Mo.
pub fn check_size(bytes: u64) -> Result<(), MediaError> {
    if bytes > MAX_MEDIA_BYTES {
        return Err(MediaError::TooLarge { size: bytes >> 20, max: MAX_MEDIA_BYTES >> 20 });
    }
    Ok(())
}

/// Kind NIP-71 d'une vidéo de `duration_secs` secondes.
pub fn video_kind(duration_secs: f64) -> u32 {
    if duration_secs.max(0.0) as u64 <= SHORT_VIDEO_SECS { 22 } else { 21 }
}

// ───────────────────────────── Métadonnées (TMDB) ─────────────────────────────

/// Métadonnées d'un film ou d'un épisode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaMeta {
    /// Titre (du film, ou de l'épisode si `series_name` est présent et qu'aucun nom d'épisode n'est donné).
    pub title: String,
    /// Année de sortie.
    pub year: Option<u32>,
    /// Genres (libellés).
    pub genres: Vec<String>,
    /// Nom de la série (absent pour un film).
    pub series_name: Option<String>,
    /// Saison.
    pub season_number: Option<u32>,
    /// Épisode.
    pub episode_number: Option<u32>,
    /// Titre de l'épisode.
    pub episode_name: Option<String>,
    /// Identifiant TMDB.
    pub tmdb_id: Option<u64>,
    /// Résumé.
    pub overview: Option<String>,
}

#[derive(Deserialize)]
struct RawMeta {
    #[serde(alias = "name")]
    title: Option<String>,
    year: Option<Value>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    #[serde(default)]
    genres: Vec<Value>,
    series_name: Option<String>,
    season_number: Option<Value>,
    episode_number: Option<Value>,
    #[serde(alias = "episode_title")]
    episode_name: Option<String>,
    #[serde(alias = "id")]
    tmdb_id: Option<Value>,
    overview: Option<String>,
}

fn number(v: &Option<Value>) -> Option<u64> {
    match v.as_ref()? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn clean(s: &str, max: usize) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

impl MediaMeta {
    /// Lit un document TMDB (`title`/`name`, `release_date`, `genres` en libellés ou en `{id, name}`, `series_name`,
    /// `season_number`, `episode_number`, `episode_name`). Un titre est obligatoire.
    pub fn from_tmdb(doc: &Value) -> Result<Self, MediaError> {
        let raw: RawMeta = serde_json::from_value(doc.clone()).map_err(|e| MediaError::BadMeta(e.to_string()))?;
        let year = number(&raw.year)
            .and_then(|y| u32::try_from(y).ok())
            .or_else(|| raw.release_date.as_deref().or(raw.first_air_date.as_deref()).and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()))
            .filter(|y| (1850..=2200).contains(y));
        let genres = raw
            .genres
            .iter()
            .filter_map(|g| match g {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => o.get("name").and_then(Value::as_str),
                _ => None,
            })
            .map(|g| clean(g, 40))
            .filter(|g| !g.is_empty())
            .collect();
        let series_name = raw.series_name.as_deref().map(|s| clean(s, 120)).filter(|s| !s.is_empty());
        let title = raw.title.as_deref().map(|t| clean(t, 200)).filter(|t| !t.is_empty()).ok_or_else(|| MediaError::BadMeta("titre absent".into()))?;
        let to_u32 = |v: &Option<Value>| number(v).and_then(|n| u32::try_from(n).ok());
        let (season_number, episode_number) = (to_u32(&raw.season_number), to_u32(&raw.episode_number));
        if series_name.is_some() && (season_number.is_none() || episode_number.is_none()) {
            return Err(MediaError::BadMeta("un épisode exige season_number et episode_number".into()));
        }
        Ok(Self {
            title,
            year,
            genres,
            series_name,
            season_number,
            episode_number,
            episode_name: raw.episode_name.as_deref().map(|s| clean(s, 200)).filter(|s| !s.is_empty()),
            tmdb_id: number(&raw.tmdb_id),
            overview: raw.overview.as_deref().map(|s| clean(s, 2000)).filter(|s| !s.is_empty()),
        })
    }

    /// Série ou film ?
    pub fn is_episode(&self) -> bool {
        self.series_name.is_some()
    }

    /// Titre affiché : « Série S01E02 — Titre » pour un épisode, « Titre (année) » pour un film.
    pub fn display_title(&self) -> String {
        match (&self.series_name, self.season_number, self.episode_number) {
            (Some(series), Some(s), Some(e)) => match self.episode_name.as_deref().filter(|n| !n.is_empty()).or((self.title != *series).then_some(self.title.as_str())) {
                Some(name) => format!("{series} S{s:02}E{e:02} — {name}"),
                None => format!("{series} S{s:02}E{e:02}"),
            },
            _ => match self.year {
                Some(y) => format!("{} ({y})", self.title),
                None => self.title.clone(),
            },
        }
    }

    /// Dossier d'accueil dans l'uCloud : `/Vidéos/Films` ou `/Vidéos/Séries/<série>/Saison NN`.
    pub fn folder(&self) -> String {
        match (&self.series_name, self.season_number) {
            (Some(series), Some(s)) => format!("/Vidéos/Séries/{}/Saison {s:02}", path_safe(series)),
            _ => "/Vidéos/Films".to_owned(),
        }
    }

    /// Nom du fichier : « Titre (année).ext » ou « Série S01E02.ext ».
    pub fn file_name(&self, ext: &str) -> String {
        let stem = match (&self.series_name, self.season_number, self.episode_number) {
            (Some(series), Some(s), Some(e)) => format!("{} S{s:02}E{e:02}", path_safe(series)),
            _ => match self.year {
                Some(y) => format!("{} ({y})", path_safe(&self.title)),
                None => path_safe(&self.title),
            },
        };
        format!("{stem}.{ext}")
    }
}

fn path_safe(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect::<String>().trim().trim_matches('.').to_owned()
}

/// « Science-Fiction » → « science-fiction » (tag `t`).
pub fn genre_slug(g: &str) -> String {
    let folded: String = g
        .chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect();
    folded.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

// ───────────────────────────── Sondage et plan d'encodage ─────────────────────────────

/// Piste audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Rang parmi les pistes audio (0 = première), pour `-map 0:a:N`.
    pub ordinal: usize,
    /// Codec (`aac`, `ac3`…).
    pub codec: String,
    /// Langue ISO 639 (`fra`, `eng`…), si étiquetée.
    pub lang: Option<String>,
}

impl AudioTrack {
    /// Piste française (`fra`, `fre`, `fr`).
    pub fn is_french(&self) -> bool {
        matches!(self.lang.as_deref(), Some("fra" | "fre" | "fr" | "fra-FR" | "fr-FR"))
    }
}

/// Ce que `ffprobe` a vu du fichier.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// Durée en secondes.
    pub duration: f64,
    /// Octets.
    pub size: u64,
    /// Conteneur (premier nom du format : `mov,mp4,…` → `mov`).
    pub container: String,
    /// Codec vidéo.
    pub video_codec: String,
    /// Largeur.
    pub width: u32,
    /// Hauteur.
    pub height: u32,
    /// Pistes audio.
    pub audio: Vec<AudioTrack>,
}

/// Lit la sortie de `ffprobe -print_format json -show_format -show_streams`.
pub fn parse_ffprobe(json: &str) -> Result<Probe, MediaError> {
    let v: Value = serde_json::from_str(json).map_err(|e| MediaError::BadProbe(e.to_string()))?;
    let streams = v.get("streams").and_then(Value::as_array).ok_or_else(|| MediaError::BadProbe("pas de « streams »".into()))?;
    let format = v.get("format").ok_or_else(|| MediaError::BadProbe("pas de « format »".into()))?;
    let video = streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video") && s.pointer("/disposition/attached_pic").and_then(Value::as_u64) != Some(1)).ok_or(MediaError::NoVideo)?;
    let text = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
    let num = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())));
    let duration = num(format, "duration").or_else(|| num(video, "duration")).filter(|d| d.is_finite() && *d >= 0.0).ok_or_else(|| MediaError::BadProbe("durée absente".into()))?;
    let audio = streams
        .iter()
        .filter(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"))
        .enumerate()
        .map(|(ordinal, s)| AudioTrack { ordinal, codec: text(s, "codec_name").unwrap_or_default(), lang: s.pointer("/tags/language").and_then(Value::as_str).map(|l| l.to_ascii_lowercase()).filter(|l| l != "und") })
        .collect();
    Ok(Probe {
        duration,
        size: num(format, "size").map_or(0, |s| s as u64),
        container: text(format, "format_name").and_then(|f| f.split(',').next().map(str::to_owned)).unwrap_or_default(),
        video_codec: text(video, "codec_name").unwrap_or_default(),
        width: num(video, "width").map_or(0, |w| w as u32),
        height: num(video, "height").map_or(0, |h| h as u32),
        audio,
    })
}

/// Décision d'encodage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodePlan {
    /// Piste audio retenue (rang), française si elle existe.
    pub audio_ordinal: Option<usize>,
    /// Aucune piste française : la première a été gardée, à signaler.
    pub no_french_audio: bool,
    /// Raisons pour lesquelles un réencodage H.264/AAC est nécessaire (vide : fichier conforme).
    pub reasons: Vec<String>,
}

impl EncodePlan {
    /// Faut-il réencoder ?
    pub fn needs_transcode(&self) -> bool {
        !self.reasons.is_empty()
    }
}

/// Plan d'encodage : MP4 + H.264 + AAC, une seule piste audio (la française si possible).
pub fn plan_encoding(p: &Probe) -> EncodePlan {
    let chosen = p.audio.iter().find(|a| a.is_french()).or_else(|| p.audio.first());
    let no_french_audio = !p.audio.is_empty() && !p.audio.iter().any(AudioTrack::is_french);
    let mut reasons = Vec::new();
    if p.video_codec != "h264" {
        reasons.push(format!("vidéo {} → H.264", if p.video_codec.is_empty() { "inconnue" } else { &p.video_codec }));
    }
    match chosen {
        Some(a) if a.codec != "aac" => reasons.push(format!("audio {} → AAC", if a.codec.is_empty() { "inconnu" } else { &a.codec })),
        Some(_) => {}
        None => {}
    }
    if p.audio.len() > 1 {
        reasons.push(format!("{} pistes audio → une seule (économie d'octets)", p.audio.len()));
    }
    if p.container != "mov" && p.container != "mp4" {
        reasons.push(format!("conteneur {} → MP4", if p.container.is_empty() { "inconnu" } else { &p.container }));
    }
    EncodePlan { audio_ordinal: chosen.map(|a| a.ordinal), no_french_audio, reasons }
}

/// Arguments de `ffmpeg` pour produire `output` (MP4 H.264/AAC, démarrage rapide, audio choisi).
pub fn ffmpeg_args(input: &str, output: &str, plan: &EncodePlan) -> Vec<String> {
    let mut a: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i", input, "-map", "0:v:0"].map(String::from).to_vec();
    if let Some(n) = plan.audio_ordinal {
        a.extend(["-map".to_owned(), format!("0:a:{n}")]);
    }
    a.extend(["-c:v", "libx264", "-preset", "medium", "-crf", "23", "-pix_fmt", "yuv420p"].map(String::from));
    if plan.audio_ordinal.is_some() {
        a.extend(["-c:a", "aac", "-b:a", "128k", "-ac", "2"].map(String::from));
    }
    a.extend(["-sn", "-dn", "-movflags", "+faststart", output].map(String::from));
    a
}

/// Arguments de `ffprobe`.
pub fn ffprobe_args(input: &str) -> Vec<String> {
    ["-v", "error", "-print_format", "json", "-show_format", "-show_streams", input].map(String::from).to_vec()
}

// ───────────────────────────── Événement vidéo NIP-71 ─────────────────────────────

/// Ce que l'événement vidéo référence.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoRef {
    /// CID IPFS du blob chiffré (UENC v2).
    pub cid: String,
    /// SHA-256 du clair (NIP-94 `x`).
    pub sha256: String,
    /// Octets de clair.
    pub size: u64,
    /// Type MIME.
    pub mime: String,
    /// Sondage du fichier déposé.
    pub probe: Probe,
    /// Date de publication d'origine (UNIX).
    pub published_at: u64,
}

/// Tags de l'événement vidéo (kind 21 ou 22) et son contenu.
pub fn video_event(meta: &MediaMeta, r: &VideoRef) -> (u32, Vec<Vec<String>>, String) {
    let t = |name: &str, v: &str| vec![name.to_owned(), v.to_owned()];
    let kind = video_kind(r.probe.duration);
    let duration = r.probe.duration.round() as u64;
    let dim = format!("{}x{}", r.probe.width, r.probe.height);
    let url = format!("/ipfs/{}", r.cid);
    let mut imeta = vec!["imeta".to_owned(), format!("dim {dim}"), format!("url {url}"), format!("m {}", r.mime), format!("x {}", r.sha256), format!("size {}", r.size)];
    if r.probe.duration > 0.0 {
        imeta.push(format!("duration {:.3}", r.probe.duration));
        imeta.push(format!("bitrate {}", (r.size as f64 * 8.0 / r.probe.duration) as u64));
    }
    imeta.push("encrypted uenc-v2".to_owned());
    let mut tags = vec![
        t("title", &meta.display_title()),
        t("url", &url),
        t("m", &r.mime),
        t("dim", &dim),
        imeta,
        t("size", &r.size.to_string()),
        t("duration", &duration.to_string()),
        t("published_at", &r.published_at.to_string()),
        t("x", &r.sha256),
        // Chiffré dans l'uCloud : l'URL n'est lisible que par qui reçoit la clé (lien de partage du titulaire).
        t("encrypted", "uenc-v2"),
        t("t", if meta.is_episode() { "series" } else { "film" }),
        t("t", if meta.is_episode() { "tv-show" } else { "movie" }),
        t("t", if kind == 22 { "short" } else { "regular" }),
        t("t", "private-copy"),
        // NIP-70 : événement protégé, jamais republié par un tiers.
        vec!["-".to_owned()],
        // NIP-32 : espace de noms de licence et genres.
        t("L", "genre"),
        t("L", "license"),
        vec!["l".to_owned(), "private-copy-cpi-l122-5".to_owned(), "license".to_owned()],
    ];
    for g in &meta.genres {
        let slug = genre_slug(g);
        if !slug.is_empty() {
            tags.push(vec!["l".to_owned(), g.clone(), "genre".to_owned()]);
            tags.push(t("t", &slug));
        }
    }
    if let Some(y) = meta.year {
        tags.push(t("year", &y.to_string()));
    }
    if let Some(id) = meta.tmdb_id {
        tags.push(vec!["i".to_owned(), format!("tmdb:{id}")]);
    }
    if let Some(s) = &meta.series_name {
        tags.push(t("series_name", s));
    }
    if let Some(n) = meta.season_number {
        tags.push(t("season_number", &n.to_string()));
    }
    if let Some(n) = meta.episode_number {
        tags.push(t("episode_number", &n.to_string()));
    }
    if let Some(n) = &meta.episode_name {
        tags.push(t("episode_name", n));
    }
    (kind, tags, meta.overview.clone().unwrap_or_default())
}

// ───────────────────────────── Attestation de copie privée ─────────────────────────────

/// Tags de l'attestation (kind 31903).
pub fn consent_tags() -> Vec<Vec<String>> {
    vec![
        vec!["d".into(), CONSENT_D_TAG.into()],
        vec!["v".into(), CONSENT_VERSION.to_string()],
        vec!["article".into(), "CPI L.122-5".into()],
        vec!["L".into(), "license".into()],
        vec!["l".into(), "private-copy-cpi-l122-5".into(), "license".into()],
    ]
}

/// Contenu signé de l'attestation : le texte exact accepté, pour qu'il ne puisse pas être réécrit après coup.
pub fn consent_content(accepted_at: u64) -> String {
    serde_json::json!({ "v": CONSENT_VERSION, "accepted_at": accepted_at, "text": CONSENT_TEXT }).to_string()
}

/// Lit la version attestée d'un événement 31903 `d = private_copy`. `None` : pas une attestation valide
/// (mauvaise version, texte modifié).
pub fn parse_consent(tags: &[Vec<String>], content: &str) -> Option<u32> {
    let d = tags.iter().find(|t| t.first().map(String::as_str) == Some("d")).and_then(|t| t.get(1))?;
    if d != CONSENT_D_TAG {
        return None;
    }
    let v: Value = serde_json::from_str(content).ok()?;
    (v.get("v")?.as_u64()? == u64::from(CONSENT_VERSION) && v.get("text")?.as_str()? == CONSENT_TEXT).then_some(CONSENT_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn probe(video: &str, audio: &[(&str, Option<&str>)], container: &str, duration: f64) -> Probe {
        Probe {
            duration,
            size: 50_000_000,
            container: container.into(),
            video_codec: video.into(),
            width: 1920,
            height: 1080,
            audio: audio.iter().enumerate().map(|(i, (c, l))| AudioTrack { ordinal: i, codec: (*c).into(), lang: l.map(str::to_owned) }).collect(),
        }
    }

    #[test]
    fn the_size_cap_is_1024_mb_and_the_kind_follows_the_duration() {
        assert!(check_size(MAX_MEDIA_BYTES).is_ok());
        assert_eq!(check_size(MAX_MEDIA_BYTES + 1), Err(MediaError::TooLarge { size: 1024, max: 1024 }));
        assert_eq!((video_kind(0.0), video_kind(60.0), video_kind(60.4), video_kind(61.0), video_kind(7200.0)), (22, 22, 22, 21, 21));
    }

    #[test]
    fn tmdb_documents_give_films_and_episodes() {
        let film = MediaMeta::from_tmdb(&json!({ "title": "Le Grand Bleu", "release_date": "1988-05-11", "genres": [{"id": 18, "name": "Drame"}, "Aventure"], "id": 5 })).unwrap();
        assert_eq!((film.year, film.tmdb_id, film.genres.as_slice()), (Some(1988), Some(5), ["Drame".to_owned(), "Aventure".to_owned()].as_slice()));
        assert_eq!((film.display_title().as_str(), film.folder().as_str(), film.file_name("mp4").as_str()), ("Le Grand Bleu (1988)", "/Vidéos/Films", "Le Grand Bleu (1988).mp4"));
        let ep = MediaMeta::from_tmdb(&json!({ "series_name": "Kaamelott", "title": "Kaamelott", "season_number": "2", "episode_number": 3, "episode_name": "Les Exploités", "first_air_date": "2005-01-01" })).unwrap();
        assert_eq!(ep.display_title(), "Kaamelott S02E03 — Les Exploités");
        assert_eq!((ep.folder().as_str(), ep.file_name("mp4").as_str()), ("/Vidéos/Séries/Kaamelott/Saison 02", "Kaamelott S02E03.mp4"));
        // Un épisode sans numéros, ou un document sans titre, est refusé.
        assert!(MediaMeta::from_tmdb(&json!({ "series_name": "X", "title": "X" })).is_err());
        assert!(MediaMeta::from_tmdb(&json!({ "year": 2020 })).is_err());
        // Pas de traversée de dossier par un titre.
        let evil = MediaMeta::from_tmdb(&json!({ "title": "../../etc/passwd", "series_name": "a/b", "season_number": 1, "episode_number": 1 })).unwrap();
        assert!(!evil.folder().contains("..") && evil.folder().matches('/').count() == 4, "{}", evil.folder());
        assert!(!evil.file_name("mp4").contains('/'));
    }

    #[test]
    fn ffprobe_output_is_parsed_with_french_audio_and_cover_art_ignored() {
        let out = json!({
            "format": { "format_name": "matroska,webm", "duration": "5400.5", "size": "734003200" },
            "streams": [
                { "codec_type": "video", "codec_name": "mjpeg", "disposition": { "attached_pic": 1 }, "width": 600, "height": 900 },
                { "codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 800 },
                { "codec_type": "audio", "codec_name": "ac3", "tags": { "language": "eng" } },
                { "codec_type": "audio", "codec_name": "dts", "tags": { "language": "fre" } },
                { "codec_type": "subtitle", "codec_name": "subrip" }
            ]
        })
        .to_string();
        let p = parse_ffprobe(&out).unwrap();
        assert_eq!((p.video_codec.as_str(), p.container.as_str(), p.width, p.audio.len(), p.size), ("hevc", "matroska", 1920, 2, 734_003_200));
        let plan = plan_encoding(&p);
        assert_eq!(plan.audio_ordinal, Some(1), "la piste française est choisie");
        assert!(plan.needs_transcode() && !plan.no_french_audio);
        let args = ffmpeg_args("in.mkv", "out.mp4", &plan);
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:v:0 -map 0:a:1") && joined.contains("libx264") && joined.contains("-c:a aac") && joined.ends_with("out.mp4"), "{joined}");
        assert!(!joined.contains("0:a:0"));
        assert_eq!(parse_ffprobe("{}").unwrap_err(), MediaError::BadProbe("pas de « streams »".into()));
        assert_eq!(parse_ffprobe(&json!({ "format": {}, "streams": [{ "codec_type": "audio" }] }).to_string()).unwrap_err(), MediaError::NoVideo);
    }

    #[test]
    fn a_conforming_file_is_never_re_encoded_and_a_missing_french_track_is_flagged() {
        let ok = probe("h264", &[("aac", Some("fra"))], "mov", 100.0);
        assert!(!plan_encoding(&ok).needs_transcode());
        let english = probe("h264", &[("aac", Some("eng"))], "mov", 100.0);
        let plan = plan_encoding(&english);
        assert!(!plan.needs_transcode() && plan.no_french_audio && plan.audio_ordinal == Some(0));
        let multi = probe("h264", &[("aac", Some("eng")), ("aac", Some("fra"))], "mp4", 100.0);
        let plan = plan_encoding(&multi);
        assert!(plan.needs_transcode() && plan.audio_ordinal == Some(1), "{:?}", plan.reasons);
        let silent = probe("h264", &[], "mov", 10.0);
        assert!(!plan_encoding(&silent).needs_transcode() && !ffmpeg_args("a", "b", &plan_encoding(&silent)).join(" ").contains("-c:a"));
    }

    #[test]
    fn the_video_event_carries_nip71_nip94_nip32_and_series_tags() {
        let meta = MediaMeta::from_tmdb(&json!({ "series_name": "Kaamelott", "title": "Kaamelott", "season_number": 2, "episode_number": 3, "genres": ["Comédie", "Science-Fiction"], "id": 12 })).unwrap();
        let r = VideoRef { cid: "QmCid".into(), sha256: "ab".repeat(32), size: 1000, mime: "video/mp4".into(), probe: probe("h264", &[("aac", Some("fra"))], "mov", 90.0), published_at: 1_800_000_000 };
        let (kind, tags, _) = video_event(&meta, &r);
        assert_eq!(kind, 21);
        let has = |t: &[&str]| tags.iter().any(|x| x.iter().map(String::as_str).eq(t.iter().copied()));
        for t in [
            &["series_name", "Kaamelott"][..], &["season_number", "2"], &["episode_number", "3"], &["duration", "90"], &["-"], &["L", "genre"],
            &["l", "Comédie", "genre"], &["l", "Science-Fiction", "genre"], &["t", "science-fiction"], &["t", "comedie"], &["t", "series"], &["i", "tmdb:12"], &["url", "/ipfs/QmCid"], &["encrypted", "uenc-v2"],
        ] {
            assert!(has(t), "tag manquant : {t:?}");
        }
        let imeta = tags.iter().find(|t| t[0] == "imeta").unwrap();
        assert!(imeta.contains(&"dim 1920x1080".to_owned()) && imeta.contains(&format!("x {}", "ab".repeat(32))) && imeta.iter().any(|p| p.starts_with("bitrate ")));
        let short = VideoRef { probe: probe("h264", &[], "mov", 12.0), ..r };
        let (kind, tags, _) = video_event(&MediaMeta { title: "Clip".into(), ..Default::default() }, &short);
        assert_eq!(kind, 22);
        assert!(tags.iter().any(|t| t == &["t", "short"]) && tags.iter().any(|t| t == &["t", "film"]));
    }

    #[test]
    fn the_consent_binds_the_exact_legal_text() {
        let tags = consent_tags();
        assert_eq!(parse_consent(&tags, &consent_content(1_800_000_000)), Some(CONSENT_VERSION));
        assert_eq!(parse_consent(&tags, &json!({ "v": 1, "text": "oui" }).to_string()), None, "texte réécrit");
        assert_eq!(parse_consent(&tags, &json!({ "v": 2, "text": CONSENT_TEXT }).to_string()), None, "version inconnue");
        assert_eq!(parse_consent(&[vec!["d".into(), "autre".into()]], &consent_content(1)), None);
        assert_eq!(parse_consent(&tags, "pas du json"), None);
    }
}
