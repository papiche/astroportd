//! Régie des communs : grille tarifaire d'un nœud et baux temporels sur ses ressources partagées.
//!
//! - **Tarifs** (kind 30850, `d = "tariffs"`) : un tag `["price", <ressource>, <montant Ẑen>, <unité>]` par ressource —
//!   paliers de disque (`disk_10gb`, `disk_128gb`, `disk_256gb`, en Ẑen par semaine) et part de calcul GPU
//!   (`gpu_1_24`, en Ẑen par **semaine**, comme le disque) ;
//! - **Baux** : un virement Ğ1-N² (kind 30852) du bénéficiaire vers le nœud, portant `["t","lease"]`,
//!   `["resource", …]`, `["units", N]` (semaines), `["unit_price", <Ẑen>]` et, facultatif,
//!   `["start", <secondes UNIX>]`, ouvre un bail `[début, fin[`. Le prix unitaire est **figé dans la transaction**
//!   (`unit_price_cents`) : le paiement doit couvrir `prix figé × unités`, et le prix figé ne peut pas être
//!   inférieur au tarif du nœud **en vigueur à la date du virement**, lu dans l'historique immuable
//!   (`tariffs_history`). Un tarif modifié plus tard ne change jamais le statut d'un bail passé, au replay comme
//!   en direct ;
//! - **Accès** : une part `gpu_1_24` est 1/24 du GPU ; un nœud n'en vend pas plus de 24 en même temps. Le flux P2P
//!   vers un service lourd n'est ouvert qu'à qui détient un bail actif pour l'instant présent.
//!
//! Tous les montants sont des centimes entiers. Le module fabrique des requêtes SQL (comme [`crate::store`]) : la
//! décision d'admission dépend de l'état (tarif publié, places restantes) et se prend donc dans la base, ce qui
//! rend le replay indépendant de l'ordre d'arrivée des tarifs.

use std::collections::BTreeMap;

use astro_core::Amount;
use astro_db::Statement;
use rusqlite::types::Value;

/// Nom du `d`-tag de l'événement de tarifs.
pub const TARIFF_D_TAG: &str = "tariffs";
/// Part de GPU vendable en même temps (1/24).
pub const GPU_SHARES: u32 = 24;
/// Durée maximale d'un bail GPU, en semaines (12 semaines).
pub const MAX_GPU_WEEKS: u32 = 12;
/// Quota de calcul GPU d'une part hebdomadaire : 1 heure par jour…
pub const GPU_DAILY_QUOTA_SECS: u64 = 3_600;
/// … soit 7 heures de calcul effectif par semaine.
pub const GPU_WEEKLY_QUOTA_SECS: u64 = 7 * GPU_DAILY_QUOTA_SECS;
/// Durée maximale d'un bail de disque, en semaines (1 an).
pub const MAX_DISK_WEEKS: u32 = 52;
/// Un bail ne peut pas démarrer plus de 30 jours après le paiement.
pub const MAX_ADVANCE_SECS: u64 = 30 * 86_400;

const GIB: u64 = 1 << 30;

/// Erreurs de lecture d'un tarif ou d'un bail.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LeaseError {
    /// Ressource inconnue.
    #[error("ressource inconnue : {0}")]
    UnknownResource(String),
    /// Tag manquant ou en double.
    #[error("tag « {0} » manquant ou en double")]
    Tag(&'static str),
    /// Valeur illisible ou hors bornes.
    #[error("valeur invalide pour « {0} »")]
    Value(&'static str),
    /// L'unité du tarif ne correspond pas à la ressource.
    #[error("unité « {unit} » incorrecte pour {resource} (attendu : {expected})")]
    Unit {
        /// Ressource.
        resource: &'static str,
        /// Unité reçue.
        unit: String,
        /// Unité attendue.
        expected: &'static str,
    },
}

/// Unité de facturation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Heure.
    Hour,
    /// Semaine.
    Week,
}

impl Unit {
    /// Nom publié (`hour`, `week`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Week => "week",
        }
    }

    /// Durée en secondes.
    pub fn secs(self) -> u64 {
        match self {
            Self::Hour => 3_600,
            Self::Week => 7 * 86_400,
        }
    }
}

/// Ressource partagée d'un nœud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resource {
    /// 10 Go de disque.
    Disk10,
    /// 128 Go de disque.
    Disk128,
    /// 256 Go de disque.
    Disk256,
    /// Une part de 1/24 du GPU.
    Gpu1of24,
}

