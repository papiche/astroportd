//! Lecture minimale d'un corps `multipart/form-data` : **le premier champ `file`**, rien d'autre.
//!
//! Écrite ici plutôt que tirée d'une dépendance : l'API n'a besoin que de l'envoi d'une image (NIP-96), le corps est déjà
//! borné par l'appelant, et un analyseur de 80 lignes strict (structure CRLF exacte, une seule passe, aucune copie inutile)
//! vaut mieux qu'une bibliothèque de plus dans le chemin d'authentification d'une station. Tout ce qui s'écarte de la forme
//! attendue est **refusé**, jamais deviné.

/// Le champ `file` reçu.
#[derive(Debug, PartialEq, Eq)]
pub struct FilePart {
    /// Nom de fichier annoncé (`filename`), à ne jamais utiliser tel quel comme chemin.
    pub filename: Option<String>,
    /// Type annoncé par le client (indicatif : la station décide d'après le contenu).
    pub content_type: Option<String>,
    /// Octets du fichier.
    pub data: Vec<u8>,
}

/// Délimiteur annoncé par `Content-Type: multipart/form-data; boundary=…` (1 à 70 caractères permis par la RFC 2046).
pub fn boundary(content_type: &str) -> Result<String, String> {
    let mut parts = content_type.split(';');
    if !parts.next().is_some_and(|m| m.trim().eq_ignore_ascii_case("multipart/form-data")) {
        return Err("multipart/form-data attendu".into());
    }
    let b = parts.find_map(|p| p.trim().strip_prefix("boundary=").or_else(|| p.trim().strip_prefix("Boundary="))).ok_or("délimiteur multipart absent")?;
    let b = b.trim_matches('"');
    let valid = !b.is_empty() && b.len() <= 70 && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&c)) && !b.ends_with(' ');
    if valid { Ok(b.to_owned()) } else { Err("délimiteur multipart invalide".into()) }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// Valeur d'un paramètre `name="valeur"` d'un en-tête de partie (guillemets obligatoires, pas d'échappement admis).
fn param(header: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    // `name=` ne doit pas être la fin d'un autre paramètre (`filename=` quand on cherche `name=`) : on garde la bonne occurrence.
    let at = header.match_indices(&key).map(|(i, _)| i).find(|&i| i > 0 && matches!(header.as_bytes()[i - 1], b' ' | b';'))?;
    let start = at + key.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_owned())
}

