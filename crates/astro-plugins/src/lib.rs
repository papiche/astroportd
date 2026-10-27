//! Runtime WebAssembly des extensions d'Astroport.
//!
//! Un plugin est un fichier `.wasm` déposé dans un dossier `plugins/`. Il est exécuté dans une **bulle** :
//!
//! - **aucune capacité** : pas de WASI, pas de fichier, pas de réseau, pas d'horloge, pas d'aléa, pas de fonction
//!   fournie par l'hôte. Un module qui *importe* quoi que ce soit est refusé au chargement, avec la liste de ce qu'il
//!   réclamait ;
//! - **bornes dures** : carburant (instructions) limité par appel, mémoire limitée (16 Mio par défaut), entrée et
//!   sortie plafonnées ; une boucle infinie ou une explosion mémoire finit en erreur, jamais en blocage du démon ;
//! - **sans état** : chaque appel instancie le module à neuf, rien ne survit d'un appel à l'autre ni ne fuit d'un
//!   appelant à l'autre ;
//! - **déterministe** : sans horloge ni aléa, même entrée = même sortie, donc rejouable et vérifiable.
//!
//! # Interface (ABI v1)
//!
//! Échange par JSON UTF-8 dans la mémoire linéaire du plugin. Le plugin exporte :
//!
//! | Export | Signature | Rôle |
//! |---|---|---|
//! | `memory` | mémoire | mémoire linéaire |
//! | `alloc` | `(len: i32) -> i32` | réserve `len` octets, rend leur adresse |
//! | `plugin_call` | `(ptr: i32, len: i32) -> i64` | traite le JSON en `[ptr, ptr+len)` ; rend `(adresse_sortie << 32) \| longueur_sortie` |
//! | `dealloc` *(facultatif)* | `(ptr: i32, len: i32)` | libère un bloc rendu par `alloc` |
//! | `plugin_manifest` *(facultatif)* | `() -> i64` | objet JSON brut `{"version","description"}` empaqueté comme ci-dessus |
//!
//! La sortie est une enveloppe : `{"ok": <valeur>}` ou `{"error": "<message>"}`. L'hôte la déballe en
//! `Result` ([`PluginHost::call`]).

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wasmtime::{Config, Engine, Instance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc};

/// Version de l'interface décrite ci-dessus.
pub const ABI_VERSION: u32 = 1;

/// Erreurs du runtime.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// Aucun plugin de ce nom.
    #[error("plugin « {0} » inconnu")]
    NotFound(String),
    /// Nom de plugin invalide.
    #[error("nom de plugin invalide « {0} » (1 à 32 caractères parmi a-z 0-9 _ -)")]
    BadName(String),
    /// Module illisible ou non conforme à l'ABI.
    #[error("module refusé : {0}")]
    Invalid(String),
    /// Le module réclame des capacités que la bulle ne donne pas.
    #[error("le plugin importe des fonctions interdites : {0}")]
    ForbiddenImports(String),
    /// Entrée ou sortie au-delà du plafond.
    #[error("{what} de {size} octets au-delà du plafond de {max}")]
    TooLarge {
        /// `entrée` ou `sortie`.
        what: &'static str,
        /// Taille constatée.
        size: usize,
        /// Plafond.
        max: usize,
    },
    /// Le plugin a épuisé son carburant (boucle infinie, calcul trop long).
    #[error("le plugin a dépassé son budget de calcul")]
    OutOfFuel,
    /// Le plugin a dépassé sa mémoire.
    #[error("le plugin a dépassé sa limite de mémoire")]
    MemoryLimit,
    /// Le plugin a planté (trappe WebAssembly).
    #[error("le plugin a planté : {0}")]
    Trap(String),
    /// Sortie qui n'est pas l'enveloppe JSON attendue.
    #[error("sortie du plugin invalide : {0}")]
    BadOutput(String),
    /// Erreur rendue par le plugin lui-même.
    #[error("{0}")]
    Plugin(String),
    /// Lecture d'un fichier.
    #[error("{0} : {1}")]
    Io(PathBuf, std::io::Error),
}

/// Bornes d'un appel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Instructions WebAssembly au plus (carburant).
    pub fuel: u64,
    /// Mémoire linéaire au plus, en octets.
    pub memory_bytes: usize,
    /// Taille maximale du JSON d'entrée.
    pub max_input: usize,
    /// Taille maximale du JSON de sortie.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { fuel: 50_000_000, memory_bytes: 16 << 20, max_input: 1 << 20, max_output: 1 << 20 }
    }
}

