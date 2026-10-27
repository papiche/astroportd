#!/bin/sh
# Compile les plugins d'exemple en WebAssembly et dépose les `.wasm` dans ce dossier.
# Prérequis : rustup target add wasm32-unknown-unknown
set -eu
cd "$(dirname "$0")"
for src in */; do
    name="${src%/}"
    [ -f "$name/Cargo.toml" ] || continue
    (cd "$name" && cargo build --release --target wasm32-unknown-unknown)
    cp "$name/target/wasm32-unknown-unknown/release/$name.wasm" "./$name.wasm"
    echo "plugins/$name.wasm : $(wc -c < "./$name.wasm") octets"
done
