#!/usr/bin/env bash
# Build the feature-selected FFmpeg backend for ANDROID (NDK cross-compile).
#
# Contract (matches .github/workflows/ffmpeg-mobile.yml):
#   scripts/ffmpeg-builder/build-android.sh [--all-abis | <abi>...]
#   env: ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) — NDK r27+ with LLVM toolchain
#        VELMUX_ANDROID_API (default 29, matches the product minSdk, D-003)
#        VELMUX_FFMPEG_JOBS=<parallel jobs> (default: nproc)
#
# ABIs: arm64-v8a, armeabi-v7a, x86_64.
#   armeabi-v7a omits SVT-AV1 (upstream does not support 32-bit); AV1
#   encoding is honestly reported unavailable on that ABI.
#
# Idempotent per ABI: exits 0 quickly when that ABI's manifest is present.
# Output layout:
#   out/android/<abi>/include, out/android/<abi>/lib (static),
#   out/android/<abi>/share/velmux/ffmpeg-capabilities.json
#   out/android/manifest.json (aggregated per-ABI record)
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

# ---------------------------------------------------------------------------
# Arguments and environment
# ---------------------------------------------------------------------------
ALL_ABIS=(arm64-v8a armeabi-v7a x86_64)
ABIS=()
if [ "${1:-}" = "--all-abis" ]; then
  ABIS=("${ALL_ABIS[@]}")
else
  for arg in "$@"; do
    case "$arg" in
      arm64-v8a|armeabi-v7a|x86_64) ABIS+=("$arg") ;;
      *) die "unknown ABI '$arg' (supported: ${ALL_ABIS[*]})" ;;
    esac
  done
