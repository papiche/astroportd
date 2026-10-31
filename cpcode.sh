#!/bin/bash
# Copie le code du dépôt dans le presse-papiers (via `cpcode`) pour le donner à un assistant, sans ce qui n'est pas du code
# à lire : bibliothèque tierce embarquée (xterm), dictionnaires de langue, documentation.
#
# Usage : ./cpcode.sh [--tests] [--docs] [--all] [options de cpcode…]
#   (rien)    code de production seul
#   --tests   ajoute les tests (tests/, crates/*/tests/)
#   --docs    ajoute README, docs/ et notes de version
#   --all     tout (équivaut à --tests --docs, plus xterm et les dictionnaires)
# Les autres options sont transmises à cpcode (ex. --maxfilesize 100000, --json, --exclude motif).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

tests=false docs=false all=false
args=()
for a in "$@"; do
  case "$a" in
    --tests) tests=true ;;
    --docs) docs=true ;;
    --all) all=true ;;
    *) args+=("$a") ;;
  esac
done

exclude=()
$all || exclude+=(--exclude xterm --exclude m-lang-)
$all || $tests || exclude+=(--exclude tests/)
$all || $docs || exclude+=(--exclude docs/ --exclude README.md --exclude RELEASE_NOTES.md)

exec cpcode "${exclude[@]}" "${args[@]}" toml rs json sql html css js md ./
