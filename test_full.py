#!/usr/bin/env python3
"""Test des détecteurs avancés :

- CROWD : 20 joueurs qui se ciblent MUTUELLEMENT (grosse foule légitime,
  event à 200 dans une zone) -> haute portée MAIS mutualité ~100% -> NON flaggé.
- Cheater : cible toute la foule, mais personne ne le cible en retour
  -> haute portée + mutualité ~0% -> FLAGGÉ.
- Chunker : cible 5 joueurs à la fois en tournant sur les 20
  -> portée instantanée faible mais fenêtre large + mutualité ~0% -> FLAGGÉ.
"""
import ssl
import threading
import time

if not hasattr(ssl, "wrap_socket"):
    def _ws(sock, keyfile=None, certfile=None, server_side=False, cert_reqs=ssl.CERT_NONE,
            ssl_version=None, ca_certs=None, do_handshake_on_connect=True, suppress_ragged_eofs=True, ciphers=None):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        if certfile:
            ctx.load_cert_chain(certfile, keyfile)
        return ctx.wrap_socket(sock, server_side=server_side, do_handshake_on_connect=do_handshake_on_connect,
                               suppress_ragged_eofs=suppress_ragged_eofs)
    ssl.wrap_socket = _ws

import pymumble_py3 as pymumble
from pymumble_py3 import mumble_pb2
from pymumble_py3.constants import PYMUMBLE_MSG_TYPES_VOICETARGET

HOST, PORT = "127.0.0.1", 64738
CROWD = 20
keep = []


def connect(name):
    m = pymumble.Mumble(HOST, name, port=PORT, password="", reconnect=False, debug=False)
    m.set_receive_sound(False)
    m.start()
    m.is_ready()
    # sérialise tous les envois de contrôle (pings pymumble + les nôtres)
    lock = threading.Lock()
    orig = m.send_message

    def locked(t, msg):
        with lock:
            return orig(t, msg)

    m.send_message = locked
    return m


def sessions_by_name(m):
    out = {}
    for sess, u in list(m.users.items()):
        try:
            out[u["name"]] = sess
        except Exception:
            pass
    return out


def send_target(m, sessions):
    vt = mumble_pb2.VoiceTarget()
    vt.id = 1
    t = vt.targets.add()
    for s in sessions:
        t.session.append(s)
    m.send_message(PYMUMBLE_MSG_TYPES_VOICETARGET, vt)


def crowd_loop(m, myname):
    while True:
        s2n = sessions_by_name(m)
        tgts = [s for n, s in s2n.items() if n.startswith("Crowd") and n != myname]
        if tgts:
            send_target(m, tgts)
        time.sleep(1)


def cheater_loop(m):
    while True:
        s2n = sessions_by_name(m)
        tgts = [s for n, s in s2n.items() if n.startswith("Crowd")]
        if tgts:
            send_target(m, tgts)
        time.sleep(1)


def chunker_loop(m):
    i = 0
    while True:
        s2n = sessions_by_name(m)
        pool = [s for n, s in s2n.items() if n.startswith("Crowd")]
        if pool:
            start = (i * 5) % len(pool)
            chunk = [pool[(start + j) % len(pool)] for j in range(min(5, len(pool)))]
            send_target(m, chunk)
            i += 1
        time.sleep(0.5)


def main():
    print(f"==> connexion de {CROWD} bots foule + cheater + chunker", flush=True)
    crowd = []
    for i in range(1, CROWD + 1):
        name = f"Crowd{i:02d}"
        m = connect(name)
        keep.append(m)
        crowd.append((name, m))
        time.sleep(0.15)
    cheater = connect("Cheater")
    keep.append(cheater)
    chunker = connect("Chunker")
    keep.append(chunker)

    print("==> attente stabilisation des listes users (4s)", flush=True)
    time.sleep(4)

    for name, m in crowd:
        threading.Thread(target=crowd_loop, args=(m, name), daemon=True).start()
    threading.Thread(target=cheater_loop, args=(cheater,), daemon=True).start()
    threading.Thread(target=chunker_loop, args=(chunker,), daemon=True).start()
    print("==> boucles lancées (foule mutuelle / cheater / chunker). Ctrl+C pour couper.", flush=True)

    while True:
        time.sleep(1)


if __name__ == "__main__":
    main()
