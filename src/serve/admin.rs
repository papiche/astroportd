//! Actions de l'écran « Assistance » du Capitaine : demande, réponse, accès local à chaud et ticket de console.

use super::*;

/// Fonction à donner à `station_admin::Admin` pour la section `assist`.
pub(super) fn assist_admin(assist: &Arc<astroportd::assist::Assist<astroportd::mesh_outbox::MeshOutbox>>, forwards: &Arc<astroportd::forwards::Forwards>, consoles: &Arc<astroportd::console::Consoles>) -> astroportd::ui_backend::AdminFn {
    let (assist, forwards, consoles) = (assist.clone(), forwards.clone(), consoles.clone());
    Arc::new(move |_section, body| {
        let (assist, forwards, consoles) = (assist.clone(), forwards.clone(), consoles.clone());
        Box::pin(async move {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            let with_local = |mut view: serde_json::Value| {
                let open = forwards.list(now);
                if let Some(rows) = view["incoming"].as_array_mut() {
                    for r in rows {
                        r["local"] = open.iter().find(|o| r["id"].as_str() == Some(o.0.as_str())).map_or(serde_json::Value::Null, |o| serde_json::json!({ "addr": o.1.to_string(), "until": o.2 }));
                    }
                }
                view
            };
            let Some(b) = body else { return assist.view().await.map(with_local) };
            match b["action"].as_str() {
                Some("connect") => {
                    let id = b["id"].as_str().unwrap_or("");
                    let (station, service, hours) = assist.accepted_incoming(id).await.ok_or("demande introuvable ou pas encore acceptée")?;
                    forwards.open(id, &station, &service, now + hours * 3600, now).await?;
                    assist.view().await.map(with_local)
                }
                Some("console") => {
                    let id = b["id"].as_str().unwrap_or("");
                    let (station, _, _) = assist.accepted_incoming(id).await.ok_or("demande introuvable ou pas encore acceptée")?;
                    let (_, addr, until) = forwards.list(now).into_iter().find(|o| o.0 == id).ok_or("ouvrez d'abord l'accès local (Se connecter)")?;
                    let ticket = consoles.ticket(addr, b["user"].as_str().unwrap_or(""), &station, until)?;
                    Ok(serde_json::json!({ "ticket": ticket }))
                }
                Some("disconnect") => {
                    forwards.close(b["id"].as_str().unwrap_or(""));
                    assist.view().await.map(with_local)
                }
                Some("request") => {
                    let sent = assist.request(b["service"].as_str().unwrap_or(""), b["hours"].as_i64().unwrap_or(0), b["note"].as_str().unwrap_or(""), b["to"].as_str().filter(|t| !t.is_empty()), b["exclusive"].as_bool().unwrap_or(false)).await?;
                    let mut view = assist.view().await?;
                    view["sent"] = sent["sent"].clone();
                    view["error"] = sent["error"].clone();
                    Ok(view)
                }
                Some("respond") => assist.respond(b["id"].as_str().unwrap_or(""), b["other"].as_str().unwrap_or(""), b["accept"].as_bool().unwrap_or(false)).await,
                Some("cancel") => assist.cancel(b["id"].as_str().unwrap_or("")).await,
                _ => Err("action attendue : request, respond, cancel, connect ou disconnect".to_owned()),
            }
        })
    })
}
