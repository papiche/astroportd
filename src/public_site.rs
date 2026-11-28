//! **Site public** d'un compte : tout ce qu'un membre met dans le dossier `/Public` de son uCloud est servi **en clair, sans
//! authentification**, comme un site web. C'est le geste de publication : déplacer un fichier dans `/Public` le rend public, l'en
//! sortir le retire. On y dépose aussi des pages HTML, du CSS, des images… pour construire son site depuis un client WebDAV.
//!
//! Les fichiers restent chiffrés au repos (comme tout l'uCloud) : la station les déchiffre à la volée, uniquement pour ce dossier.
//!
//! Deux adresses, qui servent le même contenu :
//! - `/site/<nom>/…` sur n'importe quelle adresse de la station ;
//! - `https://<nom>.<zone>/…` quand le nom d'hôte est `<nom>.<zone>` (la zone DNS de la coopérative, `OVH_ZONE` de la configuration
//!   coopérative) : le même nom que l'adresse DNSLink du MULTIPASS. Elle demande un enregistrement DNS (ou un joker) et un certificat
//!   vers la station ; la station, elle, reconnaît le nom.
//!
//! `<nom>` est celui de l'adresse DNSLink (`dnslink::youser` : `alice.martin@exemple.org` → `martin.alice.exemple.org`).
//!
//! **Sécurité** : le code d'un site (scripts compris) est celui de son auteur, pas de la station. Chaque réponse porte donc une
//! politique `sandbox` (origine opaque : la page n'atteint ni la session ni l'API de la station), `nosniff` et `no-referrer`. Seul
//! `/Public` est lisible ; un chemin avec `..` ou un fichier caché (`.nom`) n'est jamais servi.

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use astro_dav::vfs::{self, Entry, EntryKind};
use astro_dav::{BlobStore, DavState, get_owner};
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde_json::{Value, json};

/// Dossier public d'un compte.
pub const DIR: &str = "/Public";
/// Politique de sécurité de toute réponse du site public : le code du membre tourne dans une origine opaque.
const CSP: &str = "sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads";
const ZONE_TTL: Duration = Duration::from_secs(60);
/// Pages distinctes comptées par site et par jour (au-delà : « (autres) »), et durée de garde des compteurs.
const MAX_PAGES_PER_DAY: i64 = 200;
const KEEP_DAYS: i64 = 90;

/// Nom public d'un compte : celui de son adresse DNSLink, en minuscules.
pub fn name_of(email: &str) -> String {
    crate::dnslink::youser(email).to_ascii_lowercase()
}

/// Chemin relatif sûr (sans `..`, sans fichier caché, sans antislash) ; `None` s'il est refusé. `""` : la racine du site.
fn clean(rest: &str) -> Option<String> {
    let mut parts = Vec::new();
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." || seg.starts_with('.') || seg.contains('\\') || seg.chars().any(char::is_control) {
            return None;
        }
        parts.push(seg);
    }
    Some(parts.join("/"))
}

/// Sonde de joignabilité d'une station d'accueil (imposée par les tests).
type Probe = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Site public des comptes de la station.
pub struct Site<B: BlobStore> {
    dav: Arc<DavState<B>>,
    uplanet_name: String,
    zone: Mutex<Option<(Instant, Option<String>)>>,
    /// Stations d'accueil sondées récemment : adresse → (quand, joignable).
    probes: Mutex<std::collections::HashMap<String, (Instant, bool)>>,
    /// File de réplication à la demande : (clé du membre, relai de sa station) confiés au crawler N².
    want: Option<tokio::sync::mpsc::UnboundedSender<(String, Option<String>)>>,
    /// Dernière demande de réplication par membre : pas plus d'une toutes les 10 min.
    asked: Mutex<std::collections::HashMap<String, Instant>>,
    /// Sonde imposée (tests : pas de réseau) ; `None` en production.
    probe: Option<Probe>,
    /// Photos nettoyées récemment servies (CID de l'original → copie nettoyée) : une photo très vue n'est pas redéchiffrée à chaque visite.
    cleaned: Mutex<std::collections::VecDeque<(String, bytes::Bytes)>>,
}

/// Taille au-delà de laquelle une photo n'est pas nettoyée (donc pas servie) : le nettoyage la tient entière en mémoire.
const MAX_CLEAN: u64 = 64 * 1024 * 1024;
/// Octets de photos nettoyées gardés en mémoire.
const CLEAN_CACHE: usize = 64 * 1024 * 1024;

impl<B: BlobStore> Site<B> {
    /// Site pour la constellation `uplanet_name` (la zone DNS vient de sa configuration coopérative).
    pub fn new(dav: Arc<DavState<B>>, uplanet_name: String) -> Self {
        Self { dav, uplanet_name, zone: Mutex::new(None), probes: Mutex::new(std::collections::HashMap::new()), probe: None, want: None, asked: Mutex::new(std::collections::HashMap::new()), cleaned: Mutex::new(std::collections::VecDeque::new()) }
    }

    /// Branche la réplication à la demande : afficher le blog d'un membre d'une autre station en rapatrie les événements ici.
    #[must_use]
    pub fn with_replicator(mut self, tx: tokio::sync::mpsc::UnboundedSender<(String, Option<String>)>) -> Self {
        self.want = Some(tx);
        self
    }

