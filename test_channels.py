#!/usr/bin/env python3
"""Test PROD-RÉALISTE : prouve que la détection dépend de la TAILLE de la
voice-target (nombre de joueurs ciblés), PAS du fait que les joueurs partagent
un channel.

En prod FiveM, chaque joueur est seul dans son channel et pma-voice construit
la voice-target avec les CHANNELS des joueurs à portée :
  - proximité normale  -> quelques channels          -> NON flaggé
  - cheat "map-wide"   -> les channels de TOUT le monde -> FLAGGÉ

Ici chaque bot envoie une voice-target contenant N `channel_id` (exactement ce
que fait MumbleAddVoiceTargetChannel). Le détecteur compte ces entrées, que les
channels existent ou non — c'est le NOMBRE de cibles qui trahit le cheat.
"""
import ssl
import sys
import threading
import time

if not hasattr(ssl, "wrap_socket"):
    def _wrap_socket(sock, keyfile=None, certfile=None, server_side=False,
                     cert_reqs=ssl.CERT_NONE, ssl_version=None, ca_certs=None,
                     do_handshake_on_connect=True, suppress_ragged_eofs=True, ciphers=None):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        if certfile:
            ctx.load_cert_chain(certfile, keyfile)
        return ctx.wrap_socket(sock, server_side=server_side,
                               do_handshake_on_connect=do_handshake_on_connect,
                               suppress_ragged_eofs=suppress_ragged_eofs)
    ssl.wrap_socket = _wrap_socket

import pymumble_py3 as pymumble
from pymumble_py3 import mumble_pb2
from pymumble_py3.constants import PYMUMBLE_MSG_TYPES_VOICETARGET

HOST = "127.0.0.1"
PORT = 64738

# combien de channels chaque profil met dans sa voice-target
PROXIMITY_CHANNELS = 3     # joueur normal : quelques voisins
CHEAT_CHANNELS = 40        # cheater : toute la map

keepalive = []


def connect(name):
    m = pymumble.Mumble(HOST, name, port=PORT, password="", reconnect=False, debug=False)
    m.set_receive_sound(False)
    m.start()
    m.is_ready()
    # Sérialise TOUS les envois de contrôle (pings internes de pymumble + les
    # nôtres) derrière un même lock -> plus de race sur le socket SSL.
    lock = threading.Lock()
    orig = m.send_message

    def locked_send(mtype, message):
        with lock:
            return orig(mtype, message)

    m.send_message = locked_send
    return m


def send_channel_target(m, first_channel, count):
    """Envoie une voice-target (whisper id=1) avec `count` channels, comme
    pma-voice le fait pour la proximité (un channel par joueur ciblé)."""
    vt = mumble_pb2.VoiceTarget()
    vt.id = 1
    for cid in range(first_channel, first_channel + count):
        tgt = vt.targets.add()
        tgt.channel_id = cid
    m.send_message(PYMUMBLE_MSG_TYPES_VOICETARGET, vt)


def loop_send(m, count, label):
    print(f"==> {label} : voice-target = {count} channels", flush=True)
    while True:
        try:
            send_channel_target(m, 100, count)
        except Exception as e:
            print(f"!! {label} send err: {e}", flush=True)
            return
        time.sleep(0.4)


def main():
    # quelques joueurs "figurants" pour un compteur de connectés réaliste
    for i in range(1, 5):
        keepalive.append(connect(f"Filler{i:02d}"))
        time.sleep(0.2)

    # joueur NORMAL : proximité = peu de channels -> ne doit PAS être flaggé
    normal = connect("PlayerNormal")
    keepalive.append(normal)
    threading.Thread(target=loop_send, args=(normal, PROXIMITY_CHANNELS, "PlayerNormal (proximité)"), daemon=True).start()

    time.sleep(1)

    # CHEATER : cible toute la map -> doit être FLAGGÉ
    cheater = connect("CheaterMapWide")
    keepalive.append(cheater)
    threading.Thread(target=loop_send, args=(cheater, CHEAT_CHANNELS, "CheaterMapWide (map-wide)"), daemon=True).start()

    print("==> tout lancé. Ctrl+C pour couper.", flush=True)
    while True:
        time.sleep(1)


if __name__ == "__main__":
    main()
