//! Liens d'invité « magiques » : `https://station/s/<slug>#<clé>`.
//!
//! La clé AES-256 du lien est dans le **fragment** (`#…`) : un navigateur ne l'envoie jamais au serveur. Le serveur ne
//! détient que des blobs UENC v2 chiffrés avec cette clé (fichiers *et* manifeste : noms et types restent privés) ;
//! la page déchiffre dans le navigateur avec WebCrypto. Les blobs du lien sont des copies ré-encrypées en AES-256-GCM
//! avec la clé du lien (les clés de fichier du propriétaire ne sortent jamais), donc révocables sans rien changer d'autre.

use std::collections::BTreeSet;
use std::sync::Arc;

use astro_crypto::UencKey;
use astro_crypto::uenc::{self, Cipher};
use astro_db::{Db, DbError};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::blob::{BlobError, BlobStore, BlobWriter, is_cid};
use crate::server::{DavState, parse_range};
use crate::vfs::{self, EntryKind, VfsError};

const SCOPE: &str = "guest_share";
/// Fichiers au plus par lien.
pub const MAX_ITEMS: usize = 200;

/// Erreurs de partage.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    /// Chemin absent ou dossier vide.
    #[error("rien à partager : {0}")]
    Nothing(String),
    /// Trop de fichiers.
    #[error("trop de fichiers (max {MAX_ITEMS})")]
    TooMany,
    /// Base.
    #[error("base : {0}")]
    Db(#[from] DbError),
    /// VFS.
    #[error("vfs : {0}")]
    Vfs(#[from] VfsError),
    /// Blob.
    #[error("blob : {0}")]
    Blob(#[from] BlobError),
    /// Chiffrement.
    #[error("chiffrement : {0}")]
    Uenc(#[from] uenc::UencError),
    /// Autre.
    #[error("{0}")]
    Other(String),
}

/// Lien créé : à communiquer sous la forme `https://hôte{path}#{key_hex}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestLink {
    /// Identifiant public du lien.
    pub slug: String,
    /// Clé du lien (64 hex) : à placer dans le fragment, jamais envoyée au serveur.
    pub key_hex: String,
    /// Chemin de la page (`/s/<slug>`).
    pub path: String,
    /// Expiration, secondes UNIX.
    pub expires_at: i64,
    /// Nombre de fichiers.
    pub items: usize,
}

#[derive(Serialize, Deserialize)]
struct Record {
    owner: String,
    manifest_cid: String,
    cids: Vec<String>,
    expires_at: i64,
}

#[derive(Serialize)]
struct Item {
    name: String,
    mime: String,
    size: u64,
    cid: String,
}

async fn kv_get(db: &Db, key: &str) -> Result<Option<String>, DbError> {
    let key = key.to_owned();
    db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, key], |r| r.get(0)))?)).await
}

async fn encrypt_blob<B: BlobStore>(blobs: &B, key: &UencKey, mut input: impl tokio::io::AsyncRead + Unpin) -> Result<(String, u64), ShareError> {
    let mut w = blobs.begin().await?;
    uenc::encrypt_stream(key, Cipher::Aes256Gcm, &mut input, &mut w).await?;
    let meta = w.commit().await?;
    Ok((meta.cid, meta.size))
}

