use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::Html,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

use super::AppStateRef;
use crate::anticheat::{action_name, parse_action};
use crate::client::ClientArc;
use crate::error::DisconnectReason;

#[derive(Serialize)]
pub struct AnticheatClient {
    pub session_id: u32,
    pub name: String,
    pub ip: String,
    pub version: String,
    pub release: String,
    pub os: String,
    pub channel_id: u32,
    pub reach_instant: u32,
    pub reach_window: u32,
    /// mutuality percent 0-100, or 255 = n/a
    pub mutuality: u32,
    pub listens: u32,
    pub has_pos: bool,
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
    /// Retours vers un lieu antérieur lointain (oscillation) dans la fenêtre.
    pub pos_jumps: u32,
    pub max_speed: f32,
    /// Cibles proximité à position fraîche hors de portée (incohérence).
    pub far_targets: u32,
    pub score: u64,
    pub flags: u32,
    pub muted: bool,
    pub exempt: bool,
}

#[derive(Serialize)]
pub struct AnticheatStatus {
    pub enabled: bool,
    pub threshold_pct: u32,
    pub min_recipients: u32,
    pub mutuality_max_pct: u32,
    pub window_secs: u32,
    pub strikes_required: u32,
    pub listen_max: u32,
    pub pos_osc_dist: u32,
    pub pos_osc_max: u32,
    pub pos_far_dist: u32,
    pub pos_far_min: u32,
    pub pos_far_pct: u32,
    pub heard_secs: u32,
    pub action: &'static str,
    pub webhook: Option<String>,
    pub panel_url: Option<String>,
    pub connected: u32,
    pub clients: Vec<AnticheatClient>,
    pub logs: Vec<String>,
    pub bans: Vec<crate::anticheat::BanEntry>,
}

pub async fn get_anticheat(State(state): State<AppStateRef>) -> Json<AnticheatStatus> {
    let ac = &state.server.anticheat;

    let mut clients = Vec::new();
    let mut iter = state.server.clients.first_entry_async().await;
    while let Some(entry) = iter {
        let client = entry.get();
        clients.push(AnticheatClient {
            session_id: client.session_id,
            name: client.get_name().as_ref().clone(),
            ip: client.peer_ip.to_string(),
            version: client.version_str.clone(),
            release: client.version_release.clone(),
            os: client.version_os.clone(),
            channel_id: client.channel_id.load(Ordering::Relaxed),
            reach_instant: client.ac_reach_instant.load(Ordering::Relaxed),
            reach_window: client.ac_reach_window.load(Ordering::Relaxed),
            mutuality: client.ac_mutuality.load(Ordering::Relaxed),
            listens: client.ac_listen_count.load(Ordering::Relaxed),
            has_pos: client.ac_has_pos.load(Ordering::Relaxed),
            pos_x: client.ac_pos_x.load(Ordering::Relaxed),
            pos_y: client.ac_pos_y.load(Ordering::Relaxed),
            pos_z: client.ac_pos_z.load(Ordering::Relaxed),
            pos_jumps: client.ac_pos_jumps.load(Ordering::Relaxed),
            max_speed: client.ac_max_speed.load(Ordering::Relaxed),
            far_targets: client.ac_far_targets.load(Ordering::Relaxed),
            score: client.ac_score.load(Ordering::Relaxed),
            flags: client.ac_flags.load(Ordering::Relaxed),
            muted: client.is_muted(),
            exempt: client.ac_exempt.load(Ordering::Relaxed),
        });
        iter = entry.next_async().await;
    }

    clients.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(b.reach_window.cmp(&a.reach_window)));

    Json(AnticheatStatus {
        enabled: ac.enabled.load(Ordering::Relaxed),
        threshold_pct: ac.threshold_pct.load(Ordering::Relaxed),
        min_recipients: ac.min_recipients.load(Ordering::Relaxed),
        mutuality_max_pct: ac.mutuality_max_pct.load(Ordering::Relaxed),
        window_secs: ac.window_secs.load(Ordering::Relaxed),
        strikes_required: ac.strikes_required.load(Ordering::Relaxed),
        listen_max: ac.listen_max.load(Ordering::Relaxed),
        pos_osc_dist: ac.pos_osc_dist.load(Ordering::Relaxed),
        pos_osc_max: ac.pos_osc_max.load(Ordering::Relaxed),
        pos_far_dist: ac.pos_far_dist.load(Ordering::Relaxed),
        pos_far_min: ac.pos_far_min.load(Ordering::Relaxed),
        pos_far_pct: ac.pos_far_pct.load(Ordering::Relaxed),
        heard_secs: ac.heard_secs.load(Ordering::Relaxed),
        action: action_name(ac.action.load(Ordering::Relaxed)),
        webhook: ac.webhook.lock().clone(),
        panel_url: ac.panel_url.lock().clone(),
        connected: state.server.active_clients.load(Ordering::Relaxed),
        clients,
        logs: ac.recent_logs(),
        bans: state.server.bans.list(),
    })
}

