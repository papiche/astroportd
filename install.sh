#!/usr/bin/env bash
# Installeur d'astroportd — préversion 0.2.0-beta
#
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --yes --mode origin
#
# Ce script : détecte votre machine, télécharge l'archive de la release GitHub, VÉRIFIE son empreinte SHA-256
# (il s'arrête si elle ne correspond pas : aucun repli silencieux), installe le binaire, prépare la reconnaissance
# de visages et d'embeddings locaux (libonnxruntime + modèles, chacun vérifié par SHA-256), signale les ports déjà pris,
# puis propose de lancer `astroportd init`. Relancé sur une station déjà installée, il montre ce qui est en place et
# fait une MISE À JOUR (binaire, modèles manquants, redémarrage du service) sans reproposer `init`.
# Il ne demande jamais les droits administrateur.
#
# Avant d'installer les modèles, il examine la machine (architecture, cœurs, mémoire, place et vitesse du disque, GPU,
# Ollama) et recommande un profil — léger, standard ou confort — que vous pouvez accepter ou changer.
# `install.sh --check-hardware` affiche ce diagnostic sans rien installer.

set -euo pipefail

REPO="papiche/astroportd"
BIN="astroportd"
PREFIX="${HOME}/.local"
VERSION="latest"
STATIC=0
YES=0
RUN_INIT=1
NO_FACE=0
EMBED=auto       # auto : selon la machine ; yes : impose ; no : refuse
PROFILE=auto     # auto | leger | standard | confort
CHECK_ONLY=0
FORCE=0
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
  --profile P        auto (défaut), leger, standard ou confort : ce qui tourne sur cette machine (voir ci-dessous)
  --no-face          n'installe pas les modèles de visages
  --embed / --no-embed   impose ou refuse les embeddings locaux de BRO (bge-m3, ≈ 560 Mo), quelle que soit la machine
  --check-hardware   affiche le diagnostic de la machine et le profil recommandé, sans rien installer
  --no-init          n'appelle pas `astroportd init` à la fin
  --force            réinstalle même si la version en place est déjà la dernière
  --yes, -y          mode non interactif : accepte les réponses par défaut
  --mode origin|zen  constellation à rejoindre (transmis à `astroportd init`)
  --email EMAIL      email du Capitaine (transmis à `astroportd init`)
  --domain NOM       domaine public : HTTPS Let's Encrypt géré par astroportd si les ports 80/443 sont libres
                     (s'ils sont pris par Nginx Proxy Manager, astroportd reste local sur 127.0.0.1:8090)
  --help, -h         cette aide

Profils (recommandés d'après votre machine, que l'assistant détecte et vous montre) :
  leger      rien en local : visages et mémoire passent par une autre machine (Brain GPU, Ollama) — Pi Zero, vieux Pi, NAS
  standard   visages locaux (≈ 190 Mo) et, si la machine a assez de mémoire et de disque, embeddings locaux ; fils limités
  confort    visages et embeddings locaux, tous les cœurs — PC ou serveur avec 8 Go de mémoire et 6 cœurs ou plus

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
    --force)   FORCE=1 ;;
    --no-face) NO_FACE=1 ;;
    --embed)    EMBED=yes ;;
    --no-embed) EMBED=no ;;
    --profile)  shift; PROFILE="${1:?--profile attend auto, leger, standard ou confort}" ;;
    --check-hardware) CHECK_ONLY=1 ;;
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
case "$PROFILE" in
  light|léger) PROFILE=leger ;;
  comfort) PROFILE=confort ;;
esac
case "$PROFILE" in auto|leger|standard|confort) ;; *) die "--profile doit valoir auto, leger, standard ou confort (reçu : $PROFILE)" ;; esac

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

# --- glibc trop ancienne : les binaires gnu demandent la 2.35 (Debian 12, Ubuntu 22.04) ; en dessous, binaire statique -------
case "$TARGET" in
  *-linux-gnu*)
    glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')"
    if [ -n "$glibc" ] && [ "$(printf '%s\n2.35\n' "$glibc" | sort -V | head -n 1)" != "2.35" ]; then
      case "$TARGET" in
        x86_64-*) say "glibc ${glibc} (< 2.35) : utilisation du binaire statique (sans visages ni embeddings locaux)."; TARGET="x86_64-unknown-linux-musl" ;;
        aarch64-*) say "glibc ${glibc} (< 2.35) : utilisation du binaire statique (sans visages ni embeddings locaux)."; TARGET="aarch64-unknown-linux-musl" ;;
        *) die "glibc ${glibc} trop ancienne pour ce binaire (2.35 requise) : mettez le système à jour." ;;
      esac
    fi ;;
