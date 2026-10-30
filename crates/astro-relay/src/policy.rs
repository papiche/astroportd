//! Pare-feu social à deux niveaux (Tiered Gated Relay).
//!
//! **Lecture** : les kinds publics sont ouverts à tous. Tout autre kind exige une authentification NIP-42
//! par une clé du cercle social ; les kinds adressés (4, 1059) ne sont en outre lisibles que par leur auteur
//! ou leur destinataire (`p`). Un kind inconnu est donc fermé par défaut.
//!
//! **Écriture** : seuls les membres du cercle publient. Exception d'amorçage NIP-59 : un gift wrap (1059)
//! dont un tag `p` vise un MULTIPASS hébergé ici est accepté de n'importe qui, sous limitation de débit par IP.

use std::collections::{BTreeMap, HashSet};

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
    /// Kind ouvert à tous par la matrice du Capitaine : accepté sous limitation de débit par IP.
    AcceptLimited,
    /// Refusé, avec le message `OK false` préfixé.
    Reject(&'static str),
}

/// Qui peut lire un kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRule {
    /// Tout le monde, sans authentification.
    Public,
    /// Authentifié du cercle social ; en plus, seulement l'auteur et les destinataires `p`.
    Addressed,
    /// L'auteur et ceux qu'il suit (N¹ de l'auteur).
    Friends,
    /// Authentifié (NIP-42) et membre du cercle social.
    Circle,
    /// Personne : le kind n'est jamais servi.
    Deny,
}

/// Qui peut publier un kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteRule {
    /// Membres du cercle social.
    Circle,
    /// N'importe qui, sous limitation de débit par IP.
    Open,
    /// Personne : le kind est refusé.
    Deny,
}

/// Règle d'un kind : lecture, écriture, projection en base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindRule {
    /// Lecture.
    pub read: ReadRule,
    /// Écriture.
    pub write: WriteRule,
    /// Projection dans les tables dérivées (identités, ledger…) ; l'événement reste stocké et servi.
    pub project: bool,
}

impl ReadRule {
    /// Nom dans `station.conf` et l'API.
    pub fn name(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Addressed => "addressed",
            Self::Friends => "friends",
            Self::Circle => "circle",
            Self::Deny => "deny",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "public" => Self::Public,
            "addressed" => Self::Addressed,
            "friends" => Self::Friends,
            "circle" => Self::Circle,
            "deny" => Self::Deny,
            _ => return None,
        })
    }
}

impl WriteRule {
    /// Nom dans `station.conf` et l'API.
    pub fn name(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Open => "open",
            Self::Deny => "deny",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "circle" => Self::Circle,
            "open" => Self::Open,
            "deny" => Self::Deny,
            _ => return None,
        })
    }
}

impl KindRule {
    /// `read=circle write=deny project=off` ; les clés omises gardent la valeur de `base`.
    pub fn parse(spec: &str, base: Self) -> Result<Self, String> {
        let mut rule = base;
        for part in spec.split_whitespace() {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("« {part} » : attendu clé=valeur"))?;
            match k {
                "read" => rule.read = ReadRule::parse(v).ok_or_else(|| format!("read={v} : public, addressed, friends, circle ou deny"))?,
                "write" => rule.write = WriteRule::parse(v).ok_or_else(|| format!("write={v} : circle, open ou deny"))?,
                "project" => rule.project = match v {
                    "on" | "true" => true,
                    "off" | "false" => false,
                    _ => return Err(format!("project={v} : on ou off")),
                },
                _ => return Err(format!("clé inconnue « {k} » (read, write, project)")),
            }
        }
        Ok(rule)
    }

    /// Forme texte, relisible par [`KindRule::parse`].
    pub fn spec(self) -> String {
        format!("read={} write={} project={}", self.read.name(), self.write.name(), if self.project { "on" } else { "off" })
    }
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
    /// Matrice du Capitaine : une règle par kind, qui l'emporte sur les ensembles ci-dessus.
    pub rules: BTreeMap<u32, KindRule>,
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
                kind::STATION_BUILD,
                kind::VISIO_PRESENCE,
                kind::VISIO_RING,
            ]
            .into_iter()
            .collect(),
            addressed_kinds: [kind::DM_LEGACY, kind::GIFT_WRAP].into_iter().collect(),
            friend_kinds: [kind::VIDEO, kind::VIDEO_SHORT].into_iter().collect(),
            rules: BTreeMap::new(),
        }
    }
}

