//! Lecture des corps XML WebDAV entrants : `LOCK` (`lockinfo`) et `PROPPATCH` (`propertyupdate`).

use quick_xml::NsReader;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

/// Erreur de lecture XML.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// XML mal formé.
    #[error("XML mal formé")]
    Malformed,
    /// Corps trop gros.
    #[error("corps XML trop volumineux")]
    TooLarge,
}

/// Taille maximale d'un corps XML accepté.
pub const MAX_XML: usize = 64 * 1024;

/// Contenu d'un `lockinfo`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LockInfo {
    /// Verrou partagé demandé (non supporté : seul l'exclusif l'est).
    pub shared: bool,
    /// Texte de l'élément `owner`.
    pub owner: String,
}

fn ns_of(r: &ResolveResult<'_>) -> String {
    match r {
        ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
        _ => String::new(),
    }
}

/// Analyse un `lockinfo`.
pub fn parse_lockinfo(body: &[u8]) -> Result<LockInfo, ParseError> {
    if body.len() > MAX_XML {
        return Err(ParseError::TooLarge);
    }
    let mut reader = NsReader::from_reader(body);
    let (mut info, mut in_owner, mut open, mut buf) = (LockInfo::default(), false, 0i64, Vec::new());
    loop {
        let ev = reader.read_resolved_event_into(&mut buf).map_err(|_| ParseError::Malformed)?;
        match &ev.1 {
            Event::Start(_) => open += 1,
            Event::End(_) => open -= 1,
            Event::Eof if open != 0 => return Err(ParseError::Malformed),
            _ => {}
        }
        match ev {
            (ns, Event::Start(e) | Event::Empty(e)) if ns_of(&ns) == "DAV:" => match e.local_name().as_ref() {
                b"shared" => info.shared = true,
                b"owner" => in_owner = true,
                _ => {}
            },
            (_, Event::Text(t)) if in_owner => info.owner.push_str(&t.unescape().map_err(|_| ParseError::Malformed)?),
            (ns, Event::End(e)) if ns_of(&ns) == "DAV:" && e.local_name().as_ref() == b"owner" => in_owner = false,
            (_, Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    info.owner = info.owner.trim().chars().take(512).collect();
    Ok(info)
}

/// Analyse un `propertyupdate` : `(espace de noms, nom, Some(valeur) pour set | None pour remove)`.
pub fn parse_propertyupdate(body: &[u8]) -> Result<Vec<(String, String, Option<String>)>, ParseError> {
    if body.len() > MAX_XML {
        return Err(ParseError::TooLarge);
    }
    let mut reader = NsReader::from_reader(body);
    let mut out: Vec<(String, String, Option<String>)> = Vec::new();
    // pile de noms locaux DAV: ouverts, et profondeur relative sous `prop`
    let (mut mode_set, mut in_prop, mut depth, mut open, mut buf) = (None::<bool>, false, 0usize, 0i64, Vec::new());
    loop {
        let ev = reader.read_resolved_event_into(&mut buf).map_err(|_| ParseError::Malformed)?;
        match &ev.1 {
            Event::Start(_) => open += 1,
            Event::End(_) => open -= 1,
            Event::Eof if open != 0 => return Err(ParseError::Malformed),
            _ => {}
        }
        match ev {
            (ns, Event::Start(e)) => {
                let (nsu, local) = (ns_of(&ns), String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                if in_prop {
                    depth += 1;
                    if depth == 1 {
                        out.push((nsu, local, mode_set.unwrap_or(true).then(String::new)));
                    }
                } else if nsu == "DAV:" {
                    match local.as_str() {
                        "set" => mode_set = Some(true),
                        "remove" => mode_set = Some(false),
                        "prop" if mode_set.is_some() => in_prop = true,
                        _ => {}
                    }
                }
            }
            (ns, Event::Empty(e)) if in_prop && depth == 0 => {
                out.push((ns_of(&ns), String::from_utf8_lossy(e.local_name().as_ref()).into_owned(), mode_set.unwrap_or(true).then(String::new)));
            }
            (_, Event::Text(t)) if in_prop && depth >= 1 => {
                if let Some((_, _, Some(v))) = out.last_mut() {
                    v.push_str(&t.unescape().map_err(|_| ParseError::Malformed)?);
                }
            }
            (ns, Event::End(e)) => {
                if in_prop && depth > 0 {
                    depth -= 1;
                } else if ns_of(&ns) == "DAV:" {
                    match e.local_name().as_ref() {
                        b"prop" => in_prop = false,
                        b"set" | b"remove" => mode_set = None,
                        _ => {}
                    }
                }
            }
            (_, Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockinfo_variants() {
        let windows = br#"<?xml version="1.0"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>PC\fred</D:href></D:owner></D:lockinfo>"#;
        assert_eq!(parse_lockinfo(windows).unwrap(), LockInfo { shared: false, owner: "PC\\fred".into() });
        let shared = br#"<lockinfo xmlns="DAV:"><lockscope><shared/></lockscope><locktype><write/></locktype></lockinfo>"#;
        assert!(parse_lockinfo(shared).unwrap().shared);
        assert_eq!(parse_lockinfo(b"<a><b></a>"), Err(ParseError::Malformed));
        assert_eq!(parse_lockinfo(&vec![b' '; MAX_XML + 1]), Err(ParseError::TooLarge));
    }

    #[test]
    fn propertyupdate_set_and_remove_with_namespaces() {
        let body = br#"<?xml version="1.0"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:">
  <D:set><D:prop><Z:Win32LastModifiedTime>Thu, 05 Nov 2026 10:00:00 GMT</Z:Win32LastModifiedTime><Z:Win32FileAttributes>00000020</Z:Win32FileAttributes></D:prop></D:set>
  <D:remove><D:prop><Z:Win32CreationTime/></D:prop></D:remove>
</D:propertyupdate>"#;
        let got = parse_propertyupdate(body).unwrap();
        assert_eq!(
            got,
            [
                ("urn:schemas-microsoft-com:".into(), "Win32LastModifiedTime".into(), Some("Thu, 05 Nov 2026 10:00:00 GMT".into())),
                ("urn:schemas-microsoft-com:".into(), "Win32FileAttributes".into(), Some("00000020".into())),
                ("urn:schemas-microsoft-com:".into(), "Win32CreationTime".into(), None),
            ]
        );
        let dav_prop = br#"<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:getlastmodified>x</D:getlastmodified></D:prop></D:set></D:propertyupdate>"#;
        assert_eq!(parse_propertyupdate(dav_prop).unwrap()[0].0, "DAV:");
        assert_eq!(parse_propertyupdate(b"<x>"), Err(ParseError::Malformed));
    }
}
