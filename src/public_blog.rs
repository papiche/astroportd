//! **Blog du site public** : sans `index.html` dans `/Public`, le site d'un membre devient un vrai blog, rendu ici, sans aucun script.
//!
//! Les sources sont les événements Nostr du membre (la vérité, rejouable) : ses **articles** (kind 30023, Markdown, NIP-23 : `title`,
//! `summary`, `image`, `published_at`, `t`) et ses **notes** (kind 1 qui ne répondent à personne). Rien n'est stocké en plus.
//!
//! | Adresse | Page |
//! |---|---|
//! | `/` | accueil : profil, article à la une, derniers articles, dernières notes, fichiers publics |
//! | `/blog/`, `/blog/page/<n>` | tous les articles |
//! | `/blog/<slug>` | un article (temps de lecture, étiquettes, précédent / suivant, aperçus pour les réseaux) |
//! | `/tag/<étiquette>`, `/archive/<aaaa-mm>` | articles d'une étiquette, d'un mois |
//! | `/notes/`, `/notes/page/<n>` | les notes |
//! | `/files/` | les fichiers publics |
//! | `/feed.xml`, `/sitemap.xml`, `/robots.txt` | flux Atom, plan du site |
//!
//! Un vrai fichier ou dossier de `/Public` au même chemin passe toujours avant (le membre garde la main sur son site). Le texte du membre
//! est échappé partout ; le HTML brut du Markdown est rendu comme du texte ; seuls les liens `http(s)`, `mailto` et relatifs, et les images
//! `https` ou relatives, sont repris. Ces pages ont leur propre politique : aucun script, aucune requête hors images et médias.

use astro_dav::vfs::{self, Entry, EntryKind};
use serde_json::Value;

/// Politique de sécurité des pages du blog : rien ne s'exécute, seules les images et les médias se chargent.
pub const CSP: &str = "default-src 'none'; img-src https: 'self' data:; media-src https: 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox allow-popups allow-popups-to-escape-sandbox";
const PER_PAGE: usize = 10;
const NOTES_PER_PAGE: usize = 20;
const MAX_ARTICLES: i64 = 500;

/// Un article (NIP-23).
#[derive(Debug, Clone)]
pub struct Article {
    /// Identifiant stable (tag `d`) : l'adresse de l'article.
    pub slug: String,
    /// Titre.
    pub title: String,
    /// Résumé (tag `summary`, sinon le début du texte).
    pub summary: String,
    /// Image de couverture (`https` seulement).
    pub image: Option<String>,
    /// Date de publication (tag `published_at`, sinon la date de l'événement).
    pub published: i64,
    /// Dernière modification.
    pub updated: i64,
    /// Étiquettes (tags `t`).
    pub tags: Vec<String>,
    /// Texte Markdown.
    pub content: String,
}

/// Une note (kind 1 sans réponse).
#[derive(Debug, Clone)]
pub struct Note {
    /// Identifiant de l'événement.
    pub id: String,
    /// Date.
    pub created: i64,
    /// Texte brut.
    pub content: String,
}

/// Une réponse (kind 1 qui cite une note par `e` ou un article par `a`), avec ce qu'on sait de son auteur.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Identifiant de l'événement.
    pub id: String,
    /// Note ou article visé.
    pub target: String,
    /// Clé de l'auteur.
    pub author: String,
    /// Date.
    pub created: i64,
    /// Texte.
    pub content: String,
    /// Nom affiché de l'auteur.
    pub name: String,
    /// Avatar de l'auteur (`https` ou de la station).
    pub picture: Option<String>,
    /// Nom public du site de l'auteur, s'il est membre de la constellation.
    pub site: Option<String>,
}

/// Un ami réciproque qui a un site.
#[derive(Debug, Clone)]
pub struct Friend {
    /// Nom affiché.
    pub name: String,
    /// Avatar.
    pub picture: Option<String>,
    /// Nom public de son site.
    pub site: String,
}

fn person(profile: Option<String>, email: Option<String>, hex: &str) -> (String, Option<String>, Option<String>) {
    let p: Value = profile.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or(Value::Null);
    let name = p["display_name"].as_str().or_else(|| p["name"].as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("{}…", &hex[..hex.len().min(8)]));
    let picture = p["picture"].as_str().filter(|u| u.starts_with("https://") || u.starts_with('/')).map(str::to_owned);
    (name, picture, email.map(|e| crate::public_site::name_of(&e)))
}

