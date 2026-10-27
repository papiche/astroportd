//! Plugin d'exemple pour `astro-plugins` (ABI v1) : deux fonctions pures, déterministes, sans aucune capacité.
//!
//! - `{"fn":"derive_key","seed":"…","label":"…","length":32}` → `{"key_hex":"…","length":32}`
//!   (SHA-256 itéré sur `seed ‖ 0x00 ‖ label ‖ compteur` : une clé par étiquette, jamais la graine elle-même) ;
//! - `{"fn":"resonance","a":"…","b":"…"}` → `{"permille":0..1000,"shared":n}`
//!   (cosinus des histogrammes de paires d'octets : 1000 = textes identiques, 0 = rien en commun).

use std::alloc::{Layout, alloc as raw_alloc, dealloc as raw_dealloc};
use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn layout(len: usize) -> Layout {
    Layout::from_size_align(len.max(1), 1).expect("taille raisonnable")
}

/// Réserve `len` octets dans la mémoire du plugin pour l'hôte.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: i32) -> i32 {
    // SAFETY : `layout` est valide (taille ≥ 1, alignement 1) ; l'hôte écrit au plus `len` octets.
    unsafe { raw_alloc(layout(len.max(0) as usize)) as i32 }
}

/// Libère un bloc rendu par `alloc` ou par `plugin_call`.
#[unsafe(no_mangle)]
pub extern "C" fn dealloc(ptr: i32, len: i32) {
    // SAFETY : l'hôte ne rend que des blocs issus de `alloc` / `plugin_call`, avec leur longueur d'origine.
    unsafe { raw_dealloc(ptr as *mut u8, layout(len.max(0) as usize)) }
}

fn pack(bytes: Vec<u8>) -> i64 {
    let len = bytes.len();
    // Copie dans un bloc de taille exacte, pour que `dealloc(ptr, len)` côté hôte soit correct.
    let ptr = alloc(len as i32);
    // SAFETY : `ptr` vient d'être réservé pour `len` octets.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, len) };
    ((ptr as u32 as i64) << 32) | len as u32 as i64
}

fn envelope(r: Result<Value, String>) -> Vec<u8> {
    match r {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "error": e }),
    }
    .to_string()
    .into_bytes()
}

/// Décrit le plugin.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_manifest() -> i64 {
    pack(json!({ "version": env!("CARGO_PKG_VERSION"), "description": "clé dérivée (derive_key) et résonance entre deux textes (resonance)" }).to_string().into_bytes())
}

/// Point d'entrée : JSON en `[ptr, ptr+len)`, enveloppe JSON en retour.
#[unsafe(no_mangle)]
pub extern "C" fn plugin_call(ptr: i32, len: i32) -> i64 {
    // SAFETY : l'hôte a écrit `len` octets à `ptr` juste après `alloc`.
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize) };
    pack(envelope(handle(input)))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key).and_then(Value::as_str).ok_or_else(|| format!("champ « {key} » (texte) manquant"))
}

fn handle(input: &[u8]) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(input).map_err(|e| format!("JSON illisible : {e}"))?;
    match text(&v, "fn")? {
        "derive_key" => derive_key(text(&v, "seed")?, text(&v, "label")?, v.get("length").and_then(Value::as_u64).unwrap_or(32)),
        "resonance" => Ok(resonance(text(&v, "a")?, text(&v, "b")?)),
        other => Err(format!("fonction « {other} » inconnue (derive_key, resonance)")),
    }
}

fn derive_key(seed: &str, label: &str, length: u64) -> Result<Value, String> {
    if !(16..=64).contains(&length) {
        return Err("length doit valoir entre 16 et 64".into());
    }
    let mut key = Vec::new();
    for counter in 0u8.. {
        if key.len() >= length as usize {
            break;
        }
        let mut h = Sha256::new();
        h.update(seed.as_bytes());
        h.update([0]);
        h.update(label.as_bytes());
        h.update([counter]);
        key.extend_from_slice(&h.finalize());
    }
    key.truncate(length as usize);
    Ok(json!({ "key_hex": key.iter().map(|b| format!("{b:02x}")).collect::<String>(), "length": length }))
}

fn bigrams(s: &str) -> BTreeMap<(u8, u8), u64> {
    let mut m = BTreeMap::new();
    for w in s.as_bytes().windows(2) {
        *m.entry((w[0], w[1])).or_insert(0) += 1;
    }
    m
}

fn resonance(a: &str, b: &str) -> Value {
    let (x, y) = (bigrams(a), bigrams(b));
    let dot: u64 = x.iter().filter_map(|(k, n)| y.get(k).map(|m| n * m)).sum();
    let norm = |m: &BTreeMap<(u8, u8), u64>| (m.values().map(|n| n * n).sum::<u64>() as f64).sqrt();
    let denom = norm(&x) * norm(&y);
    let permille = if denom == 0.0 { 0 } else { ((dot as f64 / denom) * 1000.0).round() as u64 };
    json!({ "permille": permille, "shared": x.keys().filter(|k| y.contains_key(*k)).count() })
}
