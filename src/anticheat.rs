use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use scc::ebr::Guard;
use serde::{Deserialize, Serialize};

use crate::client::ClientArc;
use crate::error::DisconnectReason;
use crate::state::{ServerState, ServerStateRef};

/// Une entrée de la liste de bans. On matche par IP OU par nom (partie après
/// "[id] " du pseudo mumble, car le [id] change à chaque session).
#[derive(Clone, Serialize, Deserialize)]
pub struct BanEntry {
    pub id: u64,
    /// "ban" (refuse la connexion) ou "mute" (mute persistant à chaque connexion).
    #[serde(default = "default_kind")]
    pub kind: String,
    pub ip: Option<String>,
    pub name: Option<String>,
    pub display: String,
    pub reason: String,
    pub at: String,
}

fn default_kind() -> String {
    "ban".to_string()
}

/// Extrait la partie "nom" du pseudo mumble FiveM : "[36] ReviewB" -> "ReviewB".
pub fn name_part(username: &str) -> String {
    match username.find("] ") {
        Some(pos) => username[pos + 2..].trim().to_string(),
        None => username.trim().to_string(),
    }
}

/// Liste de bans persistée sur disque (JSON). Vérifiée à chaque connexion, donc
/// une reconnexion FiveM est refusée immédiatement.
pub struct BanList {
    bans: Mutex<Vec<BanEntry>>,
    next_id: AtomicU64,
    file: Option<PathBuf>,
}

impl BanList {
    pub fn load(file: Option<PathBuf>) -> Self {
        let mut bans = Vec::new();
        let mut next = 1u64;
        if let Some(ref f) = file {
            if let Ok(data) = fs::read_to_string(f) {
                if let Ok(v) = serde_json::from_str::<Vec<BanEntry>>(&data) {
                    next = v.iter().map(|b| b.id).max().unwrap_or(0) + 1;
                    bans = v;
                }
            }
        }
        tracing::info!("banlist: {} entrée(s) chargée(s)", bans.len());
        Self {
            bans: Mutex::new(bans),
            next_id: AtomicU64::new(next),
            file,
        }
    }

    fn save(&self) {
        if let Some(ref f) = self.file {
            let data = serde_json::to_string_pretty(&*self.bans.lock()).unwrap_or_default();
            if let Err(e) = fs::write(f, data) {
                tracing::error!("banlist: échec écriture {}: {}", f.display(), e);
            }
        }
    }

    fn matches(entry: &BanEntry, ip: &str, name_part: &str) -> bool {
        if let Some(ref bip) = entry.ip {
            if bip == ip {
                return true;
            }
        }
        if let Some(ref bn) = entry.name {
            if !bn.is_empty() && bn.eq_ignore_ascii_case(name_part) {
                return true;
            }
        }
        false
    }

    /// Renvoie la raison si (ip, name_part) matche un BAN.
    pub fn is_banned(&self, ip: &str, name_part: &str) -> Option<String> {
        let bans = self.bans.lock();
        bans.iter()
            .find(|b| b.kind == "ban" && Self::matches(b, ip, name_part))
            .map(|b| b.reason.clone())
    }

    /// True si (ip, name_part) matche un MUTE persistant.
    pub fn is_muted(&self, ip: &str, name_part: &str) -> bool {
        let bans = self.bans.lock();
        bans.iter().any(|b| b.kind == "mute" && Self::matches(b, ip, name_part))
    }

