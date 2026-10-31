//! Validation humaine des visages : nommer un visage, accepter ou refuser une suggestion « est-ce X ? ».
//!
//! Fonctions libres (elles n'ont besoin que de la base) : l'interface web les appelle sans connaître le moteur
//! d'inférence ni le stockage des blobs.

use astro_db::Db;
use rusqlite::types::Value;

use crate::FaceError;

/// Nomme un visage (validation humaine) ; efface la suggestion.
pub async fn name_face(db: &Db, owner: &str, face_id: i64, pubkey: Option<&str>, name: &str) -> Result<(), FaceError> {
    let n = db
        .writer()
        .execute(
            "UPDATE faces SET name = ?3, pubkey = ?4, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL WHERE owner_hex = ?1 AND face_id = ?2",
            vec![owner.to_owned().into(), face_id.into(), name.to_owned().into(), pubkey.map_or(Value::Null, |p| p.to_owned().into())],
        )
        .await?;
    if n.changes == 0 { Err(FaceError::Invalid("visage introuvable")) } else { Ok(()) }
}

/// Valide la suggestion « est-ce X ? » d'un visage : il prend le nom et la clé suggérés. `false` : rien à valider.
pub async fn accept_suggestion(db: &Db, owner: &str, face_id: i64) -> Result<bool, FaceError> {
    let n = db
        .writer()
        .execute(
            "UPDATE faces SET name = suggest_name, pubkey = suggest_pubkey, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL \
             WHERE owner_hex = ?1 AND face_id = ?2 AND suggest_name IS NOT NULL",
            vec![owner.to_owned().into(), face_id.into()],
        )
        .await?;
    Ok(n.changes > 0)
}

/// Visages rangés ou oubliés au plus par appel.
pub const BULK_MAX: usize = 2_000;

fn checked_ids(ids: &[i64]) -> Result<(), FaceError> {
    if ids.is_empty() || ids.len() > BULK_MAX || ids.iter().any(|i| *i <= 0) {
        return Err(FaceError::Invalid("1 à 2000 identifiants de visages attendus"));
    }
    Ok(())
}

/// Range des visages dans une personne : ils prennent `name` (et la clé Nostr de la personne).
///
/// Sans `pubkey`, un nom déjà porté par une personne qui a une clé la reprend : ranger dans « Marie » ne crée pas
/// une seconde Marie sans clé. Les références de contacts et visages confiés par des amis ne sont jamais touchés.
/// Rend le nombre de visages modifiés.
pub async fn name_faces(db: &Db, owner: &str, ids: &[i64], pubkey: Option<&str>, name: &str) -> Result<usize, FaceError> {
    checked_ids(ids)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(FaceError::Invalid("nom attendu (80 caractères, sans caractère de contrôle)"));
    }
    if pubkey.is_some_and(|p| !astro_nostr_proto::is_hex64(p)) {
        return Err(FaceError::Invalid("clé Nostr invalide (64 caractères hexadécimaux)"));
    }
    let (o, n, pk, ids) = (owner.to_owned(), name.to_owned(), pubkey.map(str::to_owned), ids.to_vec());
    Ok(db
        .writer()
        .transaction(move |tx| {
            let pk = match pk {
                Some(p) => Some(p),
                None => tx
                    .query_row("SELECT pubkey FROM faces WHERE owner_hex = ?1 AND name = ?2 AND pubkey IS NOT NULL AND ref IS NULL LIMIT 1", rusqlite::params![o, n], |r| r.get::<_, String>(0))
                    .ok(),
            };
            let mut changed = 0;
            for id in ids {
                changed += tx.execute(
                    "UPDATE faces SET name = ?3, pubkey = ?4, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL WHERE owner_hex = ?1 AND face_id = ?2 AND ref IS NULL",
                    rusqlite::params![o, id, n, pk],
                )?;
            }
            Ok(changed)
        })
        .await?)
}

/// « Pas à reconnaître » : oublie ces visages (empreinte et fiche). Les photos restent dans le uCloud.
/// Rend le nombre de visages oubliés.
pub async fn forget_faces(db: &Db, owner: &str, ids: &[i64]) -> Result<usize, FaceError> {
    checked_ids(ids)?;
    let (o, ids) = (owner.to_owned(), ids.to_vec());
    Ok(db
        .writer()
        .transaction(move |tx| {
            let mut gone = 0;
            for id in ids {
                let mine: Option<i64> = tx.query_row("SELECT face_id FROM faces WHERE owner_hex = ?1 AND face_id = ?2 AND ref IS NULL", rusqlite::params![o, id], |r| r.get(0)).ok();
                if mine.is_none() {
                    continue;
                }
                tx.execute("DELETE FROM face_vectors WHERE face_id = ?1", [id])?;
                gone += tx.execute("DELETE FROM faces WHERE owner_hex = ?1 AND face_id = ?2", rusqlite::params![o, id])?;
            }
            Ok(gone)
        })
        .await?)
}

/// Lot de photos en attente de confirmation pour un ami (zone de précaution : visage flou ou score < seuil d'envoi direct).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBatch {
    /// Clé Nostr de l'ami.
    pub recipient_hex: String,
    /// Nom de la personne (celui de ses visages), à défaut le début de sa clé.
    pub name: String,
    /// Nombre de photos en attente.
    pub count: usize,
    /// Une photo du lot, pour l'aperçu.
    pub sample_path: String,
}

/// Lots en attente de `owner`, du plus fourni au plus petit.
pub async fn pending_batches(db: &Db, owner: &str) -> Result<Vec<PendingBatch>, FaceError> {
    let o = owner.to_owned();
    Ok(db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT p.recipient_hex, COUNT(*), MAX(p.path), \
                        (SELECT f.name FROM faces f WHERE f.owner_hex = p.owner_hex AND f.pubkey = p.recipient_hex ORDER BY f.face_id DESC LIMIT 1) \
                 FROM face_share_pending p WHERE p.owner_hex = ?1 GROUP BY p.recipient_hex ORDER BY COUNT(*) DESC, p.recipient_hex",
            )?;
            let rows = stmt.query_map([&o], |r| {
                let hex: String = r.get(0)?;
                let name: Option<String> = r.get(3)?;
                Ok(PendingBatch { name: name.unwrap_or_else(|| hex.chars().take(8).collect()), recipient_hex: hex, count: usize::try_from(r.get::<_, i64>(1)?).unwrap_or(0), sample_path: r.get(2)? })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}
