//! **Paramètres IA d'un MULTIPASS** : l'Ollama (URL) ou la clé d'API (Anthropic, Gemini) que le titulaire déclare dans Mon compte.
//!
//! Stockage : un événement kind 31903 `d = ai`, signé de la clé du membre, dont le contenu est chiffré **pour lui seul** (NIP-44 vers
//! lui-même), comme le registre des agents ([`crate::agents`]). La clé d'API n'est donc jamais en clair dans la base, sur le relai ni
//! dans une réponse de l'API : l'interface ne reçoit que « une clé est enregistrée ». Le relai en fait un cache rejouable (`kv`, portée
//! `member_ai`) que le projecteur reconstruit au `replay`.
//!
//! Règle d'affichage : **BRO n'apparaît que si le MULTIPASS a déclaré une IA** (un Ollama, ou un fournisseur avec sa clé).

use std::sync::Arc;

use astro_crypto::{KeySource, nip44};
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::register::PublishFn;

/// `d`-tag (kind 31903).
pub const D_TAG: &str = "ai";
/// Portée `kv` du cache rejouable.
pub const KV_SCOPE: &str = "member_ai";
/// Fournisseurs d'API acceptés.
pub const PROVIDERS: [&str; 2] = ["anthropic", "gemini"];

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Paramètres IA du membre.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiParams {
    /// Adresse d'un Ollama (`http://hôte:11434`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<String>,
    /// `anthropic` ou `gemini`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Clé d'API du fournisseur : **jamais renvoyée par l'API**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Modèle préféré.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl AiParams {
    /// Une IA est-elle déclarée ? (Un Ollama, ou un fournisseur muni de sa clé.)
    pub fn declared(&self) -> bool {
        self.ollama_url.is_some() || (self.provider.is_some() && self.api_key.is_some())
    }

    /// Vue pour l'interface : tout sauf la clé.
    pub fn view(&self) -> Value {
        json!({ "ollama_url": self.ollama_url, "provider": self.provider, "has_key": self.api_key.is_some(), "model": self.model, "declared": self.declared() })
    }
}

/// Échec d'une opération sur les paramètres IA.
#[derive(Debug, PartialEq, Eq)]
pub enum AiError {
    /// Saisie refusée.
    Invalid(String),
    /// Panne de la station.
    Internal(String),
}

/// Adresse d'un Ollama : `http(s)://hôte[:port]`, sans identifiants, chemin ni fragment.
fn valid_url(raw: &str) -> Result<String, AiError> {
    let url = raw.trim().trim_end_matches('/');
    let bad = |why: &str| AiError::Invalid(format!("adresse d'Ollama : {why}"));
    if url.len() > 256 {
        return Err(bad("trop longue"));
    }
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")).ok_or_else(|| bad("http:// ou https:// attendu"))?;
    if rest.is_empty() || rest.contains(['@', '/', '?', '#', ' ']) {
        return Err(bad("hôte et port seulement (ex. http://127.0.0.1:11434), sans identifiants ni chemin"));
    }
    Ok(url.to_owned())
}

fn valid_key(raw: &str) -> Result<String, AiError> {
    let key = raw.trim();
    if key.len() < 8 || key.len() > 256 || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AiError::Invalid("clé d'API : 8 à 256 caractères, sans espace".into()));
    }
    Ok(key.to_owned())
}

fn valid_model(raw: &str) -> Result<String, AiError> {
    let m = raw.trim();
    if m.is_empty() || m.len() > 64 || !m.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-' | b'/')) {
        return Err(AiError::Invalid("modèle : 64 caractères au plus (lettres, chiffres, . _ : - /)".into()));
    }
    Ok(m.to_owned())
}

/// Service des paramètres IA.
pub struct MemberAi {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
}