fi
[ ${#ABIS[@]} -gt 0 ] || ABIS=(arm64-v8a)

API_LEVEL="${VELMUX_ANDROID_API:-29}"

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
[ -n "$NDK" ] || die "ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) not set"
[ -d "$NDK" ] || die "NDK directory not found: $NDK"

TOOLCHAIN=""
for candidate in "$NDK"/toolchains/llvm/prebuilt/*; do
  if [ -d "$candidate" ]; then
    TOOLCHAIN="$candidate"
    break
  fi
done
[ -n "$TOOLCHAIN" ] \
  || die "LLVM toolchain not found under $NDK/toolchains/llvm/prebuilt"
SYSROOT="$TOOLCHAIN/sysroot"

require_tool curl
require_tool make
require_tool python3
require_tool pkg-config
ensure_cmake_ninja
ensure_meson

FFMPEG_VERSION="$(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.version")"

log "NDK: $NDK"
log "toolchain: $TOOLCHAIN"
log "API level: $API_LEVEL"
log "ABIs: ${ABIS[*]}"

# ---------------------------------------------------------------------------
# Per-ABI settings
# ---------------------------------------------------------------------------
abi_settings() {
  case "$1" in
    arm64-v8a)
      CC="$TOOLCHAIN/bin/aarch64-linux-android${API_LEVEL}-clang"
      CXX="$TOOLCHAIN/bin/aarch64-linux-android${API_LEVEL}-clang++"
      FF_ARCH="aarch64"
      FF_CPU=""
      AUTOCONF_HOST="aarch64-linux-android"
      VPX_TARGET="arm64-android-gcc"
      MESON_CPU_FAMILY="aarch64"
      MESON_CPU="aarch64"
      ABI_CFLAGS=""
      WANT_SVT=1
      ;;
    armeabi-v7a)
      CC="$TOOLCHAIN/bin/armv7a-linux-androideabi${API_LEVEL}-clang"
      CXX="$TOOLCHAIN/bin/armv7a-linux-androideabi${API_LEVEL}-clang++"
      FF_ARCH="arm"
      FF_CPU="armv7-a"
      AUTOCONF_HOST="arm-linux-androideabi"
      VPX_TARGET="armv7-android-gcc"
      MESON_CPU_FAMILY="arm"
      MESON_CPU="armv7"
      ABI_CFLAGS="-march=armv7-a -mfpu=neon -mfloat-abi=softfp"
      # SVT-AV1 does not support 32-bit targets.
      WANT_SVT=0
      ;;
    x86_64)
      CC="$TOOLCHAIN/bin/x86_64-linux-android${API_LEVEL}-clang"
      CXX="$TOOLCHAIN/bin/x86_64-linux-android${API_LEVEL}-clang++"
      FF_ARCH="x86_64"
      FF_CPU=""
      AUTOCONF_HOST="x86_64-linux-android"
      VPX_TARGET="x86_64-android-gcc"
      MESON_CPU_FAMILY="x86_64"
      MESON_CPU="x86_64"
      ABI_CFLAGS=""
      WANT_SVT=1
      ;;
    *)
      die "unsupported ABI: $1"
      ;;
  esac
  [ -x "$CC" ] || die "compiler wrapper not found: $CC"
  AR="$TOOLCHAIN/bin/llvm-ar"
  RANLIB="$TOOLCHAIN/bin/llvm-ranlib"
  STRIP="$TOOLCHAIN/bin/llvm-strip"
  NM="$TOOLCHAIN/bin/llvm-nm"
  for tool in "$AR" "$RANLIB" "$STRIP" "$NM"; do
    [ -x "$tool" ] || die "tool not found: $tool"
  done
}

# Meson cross file for one ABI. Written to a temp location, printed on stdout.
# Only the host machine is described; meson derives the build (native) machine
# from the environment (CC/CXX are unset for the meson invocations).
write_meson_cross() {
  cat <<EOF
[binaries]
c = '$CC'
cpp = '$CXX'
ar = '$AR'
ranlib = '$RANLIB'
strip = '$STRIP'
nm = '$NM'
pkg-config = 'pkg-config'

[host_machine]
system = 'android'
cpu_family = '$MESON_CPU_FAMILY'
cpu = '$MESON_CPU'
endian = 'little'

[properties]
needs_exe_wrapper = true
sys_root = '$SYSROOT'
EOF
}

build_one_abi() {
  local abi="$1"
  abi_settings "$abi"

  local prefix="$OUT_DIR/android/$abi"
  local manifest="$prefix/share/velmux/ffmpeg-capabilities.json"
  local identity
  identity="$(compute_identity android "abi=$abi" "api=$API_LEVEL" "svt=$WANT_SVT")"
  if [ -f "$prefix/lib/pkgconfig/libavcodec.pc" ] && [ -f "$manifest" ] \
     && identity_matches "$prefix" "$identity"; then
    log "[$abi] build up to date (identity verified)"
    fixup_pkgconfig_prefix "$prefix"
    echo "$abi"
    return 0
  fi
  if [ -d "$prefix" ] && ! identity_matches "$prefix" "$identity"; then
    log "[$abi] recorded build identity does not match the current tree; rebuilding"
    rm -rf "$prefix"
  fi

  local srctag="android-$abi"
  local build_tmp="$SRC_DIR/build-$srctag"
  mkdir -p "$prefix" "$build_tmp"

  log "[$abi] building toolchain deps (CC=$CC)"

  # Exported for autoconf-based dependencies (configure picks up CC/AR/...).
  # CFLAGS is deliberately NOT exported: cmake/meson must not inherit the
  # autoconf flags, and the NDK wrappers already target the right arch.
  export CC CXX AR RANLIB STRIP NM

  # Hermetic pkg-config resolution for the whole ABI build: every cmake/meson
  # find-module that consults pkg-config (notably CMake's FindPNG inside
  # freetype) must only ever see this prefix. Without PKG_CONFIG_LIBDIR, a
  # dev-package installed on the build host (e.g. libpng) would leak into
  # generated .pc files as an unresolvable requirement for the cross build.
  export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
  export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"

  # --- zlib (cmake; no system zlib on android) -----------------------------
  if [ ! -f "$prefix/lib/libz.a" ]; then
    log "[$abi] building zlib"
    zlib_tb="$(fetch_tarball zlib)"
    zlib_src="$(extract_tarball "$zlib_tb" "$srctag")"
    zlib_build="$build_tmp/zlib"
    rm -rf "$zlib_build"
    cmake -G Ninja -S "$zlib_src" -B "$zlib_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" >/dev/null
    cmake --build "$zlib_build" >/dev/null
    cmake --install "$zlib_build" >/dev/null
    # zlib's CMake build installs no pkg-config file (only autotools does);
    # freetype2.pc's "Requires: zlib" needs one, so write it explicitly with
    # the pinned version.
    mkdir -p "$prefix/lib/pkgconfig"
    cat > "$prefix/lib/pkgconfig/zlib.pc" <<ZPC
prefix=$prefix
exec_prefix=\${prefix}
libdir=\${prefix}/lib
includedir=\${prefix}/include

Name: zlib
Description: zlib compression library
Version: 1.3.1

Requires:
Libs: -L\${libdir} -lz
Cflags: -I\${includedir}
ZPC
    log "[$abi] zlib installed"
  fi

  # --- x264 (autoconf, in-tree per ABI) ------------------------------------
  if [ ! -f "$prefix/lib/libx264.a" ]; then
    log "[$abi] building x264"
    x264_tb="$(fetch_tarball x264)"
    x264_src="$(extract_tarball "$x264_tb" "$srctag")"
    (cd "$x264_src" && ./configure \
       --prefix="$prefix" --enable-static --enable-pic --disable-cli \
       --host="$AUTOCONF_HOST" \
       --extra-cflags="$ABI_CFLAGS -O3" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] x264 installed"
  fi

  # --- x265 (cmake cross; HEVC software encode for the FFmpeg backend) ------
  if [ ! -f "$prefix/lib/libx265.a" ]; then
    log "[$abi] building x265"
    x265_tb="$(fetch_tarball x265)"
    x265_src="$(extract_tarball "$x265_tb" "$srctag")"
    patch_x265_cmake4_compat "$x265_src"
    x265_build="$build_tmp/x265"
    rm -rf "$x265_build"
    # ENABLE_ASSEMBLY=OFF: x265's asm path expects nasm on x86 and is not
    # wired for NDK cross targets; C++ intrinsics carry the real speed.
    cmake -G Ninja -S "$x265_src/source" -B "$x265_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" \
      -DENABLE_SHARED=OFF -DENABLE_CLI=OFF -DENABLE_ASSEMBLY=OFF \
      >/dev/null
    cmake --build "$x265_build" >/dev/null
    cmake --install "$x265_build" >/dev/null
    sanitize_x265_pc "$prefix"
    log "[$abi] x265 installed"
  fi

  # --- lame (autoconf, in-tree per ABI) --------------------------------------
  if [ ! -f "$prefix/lib/libmp3lame.a" ]; then
    log "[$abi] building lame"
    lame_tb="$(fetch_tarball lame)"
    lame_src="$(extract_tarball "$lame_tb" "$srctag")"
    (cd "$lame_src" && ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic \
       --disable-dependency-tracking --disable-gtktest --disable-nasm \
       --disable-frontend \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] lame installed"
  fi

  # --- opus (autoconf, in-tree per ABI) --------------------------------------
  if [ ! -f "$prefix/lib/libopus.a" ]; then
    log "[$abi] building opus"
    opus_tb="$(fetch_tarball opus)"
    opus_src="$(extract_tarball "$opus_tb" "$srctag")"
    (cd "$opus_src" && ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic \
       --disable-doc --disable-extra-programs \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] opus installed"
  fi

  # --- libogg + libvorbis (autoconf, in-tree per ABI) ------------------------
  if [ ! -f "$prefix/lib/libogg.a" ]; then
    log "[$abi] building libogg"
    ogg_tb="$(fetch_tarball libogg)"
    ogg_src="$(extract_tarball "$ogg_tb" "$srctag")"
    (cd "$ogg_src" && ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] libogg installed"
  fi
  if [ ! -f "$prefix/lib/libvorbis.a" ]; then
    log "[$abi] building libvorbis"
    vorbis_tb="$(fetch_tarball libvorbis)"
    vorbis_src="$(extract_tarball "$vorbis_tb" "$srctag")"
    (cd "$vorbis_src" && PKG_CONFIG_PATH="$prefix/lib/pkgconfig" ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic --disable-docs \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] libvorbis installed"
  fi

  # --- libvpx (custom configure, in-tree per ABI) -----------------------------
  if [ ! -f "$prefix/lib/libvpx.a" ]; then
    log "[$abi] building libvpx"
    vpx_tb="$(fetch_tarball libvpx)"
    vpx_src="$(extract_tarball "$vpx_tb" "$srctag")"
    # vpx's die() prints to stdout (plain echo), so failures are invisible
    # under >/dev/null; capture to a log and dump it on failure. AS must be
    # the clang wrapper (NDK has no standalone `as`; clang assembles with -c,
    # which vpx's android branch adds itself for *clang AS).
    #
    # vpx's android branch also hardcodes `-static` into LDFLAGS (an old-NDK
    # assumption); modern NDK clang link tests fail with it. Disable the
    # line for our build (libraries are built static regardless).
    sed -i.bak 's/^          check_add_ldflags -static$/          : velmux: no -static with modern NDK/' \
      "$vpx_src/build/make/configure.sh"
    local vpx_log="$build_tmp/vpx-configure-$abi.log"
    local vpx_as="$CC"
    if [ "$abi" = "x86_64" ]; then
      # x86 asm is nasm syntax; the runner has nasm via apt.
      command -v nasm >/dev/null 2>&1 && vpx_as="nasm"
    fi
    if ! (cd "$vpx_src" && AS="$vpx_as" LD="$CC" ./configure \
       --target="$VPX_TARGET" \
       --prefix="$prefix" \
       --disable-shared --enable-static --enable-pic \
       --disable-unit-tests --disable-examples --disable-tools --disable-docs \
       --disable-install-docs --disable-webm-io \
       --extra-cflags="$ABI_CFLAGS -O3" \
       >"$vpx_log" 2>&1); then
      cat "$vpx_log" >&2
      # vpx records the exact failing compiler invocation in config.log
      # (inside the source tree, since configure runs in-tree).
      if [ -f "$vpx_src/config.log" ]; then
        echo "--- vpx config.log (tail) ---" >&2
        tail -n 80 "$vpx_src/config.log" >&2
      fi
      die "[$abi] libvpx configure failed (full log above)"
    fi
    (cd "$vpx_src" && make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] libvpx installed"
  fi

  # --- dav1d (meson cross) -----------------------------------------------------
  if [ ! -f "$prefix/lib/libdav1d.a" ]; then
    log "[$abi] building dav1d"
    dav1d_tb="$(fetch_tarball dav1d)"
    dav1d_src="$(extract_tarball "$dav1d_tb")" # cmake-style, out of tree
    dav1d_build="$build_tmp/dav1d"
    rm -rf "$dav1d_build"
    local cross_file="$build_tmp/dav1d-cross-$abi.meson"
    write_meson_cross > "$cross_file"
    # dav1d's x86 asm needs nasm; without it, build honestly without asm
    # (performance-only difference, capabilities unchanged).
    local dav1d_asm_args=()
    if [ "$abi" = "x86_64" ] && ! command -v nasm >/dev/null 2>&1; then
      log "[$abi] nasm not found: dav1d built without x86 asm"
      dav1d_asm_args=(-Denable_asm=false)
    fi
    # meson: env CC/CXX describe the NATIVE compiler; the cross file owns
    # the target, so the exports must not leak into setup. All meson/ninja
    # output is captured and dumped on failure — meson writes some failure
    # modes to stdout, which >/dev/null would hide.
    local dav1d_log="$build_tmp/dav1d-$abi.log"
    if ! env -u CC -u CXX meson setup "$dav1d_build" "$dav1d_src" \
      --prefix="$prefix" --libdir=lib \
      --default-library=static --buildtype=release -Db_staticpic=true \
      --cross-file "$cross_file" \
      -Denable_tools=false -Denable_tests=false "${dav1d_asm_args[@]}" \
      >"$dav1d_log" 2>&1; then
      cat "$dav1d_log" >&2
      die "[$abi] dav1d meson setup failed (full log above)"
    fi
    if ! ninja -C "$dav1d_build" >"$dav1d_log" 2>&1; then
      tail -n 60 "$dav1d_log" >&2
      die "[$abi] dav1d ninja build failed (tail above)"
    fi
    meson install -C "$dav1d_build" >"$dav1d_log" 2>&1
    log "[$abi] dav1d installed"
  fi

  # --- SVT-AV1 (cmake cross; 64-bit only) --------------------------------------
  if [ "$WANT_SVT" = "1" ]; then
    if [ ! -f "$prefix/lib/libSvtAv1Enc.a" ]; then
      log "[$abi] building SVT-AV1"
      svt_tb="$(fetch_tarball svt-av1)"
      svt_src="$(extract_tarball "$svt_tb")"
      svt_build="$build_tmp/svt-av1"
      rm -rf "$svt_build"
      cmake -G Ninja -S "$svt_src" -B "$svt_build" \
        -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
        -DCMAKE_FIND_ROOT_PATH="$prefix" \
        -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DBUILD_APPS=OFF \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 >/dev/null
      cmake --build "$svt_build" >/dev/null
      cmake --install "$svt_build" >/dev/null
      log "[$abi] SVT-AV1 installed"
    fi
  else
    log "[$abi] SVT-AV1 skipped (not supported on 32-bit)"
  fi

  # --- freetype (cmake cross) ----------------------------------------------------
  if [ ! -f "$prefix/lib/libfreetype.a" ]; then
    log "[$abi] building freetype"
    ft_tb="$(fetch_tarball freetype)"
    ft_src="$(extract_tarball "$ft_tb")"
    ft_build="$build_tmp/freetype"
    rm -rf "$ft_build"
    cmake -G Ninja -S "$ft_src" -B "$ft_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" \
      -DBUILD_SHARED_LIBS=OFF -DFT_DISABLE_ZLIB=FALSE \
      -DFT_REQUIRE_ZLIB=TRUE -DFT_DISABLE_BZIP2=TRUE -DFT_DISABLE_BROTLI=TRUE \
      -DFT_DISABLE_HARFBUZZ=TRUE -DFT_DISABLE_PNG=TRUE >/dev/null
    cmake --build "$ft_build" >/dev/null
    cmake --install "$ft_build" >/dev/null
    log "[$abi] freetype installed"
  fi

  # --- libpng (cmake cross) -------------------------------------------------------
  if [ ! -f "$prefix/lib/libpng16.a" ]; then
    log "[$abi] building libpng"
    png_tb="$(fetch_tarball libpng)"
    png_src="$(extract_tarball "$png_tb")"
    png_build="$build_tmp/libpng"
    rm -rf "$png_build"
    cmake -G Ninja -S "$png_src" -B "$png_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" \
      -DBUILD_SHARED_LIBS=OFF -DPNG_TESTS=OFF -DPNG_TOOLS=OFF \
      -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_FRAMEWORK=OFF \
      -DPNG_EXECUTABLES=OFF >/dev/null
    cmake --build "$png_build" >/dev/null
    cmake --install "$png_build" >/dev/null
    log "[$abi] libpng installed"
  fi

  # --- harfbuzz (cmake cross, with freetype) ----------------------------------------
  if [ ! -f "$prefix/lib/libharfbuzz.a" ]; then
    log "[$abi] building harfbuzz"
    hb_tb="$(fetch_tarball harfbuzz)"
    hb_src="$(extract_tarball "$hb_tb")"
    hb_build="$build_tmp/harfbuzz"
    rm -rf "$hb_build"
    cmake -G Ninja -S "$hb_src" -B "$hb_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" \
      -DBUILD_SHARED_LIBS=OFF -DHB_BUILD_UTILS=OFF -DHB_BUILD_TESTS=OFF \
      -DHB_HAVE_GLIB=OFF -DHB_HAVE_FREETYPE=ON >/dev/null
    cmake --build "$hb_build" >/dev/null
    cmake --install "$hb_build" >/dev/null
    log "[$abi] harfbuzz installed"
  fi

  # --- fribidi (meson cross) ----------------------------------------------------------
  if [ ! -f "$prefix/lib/libfribidi.a" ]; then
    log "[$abi] building fribidi"
    fribidi_tb="$(fetch_tarball fribidi)"
    fribidi_src="$(extract_tarball "$fribidi_tb")"
    fribidi_build="$build_tmp/fribidi"
    rm -rf "$fribidi_build"
    local fribidi_cross="$build_tmp/fribidi-cross-$abi.meson"
    write_meson_cross > "$fribidi_cross"
    local fribidi_log="$build_tmp/fribidi-$abi.log"
    if ! env -u CC -u CXX meson setup "$fribidi_build" "$fribidi_src" \
      --prefix="$prefix" --libdir=lib \
      --default-library=static --buildtype=release -Db_staticpic=true \
      --cross-file "$fribidi_cross" \
      -Ddocs=false -Dtests=false >"$fribidi_log" 2>&1; then
      cat "$fribidi_log" >&2
      die "[$abi] fribidi meson setup failed (full log above)"
    fi
    if ! ninja -C "$fribidi_build" >"$fribidi_log" 2>&1; then
      tail -n 60 "$fribidi_log" >&2
      die "[$abi] fribidi ninja build failed (tail above)"
    fi
    meson install -C "$fribidi_build" >"$fribidi_log" 2>&1
    log "[$abi] fribidi installed"
  fi

  # --- libass (autoconf, in-tree per ABI) ------------------------------------------------
  if [ ! -f "$prefix/lib/libass.a" ]; then
    log "[$abi] building libass"
    ass_tb="$(fetch_tarball libass)"
    ass_src="$(extract_tarball "$ass_tb" "$srctag")"
    # libass uses plain autoconf (no --extra-cflags); flags travel via env.
    (cd "$ass_src" && \
     CFLAGS="$ABI_CFLAGS -O3 -fPIC -I$prefix/include" \
     LDFLAGS="-L$prefix/lib" \
     PKG_CONFIG_PATH="$prefix/lib/pkgconfig" \
     PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig" \
     ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic \
       --disable-fontconfig --disable-require-system-font-provider \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] libass installed"
  fi

  # --- zimg (autoconf cross, in-tree per ABI; zscale for HDR tone mapping) ------------
  if [ ! -f "$prefix/lib/libzimg.a" ]; then
    log "[$abi] building zimg"
    ensure_build_libtool "$OUT_DIR/tools"
    zimg_tb="$(fetch_tarball zimg)"
    zimg_src="$(extract_tarball "$zimg_tb" "$srctag")"
    # The archive ships no configure: regenerate once per ABI source tree.
    (cd "$zimg_src" && ./autogen.sh >/dev/null)
    (cd "$zimg_src" && \
     CFLAGS="$ABI_CFLAGS -O3 -fPIC -I$prefix/include" \
     CXXFLAGS="$ABI_CFLAGS -O3 -fPIC -I$prefix/include" \
     LDFLAGS="-L$prefix/lib" \
     ./configure \
       --prefix="$prefix" --disable-shared --enable-static --with-pic \
       --host="$AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$abi] zimg installed"
  fi
  # bionic keeps the math functions in libm; FFmpeg's static probes
  # link the .pc's Libs only, so zimg/libwebp/sharpyuv need -lm listed
  # explicitly (glibc/libSystem hosts are unaffected: harmless no-op).
  append_pc_libs "$prefix" zimg -lm

  # --- libwebp (cmake cross; still WebP output, mandate 29.3) --------------------------
  if [ ! -f "$prefix/lib/libwebp.a" ]; then
    log "[$abi] building libwebp"
    webp_tb="$(fetch_tarball libwebp)"
    webp_src="$(extract_tarball "$webp_tb" "$srctag")"
    webp_build="$build_tmp/webp"
    rm -rf "$webp_build"
    cmake -G Ninja -S "$webp_src" -B "$webp_build" \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$API_LEVEL" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$prefix" \
      -DBUILD_SHARED_LIBS=OFF \
      -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF \
      -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_GIF2WEBP=OFF \
      -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF \
      -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF \
      -DWEBP_BUILD_EXTRAS=OFF >/dev/null
    cmake --build "$webp_build" >/dev/null
    cmake --install "$webp_build" >/dev/null
    log "[$abi] libwebp installed"
  fi
  append_pc_libs "$prefix" libwebp -lm -lsharpyuv
  append_pc_libs "$prefix" sharpyuv -lm

  # --- FFmpeg (out of tree) -----------------------------------------------------------------
  ffmpeg_tb="$(fetch_ffmpeg)"
  ffmpeg_src="$(extract_tarball "$ffmpeg_tb")"
  local ffbuild="$build_tmp/ffmpeg"
  if [ ! -f "$prefix/lib/libavcodec.a" ]; then
    log "[$abi] configuring FFmpeg"
    rm -rf "$ffbuild"
    mkdir -p "$ffbuild"
    local svt_flag=""
    if [ "$WANT_SVT" = "1" ]; then
      svt_flag="--enable-libsvtav1"
    fi
    local configure_log="$build_tmp/ffmpeg-configure-$abi.log"
    # pkg-config must resolve every dependency from the per-ABI prefix.
    if ! (cd "$ffbuild" && PKG_CONFIG_PATH="$prefix/lib/pkgconfig" \
        PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig" \
        "$ffmpeg_src/configure" \
       --prefix="$prefix" \
       --pkg-config-flags=--static \
       --enable-cross-compile \
       --target-os=android \
       --arch="$FF_ARCH" \
       ${FF_CPU:+--cpu="$FF_CPU"} \
       --cc="$CC" --cxx="$CXX" \
       --ar="$AR" --ranlib="$RANLIB" --nm="$NM" --strip="$STRIP" \
       --sysroot="$SYSROOT" \
       --extra-cflags="-I$prefix/include $ABI_CFLAGS -O3 -fvisibility=hidden" \
       --extra-ldflags="-L$prefix/lib" \
       --disable-shared --enable-static --enable-pic \
       --disable-debug --disable-doc \
       --disable-ffplay \
       --disable-network \
       --disable-avdevice \
       --disable-autodetect \
       --disable-x86asm \
       --enable-pthreads \
       --enable-jni --enable-mediacodec \
       --enable-gpl --enable-libx264 --enable-libx265 \
       --enable-libmp3lame \
       --enable-libopus \
       --enable-libvorbis \
       --enable-libvpx \
       --enable-libdav1d \
       $svt_flag \
       --enable-libfreetype \
       --enable-libharfbuzz \
       --enable-libfribidi \
       --enable-libass \
       --enable-libzimg \
       --enable-libwebp \
       --enable-zlib \
       --enable-protocol=file \
       --enable-protocol=pipe \
       >"$configure_log" 2>&1); then
      cat "$configure_log" >&2
      if [ -f "$ffbuild/ffbuild/config.log" ]; then
        echo "--- FFmpeg config.log (tail) ---" >&2
        tail -n 120 "$ffbuild/ffbuild/config.log" >&2
      fi
      die "[$abi] FFmpeg configure failed (full log above)"
    fi
    log "[$abi] building FFmpeg with $JOBS jobs"
    if ! (cd "$ffbuild" && make -j"$JOBS" >"$configure_log" 2>&1); then
      tail -n 80 "$configure_log" >&2
      die "[$abi] FFmpeg build failed (tail above)"
    fi
    (cd "$ffbuild" && make install >/dev/null)
    log "[$abi] FFmpeg installed to $prefix"
  fi

  # --- Capability manifest (from the actual build tree; the probe cannot
  # --- run cross-compiled binaries on the build host) ---------------------------------------
  if [ ! -f "$manifest" ] || [ "$BUILDER_ROOT/manifest-from-config.py" -nt "$manifest" ]; then
    log "[$abi] generating capability manifest"
    python3 "$BUILDER_ROOT/manifest-from-config.py" \
      "$ffbuild" "$ffmpeg_src" "$FFMPEG_VERSION" "$manifest" >&2
  fi

  # --- Validate the real build against the Velmux feature requirements ----------------------
  local exempt=""
  if [ "$WANT_SVT" = "0" ]; then
    exempt="--exempt libsvtav1"
  fi
  log "[$abi] validating build"
  # stdout stays reserved for the ABI name (captured by the caller for the
  # aggregate manifest); validation output belongs to the build log.
  python3 "$BUILDER_ROOT/validate.py" $exempt "$manifest" >&2

  # Record the build identity and make the pkg-config files
  # position-independent (restored artifacts may live at a different path
  # than the machine that built them).
  write_identity "$prefix" "$identity" \
    "android FFmpeg, abi=$abi api=$API_LEVEL, jobs=$JOBS"
  fixup_pkgconfig_prefix "$prefix"

  # Unset per-ABI exports so the next ABI starts clean.
  unset CC CXX AR RANLIB STRIP NM PKG_CONFIG_PATH PKG_CONFIG_LIBDIR

  log "[$abi] build complete: $prefix"
  echo "$abi"
}

# ---------------------------------------------------------------------------
# Build every requested ABI, then aggregate.
# ---------------------------------------------------------------------------
BUILT_ABIS=()
for abi in "${ABIS[@]}"; do
  built="$(build_one_abi "$abi")"
  BUILT_ABIS+=("$built")
done

AGGREGATE="$OUT_DIR/android/manifest.json"
log "aggregating manifests"
python3 - "$OUT_DIR/android" "$FFMPEG_VERSION" "$API_LEVEL" "${BUILT_ABIS[@]}" <<'PY'
import json, sys
from pathlib import Path

out_dir = Path(sys.argv[1])
version, api, abis = sys.argv[2], sys.argv[3], sys.argv[4:]
result = {
    "schema": 1,
    "target": "android",
    "ffmpeg": version,
    "apiLevel": int(api),
    "abis": {},
}
for abi in abis:
    manifest_path = out_dir / abi / "share" / "velmux" / "ffmpeg-capabilities.json"
    manifest = json.loads(manifest_path.read_text())
    encoders = {e["name"] for e in manifest["encoders"]}
    result["abis"][abi] = {
        "manifest": str(manifest_path.relative_to(out_dir)),
        "decoders": len(manifest["decoders"]),
        "encoders": len(manifest["encoders"]),
        "muxers": len(manifest["muxers"]),
        "demuxers": len(manifest["demuxers"]),
        "av1Encode": "libsvtav1" in encoders,
        "configuration": manifest["configuration"],
    }
(out_dir / "manifest.json").write_text(json.dumps(result, indent=1) + "\n")
print(f"aggregated {len(abis)} ABI(s): " + ", ".join(abis))
PY

log "android build complete: $OUT_DIR/android"