impl Resource {
    /// Toutes les ressources.
    pub const ALL: [Resource; 4] = [Self::Disk10, Self::Disk128, Self::Disk256, Self::Gpu1of24];

    /// Nom publié.
    pub fn name(self) -> &'static str {
        match self {
            Self::Disk10 => "disk_10gb",
            Self::Disk128 => "disk_128gb",
            Self::Disk256 => "disk_256gb",
            Self::Gpu1of24 => "gpu_1_24",
        }
    }

    /// Ressource d'un nom publié.
    pub fn parse(s: &str) -> Result<Self, LeaseError> {
        Self::ALL.into_iter().find(|r| r.name() == s).ok_or_else(|| LeaseError::UnknownResource(s.chars().take(40).collect()))
    }

    /// Unité de facturation.
    pub fn unit(self) -> Unit {
        Unit::Week
    }

    /// Durée maximale d'un bail, en unités.
    pub fn max_units(self) -> u32 {
        match self {
            Self::Gpu1of24 => MAX_GPU_WEEKS,
            _ => MAX_DISK_WEEKS,
        }
    }

    /// Octets de disque donnés par ce palier (`None` pour le calcul).
    pub fn disk_bytes(self) -> Option<u64> {
        match self {
            Self::Disk10 => Some(10 * GIB),
            Self::Disk128 => Some(128 * GIB),
            Self::Disk256 => Some(256 * GIB),
            Self::Gpu1of24 => None,
        }
    }

    /// Baux simultanés au plus (0 = illimité).
    pub fn capacity(self) -> u32 {
        match self {
            Self::Gpu1of24 => GPU_SHARES,
            _ => 0,
        }
    }
}

fn tag_all<'a>(tags: &'a [Vec<String>], name: &'a str) -> impl Iterator<Item = &'a Vec<String>> {
    tags.iter().filter(move |t| t.first().map(String::as_str) == Some(name))
}

fn tag_one<'a>(tags: &'a [Vec<String>], name: &'static str) -> Result<Option<&'a str>, LeaseError> {
    let mut it = tag_all(tags, name).filter_map(|t| t.get(1)).map(String::as_str);
    match (it.next(), it.next()) {
        (v, None) => Ok(v),
        _ => Err(LeaseError::Tag(name)),
    }
}

/// Grille tarifaire d'un nœud.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tariff {
    /// Prix par semaine, en centimes de Ẑen.
    pub prices: BTreeMap<Resource, Amount>,
}

impl Tariff {
    /// Lit les tags `["price", ressource, montant, unité]`. Une ressource en double ou une unité fausse est une erreur.
    pub fn from_tags(tags: &[Vec<String>]) -> Result<Self, LeaseError> {
        let mut prices = BTreeMap::new();
        for t in tag_all(tags, "price") {
            let (Some(res), Some(amount), Some(unit)) = (t.get(1), t.get(2), t.get(3)) else { return Err(LeaseError::Value("price")) };
            let resource = Resource::parse(res)?;
            if unit != resource.unit().as_str() {
                return Err(LeaseError::Unit { resource: resource.name(), unit: unit.clone(), expected: resource.unit().as_str() });
            }
            let price: Amount = amount.parse().map_err(|_| LeaseError::Value("price"))?;
            if price.0 == 0 || prices.insert(resource, price).is_some() {
                return Err(LeaseError::Value("price"));
            }
        }
        Ok(Self { prices })
    }

    /// Tags à publier dans l'événement 30850 (`d = "tariffs"`).
    pub fn to_tags(&self) -> Vec<Vec<String>> {
        let mut tags = vec![vec!["d".to_owned(), TARIFF_D_TAG.to_owned()]];
        tags.extend(self.prices.iter().map(|(r, p)| vec!["price".to_owned(), r.name().to_owned(), p.to_string(), r.unit().as_str().to_owned()]));
        tags
    }

    /// Prix de `units` unités de `r`, si la ressource est au tarif (et sans dépassement).
    pub fn cost(&self, r: Resource, units: u32) -> Option<Amount> {
        self.prices.get(&r)?.0.checked_mul(u64::from(units)).map(Amount)
    }
}

/// Demande de bail portée par un virement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRequest {
    /// Ressource demandée.
    pub resource: Resource,
    /// Début (secondes UNIX).
    pub start: u64,
    /// Durée en semaines.
    pub units: u32,
    /// Prix hebdomadaire accepté par l'acheteur (centimes de Ẑen par semaine), figé dans la transaction.
    pub unit_price: Amount,
}

