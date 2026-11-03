//! **Liens d'accueil** de la station : un lien (et son QR) que le Capitaine affiche à l'entrée d'un lieu, sur un site ou dans
//! un article, et qui sert à **plusieurs** personnes.
//!
//! Qui l'ouvre (`/w/<jeton>`) crée son MULTIPASS en un formulaire (email, ou pseudo seul), devient **ami réciproque** du
//! Capitaine (chacun suit l'autre : kind 3 signés), et arrive directement dans la discussion avec lui. Contrairement à une
//! invitation `/i/` (une personne, des photos à recevoir), le lien d'accueil n'apporte aucun fichier : c'est une porte.
//!
//! Garde-fous : un nombre de places (`max_uses`), une date de fin, la révocation à tout moment, la limite d'inscriptions par
//! adresse de la station, et l'inscription reste fermée tant que la station n'a pas de Capitaine. Chaque place est réservée
//! **avant** la création du compte (acteur d'écriture unique : deux visiteurs simultanés ne dépassent pas le plafond) et rendue
//! si la création échoue.
//!
//! Stockage : état local, `kv(scope = 'welcome', key = <jeton>)`, valeur JSON. Rien n'est publié : le lien ne quitte la
//! station que par le Capitaine.

use std::net::IpAddr;
use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_dav::blob::BlobStore;
use astro_db::Db;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::invites::{InviteError, Invites};
use crate::register::{RegisterError, Registered};

const SCOPE: &str = "welcome";
/// Places par défaut, et au plus.
pub const DEFAULT_USES: u32 = 50;
const MAX_USES: u32 = 1000;
/// Durée par défaut, et au plus (jours).
pub const DEFAULT_DAYS: u32 = 90;
const MAX_DAYS: u32 = 366;
/// Liens actifs au plus par station.
const MAX_LINKS: usize = 20;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Les liens d'accueil de la station.
pub struct Welcome<B: BlobStore> {
    db: Arc<Db>,
    invites: Arc<Invites<B>>,
    activity: Option<ActivityBus>,
}

/// Arrivée par un lien d'accueil.
pub struct Arrived {
    /// Compte créé (secrets remis une seule fois).
    pub registered: Registered,
    /// Clé de qui a créé le lien : la discussion s'ouvre avec lui.
    pub host: String,
    /// Avertissements (amitié non établie…), sans bloquer l'arrivée.
    pub warnings: Vec<String>,
}

impl<B: BlobStore> Welcome<B> {
    /// Service des liens d'accueil.
    pub fn new(db: Arc<Db>, invites: Arc<Invites<B>>, activity: Option<ActivityBus>) -> Self {
        Self { db, invites, activity }
    }

