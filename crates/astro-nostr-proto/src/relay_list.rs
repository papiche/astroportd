//! NIP-65 : liste de relais d'un compte (kind 10002, remplaçable).
//!
//! Chaque tag `["r", url, marqueur?]` annonce un relai ; sans marqueur il sert en lecture et en écriture. Pour écrire à
//! quelqu'un, on publie sur ses relais de **lecture** ; pour le lire, on interroge ses relais d'**écriture**.

/// Un relai d'une liste NIP-65.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEntry {
    /// Adresse `wss://…` (ou `ws://…`).
    pub url: String,
    /// Le compte lit sur ce relai.
    pub read: bool,
    /// Le compte écrit sur ce relai.
    pub write: bool,
}

/// Adresse de relai acceptable : `wss://` ou `ws://`, un hôte, sans espace ni identifiants.
pub fn valid_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("wss://").or_else(|| url.strip_prefix("ws://")) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    url.len() <= 256 && !host.is_empty() && !host.contains('@') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Relais annoncés par les tags d'un kind 10002 (adresses invalides et doublons ignorés ; premier marqueur gagnant).
pub fn parse(tags: &[Vec<String>]) -> Vec<RelayEntry> {
    let mut out: Vec<RelayEntry> = Vec::new();
    for t in tags {
        if t.first().map(String::as_str) != Some("r") {
            continue;
        }
        let Some(url) = t.get(1).map(|u| u.trim_end_matches('/')).filter(|u| valid_url(u)) else { continue };
        let (read, write) = match t.get(2).map(String::as_str) {
            Some("read") => (true, false),
            Some("write") => (false, true),
            None | Some("") => (true, true),
            Some(_) => continue,
        };
        if !out.iter().any(|e| e.url == url) {
            out.push(RelayEntry { url: url.to_owned(), read, write });
        }
    }
    out
}

/// Tags d'un kind 10002 pour ces relais.
pub fn tags(entries: &[RelayEntry]) -> Vec<Vec<String>> {
    entries
        .iter()
        .map(|e| {
            let mut t = vec!["r".to_owned(), e.url.clone()];
            match (e.read, e.write) {
                (true, false) => t.push("read".into()),
                (false, true) => t.push("write".into()),
                _ => {}
            }
            t
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn markers_decide_read_and_write() {
        let got = parse(&[r(&["r", "wss://a.example"]), r(&["r", "wss://b.example/", "read"]), r(&["r", "ws://c.example:7777", "write"])]);
        assert_eq!(got.iter().map(|e| (e.url.as_str(), e.read, e.write)).collect::<Vec<_>>(), [("wss://a.example", true, true), ("wss://b.example", true, false), ("ws://c.example:7777", false, true)]);
    }

    #[test]
    fn bad_entries_and_duplicates_are_ignored() {
        let got = parse(&[r(&["p", "x"]), r(&["r", "https://a.example"]), r(&["r", "wss://u:p@a.example"]), r(&["r", "wss://"]), r(&["r", "wss://ok.example", "autre"]), r(&["r", "wss://ok.example"]), r(&["r", "wss://ok.example", "read"]), r(&["r", "wss://sp ace"])]);
        assert_eq!(got, [RelayEntry { url: "wss://ok.example".into(), read: true, write: true }]);
    }

    #[test]
    fn tags_round_trip() {
        let entries = vec![RelayEntry { url: "wss://a.example".into(), read: true, write: true }, RelayEntry { url: "wss://b.example".into(), read: true, write: false }, RelayEntry { url: "wss://c.example".into(), read: false, write: true }];
        assert_eq!(parse(&tags(&entries)), entries);
    }
}