/// Fils de réponses des cibles (25 au plus), dans l'ordre chronologique.
pub async fn replies(db: &astro_db::Db, targets: Vec<String>) -> std::collections::HashMap<String, Vec<Reply>> {
    let mut map: std::collections::HashMap<String, Vec<Reply>> = std::collections::HashMap::new();
    let targets: Vec<String> = targets.into_iter().take(25).collect();
    if targets.is_empty() {
        return map;
    }
    let t = now();
    type Row = (String, String, String, i64, String, Option<String>, Option<String>);
    let rows: Vec<Row> = db
        .readers()
        .read(move |c| {
            let marks = vec!["?"; targets.len()].join(",");
            let sql = format!(
                "SELECT e.id, t.value, e.pubkey, e.created_at, e.content,                         (SELECT p.content FROM nostr_events p WHERE p.pubkey = e.pubkey AND p.kind = 0 ORDER BY p.created_at DESC LIMIT 1),                         (SELECT i.email FROM identities i WHERE i.hex = e.pubkey)                  FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq                  WHERE e.kind = 1 AND instr(e.tags, '[\"-\"]') = 0 AND t.name IN ('e', 'a') AND t.value IN ({marks}) AND (e.expires_at IS NULL OR e.expires_at > {t})                  ORDER BY e.created_at LIMIT 2000"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(rusqlite::params_from_iter(targets.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for (id, target, author, created, content, profile, email) in rows {
        if !seen.insert((id.clone(), target.clone())) {
            continue;
        }
        let (name, picture, site) = person(profile, email, &author);
        map.entry(target.clone()).or_default().push(Reply { id, target, author, created, content, name, picture, site });
    }
    map
}

/// Clés des amis réciproques du membre (il les suit, ils le suivent).
pub async fn reciprocal(db: &astro_db::Db, hex: &str) -> Vec<String> {
    let h = hex.to_owned();
    db.readers()
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT g.pubkey FROM social_graph g WHERE g.owner_hex = ?1 AND g.level = 1 \
                   AND EXISTS (SELECT 1 FROM nostr_events k JOIN nostr_tags t ON t.event_id = k.seq WHERE k.kind = 3 AND k.pubkey = g.pubkey AND t.name = 'p' AND t.value = ?1) LIMIT 2000",
            )?;
            let rows = st.query_map([&h], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default()
}

/// Amis réciproques du membre (il les suit, ils le suivent) dont on connaît le site.
pub async fn friends(db: &astro_db::Db, hex: &str) -> Vec<Friend> {
    let h = hex.to_owned();
    let rows: Vec<(String, Option<String>, Option<String>)> = db
        .readers()
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT g.pubkey, (SELECT p.content FROM nostr_events p WHERE p.pubkey = g.pubkey AND p.kind = 0 ORDER BY p.created_at DESC LIMIT 1),                         (SELECT i.email FROM identities i WHERE i.hex = g.pubkey)                  FROM social_graph g WHERE g.owner_hex = ?1 AND g.level = 1                    AND EXISTS (SELECT 1 FROM nostr_events k JOIN nostr_tags t ON t.event_id = k.seq WHERE k.kind = 3 AND k.pubkey = g.pubkey AND t.name = 'p' AND t.value = ?1)                  LIMIT 200",
            )?;
            let rows = st.query_map([&h], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut out: Vec<Friend> = rows
        .into_iter()
        .filter_map(|(hex, profile, email)| {
            let (name, picture, site) = person(profile, email, &hex);
            site.map(|site| Friend { name, picture, site })
        })
        .collect();
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

/// Ce qu'il faut pour rendre une page : préfixe des liens, origine absolue (flux, aperçus), profil et fichiers publics.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// Clé Nostr du membre (adresses des articles, liens « Répondre »).
    pub hex: &'a str,
    /// Adresse du site d'un autre membre : `{}` est remplacé par son nom public (`/site/{}/` ou `https://{}.zone/`).
    pub peer: &'a str,
    /// Origine de l'application de la station (vide : la même que le site), pour « Répondre ».
    pub app: &'a str,
    /// Fils de réponses, par cible (identifiant de note ou adresse `30023:<auteur>:<d>`).
    pub replies: &'a std::collections::HashMap<String, Vec<Reply>>,
    /// Amis réciproques du membre qui ont un site connu.
    pub friends: &'a [Friend],
    /// Bandeau de la copie de secours (station d'accueil injoignable : une station amie sert le blog), sinon vide.
    pub notice: &'a str,
    /// Blog d'un membre lu ici mais hébergé ailleurs : adresse de ses fichiers partagés sur sa station (vide : fichiers d'ici).
    pub files_at: &'a str,
    /// Préfixe des liens (`/site/<nom>` ou vide).
    pub base: &'a str,
    /// Origine absolue (`https://station`), pour le flux et les aperçus.
    pub origin: &'a str,
    /// Profil du membre (kind 0).
    pub profile: &'a Value,
    /// Thème : `aurore`, `sobre` ou `kraft`.
    pub theme: &'a str,
    /// Pages vues ces 30 derniers jours (compteur sans cookie ni identifiant de visiteur), montré en pied de page ; `None` : rien.
    pub visits: Option<i64>,
}

/// Réponse d'une page du blog.
pub enum Page {
    /// Page HTML.
    Html(String),
    /// Autre texte (type MIME, corps).
    Text(&'static str, String),
}

fn now() -> i64 {
    i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0)
}

/// Articles du membre, du plus récent au plus ancien.
pub async fn articles(db: &astro_db::Db, hex: &str) -> Vec<Article> {
    let (h, t) = (hex.to_owned(), now());
    let rows: Vec<(String, String, String, i64)> = db
        .readers()
        .read(move |c| {
            let mut st = c.prepare("SELECT content, tags, d_tag, created_at FROM nostr_events WHERE pubkey = ?1 AND kind = 30023 AND d_tag IS NOT NULL AND d_tag <> '' AND instr(tags, '[\"-\"]') = 0 AND (expires_at IS NULL OR expires_at > ?2) ORDER BY created_at DESC LIMIT ?3")?;
            let rows = st.query_map(rusqlite::params![h, t, MAX_ARTICLES], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let t = now();
    let mut list: Vec<Article> = rows.into_iter().map(|(content, tags, d, created)| parse_article(&content, &tags, d, created)).filter(|a| a.published <= t).collect();
    list.sort_by(|a, b| b.published.cmp(&a.published).then_with(|| a.slug.cmp(&b.slug)));
    list
}

/// Notes du membre (sans les réponses), à partir de `offset` ; `limit + 1` sont lues pour savoir s'il y a une suite.
pub async fn notes(db: &astro_db::Db, hex: &str, limit: usize, offset: usize) -> (Vec<Note>, bool) {
    let (h, t) = (hex.to_owned(), now());
    let (lim, off) = (i64::try_from(limit + 1).unwrap_or(21), i64::try_from(offset).unwrap_or(0));
    let mut rows: Vec<Note> = db
        .readers()
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT id, content, created_at FROM nostr_events e WHERE pubkey = ?1 AND kind = 1 AND instr(tags, '[\"-\"]') = 0 AND (expires_at IS NULL OR expires_at > ?2) \
                 AND NOT EXISTS (SELECT 1 FROM nostr_tags t WHERE t.event_id = e.seq AND t.name = 'e') ORDER BY created_at DESC LIMIT ?3 OFFSET ?4",
            )?;
            let rows = st.query_map(rusqlite::params![h, t, lim, off], |r| Ok(Note { id: r.get(0)?, content: r.get(1)?, created: r.get(2)? }))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let more = rows.len() > limit;
    rows.truncate(limit);
    (rows, more)
}

/// Lit un article depuis son contenu et ses tags (JSON).
pub fn parse_article(content: &str, tags: &str, slug: String, created: i64) -> Article {
    let tags: Vec<Vec<String>> = serde_json::from_str(tags).unwrap_or_default();
    let first = |name: &str| tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let mut topics: Vec<String> = tags.iter().filter(|t| t.first().map(String::as_str) == Some("t")).filter_map(|t| t.get(1)).map(|s| s.trim().trim_start_matches('#').to_lowercase()).filter(|s| !s.is_empty() && s.len() <= 60).collect();
    topics.dedup();
    topics.truncate(12);
    let heading = content.lines().find_map(|l| l.strip_prefix("# ")).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let title = first("title").or(heading).unwrap_or_else(|| slug.clone());
    let summary = first("summary").unwrap_or_else(|| excerpt(content, 220));
    let published = first("published_at").and_then(|s| s.parse::<i64>().ok()).filter(|&p| p > 0).unwrap_or(created);
    let image = first("image").filter(|u| u.starts_with("https://"));
    Article { slug, title, summary, image, published, updated: created, tags: topics, content: content.to_owned() }
}

/// Début d'un texte Markdown, en texte brut, coupé proprement à `max` caractères.
fn excerpt(md: &str, max: usize) -> String {
    let mut plain = String::new();
    for ev in pulldown_cmark::Parser::new(md) {
        match ev {
            pulldown_cmark::Event::Text(t) | pulldown_cmark::Event::Code(t) => plain.push_str(&t),
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak | pulldown_cmark::Event::End(_) => plain.push(' '),
            _ => {}
        }
        if plain.chars().count() > max * 2 {
            break;
        }
    }
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.chars().count() <= max {
        return plain;
    }
    let cut: String = plain.chars().take(max).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a).trim_end_matches([',', ';', ':', '.']))
}

/// Échappe un texte pour le HTML (et les attributs).
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    // Typographie française : une espace fine insécable avant ? ! ; et : (jamais de « ? » seul en début de ligne).
    for p in ["?", "!", ";", ":"] {
        o = o.replace(&format!(" {p}"), &format!("\u{202F}{p}"));
    }
    o.replace("« ", "«\u{202F}").replace(" »", "\u{202F}»")
}

/// Encode un segment d'adresse (étiquette, identifiant d'article).
fn seg(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(char::from(b));
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Lien sûr : `http(s)`, `mailto`, ancre ou relatif ; tout le reste (`javascript:`, `data:`…) devient `#`.
fn safe_link(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:") || l.starts_with('#') || l.starts_with('/') || !l.contains(':')
}

fn safe_image(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    l.starts_with("https://") || l.starts_with('/') || !l.contains(':')
}

/// Markdown → HTML sûr : le HTML brut devient du texte, les liens et images douteux sont neutralisés.
pub fn markdown(md: &str) -> String {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES | Options::ENABLE_TASKLISTS | Options::ENABLE_SMART_PUNCTUATION;
    let events = Parser::new_ext(md, opts).map(|ev| match ev {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe_link(&dest_url) => Event::Start(Tag::Link { link_type, dest_url: CowStr::Borrowed("#"), title, id }),
        Event::Start(Tag::Image { link_type, dest_url, title, id }) if !safe_image(&dest_url) => Event::Start(Tag::Image { link_type, dest_url: CowStr::Borrowed(""), title, id }),
        other => other,
    });
    let mut out = String::with_capacity(md.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut out, events);
    // Liens externes : nouvel onglet, sans transmettre l'adresse ; images : chargement paresseux.
    out.replace("<a href=\"http", "<a rel=\"noopener nofollow\" target=\"_blank\" href=\"http").replace("<img ", "<img loading=\"lazy\" decoding=\"async\" ")
}

/// Texte d'une note → HTML : échappé, adresses rendues cliquables (images montrées), sauts de ligne gardés.
pub fn note_html(text: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        for (j, word) in line.split(' ').enumerate() {
            if j > 0 {
                out.push(' ');
            }
            let w = word.trim_end_matches([',', '.', ';', ')', '!', '?']);
            let tail = &word[w.len()..];
            if w.starts_with("https://") && w.len() > 8 {
                let l = w.to_ascii_lowercase();
                if [".jpg", ".jpeg", ".png", ".webp", ".gif", ".avif"].iter().any(|x| l.split('?').next().unwrap_or("").ends_with(x)) {
                    out.push_str(&format!("<img loading=\"lazy\" decoding=\"async\" src=\"{}\" alt=\"\">", esc(w)));
                } else {
                    let shown: String = w.trim_start_matches("https://").chars().take(48).collect();
                    out.push_str(&format!("<a rel=\"noopener nofollow\" target=\"_blank\" href=\"{}\">{}{}</a>", esc(w), esc(&shown), if w.len() > 56 { "…" } else { "" }));
                }
                out.push_str(&esc(tail));
            } else {
                out.push_str(&esc(word));
            }
        }
    }
    out
}

/// Jour civil (année, mois, jour) d'un horodatage Unix (algorithme de H. Hinnant).
fn civil(ts: i64) -> (i64, u32, u32, i64) {
    let (days, secs) = (ts.div_euclid(86_400), ts.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (yoe + era * 400 + i64::from(m <= 2), m, d, secs)
}

const MOIS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];

/// « 21 novembre 2026 ».
pub fn fr_date(ts: i64) -> String {
    let (y, m, d, _) = civil(ts);
    format!("{d} {} {y}", MOIS[(m as usize).saturating_sub(1).min(11)])
}

fn month_label(key: &str) -> String {
    let (y, m) = key.split_once('-').unwrap_or((key, "1"));
    let m: usize = m.parse().unwrap_or(1);
    let name = MOIS[m.saturating_sub(1).min(11)];
    let mut c = name.chars();
    format!("{}{} {y}", c.next().map(|f| f.to_uppercase().to_string()).unwrap_or_default(), c.as_str())
}

fn month_key(ts: i64) -> String {
    let (y, m, _, _) = civil(ts);
    format!("{y:04}-{m:02}")
}

fn rfc3339(ts: i64) -> String {
    let (y, m, d, s) = civil(ts);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// Minutes de lecture (220 mots par minute, au moins une).
pub fn reading_minutes(md: &str) -> usize {
    md.split_whitespace().count().div_ceil(220).max(1)
}

/// Aiguillage des pages virtuelles. `rel` : chemin relatif déjà nettoyé. `None` : pas une page du blog.
pub async fn route(db: &astro_db::Db, hex: &str, rel: &str, ctx: &Ctx<'_>, files: impl std::future::Future<Output = Vec<Entry>>) -> Option<Page> {
    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let page_n = |s: &str| s.parse::<usize>().ok().filter(|n| (1..=10_000).contains(n));
    Some(match parts.as_slice() {
        [] => {
            let (arts, (ns, _), fs, fr) = (articles(db, hex).await, notes(db, hex, 5, 0).await, files.await, friends(db, hex).await);
            let rp = replies(db, ns.iter().map(|n| n.id.clone()).collect()).await;
            Page::Html(home(&Ctx { replies: &rp, friends: &fr, ..*ctx }, &arts, &ns, &fs))
        }
        ["blog"] => Page::Html(list(ctx, &articles(db, hex).await, ListOf::All, 1)?),
        ["blog", "page", n] => Page::Html(list(ctx, &articles(db, hex).await, ListOf::All, page_n(n)?)?),
        ["blog", slug] => {
            let arts = articles(db, hex).await;
            let i = arts.iter().position(|a| a.slug == *slug)?;
            let rp = replies(db, vec![coord(hex, slug)]).await;
            Page::Html(article(&Ctx { replies: &rp, ..*ctx }, &arts, i))
        }
        ["tag", t] => Page::Html(list(ctx, &articles(db, hex).await, ListOf::Tag(&t.to_lowercase()), 1)?),
        ["archive", m] if m.len() == 7 && m.as_bytes()[4] == b'-' => Page::Html(list(ctx, &articles(db, hex).await, ListOf::Month(m), 1)?),
        ["notes"] => notes_page(ctx, db, hex, 1).await?,
        ["notes", "page", n] => notes_page(ctx, db, hex, page_n(n)?).await?,
        ["files"] => Page::Html(files_page(ctx, "", &files.await)),
        ["repondre", code] => Page::Html(reply_page(ctx, reply_code(code)?)),
        ["feed.xml"] => Page::Text("application/atom+xml; charset=utf-8", feed(ctx, &articles(db, hex).await)),
        ["sitemap.xml"] => Page::Text("application/xml; charset=utf-8", sitemap(ctx, &articles(db, hex).await)),
        ["robots.txt"] => Page::Text("text/plain; charset=utf-8", format!("User-agent: *\nAllow: /\nSitemap: {}{}/sitemap.xml\n", ctx.origin, ctx.base)),
        _ => return None,
    })
}

async fn notes_page(ctx: &Ctx<'_>, db: &astro_db::Db, hex: &str, n: usize) -> Option<Page> {
    let (ns, more) = notes(db, hex, NOTES_PER_PAGE, (n - 1) * NOTES_PER_PAGE).await;
    if ns.is_empty() && n > 1 {
        return None;
    }
    let rp = replies(db, ns.iter().map(|n| n.id.clone()).collect()).await;
    let ctx = &Ctx { replies: &rp, ..*ctx };
    let b = ctx.base;
    let mut body = String::from("<section class=\"wrap\"><h1 class=\"page-title\">Notes</h1>");
    body.push_str(&notes_list(ctx, &ns));
    if ns.is_empty() {
        body.push_str("<p class=\"empty\">Aucune note publiée pour l'instant.</p>");
    }
    body.push_str(&pager(n, more, &format!("{b}/notes/")));
    body.push_str("</section>");
    Some(Page::Html(layout(ctx, &Meta::page("Notes", &name(ctx)), "notes", &body)))
}

enum ListOf<'a> {
    All,
    Tag(&'a str),
    Month(&'a str),
}

fn list(ctx: &Ctx<'_>, arts: &[Article], of: ListOf<'_>, n: usize) -> Option<String> {
    let b = ctx.base;
    let (title, kept, root): (String, Vec<&Article>, String) = match of {
        ListOf::All => ("Articles".into(), arts.iter().collect(), format!("{b}/blog/")),
        ListOf::Tag(t) => (format!("#{t}"), arts.iter().filter(|a| a.tags.iter().any(|x| x == t)).collect(), format!("{b}/tag/{}", seg(t))),
        ListOf::Month(m) => (month_label(m), arts.iter().filter(|a| month_key(a.published) == m).collect(), format!("{b}/archive/{m}")),
    };
    if kept.is_empty() && !matches!(of, ListOf::All) {
        return None;
    }
    let start = (n - 1) * PER_PAGE;
    if start > 0 && start >= kept.len() {
        return None;
    }
    let shown = &kept[start..kept.len().min(start + PER_PAGE)];
    let mut body = format!("<section class=\"wrap\"><h1 class=\"page-title\">{}</h1><p class=\"count\">{} article{}</p>", esc(&title), kept.len(), if kept.len() > 1 { "s" } else { "" });
    if kept.is_empty() {
        body.push_str("<p class=\"empty\">Aucun article publié pour l'instant.</p>");
    }
    body.push_str("<div class=\"cards\">");
    for a in shown {
        body.push_str(&card(ctx, a));
    }
    body.push_str("</div>");
    body.push_str(&pager(n, start + PER_PAGE < kept.len(), &root));
    body.push_str(&explore(ctx, arts));
    body.push_str("</section>");
    Some(layout(ctx, &Meta::page(&title, &name(ctx)), "blog", &body))
}

/// Étiquettes et archives : de quoi se promener dans le blog.
fn explore(ctx: &Ctx<'_>, arts: &[Article]) -> String {
    if arts.is_empty() {
        return String::new();
    }
    let b = ctx.base;
    let mut tags: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut months: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for a in arts {
        for t in &a.tags {
            *tags.entry(t).or_default() += 1;
        }
        *months.entry(month_key(a.published)).or_default() += 1;
    }
    let mut tags: Vec<(&str, usize)> = tags.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut o = String::from("<aside class=\"explore\">");
    if !tags.is_empty() {
        o.push_str("<div><h2>Étiquettes</h2><p class=\"tags\">");
        for (t, n) in tags.iter().take(30) {
            o.push_str(&format!("<a class=\"tag\" href=\"{b}/tag/{}\">#{}<small>{n}</small></a> ", seg(t), esc(t)));
        }
        o.push_str("</p></div>");
    }
    o.push_str("<div><h2>Archives</h2><ul class=\"months\">");
    for (m, n) in months.iter().rev().take(24) {
        o.push_str(&format!("<li><a href=\"{b}/archive/{m}\">{}</a> <small>{n}</small></li>", month_label(m)));
    }
    o.push_str("</ul></div></aside>");
    o
}

fn pager(n: usize, more: bool, root: &str) -> String {
    if n == 1 && !more {
        return String::new();
    }
    let root = root.trim_end_matches('/');
    let prev = match n {
        1 => String::new(),
        2 => format!("<a rel=\"prev\" href=\"{root}/\">← Plus récents</a>"),
        _ => format!("<a rel=\"prev\" href=\"{root}/page/{}\">← Plus récents</a>", n - 1),
    };
    let next = if more { format!("<a rel=\"next\" href=\"{root}/page/{}\">Plus anciens →</a>", n + 1) } else { String::new() };
    format!("<nav class=\"pager\" aria-label=\"Pages\">{prev}<span>Page {n}</span>{next}</nav>")
}

fn card(ctx: &Ctx<'_>, a: &Article) -> String {
    let b = ctx.base;
    let img = a.image.as_deref().map(|u| format!("<img class=\"cover\" loading=\"lazy\" decoding=\"async\" src=\"{}\" alt=\"\">", esc(u))).unwrap_or_else(|| format!("<div class=\"cover none\" aria-hidden=\"true\">{}</div>", esc(&a.title.chars().next().unwrap_or('·').to_string())));
    format!(
        "<article class=\"card\"><a class=\"card-link\" href=\"{b}/blog/{}\">{img}<div class=\"card-body\"><p class=\"meta\"><time datetime=\"{}\">{}</time> · {} min</p><h3>{}</h3><p>{}</p></div></a></article>",
        seg(&a.slug),
        rfc3339(a.published),
        fr_date(a.published),
        reading_minutes(&a.content),
        esc(&a.title),
        esc(&a.summary)
    )
}

/// Adresse NIP-33 d'un article : la cible des commentaires.
pub fn coord(hex: &str, slug: &str) -> String {
    format!("30023:{hex}:{slug}")
}

/// Adresse NIP-19 (`nevent` ou `naddr`) en TLV bech32 : 0 = identifiant (32 octets ou `d`), 1 = relai, 2 = auteur, 3 = kind.
fn nip19(hrp: &str, special: &[u8], relay: &str, author: &str, kind: Option<u32>) -> Option<String> {
    let mut data = Vec::new();
    let mut tlv = |t: u8, v: &[u8]| -> Option<()> {
        data.push(t);
        data.push(u8::try_from(v.len()).ok()?);
        data.extend_from_slice(v);
        Some(())
    };
    tlv(0, special)?;
    if !relay.is_empty() {
        tlv(1, relay.as_bytes())?;
    }
    tlv(2, &hex::decode(author).ok().filter(|k| k.len() == 32)?)?;
    if let Some(k) = kind {
        tlv(3, &k.to_be_bytes())?;
    }
    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse(hrp).ok()?, &data).ok()
}

/// `nevent` d'une note (NIP-19), avec ce relai comme indication.
pub fn nevent(id: &str, author: &str, relay: &str) -> Option<String> {
    nip19("nevent", &hex::decode(id).ok().filter(|i| i.len() == 32)?, relay, author, Some(1))
}

/// `naddr` d'un article (NIP-19).
pub fn naddr(d: &str, author: &str, relay: &str) -> Option<String> {
    nip19("naddr", d.as_bytes(), relay, author, Some(30_023))
}

/// Relai de la station vu du visiteur (`wss://…`), indiqué dans les adresses Nostr.
fn relay_of(ctx: &Ctx<'_>) -> String {
    ctx.origin.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1)
}

/// « Répondre » : une page de ce site ([`reply_page`]) qui guide le visiteur. Un lien `nostr:` ou `web+astro:` nu ne mène nulle
/// part dans un navigateur qui ne connaît pas ces adresses (le cas de presque tous les visiteurs).
fn reply_link(ctx: &Ctx<'_>, code: Option<String>, label: &str) -> String {
    match code {
        Some(c) => format!("<p class=\"reply\"><a href=\"{}/repondre/{c}\" rel=\"nofollow\">💬 {label}</a></p>", ctx.base),
        None => String::new(),
    }
}

/// Adresse Nostr (`nevent1…` ou `naddr1…`) reçue dans un chemin : bech32 seulement, longueur bornée.
fn reply_code(code: &str) -> Option<&str> {
    (code.len() <= 600 && (code.starts_with("nevent1") || code.starts_with("naddr1")) && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())).then_some(code)
}

/// Page « Répondre » : chaque visiteur trouve son chemin, sans script. Le relai de la station n'accepte que son cercle : la réponse
/// part de l'application Astroport du visiteur (cette station ou la sienne), la synchro N² la ramène sous la note.
/// 1. Compte sur cette station : se connecter, l'éditeur s'ouvre sur la réponse (`/#/write?nostr=…`).
/// 2. Compte sur une autre station : `web+astro:` ouvre SON application (si ce navigateur l'a enregistrée), sinon l'adresse à copier.
/// 3. Pas de compte : créer son MULTIPASS. 4. Une autre application Nostr : `nostr:`.
fn reply_page(ctx: &Ctx<'_>, code: &str) -> String {
    let (app, who) = (ctx.app, esc(&name(ctx)));
    let body = format!(
        "<section class=\"wrap answer\"><h1 class=\"page-title\">Répondre à {who}</h1>\
<p class=\"lead\">Votre réponse part de <strong>votre</strong> application Astroport, signée par vous, puis apparaît ici sous la publication.</p>\
<ol class=\"ways\">\
<li><h2>J'ai un compte sur cette station</h2><p>Connectez-vous : l'éditeur s'ouvre, prêt à répondre.</p><p><a class=\"go\" href=\"{app}/#/write?nostr={code}\">Me connecter et répondre</a></p></li>\
<li><h2>Mon compte est sur une autre station</h2><p><a class=\"go\" href=\"web+astro:{code}\">Ouvrir mon application Astroport</a></p><p class=\"small\">Rien ne s'ouvre ? Ce navigateur ne connaît pas encore votre station : ouvrez-la une fois et acceptez qu'elle ouvre les liens Astroport, ou collez cette adresse dans « Mon blog » de votre station :</p><p><code class=\"code\">{code}</code></p><p class=\"small\"><a href=\"{app}/stations#membre\">Je ne sais plus où est ma station</a></p></li>\
<li><h2>Je n'ai pas encore de compte</h2><p>Créez votre MULTIPASS en une minute : un e-mail suffit, sans publicité.</p><p><a class=\"go\" href=\"{app}/register\">Créer mon MULTIPASS</a></p></li>\
<li><h2>J'utilise une autre application Nostr</h2><p><a href=\"nostr:{code}\">Ouvrir dans mon application Nostr</a> · l'adresse ci-dessus s'y colle aussi.</p></li>\
</ol><p><a href=\"{b}/\">← Revenir au site de {who}</a></p></section>",
        b = ctx.base
    );
    layout(ctx, &Meta::page("Répondre", &name(ctx)), "", &body)
}

/// Fil de réponses d'une cible, replié par défaut ; chaque auteur membre mène à son blog.
fn thread(ctx: &Ctx<'_>, target: &str, open: bool) -> String {
    let Some(list) = ctx.replies.get(target).filter(|l| !l.is_empty()) else { return String::new() };
    let mut o = format!("<details class=\"thread\"{}><summary>{} réponse{}</summary><ol>", if open { " open" } else { "" }, list.len(), if list.len() > 1 { "s" } else { "" });
    for r in list {
        let face = r.picture.as_deref().map(|u| format!("<img class=\"avatar\" src=\"{}\" alt=\"\">", esc(u))).unwrap_or_else(|| format!("<span class=\"avatar letter\" aria-hidden=\"true\">{}</span>", esc(&r.name.chars().next().unwrap_or('·').to_uppercase().to_string())));
        let who = match &r.site {
            Some(site) => format!("<a class=\"who\" href=\"{}\">{face}<b>{}</b></a>", esc(&ctx.peer.replace("{}", site)), esc(&r.name)),
            None => format!("<span class=\"who\">{face}<b>{}</b></span>", esc(&r.name)),
        };
        o.push_str(&format!("<li id=\"r-{}\">{who}<time datetime=\"{}\">{}</time><div>{}</div></li>", esc(&r.id[..r.id.len().min(16)]), rfc3339(r.created), fr_date(r.created), note_html(&r.content)));
    }
    o.push_str("</ol></details>");
    o
}

fn notes_list(ctx: &Ctx<'_>, ns: &[Note]) -> String {
    let mut o = String::from("<ol class=\"notes\">");
    for n in ns {
        o.push_str(&format!(
            "<li class=\"note\" id=\"n-{}\"><time datetime=\"{}\">{}</time><div>{}</div>{}{}</li>",
            esc(&n.id[..n.id.len().min(16)]),
            rfc3339(n.created),
            fr_date(n.created),
            note_html(&n.content),
            thread(ctx, &n.id, false),
            reply_link(ctx, nevent(&n.id, ctx.hex, &relay_of(ctx)), "Répondre")
        ));
    }
    o.push_str("</ol>");
    o
}

/// Amis réciproques : chacun mène à son blog.
fn friends_block(ctx: &Ctx<'_>) -> String {
    if ctx.friends.is_empty() {
        return String::new();
    }
    let mut o = String::from("<section class=\"wrap\"><h2 class=\"section\">Les blogs de mes amis</h2><ul class=\"friends\">");
    for f in ctx.friends.iter().take(48) {
        let face = f.picture.as_deref().map(|u| format!("<img class=\"avatar\" loading=\"lazy\" src=\"{}\" alt=\"\">", esc(u))).unwrap_or_else(|| format!("<span class=\"avatar letter\" aria-hidden=\"true\">{}</span>", esc(&f.name.chars().next().unwrap_or('·').to_uppercase().to_string())));
        o.push_str(&format!("<li><a href=\"{}\">{face}<span>{}</span></a></li>", esc(&ctx.peer.replace("{}", &f.site)), esc(&f.name)));
    }
    o.push_str("</ul></section>");
    o
}

fn name(ctx: &Ctx<'_>) -> String {
    ctx.profile["display_name"].as_str().or_else(|| ctx.profile["name"].as_str()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Site public").to_owned()
}

fn home(ctx: &Ctx<'_>, arts: &[Article], ns: &[Note], fs: &[Entry]) -> String {
    let b = ctx.base;
    let mut body = String::new();
    if let Some((first, rest)) = arts.split_first() {
        body.push_str("<section class=\"wrap\"><h2 class=\"section\">À la une</h2>");
        let img = first.image.as_deref().map(|u| format!("<img loading=\"lazy\" decoding=\"async\" src=\"{}\" alt=\"\">", esc(u))).unwrap_or_default();
        body.push_str(&format!(
            "<article class=\"feature{}\"><a href=\"{b}/blog/{}\">{img}<div><p class=\"meta\"><time datetime=\"{}\">{}</time> · {} min de lecture</p><h3>{}</h3><p>{}</p><span class=\"more\">Lire l'article →</span></div></a></article>",
            if first.image.is_some() { "" } else { " plain" },
            seg(&first.slug),
            rfc3339(first.published),
            fr_date(first.published),
            reading_minutes(&first.content),
            esc(&first.title),
            esc(&first.summary)
        ));
        if !rest.is_empty() {
            body.push_str("<div class=\"cards\">");
            for a in rest.iter().take(3) {
                body.push_str(&card(ctx, a));
            }
            body.push_str("</div>");
        }
        body.push_str(&format!("<p class=\"all\"><a href=\"{b}/blog/\">Tous les articles ({}) →</a></p></section>", arts.len()));
    }
    if !ns.is_empty() {
        body.push_str("<section class=\"wrap\"><h2 class=\"section\">Dernières notes</h2>");
        body.push_str(&notes_list(ctx, ns));
        body.push_str(&format!("<p class=\"all\"><a href=\"{b}/notes/\">Toutes les notes →</a></p></section>"));
    }
    if !fs.is_empty() {
        body.push_str("<section class=\"wrap\"><h2 class=\"section\">Fichiers partagés</h2>");
        body.push_str(&file_grid(ctx, "", &fs[..fs.len().min(12)]));
        if fs.len() > 12 {
            body.push_str(&format!("<p class=\"all\"><a href=\"{b}/files/\">Tous les fichiers ({}) →</a></p>", fs.len()));
        }
        body.push_str("</section>");
    }
    body.push_str(&friends_block(ctx));
    if arts.is_empty() && ns.is_empty() && fs.is_empty() && ctx.friends.is_empty() {
        body.push_str("<section class=\"wrap\"><p class=\"empty big\">Rien n'est publié pour l'instant.<br>Revenez bientôt.</p></section>");
    }
    let about = ctx.profile["about"].as_str().unwrap_or("");
    layout(ctx, &Meta { title: name(ctx), description: excerpt(about, 200), image: avatar(ctx).map(str::to_owned), kind: "website", hero: true, ..Meta::default() }, "home", &body)
}

fn icon_of(e: &Entry) -> &'static str {
    if e.kind == EntryKind::Dir {
        return "📁";
    }
    match e.mime.as_deref().unwrap_or("") {
        m if m.starts_with("image/") => "🖼️",
        m if m.starts_with("video/") => "🎬",
        m if m.starts_with("audio/") => "🎵",
        "application/pdf" => "📕",
        m if m.starts_with("text/") => "📄",
        _ => "📦",
    }
}

fn human(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} o"),
        1024..=1_048_575 => format!("{} Ko", n / 1024),
        _ => format!("{}.{} Mo", n / 1_048_576, n % 1_048_576 * 10 / 1_048_576),
    }
}

fn file_grid(ctx: &Ctx<'_>, rel: &str, entries: &[Entry]) -> String {
    let b = ctx.base;
    let mut o = String::from("<ul class=\"files\">");
    for e in entries {
        let name = vfs::name_of(&e.path);
        let href = format!("{b}/{}{}", if rel.is_empty() { String::new() } else { format!("{}/", rel.split('/').map(seg).collect::<Vec<_>>().join("/")) }, seg(name));
        let thumb = if e.kind == EntryKind::File && e.mime.as_deref().is_some_and(|m| m == "image/jpeg" || m == "image/png" || m == "image/webp") {
            format!("<img loading=\"lazy\" decoding=\"async\" src=\"{}\" alt=\"\">", esc(&href))
        } else {
            format!("<span class=\"ico\" aria-hidden=\"true\">{}</span>", icon_of(e))
        };
        let (href, size) = if e.kind == EntryKind::Dir { (format!("{href}/"), String::from("dossier")) } else { (href, human(e.size)) };
        o.push_str(&format!("<li><a href=\"{}\">{thumb}<span class=\"fname\">{}</span><small>{size}</small></a></li>", esc(&href), esc(name)));
    }
    o.push_str("</ul>");
    o
}

/// Liste d'un dossier public (racine via `/files/`, sous-dossiers à leur vrai chemin).
pub fn files_page(ctx: &Ctx<'_>, rel: &str, entries: &[Entry]) -> String {
    let b = ctx.base;
    let title = if rel.is_empty() { "Fichiers".to_owned() } else { rel.rsplit('/').next().unwrap_or(rel).to_owned() };
    let mut body = String::from("<section class=\"wrap\">");
    if !rel.is_empty() {
        let mut crumbs = format!("<nav class=\"crumbs\"><a href=\"{b}/files/\">Fichiers</a>");
        let mut acc = String::new();
        for s in rel.split('/') {
            acc = if acc.is_empty() { seg(s) } else { format!("{acc}/{}", seg(s)) };
            crumbs.push_str(&format!(" / <a href=\"{b}/{acc}/\">{}</a>", esc(s)));
        }
        body.push_str(&crumbs);
        body.push_str("</nav>");
    }
    body.push_str(&format!("<h1 class=\"page-title\">{}</h1>", esc(&title)));
    if entries.is_empty() {
        body.push_str("<p class=\"empty\">Rien n'est publié ici pour l'instant.</p>");
    } else {
        body.push_str(&file_grid(ctx, rel, entries));
    }
    body.push_str("</section>");
    layout(ctx, &Meta::page(&title, &name(ctx)), "files", &body)
}

fn article(ctx: &Ctx<'_>, arts: &[Article], i: usize) -> String {
    let (a, b) = (&arts[i], ctx.base);
    let mut body = String::from("<article class=\"post\">");
    if let Some(u) = &a.image {
        body.push_str(&format!("<img class=\"post-cover\" src=\"{}\" alt=\"\">", esc(u)));
    }
    body.push_str(&format!(
        "<header class=\"post-head\"><p class=\"meta\"><a href=\"{b}/blog/\">Articles</a> · <time datetime=\"{}\">{}</time> · {} min de lecture</p><h1>{}</h1>{}</header>",
        rfc3339(a.published),
        fr_date(a.published),
        reading_minutes(&a.content),
        esc(&a.title),
        if a.summary.is_empty() { String::new() } else { format!("<p class=\"lede\">{}</p>", esc(&a.summary)) }
    ));
    // Le titre est déjà en tête : un « # Titre » en première ligne du texte n'est pas répété.
    let content = a.content.trim_start();
    let content = if content.starts_with("# ") { content.split_once('\n').map_or("", |(_, rest)| rest) } else { content };
    body.push_str(&format!("<div class=\"prose\">{}</div>", markdown(content)));
    if !a.tags.is_empty() {
        body.push_str("<p class=\"tags\">");
        for t in &a.tags {
            body.push_str(&format!("<a class=\"tag\" href=\"{b}/tag/{}\">#{}</a> ", seg(t), esc(t)));
        }
        body.push_str("</p>");
    }
    if a.updated > a.published + 86_400 {
        body.push_str(&format!("<p class=\"updated\">Mis à jour le {}</p>", fr_date(a.updated)));
    }
    body.push_str(&format!("<footer class=\"byline\">{}<div><strong>{}</strong>{}</div></footer>", avatar_img(ctx), esc(&name(ctx)), ctx.profile["about"].as_str().map(|s| format!("<p>{}</p>", esc(&excerpt(s, 180)))).unwrap_or_default()));
    let target = coord(ctx.hex, &a.slug);
    body.push_str(&format!("<section class=\"comments\"><h2>Commentaires</h2>{}{}</section>", thread(ctx, &target, true), reply_link(ctx, naddr(&a.slug, ctx.hex, &relay_of(ctx)), "Commenter")));
    let newer = i.checked_sub(1).map(|j| &arts[j]);
    let older = arts.get(i + 1);
    if newer.is_some() || older.is_some() {
        body.push_str("<nav class=\"prevnext\">");
        body.push_str(&older.map(|o| format!("<a rel=\"prev\" href=\"{b}/blog/{}\"><small>← Article précédent</small>{}</a>", seg(&o.slug), esc(&o.title))).unwrap_or_else(|| "<span></span>".into()));
        body.push_str(&newer.map(|o| format!("<a rel=\"next\" class=\"next\" href=\"{b}/blog/{}\"><small>Article suivant →</small>{}</a>", seg(&o.slug), esc(&o.title))).unwrap_or_default());
        body.push_str("</nav>");
    }
    body.push_str("</article>");
    let meta = Meta { title: a.title.clone(), description: a.summary.clone(), image: a.image.clone().or_else(|| avatar(ctx).map(str::to_owned)), kind: "article", canonical: Some(format!("{}{b}/blog/{}", ctx.origin, seg(&a.slug))), published: Some(a.published), site: name(ctx), ..Meta::default() };
    layout(ctx, &meta, "blog", &body)
}

fn avatar<'a>(ctx: &Ctx<'a>) -> Option<&'a str> {
    ctx.profile["picture"].as_str().filter(|u| u.starts_with("https://") || u.starts_with('/'))
}

