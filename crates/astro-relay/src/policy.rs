//! Pare-feu social à deux niveaux (Tiered Gated Relay).
//!
//! **Lecture** : les kinds publics sont ouverts à tous. Tout autre kind exige une authentification NIP-42
//! par une clé du cercle social ; les kinds adressés (4, 1059) ne sont en outre lisibles que par leur auteur
//! ou leur destinataire (`p`). Un kind inconnu est donc fermé par défaut.
//!
//! **Écriture** : seuls les membres du cercle publient. Exception d'amorçage NIP-59 : un gift wrap (1059)
//! dont un tag `p` vise un MULTIPASS hébergé ici est accepté de n'importe qui, sous limitation de débit par IP.

use std::collections::HashSet;

use astro_nostr_proto::{Event, Filter, kind};

use crate::circle::CircleProvider;

/// Clés authentifiées (NIP-42) d'une connexion.
#[derive(Debug, Default, Clone)]
pub struct AuthState {
    pubkeys: HashSet<String>,
}

impl AuthState {
    /// Enregistre une clé authentifiée.
    pub fn add(&mut self, pubkey: String) {
        self.pubkeys.insert(pubkey);
    }

    /// Aucune authentification.
    pub fn is_empty(&self) -> bool {
        self.pubkeys.is_empty()
    }

    /// La clé est authentifiée sur cette connexion.
    pub fn contains(&self, pubkey: &str) -> bool {
        self.pubkeys.contains(pubkey)
    }

    fn any_in_circle(&self, circle: &dyn CircleProvider) -> bool {
        self.pubkeys.iter().any(|k| circle.in_circle(k))
    }
}

/// Résultat de l'examen d'une souscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAccess {
    /// Souscription servie (le filtrage par événement s'applique ensuite).
    Open,
    /// Kinds privés demandés sans authentification.
    NeedsAuth,
    /// Authentifié, mais hors du cercle social.
    Restricted,
}

/// Décision d'écriture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDecision {
    /// Accepté.
    Accept,
    /// Gift wrap d'amorçage : accepté sous limitation de débit par IP.
    AcceptGiftWrap,
    /// Refusé, avec le message `OK false` préfixé.
    Reject(&'static str),
}

/// Politique d'accès du relai.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Kinds lisibles sans authentification.
    pub public_kinds: HashSet<u32>,
    /// Kinds privés limités à leur auteur et à leurs destinataires `p`.
    pub addressed_kinds: HashSet<u32>,
    /// Kinds réservés au cercle de confiance **N¹** de leur auteur (vidéos NIP-71 : 21 et 22) : l'auteur et ceux
    /// qu'il suit, jamais le reste du cercle N².
    pub friend_kinds: HashSet<u32>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            public_kinds: [
                kind::METADATA,
                kind::TEXT_NOTE,
                kind::CONTACTS,
                10_002, // listes de relais
                kind::LONG_FORM,
                kind::DID,
                kind::ECONOMIC_HEALTH,
                kind::VISIO_PRESENCE,
                kind::VISIO_RING,
            ]
            .into_iter()
            .collect(),
            addressed_kinds: [kind::DM_LEGACY, kind::GIFT_WRAP].into_iter().collect(),
            friend_kinds: [kind::VIDEO, kind::VIDEO_SHORT].into_iter().collect(),
        }
    }
}

impl Policy {
    /// Examine les kinds explicitement demandés par un filtre.
    ///
    /// Un filtre sans `kinds` reste ouvert : seuls les événements publics (ou autorisés) sortiront.
    pub fn filter_access(&self, f: &Filter, auth: &AuthState, circle: &dyn CircleProvider) -> ReadAccess {
        let Some(kinds) = &f.kinds else { return ReadAccess::Open };
        if kinds.iter().all(|k| self.public_kinds.contains(k)) {
            return ReadAccess::Open;
        }
        if auth.is_empty() {
            ReadAccess::NeedsAuth
        } else if !auth.any_in_circle(circle) && !kinds.iter().any(|k| self.friend_kinds.contains(k)) {
            // Les vidéos se jugent événement par événement (N¹ de l'auteur), même pour un lecteur hors du cercle
            // de cette station : un ami d'un auteur distant n'est pas forcément connu ici.
            ReadAccess::Restricted
        } else {
            ReadAccess::Open
        }
    }

