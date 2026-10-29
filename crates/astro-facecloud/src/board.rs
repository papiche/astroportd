//! Données de la « table de tri » : les visages d'un titulaire avec leur position 2D, leurs groupes et leurs suggestions.
//!
//! - `x`, `y` : projection **PCA** 2D des embeddings 512D, normalisée dans [-1, 1] (la table s'en sert pour ranger les
//!   visages isolés les uns près des autres) ;
//! - `group_id` : visages **non identifiés** proches entre eux, par plus proches voisins **mutuels** (un lien i–j n'existe
//!   que si chacun est parmi les `k` plus proches de l'autre ET si leur cosinus dépasse le seuil). Un simple seuil
//!   transitif enchaînerait A–B–C… en groupes géants mélangeant plusieurs personnes : un groupe trop grand est
//!   redécoupé avec un seuil relevé.
//!
//! Les vecteurs ne sortent jamais de la station : seules des coordonnées normalisées et des identifiants de groupe
//! sont rendus. Le calcul (le plus coûteux : quadratique en nombre de visages non identifiés) est mémoïsé par révision
//! du catalogue, jamais refait tant que rien n'a changé.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use astro_db::Db;
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::engine::DIM;
use crate::FaceError;

/// Voisins examinés par visage.
pub const GROUP_KNN: usize = 8;
/// Au-delà, un groupe est redécoupé avec un seuil plus strict.
pub const GROUP_MAX: usize = 120;
/// Cosinus minimal entre deux visages d'un même groupe (le bas de la zone « est-ce lui ? »).
pub const GROUP_MIN_SIM: f32 = 0.55;
/// Au-delà de ce nombre de visages non identifiés, les groupes ne sont pas calculés (coût quadratique).
pub const GROUP_CAP: usize = 8_000;
/// Lignes confrontées ensemble au calcul des voisins (tuile qui tient dans le cache).
const TILE: usize = 16;
/// Au-delà de ce nombre de visages, la projection n'est pas calculée.
pub const PCA_CAP: usize = 20_000;
/// Visages rendus au plus.
pub const BOARD_MAX: usize = 20_000;

/// Un visage sur la table.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardFace {
    /// Identifiant du visage.
    pub face_id: i64,
    /// Nom (`Inconnu_xxxxxxxx` tant qu'il n'est pas identifié).
    pub name: String,
    /// Clé Nostr de la personne, si elle en a une.
    pub pubkey: Option<String>,
    /// Le visage appartient à une personne (clé ou vrai nom).
    pub named: bool,
    /// Photo d'origine dans le uCloud.
    pub path: String,
    /// Cadre du visage `[x1, y1, x2, y2]` dans l'image d'origine.
    pub bbox: [f64; 4],
    /// Suggestion « est-ce X ? » (zone grise de reconnaissance).
    pub suggest: Option<(String, f64)>,
    /// Position PCA, dans [-1, 1].
    pub x: f32,
    /// Position PCA, dans [-1, 1].
    pub y: f32,
    /// Groupe de visages non identifiés proches (au moins 2), sinon `None`.
    pub group: Option<(String, usize)>,
}

/// Une personne (visages regroupés par clé, sinon par nom).
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    /// Nom affiché.
    pub name: String,
    /// Clé Nostr.
    pub pubkey: Option<String>,
    /// Visages.
    pub faces: usize,
    /// Photos distinctes.
    pub photos: usize,
}

/// La table, prête à être dessinée.
#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    /// Visages (hors références de contacts et visages confiés par des amis).
    pub faces: Vec<BoardFace>,
    /// Personnes reconnues, les plus photographiées d'abord.
    pub people: Vec<Person>,
    /// Visages non identifiés.
    pub unknown: usize,
    /// Les groupes n'ont pas été calculés (trop de visages non identifiés).
    pub groups_skipped: bool,
    /// Le catalogue dépasse [`BOARD_MAX`] : les plus récents seulement.
    pub truncated: bool,
}

/// Ce qui est coûteux à calculer, par visage.
#[derive(Debug, Clone, Default)]
struct Layout {
    xy: HashMap<i64, (f32, f32)>,
    group: HashMap<i64, (String, usize)>,
    skipped: bool,
}

/// Mémoïsation du calcul par titulaire et par révision du catalogue.
#[derive(Default)]
pub struct BoardCache {
    inner: Mutex<HashMap<String, (String, Arc<Layout>)>>,
}

