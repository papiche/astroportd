//! Télétrafic : formules d'Erlang pour dimensionner les ressources partagées (parts de GPU 1/24 d'un Brain).
//!
//! - **Erlang-B** : probabilité qu'une demande soit *perdue* (refusée) quand les `c` serveurs sont tous occupés et
//!   qu'il n'y a pas de file — c'est le cas d'un bail 1/24 vendu en plein ([`crate::telemetry::erlang_b`]) ;
//! - **Erlang-C** : probabilité qu'une demande doive *attendre* quand elle peut faire la queue, et délai moyen
//!   d'attente ([`erlang_c`], [`mean_wait_secs`]).
//!
//! Charge offerte `a` en erlangs = taux d'arrivée × durée moyenne de service. Les calculs sont exacts (récurrence
//! d'Erlang, sans factorielles : stable pour des centaines de serveurs).

/// Parts de GPU qu'un Brain vend en même temps (1/24).
pub const BRAIN_CAPACITY: u32 = 24;
/// Durée moyenne d'une tâche GPU (génération d'image, requête LLM longue), en secondes.
pub const DEFAULT_SERVICE_SECS: f64 = 120.0;
/// Délai d'attente moyen au-delà duquel une station sans GPU doit réserver du calcul, en secondes.
pub const CRITICAL_WAIT_SECS: f64 = 30.0;

fn sane(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

/// Erlang-B : probabilité de perte pour une charge offerte `offered` (erlangs) sur `servers` serveurs.
///
/// `B(0) = 1`, `B(k) = a·B(k−1) / (k + a·B(k−1))`. Sans serveur, toute demande est perdue (sauf charge nulle).
pub fn erlang_b(offered: f64, servers: u32) -> f64 {
    let a = sane(offered);
    if a == 0.0 {
        return 0.0;
    }
    let mut b = 1.0;
    for k in 1..=servers {
        b = a * b / (f64::from(k) + a * b);
    }
    b
}

/// Erlang-C : probabilité qu'une demande attende (file d'attente illimitée). Vaut 1 dès que la charge atteint la
/// capacité (`a ≥ c` : la file ne se vide jamais).
pub fn erlang_c(offered: f64, servers: u32) -> f64 {
    let a = sane(offered);
    if a == 0.0 {
        return 0.0;
    }
    if servers == 0 || a >= f64::from(servers) {
        return 1.0;
    }
    let b = erlang_b(a, servers);
    let rho = a / f64::from(servers);
    b / (1.0 - rho * (1.0 - b))
}

/// Délai moyen d'attente, toutes demandes confondues : `C(a,c) · h / (c − a)`. Infini si la file ne se vide pas.
pub fn mean_wait_secs(offered: f64, servers: u32, mean_service_secs: f64) -> f64 {
    let a = sane(offered);
    if a == 0.0 {
        return 0.0;
    }
    if servers == 0 || a >= f64::from(servers) {
        return f64::INFINITY;
    }
    erlang_c(a, servers) * sane(mean_service_secs) / (f64::from(servers) - a)
}

/// Hypothèses du modèle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadModel {
    /// Serveurs : parts de GPU effectivement disponibles pour cette charge.
    pub servers: u32,
    /// Durée moyenne d'une tâche, en secondes.
    pub mean_service_secs: f64,
}

impl Default for LoadModel {
    fn default() -> Self {
        Self { servers: BRAIN_CAPACITY, mean_service_secs: DEFAULT_SERVICE_SECS }
    }
}

/// Diagnostic de charge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrafficReport {
    /// Charge offerte en erlangs : flux actifs + tâches en attente (tout ce qui demande du service).
    pub offered_erlangs: f64,
    /// Serveurs du modèle.
    pub servers: u32,
    /// Taux d'occupation, entre 0 et 1.
    pub utilization: f64,
    /// Erlang-B : probabilité de perte sans file.
    pub blocking_probability: f64,
    /// Erlang-C : probabilité d'attendre avec file.
    pub wait_probability: f64,
    /// Délai moyen d'attente (toutes demandes), en secondes ; infini si la file ne se vide pas.
    pub mean_wait_secs: f64,
    /// Délai moyen de ceux qui attendent réellement, en secondes.
    pub mean_wait_if_queued_secs: f64,
}

impl TrafficReport {
    /// Le délai estimé dépasse-t-il `limit_secs` ?
    pub fn wait_exceeds(&self, limit_secs: f64) -> bool {
        self.mean_wait_secs > limit_secs
    }
}

/// Évalue la charge d'un Brain (24 parts, tâches de 120 s) à partir des flux actifs et des tâches en attente.
pub fn estimate_load(active_streams: u32, waiting_tasks: u32) -> TrafficReport {
    estimate_load_for(active_streams, waiting_tasks, LoadModel::default())
}

