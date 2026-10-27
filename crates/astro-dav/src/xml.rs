//! Réponses XML WebDAV (RFC 4918) et formats de date.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::locks::Lock;
use crate::vfs::{Entry, EntryKind, name_of};

/// Caractères à encoder dans un `href` : tout sauf les non réservés et `/`.
const HREF: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'`').add(b'{').add(b'}').add(b'[').add(b']').add(b'|').add(b'\\').add(b'^').add(b'&').add(b'+').add(b',').add(b';').add(b'=').add(b'@').add(b'$').add(b'!').add(b'\'').add(b'(').add(b')').add(b'*').add(b':');

/// Échappe le texte XML.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Date HTTP (RFC 7231) : `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn http_date(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{}, {d:02} {} {y} {:02}:{:02}:{:02} GMT", DAYS[days.rem_euclid(7) as usize], MONTHS[(m - 1) as usize], secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Date ISO 8601 UTC : `1994-11-06T08:49:37Z`.
pub fn iso8601(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// `href` d'une entrée : préfixe de montage + chemin encodé ; les dossiers finissent par `/`.
pub fn href(mount: &str, path: &str, dir: bool) -> String {
    let mount = mount.trim_end_matches('/');
    let encoded = if path == "/" { String::new() } else { utf8_percent_encode(path, HREF).to_string() };
    let slash = if dir && !encoded.ends_with('/') { "/" } else { "" };
    format!("{mount}{encoded}{slash}")
}

/// Fragment `<D:activelock>` d'un verrou.
pub fn activelock(mount: &str, l: &Lock, now: i64) -> String {
    format!(
        "<D:activelock><D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope><D:depth>{}</D:depth>\
<D:owner>{}</D:owner><D:timeout>Second-{}</D:timeout><D:locktoken><D:href>{}</D:href></D:locktoken>\
<D:lockroot><D:href>{}</D:href></D:lockroot></D:activelock>",
        if l.infinity { "infinity" } else { "0" },
        escape(&l.holder),
        (l.expires_at - now).max(0),
        escape(&l.token),
        escape(&href(mount, &l.path, false)),
    )
}

/// Nom XML sûr pour une propriété morte (les noms viennent d'un XML déjà analysé, mais on revérifie).
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) && !name.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.')
}

