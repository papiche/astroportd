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

    /// Lignes de l'index : `(chemin, champs, me, clé de nom, clé Nostr, lien, avertissement)`.
    async fn rows(&self, owner: &str) -> Result<Vec<Row>, BookError> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT k.path, k.fields, k.me, k.search, COALESCE(l.name_key, ''), l.pubkey, l.via, l.warning FROM contact_cards k \
                     LEFT JOIN identity_links l ON l.owner_hex = k.owner_hex AND l.path = k.path WHERE k.owner_hex = ?1 ORDER BY k.path LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![o, MAX_CARDS], |r| {
                        Ok(Row { path: r.get(0)?, fields: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(), me: r.get::<_, i64>(2)? == 1, search: r.get(3)?, name_key: r.get(4)?, pubkey: r.get(5)?, via: r.get(6)?, warning: r.get(7)? })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(internal)
    }

    async fn faces_by_key(&self, owner: &str) -> HashMap<String, i64> {
        let o = owner.to_owned();
        let rows: Vec<(String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT name, count(*) FROM faces WHERE owner_hex = ?1 AND ref IS NULL AND name IS NOT NULL AND name NOT LIKE 'Inconnu%' GROUP BY name")?;
                Ok(stmt.query_map([o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut out: HashMap<String, i64> = HashMap::new();
        for (name, n) in rows {
            *out.entry(vcard::name_key(&name)).or_default() += n;
        }
        out
    }

    /// Fiches filtrées par `q` (tous les mots, sans accents ni casse, dans le nom, la société, les emails, les chiffres des téléphones, les notes…)
    /// et par `group`, avec la liste des groupes et des lettres de l'index.
    pub async fn list(&self, owner: &str, q: &str, group: Option<&str>) -> Result<Value, BookError> {
        let rows = self.rows(owner).await?;
        let faces = self.faces_by_key(owner).await;
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
                "faces": faces.get(&r.name_key).copied().unwrap_or(0), "picture": r.fields.picture,
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
            .read(move |c| Ok(c.query_row("SELECT name_key, pubkey, via, warning FROM identity_links WHERE owner_hex = ?1 AND path = ?2", [o, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?))
            .await
            .map_err(internal)?;
        let (name_key, pubkey, via, warning) = link.unwrap_or_default();
        let pubkey = if me { Some(owner.to_owned()) } else { pubkey };
        let profile = match &pubkey {
            Some(k) => self.profile_of(k).await,
            None => None,
        };
        let faces = self.faces_by_key(owner).await.get(&name_key).copied().unwrap_or(0);
        let hosted_here: Option<String> = match &pubkey {
            Some(k) => {
                let k = k.clone();
                self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1 AND hosted = 1", [k], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten()
            }
            None => None,
        };
        let comparison = profile.as_ref().map(|p| diff(&fields, p));
        Ok(json!({ "path": path, "fields": fields, "me": me, "pubkey": pubkey, "via": via, "warning": warning, "faces": faces, "hosted_here": hosted_here, "profile": profile.as_ref().map(|p| json!({
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

    /// Supprime une fiche (jamais celle du titulaire).
    pub async fn delete(&self, owner: &str, path: &str) -> Result<Value, BookError> {
        let text = self.read(owner, path).await?;
        if vcard::is_me(&text) {
            return Err(BookError::Invalid("votre propre fiche ne se supprime pas".into()));
        }
        let (o, p) = (owner.to_owned(), path.to_owned());
        let cids = astro_dav::vfs::vfs_tx(&self.dav.db, move |tx| astro_dav::vfs::delete_tree_tx(tx, &o, &p)).await.map_err(|e| BookError::Internal(e.to_string()))?;
        astro_dav::server::release_blobs(&self.dav, cids).await;
        if let Some(p) = &self.dav.publisher {
            p.notify(owner);
        }
        self.links.sync_owner(owner).await?;
        Ok(json!({ "deleted": path }))
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
        let keep_text = self.read(owner, keep).await?;
        let mut f = vcard::read_fields(&keep_text);
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
        let out = vcard::apply_fields(&text, &f, now()).ok_or_else(|| BookError::Invalid("un nom est nécessaire".into()))?;
        self.write(owner, keep, out).await?;
        for d in drop {
            self.delete(owner, d).await?;
        }
        self.links.sync_owner(owner).await?;
        Ok(json!({ "kept": keep, "removed": drop.len() }))
    }

    /// Fiche de `name` : la fiche existante, sinon une fiche neuve créée sur le champ (depuis la table de tri des visages).
    pub async fn ensure_person(&self, owner: &str, name: &str) -> Result<Value, BookError> {
        let key = vcard::name_key(name);
        let (o, k) = (owner.to_owned(), key);
        let found: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT path FROM identity_links WHERE owner_hex = ?1 AND name_key = ?2 ORDER BY path LIMIT 1", [o, k], |r| r.get(0)).optional()?)).await.map_err(internal)?;
        if let Some(path) = found {
            return Ok(json!({ "path": path, "created": false }));
        }
        let made = self.links.create_from_person(owner, name, None).await?;
        Ok(json!({ "path": made["path"], "created": true }))
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
type LinkRow = (String, Option<String>, Option<String>, Option<String>);

struct Row {
    path: String,
    fields: Fields,
    me: bool,
    search: String,
    name_key: String,
    pubkey: Option<String>,
    via: Option<String>,
    warning: Option<String>,
}

/// Un champ typé simple, pour les appelants qui n'ont qu'une valeur.
pub fn typed(kind: &str, value: &str) -> Typed {
    Typed { kind: kind.to_owned(), value: value.to_owned() }
}
