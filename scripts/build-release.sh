#!/usr/bin/env bash
# Builds a self-contained LiveStage release folder in dist/ (macOS, Linux).
#
#   scripts/build-release.sh                    # FFmpeg required, bundled into dist/
#   scripts/build-release.sh --allow-no-ffmpeg  # build without movie playback if FFmpeg is missing
#   FFMPEG_PREFIX=$HOME/.local/ffmpeg-9 scripts/build-release.sh
#
# FFmpeg is found with pkg-config (PKG_CONFIG_PATH, or FFMPEG_PREFIX/lib/pkgconfig).
# Its shared libraries and their non-system dependencies are copied into
# dist/<name>/lib and the binary is pointed at them (@executable_path/../lib on
# macOS, $ORIGIN/../lib on Linux), so the folder runs on machines without
# FFmpeg installed. The staged binary must pass `livestage --check-media` in a
# clean environment or the script fails.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

allow_no_ffmpeg=0
for arg in "$@"; do
    case "$arg" in
        --allow-no-ffmpeg) allow_no_ffmpeg=1 ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [[ -n "${FFMPEG_PREFIX:-}" ]]; then
    export PKG_CONFIG_PATH="$FFMPEG_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi

os="$(uname -s)"
arch="$(uname -m)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

features=()
with_ffmpeg=0
if pkg-config --exists libavcodec libavformat libavutil libswscale 2>/dev/null; then
    with_ffmpeg=1
    features=(--features ffmpeg)
    echo "FFmpeg: libavcodec $(pkg-config --modversion libavcodec) in $(pkg-config --variable=libdir libavcodec)"
elif [[ $allow_no_ffmpeg == 1 ]]; then
    echo "warning: FFmpeg not found; building WITHOUT movie playback (--allow-no-ffmpeg)" >&2
else
    cat >&2 <<'EOF'
error: FFmpeg 9.x development libraries not found by pkg-config.
  Install FFmpeg 9.x (a Homebrew or apt package works if it ships 9.x; otherwise build it
  into a prefix such as ~/.local/ffmpeg-9) and set FFMPEG_PREFIX or PKG_CONFIG_PATH. To ship a build without movie playback anyway, pass
  --allow-no-ffmpeg.
EOF
    exit 1
fi

cargo build --release -p livestage_ui "${features[@]}"

suffix=""
[[ $with_ffmpeg == 0 ]] && suffix="-nomovies"
name="livestage-$version-$(echo "$os" | tr '[:upper:]' '[:lower:]')-$arch$suffix"
dist="$ROOT/dist/$name"
rm -rf "$dist"
mkdir -p "$dist/bin" "$dist/lib"
cp target/release/livestage "$dist/bin/"
cp README.md "$dist/"

# --- bundle shared libraries ------------------------------------------------

is_system_lib() {
    case "$1" in
        /usr/lib/*|/System/*|/lib/*|/lib64/*|/usr/lib64/*) return 0 ;;
        *linux-vdso*|*ld-linux*) return 0 ;;
    esac
    return 1
}

bundle_macos() {
    local queue=("$dist/bin/livestage") seen=" " file dep base real dir
    while ((${#queue[@]})); do
        file="${queue[0]}"; queue=("${queue[@]:1}")
        dir="$(dirname "$file")"
        # Skip the first line (file name) and, for dylibs, their own id.
        while read -r dep; do
            is_system_lib "$dep" && continue
            case "$dep" in
                @rpath/*|@loader_path/*|@executable_path/*)
                    real="$(pkg-config --variable=libdir libavcodec 2>/dev/null)/${dep##*/}"
                    [[ -f "$real" ]] || real="$dir/${dep##*/}"
                    ;;
                *) real="$dep" ;;
            esac
            base="${dep##*/}"
            [[ "$base" == "$(basename "$file")" ]] && continue
            if [[ "$seen" != *" $base "* ]]; then
                [[ -f "$real" ]] || { echo "error: cannot find $dep (needed by $file)" >&2; exit 1; }
                cp -L "$real" "$dist/lib/$base"
                chmod u+w "$dist/lib/$base"
                install_name_tool -id "@rpath/$base" "$dist/lib/$base" 2>/dev/null
                seen+="$base "
                queue+=("$dist/lib/$base")
            fi
            install_name_tool -change "$dep" "@rpath/$base" "$file" 2>/dev/null
        done < <(otool -L "$file" | tail -n +2 | awk '{print $1}')
    done
    install_name_tool -add_rpath "@executable_path/../lib" "$dist/bin/livestage" 2>/dev/null || true
    for lib in "$dist"/lib/*.dylib; do
        install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null || true
    done
    # Rewritten load commands invalidate signatures; re-sign ad hoc
    # (replace with a Developer ID identity for distribution).
    codesign --force --sign "${CODESIGN_IDENTITY:--}" "$dist"/lib/*.dylib "$dist/bin/livestage" 2>/dev/null
}

bundle_linux() {
    local libdir dep
    libdir="$(pkg-config --variable=libdir libavcodec)"
    # Copy every shared object that resolves outside the system directories.
    while read -r dep; do
        [[ -n "$dep" && -f "$dep" ]] || continue
        is_system_lib "$dep" && continue
        cp -L "$dep" "$dist/lib/"
    done < <(LD_LIBRARY_PATH="$libdir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ldd "$dist/bin/livestage" | awk '/=>/ {print $3}')
    if command -v patchelf >/dev/null; then
        patchelf --set-rpath '$ORIGIN/../lib' "$dist/bin/livestage"
        for lib in "$dist"/lib/*.so*; do patchelf --set-rpath '$ORIGIN' "$lib"; done
    else
        echo "warning: patchelf not found; run with LD_LIBRARY_PATH=$dist/lib" >&2
    fi
}

if [[ $with_ffmpeg == 1 ]]; then
    case "$os" in
        Darwin) bundle_macos ;;
        Linux) bundle_linux ;;
        *) echo "warning: library bundling not implemented for $os; see README" >&2 ;;
    esac
fi
rmdir "$dist/lib" 2>/dev/null || true

# --- verify -----------------------------------------------------------------

echo "Bundled libraries:"
ls "$dist/lib" 2>/dev/null | sed 's/^/  /' || echo "  (none)"
echo "Checking movie playback in a clean environment:"
set +e
env -i HOME="$HOME" PATH=/usr/bin:/bin "$dist/bin/livestage" --check-media
status=$?
set -e
if [[ $with_ffmpeg == 1 && $status != 0 ]]; then
    echo "error: the staged build failed --check-media" >&2
    exit 1
fi

tar -C "$ROOT/dist" -czf "$ROOT/dist/$name.tar.gz" "$name"
echo "Release: dist/$name.tar.gz"