#[derive(Deserialize)]
pub struct ConfigUpdate {
    pub enabled: Option<bool>,
    pub threshold_pct: Option<u32>,
    pub min_recipients: Option<u32>,
    pub mutuality_max_pct: Option<u32>,
    pub window_secs: Option<u32>,
    pub strikes_required: Option<u32>,
    pub listen_max: Option<u32>,
    pub pos_osc_dist: Option<u32>,
    pub pos_osc_max: Option<u32>,
    pub pos_far_dist: Option<u32>,
    pub pos_far_min: Option<u32>,
    pub pos_far_pct: Option<u32>,
    pub heard_secs: Option<u32>,
    pub action: Option<String>,
    pub webhook: Option<String>,
    pub panel_url: Option<String>,
}

pub async fn post_anticheat_config(State(state): State<AppStateRef>, Json(update): Json<ConfigUpdate>) -> StatusCode {
    let ac = &state.server.anticheat;

    if let Some(enabled) = update.enabled {
        ac.enabled.store(enabled, Ordering::Relaxed);
    }
    if let Some(pct) = update.threshold_pct {
        ac.threshold_pct.store(pct.clamp(1, 100), Ordering::Relaxed);
    }
    if let Some(min) = update.min_recipients {
        ac.min_recipients.store(min, Ordering::Relaxed);
    }
    if let Some(m) = update.mutuality_max_pct {
        ac.mutuality_max_pct.store(m.min(100), Ordering::Relaxed);
    }
    if let Some(w) = update.window_secs {
        ac.window_secs.store(w.max(1), Ordering::Relaxed);
    }
    if let Some(s) = update.strikes_required {
        ac.strikes_required.store(s.max(1), Ordering::Relaxed);
    }
    if let Some(l) = update.listen_max {
        ac.listen_max.store(l, Ordering::Relaxed);
    }
    if let Some(v) = update.pos_osc_dist {
        // 0 = off ; sinon plancher à 50 m pour ne pas compter la marche comme "lieu".
        ac.pos_osc_dist.store(if v == 0 { 0 } else { v.max(50) }, Ordering::Relaxed);
    }
    if let Some(v) = update.pos_osc_max {
        ac.pos_osc_max.store(v, Ordering::Relaxed);
    }
    if let Some(v) = update.pos_far_dist {
        // Plancher à 100 m : sous la portée cri de pma-voice, ça flaggerait la proximité.
        ac.pos_far_dist.store(v.max(100), Ordering::Relaxed);
    }
    if let Some(v) = update.pos_far_min {
        ac.pos_far_min.store(v, Ordering::Relaxed);
    }
    if let Some(v) = update.pos_far_pct {
        ac.pos_far_pct.store(v.clamp(1, 100), Ordering::Relaxed);
    }
    if let Some(v) = update.heard_secs {
        // 10 s à 2 h : borne la mémoire "qui a parlé" à des valeurs raisonnables.
        ac.heard_secs.store(v.clamp(10, 7200), Ordering::Relaxed);
    }
    if let Some(action) = update.action {
        ac.action.store(parse_action(&action), Ordering::Relaxed);
    }
    if let Some(w) = update.webhook {
        *ac.webhook.lock() = if w.trim().is_empty() { None } else { Some(w) };
    }
    if let Some(p) = update.panel_url {
        *ac.panel_url.lock() = if p.trim().is_empty() { None } else { Some(p) };
    }

    tracing::info!(
        "[ANTICHEAT] config updated via api: enabled={}, threshold={}%, min_recipients={}, mutuality_max={}%, window={}s, strikes={}, listen_max={}, action={}",
        ac.enabled.load(Ordering::Relaxed),
        ac.threshold_pct.load(Ordering::Relaxed),
        ac.min_recipients.load(Ordering::Relaxed),
        ac.mutuality_max_pct.load(Ordering::Relaxed),
        ac.window_secs.load(Ordering::Relaxed),
        ac.strikes_required.load(Ordering::Relaxed),
        ac.listen_max.load(Ordering::Relaxed),
        action_name(ac.action.load(Ordering::Relaxed)),
    );

    StatusCode::OK
}