impl MemberAi {
    /// Service signant avec les clés de `keys` et publiant par `publish`.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, publish: PublishFn) -> Self {
        Self { db, keys, publish }
    }

    async fn load_stamped(&self, owner: &str) -> Result<(AiParams, u64), AiError> {
        let o = owner.to_owned();
        let stored: Option<(String, i64)> = self
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_SCOPE, o], |r| Ok((r.get(0)?, r.get(1)?))))?))
            .await
            .map_err(|e| AiError::Internal(e.to_string()))?;
        let Some((payload, stamp)) = stored else { return Ok((AiParams::default(), 0)) };
        let secret = self.keys.secret(owner).ok_or_else(|| AiError::Internal("clé du compte absente de cette station".into()))?;
        let plain = nip44::decrypt_from_self(&secret, owner, &payload).map_err(|e| AiError::Internal(format!("paramètres IA illisibles : {e}")))?;
        Ok((serde_json::from_str(&plain).map_err(|e| AiError::Internal(format!("paramètres IA illisibles : {e}")))?, u64::try_from(stamp).unwrap_or(0)))
    }

    /// Paramètres du membre (vide s'il n'en a jamais déclaré).
    ///
    /// # Errors
    /// Base ou clé indisponibles.
    pub async fn get(&self, owner: &str) -> Result<AiParams, AiError> {
        self.load_stamped(owner).await.map(|(p, _)| p)
    }

    /// Une IA est-elle déclarée ? Une lecture impossible compte comme « non » (BRO reste masqué, rien ne casse).
    pub async fn declared(&self, owner: &str) -> bool {
        self.get(owner).await.is_ok_and(|p| p.declared())
    }

    /// Applique un correctif : un champ **absent** reste tel quel, un champ `null` ou vide est **effacé**. Valide, signe, publie, range.
    ///
    /// # Errors
    /// Saisie invalide, clé du compte absente, publication impossible.
    pub async fn set(&self, owner: &str, patch: &Value) -> Result<AiParams, AiError> {
        let (mut p, previous) = self.load_stamped(owner).await?;
        let field = |name: &str| -> Option<Option<String>> {
            patch.get(name).map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        };
        if let Some(v) = field("ollama_url") {
            p.ollama_url = v.map(|u| valid_url(&u)).transpose()?;
        }
        if let Some(v) = field("provider") {
            p.provider = match v {
                None => None,
                Some(name) if PROVIDERS.contains(&name.as_str()) => Some(name),
                Some(_) => return Err(AiError::Invalid(format!("fournisseur : {} attendu", PROVIDERS.join(" ou ")))),
            };
        }
        if let Some(v) = field("api_key") {
            p.api_key = v.map(|k| valid_key(&k)).transpose()?;
        }
        if let Some(v) = field("model") {
            p.model = v.map(|m| valid_model(&m)).transpose()?;
        }
        if p.provider.is_none() {
            if matches!(field("api_key"), Some(Some(_))) {
                return Err(AiError::Invalid("choisissez le fournisseur de cette clé d'API (anthropic ou gemini)".into()));
            }
            p.api_key = None; // une clé sans fournisseur ne sert à rien : retirer le fournisseur retire aussi la clé
        }
        let secret = self.keys.secret(owner).ok_or_else(|| AiError::Internal("clé du compte absente de cette station".into()))?;
        let plain = serde_json::to_string(&p).map_err(|e| AiError::Internal(e.to_string()))?;
        let content = nip44::encrypt_to_self(&secret, owner, &plain).map_err(|e| AiError::Internal(e.to_string()))?;
        // L'horodatage dépasse toujours celui de la version précédente : un relai garde le plus récent d'un événement remplaçable.
        let at = now().max(previous + 1);
        let event = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), D_TAG.into()]], content: content.clone() }.sign(&secret).map_err(|e| AiError::Internal(e.to_string()))?;
        (self.publish)(event).await.map_err(|e| AiError::Internal(format!("paramètres non publiés : {e}")))?;
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at WHERE excluded.updated_at >= kv.updated_at",
                vec![KV_SCOPE.to_owned().into(), owner.to_owned().into(), content.into(), i64::try_from(at).unwrap_or(0).into()],
            )
            .await
            .map_err(|e| AiError::Internal(e.to_string()))?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_nostr_proto::{Event, pubkey_of};

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32];

    async fn rig(tag: &str) -> (MemberAi, Arc<Mutex<Vec<Event>>>, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("astroportd-memberai-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let publish: PublishFn = Arc::new(move |e| {
            let log = log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(e);
                Ok(())
            })
        });
        let ai = MemberAi::new(db, Arc::new(StaticKeySource::new().with(ALICE)), publish);
        (ai, sent, pubkey_of(&ALICE).unwrap(), dir)
    }

    #[tokio::test]
    async fn nothing_is_declared_until_an_ollama_or_a_provider_with_its_key_is_given() {
        let (ai, _, me, dir) = rig("declared").await;
        assert!(!ai.declared(&me).await, "un MULTIPASS neuf n'a pas d'IA : BRO reste masqué");
        ai.set(&me, &json!({ "model": "llama3" })).await.unwrap();
        assert!(!ai.declared(&me).await, "un modèle seul ne déclare rien");
        ai.set(&me, &json!({ "provider": "anthropic" })).await.unwrap();
        assert!(!ai.declared(&me).await, "un fournisseur sans clé non plus");
        ai.set(&me, &json!({ "api_key": "sk-ant-1234567890" })).await.unwrap();
        assert!(ai.declared(&me).await, "fournisseur et clé : IA déclarée");
        ai.set(&me, &json!({ "api_key": null })).await.unwrap();
        assert!(!ai.declared(&me).await, "effacer la clé retire l'IA");
        ai.set(&me, &json!({ "ollama_url": "http://127.0.0.1:11434/" })).await.unwrap();
        let p = ai.get(&me).await.unwrap();
        assert_eq!((p.declared(), p.ollama_url.as_deref()), (true, Some("http://127.0.0.1:11434")), "un Ollama suffit (barre finale retirée)");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_api_key_is_published_encrypted_for_the_owner_and_never_shown() {
        let (ai, sent, me, dir) = rig("secret").await;
        ai.set(&me, &json!({ "provider": "gemini", "api_key": "AIzaSyTRESSECRET123" })).await.unwrap();
        let ev = sent.lock().unwrap()[0].clone();
        assert_eq!((ev.kind, ev.pubkey.as_str(), ev.tag_value("d")), (31903, me.as_str(), Some("ai")));
        assert!(!ev.content.contains("TRESSECRET"), "rien en clair sur le relai");
        let plain = nip44::decrypt_from_self(&ALICE, &me, &ev.content).unwrap();
        assert!(plain.contains("TRESSECRET"), "le titulaire la relit");
        let view = ai.get(&me).await.unwrap().view();
        assert_eq!((view["has_key"].as_bool(), view["provider"].as_str()), (Some(true), Some("gemini")));
        assert!(!view.to_string().contains("TRESSECRET"), "la vue de l'interface ne contient jamais la clé");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_input_is_refused_and_a_key_needs_its_provider() {
        let (ai, sent, me, dir) = rig("invalid").await;
        for bad in [json!({ "ollama_url": "ftp://x" }), json!({ "ollama_url": "http://u:p@h" }), json!({ "ollama_url": "http://h/chemin" }), json!({ "provider": "openai" }), json!({ "api_key": "court" }), json!({ "api_key": "avec espace 1234567" }), json!({ "model": "a b" }), json!({ "api_key": "sk-1234567890" })] {
            assert!(matches!(ai.set(&me, &bad).await, Err(AiError::Invalid(_))), "{bad}");
        }
        assert!(sent.lock().unwrap().is_empty(), "un refus ne publie rien");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_new_version_always_outranks_the_previous_one() {
        let (ai, sent, me, dir) = rig("order").await;
        for url in ["http://a:1", "http://b:2", "http://c:3"] {
            ai.set(&me, &json!({ "ollama_url": url })).await.unwrap();
        }
        let stamps: Vec<u64> = sent.lock().unwrap().iter().map(|e| e.created_at).collect();
        assert!(stamps.windows(2).all(|w| w[1] > w[0]), "horodatages strictement croissants : {stamps:?}");
        assert_eq!(ai.get(&me).await.unwrap().ollama_url.as_deref(), Some("http://c:3"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