    /// Demande au crawler de rapatrier les événements de `hex` (au plus une fois toutes les 10 min par membre).
    fn ask(&self, hex: &str, home: Option<&str>) {
        let Some(tx) = &self.want else { return };
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        if asked.get(hex).is_some_and(|at| at.elapsed() < Duration::from_secs(600)) {
            return;
        }
        if asked.len() > 5000 {
            asked.clear();
        }
        asked.insert(hex.to_owned(), Instant::now());
        let relay = home.map(|u| u.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1));
        let _ = tx.send((hex.to_owned(), relay));
    }

    /// Événements du blog de `hex` déjà présents ici (articles publics, notes).
    async fn has_blog(&self, hex: &str) -> bool {
        let h = hex.to_owned();
        self.dav.db.readers().read(move |c| Ok(c.query_row("SELECT EXISTS (SELECT 1 FROM nostr_events WHERE pubkey = ?1 AND kind IN (1, 30023))", [&h], |r| r.get::<_, bool>(0))?)).await.unwrap_or(false)
    }

    /// Blog d'un membre d'une autre station, **lu ici** : la station rapatrie ses événements (synchro N² à la demande) et le rend
    /// elle-même, sans renvoyer le visiteur ailleurs. Rien encore ici : on attend la réplication quelques secondes, puis, à défaut,
    /// on redirige vers sa station si elle répond, sinon la copie de secours (vide) le dit.
    async fn mirror(&self, who: &str, hex: &str, home: Option<String>, base: &str, rest: &str, headers: &HeaderMap) -> Response {
        self.ask(hex, home.as_deref());
        if !self.has_blog(hex).await && self.want.is_some() {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if self.has_blog(hex).await {
                    break;
                }
            }
        }
        let up = match &home {
            Some(url) => self.reachable(url).await,
            None => false,
        };
        if !self.has_blog(hex).await {
            if let (true, Some(url)) = (up, &home) {
                return secure(Redirect::temporary(&format!("{url}/site/{who}/{rest}")).into_response());
            }
        }
        let files_at = match (&home, up) {
            (Some(url), true) => format!("{url}/site/{who}/"),
            _ => String::new(),
        };
        self.backup(hex, base, rest, headers, !up, &files_at).await
    }

    /// Remplace la sonde des stations d'accueil (tests seulement : aucune configuration n'y mène).
    #[must_use]
    pub fn with_probe(mut self, probe: Arc<dyn Fn(&str) -> bool + Send + Sync>) -> Self {
        self.probe = Some(probe);
        self
    }

    async fn zone(&self) -> Option<String> {
        if let Some((at, z)) = self.zone.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(at, _)| at.elapsed() < ZONE_TTL) {
            let _ = at;
            return z.clone();
        }
        let z = crate::coop_config::domain(&self.dav.db, &self.uplanet_name).await;
        *self.zone.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), z.clone()));
        z
    }

    /// Compte hébergé dont le nom public est `who` : `(clé Nostr, email)`.
    async fn owner(&self, who: &str) -> Option<(String, String)> {
        let rows: Vec<(String, String)> = self
            .dav
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT hex, email FROM identities WHERE hosted = 1 AND email IS NOT NULL AND role IN ('multipass', 'captain') LIMIT 5000")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .ok()?;
        let who = who.to_ascii_lowercase();
        rows.into_iter().find(|(_, email)| name_of(email) == who)
    }

    async fn entry(&self, hex: &str, path: &str) -> Option<Entry> {
        let (h, p) = (hex.to_owned(), path.to_owned());
        self.dav.db.readers().read(move |c| vfs::get_entry(c, &h, &p).map_err(|_| astro_db::DbError::Aborted)).await.ok().flatten()
    }

    async fn children(&self, hex: &str, path: &str) -> Vec<Entry> {
        let (h, p) = (hex.to_owned(), path.to_owned());
        self.dav.db.readers().read(move |c| vfs::list_children(c, &h, &p).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or_default()
    }

    async fn profile(&self, hex: &str) -> Value {
        let h = hex.to_owned();
        self.dav
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT content FROM nostr_events WHERE kind = 0 AND pubkey = ?1 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get::<_, String>(0)))?))
            .await
            .ok()
            .flatten()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Null)
    }

    /// Membre d'une autre station de la constellation dont le nom public est `who` : sa clé, et l'adresse publique de sa station d'accueil,
    /// lue dans le résumé de santé qu'elle publie (tag `url`, répliqué par la constellation) à partir du `home_station` de son DID
    /// (`<PeerID>:<clé du NODE>`). C'est ce qui permet à un DNS en tourniquet (plusieurs stations derrière `*.<zone>`) de mener chaque
    /// visiteur à la bonne station, et à une station amie de servir la copie de secours quand celle-ci est tombée.
    async fn remote(&self, who: &str) -> Option<(String, Option<String>)> {
        let who = who.to_ascii_lowercase();
        let rows: Vec<(String, String, Option<String>)> = self
            .dav
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT hex, email, home_station FROM identities WHERE hosted = 0 AND email IS NOT NULL LIMIT 20000")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .ok()?;
        let (hex, _, home) = rows.into_iter().find(|(_, email, _)| name_of(email) == who)?;
        let url = match home {
            Some(home) => self.dav.db.readers().read(move |c| Ok(crate::locate::home_url(c, &home)?)).await.ok().flatten(),
            None => None,
        };
        Some((hex, url))
    }

    /// La station d'accueil répond-elle (`/health`, 3 s) ? Résultat gardé 60 s : une station tombée ne ralentit pas chaque visite.
    async fn reachable(&self, url: &str) -> bool {
        if let Some(p) = &self.probe {
            return p(url);
        }
        if let Some((at, ok)) = self.probes.lock().unwrap_or_else(|e| e.into_inner()).get(url).copied() {
            if at.elapsed() < Duration::from_secs(60) {
                return ok;
            }
        }
        let limits = crate::fetch::FetchLimits { timeout: Duration::from_secs(3), max_redirects: 0, ..crate::fetch::FetchLimits::new(64 * 1024) };
        let ok = crate::fetch::fetch(&format!("{}/health", url.trim_end_matches('/')), &limits).await.is_ok();
        let mut probes = self.probes.lock().unwrap_or_else(|e| e.into_inner());
        if probes.len() > 1000 {
            probes.clear();
        }
        probes.insert(url.to_owned(), (Instant::now(), ok));
        ok
    }

    /// Blog de secours d'un membre d'une autre station, quand la sienne est injoignable : rendu ici depuis ses événements, que la
    /// synchro de la constellation a déjà apportés (articles, notes, réponses). Ses fichiers publics, eux, restent chez lui.
    async fn backup(&self, hex: &str, base: &str, rest: &str, headers: &HeaderMap, down: bool, files_at: &str) -> Response {
        let Some(rel) = clean(rest) else { return secure(not_found()) };
        if rel == "files" || rel.split('/').next().is_some_and(|s| s.contains('.') && !matches!(s, "feed.xml" | "sitemap.xml" | "robots.txt")) {
            return secure(not_found());
        }
        let (profile, origin, (peer, app)) = (self.profile(hex).await, origin_of(headers), self.links(base).await);
        let none = std::collections::HashMap::new();
        let notice = if down { "Copie de secours : la station de ce blog est injoignable, une station amie le sert. Les fichiers partagés reviendront avec elle." } else { "" };
        let ctx = crate::public_blog::Ctx { hex, peer: &peer, app: &app, replies: &none, friends: &[], notice, files_at, base, origin: &origin, profile: &profile, theme: "aurore", visits: None };
        match crate::public_blog::route(&self.dav.db, hex, &rel, &ctx, async { Vec::new() }).await {
            Some(page) => {
                let mut r = blog_response(page, false);
                r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=30"));
                r
            }
            None => secure(not_found()),
        }
    }

    /// Compte une visite (page vue) de `hex` : un compteur par jour et par page, sans adresse ni identifiant de visiteur. Se fait en tâche de
    /// fond : l'affichage de la page n'attend jamais le compteur.
    fn count_visit(&self, hex: &str, page: &str) {
        let (db, hex, page) = (self.dav.db.clone(), hex.to_owned(), page.chars().take(200).collect::<String>());
        tokio::spawn(async move {
            let day = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400).unwrap_or(0);
            let _ = db
                .writer()
                .transaction(move |tx| {
                    let known: i64 = tx.query_row("SELECT COUNT(*) FROM site_visits WHERE hex = ?1 AND day = ?2 AND path = ?3", rusqlite::params![hex, day, page], |r| r.get(0))?;
                    let distinct: i64 = tx.query_row("SELECT COUNT(*) FROM site_visits WHERE hex = ?1 AND day = ?2", rusqlite::params![hex, day], |r| r.get(0))?;
                    let page = if known == 0 && distinct >= MAX_PAGES_PER_DAY { "(autres)".to_owned() } else { page };
                    tx.execute("INSERT INTO site_visits(hex, day, path, hits) VALUES (?1, ?2, ?3, 1) ON CONFLICT(hex, day, path) DO UPDATE SET hits = hits + 1", rusqlite::params![hex, day, page])?;
                    tx.execute("DELETE FROM site_visits WHERE hex = ?1 AND day < ?2", rusqlite::params![hex, day - KEEP_DAYS])?;
                    Ok(())
                })
                .await;
        });
    }

    /// Pages vues du site de `hex` ces 30 derniers jours (pied de page public) ; `None` si la base ne répond pas.
    async fn month_visits(&self, hex: &str) -> Option<i64> {
        let (h, today) = (hex.to_owned(), i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400).unwrap_or(0));
        self.dav.db.readers().read(move |c| Ok(c.query_row("SELECT COALESCE(SUM(hits), 0) FROM site_visits WHERE hex = ?1 AND day >= ?2", rusqlite::params![h, today - 29], |r| r.get::<_, i64>(0))?)).await.ok()
    }

    /// Visites du site de `hex` : aujourd'hui, 7 jours, 90 jours, par jour sur 30 jours, et les pages les plus vues sur 7 jours.
    pub async fn visits(&self, hex: &str) -> Value {
        let (h, today) = (hex.to_owned(), i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400).unwrap_or(0));
        self.dav
            .db
            .readers()
            .read(move |c| {
                let sum = |from: i64| -> rusqlite::Result<i64> { c.query_row("SELECT COALESCE(SUM(hits), 0) FROM site_visits WHERE hex = ?1 AND day >= ?2", rusqlite::params![h, from], |r| r.get(0)) };
                let mut st = c.prepare("SELECT day, SUM(hits) FROM site_visits WHERE hex = ?1 AND day >= ?2 GROUP BY day ORDER BY day")?;
                let per_day = st.query_map(rusqlite::params![h, today - 29], |r| Ok(json!({ "day": r.get::<_, i64>(0)?, "hits": r.get::<_, i64>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
                let mut st = c.prepare("SELECT path, SUM(hits) AS n FROM site_visits WHERE hex = ?1 AND day >= ?2 GROUP BY path ORDER BY n DESC, path LIMIT 5")?;
                let top = st.query_map(rusqlite::params![h, today - 6], |r| Ok(json!({ "path": r.get::<_, String>(0)?, "hits": r.get::<_, i64>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
                Ok(json!({ "today": sum(today)?, "week": sum(today - 6)?, "days90": sum(today - 89)?, "per_day": per_day, "top": top }))
            })
            .await
            .unwrap_or(Value::Null)
    }

    /// Page `rest` du site de `hex` (`email` : son compte). `base` : préfixe des liens (`/site/<nom>` ou vide).
    pub async fn serve(&self, hex: &str, base: &str, rest: &str, headers: &HeaderMap, head: bool) -> Response {
        let Some(rel) = clean(rest) else { return secure(not_found()) };
        let path = if rel.is_empty() { DIR.to_owned() } else { format!("{DIR}/{rel}") };
        let shown = format!("/{rel}");
        let resp = match self.entry(hex, &path).await {
            Some(e) if e.kind == EntryKind::File => {
                let page = e.mime.as_deref().is_some_and(|m| m.starts_with("text/html"));
                let r = secure(self.file(hex, &path, &e, headers, head).await);
                if page && !head && r.status() == StatusCode::OK {
                    self.count_visit(hex, &shown);
                }
                return r;
            }
            Some(_) => self.directory(hex, base, &rel, &path, headers, head).await,
            None if rel.is_empty() => self.directory(hex, base, &rel, &path, headers, head).await, // /Public pas encore créé : page d'accueil par défaut
            // Pas de vrai fichier à ce chemin : une page du blog (articles, notes, flux…), sinon 404.
            None => match self.blog(hex, base, &rel, headers, head).await {
                Some(r) => r,
                None => return secure(not_found()),
            },
        };
        if !head && resp.status() == StatusCode::OK {
            self.count_visit(hex, &shown);
        }
        resp
    }

    async fn directory(&self, hex: &str, base: &str, rel: &str, path: &str, headers: &HeaderMap, head: bool) -> Response {
        let index = format!("{path}/index.html");
        if let Some(e) = self.entry(hex, &index).await.filter(|e| e.kind == EntryKind::File) {
            return secure(self.file(hex, &index, &e, headers, head).await);
        }
        // Sans index.html : l'accueil du blog à la racine, la liste du dossier ailleurs (même mise en page).
        if rel.is_empty() {
            return self.blog(hex, base, rel, headers, head).await.unwrap_or_else(|| secure(not_found()));
        }
        let entries: Vec<Entry> = self.children(hex, path).await.into_iter().filter(|e| !vfs::name_of(&e.path).starts_with('.')).collect();
        let (profile, origin, theme, (peer, app)) = (self.profile(hex).await, origin_of(headers), self.theme(hex).await, self.links(base).await);
        let none = std::collections::HashMap::new();
        let ctx = crate::public_blog::Ctx { hex, peer: &peer, app: &app, replies: &none, friends: &[], notice: "", files_at: "", base, origin: &origin, profile: &profile, theme: &theme, visits: self.month_visits(hex).await };
        blog_response(crate::public_blog::Page::Html(crate::public_blog::files_page(&ctx, rel, &entries)), head)
    }

    /// Thème du site choisi par le membre (`kv` site-theme), Aurore par défaut.
    pub async fn theme(&self, hex: &str) -> String {
        let h = hex.to_owned();
        self.dav.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = 'site-theme' AND key = ?1", [&h], |r| r.get::<_, String>(0)))?)).await.ok().flatten().filter(|t| matches!(t.as_str(), "aurore" | "sobre" | "kraft")).unwrap_or_else(|| "aurore".into())
    }

    /// Choisit le thème du site (`aurore`, `sobre` ou `kraft`).
    pub async fn set_theme(&self, hex: &str, theme: &str) -> Result<(), String> {
        if !matches!(theme, "aurore" | "sobre" | "kraft") {
            return Err("thème : aurore, sobre ou kraft".into());
        }
        let (h, t, at) = (hex.to_owned(), theme.to_owned(), i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0));
        self.dav.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('site-theme', ?1, ?2, ?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![h.into(), t.into(), at.into()]).await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// Modèle d'adresse des sites des autres membres et origine de l'application, selon que le site est servi en `/site/<nom>` ou
    /// sous son propre nom d'hôte (`<nom>.<zone>` : l'application est alors à la racine de la zone).
    async fn links(&self, base: &str) -> (String, String) {
        if !base.is_empty() {
            return ("/site/{}/".into(), String::new());
        }
        match self.zone().await {
            Some(z) => (format!("https://{{}}.{z}/"), format!("https://{z}")),
            None => ("/site/{}/".into(), String::new()),
        }
    }

    /// Range une image d'article dans `/Public/images` (nom daté, jamais écrasé) ; rend son chemin dans le site (`images/<nom>`).
    pub async fn save_image(&self, hex: &str, name: &str, bytes: Vec<u8>) -> Result<String, String> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("image trop lourde (16 Mo au plus)".into());
        }
        // Le type vient des octets, jamais du nom : seuls JPEG, PNG et WebP passent (ceux dont la station retire lieu et appareil).
        let mime = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else {
            return Err("format refusé : JPEG, PNG ou WebP".into());
        };
        crate::public_clean::clean(&bytes).map_err(|e| format!("image illisible : {e}"))?;
        let stem: String = name.rsplit_once('.').map_or(name, |(a, _)| a).chars().filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_')).take(40).collect();
        let ext = match mime {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let file = format!("{at}-{}.{ext}", if stem.is_empty() { "image" } else { &stem });
        let (h, now) = (hex.to_owned(), i64::try_from(at / 1000).unwrap_or(0));
        vfs::vfs_tx(&self.dav.db, move |tx| vfs::mkdir_p_tx(tx, &h, "/Public/images", now)).await.map_err(|e| e.to_string())?;
        astro_dav::ingest::replace(&self.dav, hex, "/Public/images", &file, Some(mime), Cursor::new(bytes)).await.map_err(|e| e.to_string())?;
        if let Some(p) = &self.dav.publisher {
            p.notify(hex);
        }
        Ok(format!("images/{file}"))
    }

    /// Page du blog au chemin `rel` (voir `public_blog`) ; `None` si ce n'en est pas une.
    async fn blog(&self, hex: &str, base: &str, rel: &str, headers: &HeaderMap, head: bool) -> Option<Response> {
        let (profile, origin, theme, (peer, app)) = (self.profile(hex).await, origin_of(headers), self.theme(hex).await, self.links(base).await);
        let none = std::collections::HashMap::new();
        let ctx = crate::public_blog::Ctx { hex, peer: &peer, app: &app, replies: &none, friends: &[], notice: "", files_at: "", base, origin: &origin, profile: &profile, theme: &theme, visits: self.month_visits(hex).await };
        let files = async { self.children(hex, DIR).await.into_iter().filter(|e| !vfs::name_of(&e.path).starts_with('.')).collect::<Vec<_>>() };
        let page = crate::public_blog::route(&self.dav.db, hex, rel, &ctx, files).await?;
        Some(blog_response(page, head))
    }

    /// Sert un fichier de `/Public` : une photo (JPEG, PNG, WebP, reconnue à ses octets) part **nettoyée** de sa position et de
    /// l'identité de l'appareil (`public_clean`) ; HEIC, AVIF et TIFF ne sont pas servis ; le reste part tel quel. L'original n'est
    /// jamais modifié. Dans le doute (fichier changé entre-temps, structure douteuse, lecture incomplète), on refuse.
    async fn file(&self, hex: &str, path: &str, e: &Entry, headers: &HeaderMap, head: bool) -> Response {
        use crate::public_clean::{Kind, SNIFF_LEN, sniff};
        let Some(cid) = e.cid.as_deref() else { return get_owner(&self.dav, hex, path, headers, head).await };
        let same = |r: &Response| r.headers().get(header::ETAG).and_then(|v| v.to_str().ok()) == Some(format!("\"{cid}\"").as_str());
        let kind = if e.size == 0 {
            Kind::Other
        } else {
            let mut h = HeaderMap::new();
            h.insert(header::RANGE, HeaderValue::from_str(&format!("bytes=0-{}", e.size.min(SNIFF_LEN) - 1)).unwrap_or(HeaderValue::from_static("bytes=0-0")));
            let r = get_owner(&self.dav, hex, path, &h, false).await;
            if !r.status().is_success() {
                return r;
            }
            if !same(&r) {
                return refuse(StatusCode::SERVICE_UNAVAILABLE, "Fichier en cours de modification : réessayez.");
            }
            match axum::body::to_bytes(r.into_body(), 64).await {
                Ok(b) => sniff(&b),
                Err(_) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, "Fichier illisible."),
            }
        };
        match kind {
            Kind::Other => {
                let r = get_owner(&self.dav, hex, path, headers, head).await;
                if r.status().is_success() && !same(&r) {
                    return refuse(StatusCode::SERVICE_UNAVAILABLE, "Fichier en cours de modification : réessayez.");
                }
                r
            }
            Kind::Refused => refuse(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Cette photo (HEIC, AVIF ou TIFF) garde des informations que la station ne sait pas retirer (lieu, appareil) : elle n'est pas servie publiquement. Publiez-la en JPEG."),
            Kind::Clean => self.clean_photo(hex, path, e, cid, headers, head).await,
        }
    }

    async fn clean_photo(&self, hex: &str, path: &str, e: &Entry, cid: &str, headers: &HeaderMap, head: bool) -> Response {
        let etag = format!("\"{cid}-public\"");
        if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*")) {
            return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
        }
        if e.size > MAX_CLEAN {
            return refuse(StatusCode::PAYLOAD_TOO_LARGE, "Photo trop lourde pour être servie publiquement sans ses métadonnées (64 Mo au plus).");
        }
        let cached = self.cleaned.lock().ok().and_then(|c| c.iter().find(|(k, _)| k == cid).map(|(_, b)| b.clone()));
        let body = match cached {
            Some(b) => b,
            None => {
                let r = get_owner(&self.dav, hex, path, &HeaderMap::new(), false).await;
                let same = r.headers().get(header::ETAG).and_then(|v| v.to_str().ok()) == Some(format!("\"{cid}\"").as_str());
                if !r.status().is_success() || !same {
                    return refuse(StatusCode::SERVICE_UNAVAILABLE, "Fichier en cours de modification : réessayez.");
                }
                let limit = usize::try_from(e.size).unwrap_or(usize::MAX).saturating_add(1);
                let raw = match axum::body::to_bytes(r.into_body(), limit).await {
                    Ok(b) if b.len() as u64 == e.size => b,
                    _ => return refuse(StatusCode::INTERNAL_SERVER_ERROR, "Photo illisible : elle n'est pas servie."),
                };
                let Ok(clean) = crate::public_clean::clean(&raw) else {
                    return refuse(StatusCode::UNPROCESSABLE_ENTITY, "Photo abîmée : la station ne peut pas en retirer le lieu et l'appareil, elle n'est donc pas servie publiquement.");
                };
                let b = bytes::Bytes::from(clean);
                if let Ok(mut c) = self.cleaned.lock() {
                    c.push_back((cid.to_owned(), b.clone()));
                    let mut total: usize = c.iter().map(|(_, b)| b.len()).sum();
                    while total > CLEAN_CACHE {
                        let Some((_, old)) = c.pop_front() else { break };
                        total -= old.len();
                    }
                }
                b
            }
        };
        let size = body.len() as u64;
        let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(|v| astro_dav::server::parse_range(v, size)) {
            Some(Err(())) => return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{size}"))]).into_response(),
            Some(Ok(r)) => r,
            None => None,
        };
        let mime = e.mime.clone().filter(|m| m.starts_with("image/")).unwrap_or_else(|| vfs::guess_mime(path).to_owned());
        let (status, part) = match range {
            Some((s, end)) => (StatusCode::PARTIAL_CONTENT, body.slice(usize::try_from(s).unwrap_or(0)..usize::try_from(end + 1).unwrap_or(body.len()))),
            None => (StatusCode::OK, body),
        };
        let mut r = Response::builder().status(status).header(header::CONTENT_TYPE, mime).header(header::ETAG, etag).header(header::ACCEPT_RANGES, "bytes").header(header::CONTENT_LENGTH, part.len());
        if let Some((s, end)) = range {
            r = r.header(header::CONTENT_RANGE, format!("bytes {s}-{end}/{size}"));
        }
        r.body(if head { axum::body::Body::empty() } else { axum::body::Body::from(part) }).unwrap_or_else(|_| refuse(StatusCode::INTERNAL_SERVER_ERROR, "Erreur interne."))
    }

    /// Nom, adresses et état du site d'un compte (écran Mon compte).
    pub async fn info(&self, hex: &str, email: &str) -> Value {
        let name = name_of(email);
        let zone = self.zone().await;
        let files = self.children(hex, DIR).await.len();
        let has_index = self.entry(hex, &format!("{DIR}/index.html")).await.is_some();
        json!({ "name": name, "path_url": format!("/site/{name}/"), "host_url": zone.as_ref().map(|z| format!("https://{name}.{z}/")), "has_dir": self.entry(hex, DIR).await.is_some(), "has_index": has_index, "files": files, "dav_dir": DIR, "visits": self.visits(hex).await })
    }

    /// Crée `/Public` s'il manque et y dépose le site d'exemple (`index.html`, `style.css`, `LISEZMOI.txt`). Refuse d'écraser une page
    /// d'accueil existante sauf `force`.
    pub async fn install_example(&self, hex: &str, force: bool) -> Result<Vec<String>, String> {
        if !force && self.entry(hex, &format!("{DIR}/index.html")).await.is_some() {
            return Err("une page d'accueil existe déjà dans /Public : elle n'est pas écrasée".into());
        }
        let (h, now) = (hex.to_owned(), i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0));
        vfs::vfs_tx(&self.dav.db, move |tx| vfs::mkdir_p_tx(tx, &h, DIR, now)).await.map_err(|e| e.to_string())?;
        let mut written = Vec::new();
        for (name, mime, text) in [("index.html", "text/html", EXAMPLE_INDEX), ("style.css", "text/css", EXAMPLE_CSS), ("LISEZMOI.txt", "text/plain", EXAMPLE_README)] {
            astro_dav::ingest::replace(&self.dav, hex, DIR, name, Some(mime), Cursor::new(text.as_bytes().to_vec())).await.map_err(|e| e.to_string())?;
            written.push(format!("{DIR}/{name}"));
        }
        Ok(written)
    }

    /// Publie (`publish`) ou retire du public des fichiers et dossiers : un **déplacement** vers `/Public` (le nom est gardé) ou hors de
    /// `/Public` (vers `/Retirés de Public`, sous-dossiers conservés). Rien n'est copié : un fichier n'existe qu'à un endroit. Rend ce qui a été
    /// déplacé et, pour chaque refus, pourquoi (nom déjà pris, déjà public…).
    pub async fn set_public(&self, hex: &str, paths: &[String], publish: bool) -> Value {
        const WITHDRAWN: &str = "/Retirés de Public";
        let (mut moved, mut refused) = (Vec::new(), Vec::new());
        let now = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0);
        for raw in paths.iter().take(500) {
            let Ok(from) = vfs::normalize_path(raw) else {
                refused.push(json!({ "path": raw, "why": "chemin invalide" }));
                continue;
            };
            let inside = from == DIR || from.starts_with("/Public/");
            let to = if publish {
                format!("{DIR}/{}", vfs::name_of(&from))
            } else if let Some(rel) = from.strip_prefix("/Public/") {
                format!("{WITHDRAWN}/{rel}")
            } else {
                String::new()
            };
            let why = match (publish, inside) {
                (true, true) => Some("déjà public"),
                (false, false) => Some("pas dans Public"),
                _ if from == "/" || from == DIR || to.is_empty() => Some("dossier Public lui-même : rien à déplacer"),
                _ if vfs::is_system(&from) => Some("le tiroir système (coffre, contacts, agenda) n'est jamais publié"),
                _ => None,
            };
            if let Some(why) = why {
                refused.push(json!({ "path": from, "why": why }));
                continue;
            }
            let (h, f, t) = (hex.to_owned(), from.clone(), to.clone());
            let done = vfs::vfs_tx(&self.dav.db, move |tx| {
                vfs::mkdir_p_tx(tx, &h, &vfs::parent_of(&t), now)?;
                vfs::move_tx(tx, &h, &f, &t, false, now).map(|_| ())
            })
            .await;
            match done {
                Ok(()) => moved.push(json!({ "from": from, "to": to })),
                Err(e) => refused.push(json!({ "path": from, "why": e.to_string() })),
            }
        }
        if !moved.is_empty() {
            if let Some(p) = &self.dav.publisher {
                p.notify(hex);
            }
        }
        json!({ "moved": moved, "refused": refused, "publish": publish })
    }

    /// Images du dossier public (jusqu'à 200, sous-dossiers compris) : de quoi choisir une image de profil ou un bandeau.
    pub async fn images(&self, hex: &str) -> Value {
        let h = hex.to_owned();
        let entries: Vec<Entry> = self.dav.db.readers().read(move |c| vfs::list_subtree(c, &h, DIR, 2000).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or_default();
        let list: Vec<Value> = entries.into_iter().filter(|e| e.kind == EntryKind::File && e.mime.as_deref().is_some_and(|m| m.starts_with("image/")) && !e.path.split('/').any(|s| s.starts_with('.'))).take(200).map(|e| json!({ "path": e.path, "name": vfs::name_of(&e.path), "size": e.size, "rel": e.path.trim_start_matches("/Public/") })).collect();
        json!({ "images": list })
    }

    /// Clair d'un fichier de `/Public` (4 Mo au plus) : sert à reprendre une image publique comme image de profil. Tout autre chemin est refusé.
    pub async fn read_public(&self, hex: &str, path: &str) -> Result<Vec<u8>, String> {
        let path = vfs::normalize_path(path).map_err(|_| "chemin invalide".to_owned())?;
        if !path.starts_with("/Public/") || path.split('/').any(|s| s.starts_with('.')) {
            return Err("seul un fichier du dossier Public peut être choisi".into());
        }
        let resp = get_owner(&self.dav, hex, &path, &HeaderMap::new(), false).await;
        if !resp.status().is_success() {
            return Err("fichier introuvable dans Public".into());
        }
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024 + 1).await.map_err(|_| "fichier illisible".to_owned())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("image trop lourde (4 Mo au plus)".into());
        }
        Ok(bytes.to_vec())
    }

    /// Routes `/site/<nom>/…`.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new().route("/site/{who}", get(redirect)).route("/site/{who}/", get(root::<B>)).route("/site/{who}/{*rest}", get(sub::<B>)).with_state(self)
    }

    /// Sert la requête si son nom d'hôte est `<nom>.<zone>` d'un compte de la station ; sinon `None`.
    pub async fn by_host(&self, host: &str, path: &str, headers: &HeaderMap, head: bool) -> Option<Response> {
        let zone = self.zone().await?;
        let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
        let who = host.strip_suffix(&format!(".{zone}"))?;
        let Some((hex, _)) = self.owner(who).await else {
            // Pas hébergé ici : le DNS (tourniquet) a mené le visiteur à une autre station que celle du membre. On le redirige vers la bonne
            // (jamais vers cette adresse-ci : pas de boucle) ; si elle est injoignable, on sert la copie de secours de son blog.
            let (rhex, url) = self.remote(who).await?;
            return Some(self.mirror(who, &rhex, url.filter(|u| !u.contains(host.as_str())), "", path.trim_start_matches('/'), headers).await);
        };
        Some(self.serve(&hex, "", path, headers, head).await)
    }
}