impl Policy {
    /// Règle en vigueur pour un kind : celle du Capitaine, sinon celle qui découle des ensembles par défaut.
    pub fn rule(&self, kind: u32) -> KindRule {
        self.rules.get(&kind).copied().unwrap_or_else(|| self.default_rule(kind))
    }

    /// Règle sans la matrice du Capitaine.
    pub fn default_rule(&self, kind: u32) -> KindRule {
        let read = if self.public_kinds.contains(&kind) {
            ReadRule::Public
        } else if self.friend_kinds.contains(&kind) {
            ReadRule::Friends
        } else if self.addressed_kinds.contains(&kind) {
            ReadRule::Addressed
        } else {
            ReadRule::Circle
        };
        KindRule { read, write: WriteRule::Circle, project: true }
    }

    /// Les événements de ce kind sont-ils projetés dans les tables dérivées ?
    pub fn projects(&self, kind: u32) -> bool {
        self.rule(kind).project
    }

    /// Ajoute les règles d'une matrice ; refuse les kinds dont la règle est fixée par le protocole.
    pub fn with_rules(mut self, rules: impl IntoIterator<Item = (u32, KindRule)>) -> Result<Self, String> {
        for (k, r) in rules {
            if k == kind::AUTH {
                return Err("le kind 22242 (authentification) n'est jamais stocké : pas de règle possible".into());
            }
            if k == kind::GIFT_WRAP && r.write != WriteRule::Circle {
                return Err("le kind 1059 garde la règle d'amorçage (gift wrap vers un compte hébergé)".into());
            }
            if k == kind::GIFT_WRAP && !matches!(r.read, ReadRule::Addressed | ReadRule::Deny) {
                return Err("le kind 1059 reste lisible par son seul destinataire (addressed) ou fermé (deny)".into());
            }
            self.rules.insert(k, r);
        }
        Ok(self)
    }

    /// Examine les kinds explicitement demandés par un filtre.
    ///
    /// Un filtre sans `kinds` reste ouvert : seuls les événements publics (ou autorisés) sortiront.
    pub fn filter_access(&self, f: &Filter, auth: &AuthState, circle: &dyn CircleProvider) -> ReadAccess {
        let Some(kinds) = &f.kinds else { return ReadAccess::Open };
        // Un kind fermé (deny) ne ralentit personne : il ne sortira simplement aucun événement.
        if kinds.iter().all(|k| matches!(self.rule(*k).read, ReadRule::Public | ReadRule::Deny)) {
            return ReadAccess::Open;
        }
        if auth.is_empty() {
            ReadAccess::NeedsAuth
        } else if !auth.any_in_circle(circle) && !kinds.iter().any(|k| self.rule(*k).read == ReadRule::Friends) {
            // Les vidéos se jugent événement par événement (N¹ de l'auteur), même pour un lecteur hors du cercle
            // de cette station : un ami d'un auteur distant n'est pas forcément connu ici.
            ReadAccess::Restricted
        } else {
            ReadAccess::Open
        }
    }

    /// L'événement peut-il être livré à cette connexion ?
    pub fn can_read(&self, e: &Event, auth: &AuthState, circle: &dyn CircleProvider) -> bool {
        match self.rule(e.kind).read {
            ReadRule::Deny => false,
            ReadRule::Public => true,
            // N¹ de l'auteur : lecteur authentifié (NIP-42) qui est l'auteur ou figure dans son dernier kind 3.
            ReadRule::Friends => auth.contains(&e.pubkey) || auth.pubkeys.iter().any(|k| circle.is_friend_of(&e.pubkey, k)),
            ReadRule::Addressed => auth.any_in_circle(circle) && (auth.contains(&e.pubkey) || p_tags(e).any(|p| auth.contains(p))),
            ReadRule::Circle => auth.any_in_circle(circle),
        }
    }

