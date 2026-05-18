#!/usr/bin/env bash
# Bump crates.io deps, Truce git tag, and the polez submodule to their latest versions.
# Intended for CI (ubuntu) and local use before opening a dependency PR.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TRUCE_REPO="truce-audio/truce"
TRUCE_TAG_FILE=".github/versions/truce-tag"
POLEZ_SUBMODULE="third_party/polez"

log() { printf '==> %s\n' "$*"; }

latest_truce_tag() {
  local tag=""

  if command -v gh >/dev/null 2>&1; then
    tag="$(gh api "repos/${TRUCE_REPO}/releases/latest" --jq .tag_name 2>/dev/null || true)"
    if [ -n "$tag" ] && [ "$tag" != "null" ]; then
      echo "$tag"
      return 0
    fi
    tag="$(gh api "repos/${TRUCE_REPO}/tags" --paginate --jq '.[].name' 2>/dev/null \
      | grep -E '^v[0-9]' | sort -V | tail -1 || true)"
    if [ -n "$tag" ]; then
      echo "$tag"
      return 0
    fi
  fi

  # Fallback without gh (curl + jq).
  tag="$(curl -fsSL "https://api.github.com/repos/${TRUCE_REPO}/releases/latest" \
    | jq -r .tag_name 2>/dev/null || true)"
  if [ -n "$tag" ] && [ "$tag" != "null" ]; then
    echo "$tag"
    return 0
  fi

  curl -fsSL "https://api.github.com/repos/${TRUCE_REPO}/tags?per_page=100" \
    | jq -r '.[].name' \
    | grep -E '^v[0-9]' \
    | sort -V \
    | tail -1
}

sync_truce_tag_in_cargo_toml() {
  local tag="$1"
  local tmp
  tmp="$(mktemp)"
  while IFS= read -r line || [ -n "$line" ]; do
    if [[ "$line" == *"truce-audio/truce"* && "$line" == *'tag = '* ]]; then
      line="$(printf '%s' "$line" | sed -E 's/tag = "v[^"]+"/tag = "'"$tag"'"/')"
    fi
    printf '%s\n' "$line"
  done < Cargo.toml >"$tmp"
  mv "$tmp" Cargo.toml
}

write_truce_tag_file() {
  local tag="$1"
  mkdir -p "$(dirname "$TRUCE_TAG_FILE")"
  printf '%s\n' "$tag" >"$TRUCE_TAG_FILE"
}

update_crates_io_deps() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found" >&2
    exit 1
  fi

  log "Installing cargo-edit (cargo upgrade) if needed"
  if ! cargo upgrade --help >/dev/null 2>&1; then
    cargo install cargo-edit --locked
  fi

  log "Upgrading Cargo.toml version requirements to latest"
  cargo upgrade --workspace

  log "Refreshing Cargo.lock"
  cargo update --workspace
}

update_polez_submodule() {
  if [ ! -d "$POLEZ_SUBMODULE/.git" ] && [ ! -f ".git/modules/${POLEZ_SUBMODULE}/HEAD" ]; then
    log "Skipping polez submodule (not initialized)"
    return 0
  fi

  log "Updating polez submodule to upstream default branch"
  git submodule update --init --remote "$POLEZ_SUBMODULE"
  git add "$POLEZ_SUBMODULE"
}

main() {
  local current latest
  current="$(tr -d '[:space:]' <"$TRUCE_TAG_FILE")"
  latest="$(latest_truce_tag)"

  if [ -z "$latest" ]; then
    echo "Failed to resolve latest Truce tag for ${TRUCE_REPO}" >&2
    exit 1
  fi

  log "Truce tag: ${current} -> ${latest}"
  write_truce_tag_file "$latest"
  sync_truce_tag_in_cargo_toml "$latest"

  update_crates_io_deps
  update_polez_submodule

  log "Done. Review changes and run: cargo fmt --all && cargo test -p polez-vst"
}

main "$@"
