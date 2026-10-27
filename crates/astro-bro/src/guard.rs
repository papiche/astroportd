//! Contenus qui ne doivent jamais entrer dans la mémoire sémantique.
//!
//! Une mémoire est relue dans le prompt du LLM (et envoyée à un fournisseur tiers si la personne a configuré sa
//! propre clé). Port des motifs de `memory_manager.unstorable_reason()` du legacy, sans repli : un doute refuse.

use std::sync::LazyLock;

use regex::Regex;

const PREFIXES: [&str; 1] = ["STORY_KEY_BACKUP:"];

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let table: [(&str, &str); 8] = [
        (r#""key_hex"\s*:"#, "clé de chiffrement (key_hex)"),
        (r"nsec1[0-9a-z]{20,}", "clé privée NOSTR (nsec)"),
        (r"-----BEGIN [A-Z ]*PRIVATE KEY-----", "clé privée PEM"),
        (
            r"(?i)\b(?:mot de passe|mdp|password|passphrase|secret|token|api[_ -]?key|code (?:pin|secret|d.acc[eè]s)|pin)\b(?:\s+[\w'’-]+){0,4}?\s*(?:est|is|c.est|:|=)\s*\S{4,}",
            "secret déclaré (mot de passe, token…)",
        ),
        (r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){3,7}(?: ?[A-Z0-9]{1,4})?\b", "IBAN"),
        (r"\b(?:\d[ -]?){15}\d\b", "numéro de carte bancaire"),
        (r"\b[12] ?\d{2} ?(?:0[1-9]|1[0-2]) ?\d{2} ?\d{3} ?\d{3}(?: ?\d{2})?\b", "numéro de sécurité sociale"),
        (r"\b(?:sk|pk|ghp|xox[bp])[-_][A-Za-z0-9_-]{16,}", "clé d'API"),
    ];
    // Les motifs sont des constantes : une erreur de syntaxe se verrait dès le premier test.
    table.iter().filter_map(|(rx, why)| Regex::new(rx).ok().map(|r| (r, *why))).collect()
});

/// Raison du refus de `content`, ou `None` s'il est mémorisable.
pub fn unstorable_reason(content: &str) -> Option<&'static str> {
    let head = content.trim_start();
    if PREFIXES.iter().any(|p| head.starts_with(p)) {
        return Some("message machine (sauvegarde de clé)");
    }
    PATTERNS.iter().find(|(rx, _)| rx.is_match(content)).map(|(_, why)| *why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_patterns_compile() {
        assert_eq!(PATTERNS.len(), 8);
    }

    #[test]
    fn secrets_are_refused_and_ordinary_sentences_pass() {
        for (text, why) in [
            ("ma clé est nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq", "clé privée NOSTR (nsec)"),
            ("mon mot de passe est hunter2024", "secret déclaré (mot de passe, token…)"),
            ("IBAN FR76 3000 6000 0112 3456 7890 189", "IBAN"),
            ("carte 4111 1111 1111 1111", "numéro de carte bancaire"),
            ("{\"key_hex\": \"ab\"}", "clé de chiffrement (key_hex)"),
            ("token: ghp_abcdefghijklmnop1234", "secret déclaré (mot de passe, token…)"),
        ] {
            assert_eq!(unstorable_reason(text), Some(why), "{text}");
        }
        assert!(unstorable_reason("STORY_KEY_BACKUP:abcd").is_some());
        for ok in ["J'aime le pain au levain et le jazz.", "Mon objectif : courir un semi-marathon en avril.", "Rendez-vous chez Léa le 12 mars."] {
            assert_eq!(unstorable_reason(ok), None, "{ok}");
        }
    }
}
