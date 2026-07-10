use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use scc::ebr::Guard;

use crate::client::ClientArc;
use crate::error::DisconnectReason;
use crate::state::{ServerState, ServerStateRef};

/// Nombre d'événements anticheat gardés en mémoire pour le panel.
const LOG_CAPACITY: usize = 100;
/// Période d'évaluation globale (calcule reach / mutualité / listens pour tous).
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Délai minimum entre deux flags du même client (anti-spam).
const FLAG_COOLDOWN: Duration = Duration::from_secs(3);

pub const ACTION_LOG: u8 = 0;
pub const ACTION_MUTE: u8 = 1;
pub const ACTION_KICK: u8 = 2;

/// Clés pour la fenêtre glissante : on distingue un id de channel d'un id de
/// session pour compter des cibles distinctes sans collision.
pub fn key_channel(id: u32) -> u64 {
    id as u64
}
pub fn key_session(id: u32) -> u64 {
    (1u64 << 40) | id as u64
}

fn hms_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

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

/// Configuration anticheat, réglable à chaud via l'API HTTP. Tous les champs
/// sont atomiques : le hot path voix ne prend jamais de lock.
pub struct AnticheatConfig {
    pub enabled: AtomicBool,
    /// Un joueur qui atteint plus de ce % des connectés est "haute portée".
    pub threshold_pct: AtomicU32,
    /// Plancher : en dessous de ce nombre de destinataires, jamais flaggé
    /// (protège les petits serveurs où la proximité touche déjà 50 %).
    pub min_recipients: AtomicU32,
    /// Haute portée + mutualité SOUS ce % = cheat "talk map-wide". Une foule
    /// légitime a une mutualité élevée (tout le monde se cible mutuellement),
    /// un cheater ~0 % (personne ne le cible en retour). C'est le discriminant.
    pub mutuality_max_pct: AtomicU32,
    /// Fenêtre (secondes) pour compter les cibles DISTINCTES — attrape le
    /// chunking (cibler 30 par 30 en tournant reste sous le seuil instantané
    /// mais explose sur la fenêtre).
    pub window_secs: AtomicU32,
    /// Nombre de cycles suspects consécutifs avant d'agir (anti faux positif).
    pub strikes_required: AtomicU32,
    /// Écouter plus de ce nombre de channels = "listen map-wide" (espionnage
    /// via ChannelListen). 0 = désactive ce détecteur.
    pub listen_max: AtomicU32,
    /// Action sur détection : ACTION_LOG / ACTION_MUTE / ACTION_KICK.
    pub action: AtomicU8,
    /// Ring buffer des dernières détections (affiché dans le panel).
    logs: Mutex<VecDeque<String>>,
}