    /// L'événement peut-il être livré à cette connexion ?
    pub fn can_read(&self, e: &Event, auth: &AuthState, circle: &dyn CircleProvider) -> bool {
        if self.public_kinds.contains(&e.kind) {
            return true;
        }
        if self.friend_kinds.contains(&e.kind) {
            // N¹ de l'auteur : lecteur authentifié (NIP-42) qui est l'auteur ou figure dans son dernier kind 3.
            return auth.contains(&e.pubkey) || auth.pubkeys.iter().any(|k| circle.is_friend_of(&e.pubkey, k));
        }
        if !auth.any_in_circle(circle) {
            return false;
        }
        if self.addressed_kinds.contains(&e.kind) {
            return auth.contains(&e.pubkey) || p_tags(e).any(|p| auth.contains(p));
        }
        true
    }

    /// Décide du sort d'un `EVENT` entrant (signature déjà vérifiée).
    pub fn write_decision(&self, e: &Event, circle: &dyn CircleProvider) -> WriteDecision {
        if e.kind == kind::AUTH {
            return WriteDecision::Reject("invalid: use the AUTH message for kind 22242");
        }
        if e.kind == kind::GIFT_WRAP {
            return if p_tags(e).any(|p| circle.is_hosted(p)) {
                WriteDecision::AcceptGiftWrap
            } else {
                WriteDecision::Reject("restricted: recipient is not hosted on this station")
            };
        }
        if circle.in_circle(&e.pubkey) {
            WriteDecision::Accept
        } else {
            WriteDecision::Reject("restricted: author is outside the social circle")
        }
    }
}