esac

# --- Horloge : un Raspberry Pi sans pile RTC démarre en 1970 -----------------------------------------------------
now="$(date +%s)"
if [ "$now" -lt 1767225600 ]; then
  warn "L'horloge de cette machine est antérieure à 2026 ($(date -u '+%Y-%m-%d')). astroportd refuse de calculer ses"
  warn "échéances avec une heure fausse. Réglez l'heure (sudo timedatectl set-ntp true) avant de le démarrer."
fi

# --- Votre machine : détection et recommandation -----------------------------------------------------------------
# Le calcul local coûte de la mémoire (≈ 0,5 Go pour les visages, ≈ 1,3 Go au pic pour bge-m3), du disque (≈ 190 Mo et
# ≈ 560 Mo) et du processeur. L'assistant mesure ce que la machine offre et recommande ce qui tient confortablement.
DATA="${HOME}/.zen/astroportd"
MODELS="${DATA}/models"

# --- Installation déjà en place ? -------------------------------------------------------------------------------
# Binaire présent ET station initialisée (station.conf) : on met à jour, on ne refait jamais « init » (il redériverait
# l'identité de la station). Le profil est déduit des modèles déjà là : ce qui avait été refusé n'est pas installé.
UNIT="${HOME}/.config/systemd/user/${BIN}.service"
INSTALLED=""
if [ -x "${PREFIX}/bin/${BIN}" ]; then
  INSTALLED="$("${PREFIX}/bin/${BIN}" --version 2>/dev/null | awk '{print $2}' || true)"
fi
conf_get() { sed -n "s/^${1}[[:space:]]*=[[:space:]]*//p" "${DATA}/station.conf" 2>/dev/null | head -n 1; }
service_state() {
  if command -v systemctl >/dev/null 2>&1 && [ -f "$UNIT" ]; then systemctl --user is-active "$BIN" 2>/dev/null || true
  else printf 'absent'; fi
}
UPDATE=0
if [ -n "$INSTALLED" ] && [ -f "${DATA}/station.conf" ]; then
  UPDATE=1
  RUN_INIT=0
  mode_txt="ORIGIN (ouverte)"; [ "$(conf_get mode)" = zen ] && mode_txt="ẐEN (privée)"
  dom="$(conf_get domain)"; cap="$(conf_get captain_email)"
  say ""
  say "▶ Installation en place"
  say "  Binaire      : ${PREFIX}/bin/${BIN} (${INSTALLED})"
  say "  Station      : ${DATA} · constellation ${mode_txt}"
  say "  Adresse      : $([ -n "$dom" ] && printf 'https://%s/' "$dom" || printf 'HTTP local (sans domaine)')"
  [ -n "$cap" ] || cap="pas encore créé (page /welcome, code d'installation dans le journal)"
  say "  Capitaine    : ${cap}"
  case "$(service_state)" in
    active) say "  Service      : ${BIN} (systemd utilisateur) en marche" ;;
    absent) if pgrep -x "$BIN" >/dev/null 2>&1; then say "  Service      : en marche hors systemd (PID $(pgrep -x "$BIN" | head -n 1))"; else say "  Service      : aucun (lancé à la main)"; fi ;;
    *) say "  Service      : ${BIN} (systemd utilisateur) arrêté" ;;
  esac
  say "  Mise à jour  : la station, ses comptes et ses données sont conservés ; pas de nouvel « init »."
elif [ -n "$INSTALLED" ]; then
  say ""; say "▶ Binaire déjà installé (${INSTALLED}) mais station pas encore initialisée : installation, puis « init »."
fi

meminfo_mb() { awk -v k="$1:" '$1 == k { printf "%d", $2 / 1024 }' /proc/meminfo 2>/dev/null || true; }

