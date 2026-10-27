//! Filtres de souscription NIP-01.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::Event;

/// Erreurs d'analyse d'un filtre.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FilterError {
    /// Le filtre n'est pas un objet JSON.
    #[error("le filtre doit être un objet")]
    NotAnObject,
    /// Champ de type inattendu.
    #[error("champ de filtre invalide : {0}")]
    BadField(String),
}

/// Filtre NIP-01. Un champ absent ne contraint rien ; un tableau vide ne correspond à rien.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Identifiants d'événements exacts.
    pub ids: Option<Vec<String>>,
    /// Auteurs (clés publiques exactes).
    pub authors: Option<Vec<String>>,
    /// Kinds.
    pub kinds: Option<Vec<u32>>,
    /// Borne basse incluse sur `created_at`.
    pub since: Option<u64>,
    /// Borne haute incluse sur `created_at`.
    pub until: Option<u64>,
    /// Nombre maximal d'événements de l'historique.
    pub limit: Option<usize>,
    /// Filtres de tags à une lettre (`#e`, `#p`, `#d`…).
    pub tags: BTreeMap<char, Vec<String>>,
}

/// Nombre maximal d'éléments par liste d'un filtre.
pub const MAX_LIST_LEN: usize = 500;

impl Filter {
    /// Analyse un filtre JSON.
    pub fn from_value(v: &Value) -> Result<Self, FilterError> {
        let obj = v.as_object().ok_or(FilterError::NotAnObject)?;
        let mut f = Filter::default();
        for (key, val) in obj {
            match key.as_str() {
                "ids" => f.ids = Some(strings(key, val)?),
                "authors" => f.authors = Some(strings(key, val)?),
                "kinds" => {
                    let list = list(key, val)?;
                    let kinds = list
                        .iter()
                        .map(|k| k.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| FilterError::BadField(key.clone())))
                        .collect::<Result<Vec<_>, _>>()?;
                    f.kinds = Some(kinds);
                }
                "since" => f.since = Some(val.as_u64().ok_or_else(|| FilterError::BadField(key.clone()))?),
                "until" => f.until = Some(val.as_u64().ok_or_else(|| FilterError::BadField(key.clone()))?),
                "limit" => {
                    let n = val.as_u64().ok_or_else(|| FilterError::BadField(key.clone()))?;
                    f.limit = Some(usize::try_from(n).unwrap_or(usize::MAX));
                }
                k if k.len() == 2 && k.starts_with('#') => {
                    let name = k.chars().nth(1).unwrap_or('?');
                    if name.is_ascii_alphabetic() {
                        f.tags.insert(name, strings(key, val)?);
                    }
                }
                _ => {}
            }
        }
        Ok(f)
    }

    /// Représentation JSON (pour `REQ`).
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        if let Some(v) = &self.ids {
            m.insert("ids".into(), v.clone().into());
        }
        if let Some(v) = &self.authors {
            m.insert("authors".into(), v.clone().into());
        }
        if let Some(v) = &self.kinds {
            m.insert("kinds".into(), v.clone().into());
        }
        if let Some(n) = self.since {
            m.insert("since".into(), n.into());
        }
        if let Some(n) = self.until {
            m.insert("until".into(), n.into());
        }
        if let Some(n) = self.limit {
            m.insert("limit".into(), n.into());
        }
        for (name, values) in &self.tags {
            m.insert(format!("#{name}"), values.clone().into());
        }
        Value::Object(m)
    }

    /// `true` si `event` satisfait le filtre (hors `limit`).
    pub fn matches(&self, e: &Event) -> bool {
        self.ids.as_ref().is_none_or(|v| v.contains(&e.id))
            && self.authors.as_ref().is_none_or(|v| v.contains(&e.pubkey))
            && self.kinds.as_ref().is_none_or(|v| v.contains(&e.kind))
            && self.since.is_none_or(|t| e.created_at >= t)
            && self.until.is_none_or(|t| e.created_at <= t)
            && self.tags.iter().all(|(name, wanted)| {
                let name = name.to_string();
                e.tags.iter().any(|t| {
                    t.len() >= 2 && t[0] == name && wanted.iter().any(|w| *w == t[1])
                })
            })
    }
}

fn list<'a>(key: &str, v: &'a Value) -> Result<&'a Vec<Value>, FilterError> {
    let arr = v.as_array().ok_or_else(|| FilterError::BadField(key.to_owned()))?;
    if arr.len() > MAX_LIST_LEN {
        return Err(FilterError::BadField(format!("{key} (trop d'éléments)")));
    }
    Ok(arr)
}

fn strings(key: &str, v: &Value) -> Result<Vec<String>, FilterError> {
    list(key, v)?
        .iter()
        .map(|x| x.as_str().map(str::to_owned).ok_or_else(|| FilterError::BadField(key.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ev(kind: u32, pubkey: &str, created_at: u64, tags: Vec<Vec<&str>>) -> Event {
        Event {
            id: "i".repeat(64),
            pubkey: pubkey.into(),
            created_at,
            kind,
            tags: tags.into_iter().map(|t| t.into_iter().map(String::from).collect()).collect(),
            content: String::new(),
            sig: String::new(),
        }
    }

    #[test]
    fn parses_and_round_trips() {
        let v = json!({"kinds":[1,3],"authors":["aa"],"since":5,"until":9,"limit":10,"#p":["bb"],"#d":["x"],"unknown":1});
        let f = Filter::from_value(&v).unwrap();
        assert_eq!(f.kinds, Some(vec![1, 3]));
        assert_eq!(f.tags.get(&'p'), Some(&vec!["bb".to_string()]));
        assert_eq!(Filter::from_value(&f.to_value()).unwrap(), f);
    }

    #[test]
    fn rejects_malformed_fields() {
        assert_eq!(Filter::from_value(&json!([])), Err(FilterError::NotAnObject));
        assert!(Filter::from_value(&json!({"kinds":["a"]})).is_err());
        assert!(Filter::from_value(&json!({"authors":"x"})).is_err());
        assert!(Filter::from_value(&json!({"since":-1})).is_err());
        assert!(Filter::from_value(&json!({"kinds":[4294967296u64]})).is_err());
        let big: Vec<u32> = (0..501).collect();
        assert!(Filter::from_value(&json!({ "kinds": big })).is_err());
    }

    #[test]
    fn matching_semantics() {
        let e = ev(1, "aa", 100, vec![vec!["p", "bb"], vec!["t", "x"]]);
        assert!(Filter::default().matches(&e));
        assert!(Filter::from_value(&json!({"kinds":[1],"authors":["aa"],"#p":["bb","cc"],"since":100,"until":100})).unwrap().matches(&e));
        assert!(!Filter::from_value(&json!({"kinds":[2]})).unwrap().matches(&e));
        assert!(!Filter::from_value(&json!({"#p":["zz"]})).unwrap().matches(&e));
        assert!(!Filter::from_value(&json!({"since":101})).unwrap().matches(&e));
        assert!(!Filter::from_value(&json!({"until":99})).unwrap().matches(&e));
    }

    #[test]
    fn empty_lists_match_nothing() {
        let e = ev(1, "aa", 1, vec![]);
        assert!(!Filter::from_value(&json!({"authors":[]})).unwrap().matches(&e));
        assert!(!Filter::from_value(&json!({"kinds":[]})).unwrap().matches(&e));
    }
}
