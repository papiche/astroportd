#!/usr/bin/env bash
# Installeur d'astroportd — préversion 0.1.0
#
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --yes --mode origin
#
# Ce script : détecte votre architecture, télécharge l'archive de la release GitHub, VÉRIFIE son empreinte SHA-256
# (il s'arrête si elle ne correspond pas : aucun repli silencieux), installe le binaire et propose de lancer
# `astroportd init`. Il ne demande jamais les droits administrateur.

set -euo pipefail

REPO="papiche/astroportd"
BIN="astroportd"
PREFIX="${HOME}/.local"
VERSION="latest"
STATIC=0
YES=0
RUN_INIT=1
MODE=""
EMAIL=""

usage() {
  cat <<'USAGE'
Installeur d'astroportd (préversion 0.1.0)

Usage : install.sh [options]

  --version vX.Y.Z   version à installer (défaut : la dernière release)
  --prefix DIR       dossier d'installation ; le binaire va dans DIR/bin (défaut : ~/.local)
  --static           binaire statique musl (sans dépendance à la glibc) — x86_64 uniquement
  --no-init          n'appelle pas `astroportd init` à la fin
  --yes, -y          mode non interactif : accepte les réponses par défaut
  --mode origin|zen  constellation à rejoindre (transmis à `astroportd init`)
  --email EMAIL      email du Capitaine (transmis à `astroportd init`)
  --help, -h         cette aide

Architectures : x86_64 (PC, serveur), aarch64 (Raspberry Pi 4/5, Pi Zero 2 W en 64 bits),
armv7l (Raspberry Pi 2/3, Pi OS 32 bits). Linux uniquement.
USAGE
}

say()  { printf '%s\n' "$*"; }
warn() { printf '⚠  %s\n' "$*" >&2; }
die()  { printf '✘ %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) shift; VERSION="${1:?--version attend une valeur (ex. v0.1.0)}" ;;
    --prefix)  shift; PREFIX="${1:?--prefix attend un dossier}" ;;
    --static)  STATIC=1 ;;
    --no-init) RUN_INIT=0 ;;
    --yes|-y)  YES=1 ;;
    --mode)    shift; MODE="${1:?--mode attend origin ou zen}" ;;
    --email)   shift; EMAIL="${1:?--email attend une adresse}" ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "option inconnue : $1" ;;
  esac
  shift
done

case "$MODE" in ""|origin|zen) ;; *) die "--mode doit valoir origin ou zen (reçu : $MODE)" ;; esac

# --- Système et architecture ------------------------------------------------------------------------------------
[ "$(uname -s)" = "Linux" ] || die "astroportd ne tourne que sous Linux (système détecté : $(uname -s))."

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || die "outil manquant : $tool (sudo apt install curl tar coreutils)"
done

machine="$(uname -m)"
case "$machine" in
  x86_64|amd64)  TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
  armv7l|armv6l) TARGET="armv7-unknown-linux-gnueabihf" ;;
  *) die "architecture non supportée : $machine (supportées : x86_64, aarch64, armv7l)" ;;
esac

if [ "$STATIC" = 1 ]; then
  [ "$TARGET" = "x86_64-unknown-linux-gnu" ] || die "--static n'existe que pour x86_64 (architecture détectée : $machine)."
  TARGET="x86_64-unknown-linux-musl"
elif [ "$TARGET" = "x86_64-unknown-linux-gnu" ] && command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
  say "Système musl (Alpine ?) détecté : utilisation du binaire statique."
  TARGET="x86_64-unknown-linux-musl"
fi

# --- Horloge : un Raspberry Pi sans pile RTC démarre en 1970 -----------------------------------------------------
now="$(date +%s)"
if [ "$now" -lt 1767225600 ]; then
  warn "L'horloge de cette machine est antérieure à 2026 ($(date -u '+%Y-%m-%d')). astroportd refuse de calculer ses"
  warn "échéances avec une heure fausse. Réglez l'heure (sudo timedatectl set-ntp true) avant de le démarrer."
fi

# --- Téléchargement ---------------------------------------------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  # Une préversion n'est jamais « latest » pour GitHub : on interroge alors la liste des releases.
  api="https://api.github.com/repos/${REPO}/releases?per_page=1"
  tag="$(curl -fsSL "$api" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1 || true)"
  [ -n "$tag" ] || die "impossible de lire la dernière release sur $api (réseau ? aucune release publiée ?)."
  VERSION="$tag"
