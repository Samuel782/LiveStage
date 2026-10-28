# ffmpeg-sys-next 9.0.1, vendored for LiveStage

Upstream: <https://github.com/zmwangx/rust-ffmpeg-sys>, crates.io `ffmpeg-sys-next`
9.0.1 (WTFPL). It replaces the crates.io package through `[patch.crates-io]`
in the workspace `Cargo.toml`.

## Why it is vendored

`ffmpeg-sys-next` finds FFmpeg in its own build script (`FFMPEG_DIR`, then
vcpkg, then pkg-config). Nothing LiveStage's crates do can run before that
script or set its environment, so a missing FFmpeg could only be fixed by
hand. With this patch, `cargo build --features ffmpeg` works on a machine
without FFmpeg.

## The change

Two edits; everything else is the published 9.0.1 package unchanged
(`Cargo.toml` is upstream's `Cargo.toml.orig`):

1. `Cargo.toml`: build-dependency on `crates/livestage_ffmpeg_provision`.
2. `build.rs`: upstream `main` is renamed to `upstream_main`, and a new
   `main` first calls `livestage_ffmpeg_provision::build_script::run()`.
   That call returns without changes when FFmpeg is configured, or sets
   `FFMPEG_DIR` to a provided FFmpeg 9. See that crate's docs for details.

## Updating

Copy the new release from `~/.cargo/registry/src/*/ffmpeg-sys-next-<ver>/`
(using `Cargo.toml.orig` as `Cargo.toml`), reapply the two edits, and bump
the version in `crates/livestage_ui/Cargo.toml`. `grep -n livestage
Cargo.toml build.rs` shows both edits.