/// Description d'un plugin chargé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    /// Nom (le nom du fichier sans `.wasm`).
    pub name: String,
    /// Version déclarée par le plugin.
    pub version: String,
    /// Description déclarée par le plugin.
    pub description: String,
    /// Taille du module en octets.
    pub size: usize,
}

struct Loaded {
    module: Module,
    info: PluginInfo,
}

struct State {
    limits: StoreLimits,
}

/// Bilan du chargement d'un dossier.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Noms chargés.
    pub loaded: Vec<String>,
    /// Fichiers refusés, avec la raison.
    pub rejected: Vec<(PathBuf, String)>,
}

/// L'hôte de plugins.
pub struct PluginHost {
    engine: Engine,
    plugins: BTreeMap<String, Loaded>,
    limits: Limits,
}

/// Un nom de plugin est un identifiant simple : pas de chemin, pas de casse, pas d'espace.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn unpack(packed: i64) -> (u32, u32) {
    let v = packed as u64;
    ((v >> 32) as u32, v as u32)
}

impl PluginHost {
    /// Hôte vide avec les bornes `limits`.
    pub fn new(limits: Limits) -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_multi_memory(false);
        let engine = Engine::new(&config).map_err(|e| PluginError::Invalid(e.to_string()))?;
        Ok(Self { engine, plugins: BTreeMap::new(), limits })
    }

    /// Charge tous les `*.wasm` de `dir`. Un module refusé n'empêche pas les autres : il est listé dans le bilan.
    pub fn load_dir(dir: &Path, limits: Limits) -> Result<(Self, LoadReport), PluginError> {
        let mut host = Self::new(limits)?;
        let mut report = LoadReport::default();
        let entries = std::fs::read_dir(dir).map_err(|e| PluginError::Io(dir.to_owned(), e))?;
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wasm") && p.is_file()).collect();
        files.sort();
        for path in files {
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
            let result = std::fs::read(&path).map_err(|e| PluginError::Io(path.clone(), e)).and_then(|bytes| host.load_bytes(&name, &bytes));
            match result {
                Ok(()) => report.loaded.push(name),
                Err(e) => report.rejected.push((path, e.to_string())),
            }
        }
        Ok((host, report))
    }

    /// Charge (ou remplace) le plugin `name` depuis du binaire `.wasm` ou du texte `.wat`.
    pub fn load_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), PluginError> {
        if !valid_name(name) {
            return Err(PluginError::BadName(name.to_owned()));
        }
        let module = Module::new(&self.engine, bytes).map_err(|e| PluginError::Invalid(e.to_string()))?;
        let imports: Vec<String> = module.imports().map(|i| format!("{}.{}", i.module(), i.name())).collect();
        if !imports.is_empty() {
            return Err(PluginError::ForbiddenImports(imports.join(", ")));
        }
        let has = |n: &str| module.exports().any(|e| e.name() == n);
        for needed in ["memory", "alloc", "plugin_call"] {
            if !has(needed) {
                return Err(PluginError::Invalid(format!("export « {needed} » manquant (ABI v{ABI_VERSION})")));
            }
        }
        let has_manifest = has("plugin_manifest");
        let mut loaded = Loaded { module, info: PluginInfo { name: name.to_owned(), version: String::new(), description: String::new(), size: bytes.len() } };
        if has_manifest {
            if let Ok(Value::Object(m)) = self.invoke(&loaded, Invocation::Manifest) {
                loaded.info.version = m.get("version").and_then(Value::as_str).unwrap_or("").chars().take(32).collect();
                loaded.info.description = m.get("description").and_then(Value::as_str).unwrap_or("").chars().take(200).collect();
            }
        }
        self.plugins.insert(name.to_owned(), loaded);
        Ok(())
    }

    /// Plugins chargés, par nom.
    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins.values().map(|p| p.info.clone()).collect()
    }

    /// Appelle le plugin `name` avec `input_json` ; rend la valeur `ok` de l'enveloppe, en JSON.
    ///
    /// **Calcul synchrone** (borné par le carburant) : depuis un contexte asynchrone, passer par `spawn_blocking`.
    pub fn call(&self, name: &str, input_json: &str) -> Result<String, PluginError> {
        let plugin = self.plugins.get(name).ok_or_else(|| PluginError::NotFound(name.to_owned()))?;
        if input_json.len() > self.limits.max_input {
            return Err(PluginError::TooLarge { what: "entrée", size: input_json.len(), max: self.limits.max_input });
        }
        self.invoke(plugin, Invocation::Call(input_json.as_bytes())).map(|v| v.to_string())
    }

    /// Variante typée : entrée et sortie en [`Value`].
    pub fn call_value(&self, name: &str, input: &Value) -> Result<Value, PluginError> {
        let out = self.call(name, &input.to_string())?;
        serde_json::from_str(&out).map_err(|e| PluginError::BadOutput(e.to_string()))
    }

    fn invoke(&self, plugin: &Loaded, what: Invocation<'_>) -> Result<Value, PluginError> {
        let mut store = Store::new(&self.engine, State { limits: StoreLimitsBuilder::new().memory_size(self.limits.memory_bytes).instances(1).tables(4).table_elements(10_000).memories(1).trap_on_grow_failure(true).build() });
        store.limiter(|s| &mut s.limits);
        store.set_fuel(self.limits.fuel).map_err(|e| PluginError::Invalid(e.to_string()))?;
        let linker: Linker<State> = Linker::new(&self.engine); // vide : la bulle n'offre rien
        let instance = linker.instantiate(&mut store, &plugin.module).map_err(|e| classify(&e))?;
        let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| PluginError::Invalid("export « memory » absent".into()))?;

        let packed = match what {
            Invocation::Manifest => instance.get_typed_func::<(), i64>(&mut store, "plugin_manifest").map_err(|e| PluginError::Invalid(e.to_string()))?.call(&mut store, ()).map_err(|e| classify(&e))?,
            Invocation::Call(input) => {
                let alloc: TypedFunc<i32, i32> = instance.get_typed_func(&mut store, "alloc").map_err(|e| PluginError::Invalid(format!("alloc : {e}")))?;
                let call: TypedFunc<(i32, i32), i64> = instance.get_typed_func(&mut store, "plugin_call").map_err(|e| PluginError::Invalid(format!("plugin_call : {e}")))?;
                let len = i32::try_from(input.len()).map_err(|_| PluginError::TooLarge { what: "entrée", size: input.len(), max: i32::MAX as usize })?;
                let ptr = alloc.call(&mut store, len).map_err(|e| classify(&e))?;
                write(&memory, &mut store, ptr, input)?;
                call.call(&mut store, (ptr, len)).map_err(|e| classify(&e))?
            }
        };
        let (out_ptr, out_len) = unpack(packed);
        if out_len as usize > self.limits.max_output {
            return Err(PluginError::TooLarge { what: "sortie", size: out_len as usize, max: self.limits.max_output });
        }
        let bytes = read(&memory, &store, out_ptr, out_len as usize)?;
        release(&instance, &mut store, out_ptr, out_len);
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|e| PluginError::BadOutput(e.to_string()))?;
        if matches!(what, Invocation::Manifest) {
            return Ok(envelope); // le manifeste est un objet brut, sans enveloppe `ok`
        }
        match (envelope.get("ok"), envelope.get("error")) {
            (Some(v), None) => Ok(v.clone()),
            (None, Some(Value::String(msg))) => Err(PluginError::Plugin(msg.clone())),
            _ => Err(PluginError::BadOutput("enveloppe attendue : {\"ok\": …} ou {\"error\": \"…\"}".into())),
        }
    }
}