fi
case "$VERSION" in v*) ;; *) VERSION="v${VERSION}" ;; esac
BASE="https://github.com/${REPO}/releases/download/${VERSION}"
NUM="${VERSION#v}"
NAME="${BIN}-${NUM}-${TARGET}"
ARCHIVE="${NAME}.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "▶ astroportd ${VERSION} pour ${TARGET}"
say "  Téléchargement de ${ARCHIVE}…"
curl -fSL --proto '=https' --tlsv1.2 -o "${tmp}/${ARCHIVE}" "${BASE}/${ARCHIVE}" \
  || die "téléchargement impossible : ${BASE}/${ARCHIVE} (cette cible existe-t-elle dans la release ?)"
curl -fSL --proto '=https' --tlsv1.2 -o "${tmp}/SHA256SUMS" "${BASE}/SHA256SUMS" \
  || die "téléchargement impossible : ${BASE}/SHA256SUMS"

# --- Vérification de l'empreinte --------------------------------------------------------------------------------
expected="$(awk -v f="$ARCHIVE" '{ n=$2; sub(/^\*/, "", n); if (n == f) { print $1; exit } }' "${tmp}/SHA256SUMS")"
[ -n "$expected" ] || die "${ARCHIVE} ne figure pas dans SHA256SUMS : installation refusée."
actual="$(sha256sum "${tmp}/${ARCHIVE}" | awk '{print $1}')"
if [ "$expected" != "$actual" ]; then
  die "empreinte SHA-256 incorrecte pour ${ARCHIVE}
    attendue : ${expected}
    calculée : ${actual}
  Le fichier est corrompu ou a été modifié : installation refusée."
fi
say "  ✔ empreinte SHA-256 vérifiée"

# --- Installation -----------------------------------------------------------------------------------------------
tar -C "$tmp" -xzf "${tmp}/${ARCHIVE}" || die "archive illisible : ${ARCHIVE}"
[ -f "${tmp}/${NAME}/${BIN}" ] || die "l'archive ne contient pas ${NAME}/${BIN}"

mkdir -p "${PREFIX}/bin"
install -m 0755 "${tmp}/${NAME}/${BIN}" "${PREFIX}/bin/${BIN}"
say "  ✔ installé : ${PREFIX}/bin/${BIN}"
"${PREFIX}/bin/${BIN}" --version 2>/dev/null || warn "le binaire installé ne répond pas à --version (architecture incompatible ?)"

case ":${PATH}:" in
  *":${PREFIX}/bin:"*) ;;
  *) warn "${PREFIX}/bin n'est pas dans votre PATH. Ajoutez à ~/.bashrc :  export PATH=\"${PREFIX}/bin:\$PATH\"" ;;
esac

# --- Suite : astroportd init ------------------------------------------------------------------------------------
if [ "$RUN_INIT" = 0 ]; then
  say ""
  say "Terminé. Étape suivante : ${PREFIX}/bin/${BIN} init"
  exit 0
fi

init_args=()
[ -n "$MODE" ] && init_args+=(--mode "$MODE")
[ -n "$EMAIL" ] && init_args+=(--email "$EMAIL")

if [ "$YES" = 1 ]; then
  init_args+=(--yes)
  say ""
  say "▶ ${BIN} init ${init_args[*]}"
  "${PREFIX}/bin/${BIN}" init "${init_args[@]}"
  exit 0
fi

# Lancé par « curl | bash », l'entrée standard est le script : on interroge le terminal s'il existe.
if [ -r /dev/tty ]; then
  printf '\nLancer maintenant l'\''assistant « %s init » (clé Y-Level, choix ORIGIN/ẐEN) ? [O/n] ' "$BIN" > /dev/tty
  read -r answer < /dev/tty || answer="n"
  case "${answer:-o}" in
    n|N|non|Non) say "D'accord. Quand vous voulez : ${PREFIX}/bin/${BIN} init" ;;
    *) "${PREFIX}/bin/${BIN}" init ${init_args[@]+"${init_args[@]}"} < /dev/tty ;;
  esac
else
  say ""
  say "Pas de terminal interactif. Étape suivante : ${PREFIX}/bin/${BIN} init"
fi