/// Crée un lien d'invité pour `paths` (fichiers, ou dossiers parcourus récursivement) valable `ttl_secs`.
pub async fn create_guest_link<B: BlobStore>(db: &Db, blobs: &B, owner: &str, paths: &[String], title: &str, ttl_secs: i64, now: i64) -> Result<GuestLink, ShareError> {
    // 1. Fichiers sélectionnés, avec leur nom relatif.
    let mut files: Vec<(String, vfs::Entry)> = Vec::new();
    for p in paths {
        let path = vfs::normalize_path(p)?;
        let (o, pp) = (owner.to_owned(), path.clone());
        let found = db
            .readers()
            .read(move |c| {
                let root = vfs::get_entry(c, &o, &pp).map_err(|_| DbError::Aborted)?;
                let list = match &root {
                    Some(e) if e.kind == EntryKind::Dir => vfs::list_subtree(c, &o, &pp, MAX_ITEMS).map_err(|_| DbError::Aborted)?,
                    Some(e) => vec![e.clone()],
                    None => Vec::new(),
                };
                Ok(list)
            })
            .await?;
        if found.is_empty() {
            return Err(ShareError::Nothing(path));
        }
        let base = vfs::parent_of(&path);
        for e in found.into_iter().filter(|e| e.kind == EntryKind::File) {
            let rel = e.path.strip_prefix(&base).unwrap_or(&e.path).trim_start_matches('/').to_owned();
            files.push((rel, e));
        }
    }
    if files.is_empty() {
        return Err(ShareError::Nothing("aucun fichier".into()));
    }
    if files.len() > MAX_ITEMS {
        return Err(ShareError::TooMany);
    }

    let originals: Vec<String> = files.iter().filter_map(|(_, e)| e.cid.clone()).collect();

    // 2. Clé du lien ; chaque fichier est déchiffré (clé du propriétaire) puis rechiffré (clé du lien) en flux.
    let link_key = UencKey::generate()?;
    let mut items = Vec::new();
    let mut cids = Vec::new();
    for (name, e) in &files {
        let cid = e.cid.clone().ok_or_else(|| ShareError::Other("entrée sans blob".into()))?;
        let (o, c) = (owner.to_owned(), cid.clone());
        let key_hex: String = db
            .readers()
            .read(move |conn| Ok(conn.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", rusqlite::params![o, c], |r| r.get(0))?))
            .await?;
        let file_key = UencKey::from_hex(&key_hex)?;
        let (mut src, _) = blobs.open(&cid).await?;
        let (mut pipe_w, pipe_r) = tokio::io::duplex(128 * 1024);
        let decrypt = async move { uenc::decrypt_stream(&file_key, &mut src, &mut pipe_w).await };
        let (dec, enc) = tokio::join!(decrypt, encrypt_blob(blobs, &link_key, pipe_r));
        dec?;
        let (new_cid, _) = enc?;
        cids.push(new_cid.clone());
        items.push(Item { name: name.clone(), mime: e.mime.clone().unwrap_or_else(|| vfs::guess_mime(name).to_owned()), size: e.size, cid: new_cid });
    }
    let manifest = serde_json::json!({ "v": 1, "title": title, "items": items }).to_string();
    let (manifest_cid, _) = encrypt_blob(blobs, &link_key, manifest.as_bytes()).await?;

    // 3. Enregistrement : le serveur ne connaît que des CID et une expiration.
    let mut rnd = [0u8; 16];
    getrandom::fill(&mut rnd).map_err(|e| ShareError::Other(e.to_string()))?;
    let slug = hex::encode(rnd);
    let expires_at = now + ttl_secs;
    let record = serde_json::to_string(&Record { owner: owner.to_owned(), manifest_cid, cids, expires_at }).map_err(|e| ShareError::Other(e.to_string()))?;
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)",
            vec![SCOPE.to_owned().into(), slug.clone().into(), record.into(), now.into()],
        )
        .await?;
    crate::adopt::mark_shared(db, owner, originals, "link").await;
    Ok(GuestLink { path: format!("/s/{slug}"), slug, key_hex: link_key.to_hex(), expires_at, items: items.len() })
}

/// Révoque un lien : l'enregistrement et tous ses blobs sont supprimés.
pub async fn revoke_guest_link<B: BlobStore>(db: &Db, blobs: &B, owner: &str, slug: &str) -> Result<bool, ShareError> {
    let Some(raw) = kv_get(db, slug).await? else { return Ok(false) };
    let rec: Record = serde_json::from_str(&raw).map_err(|e| ShareError::Other(e.to_string()))?;
    if rec.owner != owner {
        return Ok(false);
    }
    drop_record(db, blobs, slug, &rec).await?;
    Ok(true)
}

async fn drop_record<B: BlobStore>(db: &Db, blobs: &B, slug: &str, rec: &Record) -> Result<(), ShareError> {
    db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), slug.to_owned().into()]).await?;
    for cid in rec.cids.iter().chain(std::iter::once(&rec.manifest_cid)) {
        blobs.remove(cid).await?;
    }
    Ok(())
}

