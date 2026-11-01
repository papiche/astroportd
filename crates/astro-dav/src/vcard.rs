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

/// Une valeur typée d'une fiche (`TEL;TYPE=CELL:…`, `EMAIL;TYPE=WORK:…`) : `kind` vaut `mobile`, `work`, `home` ou `other`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Typed {
    /// Nature (`mobile`, `work`, `home`, `other`).
    pub kind: String,
    /// Valeur.
    pub value: String,
}

/// Une adresse postale (`ADR`).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Address {
    /// Nature (`work`, `home`, `other`).
    pub kind: String,
    /// Rue.
    pub street: String,
    /// Ville.
    pub city: String,
    /// Région.
    pub region: String,
    /// Code postal.
    pub code: String,
    /// Pays.
    pub country: String,
}

/// Tout ce que l'annuaire sait lire et écrire d'une fiche. Les propriétés inconnues (photo, `X-…`) ne sont jamais touchées.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Fields {
    /// Nom affiché (`FN`).
    pub name: String,
    /// Prénom (`N`).
    #[serde(default)]
    pub given: String,
    /// Nom de famille (`N`).
    #[serde(default)]
    pub family: String,
    /// Société.
    #[serde(default)]
    pub org: String,
    /// Fonction.
    #[serde(default)]
    pub title: String,
    /// Téléphones.
    #[serde(default)]
    pub phones: Vec<Typed>,
    /// Adresses email.
    #[serde(default)]
    pub emails: Vec<Typed>,
    /// Adresses postales.
    #[serde(default)]
    pub addresses: Vec<Address>,
    /// Anniversaire `AAAA-MM-JJ` (ou `--MM-JJ` sans année).
    #[serde(default)]
    pub birthday: String,
    /// Notes.
    #[serde(default)]
    pub note: String,
    /// Sites web.
    #[serde(default)]
    pub urls: Vec<String>,
    /// Groupes (`CATEGORIES`).
    #[serde(default)]
    pub groups: Vec<String>,
    /// Présentation publique de la personne (`X-NOSTR-ABOUT`, miroir du champ `about` de son profil Nostr).
    #[serde(default)]
    pub about: String,
    /// Image (`PHOTO;VALUE=uri`, miroir de `picture`) ; une photo **incorporée** à la fiche n'est jamais touchée.
    #[serde(default)]
    pub picture: String,
    /// Identifiant NIP-05 (`X-NOSTR-NIP05`).
    #[serde(default)]
    pub nip05: String,
    /// Adresse Lightning (`X-NOSTR-LUD16`).
    #[serde(default)]
    pub lud16: String,
}

/// Cette fiche est-elle **celle du titulaire** (`X-ASTRO-ME`) ?
pub fn is_me(text: &str) -> bool {
    properties(text).iter().any(|(n, _, v)| n == "X-ASTRO-ME" && v.trim() == "1")
}

/// Texte de recherche d'une fiche : noms, société, emails, notes, groupes, adresses (sans accents ni casse) et chiffres des téléphones.
pub fn search_text(f: &Fields) -> String {
    let mut parts = vec![f.name.clone(), f.given.clone(), f.family.clone(), f.org.clone(), f.title.clone(), f.note.clone(), f.about.clone(), f.nip05.clone()];
    parts.extend(f.emails.iter().map(|e| e.value.clone()));
    parts.extend(f.groups.iter().cloned());
    parts.extend(f.addresses.iter().flat_map(|a| [a.street.clone(), a.city.clone(), a.code.clone(), a.country.clone()]));
    let mut out = name_key(&parts.join(" "));
    for p in &f.phones {
        let digits: String = p.value.chars().filter(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            out.push(' ');
            out.push_str(&digits);
        }
    }
    out
}

/// Plafonds : une fiche reste une fiche.
const MAX_ITEMS: usize = 20;
const MAX_TEXT: usize = 2000;

fn kind_of(params: &str) -> String {
    let p = params.to_ascii_lowercase();
    let has = |w: &str| p.split([';', ',', '=']).any(|t| t.trim() == w);
    if has("cell") || has("mobile") {
        "mobile"
    } else if has("work") {
        "work"
    } else if has("home") {
        "home"
    } else {
        "other"
    }
    .to_owned()
}

/// Tous les `(NOM, paramètres, valeur)` d'une fiche.
fn properties(text: &str) -> Vec<(String, String, String)> {
    unfold(text.trim_start_matches('\u{feff}'))
        .into_iter()
        .filter_map(|line| {
            let (head, value) = line.split_once(':')?;
            let mut parts = head.split(';');
            let name = parts.next()?.rsplit('.').next()?.trim().to_ascii_uppercase();
            (!name.is_empty()).then(|| (name, parts.collect::<Vec<_>>().join(";"), value.to_owned()))
        })
        .collect()
}

