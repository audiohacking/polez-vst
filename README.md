# polez-vst

[![CI](https://github.com/audiohacking/polez-vst/actions/workflows/ci.yml/badge.svg)](https://github.com/audiohacking/polez-vst/actions/workflows/ci.yml)

Real-time VST3/CLAP effect wrapping [polez](https://github.com/szichedelic/polez) — audio forensics and sanitization (detect watermarks, clean, or bypass).

Built with [truce](https://github.com/truce-audio/truce). Polez is vendored as a **git submodule** at `third_party/polez` (no sibling-repo layout required).

## Clone & build

```bash
git clone --recurse-submodules https://github.com/<you>/polez-vst.git
cd polez-vst
cargo truce install --clap --vst3 --au2   # macOS: includes AU v2 (.component)
```

If you already cloned without submodules, `cargo build` runs `git submodule update --init` automatically via `build.rs`. You can also run:

```bash
git submodule update --init --recursive
```

## Tests

```bash
# Plugin + realtime parity tests (aligned with polez pipeline tests)
cargo test -p polez-vst

# Upstream polez library tests (no CLI/GUI deps)
cargo test -p polez --no-default-features --lib

# Everything in the workspace
cargo test --workspace
```

## CI & releases

- **CI** (push/PR to `main`): tests on macOS + Linux, then `cargo truce build --clap --vst3`.
- **Release** (publish a GitHub Release, or run the workflow manually): builds plugins for **macOS arm64** (AU v2 + CLAP + VST3) and **Linux x64** (CLAP + VST3), and attaches:
  - `polez-plugins-macos-arm64.zip` — manual install (copy bundles)
  - `polez-plugins-macos-installer.pkg` — system installer (AU + CLAP + VST3 → `/Library/Audio/Plug-Ins/…`)
  - `polez-plugins-linux-x64.tar.gz`

Build the macOS installer locally after `cargo truce build --clap --vst3 --au2`:

```bash
./scripts/build-installer-pkg.sh --version 0.1.0
```

Install [cargo-truce](https://github.com/truce-audio/truce) locally (`cargo install cargo-truce --version 0.42.1 --locked`) to build bundles yourself.

## Modes

| Mode | Behavior |
|------|----------|
| **Bypass** | Passthrough (no DSP) |
| **Detect** | Passthrough + watermark confidence meter (sliding analysis window) |
| **Clean** | Overlap-add streaming sanitization, ~8192 samples latency @ any rate |

**Strength** maps to polez pipeline modes (Fast / Standard / Preserving / Aggressive). **Paranoid** enables extra spectral passes.

## Layout

| Path | Purpose |
|------|---------|
| `third_party/polez/` | Git submodule — polez engine (pinned to v2.1.2) |
| `src/lib.rs` | Truce plugin |
| `src/rt/processor.rs` | Real-time overlap-add wrapper |
| `tests/rt_parity.rs` | Integration tests vs polez expectations |
