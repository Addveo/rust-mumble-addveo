#!/usr/bin/env python3
"""Test du ban : connecte un bot, attend, tente une reconnexion.
Le script de contrôle (curl) banni entre les deux et vérifie le refus."""
import ssl
import sys
import time

if not hasattr(ssl, "wrap_socket"):
    def _ws(sock, keyfile=None, certfile=None, server_side=False, cert_reqs=ssl.CERT_NONE,
            ssl_version=None, ca_certs=None, do_handshake_on_connect=True, suppress_ragged_eofs=True, ciphers=None):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return ctx.wrap_socket(sock, server_side=server_side, do_handshake_on_connect=do_handshake_on_connect,
                               suppress_ragged_eofs=suppress_ragged_eofs)
    ssl.wrap_socket = _ws

import pymumble_py3 as pymumble

NAME = sys.argv[1] if len(sys.argv) > 1 else "[99] BanMe"


def try_connect():
    try:
        m = pymumble.Mumble("127.0.0.1", NAME, port=64738, password="", reconnect=False, debug=False)
        m.set_receive_sound(False)
        m.start()
        m.is_ready()
        return m
    except Exception as e:
        print(f"CONNEXION REFUSÉE: {e}", flush=True)
        return None


m = try_connect()
if m:
    print(f"CONNECTÉ: {NAME}", flush=True)
else:
    print(f"PAS CONNECTÉ: {NAME}", flush=True)

# reste en vie pour que le contrôleur puisse agir
while True:
    time.sleep(1)
