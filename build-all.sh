#!/usr/bin/env bash
# Compile astroportd pour toutes les plateformes supportées et dépose les archives dans dist/.
#
#   ./build-all.sh                    # toutes les cibles dont la chaîne d'outils est présente
#   ./build-all.sh --strict           # échoue (code 1) si une cible n'a pas pu être construite
#   ./build-all.sh --targets x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu
#   ./build-all.sh --list             # cibles supportées et état de leur chaîne d'outils
#   ./build-all.sh --debug            # profil dev (rapide) au lieu de release
#   ./build-all.sh --test             # lance aussi `cargo test --workspace` sur la machine hôte
#
# Cibles (Linux uniquement : le binaire dépend de /proc, systemd et des outils POSIX de la station) :
#   x86_64-unknown-linux-gnu       PC / serveur / VM                              (natif)
#   aarch64-unknown-linux-gnu      Raspberry Pi 4/5, Pi Zero 2 W en 64 bits       (gcc-aarch64-linux-gnu)
#   armv7-unknown-linux-gnueabihf  Raspberry Pi 2/3, Pi OS 32 bits                (gcc-arm-linux-gnueabihf)
#   x86_64-unknown-linux-musl      binaire statique, sans dépendance à la glibc   (musl-tools)
#   aarch64-unknown-linux-musl     binaire statique ARM 64 bits : smartphone Android (Termux), Pi, NAS
#                                                                                 (zig : pip install ziglang cargo-zigbuild)
#
# Une cible dont le compilateur C croisé ou le `rustup target` manque est SAUTÉE en le disant (jamais en silence) :
# le récapitulatif final la liste avec la commande d'installation.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

ALL_TARGETS=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
PROFILE=release
STRICT=0
RUN_TESTS=0
LIST=0
TARGETS=("${ALL_TARGETS[@]}")
BIN=astroportd
OUT=dist

usage() { sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --strict) STRICT=1 ;;
    --debug) PROFILE=debug ;;
    --test) RUN_TESTS=1 ;;
    --list) LIST=1 ;;
    --targets) shift; IFS=, read -r -a TARGETS <<< "${1:?--targets attend une liste séparée par des virgules}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "option inconnue : $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# Compilateur C/éditeur de liens croisé nécessaire à la cible (vide pour une cible native).
cross_gcc() {
  case "$1" in
    aarch64-unknown-linux-gnu) echo aarch64-linux-gnu-gcc ;;
    armv7-unknown-linux-gnueabihf) echo arm-linux-gnueabihf-gcc ;;
    x86_64-unknown-linux-musl) echo musl-gcc ;;
    *) echo "" ;;
  esac
}

# Cibles compilées avec zig comme compilateur C/éditeur de liens (pas de chaîne gcc croisée musl pour l'ARM 64 bits).
uses_zigbuild() {
  case "$1" in
    aarch64-unknown-linux-musl) return 0 ;;
    *) return 1 ;;
  esac
}

zig_ready() { # cargo-zigbuild présent, et zig (binaire ou paquet Python ziglang)
  command -v cargo-zigbuild >/dev/null 2>&1 && { command -v zig >/dev/null 2>&1 || python3 -c 'import ziglang' >/dev/null 2>&1; }
}

# Paquet Debian/Ubuntu qui fournit ce compilateur.
install_hint() {
  case "$1" in
    aarch64-unknown-linux-gnu) echo "sudo apt install gcc-aarch64-linux-gnu && rustup target add $1" ;;
    armv7-unknown-linux-gnueabihf) echo "sudo apt install gcc-arm-linux-gnueabihf && rustup target add $1" ;;
    x86_64-unknown-linux-musl) echo "sudo apt install musl-tools && rustup target add $1" ;;
    aarch64-unknown-linux-musl) echo "pip install ziglang cargo-zigbuild && rustup target add $1" ;;
    *) echo "rustup target add $1" ;;
  esac
}

# Variables d'environnement cargo pour la cible : éditeur de liens, et CC pour les crates à code C embarqué
# (SQLite « bundled », ring). Rend le nom de variable en majuscules avec des « _ ».
cross_env() {
  local t="$1" gcc var up
  gcc="$(cross_gcc "$t")"
  up="$(echo "$t" | tr 'a-z-' 'A-Z_')"
  var="$(echo "$t" | tr '-' '_')"
  if [ -n "$gcc" ]; then
    export "CARGO_TARGET_${up}_LINKER=$gcc"
    export "CC_${var}=$gcc"
  fi
  # musl n'a pas les alias BSD u_int8_t/u_int16_t/u_int64_t que sqlite-vec (C) utilise : on les redirige vers <stdint.h>.
  case "$t" in
    *-musl) export "CFLAGS_${var}=-Du_int8_t=uint8_t -Du_int16_t=uint16_t -Du_int64_t=uint64_t" ;;
  esac
}