fn avatar_img(ctx: &Ctx<'_>) -> String {
    match avatar(ctx) {
        Some(u) => format!("<img class=\"avatar\" src=\"{}\" alt=\"\">", esc(u)),
        None => format!("<span class=\"avatar letter\" aria-hidden=\"true\">{}</span>", esc(&name(ctx).chars().next().unwrap_or('·').to_uppercase().to_string())),
    }
}

/// Méta-données d'une page : titre, description et aperçu pour les réseaux.
#[derive(Default)]
struct Meta {
    title: String,
    site: String,
    description: String,
    image: Option<String>,
    kind: &'static str,
    canonical: Option<String>,
    published: Option<i64>,
    hero: bool,
}

impl Meta {
    fn page(title: &str, site: &str) -> Self {
        Self { title: title.to_owned(), site: site.to_owned(), kind: "website", ..Self::default() }
    }
}

fn layout(ctx: &Ctx<'_>, m: &Meta, current: &str, body: &str) -> String {
    let b = ctx.base;
    let who = name(ctx);
    let full_title = if m.site.is_empty() || m.title == m.site { esc(&m.title) } else { format!("{} · {}", esc(&m.title), esc(&m.site)) };
    let mut head = format!("<title>{full_title}</title><meta name=\"description\" content=\"{}\"><meta property=\"og:title\" content=\"{}\"><meta property=\"og:description\" content=\"{}\"><meta property=\"og:type\" content=\"{}\"><meta property=\"og:site_name\" content=\"{}\"><meta name=\"twitter:card\" content=\"{}\">",
        esc(&m.description), esc(&m.title), esc(&m.description), if m.kind.is_empty() { "website" } else { m.kind }, esc(&who), if m.image.is_some() { "summary_large_image" } else { "summary" });
    if let Some(i) = &m.image {
        let abs = if i.starts_with('/') { format!("{}{i}", ctx.origin) } else { i.clone() };
        head.push_str(&format!("<meta property=\"og:image\" content=\"{}\">", esc(&abs)));
    }
    if let Some(c) = &m.canonical {
        head.push_str(&format!("<link rel=\"canonical\" href=\"{}\"><meta property=\"og:url\" content=\"{}\">", esc(c), esc(c)));
    }
    if let Some(p) = m.published {
        head.push_str(&format!("<meta property=\"article:published_time\" content=\"{}\">", rfc3339(p)));
    }
    head.push_str(&format!("<link rel=\"alternate\" type=\"application/atom+xml\" title=\"{}\" href=\"{b}/feed.xml\">", esc(&who)));
    let nav = [("home", "Accueil", format!("{b}/")), ("blog", "Articles", format!("{b}/blog/")), ("notes", "Notes", format!("{b}/notes/")), ("files", "Fichiers", if ctx.files_at.is_empty() { format!("{b}/files/") } else { format!("{}files/", ctx.files_at) })]
        .iter()
        .map(|(k, l, h)| format!("<a href=\"{h}\"{}>{l}</a>", if *k == current { " aria-current=\"page\"" } else { "" }))
        .collect::<String>();
    let banner = ctx.profile["banner"].as_str().filter(|u| u.starts_with("https://") || u.starts_with('/'));
    let top = if m.hero {
        format!(
            "<header class=\"hero\">{}<div class=\"hero-in\">{}<h1>{}</h1>{}{}</div></header>",
            banner.map(|u| format!("<img class=\"banner\" src=\"{}\" alt=\"\">", esc(u))).unwrap_or_else(|| "<div class=\"banner none\"></div>".into()),
            avatar_img(ctx),
            esc(&who),
            ctx.profile["about"].as_str().filter(|s| !s.trim().is_empty()).map(|s| format!("<p class=\"about\">{}</p>", note_html(s))).unwrap_or_default(),
            ctx.profile["website"].as_str().filter(|u| u.starts_with("https://")).map(|u| format!("<p class=\"site\"><a rel=\"noopener me\" target=\"_blank\" href=\"{}\">{}</a></p>", esc(u), esc(u.trim_start_matches("https://").trim_end_matches('/')))).unwrap_or_default()
        )
    } else {
        format!("<header class=\"bar\"><a class=\"brand\" href=\"{b}/\">{}<span>{}</span></a></header>", avatar_img(ctx), esc(&who))
    };
    let theme = match ctx.theme {
        "sobre" | "kraft" => ctx.theme,
        _ => "aurore",
    };
    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\">{head}<style>{CSS}</style></head><body class=\"t-{theme}\"><a class=\"skip\" href=\"#main\">Aller au contenu</a>{notice}{top}<nav class=\"menu\" aria-label=\"Menu\">{nav}<a class=\"rss\" href=\"{b}/feed.xml\" title=\"S'abonner (RSS)\">RSS</a></nav><main id=\"main\">{body}</main><footer class=\"foot\"><p>© {} · <a href=\"{b}/feed.xml\">S'abonner</a> · <a href=\"{app}/\" rel=\"nofollow\">Se connecter</a> · <a href=\"{app}/register\" rel=\"nofollow\">Créer mon MULTIPASS</a></p><p class=\"by\">{visits}🍪 Aucun cookie, aucun pisteur : ce site ne vous suit pas. Publié avec Astroport : chiffré, coopératif, sans publicité.</p></footer></body></html>",
        esc(&who),
        app = ctx.app,
        visits = match ctx.visits {
            Some(n) if n > 0 => format!("👁 {n} visite{} ces 30 derniers jours · ", if n > 1 { "s" } else { "" }),
            _ => String::new(),
        },
        notice = if ctx.notice.is_empty() { String::new() } else { format!("<p class=\"notice\" role=\"status\">🛟 {}</p>", esc(ctx.notice)) }
    )
}

