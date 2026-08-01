#!/usr/bin/env bash
# Mesure les trois chiffres qui sont l'argument de vente de ferrite :
# taille d'image, RSS au repos, temps de demarrage.
#
#   ./tests/compat/measure_container.sh [tag]
#
# Le temps de demarrage mesure est celui qui compte pour un utilisateur : entre
# `docker run` et le premier `GET /` qui repond.
set -euo pipefail

cd "$(dirname "$0")/../.."
TAG="${1:-ferrite:0.1.0}"
NAME="ferrite-mesure-$$"
PORT="${PORT:-9299}"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== image : $TAG"
docker image inspect "$TAG" --format '{{.Size}}' | \
  awk '{printf "taille d'"'"'image   : %d octets (%.1f Mo)\n", $1, $1/1048576}'

# --- temps de demarrage -----------------------------------------------------
cleanup
START=$(date +%s%N)
docker run -d --name "$NAME" -p "$PORT:9200" "$TAG" >/dev/null
until curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1; do
  sleep 0.005
done
END=$(date +%s%N)
echo "demarrage        : $(( (END - START) / 1000000 )) ms (docker run -> premier GET / servi)"

# --- RSS au repos -----------------------------------------------------------
PID=$(docker inspect -f '{{.State.Pid}}' "$NAME")
sleep 2
RSS=$(awk '/^VmRSS:/ {print $2}' "/proc/$PID/status")
echo "RSS au repos     : ${RSS} Ko ($(awk -v r="$RSS" 'BEGIN{printf "%.1f", r/1024}') Mo)"

# --- il repond vraiment -----------------------------------------------------
echo
echo "== la poignee de main servie par le conteneur"
curl -s "http://127.0.0.1:$PORT/" | head -c 400
echo
curl -s -I "http://127.0.0.1:$PORT/" | grep -i x-elastic-product
