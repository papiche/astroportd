//! Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;

/// État du domaine « social ».
pub struct SocialBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    /// Mémoire de BRO (avec `--ollama`).
    pub(super) memory: Option<Arc<BroMemory>>,
    pub(super) contacts: Option<Arc<crate::identity_links::IdentityLinks<B>>>,
    pub(super) agents: Option<Arc<crate::agents::Agents>>,
    pub(super) ai: Option<Arc<crate::member_ai::MemberAi>>,
    pub(super) book: Option<Arc<crate::address_book::AddressBook<B>>>,
    pub(super) chat: Option<Arc<crate::chat::Chat>>,
    /// Visio : port STUN de la station (UDP), secret du relais d'appel s'il est actif, serveurs supplémentaires.
    pub(super) ice: (Option<u16>, Option<[u8; 32]>, Vec<String>),
    pub(super) presence: Option<Arc<crate::presence::Presence>>,
    pub(super) people: Option<Arc<crate::person_keys::PersonKeys>>,
}

impl_auth!(SocialBackend);

impl<B: BlobStore> SocialBackend<B> {
    /// Ajoute à une fiche `keys` : les clés de la personne dans chaque UPlanet (emails de la fiche, clé déclarée, liens prouvés).
    async fn add_keys(&self, card: &mut Value) {
        let Some(people) = &self.people else { return };
        let emails: Vec<String> = card["fields"]["emails"].as_array().map(|a| a.iter().filter_map(|e| e["value"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let declared = card["pubkey"].as_str().map(str::to_owned);
        card["keys"] = json!(people.keys_for(&emails, declared.as_deref()).await);
        card["plane"] = json!(people.plane());
    }

    fn agents_service(&self) -> Result<&Arc<crate::agents::Agents>, ApiError> {
        self.agents.as_ref().ok_or_else(|| ApiError::unavailable("agents non disponibles sur cette station"))
    }

    fn contacts_service(&self) -> Result<&Arc<crate::identity_links::IdentityLinks<B>>, ApiError> {
        self.contacts.as_ref().ok_or_else(|| ApiError::unavailable("l'annuaire n'est pas disponible sur cette station"))
    }
}

impl<B: BlobStore> SocialApi for SocialBackend<B> {
    async fn agents_list(&self, who: &Principal) -> Result<Value, ApiError> {
        self.agents_service()?.list(&who.hex).await.map_err(agent_err)
    }
    async fn agents_enroll(&self, who: &Principal, name: String, group: String, max_secs: u64, totp: bool) -> Result<Value, ApiError> {
        self.agents_service()?.enroll(&who.hex, &name, &group, max_secs, totp).await.map_err(agent_err)
    }
    async fn agents_revoke(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.agents_service()?.revoke(&who.hex, &id).await.map_err(agent_err)
    }
    async fn agents_requests(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(self.agents_service()?.requests(&who.hex))
    }
    async fn agents_decide(&self, who: &Principal, id: String, approve: bool, secs: Option<u64>) -> Result<Value, ApiError> {
        self.agents_service()?.decide(&who.hex, &id, approve, secs).await.map_err(agent_err)
    }
    async fn mcp(&self, bearer: Option<String>, body: Value) -> Result<Option<Value>, ApiError> {
        self.agents_service()?.mcp(bearer.as_deref(), &body).await.map_err(agent_err)
    }
    async fn book(&self, who: &Principal, op: BookOp) -> Result<Value, ApiError> {
        let book = self.book.as_ref().ok_or_else(|| ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let fields = |v: Value| serde_json::from_value::<astro_dav::vcard::Fields>(v).map_err(|e| ApiError::bad_request(format!("champs de la fiche invalides : {e}")));
        let out = match op {
            BookOp::List(q, group) => book.list(o, &q, group.as_deref()).await,
            BookOp::Get(path) => match book.get(o, &path).await {
                Ok(mut card) => {
                    self.add_keys(&mut card).await;
                    Ok(card)
                }
                e => e,
            },
            BookOp::Save(path, f) => book.save(o, path.as_deref(), &fields(f)?).await,
            BookOp::Delete(path) => book.delete(o, &path).await,
            BookOp::DeleteMany(paths) => book.delete_many(o, &paths).await,
            BookOp::Photo(path) => book.photo(o, &path).await,
            BookOp::PhotoChoices(path) => book.photo_choices(o, &path).await,
            BookOp::SetPhoto(path, uri) => book.set_photo(o, &path, uri.as_deref()).await,
            BookOp::Take(path, keys) => book.take(o, &path, &keys).await,
            BookOp::Group(from, to) => book.set_group(o, &from, to.as_deref()).await,
            BookOp::Import(vcf) => book.import(o, &vcf).await,
            BookOp::Duplicates => book.duplicates(o).await,
            BookOp::Merge(keep, drop) => book.merge(o, &keep, &drop).await,
            BookOp::MergeExact(apply, limit) => book.merge_exact(o, apply, limit).await,
            BookOp::Me => book.me(o).await,
            BookOp::SaveMe(f) => book.save_me(o, &fields(f)?).await,
        };
        out.map_err(book_error)
    }
    async fn my_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let keys = people.class_of(&who.hex).await;
        Ok(json!({ "plane": people.plane(), "siblings": people.siblings().iter().map(|s| &s.name).collect::<Vec<_>>(), "keys": keys }))
    }
    async fn announce_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'email : rien à relier"))?;
        people.announce(&who.hex, email).await.map_err(ApiError::bad_request)
    }
    async fn presence(&self, who: &Principal, body: Option<Value>) -> Result<Value, ApiError> {
        let p = self.presence.as_ref().ok_or_else(|| ApiError::unavailable("la présence visio n'est pas disponible sur cette station"))?;
        if let Some(b) = body {
            if let Some(on) = b["reachable"].as_bool() {
                p.set_reachable(&who.hex, on).await.map_err(ApiError::unavailable)?;
            }
            if let Some(on) = b["in_call"].as_bool() {
                p.set_in_call(&who.hex, on).await;
            }
        }
        Ok(p.view(&who.hex).await)
    }
    async fn call_ice(&self, who: &Principal) -> Result<Value, ApiError> {
        // Les identifiants du relais sont fabriqués pour ce membre seul (l'API est authentifiée) ; ses amis N¹ reçoivent les leurs
        // dans l'offre d'appel.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let turn = self.ice.1.as_ref().map(|s| crate::turn::credentials(s, &who.hex[..16.min(who.hex.len())], now, crate::turn::CREDENTIAL_TTL)).map(|(u, c)| json!({ "username": u, "credential": c }));
        Ok(json!({ "stun_port": self.ice.0, "turn": turn, "servers": self.ice.2 }))
    }
    async fn chat(&self, who: &Principal, op: ChatOp) -> Result<Value, ApiError> {
        let chat = self.chat.as_ref().ok_or_else(|| ApiError::unavailable("le chat privé n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let out = match op {
            ChatOp::List => chat.conversations(o).await,
            ChatOp::Messages(peer, before) => chat.messages(o, &peer, before).await,
            ChatOp::Send(peer, text) => chat.send(o, &peer, &text).await,
            ChatOp::Read(peer) => chat.mark_read(o, &peer).await,
            ChatOp::Signal(sig) => chat.signal(o, sig["peer"].as_str().unwrap_or_default(), &sig).await,
        };
        out.map_err(|e| match e {
            crate::chat::ChatError::Invalid(m) => ApiError::bad_request(m),
            crate::chat::ChatError::NoKey => ApiError::unavailable("clé du compte indisponible sur cette station : envoi impossible"),
            crate::chat::ChatError::Unreachable(m) => ApiError::unavailable(m),
            crate::chat::ChatError::Db(m) => {
                eprintln!("interface : chat : {m}");
                ApiError::internal("opération impossible : voir les journaux de la station")
            }
        })
    }
    async fn ai_params(&self, who: &Principal) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.get(&who.hex).await.map(|p| p.view()).map_err(ai_error)
    }
    async fn set_ai_params(&self, who: &Principal, patch: Value) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.set(&who.hex, &patch).await.map(|p| p.view()).map_err(ai_error)
    }
    async fn contacts(&self, who: &Principal) -> Result<Value, ApiError> {
        let links = self.contacts_service()?;
        let mut view = links.list(&who.hex).await.map_err(link_error)?;
        let login = crate::account_login(&self.services.db, &who.hex).await;
        let base = self.dav.config.public_base_url.clone().unwrap_or_default();
        view["carddav"] = json!({ "url": format!("{}{}/addressbooks/{login}/contacts/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "server": format!("{}{}/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "login": login });
        Ok(view)
    }
    async fn contacts_sync(&self, who: &Principal) -> Result<Value, ApiError> {
        let r = self.contacts_service()?.sync_owner(&who.hex).await.map_err(link_error)?;
        Ok(json!({ "indexed": r.indexed, "removed": r.removed, "linked": r.linked.iter().map(|(n, _)| n).collect::<Vec<_>>() }))
    }
    async fn contacts_create(&self, who: &Principal, name: String, email: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.create_from_person(&who.hex, &name, email.as_deref(), false).await.map_err(link_error)
    }
    async fn contacts_link(&self, who: &Principal, path: String, target: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.link(&who.hex, &path, target.as_deref()).await.map_err(link_error)
    }
    /// Console de BRO : `#commande` exécute la commande (comme par message privé) ; un autre texte est noté dans le
    /// journal (emplacement 15) après avoir cherché les souvenirs proches. Pas de génération de réponse par un LLM.
    async fn bro_say(&self, who: &Principal, text: String) -> Result<Value, ApiError> {
        if let Some(command) = bus::parse(&text) {
            let out = bus::execute(self.services.as_ref(), &command, &who.hex).await;
            return Ok(json!({ "reply": out.unwrap_or_else(|e| format!("⚠️ {e}")) }));
        }
        let Some(memory) = &self.memory else { return Err(ApiError::unavailable("La mémoire de BRO n'est pas disponible : cette station n'a pas de moteur d'IA (Ollama).")) };
        let related = memory.recall(&who.hex, &text, 3, None).await.map_err(|e| ApiError::unavailable(format!("mémoire indisponible : {e}")))?;
        let meta = Meta { source: "console".into(), ..Meta::default() };
        let mut reply = match memory.remember(&who.hex, Slot::Journal, &text, meta).await {
            Ok(r) if r.mem_id.is_some() => "📝 Noté dans votre journal.".to_owned(),
            Ok(_) => "📝 Je le savais déjà.".to_owned(),
            Err(e @ astro_bro::BroError::Unstorable(_)) => format!("🔒 {e} : je ne le garde pas."),
            Err(e) => return Err(ApiError::unavailable(format!("mémoire indisponible : {e}"))),
        };
        if !related.is_empty() {
            reply.push_str("\nJe me souviens que :");
            for r in related {
                reply.push_str(&format!("\n• {}", r.memory.content));
            }
        }
        Ok(json!({ "reply": reply }))
    }
}
