//! Actions de l'écran « Assistance » du Capitaine : demande, réponse, annulation. L'accès aux services (SSH compris) s'ouvre
//! **uniquement** par `astroportd tunnel connect`, jamais depuis le navigateur.

use super::*;

/// Fonction à donner à `station_admin::Admin` pour la section `assist`.
pub(super) fn assist_admin(assist: &Arc<astroportd::assist::Assist<astroportd::mesh_outbox::MeshOutbox>>) -> astroportd::ui_backend::AdminFn {
    let assist = assist.clone();
    Arc::new(move |_section, body| {
        let assist = assist.clone();
        Box::pin(async move {
            let Some(b) = body else { return assist.view().await };
            match b["action"].as_str() {
                Some("request") => {
                    let sent = assist.request(b["service"].as_str().unwrap_or(""), b["hours"].as_i64().unwrap_or(0), b["note"].as_str().unwrap_or(""), b["to"].as_str().filter(|t| !t.is_empty()), b["exclusive"].as_bool().unwrap_or(false)).await?;
                    let mut view = assist.view().await?;
                    view["sent"] = sent["sent"].clone();
                    view["error"] = sent["error"].clone();
                    Ok(view)
                }
                Some("respond") => assist.respond(b["id"].as_str().unwrap_or(""), b["other"].as_str().unwrap_or(""), b["accept"].as_bool().unwrap_or(false)).await,
                Some("cancel") => assist.cancel(b["id"].as_str().unwrap_or("")).await,
                _ => Err("action attendue : request, respond ou cancel (ouvrir l'accès : astroportd tunnel connect)".to_owned()),
            }
        })
    })
}

/// Fonctions à donner à `station_admin::Admin` pour les sections `mesh` (stations, bootstrap) et `captains`.
pub(super) fn mesh_admins(mesh: &Arc<astroportd::mesh_admin::Mesh>, captains: &Arc<astroportd::captains::Captains>) -> (astroportd::ui_backend::AdminFn, astroportd::ui_backend::AdminFn) {
    let mesh = mesh.clone();
    let m: astroportd::ui_backend::AdminFn = Arc::new(move |_section, body| {
        let mesh = mesh.clone();
        Box::pin(async move {
            match body {
                None => Ok(mesh.view().await),
                Some(b) => mesh.act(&b).await,
            }
        })
    });
    let captains = captains.clone();
    let c: astroportd::ui_backend::AdminFn = Arc::new(move |_section, body| {
        let captains = captains.clone();
        Box::pin(async move {
            match body {
                None => captains.view().await,
                Some(b) => captains.act(&b).await,
            }
        })
    });
    (m, c)
}