/// Intergiciel : une requête GET ou HEAD dont l'hôte est `<nom>.<zone>` reçoit le site public de ce compte.
pub async fn host_layer<B: BlobStore>(State(site): State<Arc<Site<B>>>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_owned);
        if let Some(host) = host {
            let head = *req.method() == Method::HEAD;
            if let Some(resp) = site.by_host(&host, req.uri().path(), req.headers(), head).await {
                return resp;
            }
        }
    }
    next.run(req).await
}

async fn redirect(Path(who): Path<String>) -> Redirect {
    Redirect::permanent(&format!("/site/{who}/"))
}

async fn root<B: BlobStore>(State(site): State<Arc<Site<B>>>, Path(who): Path<String>, headers: HeaderMap) -> Response {
    page(&site, &who, "", &headers).await
}

async fn sub<B: BlobStore>(State(site): State<Arc<Site<B>>>, Path((who, rest)): Path<(String, String)>, headers: HeaderMap) -> Response {
    page(&site, &who, &rest, &headers).await
}

async fn page<B: BlobStore>(site: &Site<B>, who: &str, rest: &str, headers: &HeaderMap) -> Response {
    let base = format!("/site/{}", who.to_ascii_lowercase());
    match site.owner(who).await {
        Some((hex, _)) => site.serve(&hex, &base, rest, headers, false).await,
        // Membre d'une autre station : son blog est lu ICI (réplication à la demande), jamais une simple redirection.
        None => match site.remote(who).await {
            Some((hex, url)) => site.mirror(&who.to_ascii_lowercase(), &hex, url, &base, rest, headers).await,
            None => secure(not_found()),
        },
    }
}

