//! Limitation de débit par adresse IP (seau à jetons).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Au-delà de ce nombre d'adresses suivies, les seaux inactifs sont purgés.
const MAX_TRACKED: usize = 10_000;

/// Seau à jetons par IP : `burst` jetons au repos, `per_minute` jetons rendus par minute.
pub struct RateLimiter {
    burst: f64,
    per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl RateLimiter {
    /// Crée un limiteur.
    pub fn new(burst: u32, per_minute: u32) -> Self {
        Self { burst: f64::from(burst), per_sec: f64::from(per_minute) / 60.0, buckets: Mutex::new(HashMap::new()) }
    }

    /// Consomme un jeton pour `ip` ; `false` si la limite est atteinte.
    pub fn check(&self, ip: IpAddr) -> bool {
        self.check_at(ip, Instant::now())
    }

    /// Variante à horloge injectée.
    pub fn check_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED {
            let idle = Duration::from_secs(600);
            map.retain(|_, (_, last)| now.saturating_duration_since(*last) < idle);
        }
        let entry = map.entry(ip).or_insert((self.burst, now));
        let elapsed = now.saturating_duration_since(entry.1).as_secs_f64();
        entry.0 = (entry.0 + elapsed * self.per_sec).min(self.burst);
        entry.1 = now;
        if entry.0 >= 1.0 {
            entry.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn burst_then_refill() {
        let l = RateLimiter::new(3, 60); // 1 jeton par seconde
        let t0 = Instant::now();
        assert!(l.check_at(ip(1), t0));
        assert!(l.check_at(ip(1), t0));
        assert!(l.check_at(ip(1), t0));
        assert!(!l.check_at(ip(1), t0), "burst épuisé");
        assert!(!l.check_at(ip(1), t0 + Duration::from_millis(500)));
        assert!(l.check_at(ip(1), t0 + Duration::from_millis(1500)), "un jeton rendu après 1 s");
        assert!(!l.check_at(ip(1), t0 + Duration::from_millis(1500)));
    }

    #[test]
    fn addresses_are_independent_and_refill_is_capped() {
        let l = RateLimiter::new(2, 60);
        let t0 = Instant::now();
        assert!(l.check_at(ip(1), t0) && l.check_at(ip(1), t0));
        assert!(!l.check_at(ip(1), t0));
        assert!(l.check_at(ip(2), t0), "une autre IP a son propre seau");
        let later = t0 + Duration::from_secs(3600);
        assert!(l.check_at(ip(1), later) && l.check_at(ip(1), later));
        assert!(!l.check_at(ip(1), later), "le seau ne dépasse jamais le burst");
    }
}
