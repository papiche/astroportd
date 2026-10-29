//! vCard 3.0 / 4.0 (RFC 2426, RFC 6350) : lecture des champs utiles à l'annuaire et au rapprochement des identités,
//! et réécriture ciblée d'une propriété sans toucher au reste de la fiche.
//!
//! Champs étendus reconnus : `X-NOSTR-PUBKEY` (64 hexadécimaux ou `npub1…`), `X-NOSTR-NPUB` (`npub1…`), `X-G1-PUBKEY`
//! (clé publique Ğ1 en base 58). Les autres propriétés sont conservées telles quelles (photos, adresses, groupes…).

use bech32::primitives::decode::CheckedHrpstring;

/// Taille maximale d'une fiche acceptée.
pub const MAX_CARD: usize = 256 * 1024;

/// Fiche refusée.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VcardError {
    /// Pas du texte UTF-8.
    #[error("la fiche n'est pas du texte UTF-8")]
    NotUtf8,
    /// Fiche trop volumineuse.
    #[error("fiche trop volumineuse (256 Ko au plus)")]
    TooLarge,
    /// `BEGIN:VCARD` / `END:VCARD` absents ou mal placés.
    #[error("ce n'est pas une vCard (BEGIN:VCARD … END:VCARD attendus)")]
    NotACard,
    /// Version non prise en charge.
    #[error("version vCard non prise en charge (2.1, 3.0 ou 4.0)")]
    Version,
    /// Ni `FN` ni `N`.
    #[error("la fiche n'a pas de nom (FN)")]
    NoName,
}

/// Ce que l'annuaire retient d'une fiche.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    /// `UID`, s'il existe.
    pub uid: Option<String>,
    /// Nom affiché (`FN`, à défaut `N` recomposé).
    pub name: String,
    /// Adresses email, en minuscules, sans doublon.
    pub emails: Vec<String>,
    /// Clé Nostr (64 hexadécimaux) donnée par `X-NOSTR-PUBKEY` ou `X-NOSTR-NPUB`.
    pub nostr: Option<String>,
    /// `X-NOSTR-*` présent mais illisible : à signaler plutôt qu'à ignorer en silence.
    pub nostr_invalid: Option<String>,
    /// Clé publique Ğ1 (`X-G1-PUBKEY`).
    pub g1: Option<String>,
}

