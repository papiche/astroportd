//! iCalendar (RFC 5545) : validation minimale d'un objet d'agenda avant de l'accepter en CalDAV.
//!
//! On ne développe pas les récurrences (`RRULE`) et on n'interprète pas les dates : l'objet est conservé tel quel
//! (chiffré, comme tout fichier du uCloud) et le client fait le calcul. On vérifie seulement que c'est bien un
//! `VCALENDAR` 2.0, qu'il porte au moins un événement, une tâche ou une note, et qu'ils ont **le même `UID`**
//! (RFC 4791 §4.1 : une ressource = un `UID`, avec ses éventuelles exceptions de récurrence).

/// Taille maximale d'un objet accepté (les pièces jointes en ligne sont rares et lourdes).
pub const MAX_OBJECT: usize = 512 * 1024;

/// Objet refusé.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IcalError {
    /// Pas du texte UTF-8.
    #[error("l'objet n'est pas du texte UTF-8")]
    NotUtf8,
    /// Objet trop volumineux.
    #[error("objet trop volumineux (512 Ko au plus)")]
    TooLarge,
    /// `BEGIN:VCALENDAR` / `END:VCALENDAR` absents ou mal placés.
    #[error("ce n'est pas un objet iCalendar (BEGIN:VCALENDAR … END:VCALENDAR attendus)")]
    NotACalendar,
    /// `VERSION` absente ou différente de 2.0.
    #[error("version iCalendar non prise en charge (2.0 attendue)")]
    Version,
    /// Aucun `VEVENT`, `VTODO` ni `VJOURNAL`.
    #[error("l'objet ne contient ni événement, ni tâche, ni note")]
    NoComponent,
    /// Un composant sans `UID`.
    #[error("un composant n'a pas d'UID")]
    NoUid,
    /// Plusieurs `UID` différents dans le même objet.
    #[error("plusieurs UID différents dans le même objet")]
    MixedUid,
}

/// Ce que le serveur retient d'un objet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// `UID` commun à tous les composants.
    pub uid: String,
    /// Nature du premier composant : `VEVENT`, `VTODO` ou `VJOURNAL`.
    pub component: String,
    /// Une règle de récurrence (`RRULE`) est présente.
    pub recurring: bool,
}

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

/// Valide un objet d'agenda.
pub fn parse(bytes: &[u8]) -> Result<Object, IcalError> {
    if bytes.len() > MAX_OBJECT {
        return Err(IcalError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| IcalError::NotUtf8)?;
    let lines = unfold(text.trim_start_matches('\u{feff}'));
    let first = lines.iter().position(|l| !l.trim().is_empty());
    if first.is_none_or(|i| !lines[i].trim().eq_ignore_ascii_case("BEGIN:VCALENDAR")) || !lines.last().is_some_and(|l| l.trim().eq_ignore_ascii_case("END:VCALENDAR")) {
        return Err(IcalError::NotACalendar);
    }
    // `depth` : 1 = dans le VCALENDAR, 2 = dans un composant (VEVENT…), 3 = dans un sous-composant (VALARM). L'UID et le
    // RRULE ne comptent qu'au niveau 2 : un VALARM n'a pas d'UID et un VTIMEZONE porte ses propres RRULE.
    let (mut depth, mut version_ok, mut recurring) = (0usize, false, false);
    let (mut current, mut current_uid, mut uids): (Option<String>, Option<String>, Vec<String>) = (None, None, Vec::new());
    let mut component = None::<String>;
    for line in &lines {
        let l = line.trim();
        let upper = l.to_ascii_uppercase();
        if let Some(name) = upper.strip_prefix("BEGIN:") {
            depth += 1;
            if depth == 2 {
                current = Some(name.to_owned());
                current_uid = None;
            }
            continue;
        }
        if let Some(name) = upper.strip_prefix("END:") {
            if depth == 2 {
                if let Some(c) = current.take().filter(|c| matches!(c.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")) {
                    uids.push(current_uid.take().ok_or(IcalError::NoUid)?);
                    component.get_or_insert(c);
                }
                let _ = name;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        let Some((head, value)) = l.split_once(':') else { continue };
        let prop = head.split(';').next().unwrap_or("").to_ascii_uppercase();
        match (depth, prop.as_str()) {
            (1, "VERSION") => version_ok = value.trim() == "2.0",
            (2, "UID") => current_uid = Some(value.trim().to_owned()).filter(|u| !u.is_empty()),
            (2, "RRULE") if matches!(current.as_deref(), Some("VEVENT" | "VTODO" | "VJOURNAL")) => recurring = true,
            _ => {}
        }
    }
    if !version_ok {
        return Err(IcalError::Version);
    }
    let component = component.ok_or(IcalError::NoComponent)?;
    let uid = uids[0].clone();
    if uids.iter().any(|u| *u != uid) {
        return Err(IcalError::MixedUid);
    }
    Ok(Object { uid, component, recurring })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261101T100000Z\r\nSUMMARY:Café\r\nRRULE:FREQ=WEEKLY\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn an_event_with_an_alarm_and_a_recurrence_is_read() {
        let o = parse(EVENT.as_bytes()).unwrap();
        assert_eq!((o.uid.as_str(), o.component.as_str(), o.recurring), ("e1", "VEVENT", true));
    }

    #[test]
    fn exceptions_share_the_uid_and_a_timezone_needs_none() {
        let two = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nRRULE:FREQ=YEARLY\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:x\r\nRECURRENCE-ID:20261108T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let o = parse(two.as_bytes()).unwrap();
        assert_eq!((o.uid.as_str(), o.recurring), ("x", false), "le RRULE du fuseau ne compte pas");
        let mixed = two.replacen("UID:x", "UID:y", 1);
        assert_eq!(parse(mixed.as_bytes()), Err(IcalError::MixedUid));
    }

    #[test]
    fn folded_lines_are_joined_and_garbage_is_refused() {
        let folded = EVENT.replace("UID:e1", "UID:e\r\n 1");
        assert_eq!(parse(folded.as_bytes()).unwrap().uid, "e1");
        assert_eq!(parse(b"n'importe quoi"), Err(IcalError::NotACalendar));
        assert_eq!(parse(&[0xff, 0xfe]), Err(IcalError::NotUtf8));
        assert_eq!(parse(EVENT.replace("2.0", "1.0").as_bytes()), Err(IcalError::Version));
        assert_eq!(parse(EVENT.replace("UID:e1\r\n", "").as_bytes()), Err(IcalError::NoUid));
        let none = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
        assert_eq!(parse(none.as_bytes()), Err(IcalError::NoComponent));
        assert_eq!(parse(&vec![b'a'; MAX_OBJECT + 1]), Err(IcalError::TooLarge));
    }
}
