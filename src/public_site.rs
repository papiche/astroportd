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

fn esc(s: &str) -> String {
    s.chars().flat_map(|c| match c { '&' => "&amp;".chars().collect::<Vec<_>>(), '<' => "&lt;".chars().collect(), '>' => "&gt;".chars().collect(), '"' => "&quot;".chars().collect(), '\'' => "&#39;".chars().collect(), c => vec![c] }).collect()
}

fn human(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} o"),
        1024..=1_048_575 => format!("{:.0} Ko", n as f64 / 1024.0),
        _ => format!("{:.1} Mo", n as f64 / 1_048_576.0),
    }
}

/// Site public des comptes de la station.
pub struct Site<B: BlobStore> {
    dav: Arc<DavState<B>>,
    uplanet_name: String,
    zone: Mutex<Option<(Instant, Option<String>)>>,
}

impl<B: BlobStore> Site<B> {
    /// Site pour la constellation `uplanet_name` (la zone DNS vient de sa configuration coopérative).
    pub fn new(dav: Arc<DavState<B>>, uplanet_name: String) -> Self {
        Self { dav, uplanet_name, zone: Mutex::new(None) }
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

    /// Station qui héberge le site `who` quand ce n'est pas celle-ci : son adresse publique, lue dans le résumé de santé qu'elle publie (tag `url`,
    /// répliqué par la constellation) à partir de la station d'accueil du DID du membre (`home_station` = `<PeerID>:<clé du NODE>`).
    /// `None` : membre inconnu, ou station d'accueil sans adresse connue. C'est ce qui permet à un DNS en tourniquet (plusieurs stations derrière
    /// `*.<zone>`) de rediriger chaque visiteur vers la bonne station.
    async fn elsewhere(&self, who: &str) -> Option<String> {
        let who = who.to_ascii_lowercase();
        let rows: Vec<(String, String)> = self
            .dav
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT email, home_station FROM identities WHERE hosted = 0 AND email IS NOT NULL AND home_station IS NOT NULL LIMIT 20000")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .ok()?;
        let (_, home) = rows.into_iter().find(|(email, _)| name_of(email) == who)?;
        let node = home.split(':').nth(1)?.to_owned();
        let summary: String = self.dav.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = 'economic_health' AND key = ?1", [&node], |r| r.get::<_, String>(0)))?)).await.ok().flatten()?;
        let url = serde_json::from_str::<Value>(&summary).ok()?["tags"]["url"].as_str()?.trim_end_matches('/').to_owned();
        ((url.starts_with("https://") || url.starts_with("http://")) && !url[url.find("//")? + 2..].contains(['/', ' ', '?', '#'])).then_some(url)
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
                let r = secure(get_owner(&self.dav, hex, &path, headers, head).await);
                if page && !head && r.status() == StatusCode::OK {
                    self.count_visit(hex, &shown);
                }
                return r;
            }
            Some(_) => self.directory(hex, base, &rel, &path, headers, head).await,
            None if rel.is_empty() => self.directory(hex, base, &rel, &path, headers, head).await, // /Public pas encore créé : page d'accueil par défaut
            None => return secure(not_found()),
        };
        if !head && resp.status() == StatusCode::OK {
            self.count_visit(hex, &shown);
        }
        resp
    }

    async fn directory(&self, hex: &str, base: &str, rel: &str, path: &str, headers: &HeaderMap, head: bool) -> Response {
        let index = format!("{path}/index.html");
        if self.entry(hex, &index).await.is_some_and(|e| e.kind == EntryKind::File) {
            return secure(get_owner(&self.dav, hex, &index, headers, head).await);
        }
        let entries: Vec<Entry> = self.children(hex, path).await.into_iter().filter(|e| !vfs::name_of(&e.path).starts_with('.')).collect();
        let profile = if rel.is_empty() { self.profile(hex).await } else { Value::Null };
        let body = default_page(&profile, base, rel, &entries);
        secure((StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], if head { String::new() } else { body }).into_response())
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
                _ if vfs::is_vault(&from) => Some("le coffre n'est jamais publié"),
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
            // (jamais vers cette adresse-ci : pas de boucle).
            return self.elsewhere(who).await.filter(|url| !url.contains(host.as_str())).map(|url| secure(Redirect::temporary(&format!("{url}/site/{who}{path}")).into_response()));
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
    match site.owner(who).await {
        Some((hex, _)) => site.serve(&hex, &format!("/site/{}", who.to_ascii_lowercase()), rest, headers, false).await,
        None => match site.elsewhere(who).await {
            Some(url) => secure(Redirect::temporary(&format!("{url}/site/{}/{rest}", who.to_ascii_lowercase())).into_response()),
            None => secure(not_found()),
        },
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><meta charset=utf-8><title>404</title><p>Page introuvable.</p>").into_response()
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

/// Page d'accueil par défaut (aucun `index.html`) : nom, présentation, image et bandeau du profil, puis la liste des fichiers.
fn default_page(profile: &Value, base: &str, rel: &str, entries: &[Entry]) -> String {
    let text = |k: &str| profile[k].as_str().filter(|s| !s.is_empty()).map(esc);
    let img = |k: &str, cls: &str| profile[k].as_str().filter(|u| u.starts_with("https://")).map(|u| format!("<img class=\"{cls}\" src=\"{}\" alt=\"\">", esc(u)));
    let title = text("name").unwrap_or_else(|| if rel.is_empty() { "Site public".into() } else { esc(rel) });
    let mut rows = String::new();
    if !rel.is_empty() {
        rows.push_str(&format!("<li><a href=\"{base}/{}/\">⬆ ..</a></li>", esc(rel.rsplit_once('/').map_or("", |(p, _)| p)).trim_end_matches('/')));
    }
    for e in entries {
        let name = vfs::name_of(&e.path);
        let href = format!("{base}/{}{}", if rel.is_empty() { String::new() } else { format!("{rel}/") }, name);
        match e.kind {
            EntryKind::Dir => rows.push_str(&format!("<li>📁 <a href=\"{}/\">{}</a></li>", esc(&href), esc(name))),
            EntryKind::File => rows.push_str(&format!("<li>📄 <a href=\"{}\">{}</a> <small>{}</small></li>", esc(&href), esc(name), human(e.size))),
        }
    }
    if entries.is_empty() {
        rows.push_str("<li><em>Rien n'est publié pour l'instant.</em></li>");
    }
    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>{DEFAULT_CSS}</style></head><body>{banner}<main>{icon}<h1>{title}</h1>{about}<ul>{rows}</ul></main><footer>Site publié avec astroportd</footer></body></html>",
        banner = img("banner", "banner").unwrap_or_default(),
        icon = img("picture", "icon").unwrap_or_default(),
        about = text("about").map(|a| format!("<p>{a}</p>")).unwrap_or_default(),
    )
}