impl AnticheatConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enabled: bool,
        threshold_pct: u32,
        min_recipients: u32,
        mutuality_max_pct: u32,
        window_secs: u32,
        strikes_required: u32,
        listen_max: u32,
        action: u8,
    ) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            threshold_pct: AtomicU32::new(threshold_pct.clamp(1, 100)),
            min_recipients: AtomicU32::new(min_recipients),
            mutuality_max_pct: AtomicU32::new(mutuality_max_pct.min(100)),
            window_secs: AtomicU32::new(window_secs.max(1)),
            strikes_required: AtomicU32::new(strikes_required.max(1)),
            listen_max: AtomicU32::new(listen_max),
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

    /// Appelé depuis le handler VoiceTarget à chaque (ré)enregistrement : nourrit
    /// la fenêtre glissante avec les cibles courantes. Coût : quelques insertions
    /// derrière un lock par-client (pas de lock global).
    pub fn record_targets(&self, client: &ClientArc, keys: &[u64]) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        let window = Duration::from_secs(self.window_secs.load(Ordering::Relaxed).max(1) as u64);
        let mut w = client.ac_window.lock();
        for &k in keys {
            w.insert(k, now);
        }
        w.retain(|_, t| now.duration_since(*t) <= window);
    }

    /// Hot path voix : simple stat du plus grand nombre de destinataires atteint.
    pub fn note_emission(&self, client: &ClientArc, recipients: u32) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        client.ac_max_recipients.fetch_max(recipients, Ordering::Relaxed);
    }

    /// Évaluation périodique de TOUS les clients. Calcule, par client :
    /// - reach instant (joueurs réellement atteignables par sa voice-target),
    /// - reach fenêtre (cibles distinctes sur `window_secs` → chunking),
    /// - mutualité (part des cibles qui le ciblent en retour → foule vs cheat),
    /// - listens (nombre de channels écoutés → espionnage).
    pub async fn sample(&self, state: &ServerState) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        let window = Duration::from_secs(self.window_secs.load(Ordering::Relaxed).max(1) as u64);

        // Membres par channel (clients + listeners) et nombre de channels écoutés par session.
        let mut chan_members: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut listen_count: HashMap<u32, u32> = HashMap::new();
        {
            let mut it = state.channels.first_entry_async().await;
            while let Some(e) = it {
                let ch = e.get();
                let cid = ch.id;
                {
                    // scope the (non-Send) Guard so it is dropped before the await below
                    let guard = Guard::new();
                    for (s, _) in ch.clients.iter(&guard) {
                        chan_members.entry(cid).or_default().push(*s);
                    }
                    for (s, _) in ch.listeners.iter(&guard) {
                        chan_members.entry(cid).or_default().push(*s);
                        *listen_count.entry(*s).or_default() += 1;
                    }
                }
                it = e.next_async().await;
            }
        }

        // Snapshot des clients + leurs ensembles de cibles (channels / sessions).
        struct Snap {
            c: ClientArc,
            chan: u32,
            chans: HashSet<u32>,
            sesss: HashSet<u32>,
        }
        let mut snaps: Vec<Snap> = Vec::new();
        {
            let mut it = state.clients.first_entry_async().await;
            while let Some(e) = it {
                let c = e.get().clone();
                let chan = c.channel_id.load(Ordering::Relaxed);
                let mut chans = HashSet::new();
                let mut sesss = HashSet::new();
                {
                    let guard = Guard::new();
                    for slot in c.targets.iter() {
                        for (k, _) in slot.channels.iter(&guard) {
                            chans.insert(*k);
                        }
                        for (k, _) in slot.sessions.iter(&guard) {
                            sesss.insert(*k);
                        }
                    }
                }
                snaps.push(Snap { c, chan, chans, sesss });
                it = e.next_async().await;
            }
        }
        let active = snaps.len() as u32;
        if active == 0 {
            return;
        }

        let mut sess_to_idx: HashMap<u32, usize> = HashMap::new();
        for (i, s) in snaps.iter().enumerate() {
            sess_to_idx.insert(s.c.session_id, i);
        }

        let threshold_pct = self.threshold_pct.load(Ordering::Relaxed);
        let min_recipients = self.min_recipients.load(Ordering::Relaxed);
        let mutuality_max = self.mutuality_max_pct.load(Ordering::Relaxed);
        let strikes_required = self.strikes_required.load(Ordering::Relaxed).max(1);
        let listen_max = self.listen_max.load(Ordering::Relaxed);

        for s in &snaps {
            let c = &s.c;
            let my_session = c.session_id;

            // Joueurs réellement atteints (résolus depuis channels + sessions), self exclu.
            let mut reached: HashSet<u32> = HashSet::new();
            for &sess in &s.sesss {
                if sess != my_session {
                    reached.insert(sess);
                }
            }
            for &ch in &s.chans {
                if let Some(list) = chan_members.get(&ch) {
                    for &sess in list {
                        if sess != my_session {
                            reached.insert(sess);
                        }
                    }
                }
            }
            let reach_instant = reached.len() as u32;

            // Cibles distinctes sur la fenêtre (chunking).
            let window_reach = {
                let mut w = c.ac_window.lock();
                w.retain(|_, t| now.duration_since(*t) <= window);
                w.len() as u32
            };
            let reach = reach_instant.max(window_reach);

            // Mutualité sur l'ensemble atteint courant (255 = non applicable).
            let mutuality = if reached.is_empty() {
                255u32
            } else {
                let mut mutual = 0u32;
                for &r in &reached {
                    if let Some(&idx) = sess_to_idx.get(&r) {
                        let o = &snaps[idx];
                        // O me cible-t-il en retour ? (ma session dans ses sessions,
                        // ou mon channel dans ses channels)
                        if o.sesss.contains(&my_session) || o.chans.contains(&s.chan) {
                            mutual += 1;
                        }
                    }
                }
                (mutual * 100) / reach_instant.max(1)
            };

            let listens = *listen_count.get(&my_session).unwrap_or(&0);

            c.ac_reach_instant.store(reach_instant, Ordering::Relaxed);
            c.ac_reach_window.store(window_reach, Ordering::Relaxed);
            c.ac_mutuality.store(mutuality, Ordering::Relaxed);
            c.ac_listen_count.store(listens, Ordering::Relaxed);

            // Conditions de suspicion.
            let high_reach = reach >= min_recipients && (reach as u64) * 100 >= (active as u64) * (threshold_pct as u64);
            let talk_cheat = high_reach && mutuality != 255 && mutuality <= mutuality_max;
            let spy = listen_max > 0 && listens > listen_max;

            if talk_cheat || spy {
                let strikes = c.ac_strikes.fetch_add(1, Ordering::Relaxed) + 1;
                if strikes >= strikes_required {
                    let reason = if talk_cheat && spy {
                        "talk+listen map-wide"
                    } else if talk_cheat {
                        "talk map-wide (mutualité faible)"
                    } else {
                        "listen map-wide (espionnage)"
                    };
                    let metric = if spy && !talk_cheat { listens } else { reach };
                    c.ac_score.fetch_add(metric as u64, Ordering::Relaxed);
                    self.flag(state, c, reach, listens, active, mutuality, reason);
                }
            } else {
                let cur = c.ac_strikes.load(Ordering::Relaxed);
                if cur > 0 {
                    c.ac_strikes.store(cur - 1, Ordering::Relaxed);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn flag(&self, state: &ServerState, client: &ClientArc, reach: u32, listens: u32, active: u32, mutuality: u32, reason: &str) {
        let now = Instant::now();
        if now.duration_since(client.ac_last_flag.load()) < FLAG_COOLDOWN {
            return;
        }
        client.ac_last_flag.store(now);
        client.ac_flags.fetch_add(1, Ordering::Relaxed);

        let action = self.action.load(Ordering::Relaxed);
        let exempt = client.ac_exempt.load(Ordering::Relaxed);
        let score = client.ac_score.load(Ordering::Relaxed);
        let mut_s = if mutuality == 255 { "n/a".to_string() } else { format!("{}%", mutuality) };

        tracing::warn!(
            "[ANTICHEAT] {} — {} — reach {}/{}, mutualité {}, listens {} (score={}, action={}{})",
            client,
            reason,
            reach,
            active,
            mut_s,
            listens,
            score,
            action_name(action),
            if exempt { ", EXEMPT" } else { "" },
        );

        self.push_log(format!(
            "{} {} — {} — reach {}/{}, mut {}, listens {} (score {}) → {}",
            hms_now(),
            client.get_name(),
            reason,
            reach,
            active,
            mut_s,
            listens,
            score,
            if exempt { "exempt" } else { action_name(action) },
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

/// Boucle d'évaluation périodique, lancée comme tâche au démarrage.
pub async fn run_sampler(state: ServerStateRef) {
    loop {
        tokio::time::sleep(SAMPLE_INTERVAL).await;
        state.anticheat.sample(&state).await;
    }
}
