# astroportd sur macOS

> **Statut : bêta.** Le code est portable (POSIX, `rustix`, aucun `/proc` obligatoire) et la détection du matériel
> connaît macOS, mais **aucune compilation sur un Mac n'avait été éprouvée** quand ce document a été écrit. La CI macOS
> (`.github/workflows/ci.yml`, tâche `macos`) et les archives `*-apple-darwin` sont « au mieux » : si elles échouent,
> ouvrez un ticket avec la sortie de `cargo build`.

## Pourquoi un Mac

Un Mac Apple Silicon (M1 à M4) partage sa **mémoire unifiée** entre le processeur et le GPU (Metal) : 16 à 32 Go y
servent de VRAM à Ollama et à ComfyUI. C'est un bon **Brain GPU** pour une constellation.

## Ce que fait la station

- **Power-Score** : la RAM vient de `sysctl -n hw.memsize` (pas de `/proc/meminfo`). Sur Apple Silicon sans carte NVIDIA,
  la VRAM est comptée comme **66 % de la mémoire unifiée** (le système garde le reste) ; un Mac de 16 Go vaut donc
  10 Go de VRAM, soit un Power-Score de `10×4 + cœurs×2 + 16×0,5`. `ASTRO_VRAM_GB=<Go>` remplace cette estimation.
  Ce 66 % est un choix prudent, **non mesuré** : réglez `ASTRO_VRAM_GB` d'après ce que vos modèles occupent vraiment.
- **Mac Intel** : pas de mémoire unifiée, VRAM à 0 sauf `ASTRO_VRAM_GB`.
- **Reconnaissance des visages locale** : `libonnxruntime.dylib` est cherchée dans `ORT_DYLIB_PATH`, puis
  `~/.zen/astroportd/lib`, `/opt/homebrew/lib`, `/usr/local/lib`.

## Installer

```bash
xcode-select --install                    # compilateur C (SQLite, libwebp sont compilés depuis leurs sources)
brew install rustup ffmpeg pkg-config     # ffmpeg : pipeline média ; puis `rustup default stable`
cargo build --release --features onnx     # ou sans --features onnx : Brain HTTP seulement
./target/release/astroportd init          # rituel Y-Level, choix ORIGIN / ẐEN
```

`./build-all.sh --targets aarch64-apple-darwin` produit l'archive `dist/astroportd-<version>-aarch64-apple-darwin.tar.gz`.

Pour l'inférence faciale locale : `brew install onnxruntime` puis, si besoin,
`export ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib`, et `astroportd face-check`.

## Partager Ollama et ComfyUI

```bash
brew install ollama && ollama serve &          # Ollama sur Metal, port 11434
# ComfyUI : installation PyTorch/MPS, port 8188
./target/release/astroportd serve --db ~/.zen/astroportd/uplanet.db --blobs ~/.zen/astroportd/blobs \
    --p2p-listen /ip4/0.0.0.0/tcp/4101
```

Ollama (`127.0.0.1:11434`), ComfyUI (`127.0.0.1:8188`) et SSH (`127.0.0.1:22`) sont offerts au maillage **par défaut**
mais **fermés** : le Capitaine choisit qui les ouvre dans *Options › Tunnels* (voir le GUIDE, étape tunnels). Sur macOS,
activez *Réglages Système › Partage › Session à distance* pour que le SSH existe.

## Démarrer au login (launchd)

`~/Library/LaunchAgents/org.uplanet.astroportd.plist` (remplacez `VOTRE_USER`) :

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

`launchctl load ~/Library/LaunchAgents/org.uplanet.astroportd.plist`. Pour que le Mac ne s'endorme pas :
`sudo pmset -a sleep 0` (ou *Réglages Système › Batterie / Économie d'énergie*). `astroportd init --systemd` n'écrit
que l'unité Linux.

## Limites connues

- Aucune compilation macOS éprouvée à ce jour (voir l'état de la CI).
- `init --systemd` ne s'applique pas ; pas d'unité `launchd` générée.
- Pas de Brain GPU « automatique » : le Power-Score est une estimation, pas une mesure de Metal.
