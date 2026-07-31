#!/usr/bin/env bash
# Harnais de compatibilite de ferrite — la commande unique.
#
#   ./tests/compat/run.sh
#
# Compile ferrite en release, le lance sur un port et un repertoire de donnees
# jetables, installe le client Elasticsearch officiel dans un venv si besoin,
# puis exerce le serveur avec ce client : le critere d'acceptation de la carte
# #1 d'abord, la suite de compatibilite complete ensuite.
#
# Variables :
#   FERRITE_PORT   port d'ecoute (defaut : 9200)
#   FERRITE_URL    cible un serveur deja lance ; rien n'est compile ni demarre
set -euo pipefail

cd "$(dirname "$0")/../.."
PORT="${FERRITE_PORT:-9200}"
URL="${FERRITE_URL:-}"
PID=""
DATA=""

cleanup() {
  [ -n "$PID" ] && kill "$PID" 2>/dev/null || true
  [ -n "$DATA" ] && rm -rf "$DATA" || true
}
trap cleanup EXIT

# --- client officiel --------------------------------------------------------
PYTHON=python3
if ! $PYTHON -c 'import elasticsearch' 2>/dev/null; then
  if [ ! -d .venv-compat ]; then
    echo "== creation du venv .venv-compat (client Elasticsearch officiel)"
    $PYTHON -m venv .venv-compat
    ./.venv-compat/bin/pip install --quiet --upgrade pip
    ./.venv-compat/bin/pip install --quiet "elasticsearch>=8,<9"
  fi
  PYTHON="$PWD/.venv-compat/bin/python"
fi
echo "== client : $($PYTHON -c 'import elasticsearch; print("elasticsearch-py", elasticsearch.__version__)')"

# --- serveur ----------------------------------------------------------------
if [ -z "$URL" ]; then
  echo "== compilation (release)"
  cargo build --release
  DATA="$(mktemp -d)"
  echo "== demarrage de ferrite sur :$PORT (donnees : $DATA)"
  FERRITE_BIND="127.0.0.1:$PORT" FERRITE_DATA="$DATA" ./target/release/ferrite &
  PID=$!
  URL="http://127.0.0.1:$PORT"
  for _ in $(seq 1 100); do
    curl -sf "$URL/" >/dev/null 2>&1 && break
    sleep 0.1
  done
fi
echo "== cible : $URL"
echo

# `acceptance.py` est le script de la carte, recopie sans une virgule de
# changement : il ne prend pas d'argument et vise localhost:9200. On lui
# garantit seulement un index [livres] absent au depart.
if [ "$URL" = "http://127.0.0.1:9200" ] || [ "$URL" = "http://localhost:9200" ]; then
  echo "== critere d'acceptation (carte ferrite #1, script non modifie)"
  curl -s -X DELETE "$URL/livres" >/dev/null || true
  $PYTHON tests/compat/acceptance.py
  echo
else
  echo "== critere d'acceptation : ignore (le script de la carte vise :9200, cible = $URL)"
  echo
fi

echo "== suite de compatibilite"
$PYTHON tests/compat/suite.py "$URL"