/// Lignes logiques : les lignes repliées (suite commençant par un espace ou une tabulation) sont recollées.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match (line.strip_prefix([' ', '\t']), out.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => out.push(line.to_owned()),
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// `(NOM en majuscules sans groupe, valeur)` d'une ligne ; `None` si ce n'est pas une propriété.
fn split_property(line: &str) -> Option<(String, &str)> {
    let (head, value) = line.split_once(':')?;
    let name = head.split(';').next()?.rsplit('.').next()?.trim();
    (!name.is_empty()).then(|| (name.to_ascii_uppercase(), value))
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// Clé Nostr (64 hexadécimaux) d'une valeur : hex, `npub1…` ou `nostr:npub1…`.
pub fn nostr_key(value: &str) -> Option<String> {
    let v = value.trim().trim_start_matches("nostr:");
    if astro_nostr_proto::is_hex64(v) {
        return Some(v.to_owned());
    }
    let parsed = CheckedHrpstring::new::<bech32::Bech32>(v).ok()?;
    if parsed.hrp().as_str() != "npub" {
        return None;
    }
    let bytes: Vec<u8> = parsed.byte_iter().collect();
    (bytes.len() == 32).then(|| hex::encode(bytes))
}

fn is_g1_key(v: &str) -> bool {
    (40..=48).contains(&v.len()) && v.chars().all(|c| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l'))
}

/// Analyse une fiche.
pub fn parse(bytes: &[u8]) -> Result<Card, VcardError> {
    if bytes.len() > MAX_CARD {
        return Err(VcardError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| VcardError::NotUtf8)?;
    let lines = unfold(text.trim_start_matches('\u{feff}'));
    let begin = lines.iter().position(|l| !l.trim().is_empty());
    if begin.is_none_or(|i| !lines[i].trim().eq_ignore_ascii_case("BEGIN:VCARD")) || !lines.last().is_some_and(|l| l.trim().eq_ignore_ascii_case("END:VCARD")) {
        return Err(VcardError::NotACard);
    }
    let mut card = Card::default();
    let (mut version_ok, mut structured) = (false, None);
    for line in &lines {
        let Some((name, value)) = split_property(line) else { continue };
        match name.as_str() {
            "VERSION" => version_ok = matches!(value.trim(), "2.1" | "3.0" | "4.0"),
            "UID" => card.uid = Some(value.trim().trim_start_matches("urn:uuid:").to_owned()).filter(|u| !u.is_empty()),
            "FN" if card.name.is_empty() => card.name = unescape(value).trim().to_owned(),
            "N" => structured = Some(value.to_owned()),
            "EMAIL" => {
                let e = value.trim().to_ascii_lowercase();
                if e.contains('@') && !card.emails.contains(&e) {
                    card.emails.push(e);
                }
            }
            "X-NOSTR-PUBKEY" | "X-NOSTR-NPUB" => match nostr_key(value) {
                Some(k) if card.nostr.is_none() => card.nostr = Some(k),
                Some(_) => {}
                None => card.nostr_invalid = Some(format!("{name} illisible : {}", value.trim().chars().take(40).collect::<String>())),
            },
            "X-G1-PUBKEY" if is_g1_key(value.trim()) => card.g1 = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    if !version_ok {
        return Err(VcardError::Version);
    }
    if card.name.is_empty() {
        // N: famille;prénom;autres;préfixe;suffixe → « prénom famille ».
        let parts: Vec<String> = structured.unwrap_or_default().split(';').map(|p| unescape(p).trim().to_owned()).collect();
        card.name = [parts.get(1), parts.first()].into_iter().flatten().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    }
    if card.name.is_empty() {
        return Err(VcardError::NoName);
    }
    Ok(card)
}

/// Nom comparable : minuscules, sans accents, ponctuation réduite à une espace (« Léa  Martin » = « lea martin »).
pub fn name_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        let folded: &str = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
            'ç' | 'ć' | 'č' => "c",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ñ' | 'ń' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
            'ù' | 'ú' | 'û' | 'ü' => "u",
            'ý' | 'ÿ' => "y",
            'œ' => "oe",
            'æ' => "ae",
            'ß' => "ss",
            c if c.is_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => " ",
        };
        out.push_str(folded);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replie une ligne à 75 octets (RFC 6350 §3.2), sans couper un caractère.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let (mut width, mut first) = (0usize, true);
    for c in line.chars() {
        let limit = if first { 75 } else { 74 };
        if width + c.len_utf8() > limit {
            out.push_str("\r\n ");
            (width, first) = (0, false);
        }
        out.push(c);
        width += c.len_utf8();
    }
    out
}

fn rev_now(unix: i64) -> String {
    crate::xml::iso8601(unix).replace(['-', ':'], "")
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n").replace(',', "\\,").replace(';', "\\;")
}

/// Remplace (ou ajoute) la propriété `name` ; `value = None` la retire. `REV` est mis à jour. Le reste est conservé.
pub fn set_property(card: &str, name: &str, value: Option<&str>, now: i64) -> String {
    let wanted = name.to_ascii_uppercase();
    let mut lines: Vec<String> = unfold(card);
    let end = lines.iter().rposition(|l| l.trim().eq_ignore_ascii_case("END:VCARD")).unwrap_or(lines.len());
    lines.truncate(end);
    lines.retain(|l| !split_property(l).is_some_and(|(n, _)| n == wanted || n == "REV"));
    if let Some(v) = value {
        lines.push(format!("{wanted}:{v}"));
    }
    lines.push(format!("REV:{}", rev_now(now)));
    lines.push("END:VCARD".to_owned());
    let mut out = lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n");
    out.push_str("\r\n");
    out
}

/// Nouvelle fiche 3.0.
pub fn new_card(uid: &str, name: &str, email: Option<&str>, nostr_hex: Option<&str>, now: i64) -> String {
    let mut lines = vec!["BEGIN:VCARD".to_owned(), "VERSION:3.0".to_owned(), format!("UID:{uid}"), format!("FN:{}", escape(name)), format!("N:;{};;;", escape(name))];
    if let Some(e) = email {
        lines.push(format!("EMAIL;TYPE=INTERNET:{e}"));
    }
    if let Some(k) = nostr_hex {
        lines.push(format!("X-NOSTR-PUBKEY:{k}"));
    }
    lines.push(format!("REV:{}", rev_now(now)));
    lines.push("END:VCARD".to_owned());
    let mut out = lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n");
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "7d1a5f6c0b3e9a24c8f1d5b7e2a40c93186fb5d2e7a9034c1d8f6b2a5e70913c";

    fn npub(hex: &str) -> String {
        let bytes: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").unwrap(), &bytes).unwrap()
    }

    #[test]
    fn a_card_gives_its_name_emails_and_keys() {
        let text = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urn:uuid:abc-1\r\nFN:Léa Martin\r\nEMAIL;TYPE=HOME:Lea@Example.org\r\nitem1.EMAIL:lea@example.org\r\nEMAIL:autre@example.org\r\nX-NOSTR-NPUB:{}\r\nX-G1-PUBKEY:5JpkKTRnTFpUiLEmuUsGZ4BFBWdMEUfVF1Km5r4UJ9KN\r\nEND:VCARD\r\n", npub(HEX));
        let c = parse(text.as_bytes()).unwrap();
        assert_eq!(c.uid.as_deref(), Some("abc-1"));
        assert_eq!((c.name.as_str(), c.emails.as_slice()), ("Léa Martin", ["lea@example.org".to_owned(), "autre@example.org".to_owned()].as_slice()));
        assert_eq!(c.nostr.as_deref(), Some(HEX));
        assert!(c.g1.is_some() && c.nostr_invalid.is_none());
    }

    #[test]
    fn hex_npub_and_nostr_uri_are_all_accepted_and_bad_keys_are_reported() {
        for v in [HEX.to_owned(), npub(HEX), format!("nostr:{}", npub(HEX))] {
            let c = parse(format!("BEGIN:VCARD\nVERSION:4.0\nFN:X\nX-NOSTR-PUBKEY:{v}\nEND:VCARD\n").as_bytes()).unwrap();
            assert_eq!(c.nostr.as_deref(), Some(HEX), "{v}");
        }
        let c = parse(b"BEGIN:VCARD\nVERSION:4.0\nFN:X\nX-NOSTR-PUBKEY:pas-une-cle\nEND:VCARD\n").unwrap();
        assert!(c.nostr.is_none() && c.nostr_invalid.as_deref().unwrap().contains("illisible"));
        // Un npub corrompu (somme de contrôle) ou un nsec n'est pas une clé publique.
        let mut bad = npub(HEX);
        bad.replace_range(10..11, if &bad[10..11] == "q" { "p" } else { "q" });
        assert!(nostr_key(&bad).is_none());
        assert!(nostr_key("nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5").is_none());
    }

    #[test]
    fn folded_lines_and_structured_names_are_understood() {
        let c = parse(b"BEGIN:VCARD\r\nVERSION:3.0\r\nN:Martin;L\xc3\xa9a;;;\r\nNOTE:une tr\xc3\xa8s longue\r\n  note\r\nEND:VCARD\r\n").unwrap();
        assert_eq!(c.name, "Léa Martin");
        let c = parse(b"BEGIN:VCARD\nVERSION:3.0\nFN:Jean\\, Paul\nEND:VCARD").unwrap();
        assert_eq!(c.name, "Jean, Paul");
    }

    #[test]
    fn invalid_cards_are_refused_with_a_reason() {
        assert_eq!(parse(b"hello"), Err(VcardError::NotACard));
        assert_eq!(parse(b"BEGIN:VCARD\nFN:X\nEND:VCARD"), Err(VcardError::Version));
        assert_eq!(parse(b"BEGIN:VCARD\nVERSION:3.0\nEND:VCARD"), Err(VcardError::NoName));
        assert_eq!(parse(b"BEGIN:VCARD\nVERSION:3.0\nFN:X"), Err(VcardError::NotACard));
        assert_eq!(parse(&[0xff, 0xfe]), Err(VcardError::NotUtf8));
        assert_eq!(parse(&vec![b'a'; MAX_CARD + 1]), Err(VcardError::TooLarge));
    }

    #[test]
    fn names_compare_without_accents_case_or_punctuation() {
        assert_eq!(name_key("  Léa-MARTIN "), "lea martin");
        assert_eq!(name_key("Zoë Œuvre"), "zoe oeuvre");
        assert_eq!(name_key("Müller, Hans"), "muller hans");
        assert_ne!(name_key("Lea"), name_key("Léo"));
        assert_eq!(name_key("日本語"), "日本語");
    }

    #[test]
    fn setting_a_property_keeps_everything_else_and_bumps_rev() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Léa\r\nPHOTO;ENCODING=b:AAAA\r\n BBBB\r\nX-NOSTR-PUBKEY:ancienne\r\nREV:20200101T000000Z\r\nEND:VCARD\r\n";
        let out = set_property(card, "X-NOSTR-PUBKEY", Some(HEX), 1_800_000_000);
        let c = parse(out.as_bytes()).unwrap();
        assert_eq!((c.name.as_str(), c.nostr.as_deref()), ("Léa", Some(HEX)));
        assert!(out.contains("PHOTO;ENCODING=b:AAAABBBB") || out.contains("PHOTO;ENCODING=b:AAAA"), "{out}");
        assert_eq!(out.matches("X-NOSTR-PUBKEY").count(), 1);
        assert_eq!(out.matches("REV:").count(), 1);
        assert!(out.contains("REV:20270115T080000Z") && out.ends_with("END:VCARD\r\n"));
        let removed = set_property(&out, "X-NOSTR-PUBKEY", None, 1_800_000_000);
        assert!(!removed.contains("X-NOSTR-PUBKEY") && parse(removed.as_bytes()).unwrap().nostr.is_none());
    }

    #[test]
    fn long_lines_fold_at_75_octets_without_splitting_a_character() {
        let long = "é".repeat(100);
        let card = new_card("u1", &long, None, None, 0);
        assert!(card.split("\r\n").all(|l| l.len() <= 75), "{card}");
        assert_eq!(parse(card.as_bytes()).unwrap().name, long);
    }

    #[test]
    fn a_new_card_round_trips() {
        let card = new_card("uid-9", "Léa, la grande", Some("lea@example.org"), Some(HEX), 1_800_000_000);
        let c = parse(card.as_bytes()).unwrap();
        assert_eq!((c.uid.as_deref(), c.name.as_str(), c.emails.as_slice(), c.nostr.as_deref()), (Some("uid-9"), "Léa, la grande", ["lea@example.org".to_owned()].as_slice(), Some(HEX)));
    }
}
