#!/usr/bin/env bash
# Mesure ce que pese une image, et rend chaque chiffre AVEC sa definition.
#
#   ./tests/compat/measure_container.sh [tag] [-- args de docker run]
#   ./tests/compat/measure_container.sh --tailles IMAGE...   # les tailles seules
#
# Le second mode mesure n'importe quelle image, et le `--` du premier permet de
# demarrer n'importe quel serveur : c'est la seule facon de tenir la ligne
# « meme definition des deux cotes ». Elasticsearch se mesure donc ainsi :
#
#   PORT=9299 ./tests/compat/measure_container.sh \
#     docker.elastic.co/elasticsearch/elasticsearch:8.15.0 -- \
#     -e discovery.type=single-node -e xpack.security.enabled=false \
#     -e "ES_JAVA_OPTS=-Xms512m -Xmx512m"
#
# Pourquoi trois tailles et pas une : `docker image inspect --format '{{.Size}}'`
# ne mesure pas la meme chose selon le magasin d'images. Avec le magasin
# classique il rendait la somme des couches DECOMPRESSEES ; avec celui de
# containerd — le defaut depuis Docker 29 — il rend la somme des blobs
# COMPRESSES, soit ce qu'un registre sert. Sur la meme image de ferrite :
# 9,5 Mo d'un cote, 4,0 Mo de l'autre — et la colonne DISK USAGE de
# `docker images` en montre encore une troisieme (les deux additionnes, parce
# que containerd garde les blobs a cote de la copie decompressee). Trois
# chiffres honnetes, trois questions differentes.
#
# Ce script ne lit donc AUCUN champ dont le sens depend de la version : il
# demande l'image a Docker (`docker save`, format OCI) et compte les octets.
# La version du serveur Docker est imprimee quand meme, parce qu'elle change ce
# que les autres outils repondent a la meme question.
#
# Unite : les Mo affiches sont des megaoctets decimaux (10^6 octets), comme
# ceux de `docker images`. 4 007 597 octets font 4,0 Mo et 3,8 Mio : la
# confusion entre les deux est la moitie de l'ecart entre les chiffres qui ont
# circule ici.
set -euo pipefail

cd "$(dirname "$0")/../.."

TMP=""
NAME=""
cleanup() {
  [ -n "$NAME" ] && docker rm -f "$NAME" >/dev/null 2>&1
  [ -n "$TMP" ] && rm -rf "$TMP"
  true
}
trap cleanup EXIT

version_docker() {
  # Imprimee parce qu'elle change ce que les AUTRES outils repondent, pas ce que
  # celui-ci mesure.
  echo "docker serveur   : $(docker version --format '{{.Server.Version}}')" \
       "(magasin d'images : $(docker info --format '{{.Driver}}'))"
}

# Les trois tailles d'une image, comptees sur les octets que Docker en rend.
tailles() {
  local image="$1"
  TMP="$(mktemp -d)"
  docker save "$image" -o "$TMP/image.tar"
  IMAGE_TAR="$TMP/image.tar" python3 "$(dirname "$0")/tailles_image.py" "$image"
  rm -rf "$TMP"; TMP=""
}

if [ "${1:-}" = "--tailles" ]; then
  shift
  [ $# -ge 1 ] || { echo "usage: $0 --tailles IMAGE..." >&2; exit 2; }
  version_docker
  for image in "$@"; do
    echo
    tailles "$image"
  done
  exit 0
fi

TAG="${1:-ferrite:0.7.0}"
shift || true
[ "${1:-}" = "--" ] && shift   # tout ce qui suit va a `docker run`
NAME="ferrite-mesure-$$"
PORT="${PORT:-9299}"

version_docker
echo
tailles "$TAG"

# --- temps de demarrage -----------------------------------------------------
echo
docker rm -f "$NAME" >/dev/null 2>&1 || true
START=$(date +%s%N)
docker run -d --name "$NAME" -p "$PORT:9200" "$@" "$TAG" >/dev/null
until curl -sf "http://127.0.0.1:$PORT/" >/dev/null 2>&1; do
  sleep 0.005
done
END=$(date +%s%N)
echo "demarrage        : $(( (END - START) / 1000000 )) ms (docker run -> premier GET / servi)"

# --- RSS au repos -----------------------------------------------------------
# La somme des VmRSS de TOUS les processus du conteneur, pas seulement du PID 1 :
# ferrite est seul dans le sien, mais l'entrypoint d'Elasticsearch lance un shell
# qui lance la JVM — lire le PID 1 y rendrait le RSS du shell, soit deux ordres
# de grandeur en dessous, et du bon cote pour nous.
CID=$(docker inspect -f '{{.Id}}' "$NAME")
sleep 2
RSS=$({ for p in /proc/[0-9]*; do
          if grep -qs "$CID" "$p/cgroup"; then
            awk '/^VmRSS:/ {print $2}' "$p/status" 2>/dev/null || true
          fi
        done; true; } | awk '{s += $1} END {print s + 0}')
[ "$RSS" -gt 0 ] || { echo "RSS : aucun processus trouve dans le cgroup du conteneur" >&2; exit 1; }
echo "RSS au repos     : ${RSS} Ko ($(awk -v r="$RSS" 'BEGIN{printf "%.1f", r*1024/1000000}') Mo)"

# --- il repond vraiment -----------------------------------------------------
echo
echo "== la poignee de main servie par le conteneur"
curl -s "http://127.0.0.1:$PORT/" | head -c 400
echo
curl -s -I "http://127.0.0.1:$PORT/" | grep -i x-elastic-product
