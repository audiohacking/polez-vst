#!/usr/bin/env bash
# Verify `cargo truce build` output under target/bundles/.
#
# Truce layout (see cargo-truce stage_clap):
#   macOS CLAP  → Polez.clap/     (bundle directory)
#   Linux CLAP  → Polez.clap      (flat .so renamed .clap)
#   VST3        → Polez.vst3/     (bundle directory, all platforms)
#   AU v2       → Polez.component/ (macOS only)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUNDLES_DIR="${REPO_ROOT}/target/bundles"

cd "$REPO_ROOT"

if [ ! -f "$BUNDLES_DIR/manifest.toml" ]; then
  echo "Error: missing $BUNDLES_DIR/manifest.toml — run cargo truce build first." >&2
  exit 1
fi

# -e: file or directory (macOS bundle dir vs Linux flat binary).
if [ ! -e "$BUNDLES_DIR/Polez.clap" ]; then
  echo "Error: missing CLAP bundle at $BUNDLES_DIR/Polez.clap" >&2
  ls -la "$BUNDLES_DIR" 2>/dev/null || true
  exit 1
fi

if [ ! -d "$BUNDLES_DIR/Polez.vst3" ]; then
  echo "Error: missing VST3 bundle at $BUNDLES_DIR/Polez.vst3" >&2
  ls -la "$BUNDLES_DIR" 2>/dev/null || true
  exit 1
fi

if [ "$(uname -s)" = "Darwin" ]; then
  if [ ! -d "$BUNDLES_DIR/Polez.component" ]; then
    echo "Error: missing AU bundle at $BUNDLES_DIR/Polez.component" >&2
    ls -la "$BUNDLES_DIR" 2>/dev/null || true
    exit 1
  fi
fi

cat "$BUNDLES_DIR/manifest.toml"