fn unit(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

/// Produit scalaire à huit accumulateurs : le compilateur ne vectorise pas une somme flottante séquentielle (l'addition
/// n'est pas associative) ; huit sommes indépendantes tiennent dans un registre SIMD. Cinq à huit fois plus rapide.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca.remainder().iter().zip(cb.remainder()).map(|(x, y)| x * y).sum();
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// Groupes de visages probablement identiques parmi les lignes `idx` de `vecs` (n × `dim`, vecteurs unitaires).
pub fn mutual_knn_groups(vecs: &[f32], dim: usize, idx: &[usize], threshold: f32) -> Vec<Vec<usize>> {
    let n = idx.len();
    if n < 2 {
        return vec![idx.to_vec()];
    }
    let k = GROUP_KNN.min(n - 1);
    let row = |i: usize| &vecs[idx[i] * dim..(idx[i] + 1) * dim];
    // Les k meilleurs voisins de chacun, au-dessus du seuil. Calcul par tuiles de lignes : une tuile (16 visages, 32 Ko)
    // reste dans le cache pendant qu'on y confronte chaque autre visage, au lieu de relire toute la matrice par visage
    // (le calcul est limité par la mémoire, pas par l'arithmétique).
    let mut best: Vec<Vec<(f32, usize)>> = vec![Vec::with_capacity(k + 1); n];
    for i0 in (0..n).step_by(TILE) {
        let i1 = (i0 + TILE).min(n);
        for j in 0..n {
            let b = row(j);
            for (i, top) in best.iter_mut().enumerate().take(i1).skip(i0) {
                if i == j {
                    continue;
                }
                let s = dot(row(i), b);
                if s < threshold || (top.len() == k && s <= top[k - 1].0) {
                    continue;
                }
                let at = top.partition_point(|(t, _)| *t >= s);
                top.insert(at, (s, j));
                top.truncate(k);
            }
        }
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for i in 0..n {
        for &(_, j) in &best[i] {
            if j > i && best[j].iter().any(|&(_, back)| back == i) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &id) in idx.iter().enumerate() {
        let r = find(&mut parent, i);
        comps.entry(r).or_default().push(id);
    }
    let mut out = Vec::new();
    let mut comps: Vec<Vec<usize>> = comps.into_values().collect();
    comps.sort_by_key(|c| c[0]);
    for members in comps {
        if members.len() > GROUP_MAX && threshold < 0.8 {
            out.extend(mutual_knn_groups(vecs, dim, &members, threshold + 0.05));
        } else {
            out.push(members);
        }
    }
    out
}

/// Projection PCA 2D (itération de la puissance, sans former la matrice de covariance), normalisée dans [-1, 1].
pub fn pca_2d(vecs: &[f32], dim: usize, n: usize) -> Vec<(f32, f32)> {
    if n < 2 || dim == 0 {
        return vec![(0.0, 0.0); n];
    }
    let mut mean = vec![0f32; dim];
    for r in 0..n {
        for (m, x) in mean.iter_mut().zip(&vecs[r * dim..(r + 1) * dim]) {
            *m += x;
        }
    }
    mean.iter_mut().for_each(|m| *m /= n as f32);
    // Applique Xcᵀ·Xc à `v` : deux passes sur les données.
    let apply = |v: &[f32]| -> Vec<f32> {
        let mut out = vec![0f32; dim];
        for r in 0..n {
            let row = &vecs[r * dim..(r + 1) * dim];
            let t: f32 = row.iter().zip(&mean).zip(v).map(|((x, m), w)| (x - m) * w).sum();
            for ((o, x), m) in out.iter_mut().zip(row).zip(&mean) {
                *o += (x - m) * t;
            }
        }
        out
    };
    let mut axes: Vec<Vec<f32>> = Vec::new();
    for c in 0..2 {
        // Départ déterministe et non nul.
        let mut v: Vec<f32> = (0..dim).map(|i| (((i * 31 + c * 17 + 7) % 13) as f32 - 6.0) / 13.0 + 0.01).collect();
        unit(&mut v);
        for _ in 0..40 {
            let mut w = apply(&v);
            for a in &axes {
                let p = dot(&w, a);
                w.iter_mut().zip(a).for_each(|(x, y)| *x -= p * y);
            }
            let norm = w.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm < 1e-12 {
                break;
            }
            w.iter_mut().for_each(|x| *x /= norm);
            v = w;
        }
        axes.push(v);
    }
    let mut out: Vec<(f32, f32)> = (0..n)
        .map(|r| {
            let row = &vecs[r * dim..(r + 1) * dim];
            let proj = |a: &[f32]| row.iter().zip(&mean).zip(a).map(|((x, m), w)| (x - m) * w).sum::<f32>();
            (proj(&axes[0]), proj(&axes[1]))
        })
        .collect();
    let max = out.iter().flat_map(|(x, y)| [x.abs(), y.abs()]).fold(0f32, f32::max);
    if max > 0.0 {
        out.iter_mut().for_each(|(x, y)| (*x, *y) = (*x / max, *y / max));
    }
    out
}

fn named(name: &str, pubkey: Option<&str>) -> bool {
    pubkey.is_some_and(|p| !p.is_empty()) || !(name.is_empty() || name.starts_with("Inconnu"))
}

fn group_id(ids: &mut [i64]) -> String {
    ids.sort_unstable();
    let joined = ids.iter().map(i64::to_string).collect::<Vec<_>>().join("|");
    hex::encode(&Sha256::digest(joined.as_bytes())[..6])
}

/// Calcule position et groupes de `ids` (identifiants triés) à partir de leurs `vectors` (n × [`DIM`]).
fn layout(ids: &[i64], is_unnamed: &[bool], mut vectors: Vec<f32>) -> Layout {
    let n = ids.len();
    for r in 0..n {
        unit(&mut vectors[r * DIM..(r + 1) * DIM]);
    }
    let mut out = Layout::default();
    if n <= PCA_CAP {
        for (id, xy) in ids.iter().zip(pca_2d(&vectors, DIM, n)) {
            out.xy.insert(*id, xy);
        }
    }
    let unnamed: Vec<usize> = (0..n).filter(|i| is_unnamed[*i]).collect();
    if unnamed.len() > GROUP_CAP {
        out.skipped = true;
        return out;
    }
    for members in mutual_knn_groups(&vectors, DIM, &unnamed, GROUP_MIN_SIM) {
        if members.len() < 2 {
            continue;
        }
        let mut member_ids: Vec<i64> = members.iter().map(|i| ids[*i]).collect();
        let size = member_ids.len();
        let gid = group_id(&mut member_ids);
        for id in member_ids {
            out.group.insert(id, (gid.clone(), size));
        }
    }
    out
}

fn decode(blob: &[u8]) -> Option<Vec<f32>> {
    (blob.len() == DIM * 4).then(|| blob.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

impl BoardCache {
    /// Cache vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Oublie le calcul du titulaire (après une modification qu'on ne veut pas attendre).
    pub fn forget(&self, owner: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(owner);
    }

    /// La table de `owner`.
    pub async fn board(&self, db: &Db, owner: &str) -> Result<Board, FaceError> {
        let o = owner.to_owned();
        type Row = (i64, String, Option<String>, String, [f64; 4], Option<String>, Option<f64>);
        let (rows, total, revision): (Vec<Row>, i64, String) = db
            .readers()
            .read(move |c| {
                let (count, max_id, unnamed, named_sum): (i64, i64, i64, i64) = c.query_row(
                    "SELECT COUNT(*), COALESCE(MAX(face_id), 0), COALESCE(SUM(name LIKE 'Inconnu%' AND pubkey IS NULL), 0), COALESCE(SUM(face_id * (name NOT LIKE 'Inconnu%' OR pubkey IS NOT NULL)), 0) \
                     FROM faces WHERE owner_hex = ?1 AND ref IS NULL AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL",
                    [&o],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let mut stmt = c.prepare(
                    "SELECT face_id, name, pubkey, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2, suggest_name, suggest_score FROM faces \
                     WHERE owner_hex = ?1 AND ref IS NULL AND ucloud_path IS NOT NULL AND bbox_x1 IS NOT NULL ORDER BY face_id DESC LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(params![o, i64::try_from(BOARD_MAX).unwrap_or(i64::MAX)], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, [r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?], r.get(8)?, r.get(9)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((rows, count, format!("{count}:{max_id}:{unnamed}:{named_sum}")))
            })
            .await?;
        let truncated = usize::try_from(total).unwrap_or(0) > BOARD_MAX;
        let cached = self.inner.lock().unwrap_or_else(|e| e.into_inner()).get(owner).filter(|(rev, _)| *rev == revision).map(|(_, l)| l.clone());
        let layout = match cached {
            Some(l) => l,
            None => {
                let mut ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
                ids.sort_unstable();
                let unnamed: HashMap<i64, bool> = rows.iter().map(|r| (r.0, !named(&r.1, r.2.as_deref()))).collect();
                let o = owner.to_owned();
                let vectors: Vec<(i64, Vec<f32>)> = db
                    .readers()
                    .read(move |c| {
                        let mut stmt = c.prepare("SELECT face_id, embedding FROM face_vectors WHERE owner_hex = ?1")?;
                        Ok(stmt.query_map([&o], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?.filter_map(Result::ok).filter_map(|(id, b)| decode(&b).map(|v| (id, v))).collect())
                    })
                    .await?;
                let by_id: HashMap<i64, Vec<f32>> = vectors.into_iter().collect();
                // Seuls les visages qui ont un vecteur entrent dans le calcul (les autres restent au centre, sans groupe).
                let with: Vec<i64> = ids.iter().copied().filter(|i| by_id.contains_key(i)).collect();
                let flags: Vec<bool> = with.iter().map(|i| unnamed[i]).collect();
                let mut flat = Vec::with_capacity(with.len() * DIM);
                for i in &with {
                    flat.extend_from_slice(&by_id[i]);
                }
                let computed = Arc::new(tokio::task::spawn_blocking(move || layout(&with, &flags, flat)).await.map_err(|_| FaceError::Invalid("calcul de la table interrompu"))?);
                self.inner.lock().unwrap_or_else(|e| e.into_inner()).insert(owner.to_owned(), (revision, computed.clone()));
                computed
            }
        };
        let mut faces: Vec<BoardFace> = rows
            .into_iter()
            .map(|(face_id, name, pubkey, path, bbox, sname, sscore)| {
                let (x, y) = layout.xy.get(&face_id).copied().unwrap_or((0.0, 0.0));
                BoardFace {
                    face_id,
                    named: named(&name, pubkey.as_deref()),
                    group: layout.group.get(&face_id).cloned(),
                    suggest: sname.zip(sscore),
                    name,
                    pubkey,
                    path,
                    bbox,
                    x,
                    y,
                }
            })
            .collect();
        faces.sort_by_key(|f| f.face_id);
        let mut by_person: HashMap<String, (Person, std::collections::HashSet<String>)> = HashMap::new();
        for f in faces.iter().filter(|f| f.named) {
            let key = f.pubkey.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| format!("nom:{}", f.name));
            let e = by_person.entry(key).or_insert_with(|| (Person { name: f.name.clone(), pubkey: f.pubkey.clone(), faces: 0, photos: 0 }, Default::default()));
            e.0.faces += 1;
            e.1.insert(f.path.clone());
        }
        let mut people: Vec<Person> = by_person.into_values().map(|(mut p, photos)| {
            p.photos = photos.len();
            p
        }).collect();
        people.sort_by(|a, b| b.photos.cmp(&a.photos).then_with(|| a.name.cmp(&b.name)));
        let unknown = faces.iter().filter(|f| !f.named).count();
        Ok(Board { faces, people, unknown, groups_skipped: layout.skipped, truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vecteur unitaire de la dimension `axis`, légèrement incliné vers `tilt`.
    fn v(axis: usize, tilt: usize, eps: f32) -> Vec<f32> {
        let mut x = vec![0f32; DIM];
        x[axis] = 1.0;
        x[tilt] = eps;
        unit(&mut x);
        x
    }

    fn flat(rows: &[Vec<f32>]) -> Vec<f32> {
        rows.iter().flatten().copied().collect()
    }

    #[test]
    fn mutual_neighbours_group_the_same_person_and_keep_strangers_apart() {
        // Deux personnes (axes 0 et 1) vues sous quatre angles chacune, plus un visage isolé (axe 2).
        let mut rows = Vec::new();
        for t in 10..14 {
            rows.push(v(0, t, 0.2));
        }
        for t in 20..24 {
            rows.push(v(1, t, 0.2));
        }
        rows.push(v(2, 30, 0.0));
        let groups = mutual_knn_groups(&flat(&rows), DIM, &(0..rows.len()).collect::<Vec<_>>(), GROUP_MIN_SIM);
        let mut sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
        sizes.sort_unstable();
        assert_eq!(sizes, [1, 4, 4], "{groups:?}");
        assert!(groups.iter().any(|g| g.iter().all(|i| *i < 4)) && groups.iter().any(|g| g.iter().all(|i| (4..8).contains(i))));
    }

    #[test]
    fn a_one_way_neighbour_does_not_join_a_group() {
        // Vingt visages quasi identiques ; un visiteur dont les voisins sont ces vingt visages (cosinus ≈ 0,6 > seuil),
        // mais qui n'est dans le voisinage d'aucun d'eux (ils ont chacun huit voisins bien plus proches).
        let mut rows: Vec<Vec<f32>> = (10..30).map(|t| v(0, t, 0.05)).collect();
        let mut outsider = vec![0f32; DIM];
        outsider[0] = 0.6;
        outsider[100] = 0.8;
        rows.push(outsider);
        let groups = mutual_knn_groups(&flat(&rows), DIM, &(0..21).collect::<Vec<_>>(), GROUP_MIN_SIM);
        assert!(groups.iter().any(|g| g == &[20]), "le visiteur reste seul : un seuil transitif l'aurait absorbé : {groups:?}");
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 21, "personne n'est perdu ni dupliqué");
        // La réciprocité est stricte : un noyau serré peut laisser des visages isolés (c'est le comportement du legacy,
        // préféré à des groupes géants qui mélangent plusieurs personnes).
        assert!(groups.iter().all(|g| g.len() < 21));
    }

    #[test]
    fn an_oversized_group_is_split_with_a_stricter_threshold() {
        // 150 visages quasi identiques à l'angle près : un seul groupe à 0,55, redécoupé au seuil relevé (voisinage borné).
        let rows: Vec<Vec<f32>> = (0..150).map(|i| v(0, 1 + (i % 100), 0.05 + (i as f32) * 0.004)).collect();
        let groups = mutual_knn_groups(&flat(&rows), DIM, &(0..150).collect::<Vec<_>>(), GROUP_MIN_SIM);
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 150, "personne n'est perdu");
        assert!(groups.iter().all(|g| g.len() <= GROUP_MAX || g.len() == 150), "{:?}", groups.iter().map(Vec::len).collect::<Vec<_>>());
    }

    #[test]
    fn pca_separates_two_clusters_along_the_first_axis_and_stays_in_the_unit_square() {
        let mut rows = Vec::new();
        for i in 0..20 {
            rows.push(v(0, 5 + i % 3, 0.1));
        }
        for i in 0..20 {
            rows.push(v(1, 8 + i % 3, 0.1));
        }
        let xy = pca_2d(&flat(&rows), DIM, rows.len());
        assert!(xy.iter().all(|(x, y)| x.abs() <= 1.0 + 1e-5 && y.abs() <= 1.0 + 1e-5));
        let mean = |r: std::ops::Range<usize>| r.clone().map(|i| xy[i].0).sum::<f32>() / r.len() as f32;
        assert!((mean(0..20) - mean(20..40)).abs() > 0.8, "les deux nuages sont éloignés sur le premier axe : {} {}", mean(0..20), mean(20..40));
        assert_eq!(pca_2d(&[], DIM, 0), Vec::<(f32, f32)>::new());
        assert_eq!(pca_2d(&flat(&[v(0, 1, 0.1)]), DIM, 1), [(0.0, 0.0)]);
    }

    /// Mesure de passage à l'échelle : `cargo test -p astro-facecloud --release --lib bench_scaling -- --ignored --nocapture`.
    #[test]
    #[ignore = "mesure : à lancer à la main, en release"]
    fn bench_scaling() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        for n in [1_000usize, 2_000, 4_000, 8_000, 20_000] {
            // 5 % de « personnes » : des nuages de ~20 visages proches, le reste dispersé.
            let centers: Vec<Vec<f32>> = (0..n / 20).map(|_| (0..DIM).map(|_| rnd()).collect()).collect();
            let mut flat = Vec::with_capacity(n * DIM);
            for i in 0..n {
                let c = &centers[i % centers.len()];
                let mut v: Vec<f32> = c.iter().map(|x| x + 0.15 * rnd()).collect();
                unit(&mut v);
                flat.extend(v);
            }
            let t = std::time::Instant::now();
            let _ = pca_2d(&flat, DIM, n);
            let pca = t.elapsed();
            let (groups, knn) = if n <= GROUP_CAP {
                let t = std::time::Instant::now();
                let g = mutual_knn_groups(&flat, DIM, &(0..n).collect::<Vec<_>>(), GROUP_MIN_SIM);
                (g.iter().filter(|x| x.len() > 1).count(), t.elapsed())
            } else {
                (0, std::time::Duration::ZERO)
            };
            eprintln!("{n:>6} visages : PCA {pca:>10.1?} · groupes {knn:>10.1?} ({groups} groupes)");
        }
    }

    #[test]
    fn group_ids_depend_on_the_members_only() {
        assert_eq!(group_id(&mut [3, 1, 2]), group_id(&mut [1, 2, 3]));
        assert_ne!(group_id(&mut [1, 2]), group_id(&mut [1, 3]));
        assert_eq!(group_id(&mut [1, 2]).len(), 12);
    }

    #[test]
    fn a_person_needs_a_key_or_a_real_name() {
        assert!(named("Marie", None) && named("Inconnu_ab12", Some("ab".repeat(32).as_str())));
        assert!(!named("Inconnu_ab12", None) && !named("Inconnu", None) && !named("", None));
    }
}