/// Supprime les liens expirés ; retourne leur nombre.
pub async fn purge_expired<B: BlobStore>(db: &Db, blobs: &B, now: i64) -> Result<usize, ShareError> {
    let rows: Vec<(String, String)> = db
        .readers()
        .read(|c| {
            let mut s = c.prepare("SELECT key, value FROM kv WHERE scope = ?1")?;
            Ok(s.query_map([SCOPE], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)
        })
        .await?;
    let mut n = 0;
    for (slug, raw) in rows {
        if let Ok(rec) = serde_json::from_str::<Record>(&raw) {
            if rec.expires_at <= now {
                drop_record(db, blobs, &slug, &rec).await?;
                n += 1;
            }
        }
    }
    Ok(n)
}

async fn active<B: BlobStore>(st: &DavState<B>, slug: &str) -> Option<Record> {
    if slug.len() != 32 || !slug.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rec: Record = serde_json::from_str(&kv_get(&st.db, slug).await.ok()??).ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    (rec.expires_at > now).then_some(rec)
}

fn private_headers(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
}

/// Page de l'invité : HTML statique, la clé est lue dans `location.hash` par le navigateur.
pub async fn page<B: BlobStore>(State(st): State<Arc<DavState<B>>>, Path(slug): Path<String>) -> Response {
    if active(&st, &slug).await.is_none() {
        return (StatusCode::NOT_FOUND, "lien expiré ou inconnu").into_response();
    }
    let mut r = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], guest_page_html()).into_response();
    r.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src blob:; media-src blob:; connect-src 'self'; base-uri 'none'; form-action 'none'"),
    );
    private_headers(&mut r);
    r
}

/// Manifeste chiffré du lien.
pub async fn manifest<B: BlobStore>(State(st): State<Arc<DavState<B>>>, Path(slug): Path<String>, headers: HeaderMap) -> Response {
    let Some(rec) = active(&st, &slug).await else { return StatusCode::NOT_FOUND.into_response() };
    serve_blob(&st, &rec.manifest_cid, &headers).await
}

/// Blob chiffré d'un fichier du lien (`Range` supporté) ; seuls les blobs du lien sont servis.
pub async fn blob<B: BlobStore>(State(st): State<Arc<DavState<B>>>, Path((slug, cid)): Path<(String, String)>, headers: HeaderMap) -> Response {
    let Some(rec) = active(&st, &slug).await else { return StatusCode::NOT_FOUND.into_response() };
    if !is_cid(&cid) || !rec.cids.contains(&cid) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_blob(&st, &cid, &headers).await
}

async fn serve_blob<B: BlobStore>(st: &DavState<B>, cid: &str, headers: &HeaderMap) -> Response {
    let Ok((mut reader, len)) = st.blobs.open(cid).await else { return StatusCode::NOT_FOUND.into_response() };
    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(|v| parse_range(v, len)) {
        Some(Err(())) => return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response(),
        Some(Ok(r)) => r,
        None => None,
    };
    let (status, start, count) = match range {
        Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        None => (StatusCode::OK, 0, len),
    };
    if reader.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut b = Response::builder().status(status).header(header::CONTENT_TYPE, "application/octet-stream").header(header::CONTENT_LENGTH, count).header(header::ACCEPT_RANGES, "bytes");
    if let Some((s, e)) = range {
        b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{len}"));
    }
    let mut r = b.body(Body::from_stream(ReaderStream::new(reader.take(count)))).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    private_headers(&mut r);
    r
}

/// Fonctions de déchiffrement (sans DOM : testables sous Node) : UENC v2 AES-256-GCM par WebCrypto.
pub const GUEST_LIB_JS: &str = r#"
function hexToBytes(h){ if(!/^[0-9a-f]{64}$/i.test(h)) throw new Error('clé invalide'); const o=new Uint8Array(32); for(let i=0;i<32;i++) o[i]=parseInt(h.substr(i*2,2),16); return o; }
async function importKey(hex){ return crypto.subtle.importKey('raw', hexToBytes(hex), 'AES-GCM', false, ['decrypt']); }
async function decryptUenc2(key, buf){
  const b=new Uint8Array(buf);
  if(b.length<14+16 || b[0]!==0x55||b[1]!==0x45||b[2]!==0x4e||b[3]!==0x43) throw new Error('pas un blob UENC');
  if(b[4]!==2) throw new Error('version UENC non supportée');
  if(b[5]!==1) throw new Error('chiffrement non supporté par ce navigateur');
  if(b[6]<12||b[6]>24) throw new Error('taille de chunk invalide');
  const cs=1<<b[6], ct=cs+16, aad=b.slice(0,14), prefix=b.slice(7,14), parts=[]; let total=0, i=0;
  for(let off=14; off<b.length; off+=ct, i++){
    const end=Math.min(off+ct,b.length), last=end>=b.length;
    const iv=new Uint8Array(12); iv.set(prefix,0); new DataView(iv.buffer).setUint32(7,i,false); iv[11]=last?1:0;
    const plain=new Uint8Array(await crypto.subtle.decrypt({name:'AES-GCM', iv, additionalData:aad}, key, b.subarray(off,end)));
    parts.push(plain); total+=plain.length;
  }
  if(i===0) throw new Error('blob tronqué');
  const out=new Uint8Array(total); let p=0; for(const x of parts){ out.set(x,p); p+=x.length; }
  return out;
}
"#;

