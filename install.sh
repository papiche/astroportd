#!/usr/bin/env bash
# Installeur d'astroportd — préversion 0.2.0-beta
#
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --yes --mode origin
#
# Ce script : détecte votre machine, télécharge l'archive de la release GitHub, VÉRIFIE son empreinte SHA-256
# (il s'arrête si elle ne correspond pas : aucun repli silencieux), installe le binaire, prépare la reconnaissance
# de visages locale (libonnxruntime + modèles, chacun vérifié par SHA-256), signale les ports déjà pris, puis propose
# de lancer `astroportd init`. Il ne demande jamais les droits administrateur.

set -euo pipefail

REPO="papiche/astroportd"
BIN="astroportd"
PREFIX="${HOME}/.local"
VERSION="latest"
STATIC=0
YES=0
RUN_INIT=1
FACE=1
MODE=""
EMAIL=""
DOMAIN=""

usage() {
  cat <<'USAGE'
Installeur d'astroportd (préversion 0.2.0-beta)

Usage : install.sh [options]

  --version vX.Y.Z   version à installer (défaut : la dernière release)
  --prefix DIR       dossier d'installation ; le binaire va dans DIR/bin (défaut : ~/.local)
  --static           binaire statique musl (sans dépendance à la glibc) — x86_64 et aarch64 ; choisi tout seul sous Android (Termux)
  --no-face          n'installe ni libonnxruntime ni les modèles de visages (le reste est inchangé)
  --no-init          n'appelle pas `astroportd init` à la fin
  --yes, -y          mode non interactif : accepte les réponses par défaut
  --mode origin|zen  constellation à rejoindre (transmis à `astroportd init`)
  --email EMAIL      email du Capitaine (transmis à `astroportd init`)
  --domain NOM       domaine public : HTTPS Let's Encrypt géré par astroportd si les ports 80/443 sont libres
                     (s'ils sont pris par Nginx Proxy Manager, astroportd reste local sur 127.0.0.1:8090)
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
    --version) shift; VERSION="${1:?--version attend une valeur (ex. v0.2.0-beta)}" ;;
    --prefix)  shift; PREFIX="${1:?--prefix attend un dossier}" ;;
    --static)  STATIC=1 ;;
    --no-init) RUN_INIT=0 ;;
    --no-face) FACE=0 ;;
    --yes|-y)  YES=1 ;;
    --mode)    shift; MODE="${1:?--mode attend origin ou zen}" ;;
    --domain)  shift; DOMAIN="${1:?--domain attend un nom de domaine}" ;;
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

# Android (Termux) n'a pas la glibc : seul le binaire statique y tourne.
if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
  [ "$TARGET" = "aarch64-unknown-linux-gnu" ] || die "Android : seul l'ARM 64 bits (aarch64) est supporté (architecture détectée : $machine)."
  say "Android (Termux) détecté : utilisation du binaire statique ARM 64 bits."
  STATIC=1
fi

if [ "$STATIC" = 1 ]; then
  case "$TARGET" in
    x86_64-unknown-linux-gnu) TARGET="x86_64-unknown-linux-musl" ;;
    aarch64-unknown-linux-gnu) TARGET="aarch64-unknown-linux-musl" ;;
    *) die "--static n'existe que pour x86_64 et aarch64 (architecture détectée : $machine)." ;;
  esac
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

# --- Reconnaissance de visages locale (SCRFD + ArcFace) ---------------------------------------------------------
# Binaire glibc = compilé avec `onnx`. Il lui faut libonnxruntime (extraite d'une roue pip) et deux modèles, que l'on
# cherche d'abord sur IPFS (UPlanet) puis sur le web. Rien n'est jamais accepté sans SHA-256 correct.
DATA="${HOME}/.zen/astroportd"
MODELS="${DATA}/models"
# Les deux modèles du paquet InsightFace buffalo_l : nom | SHA-256 | CID IPFS (v1)
MODEL_FILES=(
  "det_10g.onnx|5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91|bafybeid76moctrw6jgyiv7uvcllidn6ihig22fo4mgu65iv7nlgzzf7zzm"
  "w600k_r50.onnx|4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43|bafybeiejw6zzuiwk36afumfy4hwvdpdnt5f6xrb5pkvkobewrf2hwtmvja"
)
IPFS_GATEWAY="${ASTRO_IPFS_GATEWAY:-https://ipfs.copylaradio.com}"
BUFFALO_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip"

