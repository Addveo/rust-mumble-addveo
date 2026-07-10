use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::Html,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

use super::AppStateRef;
use crate::anticheat::{action_name, parse_action};
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
    pub action: &'static str,
    pub connected: u32,
    pub clients: Vec<AnticheatClient>,
    pub logs: Vec<String>,
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
        action: action_name(ac.action.load(Ordering::Relaxed)),
        connected: state.server.active_clients.load(Ordering::Relaxed),
        clients,
        logs: ac.recent_logs(),
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
    pub action: Option<String>,
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
    if let Some(action) = update.action {
        ac.action.store(parse_action(&action), Ordering::Relaxed);
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

    match payload.action.as_str() {
        "block" => {
            client.ac_exempt.store(false, Ordering::Relaxed);
            client.set_mute(true);
        }
        "unblock" => {
            // exempt: the automatic action won't re-mute them on the next packet
            client.ac_exempt.store(true, Ordering::Relaxed);
            client.set_mute(false);
        }
        "kick" => {
            state
                .server
                .add_client_to_disconnect_queue(client.session_id, DisconnectReason::Anticheat);
        }
        "reset" => {
            client.ac_score.store(0, Ordering::Relaxed);
            client.ac_flags.store(0, Ordering::Relaxed);
            client.ac_max_recipients.store(0, Ordering::Relaxed);
            client.ac_strikes.store(0, Ordering::Relaxed);
            client.ac_window.lock().clear();
        }
        _ => return StatusCode::BAD_REQUEST,
    }

    tracing::info!("[ANTICHEAT] api action '{}' applied to {}", payload.action, client);

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
<label>Action:
<select id="action">
<option value="log">log</option>
<option value="mute">mute</option>
<option value="kick">kick</option>
</select>
</label>
<button onclick="applyConfig()">Appliquer</button>
<span id="confmsg"></span>
</fieldset>

<p>Connect&eacute;s: <b id="connected">?</b> &mdash; mise &agrave; jour auto toutes les 2s</p>

<div style="display:flex; gap:16px; align-items:flex-start;">

<div style="flex:1; overflow-x:auto;">
<table border="1" cellpadding="4">
<thead>
<tr><th>Session</th><th>Nom</th><th>IP</th><th>Client</th><th>OS</th><th>Chan</th><th>Reach</th><th>Fen&ecirc;tre</th><th>Mutual.</th><th>Listens</th><th>Score</th><th>Flags</th><th>Mut&eacute;</th><th>Exempt</th><th>Actions</th></tr>
</thead>
<tbody id="clients"></tbody>
</table>
</div>

<div style="width:420px; flex-shrink:0;">
<b>Logs anticheat</b>
<pre id="logs" style="height:420px; overflow:auto; border:1px solid #888; padding:6px; margin:4px 0 0; font-size:12px; white-space:pre-wrap;"></pre>
</div>

</div>

<script>
let firstLoad = true;

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
        document.getElementById('action').value = d.action;
        firstLoad = false;
    }

    const tbody = document.getElementById('clients');
    tbody.innerHTML = '';
    for (const c of d.clients) {
        const tr = document.createElement('tr');
        const mut = c.mutuality === 255 ? '-' : (c.mutuality + '%');
        const cells = [c.session_id, c.name, c.ip, c.release, c.os, c.channel_id, c.reach_instant, c.reach_window, mut, c.listens, c.score, c.flags,
                       c.muted ? 'OUI' : 'non', c.exempt ? 'OUI' : 'non'];
        for (const v of cells) {
            const td = document.createElement('td');
            td.textContent = v;
            tr.appendChild(td);
        }
        const td = document.createElement('td');
        for (const [label, action] of [['Bloquer','block'], ['Débloquer','unblock'], ['Kick','kick'], ['Reset','reset']]) {
            const b = document.createElement('button');
            b.textContent = label;
            b.onclick = () => userAction(c.name, action);
            td.appendChild(b);
        }
        tr.appendChild(td);
        tbody.appendChild(tr);
    }

    document.getElementById('logs').textContent =
        d.logs && d.logs.length ? d.logs.join('\n') : '(aucune détection pour le moment)';
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
        action: document.getElementById('action').value,
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

load();
setInterval(load, 2000);
</script>
</body>
</html>
"#;
