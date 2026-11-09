#!/bin/bash
# Copie le code du dépôt dans le presse-papiers (via `cpcode`) pour le donner à un assistant, sans ce qui n'est pas du code
# à lire : bibliothèque tierce embarquée (xterm, *.min.js), dictionnaires de langue, documentation.
#
# Usage : ./cpcode.sh [--tests] [--docs] [--all] [--lean] [--only zone1,zone2] [options de cpcode…]
#   (rien)    code de production seul (modules de tests inline retirés) : ~940 K tokens, au-delà de certains assistants
#   --lean    retire les commentaires `///` et `//` (garde les en-têtes `//!` des modules) : ~780 K tokens
#   --only Z  code complet des zones Z (séparées par des virgules) ; ailleurs, le plan de chaque module (en-tête et
#             signatures publiques). `./cpcode.sh --zones` les liste. Ex. : --only p2p,web
#   --tests   ajoute les tests (tests/, crates/*/tests/ et les modules `#[cfg(test)]` inline des sources)
#   --docs    ajoute README, docs/ et notes de version
#   --all     tout (équivaut à --tests --docs, plus xterm et les dictionnaires)
# Les autres options sont transmises à cpcode (ex. --maxfilesize 100000, --json, --exclude motif).
# Le dépôt n'est jamais modifié : tout se fait sur une copie (fichiers suivis et nouveaux fichiers non ignorés).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
repo=$(pwd)

tests=false docs=false all=false lean=false only=""
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --tests) tests=true ;;
    --docs) docs=true ;;
    --all) all=true ;;
    --lean) lean=true ;;
    --only) only="${2:?--only attend des zones, ex. --only p2p,web}"; shift ;;
    --only=*) only="${1#--only=}" ;;
    --zones) echo "Zones de --only :"; exec python3 -I "$repo/trim_copy.py" --zones ;;
    -h|--help)
      cat <<'EOF'
cpcode.sh — copie le code d'astroportd dans le presse-papiers, pour le donner à un assistant (Gemini, Claude…).
Le dépôt n'est jamais modifié : tout se fait sur une copie (fichiers suivis et nouveaux fichiers non ignorés).

Usage : ./cpcode.sh [options]

Choisir ce qui est copié :
  (rien)            tout le code de production, sans les tests            ≈ 950 K tokens
  --lean            sans les commentaires /// et // (garde les en-têtes //!)  ≈ 810 K tokens
  --only Z1,Z2      code complet des zones choisies ; le reste réduit à son plan
                    (en-tête du module, signatures publiques)             ≈ 220 à 430 K tokens par zone
  --zones           liste les zones de --only
  --tests           ajoute les tests (avec --only : ceux des modules de la zone)
  --docs            ajoute README, docs/ et notes de version
  --all             tout : tests, docs, xterm et dictionnaires de langue

Format de sortie (passé à cpcode) :
  --json            sortie JSON
  --maxfilesize N   tronque chaque fichier à N octets
  --exclude MOTIF   exclut les chemins qui contiennent MOTIF (répétable)

Exemples :
  ./cpcode.sh --only p2p              une question sur le maillage
  ./cpcode.sh --lean --only p2p,web   deux zones, sans commentaires
  ./cpcode.sh --only eco --tests      l'économie et ses tests

Zones :
EOF
      exec python3 -I "$repo/trim_copy.py" --zones ;;
    *) args+=("$1") ;;
  esac
  shift
done

exclude=()
$all || exclude+=(--exclude xterm --exclude m-lang-)
$all || $tests || exclude+=(--exclude tests/)
$all || $docs || exclude+=(--exclude docs/ --exclude README.md --exclude RELEASE_NOTES.md)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Nouveaux fichiers compris (pas encore commités), fichiers ignorés exclus.
git ls-files -z --cached --others --exclude-standard | while IFS= read -r -d '' f; do
  [ -f "$f" ] && cp --parents -- "$f" "$work"
done
$all || $tests || python3 -I "$repo/strip_tests.py" "$work"
trim=()
$lean && trim+=(--lean)
[ -n "$only" ] && trim+=(--only "$only")
if $all || $tests; then trim+=(--tests); fi
if [ ${#trim[@]} -gt 0 ]; then python3 -I "$repo/trim_copy.py" "$work" "${trim[@]}"; fi
cd "$work"
cpcode "${exclude[@]}" "${args[@]}" toml rs json sql html css js md ./
