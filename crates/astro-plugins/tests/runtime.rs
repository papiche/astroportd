//! Le runtime Wasm : un vrai plugin Rust compilé (`plugins/resonance.wasm`) et des modules hostiles écrits en WAT.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use astro_plugins::{Limits, PluginError, PluginHost, valid_name};
use serde_json::json;

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn real_host() -> PluginHost {
    let (host, report) = PluginHost::load_dir(&plugins_dir(), Limits::default()).unwrap();
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(report.loaded, ["resonance"]);
    host
}

/// Module minimal conforme à l'ABI : `plugin_call` rend l'enveloppe `{"ok":N}` où N compte les appels *de cette instance*.
const COUNTER: &str = r#"(module
  (memory (export "memory") 1)
  (data (i32.const 0) "{\"ok\":0}")
  (global $n (mut i32) (i32.const 0))
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "plugin_call") (param i32 i32) (result i64)
    (global.set $n (i32.add (global.get $n) (i32.const 1)))
    (i32.store8 (i32.const 6) (i32.add (i32.const 48) (global.get $n)))
    (i64.const 8)))"#;

fn host_with(name: &str, wat: &str, limits: Limits) -> Result<PluginHost, PluginError> {
    let mut h = PluginHost::new(limits)?;
    h.load_bytes(name, wat.as_bytes())?;
    Ok(h)
}

#[test]
fn the_real_plugin_derives_keys_and_measures_resonance_deterministically() {
    let host = real_host();
    let info = host.list();
    assert_eq!((info[0].name.as_str(), info[0].version.as_str()), ("resonance", "0.1.0"));
    assert!(info[0].description.contains("derive_key") && info[0].size > 1000);

    let call = |v: serde_json::Value| host.call_value("resonance", &v);
    let k = call(json!({ "fn": "derive_key", "seed": "graine secrète", "label": "uplanet/nostr", "length": 32 })).unwrap();
    let again = call(json!({ "fn": "derive_key", "seed": "graine secrète", "label": "uplanet/nostr", "length": 32 })).unwrap();
    assert_eq!(k, again, "déterministe : même entrée, même clé");
    assert_eq!(k["key_hex"].as_str().unwrap().len(), 64);
    let other = call(json!({ "fn": "derive_key", "seed": "graine secrète", "label": "uplanet/ipfs" })).unwrap();
    assert_ne!(k["key_hex"], other["key_hex"], "une étiquette, une clé");
    let long = call(json!({ "fn": "derive_key", "seed": "s", "label": "l", "length": 64 })).unwrap();
    assert_eq!(long["key_hex"].as_str().unwrap().len(), 128);

    assert_eq!(call(json!({ "fn": "resonance", "a": "bonjour le monde", "b": "bonjour le monde" })).unwrap()["permille"], 1000);
    assert_eq!(call(json!({ "fn": "resonance", "a": "abcd", "b": "wxyz" })).unwrap()["permille"], 0);
    let partial = call(json!({ "fn": "resonance", "a": "le pain au levain", "b": "le pain du dimanche" })).unwrap();
    assert!((1..1000).contains(&partial["permille"].as_u64().unwrap()), "{partial}");
    assert_eq!(call(json!({ "fn": "resonance", "a": "", "b": "x" })).unwrap()["permille"], 0);
}

#[test]
fn derived_keys_match_an_independent_sha256() {
    // sha256(seed ‖ 0x00 ‖ label ‖ compteur 0) pour length = 32, recalculé ici sans le plugin.
    use std::io::Write;
    let out = std::process::Command::new("python3")
        .args(["-c", "import hashlib,sys; print(hashlib.sha256(b'abc' + b'\\x00' + b'etiquette' + b'\\x00').hexdigest())"])
        .output();
    let Ok(out) = out else { return }; // pas de python : ce test de recoupement est facultatif
    let expected = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    let host = real_host();
    let k = host.call_value("resonance", &json!({ "fn": "derive_key", "seed": "abc", "label": "etiquette" })).unwrap();
    assert_eq!(k["key_hex"], expected.as_str());
    let _ = std::io::stdout().flush();
}

