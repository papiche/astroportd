//! NIP-59 (gift wrap) : rumeur non signée → scellé (kind 13) → enveloppe cadeau (kind 1059).
//!
//! L'enveloppe est signée par une clé jetable et ne révèle ni l'expéditeur ni le contenu ; seul le destinataire
//! remonte jusqu'à la rumeur, dont l'auteur est vérifié par la signature du scellé.

use astro_nostr_proto::{Event, EventError, UnsignedEvent, kind};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::nip44::{self, Nip44Error};

/// Fenêtre de brouillage des horodatages (2 jours), pour masquer l'heure réelle d'envoi.
const JITTER_SECS: u64 = 2 * 24 * 3600;

/// Événement non signé transporté par l'enveloppe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rumor {
    /// Auteur (clé publique x-only, 64 hex).
    pub pubkey: String,
    /// Horodatage réel de la rumeur.
    pub created_at: u64,
    /// Kind (14 : message, 15 : fichier…).
    pub kind: u32,
    /// Tags.
    pub tags: Vec<Vec<String>>,
    /// Contenu.
    pub content: String,
}

impl Rumor {
    /// Identifiant NIP-01 de la rumeur.
    pub fn id(&self) -> Result<String, EventError> {
        let e = Event { id: String::new(), pubkey: self.pubkey.clone(), created_at: self.created_at, kind: self.kind, tags: self.tags.clone(), content: self.content.clone(), sig: String::new() };
        Ok(hex::encode(e.compute_id()?))
    }

    fn to_json(&self) -> Result<String, EventError> {
        Ok(json!({ "id": self.id()?, "pubkey": self.pubkey, "created_at": self.created_at, "kind": self.kind, "tags": self.tags, "content": self.content }).to_string())
    }

    /// Valeur du premier tag `name`.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
    }
}

/// Erreurs NIP-59.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Nip59Error {
    /// Chiffrement ou déchiffrement NIP-44.
    #[error("NIP-44 : {0}")]
    Nip44(#[from] Nip44Error),
    /// Événement invalide (signature, identifiant).
    #[error("événement invalide : {0}")]
    Event(#[from] EventError),
    /// Structure incorrecte (kind, tags, JSON).
    #[error("enveloppe incorrecte : {0}")]
    Malformed(&'static str),
    /// L'auteur de la rumeur n'est pas celui qui a signé le scellé : usurpation.
    #[error("auteur de la rumeur différent du signataire du scellé")]
    Impersonation,
    /// Aléa système indisponible.
    #[error("aléa indisponible")]
    Rng,
}

/// Rumeur ouverte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unwrapped {
    /// Rumeur transportée.
    pub rumor: Rumor,
    /// Expéditeur authentifié (signataire du scellé).
    pub sender: String,
}

fn jitter(now: u64) -> Result<u64, Nip59Error> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|_| Nip59Error::Rng)?;
    Ok(now.saturating_sub(u64::from_le_bytes(b) % JITTER_SECS))
}

fn ephemeral_key() -> Result<Zeroizing<[u8; 32]>, Nip59Error> {
    loop {
        let mut k = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *k).map_err(|_| Nip59Error::Rng)?;
        if astro_nostr_proto::pubkey_of(&k).is_ok() {
            return Ok(k);
        }
    }
}

/// Emballe `rumor` (dont `pubkey` doit être celle de `sender_secret`) pour `recipient_hex`.
pub fn gift_wrap(sender_secret: &[u8; 32], recipient_hex: &str, rumor: &Rumor, now: u64) -> Result<Event, Nip59Error> {
    if astro_nostr_proto::pubkey_of(sender_secret)? != rumor.pubkey {
        return Err(Nip59Error::Impersonation);
    }
    let seal_content = nip44::encrypt(&nip44::conversation_key(sender_secret, recipient_hex)?, &rumor.to_json()?)?;
    let seal = UnsignedEvent { created_at: jitter(now)?, kind: kind::SEAL, tags: vec![], content: seal_content }.sign(sender_secret)?;
    let ephemeral = ephemeral_key()?;
    let wrap_content = nip44::encrypt(&nip44::conversation_key(&ephemeral, recipient_hex)?, &serde_json::to_string(&seal).map_err(|_| Nip59Error::Malformed("scellé non sérialisable"))?)?;
    Ok(UnsignedEvent { created_at: jitter(now)?, kind: kind::GIFT_WRAP, tags: vec![vec!["p".into(), recipient_hex.to_owned()]], content: wrap_content }.sign(&ephemeral)?)
}