#[derive(Deserialize)]
pub struct UserAction {
    pub user: String,
    /// block | unblock | kick | reset
    pub action: String,
}

pub async fn post_anticheat_user(State(state): State<AppStateRef>, Json(payload): Json<UserAction>) -> StatusCode {
    let Some(client) = state.server.get_client_by_name(payload.user.as_str()).await else {
        return StatusCode::NOT_FOUND;
    };

    let ip = client.peer_ip.to_string();
    let np = crate::anticheat::name_part(client.get_name().as_str());

    match payload.action.as_str() {
        "block" => {
            // mute PERSISTANT : ajouté à la liste (IP+nom) → re-muté à chaque
            // reconnexion, donc déco/reco ne l'enlève pas.
            client.ac_exempt.store(false, Ordering::Relaxed);
            state.server.bans.add(
                "mute",
                Some(ip),
                Some(np),
                client.get_name().to_string(),
                "mute manuel".to_string(),
            );
            client.set_mute(true);
        }
        "unblock" => {
            // retire le mute persistant + démute + exempt (l'anticheat auto ne le re-mute pas)
            state.server.bans.remove_matching(&ip, &np, "mute");
            client.ac_exempt.store(true, Ordering::Relaxed);
            client.set_mute(false);
        }
        "kick" => {
            state
                .server
                .add_client_to_disconnect_queue(client.session_id, DisconnectReason::Anticheat);
        }
        "reset" => {
            reset_client_counters(&client);
        }
        "ban" => {
            // ban par IP + nom, persisté, puis déconnexion. La reconnexion FiveM
            // sera refusée à l'entrée (cf. tcp.rs).
            state.server.bans.add(
                "ban",
                Some(ip),
                Some(np),
                client.get_name().to_string(),
                "banni via panel".to_string(),
            );
            state
                .server
                .add_client_to_disconnect_queue(client.session_id, DisconnectReason::Anticheat);
        }
        _ => return StatusCode::BAD_REQUEST,
    }

    tracing::info!("[ANTICHEAT] api action '{}' applied to {}", payload.action, client);

    StatusCode::OK
}

#[derive(Deserialize)]
pub struct Unban {
    pub id: u64,
}

pub async fn post_anticheat_unban(State(state): State<AppStateRef>, Json(u): Json<Unban>) -> StatusCode {
    state.server.bans.remove(u.id);
    tracing::info!("[ANTICHEAT] unban id={} via api", u.id);
    StatusCode::OK
}

/// Remet à zéro les compteurs anticheat d'UN client (bouton Reset + clear global).
fn reset_client_counters(client: &ClientArc) {
    client.ac_score.store(0, Ordering::Relaxed);
    client.ac_flags.store(0, Ordering::Relaxed);
    client.ac_max_recipients.store(0, Ordering::Relaxed);
    client.ac_strikes.store(0, Ordering::Relaxed);
    client.ac_window.lock().clear();
    client.ac_pos_jumps.store(0, Ordering::Relaxed);
    client.ac_max_speed.store(0.0, Ordering::Relaxed);
    client.ac_pos_hist.lock().clear();
    client.ac_far_targets.store(0, Ordering::Relaxed);
    client.ac_heard.lock().clear();
}

#[derive(Serialize)]
pub struct HeardItem {
    pub session_id: u32,
    pub name: String,
    pub ip: String,
    /// Secondes écoulées depuis la dernière fois qu'il lui a parlé.
    pub ago_secs: u64,
    /// Secondes de parole cumulées (~1 tick/s).
    pub secs: u32,
}

#[derive(Serialize)]
pub struct HeardResponse {
    pub session_id: u32,
    pub name: String,
    pub retention_secs: u32,
    pub heard: Vec<HeardItem>,
}

