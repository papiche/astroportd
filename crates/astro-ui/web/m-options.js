'use strict';
// Options de la station (Capitaine) : moteur FaceID commutable à chaud et matrice du relai par kind. Rien ne redémarre :
// la station applique le réglage dès qu'il est enregistré, et le conserve.
const READ = { public: 'Tous', addressed: 'Auteur et destinataires', friends: 'Auteur et ses Proches', circle: 'Cercle social', deny: 'Personne' };
const WRITE = { circle: 'Cercle social', open: 'Tous (débit limité)', deny: 'Personne' };
const TMODE = { off: 'Fermé', listed: 'Seulement la liste', friends: 'Capitaines amis + liste', swarm: 'Tout le réseau' };
const TNAME = { ssh: 'SSH (accès à la machine)', ollama: 'Ollama (IA)', comfyui: 'ComfyUI (images)' };
const MODE = { off: 'Arrêté', local: 'ONNX local (cette machine)', remote: 'Brain GPU distant' };

export default async function options(root, K) {
  const { h, t } = K, api = p => K.api('/api/v1/station/admin/' + p), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const [face, kinds, tun, asst, tk] = await Promise.all([api('face'), api('kinds'), api('tunnels').catch(() => null), api('assist').catch(() => null), api('tasks').catch(() => null)]);
  const ok = (v, text) => h('li', {}, v ? '✔ ' : '✘ ', text);

  // ── Moteur FaceID ──
  const url = h('input', { id: 'fo-url', placeholder: 'http://brain.local:8190/faces', value: face.url || '', autocomplete: 'off', disabled: !!face.locked });
  const radios = Object.entries(MODE).map(([m, label]) => h('label', {}, h('input', { type: 'radio', name: 'fo-mode', value: m, checked: face.mode === m, disabled: !!face.locked }), ' ' + t(label)));
  const apply = h('button', { disabled: !!face.locked, onclick: async ev => {
    const mode = radios.map(r => r.firstChild).find(r => r.checked)?.value;
    ev.target.disabled = true;
    try { await post('face', { mode, url: url.value.trim() }); K.toast(t('Moteur FaceID appliqué'), 1); options(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, t('Appliquer'));
  const running = face.label ? t('En service : {0}', face.label) : t('Aucun moteur en service : les nouvelles photos attendent.');
  const faceCard = h('section', { class: 'card' }, h('h2', {}, t('Reconnaissance des visages')),
    h('p', { class: face.label ? '' : 'degraded', role: 'status' }, running),
    face.locked ? h('p', { class: 'muted' }, t('Imposé par {0} sur la ligne de commande : retirez l\'option pour le régler ici.', face.locked)) : null,
    h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Moteur')), radios, h('label', { for: 'fo-url' }, t('Adresse du Brain (mode distant)')), url),
    h('ul', { class: 'muted' }, ok(face.onnx_compiled, t('fonction ONNX compilée dans ce binaire')), ok(face.models, face.models ? t('modèles : {0}', face.models) : t('modèles introuvables (det_10g.onnx, w600k_r50.onnx)')), ok(face.library, face.library ? t('onnxruntime : {0}', face.library) : t('libonnxruntime introuvable (ORT_DYLIB_PATH)'))),
    h('p', { class: 'muted' }, t('{0} photo(s) en attente · {1} abandonnée(s).', face.pending, face.failed)), h('p', {}, apply));

  // ── Matrice des kinds ──
  const sel = (choices, labels, cur) => h('select', {}, choices.map(c => h('option', { value: c, selected: c === cur }, t(labels[c] || c))));
  const rows = kinds.kinds.map(k => {
    const read = sel(kinds.choices.read, READ, k.read), write = sel(kinds.choices.write, WRITE, k.write), proj = h('input', { type: 'checkbox', checked: k.project, 'aria-label': t('Projection du kind {0}', k.kind) });
    const spec = () => `read=${read.value} write=${write.value} project=${proj.checked ? 'on' : 'off'}`;
    return { k, read, write, proj, spec, changed: () => spec() !== `read=${k.read} write=${k.write} project=${k.project ? 'on' : 'off'}` };
  });
  const addNo = h('input', { type: 'number', min: 0, max: 65535, placeholder: 'kind', 'aria-label': t('Ajouter un kind') });
  const save = h('button', { onclick: async ev => {
    const rules = {};
    rows.filter(r => r.changed()).forEach(r => { rules[r.k.kind] = r.spec(); });
    if (addNo.value) rules[addNo.value] = 'read=circle write=circle project=on';
    if (!Object.keys(rules).length) return K.toast(t('Aucune modification'));
    ev.target.disabled = true;
    try { await post('kinds', { rules }); K.toast(t('Matrice du relai appliquée'), 1); options(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, t('Appliquer la matrice'));
  const kindsCard = h('section', { class: 'card wide' }, h('h2', {}, t('Relai : lecture, écriture, projection par kind')),
    h('p', { class: 'muted' }, t('Lecture : qui reçoit les événements. Écriture : qui peut en publier. Projection : l\'événement alimente-t-il les tables dérivées (comptes, ledger…) ; il reste stocké et servi. S\'applique tout de suite.')),
    h('table', {}, h('caption', { class: 'muted' }, t('Un kind sans réglage suit les défauts du relai.')), h('thead', {}, h('tr', {}, [t('Kind'), t('Lecture'), t('Écriture'), t('Projeté'), t('Origine')].map(x => h('th', { scope: 'col' }, x)))),
      h('tbody', {}, rows.map(r => h('tr', {}, h('td', {}, h('strong', {}, String(r.k.kind)), r.k.label ? ' ' + t(r.k.label) : ''), h('td', {}, r.read), h('td', {}, r.write), h('td', {}, r.proj), h('td', { class: 'muted' }, t(r.k.source),
        r.k.source === 'capitaine' ? [' ', h('button', { class: 'ghost', onclick: async () => { try { await post('kinds', { reset: [r.k.kind] }); options(root, K); } catch (e) { K.toast(e.message); } } }, t('Défaut'))] : null))))),
    h('p', {}, addNo, ' ', save), h('p', { class: 'muted' }, t('Fixes : 1059 — {0} ; 22242 — {1}.', t(kinds.fixed['1059']), t(kinds.fixed['22242']))));

  // ── Tunnels et accès ──
  let tunnelsCard = null;
  if (tun) {
    const apply = async (body, msg) => { try { await post('tunnels', body); K.toast(msg, 1); options(root, K); } catch (e) { K.toast(e.message); } };
    const when = u => u ? new Date(u * 1000).toLocaleString() : t('sans limite');
    const svc = tun.services.map(sv => {
      const mode = h('select', { 'aria-label': t('Accès à {0}', sv.name) }, tun.modes.map(m => h('option', { value: m, selected: m === sv.mode }, t(TMODE[m]) + (m === sv.default_mode ? ' ' + t('(défaut)') : ''))));
      mode.onchange = () => apply({ service: sv.name, mode: mode.value }, t('{0} : accès modifié', sv.name));
      const who = h('select', { 'aria-label': t('Capitaine ami') }, h('option', { value: '' }, t('Choisir un ami…')), tun.friends.map(f => h('option', { value: f.peer }, f.label)));
      const peer = h('input', { placeholder: t('ou PeerID 12D3Koo…'), autocomplete: 'off', 'aria-label': 'PeerID' }), hours = h('select', { 'aria-label': t('Durée') }, [[2, '2 heures'], [24, '24 heures'], [168, '7 jours'], [0, 'Sans limite']].map(([v, l]) => h('option', { value: v }, t(l))));
      const add = h('button', { onclick: () => {
        const p = (peer.value.trim() || who.value), label = peer.value.trim() ? '' : who.options[who.selectedIndex].text;
        if (!p) return K.toast(t('Choisissez un ami ou collez un PeerID'));
        apply({ service: sv.name, grant: { peer: p, label, hours: Number(hours.value) } }, t('Accès accordé'));
      } }, t('Autoriser'));
      return h('div', { class: 'card' }, h('h3', {}, t(TNAME[sv.name] || sv.name)),
        sv.name === 'ssh' ? h('p', { class: sv.mode === 'off' ? 'muted' : 'degraded', role: 'note' }, sv.mode === 'off' ? t('Fermé : personne ne peut entrer par SSH.') : t('Ouvert : les Capitaines autorisés peuvent se connecter à cette machine (clé jumelle de leur identité, niveau Y). Chaque session est journalisée.')) : null,
        h('label', {}, t('Qui peut ouvrir ce service')), mode,
        sv.grants.length ? h('ul', {}, sv.grants.map(g => h('li', {}, h('strong', {}, g.label || g.peer.slice(0, 12)), ` — ${g.expired ? t('expiré') : t('jusqu\'à {0}', when(g.until))} `, h('button', { class: 'ghost', onclick: () => apply({ service: sv.name, revoke: g.peer }, t('Accès retiré')) }, t('Retirer'))))) : h('p', { class: 'muted' }, t(t('Aucun pair nommé.'))),
        h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Laisser un ami m\'aider')), who, ' ', peer, ' ', hours, ' ', add),
        sv.customized ? h('p', {}, h('button', { class: 'ghost', onclick: () => apply({ service: sv.name, reset: true }, t('Réglage par défaut rétabli')) }, t('Rétablir le défaut'))) : null);
    });
    tunnelsCard = h('section', { class: 'card wide' }, h('h2', {}, t('Tunnels et accès des autres Capitaines')),
      h('p', { class: 'muted' }, tun.origin ? t('UPlanet ORIGIN : le SSH est ouvert par défaut aux Capitaines amis (suivi réciproque).') : t('UPlanet ẐEN : le SSH est fermé par défaut. Vous le rouvrez pour un ami le temps qu\'il vous aide, ou pour tous vos Capitaines amis.')),
      h('p', { class: 'muted' }, t('Le réglage s\'applique tout de suite, y compris aux sessions déjà ouvertes (vérifié chaque minute).')), svc);
  }

  // ── Assistance entre Capitaines amis ──
  let assistCard = null;
  if (asst) {
    const act = async (body, msg) => { const btn = document.activeElement?.tagName === 'BUTTON' ? document.activeElement : null; if (btn) btn.disabled = true; try { const r = await post('assist', body); K.toast(r.sent !== undefined ? `${msg} (${t('{0} ami(s) sollicité(s)', r.sent)})` : msg, 1); options(root, K); } catch (e) { K.toast(e.message); if (btn) btn.disabled = false; } };
    const STATUS = { pending: 'en attente', accepted: 'accepté', declined: 'refusé', closed: 'clos', expired: 'expiré' };
    const service = h('select', { id: 'as-svc', 'aria-label': t('Service à ouvrir') }, asst.services.map(sv => h('option', { value: sv }, t(TNAME[sv] || sv))));
    const hours = h('select', { id: 'as-h', 'aria-label': t('Durée') }, [[2, '2 heures'], [24, '24 heures'], [72, '3 jours'], [168, '7 jours']].map(([v, l]) => h('option', { value: v }, t(l))));
    const to = h('select', { id: 'as-to', 'aria-label': t('Qui solliciter') }, h('option', { value: '' }, t('Tous mes Capitaines amis')), asst.friends.map(f => h('option', { value: f.hex }, f.label)));
    const first = h('input', { type: 'checkbox', id: 'as-first' }), note = h('input', { id: 'as-note', maxlength: 280, placeholder: t('Ce qui ne va pas (facultatif)'), autocomplete: 'off' });
    const send = h('button', { disabled: !asst.friends.length, onclick: () => act({ action: 'request', service: service.value, hours: Number(hours.value), note: note.value.trim(), to: to.value, exclusive: first.checked }, t('Demande envoyée')) }, t('Demander de l\'aide'));
    // Console SSH dans le navigateur : la station lance `ssh` vers l'accès local ; le ticket (30 s, un usage) passe dans le fragment de l'adresse.
    const consoleLauncher = r => {
      const user = h('input', { 'aria-label': t('Utilisateur sur la station de l\'ami'), placeholder: t('utilisateur'), size: 12, maxlength: 32, autocomplete: 'off', value: (() => { try { return localStorage.getItem('astro.console.user') || ''; } catch { return ''; } })() });
      const go = h('button', { onclick: async ev => {
        const u = user.value.trim();
        if (!u) { K.toast(t('Indiquez votre nom d\'utilisateur sur la machine de l\'ami.')); user.focus(); return; }
        const w = window.open('about:blank', '_blank'), btn = ev.currentTarget;
        btn.disabled = true;
        try {
          if (!r.local) await post('assist', { action: 'connect', id: r.id });
          const t = await post('assist', { action: 'console', id: r.id, user: u });
          try { localStorage.setItem('astro.console.user', u); } catch { /* facultatif */ }
          if (w) w.location.href = `/ui/console.html#ticket=${t.ticket}&label=${encodeURIComponent(`${u}@${r.from}`)}`; else K.toast(t('Fenêtre bloquée : autorisez les fenêtres de cette page.'));
          options(root, K);
        } catch (e) { if (w) w.close(); K.toast(e.message); btn.disabled = false; }
      } }, t('Ouvrir la console'));
      return h('span', { class: 'inline' }, user, ' ', go);
    };
    const incoming = asst.incoming.map(r => h('li', {}, h('strong', {}, r.from), ' ' + t('demande de l\'aide : {0}, {1} h', t(TNAME[r.service] || r.service), r.hours), r.note ? ` — « ${r.note} »` : '', ` (${t(STATUS[r.status])}) `,
      r.status === 'pending' ? h('p', {}, h('button', { onclick: () => act({ action: 'respond', id: r.id, other: r.other, accept: true }, t('Vous aidez {0}', r.from)) }, t('Accepter d\'aider')), ' ', h('button', { class: 'ghost', onclick: () => act({ action: 'respond', id: r.id, other: r.other, accept: false }, t('Demande refusée')) }, t('Refuser'))) : null,
      r.status === 'accepted' && r.station ? h('p', { class: 'muted' }, r.service === 'ssh'
        ? [t('Machine de {0} : ', r.from), consoleLauncher(r), r.local ? h('span', {}, ' ', h('button', { class: 'ghost', onclick: () => act({ action: 'disconnect', id: r.id }, t('Accès fermé')) }, t('Fermer l\'accès'))) : null]
        : r.local
          ? [t('Accès ouvert jusqu\'à {0} : ', K.when(r.local.until)), r.service === 'comfyui' ? h('a', { href: `http://${r.local.addr}`, target: '_blank', rel: 'noopener' }, `http://${r.local.addr}`) : h('code', {}, `http://${r.local.addr}`), ' ', h('button', { class: 'ghost', onclick: () => act({ action: 'disconnect', id: r.id }, t('Accès fermé')) }, t('Fermer l\'accès'))]
          : [t('Station de {0} : ', r.from), h('button', { onclick: () => act({ action: 'connect', id: r.id }, t('Accès local ouvert')) }, t('Ouvrir l\'accès'))]) : null));
    const outgoing = asst.outgoing.map(r => h('li', {}, `${t(TNAME[r.service] || r.service)}, ${r.hours} h${r.exclusive ? ' ' + t('(le premier qui accepte)') : ''} : `, r.recipients.map(x => `${x.label} ${t(STATUS[x.status])}`).join(', '), ' ',
      r.recipients.some(x => x.status === 'pending') ? h('button', { class: 'ghost', onclick: () => act({ action: 'cancel', id: r.id }, t('Demande retirée')) }, t('Retirer')) : null));
    assistCard = h('section', { class: 'card wide' }, h('h2', {}, t('Assistance entre Capitaines amis')),
      h('p', { class: 'muted' }, t('Demandez de l\'aide : rien n\'est ouvert tant qu\'un ami n\'a pas accepté. L\'accès s\'ouvre alors pour lui seul, pour la durée choisie, puis se ferme tout seul.')),
      h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Nouvelle demande')),
        h('div', {}, h('label', { for: 'as-svc' }, t('Service à ouvrir')), service), h('div', {}, h('label', { for: 'as-h' }, t('Pendant')), hours), h('div', {}, h('label', { for: 'as-to' }, t('Qui solliciter')), to),
        h('div', {}, h('label', { for: 'as-note' }, t('Message')), note),
        h('label', { for: 'as-first' }, first, t("Seul le premier qui accepte reçoit l'accès")), h('p', {}, send)),
      asst.friends.length ? null : h('p', { class: 'muted' }, t('Aucun Capitaine ami réciproque pour le moment (suivez-vous mutuellement).')),
      incoming.length ? [h('h3', {}, t('Demandes reçues')), h('ul', {}, incoming)] : null,
      outgoing.length ? [h('h3', {}, t('Mes demandes')), h('ul', {}, outgoing)] : null);
  }

  // ── File de traitement ──
  let tasksCard = null, tasksSummary = '';
  if (tk) {
    const KIND = { facecloud: 'Reconnaissance des visages', 'bro.upload': 'Dépôts demandés à BRO' };
    const failed = tk.kinds.reduce((n, k) => n + k.failed, 0), waiting = tk.kinds.reduce((n, k) => n + k.pending + k.running, 0);
    tasksSummary = t('{0} en cours ou en attente · {1} abandonnée(s)', waiting, failed);
    const retry = async (kind, ev) => { ev.currentTarget.disabled = true; try { const r = await post('tasks', { action: 'retry', kind }); K.toast(t('{0} tâche(s) relancée(s)', r.retried), 1); options(root, K); } catch (e) { K.toast(e.message); ev.currentTarget.disabled = false; } };
    tasksCard = h('section', { class: 'card wide' }, h('h2', {}, t('File de traitement')),
      h('p', { class: 'muted' }, t('Les tâches en arrière-plan (reconnaissance des visages, dépôts demandés à BRO). Une tâche abandonnée après plusieurs essais reste ici jusqu\'à ce que vous la relanciez.')),
      tk.kinds.length ? h('table', {}, h('thead', {}, h('tr', {}, ['Tâche', 'En attente', 'En cours', 'Terminées', 'Abandonnées'].map(x => h('th', { scope: 'col' }, t(x))))),
        h('tbody', {}, tk.kinds.map(k => h('tr', {}, h('td', {}, t(KIND[k.kind] || k.kind)), h('td', {}, String(k.pending)), h('td', {}, String(k.running)), h('td', {}, String(k.done)), h('td', { class: k.failed ? 'down' : '' }, String(k.failed), k.failed ? [' ', h('button', { class: 'ghost', onclick: ev => retry(k.kind, ev) }, t('Relancer'))] : null))))) : h('p', { class: 'muted' }, t('Aucune tâche pour le moment.')),
      tk.failed.length ? [h('h3', {}, t('Dernières abandonnées')), h('ul', {}, tk.failed.map(f => h('li', {}, `#${f.id} ${t(KIND[f.kind] || f.kind)} · ${f.attempts} ` + t('essai(s)') + (f.why ? ` — ${f.why}` : ''))))] : null);
  }

  // Les demandes d'aide arrivent d'une autre station : l'écran se met à jour seul, sans écraser une saisie en cours.
  if (asst) {
    const key = v => JSON.stringify([v.incoming, v.outgoing]);
    root._asstKey = key(asst);
    if (!root._asstPoll) {
      root._asstPoll = true; // un seul minuteur, même si l'écran est redessiné
      K.every(15000, async () => {
        const typing = ['INPUT', 'SELECT'].includes(document.activeElement?.tagName) && root.contains(document.activeElement);
        const now = await api('assist').catch(() => null);
        if (now && key(now) !== root._asstKey && !typing) options(root, K);
      });
    }
  }
  // Les réglages experts sont repliés (avec un résumé de leur état) ; ce qui est ouvert le reste d'un redessin à l'autre.
  const opened = root._opened ||= new Set();
  const fold = (sec, summary) => {
    if (!sec) return null;
    const [title, ...body] = [...sec.childNodes], key = title.textContent;
    const d = h('details', { class: 'card wide fold', open: opened.has(key) }, h('summary', {}, h('strong', {}, key), ' ', h('span', { class: 'muted' }, summary)), ...body);
    d.addEventListener('toggle', () => (d.open ? opened.add(key) : opened.delete(key)));
    return d;
  };
  const tunSummary = tun ? tun.services.map(sv => `${sv.name} : ${t(TMODE[sv.mode]).toLowerCase()}`).join(' · ') : '';
  root.replaceChildren(h('h2', {}, t('Options de la station')), h('div', { class: 'grid' }, assistCard,
    fold(tunnelsCard, tunSummary), fold(tasksCard, tasksSummary), fold(faceCard, t(MODE[face.mode] || '')), fold(kindsCard, t('réglages avancés du relai'))));
}
