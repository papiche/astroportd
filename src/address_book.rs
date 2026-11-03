//! **Carnet d'adresses** : recherche, fiches détaillées modifiables, groupes, import vCard, doublons et fusion, comparaison d'une fiche
//! avec le **profil Nostr** (kind 0) de l'ami qu'elle désigne, et **« Ma fiche »** : le profil Nostr du titulaire et sa propre fiche
//! réunis en une seule vue.
//!
//! Les fiches restent des fichiers vCard chiffrés (`/Contacts/*.vcf`, accessibles en CardDAV depuis un téléphone) : ce module les lit et
//! les écrit **sans jamais effacer ce qu'il ne gère pas** (photo incorporée, `X-NOSTR-PUBKEY`, `UID`…). L'index de recherche
//! (`contact_cards`) est une projection reconstructible, tenue par [`crate::identity_links`].
//!
//! Profil et fiche :
//! - **un ami** : sa fiche et son kind 0 se comparent champ par champ (nom, présentation, image, site, NIP-05, adresse Lightning) ; l'utilisateur
//!   prend **chaque valeur** qu'il veut, jamais en bloc ni en silence ;
//! - **soi-même** : la fiche du titulaire (`X-ASTRO-ME`) est créée à la première visite. Son profil Nostr fait foi pour les champs publics ;
//!   un seul formulaire modifie tout : l'enregistrement **publie** le kind 0 (champs publics) et **écrit** la fiche (champs publics et privés :
//!   téléphones, adresses, anniversaire, notes ne quittent jamais la fiche chiffrée).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use astro_dav::vcard::{self, Fields, Typed};
use astro_dav::{BlobStore, DavState, carddav, ingest};
use astro_db::Db;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::account::AccountService;
use crate::identity_links::{IdentityLinks, LinkError};

/// Échec d'une opération du carnet.
#[derive(Debug, PartialEq, Eq)]
pub enum BookError {
    /// Saisie refusée.
    Invalid(String),
    /// Fiche introuvable.
    NotFound(String),
    /// Conflit (fiche déjà là…).
    Conflict(String),
    /// Panne de la station.
    Internal(String),
}

impl From<LinkError> for BookError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::Invalid(m) => Self::Invalid(m),
            LinkError::NotFound(m) => Self::NotFound(m),
            LinkError::Conflict(m) => Self::Conflict(m),
            LinkError::Internal(m) => Self::Internal(m),
        }
    }
}