/// QUI a parlé À ce joueur récemment (mémoire `heard_secs`, cap 64 émetteurs).
/// Les entrées gardent nom + IP même si l'émetteur s'est déconnecté.
pub async fn get_heard(
    State(state): State<AppStateRef>,
    Path(session_id): Path<u32>,
) -> Result<Json<HeardResponse>, StatusCode> {
    let Some(entry) = state.server.clients.get_async(&session_id).await else {
        return Err(StatusCode::NOT_FOUND);
    };
    let client = entry.get();
    let retention_secs = state.server.anticheat.heard_secs.load(Ordering::Relaxed);
    let retention = std::time::Duration::from_secs(retention_secs.max(10) as u64);
    let now = std::time::Instant::now();
    let mut heard: Vec<HeardItem> = {
        let mut h = client.ac_heard.lock();
        h.retain(|_, e| now.duration_since(e.last) <= retention);
        h.iter()
            .map(|(&sid, e)| HeardItem {
                session_id: sid,
                name: e.name.clone(),
                ip: e.ip.clone(),
                ago_secs: now.duration_since(e.last).as_secs(),
                secs: e.secs,
            })
            .collect()
    };
    heard.sort_unstable_by_key(|i| i.ago_secs);
    Ok(Json(HeardResponse {
        session_id,
        name: client.get_name().as_ref().clone(),
        retention_secs,
        heard,
    }))
}

/// Vide la liste « qui lui a parlé » d'un joueur (bouton du panel) — pour
/// repartir de zéro sur une victime : le prochain émetteur sera le suspect.
pub async fn post_heard_clear(
    State(state): State<AppStateRef>,
    Path(session_id): Path<u32>,
) -> StatusCode {
    let Some(entry) = state.server.clients.get_async(&session_id).await else {
        return StatusCode::NOT_FOUND;
    };
    entry.get().ac_heard.lock().clear();
    tracing::info!("[ANTICHEAT] liste 'qui a parlé' vidée pour la session {} via api", session_id);
    StatusCode::OK
}

/// Repart de zéro : compteurs de TOUS les clients + logs du panel — sans
/// recréer le conteneur. Ne touche NI la config NI la liste de bans/mutes.
pub async fn post_anticheat_clear(State(state): State<AppStateRef>) -> StatusCode {
    let mut n = 0u32;
    let mut iter = state.server.clients.first_entry_async().await;
    while let Some(entry) = iter {
        reset_client_counters(entry.get());
        n += 1;
        iter = entry.next_async().await;
    }
    state.server.anticheat.clear_logs();
    tracing::info!("[ANTICHEAT] clear global via api : compteurs de {} clients + logs remis à zéro", n);
    StatusCode::OK
}

pub async fn get_panel() -> Html<&'static str> {
    Html(PANEL_HTML)
}

const PANEL_HTML: &str = r#"<!doctype html>
<html lang="fr">
<body>

<fieldset>
<legend>Configuration</legend>
<label><input type="checkbox" id="enabled"> Actif</label>
&nbsp;|&nbsp;
<label>Seuil % joueurs: <input type="number" id="threshold_pct" min="1" max="100" size="4"></label>
<label>Min destinataires: <input type="number" id="min_recipients" min="1" size="4"></label>
<label>Mutualit&eacute; max %: <input type="number" id="mutuality_max_pct" min="0" max="100" size="4"></label>
<label>Fen&ecirc;tre (s): <input type="number" id="window_secs" min="1" size="4"></label>
<label>Strikes: <input type="number" id="strikes_required" min="1" size="4"></label>
<label>Max listens: <input type="number" id="listen_max" min="0" size="4"></label>
<label>Oscill. max (0=off): <input type="number" id="pos_osc_max" min="0" size="4" title="Retours A&rarr;B&rarr;A vers un lieu antérieur en 30s avant flag. Un tp légitime = 0 retour."></label>
<label>Dist. lieux (m): <input type="number" id="pos_osc_dist" min="0" size="5" title="Distance séparant deux lieux distincts pour le suivi d'oscillation."></label>
<label>Cibles loin min (0=off): <input type="number" id="pos_far_min" min="0" size="4" title="Cibles proximité à position fraîche situées trop loin de l'émetteur (position incohérente)."></label>
<label>Dist. cibles (m): <input type="number" id="pos_far_dist" min="100" size="5"></label>
<label>% cibles loin: <input type="number" id="pos_far_pct" min="1" max="100" size="4"></label>
<label>M&eacute;moire qui-parle (s): <input type="number" id="heard_secs" min="10" max="7200" size="5" title="Dur&eacute;e de r&eacute;tention de la liste 'qui a parl&eacute; &agrave; ce joueur' (clic sur un nom du tableau)."></label>
<label>Action:
<select id="action">
<option value="log">log</option>
<option value="mute">mute</option>
<option value="kick">kick</option>
</select>
</label>
<button onclick="applyConfig()">Appliquer</button>
<button onclick="resetConfig()">Réinit. défauts</button>
<button onclick="clearAll()" title="Remet à zéro scores/flags/strikes/compteurs de tous les joueurs et vide les logs. Ne touche ni la config ni les bans.">Clear compteurs/logs</button>
<span id="confmsg"></span>
<br>
<label>Webhook Discord: <input type="text" id="webhook" size="70" placeholder="https://discord.com/api/webhooks/..."></label>
<label>URL panel (ce serveur): <input type="text" id="panel_url" size="40" placeholder="http://ip:13000/panel"></label>
</fieldset>

