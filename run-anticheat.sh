#!/usr/bin/env bash
# Lance le serveur rust-mumble (fork anticheat) en local pour tester.
# Usage depuis WSL :  bash run-anticheat.sh
# Panel : http://localhost:8080/panel  (admin / addveo123)
set -e

BIN="$HOME/rm-target/debug/rust-mumble"

# Rebuild auto si le binaire manque (ex. après un nettoyage)
if [ ! -x "$BIN" ]; then
  echo "==> binaire absent, build en cours..."
  cd /mnt/c/Users/lepet/Documents/Projects/Addveo/rust-mumble
  CARGO_TARGET_DIR="$HOME/rm-target" "$HOME/.cargo/bin/cargo" build
fi

echo "==> serveur voix : localhost:64738  |  panel : http://localhost:8080/panel (admin / addveo123)"
echo "==> Ctrl+C pour arrêter"

RUST_LOG=info "$BIN" \
  --listen 0.0.0.0:64738 \
  --http-listen 0.0.0.0:8080 \
  --http-password addveo123 \
  --anticheat \
  --anticheat-threshold-pct 50 \
  --anticheat-min-recipients 1 \
  --anticheat-max-target-sessions 5 \
  --anticheat-action log