fn xml(s: &str) -> String {
    esc(s)
}

/// Flux Atom des 20 derniers articles (contenu complet).
pub fn feed(ctx: &Ctx<'_>, arts: &[Article]) -> String {
    let root = format!("{}{}", ctx.origin, ctx.base);
    let updated = arts.iter().map(|a| a.updated.max(a.published)).max().unwrap_or(0);
    let mut o = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"fr\"><title>{}</title><id>{root}/</id><link href=\"{root}/\"/><link rel=\"self\" href=\"{root}/feed.xml\"/><updated>{}</updated><author><name>{}</name></author>",
        xml(&name(ctx)),
        rfc3339(updated),
        xml(&name(ctx))
    );
    for a in arts.iter().take(20) {
        let url = format!("{root}/blog/{}", seg(&a.slug));
        o.push_str(&format!(
            "<entry><title>{}</title><id>{url}</id><link href=\"{url}\"/><published>{}</published><updated>{}</updated><summary>{}</summary>{}<content type=\"html\">{}</content></entry>",
            xml(&a.title),
            rfc3339(a.published),
            rfc3339(a.updated.max(a.published)),
            xml(&a.summary),
            a.tags.iter().map(|t| format!("<category term=\"{}\"/>", xml(t))).collect::<String>(),
            xml(&markdown(&a.content))
        ));
    }
    o.push_str("</feed>");
    o
}