<p>Connect&eacute;s: <b id="connected">?</b> &mdash; mise &agrave; jour auto toutes les 2s</p>

<div style="display:flex; gap:16px; align-items:flex-start;">

<div style="flex:1; overflow-x:auto;">
<table border="1" cellpadding="4">
<thead>
<tr id="headrow"></tr>
</thead>
<tbody id="clients"></tbody>
</table>
</div>

<div style="width:420px; flex-shrink:0;">
<div id="heardbox" style="display:none; margin-bottom:12px; border:2px solid #c60; padding:6px;">
<b id="heardtitle">-</b>
<div style="margin:4px 0;">
<button onclick="clearHeard()" title="Vide la liste : le prochain qui lui parle sera le seul dans la liste — parfait pour identifier un cheater qui recommence.">Vider la liste</button>
<button onclick="hideHeard()">Fermer</button>
</div>
<table border="1" cellpadding="3" style="width:100%; font-size:12px;">
<thead><tr><th>Session</th><th>Nom</th><th>IP</th><th>Il y a</th><th>Sec. parl&eacute;es</th></tr></thead>
<tbody id="heard"></tbody>
</table>
</div>
<b>Logs anticheat</b>
<pre id="logs" style="height:200px; overflow:auto; border:1px solid #888; padding:6px; margin:4px 0 0; font-size:12px; white-space:pre-wrap;"></pre>
<b>Bannis</b>
<table border="1" cellpadding="3" style="width:100%; font-size:12px;">
<thead><tr><th>Nom</th><th>IP</th><th>Depuis</th><th></th></tr></thead>
<tbody id="bans"></tbody>
</table>
</div>

</div>

<script>
let firstLoad = true;

// Colonnes du tableau : k = clé JSON triable (null = non triable).
const COLS = [
    {k:'session_id', l:'Session'},
    {k:'name', l:'Nom'},
    {k:'ip', l:'IP'},
    {k:'release', l:'Client'},
    {k:'channel_id', l:'Chan'},
    {k:'reach_instant', l:'Reach'},
    {k:'reach_window', l:'Fenêtre'},
    {k:'mutuality', l:'Mutual.'},
    {k:'listens', l:'Listens'},
    {k:null, l:'Position'},
    {k:'max_speed', l:'V.max'},
    {k:'pos_jumps', l:'Oscill.'},
    {k:'far_targets', l:'Loin'},
    {k:'score', l:'Score'},
    {k:'flags', l:'Flags'},
    {k:'muted', l:'Muté'},
    {k:'exempt', l:'Exempt'},
    {k:null, l:'Actions'},
];
let sortKey = 'score', sortDir = -1, lastData = null;

function buildHead() {
    const tr = document.getElementById('headrow');
    tr.innerHTML = '';
    for (const col of COLS) {
        const th = document.createElement('th');
        th.textContent = col.l + (col.k === sortKey ? (sortDir < 0 ? ' ▼' : ' ▲') : '');
        if (col.k) {
            th.style.cursor = 'pointer';
            th.title = 'Trier par ' + col.l;
            th.onclick = () => {
                if (sortKey === col.k) { sortDir = -sortDir; } else { sortKey = col.k; sortDir = -1; }
                buildHead();
                renderClients();
            };
        }
        tr.appendChild(th);
    }
}