fn p_tags(e: &Event) -> impl Iterator<Item = &str> {
    e.tags.iter().filter(|t| t.first().map(String::as_str) == Some("p")).filter_map(|t| t.get(1)).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::UnsignedEvent;

    use super::*;
    use crate::circle::StaticCircle;

    fn key(n: u8) -> String {
        astro_nostr_proto::pubkey_of(&[n; 32]).unwrap()
    }

    fn circle() -> StaticCircle {
        StaticCircle::new([key(1)], [key(2), key(3)]) // 1 = hébergé, 2 = N1, 3 = N2
    }

    fn auth(keys: &[u8]) -> AuthState {
        let mut a = AuthState::default();
        keys.iter().for_each(|k| a.add(key(*k)));
        a
    }

    fn ev(secret: u8, kind: u32, tags: Vec<Vec<String>>) -> Event {
        UnsignedEvent { created_at: 1_800_000_000, kind, tags, content: String::new() }.sign(&[secret; 32]).unwrap()
    }

    fn filter(kinds: Option<Vec<u32>>) -> Filter {
        Filter { kinds, ..Filter::default() }
    }

    #[test]
    fn public_kinds_are_open_to_anonymous_readers() {
        let p = Policy::default();
        let c = circle();
        for k in [0, 1, 3, 30_023, 30_800, 30_850] {
            assert_eq!(p.filter_access(&filter(Some(vec![k])), &AuthState::default(), &c), ReadAccess::Open, "kind {k}");
        }
        assert_eq!(p.filter_access(&filter(None), &AuthState::default(), &c), ReadAccess::Open);
    }

    #[test]
    fn private_kinds_demand_authentication_then_circle_membership() {
        let p = Policy::default();
        let c = circle();
        for k in [4, 1059, 30_078, 7, 30_852] {
            let f = filter(Some(vec![1, k]));
            assert_eq!(p.filter_access(&f, &AuthState::default(), &c), ReadAccess::NeedsAuth, "kind {k}");
            assert_eq!(p.filter_access(&f, &auth(&[9]), &c), ReadAccess::Restricted, "kind {k} étranger authentifié");
            for member in [1, 2, 3] {
                assert_eq!(p.filter_access(&f, &auth(&[member]), &c), ReadAccess::Open, "kind {k} membre {member}");
            }
        }
    }

    #[test]
    fn event_level_reads_follow_the_same_rule() {
        let p = Policy::default();
        let c = circle();
        let note = ev(9, 1, vec![]);
        let app = ev(9, 30_078, vec![]);
        assert!(p.can_read(&note, &AuthState::default(), &c));
        assert!(!p.can_read(&app, &AuthState::default(), &c));
        assert!(!p.can_read(&app, &auth(&[9]), &c));
        assert!(p.can_read(&app, &auth(&[2]), &c));
    }

    #[test]
    fn videos_are_private_to_the_authors_n1() {
        let p = Policy::default();
        let c = StaticCircle::new([key(1)], [key(2), key(3)]).with_friend(key(1), key(2)); // 2 est dans le N1 de 1 ; 3 seulement N2
        for k in [21, 22] {
            let v = ev(1, k, vec![]);
            assert!(p.can_read(&v, &auth(&[1]), &c), "l'auteur relit sa vidéo (kind {k})");
            assert!(p.can_read(&v, &auth(&[2]), &c), "un ami N1 la voit (kind {k})");
            assert!(!p.can_read(&v, &auth(&[3]), &c), "un membre N2 non ami ne la voit pas (kind {k})");
            assert!(!p.can_read(&v, &auth(&[9]), &c), "un inconnu non plus (kind {k})");
            assert!(!p.can_read(&v, &AuthState::default(), &c), "ni un anonyme (kind {k})");
        }
    }

    #[test]
    fn addressed_kinds_are_limited_to_author_and_recipient() {
        let p = Policy::default();
        let c = circle();
        let wrap = ev(9, 1059, vec![vec!["p".into(), key(1)]]);
        assert!(p.can_read(&wrap, &auth(&[1]), &c), "destinataire hébergé");
        assert!(!p.can_read(&wrap, &auth(&[2]), &c), "membre du cercle mais pas destinataire");
        assert!(!p.can_read(&wrap, &AuthState::default(), &c));
        let dm = ev(2, 4, vec![vec!["p".into(), key(3)]]);
        assert!(p.can_read(&dm, &auth(&[2]), &c), "l'auteur relit ses DM");
        assert!(p.can_read(&dm, &auth(&[3]), &c));
        assert!(!p.can_read(&dm, &auth(&[1]), &c));
    }

    #[test]
    fn writes_are_limited_to_the_social_circle() {
        let p = Policy::default();
        let c = circle();
        for member in [1, 2, 3] {
            assert_eq!(p.write_decision(&ev(member, 1, vec![]), &c), WriteDecision::Accept, "membre {member}");
        }
        assert!(matches!(p.write_decision(&ev(9, 1, vec![]), &c), WriteDecision::Reject(m) if m.starts_with("restricted:")));
        assert!(matches!(p.write_decision(&ev(9, 4, vec![vec!["p".into(), key(1)]]), &c), WriteDecision::Reject(_)), "plus de DM legacy d'inconnus");
    }

    #[test]
    fn gift_wrap_bootstrap_exception() {
        let p = Policy::default();
        let c = circle();
        let to_local = ev(9, 1059, vec![vec!["p".into(), key(1)]]);
        assert_eq!(p.write_decision(&to_local, &c), WriteDecision::AcceptGiftWrap);
        let to_remote = ev(9, 1059, vec![vec!["p".into(), key(9)]]);
        assert!(matches!(p.write_decision(&to_remote, &c), WriteDecision::Reject(m) if m.contains("not hosted")));
        let no_p = ev(9, 1059, vec![]);
        assert!(matches!(p.write_decision(&no_p, &c), WriteDecision::Reject(_)));
        // Un N1 n'est pas « hébergé » : le gift wrap qui le vise seul est refusé.
        let to_n1 = ev(9, 1059, vec![vec!["p".into(), key(2)]]);
        assert!(matches!(p.write_decision(&to_n1, &c), WriteDecision::Reject(_)));
    }

    #[test]
    fn kind_22242_cannot_be_published_as_an_event() {
        let p = Policy::default();
        assert!(matches!(p.write_decision(&ev(1, 22_242, vec![]), &circle()), WriteDecision::Reject(m) if m.starts_with("invalid:")));
    }
}
