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
    /// Recherche plein texte (NIP-50) : tous les mots doivent figurer dans le contenu.
    pub search: Option<String>,
}

/// Nombre maximal d'éléments par liste d'un filtre.
pub const MAX_LIST_LEN: usize = 500;

/// Longueur maximale d'une recherche (NIP-50), en caractères.
pub const MAX_SEARCH_LEN: usize = 256;

/// Mots d'une recherche NIP-50 : les extensions `clé:valeur` sont ignorées, 8 mots de 64 caractères au plus.
pub fn search_terms(search: &str) -> Vec<String> {
    search.split_whitespace().filter(|w| !w.contains(':')).take(8).map(|w| w.chars().take(64).collect::<String>().to_lowercase()).collect()
}

/// Lettre de base d'un caractère latin accentué (« é » → « e »), en minuscule ; les autres caractères sont seulement mis en minuscule.
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => 'c',
        'ď' | 'đ' => 'd',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ř' => 'r',
        'ś' | 'š' | 'ş' => 's',
        'ť' | 'ţ' => 't',
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        c => c,
    }
}

/// Mots d'un texte : suites de lettres et de chiffres, sans casse ni accents (le découpage de l'index FTS5 `unicode61`).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.chars().map(fold).collect()).collect()
}

/// Recherche NIP-50 sur un contenu, avec la même sémantique que l'index de la base : chaque mot de `search` (hors
/// extensions `clé:valeur`) est une suite de mots consécutifs du contenu dont le dernier peut n'en être que le début.
/// Un mot sans lettre ni chiffre ne trouve rien.
pub fn search_matches(search: &str, content: &str) -> bool {
    let hay = words(content);
    search_terms(search).iter().all(|term| {
        let needle = words(term);
        let Some((last, head)) = needle.split_last() else { return false };
        hay.windows(needle.len()).any(|w| w[..head.len()] == *head && w[head.len()].starts_with(last.as_str()))
    })
}

/// Nombre maximal de filtres de tags (`#e`, `#p`…) par filtre : chacun devient une sous-requête corrélée en SQL.
pub const MAX_TAG_FILTERS: usize = 8;

/// Nombre maximal de valeurs, tous filtres de tags confondus.
pub const MAX_TAG_VALUES: usize = 1000;

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
                "search" => {
                    let text = val.as_str().ok_or_else(|| FilterError::BadField(key.clone()))?;
                    if text.chars().count() > MAX_SEARCH_LEN {
                        return Err(FilterError::BadField("search (trop long)".to_owned()));
                    }
                    f.search = Some(text.to_owned());
                }
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
        if f.tags.len() > MAX_TAG_FILTERS || f.tags.values().map(Vec::len).sum::<usize>() > MAX_TAG_VALUES {
            return Err(FilterError::BadField("filtres de tags (trop nombreux)".to_owned()));
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
        if let Some(q) = &self.search {
            m.insert("search".into(), q.clone().into());
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
            && self.search.as_ref().is_none_or(|q| !matches!(e.kind, 4 | 1059) && search_matches(q, &e.content))
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
    #[test]
    fn live_search_matches_like_the_fts_index() {
        use super::search_matches as m;
        assert!(m("CAFE", "Un café sur la place"));
        assert!(m("caf", "café"));
        assert!(!m("afe", "café"), "pas de sous-chaîne au milieu d'un mot");
        assert!(m("café place", "la place du café"), "tous les mots, dans n'importe quel ordre");
        assert!(m("100%", "100% sûr") && !m("100%", "autre chose"));
        assert!(m("foo-bar", "un foo bar ici") && !m("foo-bar", "bar foo"), "mots consécutifs");
        assert!(!m("!!!", "!!!"), "sans lettre ni chiffre : rien");
        assert!(m("language:fr hello", "Hello"), "extensions ignorées");
        assert!(!m("_ello", "hello"));
    }

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
    fn bounds_the_number_of_tag_filters() {
        let many: serde_json::Map<String, Value> = ('a'..='i').map(|c| (format!("#{c}"), json!(["x"]))).collect();
        assert!(Filter::from_value(&Value::Object(many)).is_err());
        let wide: Vec<String> = (0..MAX_LIST_LEN).map(|i| i.to_string()).collect();
        assert!(Filter::from_value(&json!({"#a": wide, "#b": wide, "#c": wide})).is_err());
        assert!(Filter::from_value(&json!({"#a": wide, "#b": wide})).is_ok());
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