function renderClients() {
    if (!lastData) return;
    const clients = lastData.clients.slice().sort((a, b) => {
        let va = a[sortKey], vb = b[sortKey];
        if (sortKey === 'mutuality') { if (va === 255) va = -1; if (vb === 255) vb = -1; }
        if (typeof va === 'string') return va.localeCompare(vb) * sortDir;
        return ((va > vb) - (va < vb)) * sortDir;
    });
    const tbody = document.getElementById('clients');
    tbody.innerHTML = '';
    for (const c of clients) {
        const tr = document.createElement('tr');
        const mut = c.mutuality === 255 ? '-' : (c.mutuality + '%');
        const pos = c.has_pos ? (Math.round(c.pos_x) + ',' + Math.round(c.pos_y) + ',' + Math.round(c.pos_z)) : '-';
        const spd = c.has_pos ? Math.round(c.max_speed) : '-';
        const cells = [c.session_id, c.name, c.ip, c.release, c.channel_id, c.reach_instant, c.reach_window, mut, c.listens, pos, spd, c.pos_jumps, c.far_targets, c.score, c.flags,
                       c.muted ? 'OUI' : 'non', c.exempt ? 'OUI' : 'non'];
        cells.forEach((v, i) => {
            const td = document.createElement('td');
            td.textContent = v;
            if (i === 1) {
                td.style.cursor = 'pointer';
                td.style.textDecoration = 'underline dotted';
                td.title = 'Voir qui lui a parlé';
                td.onclick = () => showHeard(c.session_id, c.name);
            }
            tr.appendChild(td);
        });
        const td = document.createElement('td');
        for (const [label, action] of [['Bloquer','block'], ['Débloquer','unblock'], ['Kick','kick'], ['BAN','ban'], ['Reset','reset']]) {
            const b = document.createElement('button');
            b.textContent = label;
            b.onclick = () => userAction(c.name, action);
            td.appendChild(b);
        }
        tr.appendChild(td);
        tbody.appendChild(tr);
    }
}

async function clearAll() {
    if (!confirm('Remettre à zéro les scores/flags/compteurs de TOUS les joueurs et vider les logs ?')) return;
    await fetch('anticheat/clear', {method: 'POST'});
    load();
}

// --- "Qui a parlé à X" : détail par joueur (clic sur un nom du tableau) ---
let heardSession = null;

function showHeard(sid, name) {
    heardSession = sid;
    document.getElementById('heardtitle').textContent = 'Qui a parlé à ' + name + ' ?';
    document.getElementById('heardbox').style.display = 'block';
    loadHeard();
}

function hideHeard() {
    heardSession = null;
    document.getElementById('heardbox').style.display = 'none';
}

async function clearHeard() {
    if (heardSession === null) return;
    await fetch('anticheat/heard/' + heardSession + '/clear', {method: 'POST'});
    loadHeard();
}

async function loadHeard() {
    if (heardSession === null) return;
    const r = await fetch('anticheat/heard/' + heardSession);
    const tb = document.getElementById('heard');
    if (!r.ok) {
        tb.innerHTML = '';
        document.getElementById('heardtitle').textContent += ' (déconnecté)';
        heardSession = null;
        return;
    }
    const d = await r.json();
    document.getElementById('heardtitle').textContent =
        'Qui a parlé à ' + d.name + ' ? (' + d.heard.length + ' émetteurs, mémoire ' + d.retention_secs + 's)';
    tb.innerHTML = '';
    for (const h of d.heard) {
        const tr = document.createElement('tr');
        for (const v of [h.session_id, h.name, h.ip, h.ago_secs + 's', h.secs]) {
            const td = document.createElement('td');
            td.textContent = v;
            tr.appendChild(td);
        }
        tb.appendChild(tr);
    }
}

