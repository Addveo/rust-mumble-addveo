use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::client::Client;
use crate::error::DisconnectReason;
use crate::state::ServerState;

/// Nombre d'événements anticheat gardés en mémoire pour le panel.
const LOG_CAPACITY: usize = 100;

fn hms_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

pub const ACTION_LOG: u8 = 0;
pub const ACTION_MUTE: u8 = 1;
pub const ACTION_KICK: u8 = 2;

/// Minimum delay between two flags (log + action) for the same client, so a
/// cheater emitting ~50 voice packets/s doesn't flood the logs.
const FLAG_COOLDOWN: Duration = Duration::from_secs(2);

pub fn parse_action(action: &str) -> u8 {
    match action {
        "mute" => ACTION_MUTE,
        "kick" => ACTION_KICK,
        _ => ACTION_LOG,
    }
}

pub fn action_name(action: u8) -> &'static str {
    match action {
        ACTION_MUTE => "mute",
        ACTION_KICK => "kick",
        _ => "log",
    }
}

/// Anticheat configuration, tunable at runtime through the HTTP admin API.
/// Every field is an atomic so the voice hot path never takes a lock.
pub struct AnticheatConfig {
    pub enabled: AtomicBool,
    /// A single transmission reaching more than this percentage of connected
    /// players gets flagged (1-100).
    pub threshold_pct: AtomicU32,
    /// Absolute floor: transmissions reaching fewer recipients than this are
    /// never flagged, whatever the percentage (protects low-population servers
    /// where proximity chat alone can reach 50% of players).
    pub min_recipients: AtomicU32,
    /// Registering a voice target containing more individual sessions than
    /// this gets flagged. Legit pma-voice targets hold a phone call (1-2) or a
    /// radio channel (a few dozen at most); map-wide cheats register hundreds.
    pub max_target_sessions: AtomicU32,
    /// What to do on detection: ACTION_LOG / ACTION_MUTE / ACTION_KICK.
    pub action: AtomicU8,
    /// Ring buffer of the last detections, shown in the admin panel (newest last).
    logs: Mutex<VecDeque<String>>,
}

impl AnticheatConfig {
    pub fn new(enabled: bool, threshold_pct: u32, min_recipients: u32, max_target_sessions: u32, action: u8) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            threshold_pct: AtomicU32::new(threshold_pct.clamp(1, 100)),
            min_recipients: AtomicU32::new(min_recipients),
            max_target_sessions: AtomicU32::new(max_target_sessions),
            action: AtomicU8::new(action),
            logs: Mutex::new(VecDeque::with_capacity(LOG_CAPACITY)),
        }
    }

    /// Snapshot des derniers événements pour le panel (le plus récent en premier).
    pub fn recent_logs(&self) -> Vec<String> {
        self.logs.lock().iter().rev().cloned().collect()
    }

    fn push_log(&self, line: String) {
        let mut logs = self.logs.lock();
        if logs.len() >= LOG_CAPACITY {
            logs.pop_front();
        }
        logs.push_back(line);
    }

    /// Hot path — called once per routed voice packet, right after the send
    /// loop (the recipient count is a byproduct of that loop, so this adds no
    /// extra iteration). Cost: a handful of Relaxed atomic ops, no locks, no
    /// allocations, early-out when disabled or under threshold.
    #[inline]
    pub fn observe_emission(&self, state: &ServerState, client: &Client, recipients: u32) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        client.ac_max_recipients.fetch_max(recipients, Ordering::Relaxed);

        if recipients < self.min_recipients.load(Ordering::Relaxed) {
            return;
        }

        let active = state.active_clients.load(Ordering::Relaxed).max(1);
        let threshold_pct = self.threshold_pct.load(Ordering::Relaxed);

        if u64::from(recipients) * 100 < u64::from(active) * u64::from(threshold_pct) {
            return;
        }

        client.ac_score.fetch_add(u64::from(recipients), Ordering::Relaxed);
        self.flag(state, client, recipients, active, "voice packet");
    }

    /// Cold path — called when a client (re)registers a voice target. A legit
    /// pma-voice client registers a call/radio member list; a map-wide cheat
    /// has to register (or spam) huge session lists, caught here before the
    /// cheater even speaks.
    pub fn observe_target_registration(&self, state: &ServerState, client: &Client, sessions: u32) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        if sessions <= self.max_target_sessions.load(Ordering::Relaxed) {
            return;
        }

        // Weighted heavier than emissions: registering an abnormal target is
        // deliberate, not a side effect of a crowded area.
        client.ac_score.fetch_add(u64::from(sessions) * 10, Ordering::Relaxed);
        let active = state.active_clients.load(Ordering::Relaxed);
        self.flag(state, client, sessions, active, "voice target registration");
    }

    fn flag(&self, state: &ServerState, client: &Client, count: u32, active: u32, what: &str) {
        let now = Instant::now();
        if now.duration_since(client.ac_last_flag.load()) < FLAG_COOLDOWN {
            return;
        }
        client.ac_last_flag.store(now);
        client.ac_flags.fetch_add(1, Ordering::Relaxed);

        let action = self.action.load(Ordering::Relaxed);
        let exempt = client.ac_exempt.load(Ordering::Relaxed);
        let score = client.ac_score.load(Ordering::Relaxed);
        let flags = client.ac_flags.load(Ordering::Relaxed);

        tracing::warn!(
            "[ANTICHEAT] {} reached {} recipients / {} connected via {} (score={}, flags={}, action={}{})",
            client,
            count,
            active,
            what,
            score,
            flags,
            action_name(action),
            if exempt { ", EXEMPT" } else { "" },
        );

        let act = if exempt { "exempt" } else { action_name(action) };
        self.push_log(format!(
            "{} {} — {}/{} via {} (score {}) → {}",
            hms_now(),
            client.get_name(),
            count,
            active,
            what,
            score,
            act,
        ));

        if exempt {
            return;
        }

        match action {
            ACTION_MUTE => client.set_mute(true),
            ACTION_KICK => state.add_client_to_disconnect_queue(client.session_id, DisconnectReason::Anticheat),
            _ => {}
        }
    }
}