/// Corps `207 Multi-Status` listant `entries`, avec leurs verrous actifs et propriétés mortes.
pub fn multistatus(mount: &str, entries: &[Entry], locks: &[Lock], now: i64) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n");
    for e in entries {
        let dir = e.kind == EntryKind::Dir;
        out.push_str("<D:response>\n<D:href>");
        out.push_str(&escape(&href(mount, &e.path, dir)));
        out.push_str("</D:href>\n<D:propstat>\n<D:prop>\n");
        out.push_str(&format!("<D:displayname>{}</D:displayname>\n", escape(name_of(&e.path))));
        if dir {
            out.push_str("<D:resourcetype><D:collection/></D:resourcetype>\n");
        } else {
            out.push_str("<D:resourcetype/>\n");
            out.push_str(&format!("<D:getcontentlength>{}</D:getcontentlength>\n", e.size));
            if let Some(m) = &e.mime {
                out.push_str(&format!("<D:getcontenttype>{}</D:getcontenttype>\n", escape(m)));
            }
            if let Some(c) = &e.cid {
                out.push_str(&format!("<D:getetag>\"{}\"</D:getetag>\n", escape(c)));
            }
        }
        out.push_str(&format!("<D:getlastmodified>{}</D:getlastmodified>\n", http_date(e.mtime)));
        out.push_str(&format!("<D:creationdate>{}</D:creationdate>\n", iso8601(e.mtime)));
        out.push_str("<D:supportedlock><D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry></D:supportedlock>\n");
        out.push_str("<D:lockdiscovery>");
        for l in locks.iter().filter(|l| l.covers(&e.path, false)) {
            out.push_str(&activelock(mount, l, now));
        }
        out.push_str("</D:lockdiscovery>\n");
        for (i, (ns, name, value)) in e.dead_props.iter().enumerate() {
            if safe_name(name) {
                out.push_str(&format!("<p{i}:{name} xmlns:p{i}=\"{}\">{}</p{i}:{name}>\n", escape(ns), escape(value)));
            }
        }
        out.push_str("</D:prop>\n<D:status>HTTP/1.1 200 OK</D:status>\n</D:propstat>\n</D:response>\n");
    }
    out.push_str("</D:multistatus>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted_per_rfc() {
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(iso8601(784_111_777), "1994-11-06T08:49:37Z");
        assert_eq!(http_date(1_767_225_600), "Thu, 01 Jan 2026 00:00:00 GMT");
        assert_eq!(http_date(1_709_164_800), "Thu, 29 Feb 2024 00:00:00 GMT");
    }

    #[test]
    fn hrefs_are_percent_encoded_and_dirs_end_with_a_slash() {
        assert_eq!(href("/dav", "/", true), "/dav/");
        assert_eq!(href("/dav/", "/Photos/été 2026/a&b#1.jpg", false), "/dav/Photos/%C3%A9t%C3%A9%202026/a%26b%231.jpg");
        assert_eq!(href("/dav", "/Photos", true), "/dav/Photos/");
        assert_eq!(href("", "/a", false), "/a");
    }

    #[test]
    fn locks_and_dead_properties_are_rendered() {
        let lock = Lock { token: "opaquelocktoken:abc".into(), path: "/d".into(), infinity: true, expires_at: 1100, holder: "f<red>".into() };
        let entries = [Entry {
            path: "/d/f.txt".into(),
            kind: EntryKind::File,
            cid: None,
            mime: None,
            size: 1,
            mtime: 0,
            dead_props: vec![("urn:schemas-microsoft-com:".into(), "Win32FileAttributes".into(), "00000020".into()), ("urn:x".into(), "1bad".into(), "no".into())],
        }];
        let x = multistatus("/dav", &entries, &[lock], 1000);
        assert!(x.contains("<D:timeout>Second-100</D:timeout>") && x.contains("<D:owner>f&lt;red&gt;</D:owner>") && x.contains("<D:depth>infinity</D:depth>"));
        assert!(x.contains("<D:lockroot><D:href>/dav/d</D:href></D:lockroot>"));
        assert!(x.contains("<p0:Win32FileAttributes xmlns:p0=\"urn:schemas-microsoft-com:\">00000020</p0:Win32FileAttributes>"));
        assert!(!x.contains("1bad"), "nom de propriété invalide ignoré");
    }

    #[test]
    fn multistatus_lists_props_and_escapes_markup() {
        let entries = [
            Entry { path: "/".into(), kind: EntryKind::Dir, cid: None, mime: None, size: 0, mtime: 0, dead_props: Vec::new() },
            Entry { path: "/a<b>&.txt".into(), kind: EntryKind::File, cid: Some("bafkreix".into()), mime: Some("text/plain".into()), size: 42, mtime: 784_111_777, dead_props: Vec::new() },
        ];
        let x = multistatus("/dav", &entries, &[], 0);
        assert!(x.contains("<D:multistatus xmlns:D=\"DAV:\">"));
        assert!(x.contains("<D:resourcetype><D:collection/></D:resourcetype>"));
        assert!(x.contains("<D:getcontentlength>42</D:getcontentlength>"));
        assert!(x.contains("<D:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</D:getlastmodified>"));
        assert!(x.contains("<D:displayname>a&lt;b&gt;&amp;.txt</D:displayname>"));
        assert!(x.contains("<D:getetag>\"bafkreix\"</D:getetag>"));
        assert!(!x.contains("<b>"), "balisage injecté");
        assert_eq!(x.matches("<D:response>").count(), 2);
        assert!(x.contains("<D:supportedlock>"));
    }
}