const GUEST_APP_JS: &str = r#"
const $=(id)=>document.getElementById(id);
function fail(m){ $('status').textContent=m; $('status').className='err'; }
async function get(url){ const r=await fetch(url,{credentials:'omit',referrerPolicy:'no-referrer'}); if(!r.ok) throw new Error('Lien expiré ou introuvable'); return r.arrayBuffer(); }
function fmt(n){ return n<1024?n+' o':n<1048576?(n/1024).toFixed(0)+' Ko':(n/1048576).toFixed(1)+' Mo'; }
(async()=>{
  try{
    const slug=location.pathname.split('/')[2], keyHex=location.hash.slice(1);
    if(!keyHex){ fail('Ce lien est incomplet : la clé de déchiffrement (après le #) est absente.'); return; }
    const key=await importKey(keyHex);
    const m=JSON.parse(new TextDecoder().decode(await decryptUenc2(key, await get('/s/'+slug+'/m'))));
    document.title=m.title||'Album partagé'; $('title').textContent=m.title||'Album partagé'; $('status').textContent='';
    for(const it of m.items){
      const card=document.createElement('figure'); const cap=document.createElement('figcaption');
      cap.textContent=it.name+' · '+fmt(it.size); card.appendChild(cap); $('items').appendChild(card);
      get('/s/'+slug+'/b/'+it.cid).then(async(buf)=>{
        const url=URL.createObjectURL(new Blob([await decryptUenc2(key,buf)],{type:it.mime}));
        let el;
        if(it.mime.startsWith('image/')){ el=document.createElement('img'); el.src=url; el.alt=it.name; }
        else if(it.mime.startsWith('video/')){ el=document.createElement('video'); el.src=url; el.controls=true; }
        else if(it.mime.startsWith('audio/')){ el=document.createElement('audio'); el.src=url; el.controls=true; }
        else { el=document.createElement('a'); el.href=url; el.download=it.name; el.textContent='Télécharger'; }
        card.insertBefore(el,cap);
      }).catch((e)=>{ cap.textContent=it.name+' — '+e.message; });
    }
  }catch(e){ fail('Impossible d\'ouvrir cet album : '+e.message); }
})();
"#;

/// Page HTML de l'invité.
pub fn guest_page_html() -> String {
    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"referrer\" content=\"no-referrer\"><title>Album partagé</title>\
<style>body{{font-family:system-ui,sans-serif;margin:0;background:#101418;color:#e8edf2}}main{{max-width:1100px;margin:0 auto;padding:1.5rem}}h1{{font-weight:600}}\
#items{{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:1rem}}figure{{margin:0;background:#1a2027;border-radius:12px;overflow:hidden}}\
img,video{{width:100%;display:block}}figcaption{{padding:.6rem .8rem;font-size:.85rem;color:#9fb0c0}}a{{color:#7cc4ff}}.err{{color:#ff8a8a}}footer{{margin-top:2rem;font-size:.85rem;color:#9fb0c0}}</style></head>\
<body><main><h1 id=\"title\">Album partagé</h1><p id=\"status\">Déchiffrement dans votre navigateur…</p><div id=\"items\"></div>\
<footer>Chiffré de bout en bout : la clé ne quitte jamais votre navigateur. Album hébergé souverainement avec Astroport.</footer></main>\
<script>{GUEST_LIB_JS}{GUEST_APP_JS}</script></body></html>"
    )
}

/// Ensemble des CID d'un lien (utile aux tests).
pub async fn link_cids(db: &Db, slug: &str) -> BTreeSet<String> {
    let Ok(Some(raw)) = kv_get(db, slug).await else { return BTreeSet::new() };
    serde_json::from_str::<Record>(&raw).map(|r| r.cids.into_iter().chain(std::iter::once(r.manifest_cid)).collect()).unwrap_or_default()
}
