# rust-mumble — fork Addveo avec ANTICHEAT vocal

Serveur Mumble pour FiveM (fork de [AvarianKnight/rust-mumble](https://github.com/AvarianKnight/rust-mumble), lui-même basé sur ZUMBLE), avec un **anticheat vocal intégré** : détection du talk map-wide, du chunking, de l'espionnage de channels et des incohérences de position — plus un **panel web d'administration** (config à chaud, ban/mute persistants, webhook Discord, mémoire « qui a parlé à qui »).

---

## 🚀 Démarrage rapide — copiez, collez, Entrée

### Binaire (après `cargo build --release`)

```bash
./rust-mumble \
  --listen 0.0.0.0:64738 \
  --http-listen 0.0.0.0:8080 \
  --http-password CHANGEZ_MOI \
  --restrict-to-version CitizenFX \
  --server-name "MON SERVEUR RP" \
  --anticheat \
  --anticheat-action log \
  --ban-file ./data/bans.json \
  --anticheat-webhook "https://discord.com/api/webhooks/VOTRE/WEBHOOK" \
  --anticheat-panel-url "http://VOTRE_IP:8080/panel"
```

→ Panel : **http://VOTRE_IP:8080/panel** (login `admin` / votre `--http-password`).
→ Côté FiveM : `mumble_server "VOTRE_IP:64738"` dans le server.cfg (pma-voice).

### Docker

```bash
docker build -t rust-mumble-anticheat .

mkdir -p /root/mumble-data/monserveur

docker run -d --name mumble_monserveur --restart unless-stopped \
  --network host \
  -v /root/mumble-data/monserveur:/data \
  rust-mumble-anticheat \
  --listen 0.0.0.0:64738 \
  --http-listen 0.0.0.0:8080 \
  --http-password CHANGEZ_MOI \
  --restrict-to-version CitizenFX \
  --server-name "MON SERVEUR RP" \
  --anticheat \
  --anticheat-action log \
  --ban-file /data/bans.json \
  --anticheat-webhook "https://discord.com/api/webhooks/VOTRE/WEBHOOK" \
  --anticheat-panel-url "http://VOTRE_IP:8080/panel"
```

> `--network host` est important : le NAT Docker (bridge) dégrade la voix UDP dans le temps (conntrack).
> Le volume `/data` persiste les **bans/mutes** et la mémoire **« qui a parlé »** à travers les recréations.

### Les 2 lignes à adapter, le reste peut rester tel quel

| À changer | Pourquoi |
|---|---|
| `--http-password` | mot de passe du panel ET de l'API admin (FiveM `mumble_api` l'utilise aussi) |
| `--server-name` | affiché en haut du panel + dans les embeds Discord (fini les serveurs anonymes) |

---

## ⚙️ Tous les paramètres anticheat (défauts recommandés)

**Tout est modifiable À CHAUD via le panel** — les flags CLI ne fixent que les valeurs de départ. En cas de doute : ne mettez que `--anticheat` et réglez le reste dans le panel.

| Flag | Défaut | Rôle |
|---|---|---|
| `--anticheat` | off | Active l'anticheat + le panel enrichi |
| `--anticheat-action` | `log` | `log` (observer), `mute`, `kick`, `ban` (persistant IP+nom) |
| `--anticheat-threshold-pct` | 60 | « Haute portée » = parler à plus de X% des joueurs connectés |
| `--anticheat-min-recipients` | 15 | Ne jamais flagger sous ce nombre de destinataires (protège les petits serveurs) |
| `--anticheat-mutuality-max-pct` | 30 | Haute portée + mutualité SOUS ce % = cheat. **C'est LE détecteur** : une foule légitime se cible mutuellement (~100%), un cheater non (~0%) |
| `--anticheat-window-secs` | 10 | Fenêtre de comptage des cibles distinctes (attrape le « chunking ») |
| `--anticheat-strikes` | 3 | Cycles suspects consécutifs avant d'agir (anti faux positif) |
| `--anticheat-listen-max` | 25 | Écouter plus de X channels = espionnage map-wide (0 = off) |
| `--ban-file` | — | JSON des bans/mutes persistants (mettre sur un volume) |
| `--anticheat-webhook` | — | Webhook Discord notifié à chaque détection (avec date/heure) |
| `--anticheat-panel-url` | — | URL du panel mise dans l'embed Discord (lien direct) |
| `--server-name` | adresse | Nom lisible du serveur (panel + Discord) |
| `--panel-hide-ips` | off | **Expurge TOUTES les IP côté serveur** (API HTTP + panel) — indispensable si vous donnez le panel à des utilisateurs. Le serveur continue de bannir par IP en interne |

**Conseil de mise en route** : lancez en `--anticheat-action log` une soirée, regardez les logs du panel/Discord. Les vrais cheats map-wide (mutualité ~0% avec 100+ joueurs atteints) se détectent sans faux positif. Quand c'est propre → passez `mute` ou `ban` dans le panel, sans redémarrer.

### Le panel

- **Tableau live** des joueurs (2s) : portée, mutualité, listens, position, score, flags — triable par colonne, recherche (nom/IP/session), paginé par 100.
- **Clic sur un nom** → « qui lui a parlé » (dernière heure, persisté) — idéal pour identifier le harceleur d'un streamer : videz la liste, le prochain qui parle est le suspect.
- **Actions par joueur** : Bloquer (mute persistant), Débloquer (+exempt), Kick, BAN, Reset.
- **Config à chaud** : tous les seuils, l'action, le webhook, sans redémarrage.
- **« Cacher les IP »** : masque toutes les IP pour partager l'écran.

---

## Fonctionnalités héritées (upstream)

 * 100% compatible FiveM — remplacement direct des natives client (pma-voice)
 * API HTTP des natives serveur (MumbleIsPlayerMuted / MumbleSetPlayerMuted / MumbleCreateChannel)
 * Multithread, séparé du réseau du jeu, installable sur une machine dédiée
 * Métriques Prometheus (`/metrics`)

## Build depuis les sources

```bash
# Linux : llvm + make requis
cargo build --release
# binaire : target/release/rust-mumble
```

**Ce logiciel est fourni tel quel, utilisez-le à vos risques.**
