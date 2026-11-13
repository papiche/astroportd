//! Économie coopérative : vision de la constellation, portefeuilles, charte, Ğ1Billet, hébergement.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « coop ».
pub struct CoopBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    pub(super) coop: Option<(CoopGetFn, CoopSetFn)>,
    pub(super) constellation: Option<ConstellationFn>,
    pub(super) charter: Option<CharterFn>,
    pub(super) billet: Option<BilletFn>,
    pub(super) zen_credit: Option<BilletFn>,
    pub(super) stations: Option<Arc<crate::beacon::Stations>>,
    pub(super) oc: Option<Arc<crate::oc_member::OcMember>>,
}

impl_auth!(CoopBackend);

impl<B: BlobStore> CoopBackend<B> {
    /// Appel d'une opération de billet au nom d'un compte authentifié.
    async fn billet_call(&self, op: &'static str, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let f = self.billet.as_ref().ok_or_else(|| ApiError::unavailable("Ğ1Billets non activés sur cette station"))?;
        f(None, op, who_body(who, body)).await.map_err(billet_api_error)
    }
}

impl<B: BlobStore> CoopApi for CoopBackend<B> {
    async fn zen_credit(&self, who: &Principal, op: &'static str, body: Value) -> Result<Value, ApiError> {
        let f = self.zen_credit.as_ref().ok_or_else(|| ApiError::unavailable("versement de Ẑen non activé sur cette station"))?;
        f(None, op, who_body(who, body)).await.map_err(billet_api_error)
    }
    async fn check_balance(&self, g1pub: &str) -> Result<Value, ApiError> {
        let b = self.services.balance_of_address(g1pub).await.map_err(|e| if e.starts_with("adresse") { ApiError::bad_request(e) } else if e.starts_with("trop de lectures") { ApiError::too_many(e) } else { ApiError::unavailable(e) })?;
        let dec = |cents: u64| format!("{}.{:02}", cents / 100, cents % 100);
        // Montants en entiers de centimes (`*_cents`) ; les chaînes décimales servent aux clients historiques, qui lisent `balance`.
        Ok(json!({ "g1pub": g1pub, "balance": dec(b.g1_cents), "g1_cents": b.g1_cents, "zen": dec(b.zen_cents), "zen_cents": b.zen_cents }))
    }
    async fn account_oc(&self, who: &Principal) -> Result<Value, ApiError> {
        let oc = self.oc.as_ref().ok_or_else(|| ApiError::unavailable("vérification OpenCollective non activée sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'adresse e-mail"))?;
        Ok(oc.check(email).await)
    }
    async fn uplanet_hex(&self, g1: bool) -> Result<String, ApiError> {
        let keys = self.stations.as_ref().ok_or_else(|| ApiError::unavailable("clé de l'UPlanet non disponible"))?.public_keys().await.map_err(ApiError::internal)?;
        Ok(if g1 { keys.g1hex.clone() } else { keys.hex.clone() })
    }
    async fn coop_config(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (get, _) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        get().await.map_err(|e| {
            eprintln!("interface : configuration coopérative : {e}");
            ApiError::internal("configuration coopérative illisible")
        })
    }
    async fn coop_config_set(&self, who: &Principal, patch: std::collections::BTreeMap<String, Option<String>>) -> Result<Value, ApiError> {
        let (_, set) = self.coop.as_ref().ok_or_else(|| ApiError::unavailable("configuration coopérative non activée"))?;
        let keys: Vec<String> = patch.keys().cloned().collect();
        let view = set(patch).await.map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:coop", format!("configuration coopérative modifiée : {}", keys.join(", ")), None);
        let _ = who;
        Ok(view)
    }
    async fn cooperative_view(&self, who: &Principal, ledger: Option<(u64, usize)>) -> Result<Value, ApiError> {
        let f = self.constellation.as_ref().ok_or_else(|| ApiError::unavailable("vision de la constellation non activée"))?;
        f(who.hex.clone(), ledger).await.map_err(|e| {
            eprintln!("interface : constellation : {e}");
            ApiError::internal("constellation indisponible")
        })
    }
    async fn charter_accept(&self, who: &Principal, sha256: &str) -> Result<Value, ApiError> {
        let f = self.charter.as_ref().ok_or_else(|| ApiError::unavailable("acceptation de la charte non activée"))?;
        let out = f(who.hex.clone(), sha256.to_owned()).await.map_err(ApiError::bad_request)?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "system", "ui:charter", format!("charte {} acceptée par {}", out["version"].as_str().unwrap_or("?"), who.email.as_deref().unwrap_or("le Capitaine")), None);
        Ok(out)
    }
    async fn billet(&self, ip: Option<std::net::IpAddr>, op: &'static str, body: Value) -> Result<Value, ApiError> {
        let f = self.billet.as_ref().ok_or_else(|| ApiError::unavailable("Ğ1Billets non activés sur cette station"))?;
        f(ip, op, body).await.map_err(billet_api_error)
    }
    async fn account_billet_issuer(&self, who: &Principal, hex: String, allowed: bool) -> Result<Value, ApiError> {
        let r = self.billet_call("set_issuer", who, json!({ "hex": hex, "allowed": allowed })).await?;
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "billet", format!("Création de Ğ1Billets {} pour {}", if allowed { "autorisée" } else { "retirée" }, hex.get(..8).unwrap_or(&hex)), None);
        Ok(r)
    }
    async fn billet_issue(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.billet_call("issue", who, body).await
    }
    async fn billet_fund(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.billet_call("fund", who, body).await
    }
    async fn billet_delete(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        self.billet_call("delete", who, body).await
    }
    async fn billet_issued(&self, who: &Principal) -> Result<Value, ApiError> {
        self.billet_call("issued", who, Value::Null).await
    }
    async fn constellation(&self, _who: &Principal) -> Result<Value, ApiError> {
        let svc = &self.services;
        let origin = match svc.origin {
            Some(o) => Some(o),
            None => svc
                .db
                .readers()
                .read(|c| Ok(c.query_row("SELECT lat, lon FROM identities WHERE lat IS NOT NULL AND lon IS NOT NULL ORDER BY (role = 'node') DESC, hosted DESC LIMIT 1", [], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))).ok()))
                .await
                .map_err(db_err)?,
        };
        // Les autres stations viennent de l'annuaire des balises (comme pour `astroportd route`), plus de `12345.json`.
        let rows: Vec<Value> = match &self.stations {
            Some(s) => s.list(u64::try_from(crate::private_backup::now()).unwrap_or(0)).await.map_err(ApiError::internal)?["stations"].as_array().cloned().unwrap_or_default(),
            None => Vec::new(),
        };
        let connected: Vec<String> = svc.p2p.as_ref().map(|h| h.connected_peers().iter().map(ToString::to_string).collect()).unwrap_or_default();
        let me = svc.info.peer_id.clone().unwrap_or_default();
        let others: Vec<&Value> = rows.iter().filter(|r| r["station"].as_str().is_some_and(|id| id != me)).collect();
        // Ce que chaque station connectée **nous** ouvre (sa politique de tunnels répond) : jamais ce qu'elle ne nous destine pas.
        let asked: Vec<Option<Vec<String>>> = match &svc.p2p {
            Some(h) => {
                // 12 stations connectées au plus (les ids sont déjà dans l'ordre de l'annuaire, la plus récente d'abord).
                let mut budget = 12;
                let ids: Vec<Option<String>> = others
                    .iter()
                    .map(|r| {
                        let id = r["station"].as_str().filter(|id| connected.iter().any(|c| c == id) && budget > 0)?;
                        budget -= 1;
                        Some(id.to_owned())
                    })
                    .collect();
                futures_util::future::join_all(ids.iter().map(|id| async move {
                    match id {
                        Some(id) => crate::tunnel_policy::ask_services(h, id).await.ok(),
                        None => None,
                    }
                }))
                .await
            }
            None => vec![None; others.len()],
        };
        let describe = |node_id: &str, power: f64, pos: Option<(f64, f64)>, services: Vec<String>, is_self: bool| {
            let distance = match (origin, pos) {
                (Some(o), Some(p)) => Some((haversine_km(o, p) * 10.0).round() / 10.0),
                _ => None,
            };
            json!({
                "node_id": node_id, "self": is_self, "power_score": (power * 10.0).round() / 10.0, "tier": astro_p2p::routing::Tier::of(power).label(),
                "distance_km": distance, "lat": pos.map(|p| p.0), "lon": pos.map(|p| p.1),
                "status": if is_self { "local" } else if connected.iter().any(|c| c == node_id) { "connected" } else { "known" },
                "services": services,
            })
        };
        let mut nodes = vec![describe(&me, svc.hardware.score(), origin, svc.exposed.clone(), true)];
        let mut rest: Vec<(&Value, Vec<String>)> = others.into_iter().zip(asked).map(|(r, a)| (r, a.unwrap_or_default())).collect();
        rest.sort_by(|a, b| b.0["power"].as_f64().unwrap_or(0.0).total_cmp(&a.0["power"].as_f64().unwrap_or(0.0)).then_with(|| a.0["station"].as_str().cmp(&b.0["station"].as_str())));
        nodes.extend(rest.iter().map(|(r, services)| describe(r["station"].as_str().unwrap_or_default(), r["power"].as_f64().unwrap_or(0.0), r["lat"].as_f64().zip(r["lon"].as_f64()), services.clone(), false)));
        Ok(json!({ "origin": origin.map(|o| json!({ "lat": o.0, "lon": o.1 })), "nodes": nodes }))
    }
    async fn fuel(&self, _who: &Principal) -> Result<Value, ApiError> {
        Ok(match crate::primal_init::last_status(&self.services.db).await {
            Some(mut st) => {
                st["enabled"] = json!(true);
                st
            }
            None => json!({ "enabled": false }),
        })
    }
    async fn economy(&self, _who: &Principal) -> Result<Value, ApiError> {
        let (latest, love, recent) = self
            .services
            .db
            .readers()
            .read(|c| {
                let latest: Option<(String, i64, String)> = c
                    .query_row("SELECT stream, at, data FROM journal WHERE stream IN ('economy.3x13', 'economy.drawdown', 'economy.volunteer') ORDER BY at DESC, id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok();
                let love: (i64, i64) = c.query_row(astro_economy::store::love_totals(), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut stmt = c.prepare("SELECT at, kind, amount_cents, detail FROM love_ledger ORDER BY at DESC, id DESC LIMIT 8")?;
                let recent = stmt.query_map([], |r| Ok(json!({ "at": r.get::<_, i64>(0)?, "kind": r.get::<_, Option<String>>(1)?, "amount_cents": r.get::<_, i64>(2)?, "detail": r.get::<_, Option<String>>(3)?.and_then(|d| serde_json::from_str::<Value>(&d).ok()) })))?.collect::<Result<Vec<_>, _>>()?;
                Ok((latest, love, recent))
            })
            .await
            .map_err(db_err)?;
        let cents = |v: &Value, k: &str| v[k].as_str().and_then(|s| s.parse::<Amount>().ok()).map(|a| a.0);
        let (kind, at, week, allocation, shares, gap) = match latest {
            Some((stream, at, data)) => {
                let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                match stream.as_str() {
                    "economy.3x13" => {
                        let (cash, rnd, assets) = (cents(&v, "cash").unwrap_or(0), cents(&v, "rnd").unwrap_or(0), cents(&v, "assets").unwrap_or(0));
                        let total = (cash + rnd + assets).max(1);
                        let share = |name: &str, c: u64| json!({ "name": name, "cents": c, "pct": (c as f64 * 1000.0 / total as f64).round() / 10.0 });
                        let alloc = json!({ "surplus_cents": cents(&v, "surplus"), "is_provision_cents": cents(&v, "is_provision"), "net_cents": cents(&v, "net"), "captain_bonus_cents": cents(&v, "captain_bonus") });
                        ("split", Some(at), v["week"].as_str().map(str::to_owned), Some(alloc), vec![share("Trésorerie", cash), share("R&D", rnd), share("Actifs", assets)], None)
                    }
                    "economy.drawdown" => ("drawdown", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "gap")),
                    _ => ("volunteer", Some(at), v["week"].as_str().map(str::to_owned), None, Vec::new(), cents(&v, "deficit")),
                }
            }
            None => ("none", None, None, None, Vec::new(), None),
        };
        Ok(json!({
            "kind": kind, "at": at, "week": week, "allocation": allocation, "shares": shares, "gap_cents": gap,
            "love": { "total_cents": love.0, "weeks": love.1, "recent": recent },
        }))
    }
    async fn account_hosting(&self, _who: &Principal, hex: String, quota_bytes: Option<u64>, weekly_zen: Option<u64>) -> Result<Value, ApiError> {
        let account = astro_dav::auth::lookup_by_hex(&self.services.db, &hex).await.ok_or_else(|| ApiError::not_found("compte non hébergé ici"))?;
        if account.tier == astro_dav::auth::Tier::Captain {
            return Err(ApiError::bad_request("le Capitaine n'a ni quota ni prix : seul le disque de la station le borne"));
        }
        if weekly_zen.is_some_and(|z| z > crate::hosting_fee::MAX_WEEKLY_ZEN) {
            return Err(ApiError::bad_request(format!("prix hebdomadaire au plus {} ẑ", crate::hosting_fee::MAX_WEEKLY_ZEN)));
        }
        let value: rusqlite::types::Value = quota_bytes.map_or(rusqlite::types::Value::Null, |q| rusqlite::types::Value::Integer(i64::try_from(q).unwrap_or(i64::MAX)));
        let w = self.services.db.writer();
        w.execute("UPDATE identities SET quota_bytes = ?2 WHERE hex = ?1 AND hosted = 1", vec![hex.clone().into(), value]).await.map_err(db_err)?;
        match weekly_zen {
            Some(z) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![crate::hosting_fee::PRICE_SCOPE.to_owned().into(), hex.clone().into(), z.to_string().into(), now_secs().into()]).await,
            None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![crate::hosting_fee::PRICE_SCOPE.to_owned().into(), hex.clone().into()]).await,
        }
        .map_err(db_err)?;
        let label = |q: Option<u64>, z: Option<u64>| format!("{} · {}", q.map_or_else(|| "quota par défaut".to_owned(), |q| format!("{} Go", q >> 30)), z.map_or_else(|| "prix par défaut".to_owned(), |z| format!("{z} ẑ/semaine")));
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Hébergement de {} : {}", hex.get(..8).unwrap_or(&hex), label(quota_bytes, weekly_zen)), None);
        Ok(json!({ "hex": hex, "quota_bytes": quota_bytes, "weekly_zen": weekly_zen }))
    }
    async fn hosting_defaults(&self, _who: &Principal) -> Result<Value, ApiError> {
        self.services
            .db
            .readers()
            .read(|c| {
                let get = |key: &str| -> rusqlite::Result<Option<u64>> { Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![crate::hosting_fee::DEFAULT_SCOPE, key], |r| r.get::<_, String>(0)).ok().and_then(|v| v.trim().parse().ok())) };
                Ok(json!({ "default_quota_bytes": get("default_quota")?, "default_weekly_zen": get("default_price")? }))
            })
            .await
            .map_err(db_err)
    }
    async fn set_hosting_defaults(&self, _who: &Principal, quota_bytes: Option<u64>, weekly_zen: Option<u64>) -> Result<Value, ApiError> {
        if weekly_zen.is_some_and(|z| z > crate::hosting_fee::MAX_WEEKLY_ZEN) {
            return Err(ApiError::bad_request(format!("prix hebdomadaire au plus {} ẑ", crate::hosting_fee::MAX_WEEKLY_ZEN)));
        }
        let w = self.services.db.writer();
        for (key, value) in [("default_quota", quota_bytes), ("default_price", weekly_zen)] {
            match value {
                Some(v) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![crate::hosting_fee::DEFAULT_SCOPE.to_owned().into(), key.to_owned().into(), v.to_string().into(), now_secs().into()]).await,
                None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![crate::hosting_fee::DEFAULT_SCOPE.to_owned().into(), key.to_owned().into()]).await,
            }
            .map_err(db_err)?;
        }
        self.services.activity.publish_to(astro_core::Audience::Captain, "quota", "quota", format!("Palier par défaut : {} · {}", quota_bytes.map_or_else(|| "quota de la formule".to_owned(), |q| format!("{} Go", q >> 30)), weekly_zen.map_or_else(|| "gratuit".to_owned(), |z| format!("{z} ẑ/semaine"))), None);
        Ok(json!({ "default_quota_bytes": quota_bytes, "default_weekly_zen": weekly_zen }))
    }
}
