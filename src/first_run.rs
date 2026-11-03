//! Premier démarrage d'une station sans Capitaine : le **code d'installation**.
//!
//! Une station installée sans email de Capitaine (paquet, image prête à l'emploi, `init --yes`) ne doit pas donner son
//! Capitainerie au premier visiteur venu : sur un domaine public, ce peut être n'importe qui. `serve` tire donc au démarrage
//! un code court, l'écrit **dans son journal seulement** (terminal, `journalctl`), et la page `/welcome` le demande. Qui lit
//! le journal a la main sur la machine : c'est la preuve qu'on attend.
//!
//! - Le code ne vit qu'en mémoire : un redémarrage en tire un autre.
//! - Il ne sert **qu'une fois** : une réussite le remplace aussitôt (deux onglets ne créent pas deux Capitaines).
//! - Après [`MAX_FAILURES`] essais faux, il est remplacé et le nouveau est écrit dans le journal : deviner ne sert à rien
//!   (32⁸ possibilités, cinq essais par code).

use std::sync::Mutex;

/// Essais faux tolérés avant de tirer un nouveau code.
pub const MAX_FAILURES: u32 = 5;

/// Alphabet sans caractères ambigus (ni 0/O, ni 1/I/L) : le code se recopie depuis un terminal sans erreur.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// Code d'installation de la station (voir le module).
pub struct SetupCode {
    state: Mutex<(String, u32)>,
    announce: Box<dyn Fn(&str) + Send + Sync>,
}

impl SetupCode {
    /// Tire un premier code et l'annonce par `announce` (le journal de la station).
    pub fn new(announce: Box<dyn Fn(&str) + Send + Sync>) -> Result<Self, getrandom::Error> {
        let code = draw()?;
        announce(&code);
        Ok(Self { state: Mutex::new((code, 0)), announce })
    }

    /// Vérifie un essai (casse, espaces et tirets indifférents). Une réussite **consomme** le code ; trop d'échecs le
    /// remplacent. Rend `true` si l'essai était le bon.
    pub fn redeem(&self, attempt: &str) -> bool {
        let mut st = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ok = constant_time_eq(normalize(attempt).as_bytes(), normalize(&st.0).as_bytes());
        if ok {
            // Usage unique : le code ne resservira pas (le nouveau n'est pas annoncé, la station a désormais son Capitaine).
            st.0 = draw().unwrap_or_default();
            st.1 = 0;
            return true;
        }
        st.1 += 1;
        if st.1 >= MAX_FAILURES {
            if let Ok(code) = draw() {
                (self.announce)(&code);
                *st = (code, 0);
            }
        }
        false
    }
}

/// Huit caractères, présentés `XXXX-XXXX`.
fn draw() -> Result<String, getrandom::Error> {
    let mut out = String::with_capacity(9);
    let mut buf = [0u8; 32];
    while out.len() < 9 {
        getrandom::fill(&mut buf)?;
        // 31 symboles : on rejette au-delà de 248 (= 31 × 8) pour que chacun soit équiprobable.
        for b in buf.iter().filter(|b| **b < 248) {
            if out.len() == 4 {
                out.push('-');
            }
            if out.len() == 9 {
                break;
            }
            out.push(ALPHABET[usize::from(*b) % ALPHABET.len()] as char);
        }
    }
    Ok(out)
}

fn normalize(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn gate() -> (SetupCode, Arc<Mutex<Vec<String>>>) {
        let said = Arc::new(Mutex::new(Vec::new()));
        let s = said.clone();
        (SetupCode::new(Box::new(move |c| s.lock().unwrap().push(c.to_owned()))).unwrap(), said)
    }

    #[test]
    fn the_code_is_short_unambiguous_and_announced_in_the_journal() {
        let (_g, said) = gate();
        let code = said.lock().unwrap()[0].clone();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(code.chars().filter(|c| *c != '-').all(|c| ALPHABET.contains(&(c as u8))), "{code}");
    }

    #[test]
    fn a_good_code_works_once_whatever_its_case_and_dashes() {
        let (g, said) = gate();
        let code = said.lock().unwrap()[0].clone();
        let typed = code.replace('-', " ").to_ascii_lowercase();
        assert!(g.redeem(&typed));
        assert!(!g.redeem(&code), "usage unique : un second onglet ne crée pas un second Capitaine");
    }

    #[test]
    fn too_many_wrong_guesses_draw_and_announce_a_new_code() {
        let (g, said) = gate();
        let first = said.lock().unwrap()[0].clone();
        for _ in 0..MAX_FAILURES {
            assert!(!g.redeem("AAAA-AAAA") || first == "AAAA-AAAA");
        }
        let all = said.lock().unwrap().clone();
        assert_eq!(all.len(), 2, "un nouveau code est écrit dans le journal");
        assert!(!g.redeem(&first) || all[1] == first, "l'ancien code ne vaut plus");
        assert!(g.redeem(&all[1]));
    }
}
