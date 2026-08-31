#!/usr/bin/env bash
# Mesure ce que pese une image, et rend chaque chiffre AVEC sa definition.
#
#   ./tests/compat/measure_container.sh --json docs/container.json  # la campagne
#   ./tests/compat/measure_container.sh [tag] [-- args de docker run]
#   ./tests/compat/measure_container.sh --tailles IMAGE...          # les tailles seules
#
# La premiere forme est celle qui compte : elle mesure les DEUX images que le
# README compare — ferrite et l'Elasticsearch de reference — dans la meme
# campagne, sur la meme machine, avec le meme outil, et ecrit
# `docs/container.json`. C'est la seule facon de tenir la ligne « meme
# definition des deux cotes », et le fichier existe parce qu'un chiffre qui ne
# vit que dans une sortie de terminal ne peut etre lu par personne et derive :
# la page produit a annonce 2,4 Mo des mois apres que la mesure disait 4,0.
#
# Les deux autres formes restent la pour mesurer une image quelconque a la main.
#
# Pourquoi trois tailles et pas une : `docker image inspect --format '{{.Size}}'`
# ne mesure pas la meme chose selon le magasin d'images. Avec le magasin
# classique il rendait la somme des couches DECOMPRESSEES ; avec celui de
# containerd — le defaut depuis Docker 29 — il rend la somme des blobs
# COMPRESSES, soit ce qu'un registre sert. Sur la meme image de ferrite :
# 9,7 Mo d'un cote, 4,1 Mo de l'autre — et la colonne DISK USAGE de
# `docker images` en montre encore une troisieme (les deux additionnes, parce
# que containerd garde les blobs a cote de la copie decompressee). Trois
# chiffres honnetes, trois questions differentes.
#
# Cet outil ne lit donc AUCUN champ dont le sens depend de la version : il
# demande l'image a Docker (`docker save`, format OCI) et compte les octets. La
# version du serveur Docker est imprimee quand meme, parce qu'elle change ce que
# les autres outils repondent a la meme question.
#
# `docker save` ne rend les blobs COMPRESSES que depuis le magasin d'images
# containerd. Avec le magasin classique il ecrit bien un layout OCI, mais avec
# des couches nues : la taille qu'un registre servirait n'en est pas deductible,
# et elle est alors refusee plutot que remplacee par un nombre plausible. D'ou
# $IMAGE_TAR, qui laisse passer l'artefact OCI directement :
#
#   docker buildx build --output type=oci,dest=/tmp/ferrite-oci.tar .
#   IMAGE_TAR=/tmp/ferrite-oci.tar ./tests/compat/measure_container.sh ferrite:ci
#
# C'est ce que fait la CI, dont les runners sont encore en Docker 28.
#
# Unite : les Mo affiches sont des megaoctets decimaux (10^6 octets), comme
# ceux de `docker images`. 4 073 564 octets font 4,1 Mo et 3,9 Mio : la
# confusion entre les deux est la moitie de l'ecart entre les chiffres qui ont
# circule ici.
#
# Tout le travail est dans chiffres_conteneur.py — une seule implementation de
# la taille, du RSS et du demarrage, sans quoi la campagne et la mesure a la
# main finiraient par ne plus mesurer la meme chose.
set -euo pipefail

cd "$(dirname "$0")/../.."
OUTIL=(python3 tests/compat/chiffres_conteneur.py)

case "${1:-}" in
  --json)
    shift
    exec "${OUTIL[@]}" --mesure --json "${1:-docs/container.json}"
    ;;
  --tailles)
    shift
    [ $# -ge 1 ] || { echo "usage: $0 --tailles IMAGE..." >&2; exit 2; }
    exec "${OUTIL[@]}" --tailles "$@"
    ;;
esac

# Le tag par defaut se lit dans Cargo.toml : ecrit en dur ici, il designait
# encore `ferrite:0.7.0` le jour ou le depot en etait a la 0.8.0 — donc l'image
# du binaire d'avant, mesuree sous le numero du jour.
TAG="${1:-ferrite:$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
shift || true
[ "${1:-}" = "--" ] && shift   # tout ce qui suit va a `docker run`
exec "${OUTIL[@]}" --image "$TAG" --tours "${TOURS:-5}" --run "$@"
