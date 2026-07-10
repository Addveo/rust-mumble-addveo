#!/usr/bin/env python3
"""Bots Mumble headless pour tester l'anticheat en local.

- N bots "listeners" : se connectent et restent dans le channel Racine (la foule).
- M bots "cheaters"  : émettent en continu vers tout le channel -> leur voix
  atteint tous les autres -> l'anticheat les flag et leur score explose.

Usage :
    python bots.py [nb_listeners=8] [nb_cheaters=1] [host=127.0.0.1] [port=64738]

Ton propre client Mumble se connecte à côté pour regarder le panel et
tester les boutons Bloquer / Kick sur le(s) CheaterBot.
"""
import math
import ssl
import struct
import sys
import threading
import time

# Python >= 3.12 a supprimé ssl.wrap_socket, que pymumble utilise encore.
# On le recrée via SSLContext (sans vérif de cert : rust-mumble est auto-signé).
if not hasattr(ssl, "wrap_socket"):
    def _wrap_socket(sock, keyfile=None, certfile=None, server_side=False,
                     cert_reqs=ssl.CERT_NONE, ssl_version=None, ca_certs=None,
                     do_handshake_on_connect=True, suppress_ragged_eofs=True,
                     ciphers=None):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        if certfile:
            ctx.load_cert_chain(certfile, keyfile)
        if ciphers:
            ctx.set_ciphers(ciphers)
        return ctx.wrap_socket(
            sock,
            server_side=server_side,
            do_handshake_on_connect=do_handshake_on_connect,
            suppress_ragged_eofs=suppress_ragged_eofs,
        )

    ssl.wrap_socket = _wrap_socket

import pymumble_py3 as pymumble

HOST = sys.argv[3] if len(sys.argv) > 3 else "127.0.0.1"
PORT = int(sys.argv[4]) if len(sys.argv) > 4 else 64738
NB_LISTENERS = int(sys.argv[1]) if len(sys.argv) > 1 else 8
NB_CHEATERS = int(sys.argv[2]) if len(sys.argv) > 2 else 1

RATE = 48000  # Mumble attend du PCM 48kHz mono 16-bit


def sine_pcm(seconds=0.4, freq=220):
    """Génère un bip PCM (48kHz mono 16-bit) à jouer en boucle."""
    n = int(seconds * RATE)
    return b"".join(struct.pack("<h", int(6000 * math.sin(2 * math.pi * freq * i / RATE))) for i in range(n))


def make_bot(name, deaf=False):
    m = pymumble.Mumble(HOST, name, port=PORT, password="", reconnect=False, debug=False)
    # deaf=True : ne reçoit pas l'audio (listener passif, moins de CPU) — mais
    # alors le serveur ne le compte PAS comme destinataire. Pour tester le
    # détecteur "recipients audio", les listeners doivent être NON-deaf.
    m.set_receive_sound(not deaf)
    m.start()
    m.is_ready()  # bloque jusqu'à la connexion
    return m


def run_listener(name, bots, stop):
    try:
        m = make_bot(name)
        bots.append(m)
        print(f"[listener] {name} connecté", flush=True)
        # keepalive : rust-mumble drop tout client sans ping TCP pendant 10s.
        while not stop.is_set():
            m.ping()
            time.sleep(3)
    except Exception as e:
        print(f"[listener] {name} ERREUR: {e}", flush=True)


def run_cheater(name, bots, stop):
    """Simule le cheat map-wide : enregistre une voice-target contenant TOUTES
    les sessions connectées (au lieu d'un appel/radio borné). Détecté par
    observe_target_registration côté serveur, sans dépendre de l'audio."""
    from pymumble_py3 import mumble_pb2

    try:
        m = make_bot(name)
        bots.append(m)
        print(f"[CHEATER]  {name} connecté -> voice-target vers TOUT le monde", flush=True)
        while not stop.is_set():
            m.ping()  # keepalive
            sessions = list(m.users.keys())  # toutes les sessions connues
            if len(sessions) > 1:
                vt = mumble_pb2.VoiceTarget()
                vt.id = 1
                t = vt.targets.add()
                t.session.extend(sessions)
                m.send_message(19, vt)  # 19 = VoiceTarget
            time.sleep(2)
    except Exception as e:
        print(f"[CHEATER]  {name} ERREUR: {e}", flush=True)


def main():
    print(f"==> connexion de {NB_LISTENERS} listeners + {NB_CHEATERS} cheater(s) sur {HOST}:{PORT}", flush=True)
    bots = []
    stop = threading.Event()
    threads = []

    for i in range(1, NB_LISTENERS + 1):
        t = threading.Thread(target=run_listener, args=(f"Bot{i:02d}", bots, stop), daemon=True)
        t.start()
        threads.append(t)
        time.sleep(0.25)  # on étale les connexions pour ne pas spammer

    for i in range(1, NB_CHEATERS + 1):
        t = threading.Thread(target=run_cheater, args=(f"CheaterBot{i}", bots, stop), daemon=True)
        t.start()
        threads.append(t)
        time.sleep(0.25)

    print("==> bots lancés. Ctrl+C pour tout couper.", flush=True)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        print("\n==> arrêt, déconnexion des bots...", flush=True)
        stop.set()
        for m in bots:
            try:
                m.stop()
            except Exception:
                pass


if __name__ == "__main__":
    main()