impl LeaseRequest {
    /// Fin du bail (exclue).
    pub fn end(&self) -> u64 {
        self.start.saturating_add(u64::from(self.units) * self.resource.unit().secs())
    }

    /// Un virement porte-t-il `["t","lease"]` ?
    pub fn is_lease(tags: &[Vec<String>]) -> bool {
        tag_all(tags, "t").any(|t| t.get(1).map(String::as_str) == Some("lease"))
    }

    /// Lit la demande dans les tags d'un virement créé à `created_at`. Sans `start`, le bail commence au paiement ;
    /// un début antérieur au paiement ou postérieur de plus de 30 jours est refusé (pas de bail rétroactif).
    pub fn from_tags(tags: &[Vec<String>], created_at: u64) -> Result<Self, LeaseError> {
        let resource = Resource::parse(tag_one(tags, "resource")?.ok_or(LeaseError::Tag("resource"))?)?;
        let units: u32 = tag_one(tags, "units")?.ok_or(LeaseError::Tag("units"))?.parse().map_err(|_| LeaseError::Value("units"))?;
        if units == 0 || units > resource.max_units() {
            return Err(LeaseError::Value("units"));
        }
        let start = match tag_one(tags, "start")? {
            Some(s) => s.parse::<u64>().map_err(|_| LeaseError::Value("start"))?,
            None => created_at,
        };
        if start < created_at || start > created_at.saturating_add(MAX_ADVANCE_SECS) {
            return Err(LeaseError::Value("start"));
        }
        let unit_price: Amount = tag_one(tags, "unit_price")?.ok_or(LeaseError::Tag("unit_price"))?.parse().map_err(|_| LeaseError::Value("unit_price"))?;
        if unit_price.0 == 0 {
            return Err(LeaseError::Value("unit_price"));
        }
        Ok(Self { resource, start, units, unit_price })
    }

    /// Tags à joindre au virement d'achat (en plus de ceux du registre Ğ1-N²).
    pub fn to_tags(&self) -> Vec<Vec<String>> {
        vec![
            vec!["t".into(), "lease".into()],
            vec!["resource".into(), self.resource.name().into()],
            vec!["units".into(), self.units.to_string()],
            vec!["start".into(), self.start.to_string()],
            vec!["unit_price".into(), self.unit_price.to_string()],
        ]
    }
}