/// Évalue la charge avec un modèle explicite. Une station **sans GPU** a `servers = 0` (ou le nombre de ses baux
/// actifs) : dès qu'une tâche attend, le délai est infini.
pub fn estimate_load_for(active_streams: u32, waiting_tasks: u32, model: LoadModel) -> TrafficReport {
    let a = f64::from(active_streams) + f64::from(waiting_tasks);
    let c = f64::from(model.servers);
    let wait_probability = erlang_c(a, model.servers);
    let mean_wait = mean_wait_secs(a, model.servers, model.mean_service_secs);
    let if_queued = if a == 0.0 {
        0.0
    } else if model.servers == 0 || a >= c {
        f64::INFINITY
    } else {
        sane(model.mean_service_secs) / (c - a)
    };
    TrafficReport {
        offered_erlangs: a,
        servers: model.servers,
        utilization: if model.servers == 0 { f64::from(u8::from(a > 0.0)) } else { (a / c).min(1.0) },
        blocking_probability: erlang_b(a, model.servers),
        wait_probability,
        mean_wait_secs: mean_wait,
        mean_wait_if_queued_secs: if_queued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "{a} ≠ {b}");
    }

    #[test]
    fn erlang_b_matches_the_classic_table() {
        close(erlang_b(1.0, 1), 0.5, 1e-12);
        close(erlang_b(2.0, 2), 0.4, 1e-12);
        close(erlang_b(5.0, 10), 0.018_385, 1e-5);
        close(erlang_b(10.0, 10), 0.214_6, 1e-4);
        // Contre la formule directe `(aᶜ/c!) / Σ aᵏ/k!` (factorielles, valable pour de petits c).
        let direct = |a: f64, c: u32| {
            let fact = |n: u32| (1..=n).map(f64::from).product::<f64>();
            let sum: f64 = (0..=c).map(|k| a.powi(k as i32) / fact(k)).sum();
            a.powi(c as i32) / fact(c) / sum
        };
        for (a, c) in [(0.5, 1), (3.0, 5), (7.5, 10), (14.0, 16), (19.0, 20), (20.0, 20)] {
            close(erlang_b(a, c), direct(a, c), 1e-12);
        }
        close(erlang_b(20.0, 24), 0.066_1, 1e-4); // un Brain à 20/24 refuse ~7 % des demandes sans file
    }

    #[test]
    fn erlang_c_and_the_mean_wait_follow_from_erlang_b() {
        close(erlang_c(1.0, 2), 1.0 / 3.0, 1e-12);
        close(erlang_c(8.0, 10), 0.409_2, 1e-3);
        close(mean_wait_secs(1.0, 2, 60.0), 20.0, 1e-9); // C·h/(c−a) = (1/3)·60/1
        close(mean_wait_secs(8.0, 10, 100.0), 0.409_2 * 100.0 / 2.0, 0.1);
        // Plus la charge approche la capacité, plus l'attente explose.
        assert!(mean_wait_secs(23.5, 24, 120.0) > 20.0 * mean_wait_secs(20.0, 24, 120.0));
    }

    #[test]
    fn degenerate_inputs_are_handled_exactly() {
        assert_eq!((erlang_b(0.0, 24), erlang_c(0.0, 24), mean_wait_secs(0.0, 24, 120.0)), (0.0, 0.0, 0.0));
        assert_eq!((erlang_b(3.0, 0), erlang_c(3.0, 0)), (1.0, 1.0));
        assert!(mean_wait_secs(3.0, 0, 120.0).is_infinite() && mean_wait_secs(24.0, 24, 120.0).is_infinite());
        for bad in [f64::NAN, f64::NEG_INFINITY, -5.0] {
            assert_eq!(erlang_b(bad, 24), 0.0);
        }
        assert!(erlang_b(500.0, 1000) < 1e-30, "stable pour mille serveurs");
        assert!(erlang_b(1000.0, 500) > 0.5);
    }

    #[test]
    fn a_brain_with_free_shares_has_no_queue_and_a_full_one_never_drains() {
        let idle = estimate_load(3, 0);
        assert_eq!((idle.offered_erlangs, idle.servers), (3.0, 24));
        assert!(idle.mean_wait_secs < 0.01 && idle.blocking_probability < 1e-9);
        let busy = estimate_load(20, 3);
        assert!(busy.wait_probability > 0.3 && busy.mean_wait_secs.is_finite() && busy.utilization > 0.9);
        let saturated = estimate_load(24, 5);
        assert!(saturated.mean_wait_secs.is_infinite() && saturated.wait_probability == 1.0 && saturated.utilization == 1.0);
        assert!(saturated.wait_exceeds(CRITICAL_WAIT_SECS) && !idle.wait_exceeds(CRITICAL_WAIT_SECS));
    }

    #[test]
    fn a_satellite_without_gpu_waits_forever_as_soon_as_a_task_is_queued() {
        let model = LoadModel { servers: 0, mean_service_secs: 60.0 };
        let none = estimate_load_for(0, 0, model);
        assert_eq!((none.mean_wait_secs, none.utilization), (0.0, 0.0));
        let one = estimate_load_for(0, 1, model);
        assert!(one.mean_wait_secs.is_infinite() && one.mean_wait_if_queued_secs.is_infinite() && one.utilization == 1.0);
        // Avec un bail actif (1 part) et 1 tâche de 60 s, la charge atteint la capacité : toujours infini ;
        // deux parts : attente finie.
        assert!(estimate_load_for(0, 1, LoadModel { servers: 1, mean_service_secs: 60.0 }).mean_wait_secs.is_infinite());
        let two = estimate_load_for(0, 1, LoadModel { servers: 2, mean_service_secs: 60.0 });
        close(two.mean_wait_secs, (1.0 / 3.0 * 0.0 + erlang_c(1.0, 2)) * 60.0 / 1.0, 1e-9);
    }
}