#[test]
fn plugin_errors_come_back_as_errors_not_crashes() {
    let host = real_host();
    for (input, needle) in [
        (r#"{"fn":"inconnue"}"#, "inconnue"),
        (r#"{"fn":"derive_key","seed":"a","label":"b","length":8}"#, "entre 16 et 64"),
        (r#"{"fn":"resonance","a":"x"}"#, "« b »"),
        ("pas du json", "JSON illisible"),
    ] {
        let e = host.call("resonance", input).unwrap_err();
        assert!(matches!(&e, PluginError::Plugin(m) if m.contains(needle)), "{input} → {e:?}");
    }
    assert!(matches!(host.call("absent", "{}"), Err(PluginError::NotFound(_))));
}

#[test]
fn a_module_asking_for_the_outside_world_is_refused_at_load_time() {
    for (imports, wat) in [
        ("env.read_file", r#"(module (import "env" "read_file" (func (param i32) (result i32))) (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0)) (func (export "plugin_call") (param i32 i32) (result i64) (i64.const 0)))"#),
        ("wasi_snapshot_preview1.fd_write", r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0)) (func (export "plugin_call") (param i32 i32) (result i64) (i64.const 0)))"#),
    ] {
        let e = host_with("evil", wat, Limits::default()).err().unwrap();
        assert!(matches!(&e, PluginError::ForbiddenImports(m) if m.contains(imports)), "{e:?}");
    }
    // Un module sans les exports de l'ABI n'est pas un plugin.
    let e = host_with("vide", "(module)", Limits::default()).err().unwrap();
    assert!(matches!(e, PluginError::Invalid(m) if m.contains("memory")));
    assert!(matches!(host_with("x", "pas du wasm", Limits::default()), Err(PluginError::Invalid(_))));
}

#[test]
fn runaway_computation_memory_and_traps_are_contained() {
    let looping = r#"(module (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0))
        (func (export "plugin_call") (param i32 i32) (result i64) (loop $l (br $l)) (i64.const 0)))"#;
    let h = host_with("boucle", looping, Limits { fuel: 100_000, ..Limits::default() }).unwrap();
    assert!(matches!(h.call("boucle", "{}"), Err(PluginError::OutOfFuel)), "une boucle infinie s'arrête, elle ne bloque pas le démon");

    let greedy = r#"(module (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0))
        (func (export "plugin_call") (param i32 i32) (result i64) (drop (memory.grow (i32.const 60000))) (i64.const 0)))"#;
    let h = host_with("glouton", greedy, Limits::default()).unwrap();
    assert!(matches!(h.call("glouton", "{}"), Err(PluginError::MemoryLimit | PluginError::Trap(_))), "{:?}", h.call("glouton", "{}"));

    let trapping = r#"(module (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0))
        (func (export "plugin_call") (param i32 i32) (result i64) unreachable))"#;
    let h = host_with("plante", trapping, Limits::default()).unwrap();
    assert!(matches!(h.call("plante", "{}"), Err(PluginError::Trap(_))));
}

#[test]
fn inputs_outputs_and_pointers_are_bounds_checked() {
    let h = host_with("compteur", COUNTER, Limits { max_input: 16, max_output: 4, ..Limits::default() }).unwrap();
    assert!(matches!(h.call("compteur", &"x".repeat(17)), Err(PluginError::TooLarge { what: "entrée", .. })));
    assert!(matches!(h.call("compteur", "{}"), Err(PluginError::TooLarge { what: "sortie", .. })), "sortie de 8 octets pour un plafond de 4");

    let outside = r#"(module (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) (i32.const 0))
        (func (export "plugin_call") (param i32 i32) (result i64) (i64.const 0x0000FFFF00000010)))"#; // adresse 65535, 16 octets : dépasse la page
    let h = host_with("hors", outside, Limits::default()).unwrap();
    assert!(matches!(h.call("hors", "{}"), Err(PluginError::BadOutput(_))));

    let junk = r#"(module (memory (export "memory") 1) (data (i32.const 0) "pas du json") (func (export "alloc") (param i32) (result i32) (i32.const 64))
        (func (export "plugin_call") (param i32 i32) (result i64) (i64.const 11)))"#;
    let h = host_with("bruit", junk, Limits::default()).unwrap();
    assert!(matches!(h.call("bruit", "{}"), Err(PluginError::BadOutput(_))));
}

#[test]
fn every_call_starts_from_a_fresh_instance_so_nothing_leaks_between_callers() {
    let h = host_with("compteur", COUNTER, Limits::default()).unwrap();
    for _ in 0..3 {
        assert_eq!(h.call("compteur", "{}").unwrap(), "1", "l'état du module est neuf à chaque appel");
    }
}

#[test]
fn names_are_plain_identifiers_and_a_bad_file_does_not_stop_the_others() {
    for ok in ["resonance", "g1-billet", "a_b", "x1"] {
        assert!(valid_name(ok), "{ok}");
    }
    for bad in ["", "../x", "Majuscule", "a b", "é", &"a".repeat(33)] {
        assert!(!valid_name(bad), "{bad}");
    }
    let dir = std::env::temp_dir().join(format!("astro-plugins-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(plugins_dir().join("resonance.wasm"), dir.join("resonance.wasm")).unwrap();
    std::fs::write(dir.join("casse.wasm"), b"\0asm mais faux").unwrap();
    std::fs::write(dir.join("Mauvais Nom.wasm"), COUNTER).unwrap();
    std::fs::write(dir.join("notes.txt"), "ignoré").unwrap();
    let (host, report) = PluginHost::load_dir(&dir, Limits::default()).unwrap();
    assert_eq!(report.loaded, ["resonance"]);
    assert_eq!(report.rejected.len(), 2, "{:?}", report.rejected);
    assert!(report.rejected.iter().any(|(p, why)| p.ends_with("Mauvais Nom.wasm") && why.contains("nom de plugin invalide")));
    assert_eq!(host.list().len(), 1);
    assert!(PluginHost::load_dir(&dir.join("absent"), Limits::default()).is_err());
    let _ = std::fs::remove_dir_all(dir);
}