fn int(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Requêtes qui inscrivent la grille tarifaire publiée par `node` à l'instant `at` (remplace la précédente, sauf si
/// une grille plus récente est déjà connue : l'ordre d'arrivée ne compte pas).
pub fn tariff_statements(node: &str, tariff: &Tariff, at: u64, src_event: &str) -> Vec<Statement> {
    let text = |s: &str| Value::Text(s.to_owned());
    let mut out = vec![Statement::new("DELETE FROM tariffs WHERE node_hex = ?1 AND effective_at < ?2", vec![text(node), int(at)])];
    for (r, price) in &tariff.prices {
        // Historique immuable : jamais effacé, indépendant de l'ordre d'arrivée.
        out.push(Statement::new(
            "INSERT OR IGNORE INTO tariffs_history(node_hex, resource, price_cents, unit, effective_at, src_event) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![text(node), text(r.name()), int(price.0), text(r.unit().as_str()), int(at), text(src_event)],
        ));
        out.push(Statement::new(
            "INSERT OR REPLACE INTO tariffs(node_hex, resource, price_cents, unit, effective_at, src_event) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE NOT EXISTS (SELECT 1 FROM tariffs WHERE node_hex = ?1 AND effective_at > ?5)",
            vec![text(node), text(r.name()), int(price.0), text(r.unit().as_str()), int(at), text(src_event)],
        ));
    }
    out
}

/// Requête qui inscrit le bail d'un virement `beneficiary → node` de `paid` centimes, créé à `at`. Le statut se
/// décide dans la base : `unpriced` (le nœud ne vend pas cette ressource), `underpaid` (paiement inférieur au prix
/// figé × durée, ou prix figé inférieur au tarif en vigueur à la date du virement dans `tariffs_history`), `full` (24 parts déjà vendues
/// à un instant de la période) ou `active`.
pub fn lease_statement(src_event: &str, beneficiary: &str, node: &str, req: &LeaseRequest, paid: Amount, at: u64) -> Statement {
    let text = |s: &str| Value::Text(s.to_owned());
    Statement::new(
        "INSERT OR IGNORE INTO resource_leases(src_event, beneficiary, node, resource, start_at, end_at, units, paid_cents, unit_price_cents, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10, CASE \
           WHEN NOT EXISTS (SELECT 1 FROM tariffs t WHERE t.node_hex = ?3 AND t.resource = ?4) THEN 'unpriced' \
           WHEN ?8 < ?10 * ?7 THEN 'underpaid' \
           WHEN ?10 < COALESCE((SELECT h.price_cents FROM tariffs_history h WHERE h.node_hex = ?3 AND h.resource = ?4 AND h.effective_at <= ?11 \
                ORDER BY h.effective_at DESC, h.src_event DESC LIMIT 1), 0) THEN 'underpaid' \
           WHEN ?9 > 0 AND (SELECT MAX(c) FROM ( \
                SELECT (SELECT COUNT(*) FROM resource_leases m WHERE m.node = ?3 AND m.resource = ?4 AND m.status = 'active' AND m.start_at <= p.t AND m.end_at > p.t) AS c \
                FROM (SELECT ?5 AS t UNION SELECT start_at FROM resource_leases WHERE node = ?3 AND resource = ?4 AND status = 'active' AND start_at > ?5 AND start_at < ?6) p)) >= ?9 THEN 'full' \
           ELSE 'active' END)",
        vec![text(src_event), text(beneficiary), text(node), text(req.resource.name()), int(req.start), int(req.end()), int(u64::from(req.units)), int(paid.0), int(u64::from(req.resource.capacity())), int(req.unit_price.0), int(at)],
    )
}

/// Bail inscrit comme `invalid` : un virement marqué `lease` mais illisible reste visible, jamais ignoré en silence.
pub fn invalid_lease_statement(src_event: &str, beneficiary: &str, node: &str, at: u64, paid: Amount) -> Statement {
    Statement::new(
        "INSERT OR IGNORE INTO resource_leases(src_event, beneficiary, node, resource, start_at, end_at, units, paid_cents, unit_price_cents, status) VALUES (?1, ?2, ?3, 'invalid', ?4, ?4, 0, ?5, 0, 'invalid')",
        vec![Value::Text(src_event.to_owned()), Value::Text(beneficiary.to_owned()), Value::Text(node.to_owned()), int(at), int(paid.0)],
    )
}

/// `SELECT EXISTS(…)` : un des bénéficiaires du demandeur détient-il un bail actif sur `resource` de `node` à
/// l'instant `now` ? Paramètres : `?1` nœud, `?2` ressource, `?3` instant, `?4` PeerID du demandeur.
///
/// Le demandeur d'un flux P2P est une station : ses bénéficiaires sont son NODE (`peer_id`) et les MULTIPASS qu'elle
/// héberge (`home_station = '<PeerID>:<NODE_HEX>'`).
pub const ACTIVE_FOR_PEER_SQL: &str = "SELECT EXISTS(SELECT 1 FROM resource_leases l WHERE l.node = ?1 AND l.resource = ?2 AND l.status = 'active' \
     AND l.start_at <= ?3 AND l.end_at > ?3 \
     AND l.beneficiary IN (SELECT hex FROM identities WHERE peer_id = ?4 UNION SELECT hex FROM identities WHERE home_station LIKE ?4 || ':%'))";

/// Octets de disque obtenus par les baux actifs de `beneficiary` chez `node` à l'instant `now` (le plus grand palier).
pub const LEASED_QUOTA_SQL: &str = "SELECT COALESCE(MAX(CASE resource WHEN 'disk_10gb' THEN 10 WHEN 'disk_128gb' THEN 128 WHEN 'disk_256gb' THEN 256 ELSE 0 END), 0) \
     FROM resource_leases WHERE node = ?1 AND beneficiary = ?2 AND status = 'active' AND start_at <= ?3 AND end_at > ?3 AND resource LIKE 'disk\\_%' ESCAPE '\\'";

/// Octets de disque donnés par les baux actifs (voir [`LEASED_QUOTA_SQL`], qui rend des Gio).
pub fn gib_to_bytes(gib: i64) -> u64 {
    u64::try_from(gib).unwrap_or(0).saturating_mul(GIB)
}

/// Le demandeur (PeerID) détient-il un bail actif ?
pub fn has_active_lease(conn: &rusqlite::Connection, node: &str, resource: Resource, now: u64, peer_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(ACTIVE_FOR_PEER_SQL, rusqlite::params![node, resource.name(), i64::try_from(now).unwrap_or(i64::MAX), peer_id], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|t| t.iter().map(|s| (*s).to_owned()).collect()).collect()
    }

    #[test]
    fn a_tariff_round_trips_through_its_tags() {
        let mut t = Tariff::default();
        t.prices.insert(Resource::Disk10, Amount(100));
        t.prices.insert(Resource::Disk128, Amount(400));
        t.prices.insert(Resource::Disk256, Amount(700));
        t.prices.insert(Resource::Gpu1of24, Amount(50));
        let tags = t.to_tags();
        assert_eq!(tags[0], ["d", "tariffs"]);
        assert!(tags.contains(&vec!["price".into(), "gpu_1_24".into(), "0.50".into(), "week".into()]));
        assert_eq!(Tariff::from_tags(&tags).unwrap(), t);
        assert_eq!(t.cost(Resource::Gpu1of24, 3), Some(Amount(150)));
        assert_eq!(Tariff::default().cost(Resource::Disk10, 1), None);
    }

    #[test]
    fn a_tariff_with_a_wrong_unit_a_duplicate_or_a_zero_price_is_refused() {
        for bad in [
            tags(&[&["price", "gpu_1_24", "0.50", "hour"]]),
            tags(&[&["price", "disk_10gb", "1.00", "hour"]]),
            tags(&[&["price", "disk_10gb", "1.00", "week"], &["price", "disk_10gb", "2.00", "week"]]),
            tags(&[&["price", "disk_10gb", "0", "week"]]),
            tags(&[&["price", "disk_1tb", "1.00", "week"]]),
            tags(&[&["price", "disk_10gb", "abc", "week"]]),
            tags(&[&["price", "disk_10gb"]]),
        ] {
            assert!(Tariff::from_tags(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_lease_request_is_read_from_the_transfer_tags_with_sane_bounds() {
        let now = 1_800_000_000;
        let ok = tags(&[&["t", "g1-n2"], &["t", "lease"], &["resource", "gpu_1_24"], &["units", "3"], &["unit_price", "0.50"]]);
        assert!(LeaseRequest::is_lease(&ok));
        let r = LeaseRequest::from_tags(&ok, now).unwrap();
        assert_eq!((r.resource, r.start, r.units, r.end(), r.unit_price), (Resource::Gpu1of24, now, 3, now + 3 * 7 * 86_400, Amount(50)));
        let weeks = tags(&[&["resource", "disk_128gb"], &["units", "2"], &["start", "1800003600"], &["unit_price", "4.00"]]);
        let r = LeaseRequest::from_tags(&weeks, now).unwrap();
        assert_eq!(r.end(), now + 3600 + 14 * 86_400);
        assert_eq!(LeaseRequest::from_tags(&r.to_tags(), now).unwrap(), r, "aller-retour");
        for (bad, why) in [
            (tags(&[&["resource", "gpu_1_24"], &["units", "0"], &["unit_price", "1"]]), "zéro heure"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "13"], &["unit_price", "1"]]), "plus de 12 semaines de GPU"),
            (tags(&[&["resource", "disk_10gb"], &["units", "53"], &["unit_price", "1"]]), "plus d'un an de disque"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "1"], &["start", "1799999999"], &["unit_price", "1"]]), "bail rétroactif"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "1"], &["start", "1810000000"], &["unit_price", "1"]]), "réservé trop loin"),
            (tags(&[&["units", "1"]]), "ressource absente"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "1"]]), "prix unitaire absent"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "1"], &["unit_price", "0"]]), "prix nul"),
            (tags(&[&["resource", "gpu_1_24"], &["units", "1"], &["units", "2"], &["unit_price", "1"]]), "units en double"),
            (tags(&[&["resource", "ram_8gb"], &["units", "1"], &["unit_price", "1"]]), "ressource inconnue"),
        ] {
            assert!(LeaseRequest::from_tags(&bad, now).is_err(), "{why}");
        }
        assert!(!LeaseRequest::is_lease(&tags(&[&["t", "g1-n2"]])));
    }

    #[test]
    fn resources_know_their_units_quotas_and_capacity() {
        assert_eq!(Resource::Gpu1of24.unit(), Unit::Week);
        assert_eq!(GPU_WEEKLY_QUOTA_SECS, 25_200);
        assert_eq!(Resource::Disk256.disk_bytes(), Some(256 << 30));
        assert_eq!(Resource::Gpu1of24.capacity(), 24);
        assert_eq!(Resource::Disk10.capacity(), 0);
        assert_eq!(Resource::ALL.map(Resource::name), ["disk_10gb", "disk_128gb", "disk_256gb", "gpu_1_24"]);
        assert_eq!(gib_to_bytes(128), 128 << 30);
    }
}