fn refuse(status: StatusCode, why: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], why.to_owned()).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><meta charset=utf-8><title>404</title><p>Page introuvable.</p>").into_response()
}

/// Réponse d'une page du blog : en-têtes du site public, avec la politique plus stricte du blog (aucun script).
fn blog_response(page: crate::public_blog::Page, head: bool) -> Response {
    let (ctype, body) = match page {
        crate::public_blog::Page::Html(b) => ("text/html; charset=utf-8", b),
        crate::public_blog::Page::Text(t, b) => (t, b),
    };
    let mut r = secure((StatusCode::OK, [(header::CONTENT_TYPE, ctype)], if head { String::new() } else { body }).into_response());
    r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(crate::public_blog::CSP));
    r
}

/// Origine absolue vue par le visiteur (flux Atom, aperçus) : `http` seulement en boucle locale ou si le relais le dit.
fn origin_of(headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).filter(|h| h.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))).unwrap_or("localhost");
    let proto = headers.get("x-forwarded-proto").and_then(|h| h.to_str().ok());
    let local = host.starts_with("localhost") || host.starts_with("127.") || host.starts_with("[::1]");
    format!("{}://{host}", if proto == Some("http") || (proto.is_none() && local) { "http" } else { "https" })
}

/// En-têtes de toute réponse du site public (voir le module).
fn secure(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("public, max-age=60"));
    r
}