enum Invocation<'a> {
    Manifest,
    Call(&'a [u8]),
}

fn write(memory: &Memory, store: &mut Store<State>, ptr: i32, data: &[u8]) -> Result<(), PluginError> {
    let start = usize::try_from(ptr).map_err(|_| PluginError::Invalid("adresse d'entrée négative".into()))?;
    memory.write(store, start, data).map_err(|_| PluginError::Invalid("alloc a rendu une adresse hors de la mémoire".into()))
}

fn read(memory: &Memory, store: &Store<State>, ptr: u32, len: usize) -> Result<Vec<u8>, PluginError> {
    let data = memory.data(store);
    let start = ptr as usize;
    data.get(start..start.checked_add(len).ok_or_else(|| PluginError::BadOutput("longueur absurde".into()))?).map(<[u8]>::to_vec).ok_or_else(|| PluginError::BadOutput("sortie hors de la mémoire du plugin".into()))
}

fn release(instance: &Instance, store: &mut Store<State>, ptr: u32, len: u32) {
    if let Ok(dealloc) = instance.get_typed_func::<(i32, i32), ()>(&mut *store, "dealloc") {
        let _ = dealloc.call(&mut *store, (ptr as i32, len as i32));
    }
}

fn classify(e: &wasmtime::Error) -> PluginError {
    if let Some(trap) = e.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel => PluginError::OutOfFuel,
            other => PluginError::Trap(other.to_string()),
        };
    }
    let text = format!("{e:#}");
    if text.contains("memory") && (text.contains("limit") || text.contains("grow") || text.contains("exceed")) {
        return PluginError::MemoryLimit;
    }
    PluginError::Trap(text.lines().next().unwrap_or("").to_owned())
}
