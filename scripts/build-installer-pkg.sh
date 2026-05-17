#!/usr/bin/env bash
# Build a macOS .pkg installer for polez-vst (CLAP + VST3) from the current build tree.
# Run from repo root after: cargo truce build --clap --vst3
#
# Usage:
#   ./scripts/build-installer-pkg.sh [--sign-plugins] [--version 0.1.0]
#
# Output: release-artefacts/Polez-macOS-Installer.pkg
# Install location:
#   /Library/Audio/Plug-Ins/CLAP/Polez.clap
#   /Library/Audio/Plug-Ins/VST3/Polez.vst3

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PLUGIN_CLAP_NAME="Polez.clap"
PLUGIN_VST3_NAME="Polez.vst3"
PKG_IDENTIFIER="com.audiohacking.polez-vst"

SIGN_PLUGINS=false
PKG_VERSION="0.1.0"

while [ $# -gt 0 ]; do
  case "$1" in
    --sign-plugins) SIGN_PLUGINS=true; shift ;;
    --version)
      PKG_VERSION="${2#v}"
      shift 2
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

CLAP_PATH="${REPO_ROOT}/target/bundles/${PLUGIN_CLAP_NAME}"
VST3_PATH="${REPO_ROOT}/target/bundles/${PLUGIN_VST3_NAME}"

if [ ! -d "$CLAP_PATH" ] || [ ! -d "$VST3_PATH" ]; then
  echo "Error: Plugin bundles not found. Build first:" >&2
  echo "  cargo truce build --clap --vst3" >&2
  find "${REPO_ROOT}/target" -type d \( -name "*.clap" -o -name "*.vst3" \) 2>/dev/null || true
  exit 1
fi

echo "Using CLAP: ${CLAP_PATH}"
echo "Using VST3: ${VST3_PATH}"

mkdir -p release-artefacts
rm -rf "release-artefacts/${PLUGIN_CLAP_NAME}" "release-artefacts/${PLUGIN_VST3_NAME}"
cp -R "$CLAP_PATH" "release-artefacts/${PLUGIN_CLAP_NAME}"
cp -R "$VST3_PATH" "release-artefacts/${PLUGIN_VST3_NAME}"

if [ "$SIGN_PLUGINS" = true ]; then
  echo "Ad-hoc signing plugin bundles..."
  xcrun codesign --force --sign - --deep "release-artefacts/${PLUGIN_CLAP_NAME}"
  xcrun codesign --force --sign - --deep "release-artefacts/${PLUGIN_VST3_NAME}"
fi

rm -rf payload
mkdir -p payload/Library/Audio/Plug-Ins/CLAP
mkdir -p payload/Library/Audio/Plug-Ins/VST3
cp -R "release-artefacts/${PLUGIN_CLAP_NAME}" "payload/Library/Audio/Plug-Ins/CLAP/"
cp -R "release-artefacts/${PLUGIN_VST3_NAME}" "payload/Library/Audio/Plug-Ins/VST3/"

pkgbuild \
  --root payload \
  --identifier "$PKG_IDENTIFIER" \
  --version "$PKG_VERSION" \
  --install-location / \
  "release-artefacts/Polez-macOS-Installer.pkg"

rm -rf payload

echo "Created release-artefacts/Polez-macOS-Installer.pkg (version ${PKG_VERSION})"
echo "Install: sudo installer -pkg release-artefacts/Polez-macOS-Installer.pkg -target /"
echo "Or open the .pkg in Finder for a GUI install."