    async fn load(&self, token: &str) -> Result<Option<Value>, InviteError> {
        let t = token.to_owned();
        let text: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, &t], |r| r.get(0)).optional()?)).await.map_err(|e| InviteError::Internal(e.to_string()))?;
        Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
    }

    /// Crée un lien d'accueil au nom de `owner` (le Capitaine). Rend `{ token, label, max_uses, expires_at }`.
    pub async fn create(&self, owner: &str, label: &str, max_uses: Option<u32>, days: Option<u32>) -> Result<Value, InviteError> {
        let label: String = label.trim().chars().take(80).collect();
        let max_uses = max_uses.unwrap_or(DEFAULT_USES);
        let days = days.unwrap_or(DEFAULT_DAYS);
        if !(1..=MAX_USES).contains(&max_uses) || !(1..=MAX_DAYS).contains(&days) {
            return Err(InviteError::Invalid(format!("entre 1 et {MAX_USES} places, entre 1 et {MAX_DAYS} jours")));
        }
        if self.list(owner).await?.iter().filter(|l| l["active"] == true).count() >= MAX_LINKS {
            return Err(InviteError::Invalid(format!("{MAX_LINKS} liens d'accueil actifs au plus : retirez-en un")));
        }
        let token = crate::invites::new_token()?;
        let at = now();
        let link = json!({ "owner": owner, "label": label, "max_uses": max_uses, "uses": 0, "created_at": at, "expires_at": at + i64::from(days) * 86_400, "revoked_at": null });
        self.db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), token.clone().into(), link.to_string().into(), at.into()])
            .await
            .map_err(|e| InviteError::Internal(e.to_string()))?;
        Ok(json!({ "token": token, "label": label, "max_uses": max_uses, "expires_at": link["expires_at"] }))
    }

    /// Liens de `owner`, du plus récent au plus ancien, avec leur état.
    pub async fn list(&self, owner: &str) -> Result<Vec<Value>, InviteError> {
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| Ok(c.prepare("SELECT key, value FROM kv WHERE scope = ?1")?.query_map([SCOPE], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .map_err(|e| InviteError::Internal(e.to_string()))?;
        let at = now();
        let mut out: Vec<Value> = rows
            .into_iter()
            .filter_map(|(token, v)| serde_json::from_str::<Value>(&v).ok().map(|l| (token, l)))
            .filter(|(_, l)| l["owner"] == owner)
            .map(|(token, mut l)| {
                l["token"] = json!(token);
                l["active"] = json!(state(&l, at).is_ok());
                l.as_object_mut().map(|o| o.remove("owner"));
                l
            })
            .collect();
        out.sort_by_key(|l| std::cmp::Reverse(l["created_at"].as_i64().unwrap_or(0)));
        Ok(out)
    }

    /// Retire un lien (il ne sert plus ; les comptes déjà créés restent).
    pub async fn revoke(&self, owner: &str, token: &str) -> Result<(), InviteError> {
        let (o, t, at) = (owner.to_owned(), token.to_owned(), now());
        let done = self
            .db
            .writer()
            .transaction(move |tx| {
                let Some(v) = tx.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, &t], |r| r.get::<_, String>(0)).optional()? else { return Ok(false) };
                let mut l: Value = serde_json::from_str(&v).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?;
                if l["owner"] != o.as_str() {
                    return Ok(false);
                }
                l["revoked_at"] = json!(at);
                tx.execute("UPDATE kv SET value = ?3, updated_at = ?4 WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, t, l.to_string(), at])?;
                Ok(true)
            })
            .await
            .map_err(|e| InviteError::Internal(e.to_string()))?;
        if done { Ok(()) } else { Err(InviteError::NotFound("lien d'accueil inconnu".into())) }
    }

    /// Ce que voit un visiteur avant de s'inscrire : `{ from, label, left, constellation }`.
    pub async fn preview(&self, token: &str) -> Result<Value, InviteError> {
        let l = self.load(token).await?.ok_or_else(|| InviteError::NotFound("lien d'accueil inconnu".into()))?;
        state(&l, now()).map_err(InviteError::Gone)?;
        let owner = l["owner"].as_str().unwrap_or_default();
        let left = l["max_uses"].as_u64().unwrap_or(0).saturating_sub(l["uses"].as_u64().unwrap_or(0));
        Ok(json!({ "from": self.invites.display_name(owner).await, "label": l["label"], "left": left, "constellation": self.invites.registrar().constellation() }))
    }

    /// Arrivée d'un visiteur : réserve une place, crée le compte (email, ou invité sans email), établit l'amitié réciproque
    /// avec l'hôte. Une place réservée est rendue si le compte ne peut pas être créé.
    pub async fn arrive(&self, token: &str, ip: Option<IpAddr>, email: Option<&str>, alias: Option<&str>, lang: &str) -> Result<Arrived, InviteError> {
        let host = self.reserve(token, 1).await?;
        let registrar = self.invites.registrar();
        let created = match email {
            Some(e) => registrar.register(ip, e, lang).await,
            None => registrar.register_guest(ip, alias, lang).await,
        };
        let registered = match created {
            Ok(r) => r,
            Err(e) => {
                let _ = self.reserve(token, -1).await;
                return Err(match e {
                    RegisterError::Closed | RegisterError::Full | RegisterError::NoCaptain => InviteError::Unavailable(e.to_string()),
                    RegisterError::RateLimited(_) => InviteError::RateLimited(e.to_string()),
                    RegisterError::Invalid(m) => InviteError::Invalid(m),
                    RegisterError::Conflict(_) => InviteError::Conflict("cette adresse a déjà un MULTIPASS ici : connectez-vous avec, la discussion vous attend".into()),
                    RegisterError::Internal(m) => InviteError::Internal(m),
                });
            }
        };
        let newcomer = registered.account.hex.clone();
        let warnings: Vec<String> = self.invites.befriend(&host, &newcomer).await.into_iter().map(|e| format!("amitié avec l'hôte : {e}")).collect();
        if let Some(bus) = &self.activity {
            let name = if registered.guest { registered.account.email.split('@').next().unwrap_or_default().to_owned() } else { registered.account.email.clone() };
            bus.publish_to(Audience::Owner(host.clone()), "chat", "welcome", format!("{name} a rejoint la station par votre lien d'accueil : écrivez-lui un mot de bienvenue"), None);
        }
        Ok(Arrived { registered, host, warnings })
    }

    /// Prend (`+1`) ou rend (`-1`) une place, dans une seule transaction. Rend la clé de l'hôte.
    async fn reserve(&self, token: &str, delta: i64) -> Result<String, InviteError> {
        let (t, at) = (token.to_owned(), now());
        let r: Result<String, &'static str> = self
            .db
            .writer()
            .transaction(move |tx| {
                let Some(v) = tx.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, &t], |r| r.get::<_, String>(0)).optional()? else { return Ok(Err("not_found")) };
                let mut l: Value = serde_json::from_str(&v).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?;
                if delta > 0 {
                    if let Err(code) = state(&l, at) {
                        return Ok(Err(code));
                    }
                }
                l["uses"] = json!((l["uses"].as_i64().unwrap_or(0) + delta).max(0));
                tx.execute("UPDATE kv SET value = ?3, updated_at = ?4 WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, t, l.to_string(), at])?;
                Ok(Ok(l["owner"].as_str().unwrap_or_default().to_owned()))
            })
            .await
            .map_err(|e| InviteError::Internal(e.to_string()))?;
        r.map_err(|code| if code == "not_found" { InviteError::NotFound("lien d'accueil inconnu".into()) } else { InviteError::Gone(code) })
    }
}

/// Un lien sert-il encore ? Sinon, le code stable de la raison.
fn state(l: &Value, at: i64) -> Result<(), &'static str> {
    if !l["revoked_at"].is_null() {
        return Err("invite_revoked");
    }
    if l["expires_at"].as_i64().unwrap_or(0) <= at {
        return Err("invite_expired");
    }
    if l["uses"].as_u64().unwrap_or(0) >= l["max_uses"].as_u64().unwrap_or(0) {
        return Err("welcome_full");
    }
    Ok(())
}