nearest_dir() { local d="$1"; while [ ! -d "$d" ] && [ "$d" != "/" ]; do d="$(dirname "$d")"; done; printf '%s' "$d"; }

# nvme | ssd | hdd | sd (carte SD/eMMC) | inconnu
disk_kind() {
  local src base rota
  src="$(df -P "$1" 2>/dev/null | awk 'NR==2 {print $1}')"
  case "$src" in /dev/*) ;; *) printf 'inconnu'; return 0 ;; esac
  base="$(lsblk -no pkname "$src" 2>/dev/null | head -n 1 || true)"
  [ -n "$base" ] || base="${src#/dev/}"
  case "$base" in mmcblk*) printf 'sd'; return 0 ;; nvme*) printf 'nvme'; return 0 ;; esac
  rota="$(cat "/sys/block/${base}/queue/rotational" 2>/dev/null || true)"
  case "$rota" in 1) printf 'hdd' ;; 0) printf 'ssd' ;; *) printf 'inconnu' ;; esac
}

# Mo/s en écriture synchronisée de 256 Mo dans le dossier donné ; vide si la mesure est impossible.
disk_write_mbps() {
  local f out num unit
  f="$(mktemp -p "$1" .astroportd-disktest.XXXXXX 2>/dev/null)" || return 0
  out="$(LC_ALL=C dd if=/dev/zero of="$f" bs=1M count=256 conv=fdatasync 2>&1 | tail -n 1 || true)"
  rm -f "$f"
  num="$(printf '%s' "$out" | awk '{print $(NF-1)}')"; unit="$(printf '%s' "$out" | awk '{print $NF}')"
  case "$unit" in
    GB/s) awk -v n="$num" 'BEGIN { printf "%d", n * 1000 }' ;;
    MB/s) awk -v n="$num" 'BEGIN { printf "%d", n }' ;;
    kB/s) awk -v n="$num" 'BEGIN { printf "%d", n / 1000 }' ;;
  esac
}

detect_hardware() {
  HW_CORES="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)"
  HW_RAM_MB="$(meminfo_mb MemTotal)"; HW_RAM_MB="${HW_RAM_MB:-0}"
  HW_SWAP_MB="$(meminfo_mb SwapTotal)"; HW_SWAP_MB="${HW_SWAP_MB:-0}"
  HW_CPU="$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo 2>/dev/null | head -n 1 || true)"
  [ -n "$HW_CPU" ] || HW_CPU="$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || true)"
  [ -n "$HW_CPU" ] || HW_CPU="$machine"
  # int8 rapide : AVX2 (x86) ou produit scalaire NEON « dotprod » (ARM 64 bits). Sans, bge-m3 reste utilisable mais plusieurs fois plus lent.
  HW_INT8_FAST=0
  case "$machine" in
    x86_64|amd64) grep -qw avx2 /proc/cpuinfo 2>/dev/null && HW_INT8_FAST=1 ;;
    aarch64|arm64) grep -qw asimddp /proc/cpuinfo 2>/dev/null && HW_INT8_FAST=1 ;;
  esac
  local dir; dir="$(nearest_dir "$DATA")"
  HW_DISK_DIR="$dir"
  HW_DISK_FREE_MB="$(df -Pm "$dir" 2>/dev/null | awk 'NR==2 {print $4}')"; HW_DISK_FREE_MB="${HW_DISK_FREE_MB:-0}"
  HW_DISK_KIND="$(disk_kind "$dir")"
  HW_DISK_MBPS=""
  if [ "${ASTRO_SKIP_DISKTEST:-0}" != 1 ] && [ "$HW_DISK_FREE_MB" -ge 600 ]; then HW_DISK_MBPS="$(disk_write_mbps "$dir")"; fi
  HW_GPU=""
  command -v nvidia-smi >/dev/null 2>&1 && HW_GPU="$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -n 1 || true)"
  HW_OLLAMA=0
  if curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then HW_OLLAMA=1; fi
  # Surcharges pour essayer d'autres machines (tests) : ASTRO_HW_CORES, _RAM_MB, _SWAP_MB, _DISK_FREE_MB, _DISK_KIND, _DISK_MBPS, _INT8_FAST.
  HW_CORES="${ASTRO_HW_CORES:-$HW_CORES}"; HW_RAM_MB="${ASTRO_HW_RAM_MB:-$HW_RAM_MB}"; HW_SWAP_MB="${ASTRO_HW_SWAP_MB:-$HW_SWAP_MB}"
  HW_DISK_FREE_MB="${ASTRO_HW_DISK_FREE_MB:-$HW_DISK_FREE_MB}"; HW_DISK_KIND="${ASTRO_HW_DISK_KIND:-$HW_DISK_KIND}"
  HW_DISK_MBPS="${ASTRO_HW_DISK_MBPS-$HW_DISK_MBPS}"; HW_INT8_FAST="${ASTRO_HW_INT8_FAST:-$HW_INT8_FAST}"
  return 0
}

pname() { case "$1" in leger) printf "léger" ;; confort) printf "confort" ;; *) printf "%s" "$1" ;; esac; }

fmt_mb() { awk -v m="$1" 'BEGIN { if (m >= 1024) printf "%.1f Go", m / 1024; else printf "%d Mo", m }' | tr . ,; }

# Remplit REC_FACE, REC_EMBED (1 = la machine tient), REC_PROFILE, REC_THREADS et REC_NOTES (ce qui limite ou mérite attention).
recommend() {
  REC_NOTES=(); REC_FACE=1; REC_EMBED=1
  case "$TARGET" in
    *-musl) REC_FACE=0; REC_EMBED=0; REC_NOTES+=("binaire statique (musl) : pas d'ONNX Runtime, donc ni visages ni embeddings locaux (--face-engine et --ollama vers une autre machine)") ;;
  esac
  if [ "$HW_RAM_MB" -le 0 ]; then
    REC_FACE=0; REC_EMBED=0; REC_NOTES+=("mémoire non détectée : profil léger par prudence")
  else
    if [ "$HW_RAM_MB" -lt 1800 ]; then REC_FACE=0; REC_NOTES+=("mémoire $(fmt_mb "$HW_RAM_MB") : il en faut ≈ 2 Go pour les visages locaux"); fi
    if [ "$HW_RAM_MB" -lt 3500 ]; then REC_EMBED=0; REC_NOTES+=("mémoire $(fmt_mb "$HW_RAM_MB") : bge-m3 monte à ≈ 1,3 Go, il en faut ≈ 4 Go pour le garder avec le reste"); fi
  fi
  if [ "$HW_DISK_FREE_MB" -lt 1000 ]; then REC_FACE=0; REC_NOTES+=("disque : $(fmt_mb "$HW_DISK_FREE_MB") libres, les modèles de visages demandent ≈ 1 Go de marge"); fi
  if [ "$HW_DISK_FREE_MB" -lt 2500 ]; then REC_EMBED=0; REC_NOTES+=("disque : $(fmt_mb "$HW_DISK_FREE_MB") libres, bge-m3 (≈ 560 Mo) demande ≈ 2,5 Go de marge"); fi
  if [ "$HW_CORES" -lt 2 ]; then REC_EMBED=0; REC_NOTES+=("un seul cœur : les embeddings locaux gêneraient le relai"); fi
  if [ -n "$HW_DISK_MBPS" ] && [ "$HW_DISK_MBPS" -lt 15 ]; then
    REC_EMBED=0; REC_NOTES+=("disque très lent (≈ ${HW_DISK_MBPS} Mo/s en écriture) : charger bge-m3 prendrait plus d'une minute à chaque démarrage")
  elif [ -n "$HW_DISK_MBPS" ] && [ "$HW_DISK_MBPS" -lt 40 ]; then
    REC_NOTES+=("disque lent (≈ ${HW_DISK_MBPS} Mo/s) : le démarrage chargera bge-m3 en ≈ $((560 / HW_DISK_MBPS)) s au moins")
  fi
  case "$HW_DISK_KIND" in
    sd) REC_NOTES+=("carte SD/eMMC : lente et usée par les écritures ; un SSD USB ou NVMe est bien préférable pour la base et les blobs") ;;
    hdd) REC_NOTES+=("disque mécanique : démarrage et recherche plus lents qu'avec un SSD") ;;
  esac
  [ "$REC_EMBED" = 1 ] && [ "$HW_INT8_FAST" = 0 ] && REC_NOTES+=("pas d'AVX2 / dotprod : les calculs int8 de bge-m3 seront plusieurs fois plus lents (quelques secondes par texte long)")
  if [ "$REC_FACE" = 0 ]; then REC_PROFILE=leger
  elif [ "$REC_EMBED" = 0 ]; then REC_PROFILE=standard
  elif [ "$HW_RAM_MB" -ge 7000 ] && [ "$HW_CORES" -ge 6 ]; then REC_PROFILE=confort
  else REC_PROFILE=standard
  fi
  threads_for "$REC_PROFILE"
}

# Fils de calcul par modèle : « confort » laisse ONNX Runtime prendre tous les cœurs (0) ; sinon on en garde la moitié pour le relai.
threads_for() {
  REC_THREADS=0
  [ "$1" = confort ] && return 0
  if [ "$HW_CORES" -le 2 ]; then REC_THREADS=1; elif [ "$HW_CORES" -le 4 ]; then REC_THREADS=2; else REC_THREADS=$((HW_CORES / 2)); fi
}

show_hardware() {
  say ""; say "▶ Votre machine"
  say "  Architecture : ${machine} (binaire ${TARGET})"
  say "  Processeur   : ${HW_CORES} cœur(s) — ${HW_CPU}$([ "$HW_INT8_FAST" = 1 ] && printf ' (calcul int8 rapide)')"
  say "  Mémoire      : $(fmt_mb "$HW_RAM_MB") (swap $(fmt_mb "$HW_SWAP_MB"))"
  say "  Disque       : $(fmt_mb "$HW_DISK_FREE_MB") libres dans ${HW_DISK_DIR} — ${HW_DISK_KIND}$([ -n "$HW_DISK_MBPS" ] && printf ', écriture ≈ %s Mo/s' "$HW_DISK_MBPS")"
  say "  GPU          : ${HW_GPU:-aucun NVIDIA détecté}$([ -n "$HW_GPU" ] && printf ' (inutile pour visages et embeddings : le CPU suffit)')"
  say "  Ollama       : $([ "$HW_OLLAMA" = 1 ] && printf 'joint sur 127.0.0.1:11434' || printf 'absent en local (résumés RÊVE : --ollama vers une machine qui l'\''héberge)')"
}

show_recommendation() {
  say ""; say "▶ Recommandation : profil « $(pname "$REC_PROFILE") »"
  say "  $([ "$REC_FACE" = 1 ] && printf '✔' || printf '✘') visages locaux (≈ 190 Mo de modèles)"
  say "  $([ "$REC_EMBED" = 1 ] && printf '✔' || printf '✘') embeddings locaux de BRO : bge-m3 int8 (≈ 560 Mo), recherche et mémoire sans Ollama pour les vecteurs"
  [ "$REC_PROFILE" = leger ] || [ "$REC_THREADS" = 0 ] || say "  ↳ ${REC_THREADS} fil(s) de calcul par modèle, pour laisser de la place au relai et à l'interface"
  local i; for i in "${!REC_NOTES[@]}"; do say "  ⚠ ${REC_NOTES[$i]}"; done
}

# Profil retenu : --profile, sinon la recommandation, que l'on propose de changer au terminal. Remplit WANT_FACE, WANT_EMBED, WANT_THREADS.
choose_profile() {
  local chosen="$PROFILE"
  if [ "$chosen" = auto ]; then
    chosen="$REC_PROFILE"
    if [ "$YES" = 0 ] && [ -r /dev/tty ]; then
      {
        say ""
        say "  1) léger     rien en local : visages et mémoire passent par une autre machine (Brain GPU, Ollama)"
        say "  2) standard  visages locaux, et embeddings locaux si la machine les porte ; fils de calcul limités"
        say "  3) confort   visages et embeddings locaux, tous les cœurs"
        local def=2; [ "$REC_PROFILE" = leger ] && def=1; [ "$REC_PROFILE" = confort ] && def=3
        printf '  Votre choix [%s, entrée = recommandé] : ' "$def"
      } > /dev/tty
      local a; read -r a < /dev/tty || a=""
      case "${a:-$def}" in 1) chosen=leger ;; 2) chosen=standard ;; 3) chosen=confort ;; *) warn "réponse inconnue (« $a ») : profil recommandé conservé" ;; esac
    fi
  fi
  WANT_FACE=0; WANT_EMBED=0
  case "$chosen" in
    standard) WANT_FACE=1; WANT_EMBED="$REC_EMBED" ;;
    confort)  WANT_FACE=1; WANT_EMBED=1 ;;
  esac
  case "$chosen" in
    standard|confort)
      [ "$REC_FACE" = 1 ] || warn "profil « $(pname "$chosen") » demandé alors que la machine est juste pour les visages locaux : à vos risques."
      [ "$chosen" = confort ] && [ "$REC_EMBED" = 0 ] && warn "profil « confort » demandé alors que la machine est juste pour bge-m3 : à vos risques." ;;
  esac
  [ "$NO_FACE" = 1 ] && WANT_FACE=0
  case "$EMBED" in yes) WANT_EMBED=1 ;; no) WANT_EMBED=0 ;; esac
  case "$TARGET" in *-musl) WANT_FACE=0; WANT_EMBED=0 ;; esac
  threads_for "$chosen"; WANT_THREADS="$REC_THREADS"
  PROFILE_CHOSEN="$chosen"
}

detect_hardware
# Version à installer (« latest » : la dernière release, préversions comprises).
resolve_version() {
  if [ "$VERSION" = "latest" ]; then
    # Une préversion n'est jamais « latest » pour GitHub : on interroge alors la liste des releases.
    local api="https://api.github.com/repos/${REPO}/releases?per_page=1" tag
    tag="$(curl -fsSL "$api" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1 || true)"
    [ -n "$tag" ] || die "impossible de lire la dernière release sur $api (réseau ? aucune release publiée ?)."
    VERSION="$tag"
  fi
  case "$VERSION" in v*) ;; *) VERSION="v${VERSION}" ;; esac
}

# Oui/non au terminal (oui par défaut ; --yes répond oui ; sans terminal : non).
confirm() {
  [ "$YES" = 1 ] && return 0
  [ -r /dev/tty ] || return 1
  printf '%s [O/n] ' "$1" > /dev/tty
  local a; read -r a < /dev/tty || a="n"
  case "${a:-o}" in n|N|non|Non) return 1 ;; *) return 0 ;; esac
}

if [ "$UPDATE" = 1 ] && [ "$CHECK_ONLY" = 0 ]; then
  resolve_version
  if [ "${VERSION#v}" = "$INSTALLED" ] && [ "$FORCE" = 0 ]; then
    say ""; say "✔ Déjà à jour (${INSTALLED}). Rien à faire (--force pour réinstaller). Vérifier : ${BIN} doctor"
    exit 0
  fi
  say ""
  confirm "Mettre à jour ${INSTALLED} → ${VERSION#v} ?" || { say "D'accord, rien n'a changé."; exit 0; }
fi

recommend
if [ "$UPDATE" = 0 ] || [ "$CHECK_ONLY" = 1 ]; then
  show_hardware
  show_recommendation
fi
if [ "$CHECK_ONLY" = 1 ]; then
  say ""; say "Diagnostic seul (--check-hardware) : rien n'a été installé."
  exit 0
fi
if [ "$UPDATE" = 1 ] && [ "$PROFILE" = auto ]; then
  # Mise à jour : on garde les choix de la première installation, lus sur le disque.
  WANT_FACE=0; WANT_EMBED=0; WANT_THREADS=0; PROFILE_CHOSEN=actuel
  { [ -f "${MODELS}/det_10g.onnx" ] || [ -f "${HOME}/.insightface/models/buffalo_l/det_10g.onnx" ]; } && WANT_FACE=1
  [ -f "${MODELS}/bge-m3-int8.onnx" ] && WANT_EMBED=1
  [ "$NO_FACE" = 1 ] && WANT_FACE=0
  case "$EMBED" in yes) WANT_EMBED=1 ;; no) WANT_EMBED=0 ;; esac
else
  choose_profile
fi
say ""; say "▶ Profil retenu : $(pname "$PROFILE_CHOSEN") (visages locaux : $([ "$WANT_FACE" = 1 ] && printf oui || printf non), embeddings locaux : $([ "$WANT_EMBED" = 1 ] && printf oui || printf non))"

# --- Téléchargement ---------------------------------------------------------------------------------------------
resolve_version
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
if ! "${PREFIX}/bin/${BIN}" --version 2>/dev/null; then
  "${PREFIX}/bin/${BIN}" --version 2>&1 | head -n 3 >&2 || true
  die "le binaire installé ne démarre pas (architecture ou glibc incompatible ?). Relancez avec --static : le binaire statique n'a aucune dépendance."
fi

# --- Modèles locaux : visages (SCRFD + ArcFace) et embeddings de BRO (bge-m3) --------------------------------------
# Binaire glibc = compilé avec `onnx`. Il lui faut libonnxruntime (extraite d'une roue pip) et les modèles. Rien n'est
# jamais accepté sans SHA-256 correct. Visages : IPFS (UPlanet) puis web ; bge-m3 : Hugging Face, révision figée.
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


# bge-m3 int8 (Xenova/bge-m3, licence MIT), révision figée : nom installé | chemin dans le dépôt | SHA-256 | taille en Mo
BGE_REV="4de13258303883538bd53b696b452bf8099f0858"
BGE_BASE="${ASTRO_BGE_BASE_URL:-https://huggingface.co/Xenova/bge-m3/resolve/${BGE_REV}}"
BGE_FILES=(
  "bge-m3-int8.onnx|onnx/model_int8.onnx|a206e10e995aa2a833924bcd725ba5dd6c3425cd34bac3cf2b5677cd2a1c51d6|542"
  "bge-m3-tokenizer.json|tokenizer.json|6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790|17"
)

# Télécharge un fichier (reprise possible) et ne le garde que si son SHA-256 est correct.
fetch_checked() { # nom chemin sha taille_mo
  local name="$1" path="$2" sha="$3" size="$4" dest="${MODELS}/$1" part="${MODELS}/$1.part" try
  if [ -f "$dest" ] && [ "$(sha256sum "$dest" | cut -d' ' -f1)" = "$sha" ]; then say "  ✔ $name déjà présent"; return 0; fi
  mkdir -p "$MODELS"
  local free; free="$(df -Pm "$MODELS" 2>/dev/null | awk 'NR==2 {print $4}')"
  if [ "${free:-0}" -lt $((size * 2 + 200)) ]; then warn "$name : place insuffisante dans ${MODELS} ($(fmt_mb "${free:-0}") libres, $((size * 2 + 200)) Mo demandés)"; return 1; fi
  local prog=(-sS); [ -t 2 ] && prog=(-#)
  for try in 1 2; do
    say "  $name : téléchargement (${size} Mo)…"
    curl -fL "${prog[@]}" --proto '=https' --tlsv1.2 -C - -o "$part" "${BGE_BASE}/${path}" || { rm -f "$part"; continue; }
    if [ "$(sha256sum "$part" | cut -d' ' -f1)" = "$sha" ]; then mv "$part" "$dest"; say "  ✔ $name vérifié (SHA-256)"; return 0; fi
    rm -f "$part"; warn "$name : SHA-256 incorrect (essai ${try}/2)"
  done
  return 1
}

install_embed_models() {
  local m n p sha size
  for m in "${BGE_FILES[@]}"; do
    IFS='|' read -r n p sha size <<< "$m"
    fetch_checked "$n" "$p" "$sha" "$size" || { warn "embeddings locaux non installés : placez ${n} dans ${MODELS} (Xenova/bge-m3, ${p})"; return 1; }
  done
}

# Seulement ce que le profil retenu demande.
if [ "$WANT_FACE" = 1 ] || [ "$WANT_EMBED" = 1 ]; then
  say ""; say "▶ Modèles locaux"
  if install_ort; then
    if [ "$WANT_FACE" = 1 ]; then install_models || warn "reconnaissance de visages locale incomplète : voir « astroportd face-check » (ou serve --face-engine http://…)"; fi
    if [ "$WANT_EMBED" = 1 ]; then install_embed_models || warn "embeddings locaux incomplets : voir « astroportd embed-check » (ou serve --ollama http://… pour les vecteurs)"; fi
  else
    warn "ONNX Runtime absent : ni visages ni embeddings locaux (voir « astroportd face-check » / « embed-check »)"
  fi
  if [ "$WANT_EMBED" = 1 ] && [ -f "${MODELS}/bge-m3-int8.onnx" ]; then
    "${PREFIX}/bin/${BIN}" embed-check || warn "essai des embeddings locaux en échec : voir le message ci-dessus"
  fi
fi

# --- Ports déjà occupés (cause fréquente de « Address already in use ») ------------------------------------------
if command -v ss >/dev/null 2>&1; then
  busy=""; for p in 8090 80 443; do ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${p}$" && busy="${busy} ${p}"; done
  if [ -n "$busy" ]; then
    warn "Ports déjà utilisés sur cette machine :${busy} (IPFS, Nginx Proxy Manager…)."
    warn "  « astroportd init » en tient compte : si 80/443 sont pris, astroportd écoute sur le port 8090 derrière votre proxy ; sinon il gère lui-même HTTPS."
    warn "  Proxy sur une autre machine : « astroportd init --proxy »."
  fi
fi

case ":${PATH}:" in
  *":${PREFIX}/bin:"*) ;;
  *) warn "${PREFIX}/bin n'est pas dans votre PATH. Ajoutez à ~/.bashrc :  export PATH=\"${PREFIX}/bin:\$PATH\"" ;;
esac

# --- Mise à jour : redémarrer le service, jamais « init » --------------------------------------------------------
if [ "$UPDATE" = 1 ]; then
  say ""
  case "$(service_state)" in
    active)
      if confirm "Redémarrer le service ${BIN} pour passer à la nouvelle version ?"; then
        systemctl --user restart "$BIN" || warn "redémarrage refusé par systemd : journalctl --user -u ${BIN}"
        sleep 3
        if [ "$(service_state)" = active ]; then say "  ✔ ${BIN} redémarré : $("${PREFIX}/bin/${BIN}" --version 2>/dev/null)"
        else warn "le service ne tourne pas après le redémarrage : journalctl --user -u ${BIN} -n 50"; fi
      else
        say "  Le service tourne encore l'ancienne version : systemctl --user restart ${BIN}"
      fi ;;
    absent)
      if pgrep -x "$BIN" >/dev/null 2>&1; then warn "${BIN} tourne hors systemd : arrêtez-le et relancez votre commande « ${BIN} serve … » pour passer à la nouvelle version."
      else say "  Démarrer : systemctl --user start ${BIN} (ou votre commande « ${BIN} serve … »)"; fi ;;
    *) say "  Le service est arrêté : systemctl --user start ${BIN}" ;;
  esac
  say ""
  say "Mise à jour terminée. Station, comptes et données conservés. Vérifier : ${BIN} doctor"
  exit 0
fi

# --- Plugins : présentation et « git clone » par install-plugins.sh (media, energy, dnslink…) --------------------------
PLUGINS_SCRIPT="${ASTRO_PLUGINS_SCRIPT:-https://raw.githubusercontent.com/papiche/astroportd-plugins/main/install-plugins.sh}"
offer_plugins() {
  [ "$YES" = 1 ] && return 0
  [ -r /dev/tty ] || { say "Plugins (films, énergie, DNS…) : curl -fsSL ${PLUGINS_SCRIPT} | bash"; return 0; }
  printf '\nVoir et installer des plugins (films et musique, énergie, DNSLink) ? [o/N] ' > /dev/tty
  read -r answer < /dev/tty || answer="n"
  case "${answer:-n}" in
    o|O|oui|Oui|y|Y) curl -fsSL --proto '=https' --tlsv1.2 "$PLUGINS_SCRIPT" | bash || warn "installation des plugins interrompue : relancez-la quand vous voulez (${PLUGINS_SCRIPT})." ;;
    *) say "D'accord. Quand vous voulez : curl -fsSL ${PLUGINS_SCRIPT} | bash" ;;
  esac
}

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
# Fils de calcul choisis d'après la machine (0 = tous les cœurs : on n'écrit rien).
if [ "$WANT_THREADS" -gt 0 ]; then
  [ "$WANT_FACE" = 1 ] && init_args+=(--face-threads "$WANT_THREADS")
  [ "$WANT_EMBED" = 1 ] && init_args+=(--embed-threads "$WANT_THREADS")
fi

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
  offer_plugins
else
  say ""
  say "Pas de terminal interactif. Étape suivante : ${PREFIX}/bin/${BIN} init"
fi
