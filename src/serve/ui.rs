//! Raccordement de l'interface web aux services de la station : configuration coopérative, envoi aux proches, sauvegarde privée.

use super::*;

/// Ajoute à `ui` les fonctions qui s'appuient sur la base, le publieur d'événements, le partage et la sauvegarde.
pub(super) fn wire<B: astro_dav::BlobStore>(mut ui: StationUi<B>, db: &Arc<Db>, event_publisher: &Arc<EventPublisher>, uplanet_name: &str, sharing: &Arc<astroportd::sharing::Sharing<astroportd::mesh_outbox::MeshOutbox>>, backup: &Arc<Backup>) -> StationUi<B> {
    let (for_coop_get, for_coop_set) = (db.clone(), (db.clone(), event_publisher.clone()));
    let coop_name = uplanet_name.to_owned();
    let coop_name2 = uplanet_name.to_owned();
    let coop_get: astroportd::ui_backend::CoopGetFn = Arc::new(move || {
        let (db, name) = (for_coop_get.clone(), coop_name.clone());
        Box::pin(async move {
            let (_, hex) = astroportd::coop_config::coop_keys(&name)?;
            Ok(astroportd::coop_config::view(astroportd::coop_config::load(&db, &hex).await.as_ref(), &name, &hex))
        })
    });
    let coop_set: astroportd::ui_backend::CoopSetFn = Arc::new(move |patch| {
        let ((db, publisher), name) = (for_coop_set.clone(), coop_name2.clone());
        Box::pin(async move { astroportd::coop_config::save(&db, &publisher, &name, &patch).await })
    });
    ui = ui.with_coop(coop_get, coop_set);
    let (for_friends, for_send) = (sharing.clone(), sharing.clone());
    let friends: astroportd::ui_backend::FriendsFn = Arc::new(move |owner: String| {
        let sharing = for_friends.clone();
        Box::pin(async move { sharing.friends(&owner).await.map(|f| serde_json::json!({ "friends": f.iter().map(|x| serde_json::json!({ "hex": x.hex, "label": x.label, "hosted": x.hosted })).collect::<Vec<_>>() })) })
    });
    let send: astroportd::ui_backend::SendFn = Arc::new(move |owner: String, paths: Vec<String>, to: Vec<String>| {
        let sharing = for_send.clone();
        Box::pin(async move { sharing.send(&owner, &paths, &to).await })
    });
    ui = ui.with_sharing(friends, send);
    let (b1, b2, b3) = (backup.clone(), backup.clone(), backup.clone());
    ui = ui.with_backup(astroportd::ui_backend::BackupFns {
        status: Arc::new(move |owner: String| {
            let b = b1.clone();
            Box::pin(async move { b.status(&owner).await })
        }),
        set: Arc::new(move |owner: String, enabled: bool, host: Option<String>| {
            let b = b2.clone();
            Box::pin(async move { b.set(&owner, enabled, host.as_deref()).await })
        }),
        respond: Arc::new(move |host: String, owner: String, accept: bool, quota: u64, max: u64| {
            let b = b3.clone();
            Box::pin(async move { b.respond(&host, &owner, accept, quota, max).await })
        }),
    });

    ui
}