/// Ouvre une enveloppe adressée à `recipient_secret`.
///
/// Vérifie la signature de l'enveloppe et celle du scellé, et que l'auteur de la rumeur est bien le signataire du scellé.
pub fn unwrap(recipient_secret: &[u8; 32], wrap: &Event) -> Result<Unwrapped, Nip59Error> {
    if wrap.kind != kind::GIFT_WRAP {
        return Err(Nip59Error::Malformed("ce n'est pas un gift wrap"));
    }
    wrap.verify()?;
    let seal_json = nip44::decrypt(&nip44::conversation_key(recipient_secret, &wrap.pubkey)?, &wrap.content)?;
    let seal: Event = serde_json::from_str(&seal_json).map_err(|_| Nip59Error::Malformed("scellé illisible"))?;
    if seal.kind != kind::SEAL {
        return Err(Nip59Error::Malformed("le scellé n'est pas un kind 13"));
    }
    if !seal.tags.is_empty() {
        return Err(Nip59Error::Malformed("un scellé ne porte pas de tags"));
    }
    seal.verify()?;
    let rumor_json = nip44::decrypt(&nip44::conversation_key(recipient_secret, &seal.pubkey)?, &seal.content)?;
    let v: Value = serde_json::from_str(&rumor_json).map_err(|_| Nip59Error::Malformed("rumeur illisible"))?;
    let field = |k: &'static str| v.get(k).ok_or(Nip59Error::Malformed("champ de rumeur manquant"));
    let rumor = Rumor {
        pubkey: field("pubkey")?.as_str().ok_or(Nip59Error::Malformed("pubkey"))?.to_owned(),
        created_at: field("created_at")?.as_u64().ok_or(Nip59Error::Malformed("created_at"))?,
        kind: u32::try_from(field("kind")?.as_u64().ok_or(Nip59Error::Malformed("kind"))?).map_err(|_| Nip59Error::Malformed("kind"))?,
        tags: serde_json::from_value(field("tags")?.clone()).map_err(|_| Nip59Error::Malformed("tags"))?,
        content: field("content")?.as_str().ok_or(Nip59Error::Malformed("content"))?.to_owned(),
    };
    if rumor.pubkey != seal.pubkey {
        return Err(Nip59Error::Impersonation);
    }
    if v.get("id").and_then(Value::as_str) != Some(&rumor.id()?) {
        return Err(Nip59Error::Malformed("identifiant de rumeur incorrect"));
    }
    Ok(Unwrapped { sender: seal.pubkey, rumor })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn rumor(secret: &[u8; 32]) -> Rumor {
        Rumor { pubkey: astro_nostr_proto::pubkey_of(secret).unwrap(), created_at: NOW, kind: kind::FILE, tags: vec![vec!["cid".into(), "bafkrei…".into()]], content: "ipfs://x".into() }
    }

    #[test]
    fn round_trip_authenticates_the_sender_and_hides_everything_else() {
        let (alice, bob) = ([1u8; 32], [2u8; 32]);
        let bob_pk = astro_nostr_proto::pubkey_of(&bob).unwrap();
        let wrap = gift_wrap(&alice, &bob_pk, &rumor(&alice), NOW).unwrap();
        assert_eq!(wrap.kind, 1059);
        wrap.verify().unwrap();
        assert_eq!(wrap.tags, vec![vec!["p".to_owned(), bob_pk]]);
        assert_ne!(wrap.pubkey, astro_nostr_proto::pubkey_of(&alice).unwrap(), "clé jetable : l'expéditeur n'apparaît pas");
        assert!(!wrap.content.contains("ipfs") && !wrap.content.contains("cid"));
        assert!(wrap.created_at <= NOW && NOW - wrap.created_at < JITTER_SECS, "horodatage brouillé vers le passé");
        let open = unwrap(&bob, &wrap).unwrap();
        assert_eq!(open.sender, astro_nostr_proto::pubkey_of(&alice).unwrap());
        assert_eq!(open.rumor, rumor(&alice));
        assert_eq!(open.rumor.tag_value("cid"), Some("bafkrei…"));
        // Deux enveloppes du même message diffèrent (clé jetable, nonces, horodatages).
        let again = gift_wrap(&alice, &astro_nostr_proto::pubkey_of(&bob).unwrap(), &rumor(&alice), NOW).unwrap();
        assert_ne!((&wrap.id, &wrap.pubkey), (&again.id, &again.pubkey));
    }

    #[test]
    fn only_the_recipient_can_open_it() {
        let (alice, bob, carol) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let wrap = gift_wrap(&alice, &astro_nostr_proto::pubkey_of(&bob).unwrap(), &rumor(&alice), NOW).unwrap();
        assert!(unwrap(&carol, &wrap).is_err());
        assert!(unwrap(&alice, &wrap).is_err(), "même l'expéditeur ne peut pas relire l'enveloppe");
    }

    #[test]
    fn tampering_is_detected() {
        let (alice, bob) = ([1u8; 32], [2u8; 32]);
        let bob_pk = astro_nostr_proto::pubkey_of(&bob).unwrap();
        let wrap = gift_wrap(&alice, &bob_pk, &rumor(&alice), NOW).unwrap();
        let mut forged = wrap.clone();
        forged.content.push('A');
        assert!(unwrap(&bob, &forged).is_err());
        let mut wrong_kind = wrap.clone();
        wrong_kind.kind = 1;
        assert_eq!(unwrap(&bob, &wrong_kind), Err(Nip59Error::Malformed("ce n'est pas un gift wrap")));
    }

    #[test]
    fn impersonation_is_refused_on_both_sides() {
        let (alice, mallory, bob) = ([1u8; 32], [9u8; 32], [2u8; 32]);
        let bob_pk = astro_nostr_proto::pubkey_of(&bob).unwrap();
        // Côté émission : on ne scelle pas une rumeur d'un autre auteur.
        assert_eq!(gift_wrap(&mallory, &bob_pk, &rumor(&alice), NOW), Err(Nip59Error::Impersonation));
        // Côté réception : Mallory scelle une rumeur qui prétend venir d'Alice.
        let fake = rumor(&alice);
        let seal_content = nip44::encrypt(&nip44::conversation_key(&mallory, &bob_pk).unwrap(), &fake.to_json().unwrap()).unwrap();
        let seal = UnsignedEvent { created_at: NOW, kind: 13, tags: vec![], content: seal_content }.sign(&mallory).unwrap();
        let eph = [7u8; 32];
        let wrap_content = nip44::encrypt(&nip44::conversation_key(&eph, &bob_pk).unwrap(), &serde_json::to_string(&seal).unwrap()).unwrap();
        let wrap = UnsignedEvent { created_at: NOW, kind: 1059, tags: vec![vec!["p".into(), bob_pk]], content: wrap_content }.sign(&eph).unwrap();
        assert_eq!(unwrap(&bob, &wrap), Err(Nip59Error::Impersonation));
    }
}