/// Plan du site : accueil, listes et chaque article.
pub fn sitemap(ctx: &Ctx<'_>, arts: &[Article]) -> String {
    let root = format!("{}{}", ctx.origin, ctx.base);
    let mut o = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"><url><loc>{root}/</loc></url><url><loc>{root}/blog/</loc></url><url><loc>{root}/notes/</loc></url>");
    for a in arts {
        o.push_str(&format!("<url><loc>{root}/blog/{}</loc><lastmod>{}</lastmod></url>", seg(&a.slug), &rfc3339(a.updated.max(a.published))[..10]));
    }
    o.push_str("</urlset>");
    o
}

/// Feuille de style unique, trois thèmes (les mêmes que l'application) et le mode sombre.
const CSS: &str = r#"
:root{--bg:#f4f6fc;--paper:#fff;--ink:#141a29;--soft:#4b5670;--line:#e1e6f2;--accent:#3b5bdb;--accent2:#7c3aed;--grad:linear-gradient(135deg,#3b5bdb,#7c3aed);--serif:Charter,"Bitstream Charter","Sitka Text",Cambria,Georgia,serif;--sans:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;--r:14px;--sh:0 1px 2px #0f172a0d,0 8px 28px #0f172a12}
.t-sobre{--bg:#f6f7f9;--accent:#1f2937;--accent2:#4b5563;--grad:linear-gradient(135deg,#374151,#111827)}
.t-kraft{--bg:#f3ead9;--paper:#fffaf0;--ink:#2e2314;--soft:#6f5d43;--line:#e3d4b8;--accent:#a3541a;--accent2:#7a3b10;--grad:linear-gradient(135deg,#c4894a,#8f4a14);--sans:var(--serif)}
@media(prefers-color-scheme:dark){:root{--bg:#0c0f17;--paper:#141927;--ink:#e9edf7;--soft:#a3adc4;--line:#252d40;--accent:#8aa4ff;--accent2:#b794ff;--sh:0 1px 2px #0006,0 10px 30px #0008}.t-sobre{--accent:#e5e7eb;--accent2:#9ca3af}.t-kraft{--bg:#18130d;--paper:#221b12;--ink:#f1e6d3;--soft:#bba98b;--line:#3d3223;--accent:#f0a45a;--accent2:#e08a3a}}
*{box-sizing:border-box}html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--ink);font:17px/1.65 var(--sans);text-rendering:optimizeLegibility}
a{color:var(--accent);text-decoration-thickness:1px;text-underline-offset:3px}a:hover{color:var(--accent2)}
img{max-width:100%;height:auto}
.skip{position:absolute;left:-999px}.skip:focus{left:1rem;top:1rem;z-index:9;background:var(--paper);padding:.5rem 1rem;border-radius:8px}
.hero{position:relative;text-align:center;padding-bottom:1.2rem}
.banner{display:block;width:100%;height:clamp(150px,30vw,300px);object-fit:cover}.banner.none{background:var(--grad)}
.hero-in{max-width:44rem;margin:0 auto;padding:0 1.2rem}
.hero .avatar{width:112px;height:112px;font-size:2.8rem;margin-top:-56px;border:5px solid var(--bg);box-shadow:var(--sh)}
.hero h1{font:700 clamp(1.8rem,5vw,2.6rem)/1.15 var(--serif);margin:.5rem 0 .3rem;letter-spacing:-.01em}
.about{color:var(--soft);margin:.2rem auto;max-width:36rem}.site{margin:.3rem 0 0;font-size:.95rem}
.avatar{display:inline-grid;place-items:center;border-radius:50%;object-fit:cover;background:var(--grad);color:#fff;font-weight:700}
.bar{max-width:60rem;margin:0 auto;padding:1rem 1.2rem .2rem}
.brand{display:inline-flex;align-items:center;gap:.6rem;color:var(--ink);text-decoration:none;font:700 1.15rem var(--serif)}.brand .avatar{width:38px;height:38px;font-size:1rem}
.menu{position:sticky;top:0;z-index:5;display:flex;gap:.25rem;justify-content:center;flex-wrap:wrap;padding:.55rem .8rem;background:color-mix(in srgb,var(--bg) 82%,transparent);backdrop-filter:saturate(1.4) blur(12px);border-bottom:1px solid var(--line)}
.menu a{padding:.45rem .95rem;border-radius:999px;color:var(--soft);text-decoration:none;font-weight:600;font-size:.95rem;min-height:40px;display:inline-flex;align-items:center}
.menu a:hover{color:var(--ink);background:var(--paper)}.menu a[aria-current]{color:#fff;background:var(--grad)}
.menu .rss{color:#e8590c}
main{padding:1rem 1.2rem 3rem}
.wrap{max-width:60rem;margin:1.6rem auto}
.section{font:700 .8rem var(--sans);letter-spacing:.12em;text-transform:uppercase;color:var(--soft);margin:0 0 .9rem}
.page-title{font:700 clamp(1.8rem,5vw,2.5rem)/1.15 var(--serif);margin:.4rem 0 .2rem}
.count{color:var(--soft);margin:0 0 1.2rem}
.feature a{display:grid;grid-template-columns:1.25fr 1fr;gap:0;background:var(--paper);border-radius:var(--r);overflow:hidden;box-shadow:var(--sh);color:inherit;text-decoration:none;transition:transform .2s}
.feature.plain a{grid-template-columns:1fr}.feature a:hover{transform:translateY(-2px)}
.feature img{background:var(--grad);width:100%;height:100%;min-height:240px;object-fit:cover}
.feature div{padding:1.6rem 1.8rem;align-self:center}
.feature h3{font:700 clamp(1.5rem,3.6vw,2.1rem)/1.2 var(--serif);margin:.2rem 0 .6rem}
.feature p{margin:.2rem 0;color:var(--soft)}.more{display:inline-block;margin-top:.8rem;font-weight:700;color:var(--accent)}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:1.1rem;margin-top:1.1rem}
.card-link{display:flex;flex-direction:column;height:100%;background:var(--paper);border-radius:var(--r);overflow:hidden;box-shadow:var(--sh);color:inherit;text-decoration:none;transition:transform .2s}
.card-link:hover{transform:translateY(-2px)}
.cover{width:100%;aspect-ratio:16/9;object-fit:cover;display:block;background:var(--grad)}
.cover.none{display:grid;place-items:center;background:var(--grad);color:#ffffffd0;font:700 3rem var(--serif)}
.card-body{padding:1rem 1.15rem 1.2rem}.card h3{font:700 1.2rem/1.3 var(--serif);margin:.15rem 0 .4rem}.card p{margin:0;color:var(--soft);font-size:.95rem}
.meta{font-size:.82rem!important;color:var(--soft);letter-spacing:.01em;margin:0!important}
.all{text-align:right;margin:1rem 0 0;font-weight:600}
.notes{list-style:none;padding:0;margin:0;display:grid;gap:.8rem;max-width:40rem}
.note{background:var(--paper);border-radius:var(--r);padding:1rem 1.2rem;box-shadow:var(--sh);border-left:4px solid var(--accent);overflow-wrap:anywhere}
.note img{background:var(--bg)}.note time{display:block;font-size:.8rem;color:var(--soft);margin-bottom:.3rem}.note img{display:block;border-radius:10px;margin:.6rem 0;max-height:420px}
.files{list-style:none;padding:0;margin:0;display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:.8rem}
.files a{display:flex;flex-direction:column;gap:.2rem;background:var(--paper);border-radius:var(--r);padding:.6rem;box-shadow:var(--sh);color:inherit;text-decoration:none;height:100%}
.files img,.files .ico{width:100%;aspect-ratio:4/3;object-fit:cover;border-radius:9px;display:grid;place-items:center;font-size:2.4rem;background:var(--bg)}
.fname{font-weight:600;font-size:.9rem;overflow-wrap:anywhere}.files small{color:var(--soft);font-size:.78rem}
.crumbs{font-size:.9rem;color:var(--soft)}
.explore{display:grid;grid-template-columns:2fr 1fr;gap:2rem;margin-top:2.5rem;padding-top:1.5rem;border-top:1px solid var(--line)}
.explore h2{font:700 .8rem var(--sans);letter-spacing:.12em;text-transform:uppercase;color:var(--soft);margin:0 0 .7rem}
.tags{display:flex;flex-wrap:wrap;gap:.4rem;margin:1.4rem 0}
.tag{display:inline-flex;gap:.35rem;align-items:center;padding:.3rem .75rem;border-radius:999px;background:var(--paper);border:1px solid var(--line);text-decoration:none;font-size:.88rem;font-weight:600}
.tag small{color:var(--soft);font-weight:400}
.months{list-style:none;padding:0;margin:0;columns:2}.months li{margin:.15rem 0}.months small{color:var(--soft)}
.pager{display:flex;justify-content:space-between;align-items:center;gap:1rem;margin:2rem 0 0;color:var(--soft)}
.pager a{padding:.55rem 1rem;border-radius:999px;background:var(--paper);box-shadow:var(--sh);text-decoration:none;font-weight:600}
.empty{color:var(--soft);font-style:italic}.empty.big{text-align:center;font-size:1.2rem;padding:3rem 0}
.post{max-width:42rem;margin:1.2rem auto}
.post-cover{display:block;width:100%;max-height:460px;object-fit:cover;border-radius:var(--r);box-shadow:var(--sh);margin-bottom:1.6rem}
.page-title,.feature h3,.card h3,.hero h1,.post-head h1{text-wrap:balance}.post-head h1{font:700 clamp(2rem,6vw,3rem)/1.12 var(--serif);letter-spacing:-.015em;margin:.5rem 0 .7rem}
.lede{font:400 1.25rem/1.55 var(--serif);color:var(--soft);margin:0 0 1.8rem}
.prose{font:400 1.14rem/1.78 var(--serif)}
.prose>*+*{margin-top:1.1em}.prose p{margin:0}.prose h2{font:700 1.6rem/1.25 var(--serif);margin-top:2em}.prose h3{font:700 1.3rem/1.3 var(--serif);margin-top:1.6em}
.prose img{display:block;margin:1.6em auto;border-radius:10px}
.prose blockquote{margin:1.5em 0;padding:.2em 1.2em;border-left:4px solid var(--accent);color:var(--soft);font-style:italic}
.prose pre{background:var(--paper);border:1px solid var(--line);border-radius:10px;padding:1em;overflow:auto;font-size:.88rem;line-height:1.5}
.prose code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.9em;background:var(--paper);padding:.1em .35em;border-radius:5px}.prose pre code{background:none;padding:0}
.prose table{border-collapse:collapse;width:100%;font:400 .95rem var(--sans);display:block;overflow-x:auto}.prose th,.prose td{border-bottom:1px solid var(--line);padding:.5em .7em;text-align:left}
.prose hr{border:0;height:1px;background:var(--line);margin:2.4em 0}
.prose ul,.prose ol{padding-left:1.4em}.prose li+li{margin-top:.35em}
.prose>p:first-of-type::first-letter{float:left;font:700 3.4em/.9 var(--serif);margin:.06em .1em 0 0;color:var(--accent)}
.updated{color:var(--soft);font-size:.88rem;font-style:italic}
.byline{display:flex;gap:1rem;align-items:center;margin:2.4rem 0 0;padding:1.2rem;background:var(--paper);border-radius:var(--r);box-shadow:var(--sh)}
.byline .avatar{width:56px;height:56px;flex:none;font-size:1.4rem}.byline p{margin:.2rem 0 0;color:var(--soft);font-size:.92rem}
.prevnext{display:grid;grid-template-columns:1fr 1fr;gap:1rem;margin-top:1.6rem}
.prevnext a{display:flex;flex-direction:column;gap:.2rem;padding:1rem 1.2rem;border-radius:var(--r);background:var(--paper);box-shadow:var(--sh);text-decoration:none;font:700 1.02rem/1.35 var(--serif);color:var(--ink)}
.prevnext small{font:600 .8rem var(--sans);color:var(--soft)}.prevnext .next{text-align:right}
.thread{margin:.7rem 0 0;border-top:1px dashed var(--line);padding-top:.5rem}.thread summary{cursor:pointer;color:var(--soft);font-size:.88rem;font-weight:600;min-height:32px}
.thread ol{list-style:none;padding:0;margin:.4rem 0 0;display:grid;gap:.6rem}.thread li{padding:.6rem .8rem;border-radius:10px;background:var(--bg);font-size:.95rem}
.thread time{font-size:.75rem;color:var(--soft);margin-left:.4rem}
.who{display:inline-flex;align-items:center;gap:.45rem;color:var(--ink);text-decoration:none}.who .avatar{width:26px;height:26px;font-size:.75rem}
.reply{display:flex;align-items:center;gap:.6rem;flex-wrap:wrap;margin:.4rem 0 0;font-size:.88rem;font-weight:600}.reply a{display:inline-flex;align-items:center;min-height:36px;text-decoration:none}.reply .via{font-weight:400;color:var(--soft);font-size:.8rem;text-decoration:underline}
.answer .lead{font-size:1.05rem}.ways{list-style:none;padding:0;display:grid;gap:1rem}.ways li{border:1px solid var(--line,#8884);border-radius:14px;padding:1rem 1.2rem}.ways h2{font:700 1.1rem var(--serif);margin:0 0 .3rem}.ways .go{display:inline-flex;align-items:center;min-height:44px;padding:.4rem 1.1rem;border-radius:999px;background:var(--grad);color:#fff;font-weight:600;text-decoration:none}.ways .small{font-size:.85rem;color:var(--soft)}.ways .code{display:block;overflow-wrap:anywhere;font-size:.75rem;padding:.4rem;border-radius:6px;background:#8881}
.comments{margin-top:2.2rem}.comments h2{font:700 1.3rem var(--serif);margin:0 0 .4rem}.comments .reply a:first-child{padding:.5rem 1.1rem;border-radius:999px;background:var(--grad);color:#fff}
.friends{list-style:none;padding:0;margin:0;display:grid;grid-template-columns:repeat(auto-fill,minmax(120px,1fr));gap:.8rem}
.friends a{display:flex;flex-direction:column;align-items:center;gap:.4rem;padding:.9rem .5rem;border-radius:var(--r);background:var(--paper);box-shadow:var(--sh);text-decoration:none;color:var(--ink);font-weight:600;font-size:.9rem;text-align:center}
.friends .avatar{width:64px;height:64px;font-size:1.5rem}
.notice{margin:0;padding:.6rem 1rem;text-align:center;font-size:.9rem;background:#fff3cd;color:#5c4400}@media(prefers-color-scheme:dark){.notice{background:#3a2f0b;color:#ffe08a}}
.foot{text-align:center;color:var(--soft);font-size:.88rem;padding:2rem 1rem 3rem;border-top:1px solid var(--line)}.foot p{margin:.2rem}.foot .by{font-size:.8rem;opacity:.8}
@media(max-width:640px){body{font-size:16px}.feature a{grid-template-columns:1fr}.feature img{min-height:0;aspect-ratio:16/9}.feature div{padding:1.1rem 1.2rem 1.3rem}.explore{grid-template-columns:1fr}.prevnext{grid-template-columns:1fr}.prevnext .next{text-align:left}.files{grid-template-columns:repeat(2,1fr)}.prose{font-size:1.07rem}.menu a{padding:.4rem .7rem}}
@media(prefers-reduced-motion:reduce){*{transition:none!important}}
"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn ctx(p: &Value) -> Ctx<'_> {
        static NONE: std::sync::LazyLock<std::collections::HashMap<String, Vec<Reply>>> = std::sync::LazyLock::new(Default::default);
        Ctx { hex: "ab", peer: "/site/{}/", app: "", replies: &NONE, friends: &[], notice: "", files_at: "", base: "/site/x", origin: "https://st.example", profile: p, theme: "aurore", visits: None }
    }

    #[test]
    fn a_thread_is_folded_and_each_member_leads_to_his_blog() {
        let p = json!({ "name": "Léa" });
        let mut rp = std::collections::HashMap::new();
        rp.insert("n1".to_owned(), vec![Reply { id: "r1".into(), target: "n1".into(), author: "cd".into(), created: 5, content: "<b>Bravo</b>".into(), name: "Paul".into(), picture: None, site: Some("paul.ex".into()) }]);
        let fr = [Friend { name: "Paul".into(), picture: None, site: "paul.ex".into() }];
        let c = Ctx { replies: &rp, friends: &fr, ..ctx(&p) };
        let page = home(&c, &[], &[Note { id: "n1".into(), created: 1, content: "Salut".into() }], &[]);
        assert!(page.contains("<details class=\"thread\"><summary>1 réponse</summary>"), "replié par défaut");
        assert!(page.contains("href=\"/site/paul.ex/\"") && page.contains("&lt;b&gt;Bravo"));
        assert!(page.contains("Les blogs de mes amis"));
    }

    #[test]
    fn raw_html_and_dangerous_links_never_survive_markdown() {
        let h = markdown("<script>alert(1)</script>\n\n[a](javascript:alert(1)) ![i](data:image/png;base64,AA) [ok](https://e.org) ![p](https://e.org/p.jpg)");
        assert!(!h.contains("<script>") && h.contains("&lt;script&gt;"));
        assert!(!h.contains("javascript:") && !h.contains("data:image"));
        assert!(h.contains("href=\"https://e.org\"") && h.contains("src=\"https://e.org/p.jpg\""));
    }

    #[test]
    fn an_article_reads_its_nip23_tags() {
        let tags = json!([["d", "mon-velo"], ["title", "Mon vélo"], ["summary", "Un résumé"], ["published_at", "1700000000"], ["t", "Vélo"], ["t", "#voyage"], ["image", "http://non-sur"]]).to_string();
        let a = parse_article("# Autre\n\nTexte.", &tags, "mon-velo".into(), 1_800_000_000);
        assert_eq!((a.title.as_str(), a.summary.as_str(), a.published), ("Mon vélo", "Un résumé", 1_700_000_000));
        assert_eq!(a.tags, vec!["vélo", "voyage"]);
        assert!(a.image.is_none(), "seule une image https est reprise");
        let b = parse_article("# Titre du texte\n\nUn long texte.", "[]", "s".into(), 5);
        assert_eq!((b.title.as_str(), b.published), ("Titre du texte", 5));
    }

    #[test]
    fn dates_are_civil_and_french() {
        assert_eq!(fr_date(0), "1 janvier 1970");
        assert_eq!(fr_date(1_795_219_200), "21 novembre 2026");
        assert_eq!(rfc3339(1_795_219_200 + 3_723), "2026-11-21T01:02:03Z");
        assert_eq!(month_label("2026-08"), "Août 2026");
    }

    #[test]
    fn profile_text_is_escaped_everywhere() {
        let p = json!({ "name": "<b>x</b>", "about": "<script>", "picture": "javascript:alert(1)", "banner": "data:x" });
        let page = home(&ctx(&p), &[], &[], &[]);
        assert!(!page.contains("<b>x") && !page.contains("<script>") && !page.contains("javascript:") && !page.contains("data:x"));
        assert!(page.contains("Rien n'est publié"));
    }

    #[test]
    fn a_note_links_and_shows_images_but_stays_escaped() {
        let h = note_html("Regardez https://e.org/a.jpg et <i>ça</i>, https://e.org/page.");
        assert!(h.contains("<img loading=\"lazy\" decoding=\"async\" src=\"https://e.org/a.jpg\""));
        assert!(h.contains("href=\"https://e.org/page\"") && h.contains("&lt;i&gt;"));
    }

    #[test]
    fn the_blog_lists_tags_months_and_paginates() {
        let p = json!({ "name": "Léa" });
        let arts: Vec<Article> = (0..23).map(|i| parse_article("Texte", &json!([["t", if i % 2 == 0 { "pair" } else { "impair" }]]).to_string(), format!("a{i}"), 1_790_000_000 - i * 86_400 * 10)).collect();
        let p1 = list(&ctx(&p), &arts, ListOf::All, 1).unwrap();
        assert!(p1.contains("23 articles") && p1.contains("/site/x/blog/page/2") && p1.contains("/site/x/tag/pair"));
        assert!(list(&ctx(&p), &arts, ListOf::All, 3).unwrap().contains("Plus récents"));
        assert!(list(&ctx(&p), &arts, ListOf::All, 4).is_none(), "au-delà de la dernière page : 404");
        assert!(list(&ctx(&p), &arts, ListOf::Tag("pair"), 1).unwrap().contains("12 articles"));
        assert!(list(&ctx(&p), &arts, ListOf::Tag("inconnue"), 1).is_none());
        let one = article(&ctx(&p), &arts, 1);
        assert!(one.contains("Article précédent") && one.contains("Article suivant") && one.contains("og:title"));
        let f = feed(&ctx(&p), &arts);
        assert!(f.starts_with("<?xml") && f.matches("<entry>").count() == 20 && f.contains("https://st.example/site/x/blog/a0"));
    }

    #[test]
    fn nostr_addresses_follow_nip19() {
        // Vecteur NIP-19 : identifiant et auteur de 32 octets, relai en indication.
        let id = "b9f5441e45ca39179320e0031cfb18e34078673dcc3d3e3a3b3a981760aa5696";
        let au = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";
        let ne = nevent(id, au, "wss://r.example").unwrap();
        assert!(ne.starts_with("nevent1"));
        let (hrp, data) = bech32::decode(&ne).unwrap();
        assert_eq!(hrp.as_str(), "nevent");
        assert_eq!((data[0], data[1]), (0, 32));
        assert_eq!(hex::encode(&data[2..34]), id);
        let na = naddr("mon-article", au, "").unwrap();
        let (_, d) = bech32::decode(&na).unwrap();
        assert_eq!(&d[2..2 + d[1] as usize], b"mon-article");
        assert_eq!(&d[d.len() - 4..], 30_023u32.to_be_bytes());
        assert!(nevent("zz", au, "").is_none(), "un identifiant invalide ne donne pas de lien");
    }

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(seg("été 2026/x"), "%C3%A9t%C3%A9%202026%2Fx");
    }
}