    pub fn add(&self, kind: &str, ip: Option<String>, name: Option<String>, display: String, reason: String) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.bans.lock().push(BanEntry {
            id,
            kind: kind.to_string(),
            ip,
            name,
            display,
            reason,
            at: hms_now(),
        });
        self.save();
    }

    pub fn remove(&self, id: u64) {
        self.bans.lock().retain(|b| b.id != id);
        self.save();
    }

    /// Retire les entrées d'un `kind` donné qui matchent ce joueur (pour Débloquer).
    pub fn remove_matching(&self, ip: &str, name_part: &str, kind: &str) {
        self.bans.lock().retain(|b| !(b.kind == kind && Self::matches(b, ip, name_part)));
        self.save();
    }

    pub fn list(&self) -> Vec<BanEntry> {
        self.bans.lock().clone()
    }
}

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
/// Fenêtre d'observation des oscillations de position (retours A→B→A).
const POS_OSC_WINDOW: Duration = Duration::from_secs(30);
/// Taille max de l'historique de lieux par client.
const POS_HIST_CAP: usize = 16;
/// Une position plus vieille que ça n'est plus comparable (le joueur n'émet
/// plus, il a pu bouger) — utilisée par l'incohérence position↔cibles.
const POS_FRESH: Duration = Duration::from_secs(10);

/// Cap dur d'émetteurs mémorisés par joueur pour « qui lui a parlé » —
/// borne stricte de mémoire (~100 octets/entrée → ≤ ~6 Ko par joueur).
const HEARD_CAP: usize = 64;
/// Snapshot disque de la mémoire « qui a parlé » tous les N cycles du sampler
/// (30 × 2 s = 60 s) — jamais sur le chemin voix.
const HEARD_FLUSH_EVERY: u32 = 30;

/// Une entrée « cet émetteur a parlé à ce joueur » (panel qui-parle-à-qui).
/// Clé de la map = nom (partie après "[id] ") → un cheater qui déco/reco ne
/// crée pas de doublon, sa ligne est juste remise à jour. Nom + IP figés →
/// survivent à sa déconnexion.
#[derive(Debug)]
pub struct HeardEntry {
    /// Dernière session connue de l'émetteur (info panel).
    pub session_id: u32,
    pub name: String,
    pub ip: String,
    pub last: Instant,
    /// Secondes de parole cumulées (1 tick max par seconde, throttle émetteur).
    pub secs: u32,
}

/// Version sérialisée d'une HeardEntry pour le snapshot /data/heard.json
/// (Instant → epoch pour survivre aux restarts du conteneur).
#[derive(Clone, Serialize, Deserialize)]
struct PersistedHeardEntry {
    session_id: u32,
    name: String,
    ip: String,
    /// Epoch (secondes) de la dernière parole.
    epoch: u64,
    secs: u32,
}