fn internal(e: impl std::fmt::Display) -> BookError {
    BookError::Internal(e.to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Cartes importées au plus d'un coup, et taille maximale d'un fichier d'import.
const MAX_IMPORT_CARDS: usize = 2000;
const MAX_IMPORT_BYTES: usize = 4 << 20;
/// Fiches examinées au plus (liste, doublons).
const MAX_CARDS: i64 = 5000;

/// Les champs du profil Nostr et leur place dans une fiche : `(clé du profil, libellé)`.
const PROFILE_KEYS: [(&str, &str); 6] = [("name", "Nom"), ("about", "Présentation"), ("picture", "Image"), ("website", "Site web"), ("nip05", "NIP-05"), ("lud16", "Adresse Lightning")];

/// Valeur d'un champ public dans une fiche.
fn card_value(f: &Fields, key: &str) -> String {
    match key {
        "name" => f.name.clone(),
        "about" => f.about.clone(),
        "picture" => f.picture.clone(),
        "website" => f.urls.first().cloned().unwrap_or_default(),
        "nip05" => f.nip05.clone(),
        "lud16" => f.lud16.clone(),
        _ => String::new(),
    }
}

/// Écrit une valeur de profil dans les champs d'une fiche.
fn set_card_value(f: &mut Fields, key: &str, v: &str) {
    let v = v.trim().to_owned();
    match key {
        "name" => {
            let mut parts: Vec<&str> = v.split_whitespace().collect();
            let family = if parts.len() > 1 { parts.pop().unwrap_or_default().to_owned() } else { String::new() };
            (f.given, f.family) = (parts.join(" "), family);
            f.name = v;
        }
        "about" => f.about = v,
        "picture" => f.picture = v,
        "website" => {
            f.urls.retain(|u| *u != v);
            if !v.is_empty() {
                f.urls.insert(0, v);
            }
        }
        "nip05" => f.nip05 = v,
        "lud16" => f.lud16 = v,
        _ => {}
    }
}

/// Valeur d'un champ dans le kind 0.
fn profile_value(p: &Value, key: &str) -> String {
    let get = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    if key == "name" { Some(get("display_name")).filter(|v| !v.is_empty()).unwrap_or_else(|| get("name")) } else { get(key) }
}

/// Comparaison fiche ↔ profil, champ par champ.
fn diff(f: &Fields, profile: &Value) -> Vec<Value> {
    PROFILE_KEYS
        .iter()
        .map(|(key, label)| {
            let (mine, theirs) = (card_value(f, key), profile_value(profile, key));
            let same = mine.trim().eq_ignore_ascii_case(theirs.trim());
            json!({ "key": key, "label": label, "mine": mine, "theirs": theirs, "same": same, "can_take": !same && !theirs.is_empty() })
        })
        .collect()
}

fn letter(name_key: &str) -> String {
    name_key.chars().find(|c| c.is_alphanumeric()).map_or_else(|| "#".to_owned(), |c| if c.is_alphabetic() { c.to_uppercase().collect() } else { "#".to_owned() })
}

fn digits(v: &str) -> String {
    v.chars().filter(char::is_ascii_digit).collect()
}

/// Même numéro écrit autrement (`+33 6 11…` et `06 11…`) : les 9 derniers chiffres, au moins 7.
fn same_phone(a: &str, b: &str) -> bool {
    let (a, b) = (digits(a), digits(b));
    let tail = |d: &str| d[d.len() - 9.min(d.len())..].to_owned();
    (a.len() >= 7 && b.len() >= 7 && tail(&a) == tail(&b)) || a == b
}

/// Le carnet d'adresses.
pub struct AddressBook<B: BlobStore> {
    db: Arc<Db>,
    dav: Arc<DavState<B>>,
    links: Arc<IdentityLinks<B>>,
    account: Option<Arc<AccountService>>,
}

impl<B: BlobStore> AddressBook<B> {
    /// Carnet sur les fiches de `/Contacts` ; `account` publie le profil Nostr du titulaire (« Ma fiche »).
    pub fn new(db: Arc<Db>, dav: Arc<DavState<B>>, links: Arc<IdentityLinks<B>>, account: Option<Arc<AccountService>>) -> Self {
        Self { db, dav, links, account }
    }

    fn safe_path(path: &str) -> Result<(), BookError> {
        let ok = path.starts_with("/Contacts/") && path.ends_with(".vcf") && !path.contains("..") && !path[10..].contains('/') && path.len() < 200;
        if ok { Ok(()) } else { Err(BookError::Invalid("fiche attendue : /Contacts/<nom>.vcf".into())) }
    }

    async fn read(&self, owner: &str, path: &str) -> Result<String, BookError> {
        Self::safe_path(path)?;
        let bytes = carddav::read_file(&self.dav, owner, path).await.ok_or_else(|| BookError::NotFound("fiche introuvable".into()))?;
        String::from_utf8(bytes).map_err(|_| BookError::Invalid("fiche illisible".into()))
    }

    async fn write(&self, owner: &str, path: &str, text: String) -> Result<(), BookError> {
        let name = astro_dav::vfs::name_of(path).to_owned();
        ingest::replace(&self.dav, owner, carddav::CONTACTS_DIR, &name, Some("text/vcard"), std::io::Cursor::new(text.into_bytes())).await.map_err(internal)?;
        Ok(())
    }

    /// Profil Nostr (kind 0) le plus récent de `hex`, connu du relai de la station.
    async fn profile_of(&self, hex: &str) -> Option<Value> {
        let h = hex.to_owned();
        let content: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT content FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [h], |r| r.get(0)).optional()?)).await.ok().flatten();
        content.and_then(|c| serde_json::from_str(&c).ok())
    }

    /// Lignes de l'index : chemin, champs, me, clé Nostr, lien, avertissement, UID.
    async fn rows(&self, owner: &str) -> Result<Vec<Row>, BookError> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT k.path, k.fields, k.me, k.search, l.pubkey, l.via, l.warning, l.uid FROM contact_cards k \
                     LEFT JOIN identity_links l ON l.owner_hex = k.owner_hex AND l.path = k.path WHERE k.owner_hex = ?1 ORDER BY k.path LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![o, MAX_CARDS], |r| {
                        Ok(Row { path: r.get(0)?, fields: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(), me: r.get::<_, i64>(2)? == 1, search: r.get(3)?, pubkey: r.get(4)?, via: r.get(5)?, warning: r.get(6)?, uid: r.get(7)? })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(internal)
    }

    /// Visages par fiche (UID vCard) : le lien fait foi, jamais le nom.
    async fn faces_by_uid(&self, owner: &str) -> HashMap<String, i64> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT contact_uid, count(*) FROM faces WHERE owner_hex = ?1 AND ref IS NULL AND contact_uid IS NOT NULL GROUP BY contact_uid")?;
                Ok(stmt.query_map([o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Fiches filtrées par `q` (tous les mots, sans accents ni casse, dans le nom, la société, les emails, les chiffres des téléphones, les notes…)
    /// et par `group`, avec la liste des groupes et des lettres de l'index.
    pub async fn list(&self, owner: &str, q: &str, group: Option<&str>) -> Result<Value, BookError> {
        let rows = self.rows(owner).await?;
        let faces = self.faces_by_uid(owner).await;
        let tokens: Vec<String> = vcard::name_key(q).split_whitespace().map(str::to_owned).chain(digits(q).chars().count().ge(&3).then(|| digits(q))).collect();
        let (mut groups, mut letters): (BTreeMap<String, usize>, BTreeSet<String>) = (BTreeMap::new(), BTreeSet::new());
        for r in &rows {
            for g in &r.fields.groups {
                *groups.entry(g.clone()).or_default() += 1;
            }
        }
        let mut contacts: Vec<(String, Value)> = Vec::new();
        for r in rows {
            if !tokens.iter().all(|t| r.search.contains(t.as_str())) || group.is_some_and(|g| !r.fields.groups.iter().any(|x| x == g)) {
                continue;
            }
            let key = vcard::name_key(&r.fields.name);
            let l = letter(&key);
            letters.insert(l.clone());
            contacts.push((key.clone(), json!({
                "path": r.path, "name": r.fields.name, "org": r.fields.org, "title": r.fields.title, "emails": r.fields.emails.iter().map(|e| &e.value).collect::<Vec<_>>(),
                "phones": r.fields.phones, "groups": r.fields.groups, "letter": l, "me": r.me, "pubkey": r.pubkey, "via": r.via, "warning": r.warning,
                "faces": r.uid.as_ref().and_then(|u| faces.get(u)).copied().unwrap_or(0), "picture": r.fields.picture, "uid": r.uid,
            })));
        }
        contacts.sort_by(|a, b| b.1["me"].as_bool().cmp(&a.1["me"].as_bool()).then_with(|| a.0.cmp(&b.0)));
        let total = contacts.len();
        Ok(json!({ "contacts": contacts.into_iter().map(|c| c.1).collect::<Vec<_>>(), "total": total, "letters": letters, "groups": groups.into_iter().map(|(name, count)| json!({ "name": name, "count": count })).collect::<Vec<_>>() }))
    }

    /// Une fiche en détail, avec son lien, ses visages et sa comparaison avec le profil Nostr de la personne.
    pub async fn get(&self, owner: &str, path: &str) -> Result<Value, BookError> {
        let text = self.read(owner, path).await?;
        let fields = vcard::read_fields(&text);
        let me = vcard::is_me(&text);
        let (o, p) = (owner.to_owned(), path.to_owned());
        let link: Option<LinkRow> = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT uid, pubkey, via, warning FROM identity_links WHERE owner_hex = ?1 AND path = ?2", [o, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?))
            .await
            .map_err(internal)?;
        let (uid, pubkey, via, warning) = link.unwrap_or_default();
        let pubkey = if me { Some(owner.to_owned()) } else { pubkey };
        let profile = match &pubkey {
            Some(k) => self.profile_of(k).await,
            None => None,
        };
        let faces = match &uid {
            Some(u) => self.faces_by_uid(owner).await.get(u).copied().unwrap_or(0),
            None => 0,
        };
        let hosted_here: Option<String> = match &pubkey {
            Some(k) => {
                let k = k.clone();
                self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1 AND hosted = 1", [k], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten()
            }
            None => None,
        };
        let comparison = profile.as_ref().map(|p| diff(&fields, p));
        Ok(json!({ "path": path, "uid": uid, "fields": fields, "me": me, "pubkey": pubkey, "via": via, "warning": warning, "faces": faces, "hosted_here": hosted_here, "profile": profile.as_ref().map(|p| json!({
            "name": profile_value(p, "name"), "about": profile_value(p, "about"), "picture": profile_value(p, "picture"), "website": profile_value(p, "website"), "nip05": profile_value(p, "nip05"), "lud16": profile_value(p, "lud16") })),
            "diff": comparison, "drift": comparison.as_ref().is_some_and(|d| d.iter().any(|x| x["can_take"] == true || (x["same"] == false && x["mine"] != "" && me))) }))
    }

    /// Enregistre une fiche : la crée (`path = None`) ou remplace ses champs gérés en gardant le reste.
    pub async fn save(&self, owner: &str, path: Option<&str>, fields: &Fields) -> Result<Value, BookError> {
        if fields.name.trim().is_empty() {
            return Err(BookError::Invalid("un nom est nécessaire".into()));
        }
        let path = match path {
            Some(p) => {
                let text = self.read(owner, p).await?;
                let out = vcard::apply_fields(&text, fields, now()).ok_or_else(|| BookError::Invalid("un nom est nécessaire".into()))?;
                self.write(owner, p, out).await?;
                p.to_owned()
            }
            None => {
                let base = vcard::new_card(&format!("ab-{:x}-{}", now(), vcard::name_key(&fields.name).replace(' ', "-")), fields.name.trim(), None, None, now());
                let out = vcard::apply_fields(&base, fields, now()).ok_or_else(|| BookError::Invalid("un nom est nécessaire".into()))?;
                let file = ingest::sanitize_name(fields.name.trim()).ok_or_else(|| BookError::Invalid("nom de fiche invalide".into()))?;
                ingest::ingest(&self.dav, owner, carddav::CONTACTS_DIR, &format!("{file}.vcf"), Some("text/vcard"), std::io::Cursor::new(out.into_bytes())).await.map_err(internal)?.path
            }
        };
        self.links.sync_owner(owner).await?;
        Ok(json!({ "path": path }))
    }

    /// Photo incorporée d'une fiche, en URI `data:` (`{"uri": null}` si elle n'en a pas).
    pub async fn photo(&self, owner: &str, path: &str) -> Result<Value, BookError> {
        let text = self.read(owner, path).await?;
        Ok(json!({ "uri": vcard::embedded_photo(&text) }))
    }

    /// Visages de la personne de la fiche (les plus nets d'abord, 24 au plus), reliés par son UID : `{ faces: [face_id],
    /// photos: [{ id, path, bbox }], total }` (`photos` : de quoi montrer la photo d'origine, le visage entouré).
    pub async fn photo_choices(&self, owner: &str, path: &str) -> Result<Value, BookError> {
        Self::safe_path(path)?;
        let Some(uid) = self.links.uid_of(owner, path).await? else { return Ok(json!({ "faces": [], "photos": [], "total": 0 })) };
        let o = owner.to_owned();
        let (rows, total): (Vec<(i64, String, [f64; 4])>, i64) = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT face_id, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2 FROM faces WHERE owner_hex = ?1 AND contact_uid = ?2 AND ref IS NULL \
                     AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL ORDER BY det_score DESC LIMIT 24",
                )?;
                let rows = st.query_map([&o, &uid], |r| Ok((r.get(0)?, r.get(1)?, [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?])))?.collect::<Result<Vec<_>, _>>()?;
                let total = c.query_row("SELECT count(*) FROM faces WHERE owner_hex = ?1 AND contact_uid = ?2 AND ref IS NULL", [&o, &uid], |r| r.get(0))?;
                Ok((rows, total))
            })
            .await
            .map_err(internal)?;
        Ok(json!({ "faces": rows.iter().take(12).map(|r| r.0).collect::<Vec<_>>(), "photos": rows.iter().map(|(id, p, b)| json!({ "id": id, "path": p, "bbox": b })).collect::<Vec<_>>(), "total": total }))
    }

    /// Change la photo d'une fiche. `uri` : image `data:` (JPEG, PNG, WebP ou GIF, 2 Mo au plus) ramenée à 256 px et
    /// ré-encodée en JPEG ; `None` retire la photo. La photo reste **dans** la fiche : le téléphone la synchronise.
    pub async fn set_photo(&self, owner: &str, path: &str, uri: Option<&str>) -> Result<Value, BookError> {
        use base64::Engine;
        let b64 = match uri {
            None => None,
            Some(u) => {
                let (head, data) = u.strip_prefix("data:").and_then(|r| r.split_once(',')).ok_or_else(|| BookError::Invalid("image attendue (data:)".into()))?;
                if !head.ends_with(";base64") || !matches!(head.split(';').next(), Some("image/jpeg" | "image/png" | "image/webp" | "image/gif")) {
                    return Err(BookError::Invalid("JPEG, PNG, WebP ou GIF attendu".into()));
                }
                if data.len() > 2_800_000 {
                    return Err(BookError::Invalid("image trop grosse (2 Mo au plus)".into()));
                }
                let raw = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| BookError::Invalid("image illisible".into()))?;
                let jpeg = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                    let img = image::load_from_memory(&raw).map_err(|e| e.to_string())?.thumbnail(256, 256).to_rgb8();
                    let mut out = Vec::new();
                    img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)).map_err(|e| e.to_string())?;
                    Ok(out)
                })
                .await
                .map_err(internal)?
                .map_err(|_| BookError::Invalid("image illisible".into()))?;
                Some(base64::engine::general_purpose::STANDARD.encode(jpeg))
            }
        };
        let text = self.read(owner, path).await?;
        if vcard::is_me(&text) {
            return Err(BookError::Invalid("votre propre photo se change dans votre profil".into()));
        }
        self.write(owner, path, vcard::set_embedded_photo(&text, b64.as_deref(), now())).await?;
        self.links.sync_owner(owner).await?;
        Ok(json!({ "path": path, "photo": b64.is_some() }))
    }

    /// Supprime plusieurs fiches d'un coup (500 au plus ; jamais celle du titulaire).
    /// Chaque échec est dit dans `failed`, sans arrêter les autres.
    pub async fn delete_many(&self, owner: &str, paths: &[String]) -> Result<Value, BookError> {
        if paths.is_empty() || paths.len() > 500 {
            return Err(BookError::Invalid("entre 1 et 500 fiches à la fois".into()));
        }
        let (mut deleted, mut failed) = (0, Vec::new());
        for p in paths {
            match self.delete(owner, p).await {
                Ok(_) => deleted += 1,
                Err(BookError::Invalid(m) | BookError::NotFound(m) | BookError::Conflict(m) | BookError::Internal(m)) => failed.push(json!({ "path": p, "error": m })),
            }
        }
        Ok(json!({ "deleted": deleted, "failed": failed }))
    }

    /// Supprime une fiche (jamais celle du titulaire).
    pub async fn delete(&self, owner: &str, path: &str) -> Result<Value, BookError> {
        let text = self.read(owner, path).await?;
        if vcard::is_me(&text) {
            return Err(BookError::Invalid("votre propre fiche ne se supprime pas".into()));
        }
        self.remove_file(owner, path).await?;
        self.links.sync_owner(owner).await?;
        Ok(json!({ "deleted": path }))
    }

    /// Retire le fichier d'une fiche, sans relire le carnet (l'appelant le fait une fois, après un lot).
    async fn remove_file(&self, owner: &str, path: &str) -> Result<(), BookError> {
        let (o, p) = (owner.to_owned(), path.to_owned());
        let cids = astro_dav::vfs::vfs_tx(&self.dav.db, move |tx| astro_dav::vfs::delete_tree_tx(tx, &o, &p)).await.map_err(|e| BookError::Internal(e.to_string()))?;
        astro_dav::server::release_blobs(&self.dav, cids).await;
        if let Some(p) = &self.dav.publisher {
            p.notify(owner);
        }
        Ok(())
    }

    /// Prend dans la fiche les valeurs choisies du profil Nostr de la personne (`keys` : `name`, `about`, `picture`, `website`, `nip05`, `lud16`).
    pub async fn take(&self, owner: &str, path: &str, keys: &[String]) -> Result<Value, BookError> {
        let text = self.read(owner, path).await?;
        let pubkey = self.get(owner, path).await?["pubkey"].as_str().map(str::to_owned).ok_or_else(|| BookError::Invalid("cette fiche n'est reliée à aucun profil Nostr".into()))?;
        let profile = self.profile_of(&pubkey).await.ok_or_else(|| BookError::NotFound("profil Nostr de cette personne inconnu de la station".into()))?;
        let mut f = vcard::read_fields(&text);
        let mut taken = Vec::new();
        for k in keys {
            if !PROFILE_KEYS.iter().any(|(key, _)| key == k) {
                return Err(BookError::Invalid(format!("champ inconnu : {k}")));
            }
            let v = profile_value(&profile, k);
            if !v.is_empty() {
                set_card_value(&mut f, k, &v);
                taken.push(k.clone());
            }
        }
        let out = vcard::apply_fields(&text, &f, now()).ok_or_else(|| BookError::Invalid("un nom est nécessaire".into()))?;
        self.write(owner, path, out).await?;
        self.links.sync_owner(owner).await?;
        Ok(json!({ "taken": taken }))
    }

    /// Renomme (`to = Some`) ou retire (`None`) un groupe sur toutes les fiches qui le portent.
    pub async fn set_group(&self, owner: &str, from: &str, to: Option<&str>) -> Result<Value, BookError> {
        let to = to.map(str::trim).filter(|t| !t.is_empty());
        if from.trim().is_empty() || to.is_some_and(|t| t.chars().count() > 40 || t.chars().any(char::is_control)) {
            return Err(BookError::Invalid("nom de groupe : 40 caractères au plus".into()));
        }
        let mut changed = 0;
        for r in self.rows(owner).await?.into_iter().filter(|r| r.fields.groups.iter().any(|g| g == from)) {
            let text = self.read(owner, &r.path).await?;
            let mut f = vcard::read_fields(&text);
            f.groups = f.groups.iter().filter_map(|g| if g == from { to.map(str::to_owned) } else { Some(g.clone()) }).collect::<BTreeSet<_>>().into_iter().collect();
            if let Some(out) = vcard::apply_fields(&text, &f, now()) {
                self.write(owner, &r.path, out).await?;
                changed += 1;
            }
        }
        self.links.sync_owner(owner).await?;
        Ok(json!({ "changed": changed }))
    }

    /// Importe un fichier `.vcf` (une ou plusieurs fiches) : les doublons (même `UID`, ou même nom et même email) sont ignorés, les fiches
    /// invalides sont dites avec leur raison.
    pub async fn import(&self, owner: &str, text: &str) -> Result<Value, BookError> {
        if text.len() > MAX_IMPORT_BYTES {
            return Err(BookError::Invalid("fichier trop gros (4 Mo au plus)".into()));
        }
        let existing = self.rows(owner).await?;
        let mut seen: Vec<(Option<String>, String, BTreeSet<String>)> = Vec::new();
        for r in &existing {
            let uid = self.read(owner, &r.path).await.ok().and_then(|t| vcard::parse(t.as_bytes()).ok()).and_then(|c| c.uid);
            seen.push((uid, vcard::name_key(&r.fields.name), r.fields.emails.iter().map(|e| e.value.clone()).collect()));
        }
        let (mut imported, mut duplicates, mut rejected) = (0, 0, Vec::new());
        let upper = text.to_ascii_uppercase();
        let mut start = 0;
        let mut blocks = Vec::new();
        while let Some(b) = upper[start..].find("BEGIN:VCARD") {
            let from = start + b;
            let Some(e) = upper[from..].find("END:VCARD") else { break };
            let to = from + e + "END:VCARD".len();
            blocks.push(&text[from..to]);
            start = to;
        }
        if blocks.len() > MAX_IMPORT_CARDS {
            return Err(BookError::Invalid(format!("{} fiches : {MAX_IMPORT_CARDS} au plus par import", blocks.len())));
        }
        for block in blocks {
            let normalized = block.replace("\r\n", "\n").replace('\n', "\r\n") + "\r\n";
            let card = match vcard::parse(normalized.as_bytes()) {
                Ok(c) => c,
                Err(e) => {
                    if rejected.len() < 10 {
                        rejected.push(e.to_string());
                    }
                    continue;
                }
            };
            let key = vcard::name_key(&card.name);
            let emails: BTreeSet<String> = card.emails.iter().cloned().collect();
            let dup = seen.iter().any(|(uid, k, e)| (card.uid.is_some() && uid == &card.uid) || (*k == key && (emails.is_empty() || e.is_empty() || !emails.is_disjoint(e))));
            if dup {
                duplicates += 1;
                continue;
            }
            let file = ingest::sanitize_name(&card.name).ok_or_else(|| BookError::Invalid("nom de fiche invalide".into()))?;
            ingest::ingest(&self.dav, owner, carddav::CONTACTS_DIR, &format!("{file}.vcf"), Some("text/vcard"), std::io::Cursor::new(normalized.into_bytes())).await.map_err(internal)?;
            seen.push((card.uid.clone(), key, emails));
            imported += 1;
        }
        self.links.sync_owner(owner).await?;
        Ok(json!({ "imported": imported, "duplicates": duplicates, "rejected": rejected }))
    }

    /// Fiches qui semblent désigner la même personne : même email, même téléphone (9 derniers chiffres) ou même nom.
    pub async fn duplicates(&self, owner: &str) -> Result<Value, BookError> {
        let rows: Vec<Row> = self.rows(owner).await?.into_iter().filter(|r| !r.me).collect();
        let mut parent: Vec<usize> = (0..rows.len()).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut why: HashMap<usize, BTreeSet<&'static str>> = HashMap::new();
        let mut by: HashMap<(&'static str, String), usize> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            let mut keys: Vec<(&'static str, String)> = vec![("même nom", vcard::name_key(&r.fields.name))];
            keys.extend(r.fields.emails.iter().map(|e| ("même email", e.value.clone())));
            keys.extend(r.fields.phones.iter().filter_map(|p| Some(digits(&p.value)).filter(|d| d.len() >= 7).map(|d| ("même téléphone", d[d.len() - 9.min(d.len())..].to_owned()))));
            for k in keys.into_iter().filter(|k| !k.1.is_empty()) {
                match by.get(&k) {
                    Some(&j) => {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        if a != b {
                            parent[a] = b;
                        }
                        why.entry(i).or_default().insert(k.0);
                        why.entry(j).or_default().insert(k.0);
                    }
                    None => {
                        by.insert(k, i);
                    }
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..rows.len() {
            let root = find(&mut parent, i);
            groups.entry(root).or_default().push(i);
        }
        let out: Vec<Value> = groups
            .into_values()
            .filter(|g| g.len() > 1)
            .map(|g| {
                let reasons: BTreeSet<&str> = g.iter().flat_map(|i| why.get(i).into_iter().flatten().copied()).collect();
                json!({ "reasons": reasons, "cards": g.iter().map(|&i| json!({ "path": rows[i].path, "name": rows[i].fields.name, "emails": rows[i].fields.emails.iter().map(|e| &e.value).collect::<Vec<_>>(), "phones": rows[i].fields.phones.iter().map(|p| &p.value).collect::<Vec<_>>() })).collect::<Vec<_>>() })
            })
            .take(200)
            .collect();
        Ok(json!({ "groups": out }))
    }

    /// Fusionne les fiches `drop` dans `keep` : téléphones, emails, adresses, sites et groupes s'additionnent sans doublon, les champs vides de
    /// `keep` se complètent, les notes se rejoignent, la clé Nostr de `keep` est gardée (sinon celle d'une fiche fusionnée). Les fiches `drop` sont
    /// supprimées ; celle du titulaire n'est jamais fusionnée.
    pub async fn merge(&self, owner: &str, keep: &str, drop: &[String]) -> Result<Value, BookError> {
        if drop.is_empty() || drop.len() > 20 || drop.iter().any(|d| d == keep) {
            return Err(BookError::Invalid("choisissez 1 à 20 fiches à fusionner dans la fiche gardée".into()));
        }
        self.merge_cards(owner, keep, drop).await?;
        self.links.sync_owner(owner).await?;
        Ok(json!({ "kept": keep, "removed": drop.len() }))
    }

    /// Le cœur de la fusion, sans relire le carnet : la fiche gardée n'est réécrite que si elle change (un doublon exact
    /// ne la modifie pas, et le téléphone n'a alors rien à resynchroniser) ; les visages des fiches fusionnées la rejoignent.
    async fn merge_cards(&self, owner: &str, keep: &str, drop: &[String]) -> Result<(), BookError> {
        let keep_text = self.read(owner, keep).await?;
        let mut f = vcard::read_fields(&keep_text);
        let before = f.clone();
        let mut text = keep_text.clone();
        let mut notes = vec![f.note.clone()];
        for d in drop {
            let t = self.read(owner, d).await?;
            if vcard::is_me(&t) {
                return Err(BookError::Invalid("votre propre fiche ne se fusionne pas".into()));
            }
            let g = vcard::read_fields(&t);
            for (dst, src) in [(&mut f.org, &g.org), (&mut f.title, &g.title), (&mut f.birthday, &g.birthday), (&mut f.about, &g.about), (&mut f.picture, &g.picture), (&mut f.nip05, &g.nip05), (&mut f.lud16, &g.lud16), (&mut f.given, &g.given), (&mut f.family, &g.family)] {
                if dst.is_empty() {
                    dst.clone_from(src);
                }
            }
            for p in g.phones {
                if !f.phones.iter().any(|x| same_phone(&x.value, &p.value)) {
                    f.phones.push(p);
                }
            }
            for e in g.emails {
                if !f.emails.iter().any(|x| x.value == e.value) {
                    f.emails.push(e);
                }
            }
            for a in g.addresses {
                if !f.addresses.contains(&a) {
                    f.addresses.push(a);
                }
            }
            for u in g.urls {
                if !f.urls.contains(&u) {
                    f.urls.push(u);
                }
            }
            for grp in g.groups {
                if !f.groups.contains(&grp) {
                    f.groups.push(grp);
                }
            }
            notes.push(g.note);
            if vcard::parse(text.as_bytes()).ok().and_then(|c| c.nostr).is_none() {
                if let Some(k) = vcard::parse(t.as_bytes()).ok().and_then(|c| c.nostr) {
                    text = vcard::set_property(&text, "X-NOSTR-PUBKEY", Some(&k), now());
                }
            }
        }
        let mut seen = BTreeSet::new();
        f.note = notes.into_iter().filter(|n| !n.is_empty() && seen.insert(n.clone())).collect::<Vec<_>>().join("\n");
        if f != before || text != keep_text {
            let out = vcard::apply_fields(&text, &f, now()).ok_or_else(|| BookError::Invalid("un nom est nécessaire".into()))?;
            self.write(owner, keep, out).await?;
        }
        // Les visages des fiches fusionnées rejoignent la fiche gardée avant que celles-ci disparaissent (sinon ils seraient
        // déliés). Les UID viennent de l'index : réécrire une fiche garde son UID.
        if let Some(kept) = self.links.uid_of(owner, keep).await? {
            for d in drop {
                if let Some(gone) = self.links.uid_of(owner, d).await?.filter(|u| *u != kept) {
                    self.db
                        .writer()
                        .execute("UPDATE faces SET contact_uid = ?3 WHERE owner_hex = ?1 AND contact_uid = ?2", vec![owner.to_owned().into(), gone.clone().into(), kept.clone().into()])
                        .await
                        .map_err(internal)?;
                    self.db
                        .writer()
                        .execute("UPDATE faces SET suggest_contact_uid = ?3 WHERE owner_hex = ?1 AND suggest_contact_uid = ?2", vec![owner.to_owned().into(), gone.into(), kept.clone().into()])
                        .await
                        .map_err(internal)?;
                }
            }
        }
        for d in drop {
            self.remove_file(owner, d).await?;
        }
        Ok(())
    }

    /// Doublons **exacts** : même nom, même société, mêmes emails, mêmes téléphones, avec au moins un email ou un téléphone
    /// (deux homonymes sans coordonnées ne sont jamais réunis). Par groupe, la fiche gardée est celle qui a le plus de visages,
    /// puis la plus remplie. Rend `(gardée, fusionnées)` par groupe.
    async fn exact_groups(&self, owner: &str) -> Result<Vec<(Row, Vec<Row>)>, BookError> {
        let faces = self.faces_by_uid(owner).await;
        let mut groups: BTreeMap<ExactKey, Vec<Row>> = BTreeMap::new();
        for r in self.rows(owner).await?.into_iter().filter(|r| !r.me) {
            let mut emails: Vec<String> = r.fields.emails.iter().map(|e| e.value.trim().to_lowercase()).filter(|e| !e.is_empty()).collect();
            let mut phones: Vec<String> = r.fields.phones.iter().map(|p| digits(&p.value)).filter(|d| d.len() >= 7).map(|d| d[d.len() - 9.min(d.len())..].to_owned()).collect();
            let name = vcard::name_key(&r.fields.name);
            if name.is_empty() || (emails.is_empty() && phones.is_empty()) {
                continue;
            }
            emails.sort();
            emails.dedup();
            phones.sort();
            phones.dedup();
            groups.entry((name, vcard::name_key(&r.fields.org), emails, phones)).or_default().push(r);
        }
        let weight = |r: &Row| (r.uid.as_ref().and_then(|u| faces.get(u)).copied().unwrap_or(0), serde_json::to_string(&r.fields).map_or(0, |j| j.len()));
        Ok(groups
            .into_values()
            .filter(|g| g.len() > 1)
            .map(|mut g| {
                g.sort_by(|a, b| weight(b).cmp(&weight(a)).then_with(|| a.path.cmp(&b.path)));
                let keep = g.remove(0);
                (keep, g)
            })
            .collect())
    }

    /// Fusionne les doublons exacts, par lots (`limit` groupes au plus, 1 à 200) ; `apply = false` : aperçu seulement
    /// (`{ groups, cards, sample }`). Le carnet n'est relu qu'une fois par lot. Rend `{ merged, removed, failed, remaining }` :
    /// l'interface enchaîne les lots tant qu'il en reste, et s'arrête si un lot n'avance plus.
    pub async fn merge_exact(&self, owner: &str, apply: bool, limit: usize) -> Result<Value, BookError> {
        let groups = self.exact_groups(owner).await?;
        if !apply {
            let sample: Vec<Value> = groups.iter().take(5).map(|(k, d)| json!({ "name": k.fields.name, "org": k.fields.org, "emails": k.fields.emails.iter().map(|e| &e.value).collect::<Vec<_>>(), "phones": k.fields.phones.iter().map(|p| &p.value).collect::<Vec<_>>(), "copies": d.len() + 1 })).collect();
            return Ok(json!({ "groups": groups.len(), "cards": groups.iter().map(|g| g.1.len()).sum::<usize>(), "sample": sample }));
        }
        let limit = limit.clamp(1, 200);
        let (mut merged, mut removed, mut failed) = (0, 0, Vec::new());
        for (keep, drop) in groups.iter().take(limit) {
            let paths: Vec<String> = drop.iter().map(|r| r.path.clone()).collect();
            match self.merge_cards(owner, &keep.path, &paths).await {
                Ok(()) => {
                    merged += 1;
                    removed += paths.len();
                }
                Err(BookError::Invalid(m) | BookError::NotFound(m) | BookError::Conflict(m) | BookError::Internal(m)) => failed.push(json!({ "path": keep.path, "name": keep.fields.name, "error": m })),
            }
        }
        self.links.sync_owner(owner).await?;
        Ok(json!({ "merged": merged, "removed": removed, "failed": failed, "remaining": groups.len().saturating_sub(merged) }))
    }

    /// Fiche où ranger des visages (table de tri, Trier en grand). Le **choix explicite** d'une fiche (`contact` : son UID) fait
    /// foi. Sinon, selon le nom : aucune fiche → elle est créée ; une seule → c'est elle ; plusieurs → conflit (« choisissez
    /// laquelle »), jamais deviné. `new` : une personne nouvelle, même si une fiche porte déjà ce nom (« une autre Marie »).
    /// Rend `{ path, uid, name, created }` : `name` est celui de la fiche, qui fait foi.
    pub async fn person_card(&self, owner: &str, name: &str, contact: Option<&str>, new: bool) -> Result<Value, BookError> {
        let (o, k) = (owner.to_owned(), vcard::name_key(name));
        if let Some(uid) = contact {
            let (o, u) = (owner.to_owned(), uid.to_owned());
            let found: Option<(String, String)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT path, name FROM identity_links WHERE owner_hex = ?1 AND uid = ?2 ORDER BY path LIMIT 1", [o, u], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.map_err(internal)?;
            let (path, name) = found.ok_or_else(|| BookError::NotFound("fiche introuvable : elle a peut-être été supprimée".into()))?;
            return Ok(json!({ "path": path, "uid": uid, "name": name, "created": false }));
        }
        let same: Vec<(String, Option<String>, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT path, uid, name FROM identity_links WHERE owner_hex = ?1 AND name_key = ?2 ORDER BY path")?;
                Ok(st.query_map([o, k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(internal)?;
        match (same.as_slice(), new) {
            ([(path, Some(uid), name)], false) => Ok(json!({ "path": path, "uid": uid, "name": name, "created": false })),
            ([(path, None, _)], false) => Err(BookError::Conflict(format!("la fiche {path} n'a pas d'identifiant (UID) : rouvrez-la dans Personnes pour la réparer"))),
            ([_, _, ..], false) => Err(BookError::Conflict(format!("{} fiches s'appellent « {} » : choisissez laquelle, ou créez une nouvelle personne", same.len(), name.trim()))),
            _ => {
                let made = self.links.create_from_person(owner, name, None, true).await?;
                Ok(json!({ "path": made["path"], "uid": made["uid"], "name": name.trim(), "created": true }))
            }
        }
    }

    /// « Ma fiche » : la fiche du titulaire, créée à la première visite à partir de son profil Nostr ; **le profil Nostr fait foi** pour les champs
    /// publics (la vue les montre tels que les autres les voient), et `drift` dit si la fiche chiffrée diffère encore du profil publié.
    pub async fn me(&self, owner: &str) -> Result<Value, BookError> {
        let path = match self.rows(owner).await?.into_iter().find(|r| r.me) {
            Some(r) => r.path,
            None => self.create_me(owner).await?,
        };
        let mut view = self.get(owner, &path).await?;
        if let Some(profile) = self.profile_of(owner).await {
            let mut f: Fields = serde_json::from_value(view["fields"].clone()).unwrap_or_default();
            for (key, _) in PROFILE_KEYS {
                let v = profile_value(&profile, key);
                if !v.is_empty() {
                    set_card_value(&mut f, key, &v);
                }
            }
            view["fields"] = serde_json::to_value(&f).map_err(internal)?;
        }
        Ok(view)
    }

    async fn create_me(&self, owner: &str) -> Result<String, BookError> {
        let o = owner.to_owned();
        let email: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [o], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.map_err(internal)?;
        let profile = self.profile_of(owner).await.unwrap_or_default();
        let name = Some(profile_value(&profile, "name")).filter(|n| !n.is_empty()).or_else(|| email.as_deref().and_then(|e| e.split('@').next()).map(str::to_owned)).unwrap_or_else(|| "Moi".to_owned());
        let base = vcard::new_card(&format!("me-{}", &owner[..owner.len().min(12)]), &name, email.as_deref(), Some(owner), now());
        let base = vcard::set_property(&base, "X-ASTRO-ME", Some("1"), now());
        let mut f = vcard::read_fields(&base);
        for (key, _) in PROFILE_KEYS {
            let v = profile_value(&profile, key);
            if !v.is_empty() {
                set_card_value(&mut f, key, &v);
            }
        }
        let out = vcard::apply_fields(&base, &f, now()).ok_or_else(|| BookError::Invalid("nom manquant".into()))?;
        let done = ingest::ingest(&self.dav, owner, carddav::CONTACTS_DIR, "Moi.vcf", Some("text/vcard"), std::io::Cursor::new(out.into_bytes())).await.map_err(internal)?;
        self.links.sync_owner(owner).await?;
        Ok(done.path)
    }

    /// Enregistre « Ma fiche » : **publie** le profil Nostr (champs publics) puis **écrit** la fiche chiffrée (tout) : une seule saisie, deux
    /// endroits toujours d'accord.
    pub async fn save_me(&self, owner: &str, fields: &Fields) -> Result<Value, BookError> {
        let account = self.account.as_ref().ok_or_else(|| BookError::Internal("publication du profil non disponible sur cette station".into()))?;
        let path = self.me(owner).await?["path"].as_str().map(str::to_owned).ok_or_else(|| BookError::Internal("fiche introuvable".into()))?;
        if fields.name.trim().is_empty() {
            return Err(BookError::Invalid("un nom est nécessaire".into()));
        }
        let patch = json!({ "name": fields.name.trim(), "about": fields.about.trim(), "picture": fields.picture.trim(), "website": fields.urls.first().map_or("", String::as_str), "nip05": fields.nip05.trim(), "lud16": fields.lud16.trim() });
        account.update_profile(owner, &patch).await.map_err(|e| match e {
            crate::account::AccountError::Invalid(m) => BookError::Invalid(m),
            other => BookError::Internal(format!("{other:?}")),
        })?;
        self.save(owner, Some(&path), fields).await?;
        self.me(owner).await
    }
}

/// Ligne de l'annuaire d'une fiche : clé de nom, clé Nostr, lien, avertissement.
type LinkRow = (Option<String>, Option<String>, Option<String>, Option<String>);
/// Clé d'un doublon exact : nom, société, emails, téléphones (comparables, triés).
type ExactKey = (String, String, Vec<String>, Vec<String>);

struct Row {
    path: String,
    fields: Fields,
    me: bool,
    search: String,
    pubkey: Option<String>,
    via: Option<String>,
    warning: Option<String>,
    uid: Option<String>,
}

/// Un champ typé simple, pour les appelants qui n'ont qu'une valeur.
pub fn typed(kind: &str, value: &str) -> Typed {
    Typed { kind: kind.to_owned(), value: value.to_owned() }
}