target_state() { # ok | "raison"
  local t="$1" gcc
  if ! rustup target list --installed 2>/dev/null | grep -qx "$t"; then
    echo "cible rustup absente"; return
  fi
  if uses_zigbuild "$t"; then
    zig_ready || { echo "zig / cargo-zigbuild absent"; return; }
    echo ok; return
  fi
  gcc="$(cross_gcc "$t")"
  if [ -n "$gcc" ] && ! command -v "$gcc" >/dev/null 2>&1; then
    echo "compilateur $gcc absent"; return
  fi
  echo ok
}

if [ "$LIST" = 1 ]; then
  for t in "${ALL_TARGETS[@]}"; do
    s="$(target_state "$t")"
    if [ "$s" = ok ]; then printf '  ✓ %-34s prête\n' "$t"; else printf '  ✗ %-34s %s → %s\n' "$t" "$s" "$(install_hint "$t")"; fi
  done
  exit 0
fi

command -v cargo >/dev/null 2>&1 || { echo "cargo introuvable : installer Rust (https://rustup.rs)" >&2; exit 1; }

VERSION="$(cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="astroportd"))' 2>/dev/null || echo 0.0.0)"
CARGO_FLAGS=(--locked --bin "$BIN")
[ "$PROFILE" = release ] && CARGO_FLAGS+=(--release)

if [ "$RUN_TESTS" = 1 ]; then
  echo "▶ cargo test --workspace (hôte)"
  cargo test --workspace --locked
fi

mkdir -p "$OUT"
built=(); skipped=(); failed=()

for t in "${TARGETS[@]}"; do
  state="$(target_state "$t")"
  if [ "$state" != ok ]; then
    echo "⏭  $t : $state — sauté ($(install_hint "$t"))" >&2
    skipped+=("$t")
    continue
  fi
  echo "▶ $t ($PROFILE)"
  build=(cargo build)
  uses_zigbuild "$t" && build=(cargo zigbuild)
  # Inférence faciale locale (ONNX Runtime chargé à l'exécution) : impossible dans un binaire statique musl.
  feat=()
  case "$t" in *-gnu*) feat=(--features onnx) ;; esac
  if ( cross_env "$t"; "${build[@]}" "${CARGO_FLAGS[@]}" ${feat[@]+"${feat[@]}"} --target "$t" ); then
    src="target/$t/$PROFILE/$BIN"
    name="$BIN-$VERSION-$t"
    stage="$OUT/$name"
    rm -rf "$stage"; mkdir -p "$stage"
    cp "$src" "$stage/$BIN"
    strip_tool="$(cross_gcc "$t" | sed 's/gcc$/strip/')"
    if [ -n "$strip_tool" ] && command -v "$strip_tool" >/dev/null 2>&1; then
      "$strip_tool" "$stage/$BIN" || echo "⚠ strip a échoué pour $t : binaire gardé tel quel" >&2
    fi
    [ -f README.md ] && cp README.md "$stage/"
    tar -C "$OUT" -czf "$OUT/$name.tar.gz" "$name"
    rm -rf "$stage"
    ( cd "$OUT" && sha256sum "$name.tar.gz" >> SHA256SUMS.tmp )
    built+=("$t")
  else
    echo "✗ $t : la compilation a échoué" >&2
    failed+=("$t")
  fi
done

# SHA256SUMS : une ligne par archive, dédoublonnée.
if [ -f "$OUT/SHA256SUMS.tmp" ]; then
  sort -u -k2 "$OUT/SHA256SUMS.tmp" > "$OUT/SHA256SUMS" && rm -f "$OUT/SHA256SUMS.tmp"
fi

echo
echo "── Récapitulatif ($PROFILE, astroportd $VERSION) ──"
for t in "${built[@]}";   do echo "  ✓ $t → $OUT/$BIN-$VERSION-$t.tar.gz"; done
for t in "${skipped[@]}"; do echo "  ⏭ $t (chaîne d'outils absente : $(install_hint "$t"))"; done
for t in "${failed[@]}";  do echo "  ✗ $t (échec de compilation)"; done

[ "${#failed[@]}" -eq 0 ] || exit 1
[ "$STRICT" = 0 ] || [ "${#skipped[@]}" -eq 0 ] || exit 1
[ "${#built[@]}" -gt 0 ] || { echo "aucune cible construite" >&2; exit 1; }