have_ort() {
  [ -n "${ORT_DYLIB_PATH:-}" ] && [ -e "$ORT_DYLIB_PATH" ] && return 0
  compgen -G "${DATA}/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "${HOME}/.astro/lib/python3*/site-packages/onnxruntime/capi/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/local/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/lib/*-linux-gnu/libonnxruntime.so*" >/dev/null
}

install_ort() {
  have_ort && { say "  ✔ libonnxruntime déjà présente"; return 0; }
  if ! { command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; }; then
    warn "libonnxruntime : python3/pip absents (sudo apt install python3-pip), ou définissez ORT_DYLIB_PATH"; return 1
  fi
  say "  Installation de libonnxruntime (roue pip, extraite dans ${DATA}/lib)…"
  local whl; whl="$(mktemp -d)"
  if python3 -m pip download onnxruntime --no-deps -d "$whl" -q 2>/dev/null && python3 - "$whl" "${DATA}/lib" <<'PY'
import glob, os, sys, zipfile
whl = glob.glob(os.path.join(sys.argv[1], "onnxruntime-*.whl"))[0]
os.makedirs(sys.argv[2], exist_ok=True)
with zipfile.ZipFile(whl) as z:
    for n in z.namelist():
        if n.startswith("onnxruntime/capi/libonnxruntime") and ".so" in n:
            open(os.path.join(sys.argv[2], os.path.basename(n)), "wb").write(z.read(n))
PY
  then say "  ✔ libonnxruntime installée"; rm -rf "$whl"
  else warn "libonnxruntime non installée (pip a échoué)"; rm -rf "$whl"; return 1
  fi
}

# Dépose un modèle dans $MODELS s'il a le bon SHA-256 ; essaie le nœud IPFS local, puis la passerelle.
fetch_model() { # nom sha cid
  local name="$1" sha="$2" cid="$3" dest="${MODELS}/$1" part="${MODELS}/$1.part"
  if [ -f "$dest" ] && [ "$(sha256sum "$dest" | cut -d' ' -f1)" = "$sha" ]; then say "  ✔ $name déjà présent"; return 0; fi
  mkdir -p "$MODELS"; rm -f "$part"
  if command -v ipfs >/dev/null 2>&1 && timeout 5 ipfs id >/dev/null 2>&1 \
     && timeout 120 ipfs cat "$cid" > "$part" 2>/dev/null && [ -s "$part" ]; then
    say "  $name : reçu par IPFS (nœud local)"
  elif curl -fsSL --proto '=https' --tlsv1.2 -m 600 -o "$part" "${IPFS_GATEWAY}/ipfs/${cid}" 2>/dev/null && [ -s "$part" ]; then
    say "  $name : reçu par la passerelle ${IPFS_GATEWAY}"
  else
    rm -f "$part"; return 1
  fi
  if [ "$(sha256sum "$part" | cut -d' ' -f1)" = "$sha" ]; then mv "$part" "$dest"; say "  ✔ $name vérifié (SHA-256)"
  else rm -f "$part"; warn "$name : SHA-256 incorrect, fichier refusé"; return 1; fi
}

# Dernier recours : le paquet buffalo_l du web classique (275 Mo), dont on ne garde que les deux modèles.
fetch_models_from_web() {
  say "  Repli : téléchargement de buffalo_l depuis GitHub (275 Mo)…"
  local z; z="$(mktemp -d)"
  curl -fSL --proto '=https' --tlsv1.2 -o "${z}/b.zip" "$BUFFALO_URL" || { rm -rf "$z"; return 1; }
  python3 -c 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); [open(sys.argv[2]+"/"+n,"wb").write(z.read(n)) for n in sys.argv[3:]]' \
    "${z}/b.zip" "$MODELS" det_10g.onnx w600k_r50.onnx || { rm -rf "$z"; return 1; }
  rm -rf "$z"
  local m name sha
  for m in "${MODEL_FILES[@]}"; do
    name="${m%%|*}"; sha="$(echo "$m" | cut -d'|' -f2)"
    [ "$(sha256sum "${MODELS}/${name}" | cut -d' ' -f1)" = "$sha" ] || { rm -f "${MODELS}/${name}"; warn "$name : SHA-256 incorrect"; return 1; }
  done
}

install_models() {
  # Déjà là ailleurs ? astroportd les trouve tout seul.
  if [ -f "${HOME}/.insightface/models/buffalo_l/det_10g.onnx" ] && [ -f "${HOME}/.insightface/models/buffalo_l/w600k_r50.onnx" ]; then
    say "  ✔ modèles déjà présents (~/.insightface/models/buffalo_l)"; return 0
  fi
  warn "Modèles InsightFace buffalo_l : poids publiés pour la RECHERCHE NON COMMERCIALE, vérifiez la licence de votre usage."
  local m missing=0
  for m in "${MODEL_FILES[@]}"; do
    IFS='|' read -r n sha cid <<< "$m"
    fetch_model "$n" "$sha" "$cid" || missing=1
  done
  [ "$missing" = 0 ] || fetch_models_from_web || { warn "modèles non installés : placez det_10g.onnx et w600k_r50.onnx dans ${MODELS}"; return 1; }
}

case "$TARGET" in
  *-musl) FACE=0; say "  (binaire statique : pas de reconnaissance de visages locale — utilisez --face-engine http://…)" ;;
esac
if [ "$FACE" = 1 ]; then
  say ""; say "▶ Reconnaissance de visages locale"
  command -v nvidia-smi >/dev/null 2>&1 && say "  GPU NVIDIA détecté ($(nvidia-smi --query-gpu=name --format=csv,noheader | head -n 1)) : le CPU suffit pour ce traitement, le GPU n'est pas utilisé."
  { install_ort && install_models; } || warn "reconnaissance de visages locale incomplète : voir « astroportd face-check » (ou serve --face-engine http://…)"
fi

# --- Ports déjà occupés (cause fréquente de « Address already in use ») ------------------------------------------
if command -v ss >/dev/null 2>&1; then
  busy=""; for p in 8080 80 443; do ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${p}$" && busy="${busy} ${p}"; done
  if [ -n "$busy" ]; then
    warn "Ports déjà utilisés sur cette machine :${busy} (IPFS, Nginx Proxy Manager…)."
    warn "  « astroportd init » en tient compte : si 80/443 sont pris, astroportd écoute en local (127.0.0.1:8090) derrière votre proxy ; sinon il gère lui-même HTTPS."
  fi
fi

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
[ -n "$DOMAIN" ] && init_args+=(--domain "$DOMAIN")

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
