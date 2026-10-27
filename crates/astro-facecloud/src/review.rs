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
