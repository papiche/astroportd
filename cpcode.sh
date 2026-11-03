#!/bin/bash
# Copie le code du dépôt dans le presse-papiers (via `cpcode`) pour le donner à un assistant, sans ce qui n'est pas du code
# à lire : bibliothèque tierce embarquée (xterm), dictionnaires de langue, documentation.
#
# Usage : ./cpcode.sh [--tests] [--docs] [--all] [options de cpcode…]
#   (rien)    code de production seul (modules de tests inline retirés)
#   --tests   ajoute les tests (tests/, crates/*/tests/ et les modules `#[cfg(test)]` inline des sources)
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

# Par défaut, les modules de tests inline sont retirés d'une COPIE du dépôt (le dépôt n'est jamais modifié).
if $all || $tests; then
  exec cpcode "${exclude[@]}" "${args[@]}" toml rs json sql html css js md ./
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git ls-files -z | xargs -0 -I{} cp --parents {} "$work"
python3 -I "$(pwd)/strip_tests.py" "$work"
cd "$work"
cpcode "${exclude[@]}" "${args[@]}" toml rs json sql html css js md ./