    /// Décide du sort d'un `EVENT` entrant (signature déjà vérifiée).
    pub fn write_decision(&self, e: &Event, circle: &dyn CircleProvider) -> WriteDecision {
        if e.kind == kind::AUTH {
            return WriteDecision::Reject("invalid: use the AUTH message for kind 22242");
        }
        let write = self.rule(e.kind).write;
        if write == WriteRule::Deny {
            return WriteDecision::Reject("blocked: this kind is not accepted on this station");
        }
        if e.kind == kind::GIFT_WRAP {
            return if p_tags(e).any(|p| circle.is_hosted(p)) {
                WriteDecision::AcceptGiftWrap
            } else {
                WriteDecision::Reject("restricted: recipient is not hosted on this station")
            };
        }
        if write == WriteRule::Open {
            return WriteDecision::AcceptLimited;
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

    #[test]
    fn the_default_matrix_reproduces_the_historic_behaviour() {
        let p = Policy::default();
        assert_eq!(p.rule(1), KindRule { read: ReadRule::Public, write: WriteRule::Circle, project: true });
        assert_eq!(p.rule(1059).read, ReadRule::Addressed);
        assert_eq!(p.rule(21).read, ReadRule::Friends);
        assert_eq!(p.rule(7).read, ReadRule::Circle);
    }

    #[test]
    fn a_rule_overrides_the_defaults_per_kind() {
        let c = circle();
        let p = Policy::default().with_rules([(7, KindRule::parse("write=deny", Policy::default().rule(7)).unwrap()), (1, KindRule::parse("read=deny", Policy::default().rule(1)).unwrap()), (30_078, KindRule::parse("write=open read=public project=off", Policy::default().rule(30_078)).unwrap())]).unwrap();
        assert!(matches!(p.write_decision(&ev(2, 7, vec![]), &c), WriteDecision::Reject(m) if m.starts_with("blocked:")), "un membre ne publie plus de kind 7");
        assert!(!p.can_read(&ev(2, 1, vec![]), &auth(&[2]), &c), "kind 1 fermé à la lecture, même pour un membre");
        assert_eq!(p.filter_access(&filter(Some(vec![1])), &AuthState::default(), &c), ReadAccess::Open, "un kind fermé ne demande pas d'authentification");
        assert_eq!(p.write_decision(&ev(9, 30_078, vec![]), &c), WriteDecision::AcceptLimited, "un inconnu publie un kind ouvert (débit limité)");
        assert!(p.can_read(&ev(9, 30_078, vec![]), &AuthState::default(), &c), "kind 30078 rendu public");
        assert!(!p.projects(30_078) && p.projects(1));
    }

    #[test]
    fn protocol_fixed_kinds_refuse_incoherent_rules() {
        let base = Policy::default().rule(1059);
        assert!(Policy::default().with_rules([(22_242, base)]).is_err());
        assert!(Policy::default().with_rules([(1059, KindRule { write: WriteRule::Open, ..base })]).is_err());
        assert!(Policy::default().with_rules([(1059, KindRule { read: ReadRule::Public, ..base })]).is_err());
        assert!(Policy::default().with_rules([(1059, KindRule { read: ReadRule::Deny, ..base })]).is_ok());
    }

    #[test]
    fn rule_text_round_trips_and_rejects_typos() {
        let base = Policy::default().rule(1);
        let r = KindRule::parse("read=friends write=open project=off", base).unwrap();
        assert_eq!(KindRule::parse(&r.spec(), base).unwrap(), r);
        for bad in ["read=tous", "write", "project=maybe", "lire=public"] {
            assert!(KindRule::parse(bad, base).is_err(), "{bad}");
        }
    }
}