const DEFAULT_CSS: &str = "body{margin:0;font-family:system-ui,sans-serif;background:#f5f7fb;color:#18202f}main{max-width:42rem;margin:0 auto;padding:1rem}.banner{display:block;width:100%;max-height:14rem;object-fit:cover}.icon{width:5rem;height:5rem;border-radius:50%;object-fit:cover;margin-top:-3rem;border:4px solid #fff;background:#fff}h1{margin:.4rem 0}ul{list-style:none;padding:0}li{padding:.35rem 0;border-bottom:1px solid #0001}a{color:#2a46c8}small{color:#667}footer{text-align:center;color:#889;font-size:.8rem;padding:2rem 0}@media(prefers-color-scheme:dark){body{background:#12151f;color:#eef0f8}a{color:#9bb0ff}.icon{border-color:#12151f;background:#12151f}}";

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
    fn user_text_is_escaped_in_the_default_page() {
        let profile = json!({ "name": "<script>alert(1)</script>", "about": "a & b", "picture": "javascript:alert(1)" });
        let page = default_page(&profile, "/site/x", "", &[]);
        assert!(!page.contains("<script>") && page.contains("&lt;script&gt;") && page.contains("a &amp; b"));
        assert!(!page.contains("javascript:"), "seule une image https est reprise");
    }
}