/// Découpe une valeur composée sur `sep` en respectant les `\` d'échappement.
fn split_escaped(v: &str, sep: char) -> Vec<String> {
    let (mut out, mut cur, mut esc) = (Vec::new(), String::new(), false);
    for c in v.chars() {
        match (esc, c) {
            (true, c) => {
                cur.push('\\');
                cur.push(c);
                esc = false;
            }
            (false, '\\') => esc = true,
            (false, c) if c == sep => out.push(std::mem::take(&mut cur)),
            (false, c) => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|p| unescape(&p)).collect()
}

/// Lit les champs modifiables d'une fiche.
pub fn read_fields(text: &str) -> Fields {
    let mut f = Fields::default();
    let mut structured: Option<String> = None;
    for (name, params, value) in properties(text) {
        match name.as_str() {
            "FN" if f.name.is_empty() => f.name = unescape(&value).trim().to_owned(),
            "N" => structured = Some(value),
            "ORG" => f.org = split_escaped(&value, ';').into_iter().next().unwrap_or_default().trim().to_owned(),
            "TITLE" => f.title = unescape(&value).trim().to_owned(),
            "NOTE" => f.note = unescape(&value).trim().to_owned(),
            "BDAY" => f.birthday = value.trim().to_owned(),
            "X-NOSTR-ABOUT" => f.about = unescape(&value).trim().to_owned(),
            "X-NOSTR-NIP05" => f.nip05 = value.trim().to_owned(),
            "X-NOSTR-LUD16" => f.lud16 = value.trim().to_owned(),
            "PHOTO" if value.trim_start().starts_with("http") && f.picture.is_empty() => f.picture = value.trim().to_owned(),
            "TEL" if !value.trim().is_empty() => f.phones.push(Typed { kind: kind_of(&params), value: value.trim().trim_start_matches("tel:").to_owned() }),
            "EMAIL" if value.contains('@') => {
                let v = value.trim().to_ascii_lowercase();
                if !f.emails.iter().any(|e| e.value == v) {
                    f.emails.push(Typed { kind: kind_of(&params), value: v });
                }
            }
            "URL" if !value.trim().is_empty() => f.urls.push(value.trim().to_owned()),
            "CATEGORIES" => f.groups.extend(split_escaped(&value, ',').into_iter().map(|g| g.trim().to_owned()).filter(|g| !g.is_empty())),
            "ADR" => {
                let p = split_escaped(&value, ';');
                let at = |i: usize| p.get(i).map(|x| x.trim().to_owned()).unwrap_or_default();
                let a = Address { kind: kind_of(&params), street: at(2), city: at(3), region: at(4), code: at(5), country: at(6) };
                if a != (Address { kind: a.kind.clone(), ..Address::default() }) {
                    f.addresses.push(a);
                }
            }
            _ => {}
        }
    }
    if let Some(n) = structured {
        let p = split_escaped(&n, ';');
        f.family = p.first().map(|x| x.trim().to_owned()).unwrap_or_default();
        f.given = p.get(1).map(|x| x.trim().to_owned()).unwrap_or_default();
        if f.name.is_empty() {
            f.name = [f.given.as_str(), f.family.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
        }
    }
    f.groups.dedup();
    f
}

fn type_param(kind: &str) -> &'static str {
    match kind {
        "mobile" => ";TYPE=CELL",
        "work" => ";TYPE=WORK",
        "home" => ";TYPE=HOME",
        _ => "",
    }
}

fn clean(v: &str) -> String {
    v.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().chars().take(MAX_TEXT).collect()
}

/// Écrit `f` dans la fiche `card` : les propriétés gérées (`FN`, `N`, `ORG`, `TITLE`, `TEL`, `EMAIL`, `ADR`, `BDAY`, `NOTE`, `URL`, `CATEGORIES`)
/// sont remplacées, **toutes les autres sont conservées** (photo, `X-NOSTR-PUBKEY`, `UID`…). `REV` est mis à jour. Rend `None` si le nom est vide.
pub fn apply_fields(card: &str, f: &Fields, now: i64) -> Option<String> {
    let name = clean(&f.name);
    if name.is_empty() {
        return None;
    }
    const MANAGED: [&str; 14] = ["FN", "N", "ORG", "TITLE", "TEL", "EMAIL", "ADR", "BDAY", "NOTE", "URL", "CATEGORIES", "X-NOSTR-ABOUT", "X-NOSTR-NIP05", "X-NOSTR-LUD16"];
    let mut lines: Vec<String> = unfold(card);
    let end = lines.iter().rposition(|l| l.trim().eq_ignore_ascii_case("END:VCARD")).unwrap_or(lines.len());
    lines.truncate(end);
    // Une photo `VALUE=uri` (adresse https) est gérée ici ; une photo incorporée (base64) reste telle quelle.
    lines.retain(|l| !split_property(l).is_some_and(|(n, v)| MANAGED.contains(&n.as_str()) || n == "REV" || (n == "PHOTO" && v.trim_start().starts_with("http"))));
    lines.push(format!("FN:{}", escape(&name)));
    // `N` se déduit du nom affiché quand on ne donne ni prénom ni nom de famille (« Léa Martin » → famille Martin, prénom Léa).
    let (mut given, mut family) = (clean(&f.given), clean(&f.family));
    if given.is_empty() && family.is_empty() {
        let mut parts: Vec<&str> = name.split_whitespace().collect();
        if parts.len() > 1 {
            family = parts.pop().unwrap_or_default().to_owned();
        }
        given = parts.join(" ");
    }
    lines.push(format!("N:{};{};;;", escape(&family), escape(&given)));
    let single = |lines: &mut Vec<String>, key: &str, v: &str| {
        let v = clean(v);
        if !v.is_empty() {
            lines.push(format!("{key}:{}", escape(&v)));
        }
    };
    single(&mut lines, "ORG", &f.org);
    single(&mut lines, "TITLE", &f.title);
    single(&mut lines, "NOTE", &f.note);
    single(&mut lines, "X-NOSTR-ABOUT", &f.about);
    for (key, v) in [("X-NOSTR-NIP05", &f.nip05), ("X-NOSTR-LUD16", &f.lud16)] {
        let v = clean(v);
        if !v.is_empty() && !v.contains(['\n', ' ']) {
            lines.push(format!("{key}:{v}"));
        }
    }
    let pic = clean(&f.picture);
    if pic.starts_with("https://") && !pic.contains(['\n', ' ']) {
        lines.push(format!("PHOTO;VALUE=uri:{pic}"));
    }
    let bday = f.birthday.trim();
    if !bday.is_empty() && bday.len() <= 12 && bday.chars().all(|c| c.is_ascii_digit() || c == '-') {
        lines.push(format!("BDAY:{bday}"));
    }
    for t in f.phones.iter().filter(|t| !t.value.trim().is_empty()).take(MAX_ITEMS) {
        lines.push(format!("TEL{}:{}", type_param(&t.kind), clean(&t.value).replace([';', ','], "")));
    }
    for t in f.emails.iter().filter(|t| t.value.contains('@')).take(MAX_ITEMS) {
        lines.push(format!("EMAIL;TYPE=INTERNET{}:{}", type_param(&t.kind), clean(&t.value).to_ascii_lowercase()));
    }
    for a in f.addresses.iter().take(MAX_ITEMS) {
        if [&a.street, &a.city, &a.region, &a.code, &a.country].iter().any(|x| !x.trim().is_empty()) {
            lines.push(format!("ADR{}:;;{};{};{};{};{}", type_param(&a.kind), escape(&clean(&a.street)), escape(&clean(&a.city)), escape(&clean(&a.region)), escape(&clean(&a.code)), escape(&clean(&a.country))));
        }
    }
    for u in f.urls.iter().filter(|u| !u.trim().is_empty()).take(MAX_ITEMS) {
        lines.push(format!("URL:{}", clean(u)));
    }
    let groups: Vec<String> = f.groups.iter().map(|g| clean(g)).filter(|g| !g.is_empty()).take(MAX_ITEMS).collect();
    if !groups.is_empty() {
        lines.push(format!("CATEGORIES:{}", groups.iter().map(|g| escape(g)).collect::<Vec<_>>().join(",")));
    }
    lines.push(format!("REV:{}", rev_now(now)));
    lines.push("END:VCARD".to_owned());
    let mut out = lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n");
    out.push_str("\r\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "7d1a5f6c0b3e9a24c8f1d5b7e2a40c93186fb5d2e7a9034c1d8f6b2a5e70913c";

    fn npub(hex: &str) -> String {
        let bytes: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").unwrap(), &bytes).unwrap()
    }

    const FULL: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Léa Martin\r\nN:Martin;Léa;;;\r\nORG:Atelier\\, Sud;Pôle\r\nTITLE:Cheffe\r\nTEL;TYPE=CELL:+33 6 12 34 56 78\r\nTEL;TYPE=WORK,VOICE:+33 5 61 00 00 00\r\nEMAIL;TYPE=HOME:Lea@Example.org\r\nADR;TYPE=HOME:;;1 rue des Fleurs;Toulouse;Occitanie;31000;France\r\nBDAY:1988-04-12\r\nNOTE:Rencontrée au marché\\nà revoir\r\nURL:https://lea.example\r\nCATEGORIES:Famille,Voisins\r\nX-NOSTR-ABOUT:Boulangère\\, voisine\r\nX-NOSTR-NIP05:lea@example.org\r\nPHOTO;VALUE=uri:https://img.example/lea.jpg\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\nX-ASTRO-ME:1\r\nX-NOSTR-PUBKEY:7d1a5f6c0b3e9a24c8f1d5b7e2a40c93186fb5d2e7a9034c1d8f6b2a5e70913c\r\nEND:VCARD\r\n";

    #[test]
    fn every_editable_field_is_read_with_its_kind() {
        let f = read_fields(FULL);
        assert_eq!((f.name.as_str(), f.given.as_str(), f.family.as_str(), f.org.as_str(), f.title.as_str()), ("Léa Martin", "Léa", "Martin", "Atelier, Sud", "Cheffe"));
        assert_eq!(f.phones, [Typed { kind: "mobile".into(), value: "+33 6 12 34 56 78".into() }, Typed { kind: "work".into(), value: "+33 5 61 00 00 00".into() }]);
        assert_eq!(f.emails, [Typed { kind: "home".into(), value: "lea@example.org".into() }]);
        assert_eq!((f.addresses[0].street.as_str(), f.addresses[0].city.as_str(), f.addresses[0].code.as_str(), f.addresses[0].country.as_str()), ("1 rue des Fleurs", "Toulouse", "31000", "France"));
        assert_eq!((f.about.as_str(), f.nip05.as_str(), f.picture.as_str(), is_me(FULL)), ("Boulangère, voisine", "lea@example.org", "https://img.example/lea.jpg", true));
        assert_eq!((f.birthday.as_str(), f.note.as_str(), f.urls.as_slice(), f.groups.as_slice()), ("1988-04-12", "Rencontrée au marché\nà revoir", ["https://lea.example".to_owned()].as_slice(), ["Famille".to_owned(), "Voisins".to_owned()].as_slice()));
    }

    #[test]
    fn writing_fields_changes_only_what_it_manages_and_reads_back_identically() {
        let mut f = read_fields(FULL);
        f.title = "Directrice".into();
        f.phones.push(Typed { kind: "home".into(), value: "+33 5 61 11 11 11".into() });
        f.groups = vec!["Équipe".into()];
        let out = apply_fields(FULL, &f, 1_800_000_000).unwrap();
        assert!(out.contains("PHOTO;ENCODING=b;TYPE=JPEG:AAAA") && out.contains("X-NOSTR-PUBKEY:7d1a5f6c") && out.contains("UID:u1") && out.contains("X-ASTRO-ME:1"), "photo incorporée, clé Nostr, UID et marque « moi » sont conservés");
        assert_eq!(out.matches("PHOTO").count(), 2, "une photo uri remplacée, jamais dupliquée");
        assert_eq!(out.matches("BEGIN:VCARD").count(), 1);
        assert!(out.ends_with("END:VCARD\r\n") && out.lines().all(|l| l.len() <= 76), "fiche valide, lignes repliées");
        let back = read_fields(&out);
        assert_eq!(back, f, "ce qu'on écrit se relit à l'identique");
        assert!(parse(out.as_bytes()).is_ok(), "la fiche reste lisible par l'annuaire");
        // Nom vide : refusé. Valeurs vides : propriétés retirées.
        assert!(apply_fields(FULL, &Fields::default(), 1).is_none());
        let bare = apply_fields(FULL, &Fields { name: "Léa".into(), ..Fields::default() }, 1).unwrap();
        assert!(!bare.contains("TEL") && !bare.contains("ORG") && !bare.contains("CATEGORIES") && bare.contains("X-NOSTR-PUBKEY"));
    }

    #[test]
    fn hostile_values_cannot_inject_properties() {
        let f = Fields { name: "Eve\r\nEMAIL:evil@x.org".into(), note: "a\r\nTEL:+1".into(), phones: vec![Typed { kind: "x".into(), value: "1;2,3\r\nNOTE:z".into() }], ..Fields::default() };
        let out = apply_fields("BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n", &f, 1).unwrap();
        let back = read_fields(&out);
        assert!(back.emails.is_empty(), "un saut de ligne dans un nom n'ajoute aucune propriété : {out}");
        assert_eq!(back.phones.len(), 1);
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