const EXAMPLE_INDEX: &str = "<!doctype html>\n<html lang=\"fr\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>Ma page</title>\n  <link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <main>\n    <h1>Bonjour 👋</h1>\n    <p>Cette page est le fichier <code>index.html</code> de votre dossier <code>/Public</code>.\n       Modifiez-la depuis votre client WebDAV : elle change aussitôt.</p>\n    <h2>Mes fichiers</h2>\n    <ul>\n      <li><a href=\"LISEZMOI.txt\">LISEZMOI.txt</a></li>\n      <!-- Ajoutez vos liens : <li><a href=\"photos/vacances.jpg\">Mes vacances</a></li> -->\n    </ul>\n  </main>\n</body>\n</html>\n";

const EXAMPLE_CSS: &str = "body { margin: 0; font-family: system-ui, sans-serif; background: #f5f7fb; color: #18202f; }\nmain { max-width: 40rem; margin: 0 auto; padding: 1.5rem 1rem; }\na { color: #2a46c8; }\n@media (prefers-color-scheme: dark) { body { background: #12151f; color: #eef0f8; } a { color: #9bb0ff; } }\n";

const EXAMPLE_README: &str = "Votre dossier /Public est votre site web.\n\n- Tout fichier qui s'y trouve est PUBLIC : lisible par n'importe qui, sans mot de passe.\n- Déplacez-y un fichier pour le publier, sortez-le pour le retirer.\n- index.html est la page d'accueil ; style.css, vos images et d'autres pages HTML s'y ajoutent librement (liens relatifs : <a href=\"page2.html\">).\n- Sans index.html, la station affiche une page par défaut (votre profil et la liste des fichiers).\n- Vos fichiers restent chiffrés sur la station ; elle ne les déchiffre que pour ce dossier.\n- Les scripts d'une page tournent isolés de la station : ils n'ont accès ni à votre session ni à vos autres fichiers.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_clean_relative_paths_are_served() {
        assert_eq!(clean(""), Some(String::new()));
        assert_eq!(clean("/a//b/"), Some("a/b".into()));
        for bad in ["../x", "a/../b", "a/./b", ".env", "a/.secret", "a\\b", "a/\u{0}b"] {
            assert_eq!(clean(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_public_name_is_the_dnslink_name() {
        assert_eq!(name_of("Alice.Martin@Exemple.ORG"), "martin.alice.exemple.org");
    }

    #[test]
    fn the_origin_is_https_except_on_the_loopback() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("qo-op.com"));
        assert_eq!(origin_of(&h), "https://qo-op.com");
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
        assert_eq!(origin_of(&h), "http://127.0.0.1:8080");
        h.insert(header::HOST, HeaderValue::from_static("evil\"><x"));
        assert_eq!(origin_of(&h), "http://localhost", "un hôte douteux n'entre jamais dans les pages");
    }
}