/// Format du fichier : nom du joueur écouté → (nom émetteur → entrée).
type PersistedHeard = HashMap<String, HashMap<String, PersistedHeardEntry>>;

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
    /// Distance (m) séparant deux "lieux" distincts pour le suivi d'oscillation
    /// (et rayon de retour = moitié). 0 = ne suit plus les lieux du tout.
    pub pos_osc_dist: AtomicU32,
    /// Nombre de RETOURS vers un lieu antérieur (A→B→A…) dans la fenêtre avant
    /// de flagger un spoof multi-positions. 0 = détection off (suivi affiché
    /// quand même). Un tp légitime (respawn, interior, bucket) = 0 retour.
    pub pos_osc_max: AtomicU32,
    /// Distance (m) au-delà de laquelle une cible PROXIMITÉ (channel) est
    /// "hors de portée" de la position déclarée de l'émetteur.
    pub pos_far_dist: AtomicU32,
    /// Minimum de cibles proximité lointaines (à position fraîche) pour flagger
    /// une incohérence position/cibles. 0 = détection off (compte affiché).
    pub pos_far_min: AtomicU32,
    /// Part minimale (%) de cibles lointaines parmi les cibles à position fraîche.
    pub pos_far_pct: AtomicU32,
    /// Rétention (secondes) de la mémoire « qui a parlé à ce joueur » du panel.
    pub heard_secs: AtomicU32,
    /// Fichier de persistance de « qui a parlé » (snapshot 60 s, volume /data).
    pub heard_file: Mutex<Option<PathBuf>>,
    /// Listes rechargées du disque, en attente de la reconnexion de leur
    /// propriétaire (clé = nom du joueur écouté).
    heard_seed: Mutex<PersistedHeard>,
    /// Action sur détection : ACTION_LOG / ACTION_MUTE / ACTION_KICK.
    pub action: AtomicU8,
    /// URL du webhook Discord (POST à chaque flag). Vide = désactivé.
    pub webhook: Mutex<Option<String>>,
    /// URL du panel de CE serveur (ex. http://ip:13000/panel), mise dans l'embed
    /// Discord pour identifier/ouvrir directement le serveur qui a flaggé.
    pub panel_url: Mutex<Option<String>>,
    /// Label du serveur (par défaut l'adresse d'écoute) affiché dans l'embed.
    pub server_label: String,
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
        webhook: Option<String>,
        panel_url: Option<String>,
        server_label: String,
    ) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            threshold_pct: AtomicU32::new(threshold_pct.clamp(1, 100)),
            min_recipients: AtomicU32::new(min_recipients),
            mutuality_max_pct: AtomicU32::new(mutuality_max_pct.min(100)),
            window_secs: AtomicU32::new(window_secs.max(1)),
            strikes_required: AtomicU32::new(strikes_required.max(1)),
            listen_max: AtomicU32::new(listen_max),
            pos_osc_dist: AtomicU32::new(300),
            // Oscillation OFF par défaut (activable au panel) : un admin qui se
            // tp en boucle entre 2 spots en parlant pourrait matcher. Le suivi
            // (colonne Oscill.) reste affiché pour calibrer avant d'activer.
            pos_osc_max: AtomicU32::new(0),
            // Incohérence position↔cibles ON par défaut : triple garde (cibles
            // proximité uniquement, positions fraîches, mutualité basse exigée)
            // → une foule, un respawn ou le téléphone ne peuvent pas la déclencher.
            // min à 15 (= min_recipients) : un spectate staff plafonne à ~6 cibles
            // lointaines, un staff en rondes (tp de groupe en groupe en parlant) à
            // ~12 (vus en prod le 2026-07-10, tous légitimes) ; un cheat map-wide
            // en a 100+. Même plancher que le talk map-wide : sous 15 destinataires,
            // on ne flag pas.
            pos_far_dist: AtomicU32::new(500),
            pos_far_min: AtomicU32::new(15),
            pos_far_pct: AtomicU32::new(70),
            // 15 min : assez pour qu'un streamer signale le harcèlement après
            // coup ; la RAM reste bornée par HEARD_CAP, pas par la durée.
            heard_secs: AtomicU32::new(900),
            heard_file: Mutex::new(None),
            heard_seed: Mutex::new(HashMap::new()),
            action: AtomicU8::new(action),
            webhook: Mutex::new(webhook.filter(|s| !s.is_empty())),
            panel_url: Mutex::new(panel_url.filter(|s| !s.is_empty())),
            server_label,
            logs: Mutex::new(VecDeque::with_capacity(LOG_CAPACITY)),
        }
    }

    /// Snapshot des derniers événements pour le panel (le plus récent en premier).
    pub fn recent_logs(&self) -> Vec<String> {
        self.logs.lock().iter().rev().cloned().collect()
    }

    /// Vide le ring buffer de logs (bouton "clear" du panel).
    pub fn clear_logs(&self) {
        self.logs.lock().clear();
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

    /// Marque « speaker a parlé à listener » (mémoire du panel qui-parle-à-qui).
    /// Appelé depuis le hot path voix AU PLUS 1×/seconde par émetteur (throttle
    /// côté émetteur dans voice_packet.rs) : un upsert derrière un lock
    /// par-destinataire, quelques dizaines de ns — invisible face au routage.
    /// Si l'émetteur reparle, seule l'heure (et le cumul) est mise à jour.
    pub fn note_heard(&self, speaker: &ClientArc, listener: &ClientArc) {
        let now = Instant::now();
        let key = name_part(speaker.get_name().as_str());
        let mut heard = listener.ac_heard.lock();
        if heard.len() >= HEARD_CAP && !heard.contains_key(&key) {
            let retention = Duration::from_secs(self.heard_secs.load(Ordering::Relaxed).max(10) as u64);
            heard.retain(|_, e| now.duration_since(e.last) <= retention);
            if heard.len() >= HEARD_CAP {
                // Toujours plein d'entrées récentes : on évince la plus ancienne.
                if let Some(k) = heard.iter().min_by_key(|(_, e)| e.last).map(|(k, _)| k.clone()) {
                    heard.remove(&k);
                }
            }
        }
        let e = heard.entry(key).or_insert_with(|| HeardEntry {
            session_id: speaker.session_id,
            name: speaker.get_name().to_string(),
            ip: speaker.peer_ip.to_string(),
            last: now,
            secs: 0,
        });
        if e.session_id != speaker.session_id {
            // L'émetteur s'est reconnecté : on rafraîchit session/nom/IP (pas
            // de doublon, la ligne est juste mise à jour).
            e.session_id = speaker.session_id;
            e.name = speaker.get_name().to_string();
            e.ip = speaker.peer_ip.to_string();
        }
        e.last = now;
        e.secs += 1;
    }

    /// Configure la persistance « qui a parlé » et recharge le snapshot disque
    /// (survit aux restarts/recréations du conteneur, comme bans.json).
    pub fn init_heard_persistence(&self, path: PathBuf) {
        let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let retention = self.heard_secs.load(Ordering::Relaxed).max(10) as u64;
        match fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<PersistedHeard>(&data) {
                Ok(mut seed) => {
                    // Purge ce qui a expiré pendant l'arrêt.
                    for lists in seed.values_mut() {
                        lists.retain(|_, e| now_epoch.saturating_sub(e.epoch) <= retention);
                    }
                    seed.retain(|_, l| !l.is_empty());
                    let n: usize = seed.values().map(|l| l.len()).sum();
                    tracing::info!(
                        "[ANTICHEAT] mémoire 'qui a parlé' rechargée depuis {} : {} joueurs, {} entrées",
                        path.display(), seed.len(), n,
                    );
                    *self.heard_seed.lock() = seed;
                }
                Err(e) => tracing::warn!("[ANTICHEAT] heard file illisible ({}) : {}", path.display(), e),
            },
            // Pas encore de fichier : normal au premier démarrage.
            Err(_) => {}
        }
        *self.heard_file.lock() = Some(path);
    }

    /// À la connexion d'un client : réinjecte sa liste « qui lui a parlé »
    /// depuis le snapshot disque.
    pub fn seed_heard(&self, client: &ClientArc) {
        let key = name_part(client.get_name().as_str());
        let Some(entries) = self.heard_seed.lock().remove(&key) else { return };
        let now = Instant::now();
        let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut heard = client.ac_heard.lock();
        for (speaker, e) in entries {
            let ago = now_epoch.saturating_sub(e.epoch);
            let last = now.checked_sub(Duration::from_secs(ago)).unwrap_or(now);
            heard.entry(speaker).or_insert(HeardEntry {
                session_id: e.session_id,
                name: e.name,
                ip: e.ip,
                last,
                secs: e.secs,
            });
        }
    }

    /// Hot path voix : la position 3D du joueur (si le framework l'envoie).
    /// Suit les "lieux" visités et compte les RETOURS vers un lieu antérieur
    /// lointain (oscillation A→B→A→B = spoof multi-positions). Un téléport
    /// légitime (respawn, interior, bucket) est un aller simple : 0 retour.
    pub fn note_position(&self, client: &ClientArc, x: f32, y: f32, z: f32) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        let had = client.ac_has_pos.swap(true, Ordering::Relaxed);
        if had {
            let dt = now.duration_since(client.ac_last_pos_time.load()).as_secs_f32();
            // Vitesse max sur dt court : info panel uniquement (les tp légitimes
            // FiveM rendent la vitesse inexploitable comme signal — prouvé en prod).
            if dt > 0.0 && dt < 0.5 {
                let dx = x - client.ac_pos_x.load(Ordering::Relaxed);
                let dy = y - client.ac_pos_y.load(Ordering::Relaxed);
                let dz = z - client.ac_pos_z.load(Ordering::Relaxed);
                let speed = (dx * dx + dy * dy + dz * dz).sqrt() / dt;
                if speed > client.ac_max_speed.load(Ordering::Relaxed) {
                    client.ac_max_speed.store(speed, Ordering::Relaxed);
                }
            }
        }
        client.ac_pos_x.store(x, Ordering::Relaxed);
        client.ac_pos_y.store(y, Ordering::Relaxed);
        client.ac_pos_z.store(z, Ordering::Relaxed);
        client.ac_last_pos_time.store(now);

        // Suivi des lieux : on n'enregistre une ancre que si on s'est éloigné de
        // plus de pos_osc_dist du dernier lieu (lock pris seulement dans ce cas
        // rare, jamais pendant une conversation immobile).
        let osc_dist = self.pos_osc_dist.load(Ordering::Relaxed) as f32;
        if osc_dist <= 0.0 {
            return;
        }
        let mut hist = client.ac_pos_hist.lock();
        let moved = match hist.back() {
            Some(&(_, ax, ay, az, _)) => {
                let d2 = (x - ax) * (x - ax) + (y - ay) * (y - ay) + (z - az) * (z - az);
                d2 > osc_dist * osc_dist
            }
            None => true,
        };
        if moved {
            // Retour = on ré-atterrit près d'un lieu déjà visité dans la fenêtre.
            // (Jamais le lieu précédent : on vient d'en partir de > osc_dist.)
            let rr = (osc_dist * 0.5) * (osc_dist * 0.5);
            let is_return = hist.iter().any(|&(_, hx, hy, hz, _)| {
                (x - hx) * (x - hx) + (y - hy) * (y - hy) + (z - hz) * (z - hz) < rr
            });
            hist.push_back((now, x, y, z, is_return));
            while hist.len() > POS_HIST_CAP {
                hist.pop_front();
            }
            hist.retain(|&(t, ..)| now.duration_since(t) <= POS_OSC_WINDOW);
            let returns = hist.iter().filter(|&&(.., r)| r).count() as u32;
            client.ac_pos_jumps.store(returns, Ordering::Relaxed);
        }
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
        let pos_osc_max = self.pos_osc_max.load(Ordering::Relaxed);
        let pos_far_dist = self.pos_far_dist.load(Ordering::Relaxed) as f32;
        let pos_far_min = self.pos_far_min.load(Ordering::Relaxed);
        let pos_far_pct = self.pos_far_pct.load(Ordering::Relaxed);

        // Position fraîche de chaque client (None = pas d'émission récente ou
        // framework en volume-override : position inexploitable, on s'abstient).
        let pos_of: Vec<Option<(f32, f32, f32)>> = snaps
            .iter()
            .map(|s| {
                let c = &s.c;
                if c.ac_has_pos.load(Ordering::Relaxed)
                    && now.duration_since(c.ac_last_pos_time.load()) <= POS_FRESH
                {
                    Some((
                        c.ac_pos_x.load(Ordering::Relaxed),
                        c.ac_pos_y.load(Ordering::Relaxed),
                        c.ac_pos_z.load(Ordering::Relaxed),
                    ))
                } else {
                    None
                }
            })
            .collect();

        for (my_idx, s) in snaps.iter().enumerate() {
            let c = &s.c;
            let my_session = c.session_id;

            // Joueurs réellement atteints (résolus depuis channels + sessions), self exclu.
            let mut reached: HashSet<u32> = HashSet::new();
            for &sess in &s.sesss {
                if sess != my_session {
                    reached.insert(sess);
                }
            }
            // Cibles PROXIMITÉ (channels), gardées à part : elles seules sont
            // soumises au contrôle de distance (les sessions = téléphone/radio,
            // légitimement longue distance, en sont exclues).
            let mut reached_chan: HashSet<u32> = HashSet::new();
            for &ch in &s.chans {
                if let Some(list) = chan_members.get(&ch) {
                    for &sess in list {
                        if sess != my_session {
                            reached.insert(sess);
                            reached_chan.insert(sess);
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

            // Incohérence position↔cibles : parmi mes cibles proximité à position
            // fraîche, combien sont hors de portée de MA position déclarée ?
            // Physiquement impossible en proximité légitime (<100 m) — et le
            // cheater ne peut pas l'éviter : ses cibles sont partout sur la map.
            let (far, fresh_total) = match pos_of[my_idx] {
                Some((sx, sy, sz)) => {
                    let mut far = 0u32;
                    let mut tot = 0u32;
                    for &r in &reached_chan {
                        if let Some(&idx) = sess_to_idx.get(&r) {
                            if let Some((qx, qy, qz)) = pos_of[idx] {
                                tot += 1;
                                let d2 = (sx - qx) * (sx - qx)
                                    + (sy - qy) * (sy - qy)
                                    + (sz - qz) * (sz - qz);
                                if d2 > pos_far_dist * pos_far_dist {
                                    far += 1;
                                }
                            }
                        }
                    }
                    (far, tot)
                }
                None => (0, 0),
            };
            c.ac_far_targets.store(far, Ordering::Relaxed);

            // Oscillations : ré-expire la fenêtre même si le joueur ne pose plus
            // de nouvelle ancre (sinon le compteur resterait figé).
            let pos_osc = {
                let mut hist = c.ac_pos_hist.lock();
                hist.retain(|&(t, ..)| now.duration_since(t) <= POS_OSC_WINDOW);
                let returns = hist.iter().filter(|&&(.., r)| r).count() as u32;
                c.ac_pos_jumps.store(returns, Ordering::Relaxed);
                returns
            };

            c.ac_reach_instant.store(reach_instant, Ordering::Relaxed);
            c.ac_reach_window.store(window_reach, Ordering::Relaxed);
            c.ac_mutuality.store(mutuality, Ordering::Relaxed);
            c.ac_listen_count.store(listens, Ordering::Relaxed);

            // Conditions de suspicion.
            let low_mutuality = mutuality != 255 && mutuality <= mutuality_max;
            let high_reach = reach >= min_recipients && (reach as u64) * 100 >= (active as u64) * (threshold_pct as u64);
            let talk_cheat = high_reach && low_mutuality;
            let spy = listen_max > 0 && listens > listen_max;
            let pos_spoof = pos_osc_max > 0 && pos_osc >= pos_osc_max;
            // Doublement gardée : position incohérente ET mutualité basse (une
            // foule légitime ou un framework mono-channel restent mutuels).
            let pos_far_cheat = pos_far_min > 0
                && far >= pos_far_min
                && far * 100 >= fresh_total * pos_far_pct.min(100).max(1)
                && low_mutuality;

            if talk_cheat || spy || pos_spoof || pos_far_cheat {
                let strikes = c.ac_strikes.fetch_add(1, Ordering::Relaxed) + 1;
                if strikes >= strikes_required {
                    let mut parts: Vec<&str> = Vec::new();
                    if talk_cheat {
                        parts.push("talk map-wide (mutualité faible)");
                    }
                    if spy {
                        parts.push("listen map-wide (espionnage)");
                    }
                    if pos_spoof {
                        parts.push("spoof position (oscillation multi-lieux)");
                    }
                    if pos_far_cheat {
                        parts.push("cibles hors de portée (position incohérente)");
                    }
                    let reason = parts.join(" + ");
                    let metric = reach.max(listens).max(far).max(pos_osc);
                    c.ac_score.fetch_add(metric as u64, Ordering::Relaxed);
                    self.flag(state, c, reach, listens, active, mutuality, &reason);
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

        // Notification Discord (rate-limitée par client, bien plus espacée que les logs).
        let webhook = self.webhook.lock().clone();
        if let Some(url) = webhook {
            const WEBHOOK_COOLDOWN: Duration = Duration::from_secs(60);
            if now.duration_since(client.ac_last_webhook.load()) >= WEBHOOK_COOLDOWN {
                client.ac_last_webhook.store(now);
                let server_field = match self.panel_url.lock().clone() {
                    Some(p) => format!("[Ouvrir le panel]({})\n`{}`", p, self.server_label),
                    None => format!("`{}`", self.server_label),
                };
                let payload = serde_json::json!({
                    "username": "Anticheat Mumble",
                    "embeds": [{
                        "title": "🚨 Détection anticheat",
                        "color": 15158332u32,
                        "fields": [
                            {"name": "Joueur", "value": client.get_name().to_string(), "inline": true},
                            {"name": "IP", "value": client.peer_ip.to_string(), "inline": true},
                            {"name": "Client", "value": client.version_release.clone(), "inline": true},
                            {"name": "Raison", "value": reason},
                            {"name": "Portée / Mutualité", "value": format!("{}/{} joueurs, mut {}", reach, active, mut_s), "inline": true},
                            {"name": "Listens / Score", "value": format!("{} / {}", listens, score), "inline": true},
                            {"name": "Serveur", "value": server_field}
                        ]
                    }]
                });
                post_discord(url, payload);
            }
        }

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

/// POST fire-and-forget vers un webhook Discord (dans un thread bloquant pour
/// ne jamais impacter la voix). Ignore les erreurs.
fn post_discord(url: String, payload: serde_json::Value) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = ureq::post(&url).timeout(Duration::from_secs(5)).send_json(payload) {
            tracing::warn!("[ANTICHEAT] webhook Discord échec: {}", e);
        }
    });
}

/// Boucle d'évaluation périodique, lancée comme tâche au démarrage.
pub async fn run_sampler(state: ServerStateRef) {
    let mut ticks: u32 = 0;
    loop {
        tokio::time::sleep(SAMPLE_INTERVAL).await;
        state.anticheat.sample(&state).await;
        ticks = ticks.wrapping_add(1);
        if ticks % HEARD_FLUSH_EVERY == 0 {
            flush_heard(&state).await;
        }
    }
}

/// Snapshot JSON de toutes les listes « qui a parlé » vers le volume /data —
/// hors du chemin voix (sampler), écrit en .tmp puis rename (atomique). Les
/// listes des joueurs pas encore reconnectés (seed) sont conservées.
async fn flush_heard(state: &ServerStateRef) {
    let Some(path) = state.anticheat.heard_file.lock().clone() else { return };
    if !state.anticheat.enabled.load(Ordering::Relaxed) {
        return;
    }
    let retention = state.anticheat.heard_secs.load(Ordering::Relaxed).max(10) as u64;
    let now = Instant::now();
    let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

    let mut out: PersistedHeard = HashMap::new();
    {
        let mut it = state.clients.first_entry_async().await;
        while let Some(e) = it {
            let c = e.get();
            let lists: HashMap<String, PersistedHeardEntry> = {
                let h = c.ac_heard.lock();
                h.iter()
                    .filter(|(_, en)| now.duration_since(en.last).as_secs() <= retention)
                    .map(|(k, en)| {
                        (
                            k.clone(),
                            PersistedHeardEntry {
                                session_id: en.session_id,
                                name: en.name.clone(),
                                ip: en.ip.clone(),
                                epoch: now_epoch.saturating_sub(now.duration_since(en.last).as_secs()),
                                secs: en.secs,
                            },
                        )
                    })
                    .collect()
            };
            if !lists.is_empty() {
                out.insert(name_part(c.get_name().as_str()), lists);
            }
            it = e.next_async().await;
        }
    }
    {
        // Les listes rechargées dont le propriétaire n'est pas revenu restent
        // persistées (elles expireront via la rétention).
        let seed = state.anticheat.heard_seed.lock();
        for (k, v) in seed.iter() {
            out.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    match serde_json::to_vec(&out) {
        Ok(json) => {
            let tmp = path.with_extension("tmp");
            if let Err(e) = fs::write(&tmp, &json).and_then(|_| fs::rename(&tmp, &path)) {
                tracing::warn!("[ANTICHEAT] échec écriture {} : {}", path.display(), e);
            }
        }
        Err(e) => tracing::warn!("[ANTICHEAT] échec sérialisation heard : {}", e),
    }
}
