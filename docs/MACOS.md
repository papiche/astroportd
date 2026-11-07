# Une station sur Mac (Apple Silicon)

> ⚠️ **Attention.** La version Mac est en **bêta**. Le programme a été écrit pour fonctionner sur macOS, mais **aucune compilation sur un Mac n'avait été éprouvée** quand cette page a été écrite. La vérification automatique sur Mac et les archives Mac toutes prêtes sont « au mieux » : si quelque chose échoue, ouvrez un ticket en joignant la sortie de `cargo build`.

## Pour qui c'est

Cette page s'adresse à un futur Capitaine qui veut faire tourner sa station sur un **Mac à puce Apple** (M1 à M4) et qui est à l'aise avec le Terminal. Sur Mac, il n'y a pas encore d'installeur : vous **compilez** le programme vous-même (vous le fabriquez à partir de son code source), en quelques commandes.

Si vous cherchez la voie la plus simple, une machine Linux (Raspberry Pi, mini-PC, vieux PC) reste la voie principale : suivez le [guide du Capitaine](GUIDE.md) depuis l'[étape 1](GUIDE.md#etape-0).

Pourquoi un Mac quand même ? Sur un Mac Apple Silicon, la **mémoire unifiée** (la même mémoire sert au processeur et à la puce graphique) peut servir aux outils d'IA comme Ollama (un moteur d'IA) et ComfyUI (un générateur d'images). Un Mac peut ainsi devenir un « Brain GPU » de votre constellation : la machine à qui les autres stations confient les calculs lourds.

## Ce qui marche, ce qui ne marche pas encore

| | État sur Mac |
|---|---|
| Compiler et lancer la station | prévu, **pas encore éprouvé** |
| Mesure de la machine (Power-Score) | lit la mémoire du Mac ; compte une part de la mémoire unifiée comme mémoire graphique (estimation, voir plus bas) |
| Reconnaissance des visages sur la station | possible avec ONNX Runtime installé par Homebrew |
| Partager Ollama, ComfyUI et SSH avec les autres Capitaines | oui, par le maillage P2P, selon vos réglages |
| Installeur `install.sh` | **non** : il ne fonctionne que sous Linux |
| Démarrage automatique | **à faire à la main** (launchd) : `astroportd init --systemd` n'écrit que l'unité Linux, aucune unité launchd n'est générée |
| Mac Intel | pas de mémoire unifiée : aucune mémoire graphique n'est comptée, sauf si vous la déclarez |

> **À savoir.** Le Power-Score est une **estimation**, pas une mesure de ce que la puce graphique sait faire. Il sert à la constellation pour choisir à quelle station confier un calcul.

## Pas à pas

### 1. Installer les outils

```bash
xcode-select --install                    # compilateur C (SQLite et libwebp sont compilés depuis leurs sources)
brew install rustup ffmpeg pkg-config     # ffmpeg sert au traitement des photos et vidéos
rustup default stable                     # installe le compilateur Rust
```

Ces commandes demandent [Homebrew](https://brew.sh), le gestionnaire de logiciels le plus courant sur Mac.

### 2. Compiler la station

Depuis le dossier du code source d'astroportd :

```bash
cargo build --release --features onnx     # avec la reconnaissance des visages sur la station
# ou : cargo build --release              # sans : les visages sont confiés à un service externe (Brain HTTP)
```

Le programme se trouve ensuite dans `./target/release/astroportd`.

> **À savoir.** `./build-all.sh --targets aarch64-apple-darwin` produit une archive prête à distribuer : `dist/astroportd-<version>-aarch64-apple-darwin.tar.gz`. Une archive Mac ne se construit que **sur** un Mac.

### 3. Préparer la station

```bash
./target/release/astroportd init          # Y-Level, puis choix ORIGIN ou ẐEN
```

`init` lie la clé de votre Mac à l'identité de la station (Y-Level, voir l'[étape 3 du guide](GUIDE.md#etape-2)) et vous fait choisir votre constellation (voir [Constellations](CONSTELLATIONS.md)).

### 4. (Facultatif) Reconnaissance des visages sur la station

```bash
brew install onnxruntime
export ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib   # seulement si la station ne la trouve pas
astroportd face-check                                          # dit ce qui manque, et comment l'obtenir
```

La station cherche la bibliothèque `libonnxruntime.dylib` dans cet ordre : `ORT_DYLIB_PATH`, puis `~/.zen/astroportd/lib`, `/opt/homebrew/lib`, `/usr/local/lib`.

### 5. Partager Ollama et ComfyUI

```bash
brew install ollama && ollama serve &          # Ollama, port 11434
# ComfyUI : installation PyTorch/MPS, port 8188
./target/release/astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs \
    --p2p-listen /ip4/0.0.0.0/tcp/4101
```

La station offre au maillage Ollama (`127.0.0.1:11434`), ComfyUI (`127.0.0.1:8188`) et SSH (`127.0.0.1:22`). **C'est vous qui décidez qui peut les ouvrir**, dans *Options › Tunnels*. Au départ, Ollama et ComfyUI sont ouverts à vos Capitaines amis ; le SSH est fermé en ẐEN et ouvert à vos Capitaines amis en ORIGIN. Détails dans [Constellations](CONSTELLATIONS.md) et, dans le guide, à [Relier sa station aux autres Capitaines](GUIDE.md#etape-9).

Pour que le SSH existe sur le Mac, activez *Réglages Système › Partage › Session à distance*.

### 6. Démarrer la station à l'ouverture de session (launchd)

launchd est le service de macOS qui lance des programmes au démarrage. Créez le fichier `~/Library/LaunchAgents/org.uplanet.astroportd.plist` (remplacez `VOTRE_USER` par votre nom d'utilisateur) :

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>org.uplanet.astroportd</string>
  <key>ProgramArguments</key><array>
    <string>/Users/VOTRE_USER/astroportd/target/release/astroportd</string><string>serve</string>
    <string>--db</string><string>/Users/VOTRE_USER/.zen/astroportd/uplanet.db</string>
    <string>--blobs</string><string>/Users/VOTRE_USER/.zen/astroportd/blobs</string>
    <string>--p2p-listen</string><string>/ip4/0.0.0.0/tcp/4101</string>
  </array>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/VOTRE_USER/.zen/astroportd/astroportd.log</string>
  <key>StandardErrorPath</key><string>/Users/VOTRE_USER/.zen/astroportd/astroportd.log</string>
</dict></plist>
```

Puis chargez-le :

```bash
launchctl load ~/Library/LaunchAgents/org.uplanet.astroportd.plist
```

### 7. Empêcher le Mac de s'endormir

Une station endormie ne rend plus service à ses membres.

```bash
sudo pmset -a sleep 0
```

Ou bien *Réglages Système › Batterie / Économie d'énergie*.

## Limites connues

- Aucune compilation macOS éprouvée à ce jour (voir l'état de la vérification automatique).
- `init --systemd` ne s'applique pas ; pas d'unité `launchd` générée : il faut écrire le fichier ci-dessus.
- Le Power-Score est une estimation, pas une mesure de la puce graphique.

## Pour aller plus loin (technique)

### Comment la station mesure un Mac

- **Mémoire** : lue avec `sysctl -n hw.memsize` (macOS n'a pas de `/proc/meminfo`) ; voir `crates/astro-p2p/src/routing.rs`.
- **Mémoire graphique (VRAM)** : sur Apple Silicon sans carte NVIDIA, la station compte **66 % de la mémoire unifiée** (arrondi à l'entier inférieur), le système gardant le reste. Ce 66 % est un choix prudent, **non mesuré**.
- **Formule** : Power-Score = `VRAM×4 + cœurs×2 + RAM×0,5`. Un Mac de 16 Go compte donc 10 Go de VRAM, soit `10×4 + cœurs×2 + 16×0,5`.
- **Réglage manuel** : `ASTRO_VRAM_GB=<Go>` remplace l'estimation. Réglez-le d'après ce que vos modèles occupent vraiment.
- **Mac Intel** : VRAM à 0, sauf `ASTRO_VRAM_GB`.

### Portabilité et vérification automatique

Le code est portable (POSIX, `rustix`, aucun `/proc` obligatoire). La vérification automatique Mac est la tâche `macos` de `.github/workflows/ci.yml` ; elle est non bloquante tant qu'elle n'a pas réussi une première fois. Les archives `*-apple-darwin` (`aarch64-apple-darwin` pour Apple Silicon, `x86_64-apple-darwin` pour Mac Intel) sont « au mieux » ; elles sont construites avec l'inférence locale (`--features onnx`), la bibliothèque `libonnxruntime.dylib` étant chargée à l'exécution.