/// Premier champ `file` du corps. Refuse : délimiteur absent ou répété de travers, en-têtes de partie sans `Content-Disposition`,
/// retour à la ligne nu, champ `file` absent, en-têtes de plus de 8 Kio.
pub fn first_file(content_type: &str, body: &[u8]) -> Result<FilePart, String> {
    let b = boundary(content_type)?;
    let delim = format!("--{b}").into_bytes();
    let inner = format!("\r\n--{b}").into_bytes();
    if !body.starts_with(&delim) {
        return Err("corps multipart mal formé (délimiteur initial)".into());
    }
    let mut pos = delim.len();
    loop {
        match body.get(pos..pos + 2) {
            Some(b"--") => return Err("champ « file » absent".into()),
            Some(b"\r\n") => pos += 2,
            _ => return Err("corps multipart mal formé (fin de délimiteur)".into()),
        }
        let head_end = find(body, b"\r\n\r\n", pos).ok_or("corps multipart mal formé (en-têtes de partie)")?;
        if head_end - pos > 8 * 1024 {
            return Err("en-têtes de partie trop longs".into());
        }
        let head = std::str::from_utf8(&body[pos..head_end]).map_err(|_| "en-têtes de partie illisibles".to_owned())?;
        let data_start = head_end + 4;
        // Un vrai délimiteur est suivi de CRLF (partie suivante) ou de `--` (fin) : une donnée qui en contient le début ne le coupe pas.
        let mut from = data_start;
        let end = loop {
            let at = find(body, &inner, from).ok_or("corps multipart mal formé (partie non terminée)")?;
            if matches!(body.get(at + inner.len()..at + inner.len() + 2), Some(b"\r\n" | b"--")) {
                break at;
            }
            from = at + 1;
        };
        let mut disposition = None;
        let mut content_type = None;
        for line in head.split("\r\n") {
            let (k, v) = line.split_once(':').ok_or("en-tête de partie mal formé")?;
            match k.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => disposition = Some(v.trim().to_owned()),
                "content-type" => content_type = Some(v.trim().to_owned()),
                _ => {}
            }
        }
        let disposition = disposition.ok_or("partie sans Content-Disposition")?;
        if disposition.split(';').next().map(str::trim) == Some("form-data") && param(&disposition, "name").as_deref() == Some("file") {
            return Ok(FilePart { filename: param(&disposition, "filename"), content_type, data: body[data_start..end].to_vec() });
        }
        pos = end + inner.len();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const CT: &str = "multipart/form-data; boundary=----X1";

    fn body(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (head, data) in parts {
            out.extend_from_slice(b"------X1\r\n");
            out.extend_from_slice(head.as_bytes());
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"------X1--\r\n");
        out
    }

    #[test]
    fn the_file_field_is_found_among_other_fields_and_its_bytes_are_kept_exactly() {
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\r\n------X1 pas un delimiteur\r\n\xff";
        let b = body(&[("Content-Disposition: form-data; name=\"npub\"", b"npub1abc"), ("Content-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png", png)]);
        let f = first_file(CT, &b).unwrap();
        assert_eq!((f.filename.as_deref(), f.content_type.as_deref()), (Some("a.png"), Some("image/png")));
        assert_eq!(f.data, png, "octets conservés, y compris un faux délimiteur et des CRLF");
    }

    #[test]
    fn a_param_named_name_is_never_confused_with_filename() {
        // Seul `filename="file"` est présent : ce n'est pas le champ `file`.
        let b = body(&[("Content-Disposition: form-data; name=\"x\"; filename=\"file\"", b"abc")]);
        assert_eq!(first_file(CT, &b), Err("champ « file » absent".into()));
    }

    #[test]
    fn hostile_or_malformed_bodies_are_refused_never_guessed() {
        let good = body(&[("Content-Disposition: form-data; name=\"file\"; filename=\"a.png\"", b"x")]);
        assert!(first_file(CT, &good).is_ok());
        for (ct, b) in [
            ("application/json", good.clone()),
            ("multipart/form-data", good.clone()),
            ("multipart/form-data; boundary=", good.clone()),
            ("multipart/form-data; boundary=a\r\nb", good.clone()),
            (CT, Vec::new()),
            (CT, b"------X1".to_vec()),
            (CT, b"------X1\r\n".to_vec()),
            (CT, b"------X1\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\ndonnee sans fin".to_vec()),
            (CT, b"------X1\nContent-Disposition: form-data; name=\"file\"\n\ndata\n------X1--\n".to_vec()),
            (CT, b"------X1\r\nContent-Type: image/png\r\n\r\nx\r\n------X1--\r\n".to_vec()),
            (CT, b"------X1\r\nnot-a-header\r\n\r\nx\r\n------X1--\r\n".to_vec()),
            (CT, b"------X1garbage".to_vec()),
        ] {
            assert!(first_file(ct, &b).is_err(), "{ct} {:?}", String::from_utf8_lossy(&b));
        }
        let mut long = b"------X1\r\nContent-Disposition: form-data; name=\"file\"; x=\"".to_vec();
        long.extend(std::iter::repeat_n(b'a', 9000));
        long.extend_from_slice(b"\"\r\n\r\nx\r\n------X1--\r\n");
        assert_eq!(first_file(CT, &long), Err("en-têtes de partie trop longs".into()));
    }

    #[test]
    fn the_boundary_follows_the_rfc_alphabet_and_length() {
        assert_eq!(boundary("multipart/form-data; boundary=\"abc DEF\"").unwrap(), "abc DEF");
        assert!(boundary(&format!("multipart/form-data; boundary={}", "a".repeat(71))).is_err());
        assert!(boundary("multipart/form-data; boundary=a;b").is_ok(), "un `;` termine la valeur");
        assert!(boundary("multipart/form-data; boundary=é").is_err());
    }
}