async function load() {
    const r = await fetch('anticheat');
    if (!r.ok) return;
    const d = await r.json();

    document.getElementById('connected').textContent = d.connected;

    // only prefill the config on first load so we don't clobber edits in progress
    if (firstLoad) {
        document.getElementById('enabled').checked = d.enabled;
        document.getElementById('threshold_pct').value = d.threshold_pct;
        document.getElementById('min_recipients').value = d.min_recipients;
        document.getElementById('mutuality_max_pct').value = d.mutuality_max_pct;
        document.getElementById('window_secs').value = d.window_secs;
        document.getElementById('strikes_required').value = d.strikes_required;
        document.getElementById('listen_max').value = d.listen_max;
        document.getElementById('pos_osc_max').value = d.pos_osc_max;
        document.getElementById('pos_osc_dist').value = d.pos_osc_dist;
        document.getElementById('pos_far_min').value = d.pos_far_min;
        document.getElementById('pos_far_dist').value = d.pos_far_dist;
        document.getElementById('pos_far_pct').value = d.pos_far_pct;
        document.getElementById('heard_secs').value = d.heard_secs;
        document.getElementById('action').value = d.action;
        document.getElementById('webhook').value = d.webhook || '';
        document.getElementById('panel_url').value = d.panel_url || '';
        firstLoad = false;
    }

    lastData = d;
    renderClients();
    loadHeard();

    document.getElementById('logs').textContent =
        d.logs && d.logs.length ? d.logs.join('\n') : '(aucune détection pour le moment)';

    const bans = document.getElementById('bans');
    bans.innerHTML = '';
    for (const b of (d.bans || [])) {
        const tr = document.createElement('tr');
        for (const v of [b.display, b.ip || '-', b.at]) {
            const td = document.createElement('td');
            td.textContent = v;
            tr.appendChild(td);
        }
        const td = document.createElement('td');
        const btn = document.createElement('button');
        btn.textContent = 'Déban';
        btn.onclick = () => unban(b.id);
        td.appendChild(btn);
        tr.appendChild(td);
        bans.appendChild(tr);
    }
}

function resetConfig() {
    document.getElementById('enabled').checked = true;
    document.getElementById('threshold_pct').value = 60;
    document.getElementById('min_recipients').value = 15;
    document.getElementById('mutuality_max_pct').value = 30;
    document.getElementById('window_secs').value = 10;
    document.getElementById('strikes_required').value = 3;
    document.getElementById('listen_max').value = 25;
    document.getElementById('pos_osc_max').value = 0;
    document.getElementById('pos_osc_dist').value = 300;
    document.getElementById('pos_far_min').value = 15;
    document.getElementById('pos_far_dist').value = 500;
    document.getElementById('pos_far_pct').value = 70;
    document.getElementById('heard_secs').value = 900;
    document.getElementById('action').value = 'log';
    applyConfig();
}

async function unban(id) {
    await fetch('anticheat/unban', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({id}),
    });
    load();
}

async function applyConfig() {
    const body = {
        enabled: document.getElementById('enabled').checked,
        threshold_pct: parseInt(document.getElementById('threshold_pct').value, 10),
        min_recipients: parseInt(document.getElementById('min_recipients').value, 10),
        mutuality_max_pct: parseInt(document.getElementById('mutuality_max_pct').value, 10),
        window_secs: parseInt(document.getElementById('window_secs').value, 10),
        strikes_required: parseInt(document.getElementById('strikes_required').value, 10),
        listen_max: parseInt(document.getElementById('listen_max').value, 10),
        pos_osc_max: parseInt(document.getElementById('pos_osc_max').value, 10),
        pos_osc_dist: parseInt(document.getElementById('pos_osc_dist').value, 10),
        pos_far_min: parseInt(document.getElementById('pos_far_min').value, 10),
        pos_far_dist: parseInt(document.getElementById('pos_far_dist').value, 10),
        pos_far_pct: parseInt(document.getElementById('pos_far_pct').value, 10),
        heard_secs: parseInt(document.getElementById('heard_secs').value, 10),
        action: document.getElementById('action').value,
        webhook: document.getElementById('webhook').value,
        panel_url: document.getElementById('panel_url').value,
    };
    const r = await fetch('anticheat/config', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify(body),
    });
    document.getElementById('confmsg').textContent = r.ok ? 'OK' : 'Erreur ' + r.status;
    setTimeout(() => document.getElementById('confmsg').textContent = '', 2000);
}

async function userAction(user, action) {
    await fetch('anticheat/user', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({user, action}),
    });
    load();
}

buildHead();
load();
setInterval(load, 2000);
</script>
</body>
</html>
"#;
