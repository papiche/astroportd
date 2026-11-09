//! Les événements signés par le plugin `media` (Python, sans dépendance) sont acceptés comme n'importe quel événement Nostr :
//! signature Schnorr et identifiant NIP-01 vérifiés par astroportd, tags de l'événement vidéo
//! conformes à ce que la station écrivait avant le plugin. Fixture produite par `media/bin/media_add.py`.

#![allow(clippy::unwrap_used)]

use astro_nostr_proto::Event;
use serde_json::Value;

fn video() -> Event {
    let doc: Value = serde_json::from_str(include_str!("fixtures/plugin_media_events.json")).unwrap();
    serde_json::from_value(doc["video"].clone()).unwrap()
}

#[test]
fn events_signed_by_the_plugin_verify_in_astroportd() {
    let video = video();
    video.verify().unwrap();
    assert_eq!(video.kind, 21);
    let mut tampered = video.clone();
    tampered.content.push('x');
    assert!(tampered.verify().is_err());
}

#[test]
fn the_video_event_carries_the_tags_the_station_side_used_to_write() {
    let video = video();
    let tag = |k: &str| video.tags.iter().filter(|t| t.first().map(String::as_str) == Some(k)).map(|t| t[1..].to_vec()).collect::<Vec<_>>();
    assert_eq!(tag("title")[0], ["Test S01E02 — Pilote"]);
    assert_eq!(tag("url")[0], ["/ipfs/bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"]);
    assert!(tag("t").iter().any(|t| t == &["private-copy"]) && !tag("-").is_empty() && tag("i")[0] == ["tmdb:99"]);
    assert_eq!((tag("series_name")[0].clone(), tag("season_number")[0].clone(), tag("episode_number")[0].clone()), (vec!["Test".to_owned()], vec!["1".to_owned()], vec!["2".to_owned()]));
    // Les étiquettes que lisaient les clients quand la station écrivait elle-même ces événements : aucune n'est perdue.
    let mut keys: Vec<String> = video.tags.iter().map(|t| if t[0] == "t" || t[0] == "l" { format!("{}:{}", t[0], t[1]) } else { t[0].clone() }).collect();
    keys.sort();
    keys.dedup();
    let expected = ["-", "L", "dim", "duration", "encrypted", "episode_name", "episode_number", "i", "imeta", "l:Science-Fiction", "l:private-copy-cpi-l122-5", "m", "published_at", "season_number", "series_name", "size", "t:private-copy", "t:regular", "t:science-fiction", "t:series", "t:tv-show", "title", "url", "x", "year"];
    assert_eq!(keys, expected);
}
