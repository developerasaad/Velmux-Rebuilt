#!/usr/bin/env bash
# Build the feature-selected FFmpeg backend for iOS (Xcode cross-compile).
#
# Contract (matches .github/workflows/ffmpeg-mobile.yml):
#   scripts/ffmpeg-builder/build-ios.sh
#   env: VELMUX_IOS_DEPLOYMENT_TARGET (default 15.0, D-004)
#        VELMUX_FFMPEG_JOBS=<parallel jobs> (default: sysctl -n hw.ncpu)
#        VELMUX_WITH_SVT (default 1; SVT-AV1 encoder on arm64 slices)
#        VELMUX_IOS_SIM_X86_64 (default 0; opt-in Intel-simulator slice)
#
# Target model (simulator is a development/test target, never a product
# target — it exists for ./scripts/test.sh --ios-sim):
#   iphoneos/arm64        -> out/ios/iphoneos/{include,lib,...}
#   iphonesimulator/arm64 -> out/ios/iphonesimulator/{include,lib,...}
#
# The DEFAULT simulator layout is the single-arch arm64 slice itself: its
# BUILD-IDENTITY accurately describes the layout (same assembly shape as
# the device layout, which is what fetch.sh verifies). The Intel-simulator
# slice (sim-x86_64, lipo-merged fat libs) is built only when explicitly
# requested with VELMUX_IOS_SIM_X86_64=1 for a real Intel-Mac use case;
# the merged layout then records BOTH slice identities plus the merged
# architecture list in its BUILD-IDENTITY, so the recorded identity never
# misdescribes the artifact.
#
# Idempotent per slice: exits 0 quickly when that slice's manifest exists.
# Output also aggregates out/ios/manifest.json.
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

DEPLOYMENT_TARGET="${VELMUX_IOS_DEPLOYMENT_TARGET:-15.0}"
WITH_SVT="${VELMUX_WITH_SVT:-1}"
# The Intel-simulator slice is NOT built by default (D-058): the normal
# Apple Silicon path needs only the arm64 simulator slice, and the fat
# lipo merge complicated artifact identity for no product value.
WITH_SIM_X86_64="${VELMUX_IOS_SIM_X86_64:-0}"

case "$(uname -s)" in
  Darwin) ;;
  *) die "build-ios.sh must run on macOS (Xcode toolchain required)" ;;
esac

require_tool curl
require_tool make
require_tool python3
require_tool pkg-config
require_tool xcrun
require_tool lipo
ensure_cmake_ninja
ensure_meson

# This builder is explicitly cross-configured (per-slice wrapper scripts
# with their own -isysroot and -miphoneos-version-min); a caller's SDKROOT
# or deployment-target variables only contaminate the HOST-side tool
# builds (meson's native compiler sanity run targets the caller's platform
# against the macOS sysroot and the built tool cannot run — observed when
# the Xcode "Prepare engine and framework" phase invoked this script with
# SDKROOT=iphonesimulator / IPHONEOS_DEPLOYMENT_TARGET in its
# environment).
unset SDKROOT MACOSX_DEPLOYMENT_TARGET IPHONEOS_DEPLOYMENT_TARGET \
  TVOS_DEPLOYMENT_TARGET WATCHOS_DEPLOYMENT_TARGET XROS_DEPLOYMENT_TARGET

FFMPEG_VERSION="$(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.version")"

log "Xcode: $(xcodebuild -version 2>/dev/null | head -1 || echo 'unknown')"
log "deployment target: $DEPLOYMENT_TARGET"
log "SVT-AV1: $([ "$WITH_SVT" = "1" ] && echo enabled || echo disabled)"

# ---------------------------------------------------------------------------
# Slice settings
# ---------------------------------------------------------------------------
slice_settings() {
  # $1 = slice name: device-arm64, sim-arm64, sim-x86_64
  case "$1" in
    device-arm64)
      SLICE_SDK="iphoneos"
      SLICE_ARCH="arm64"
      SLICE_MIN_FLAG="-miphoneos-version-min=$DEPLOYMENT_TARGET"
      SLICE_VPX_TARGET="arm64-darwin-gcc"
      SLICE_AUTOCONF_HOST="aarch64-apple-darwin"
      SLICE_MESON_CPU_FAMILY="aarch64"
      SLICE_MESON_CPU="aarch64"
      SLICE_OUT="iphoneos"
      SLICE_TAG="ios-device-arm64"
      SLICE_WANT_SVT="$WITH_SVT"
      ;;
    sim-arm64)
      SLICE_SDK="iphonesimulator"
      SLICE_ARCH="arm64"
      SLICE_MIN_FLAG="-mios-simulator-version-min=$DEPLOYMENT_TARGET"
      # libvpx has no arm64 simulator toolchain, and its arm64-darwin-gcc
      # target HARD-OVERRIDES CC/CXX/AS/LD with `xcrun --sdk iphoneos
      # --find ...` (the DEVICE toolchain) inside configure, ignoring the
      # environment entirely — observed as "building for 'iOS', but
      # linking in object file built for 'iOS-simulator'". The versioned
      # darwin20 target takes the same arm*-darwin-* flag path (device
      # sysroot + min-version in cflags/ldflags) but RESPECTS the env
      # toolchain, so the slice's compiler wrappers (with trailing SDK
      # re-assertion) win every flag conflict. arm64 needs no .asm
      # assembly (neon_asm is armv7-only), so clang-as-AS without -c is
      # fine here.
      SLICE_VPX_TARGET="arm64-darwin20-gcc"
      SLICE_AUTOCONF_HOST="aarch64-apple-darwin"
      SLICE_MESON_CPU_FAMILY="aarch64"
      SLICE_MESON_CPU="aarch64"
      SLICE_OUT="iphonesimulator"
      SLICE_TAG="ios-sim-arm64"
      SLICE_WANT_SVT="$WITH_SVT"
      ;;
    sim-x86_64)
      SLICE_SDK="iphonesimulator"
      SLICE_ARCH="x86_64"
      SLICE_MIN_FLAG="-mios-simulator-version-min=$DEPLOYMENT_TARGET"
      # OPT-IN slice (VELMUX_IOS_SIM_X86_64=1, Intel-Mac development only):
      # never built by default (D-058).
      # The official x86_64 simulator toolchain: darwin18 would inject
      # -mmacosx-version-min + the MacOSX sysroot, which clang REJECTS when
      # combined with the simulator min-version ("invalid argument
      # '-mmacosx-version-min=10.14' not allowed with
      # '-mios-simulator-version-min=15.0'"). iphonesimulator injects
      # -miphoneos-version-min=7.0 + the simulator sysroot instead; the
      # wrapper's trailing re-assertion wins the min-version conflict (the
      # proven arm64 behavior).
      SLICE_VPX_TARGET="x86_64-iphonesimulator-gcc"
      SLICE_AUTOCONF_HOST="x86_64-apple-darwin"
      SLICE_MESON_CPU_FAMILY="x86_64"
      SLICE_MESON_CPU="x86_64"
      SLICE_OUT="iphonesimulator"
      SLICE_TAG="ios-sim-x86_64"
      # SVT-AV1 x86_64 requires nasm for its assembly; this development-only
      # slice ships without the AV1 *encoder* (decode via dav1d remains).
      SLICE_WANT_SVT=0
      ;;
    *)
      die "unknown slice: $1"
      ;;
  esac
  SLICE_PREFIX="$OUT_DIR/ios/slices/$SLICE_TAG"
}

# ---------------------------------------------------------------------------
# Xcode toolchain wrappers (xcrun selects SDK + sysroot; flags target the
# slice arch/min-version). Plain single-command wrappers so autoconf, cmake,
# meson and FFmpeg's configure all accept them as CC/AR/... verbatim.
# ---------------------------------------------------------------------------
make_wrappers() {
  # build_tmp comes from the calling build_one_slice (bash dynamic scope).
  local bin="$build_tmp/bin-$SLICE_TAG"
  rm -rf "$bin"
  mkdir -p "$bin"
  # Resolve the slice's SDK path once. The compiler wrappers re-assert it
  # TRAILING, after the caller's own flags: some upstream configure logic
  # (notably libvpx's arm*-darwin-* toolchain, used for the simulator
  # slices because libvpx has no arm64 simulator target) appends its own
  # -isysroot plus -m*-version-min AFTER the wrapper's leading flags —
  # the iphoneos SDK on arm64 and the macosx SDK on x86_64-darwin18 —
  # into both CFLAGS and LDFLAGS. clang resolves conflicting -isysroot /
  # -m*-version-min by the LAST occurrence, so a trailing re-assertion of
  # the slice's SDK and min-version keeps every compile AND link on the
  # correct target regardless of what upstream build systems inject
  # (observed CI failure: "building for 'iOS', but linking in object
  # file ... built for 'iOS-simulator'" in libvpx's link check).
  local sdk_path
  sdk_path="$(xcrun -sdk "$SLICE_SDK" --show-sdk-path)" ||
    die "cannot resolve the $SLICE_SDK SDK path"
  [ -d "$sdk_path" ] || die "the $SLICE_SDK SDK path does not exist: $sdk_path"
  # Binutils-style wrappers: xcrun selects SDK + sysroot, no extra flags.
  for tool in ar ranlib strip nm ld; do
    cat > "$bin/$tool" <<EOF
#!/bin/sh
exec xcrun -sdk $SLICE_SDK $tool "\$@"
EOF
    chmod +x "$bin/$tool"
  done
  # Compiler wrappers: leading arch/min-version flags plus the trailing
  # sysroot/min-version re-assertion that wins flag conflicts.
  cat > "$bin/clang" <<EOF
#!/bin/sh
exec xcrun -sdk $SLICE_SDK clang -arch $SLICE_ARCH $SLICE_MIN_FLAG "\$@" -isysroot $sdk_path $SLICE_MIN_FLAG
EOF
  cat > "$bin/clang++" <<EOF
#!/bin/sh
exec xcrun -sdk $SLICE_SDK clang++ -arch $SLICE_ARCH $SLICE_MIN_FLAG "\$@" -isysroot $sdk_path $SLICE_MIN_FLAG
EOF
  chmod +x "$bin/clang" "$bin/clang++"
  ln -sf "$bin/clang" "$bin/cc"
  ln -sf "$bin/clang++" "$bin/cxx"
  export PATH="$bin:$PATH"
  CC="$bin/cc"
  CXX="$bin/cxx"
  AR="$bin/ar"
  RANLIB="$bin/ranlib"
  STRIP="$bin/strip"
  NM="$bin/nm"
  for tool in "$CC" "$CXX" "$AR" "$RANLIB" "$STRIP" "$NM"; do
    [ -x "$tool" ] || die "wrapper not created: $tool"
  done
  export CC CXX AR RANLIB STRIP NM
}

# Meson cross file for the current slice.
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
system = 'darwin'
cpu_family = '$SLICE_MESON_CPU_FAMILY'
cpu = '$SLICE_MESON_CPU'
endian = 'little'

[properties]
needs_exe_wrapper = true
EOF
}

# Native (build machine) file: the compiler that runs ON the build machine.
# macOS runners ship /usr/bin/clang; it must not be the iOS-targeting wrapper
# or fribidi's gen.tab host-side generators cannot be built.
write_meson_native() {
  cat <<EOF
[binaries]
c = '/usr/bin/clang'
EOF
}

# expected_identity_for_slice <slice> — the identity fetch.sh computed and
# will verify after the build (empty when the builder runs standalone, in
# which case the local computation is the only one).
expected_identity_for_slice() {
  case "$1" in
    device-arm64) printf '%s' "${VELMUX_IOS_EXPECTED_DEVICE_IDENTITY:-}" ;;
    sim-arm64) printf '%s' "${VELMUX_IOS_EXPECTED_SIM_ARM64_IDENTITY:-}" ;;
    sim-x86_64) printf '%s' "${VELMUX_IOS_EXPECTED_SIM_X86_64_IDENTITY:-}" ;;
    *) printf '' ;;
  esac
}

build_one_slice() {
  local slice="$1"
  slice_settings "$slice"
  local manifest="$SLICE_PREFIX/share/velmux/ffmpeg-capabilities.json"
  local identity
  identity="$(compute_identity ios "slice=$slice" "deployment=$DEPLOYMENT_TARGET" "svt=$SLICE_WANT_SVT")"
  # fetch.sh (the only caller in CI and app builds) passes the identity it
  # will verify afterwards — single source of truth: the artifact records
  # exactly the identity the gate expects, so the gate can never disagree
  # with what the builder wrote. Cross-assert against the local
  # computation: a divergence means the environment changed between the
  # two computations (observed once on CI, 2026-11-24: same checkout,
  # seconds apart, different hashes, cause invisible because the inputs
  # were not logged). Failing HERE — before a 14-minute build — with the
  # full local input list makes the diverging line visible in the log.
  local expected
  expected="$(expected_identity_for_slice "$slice")"
  if [ -n "$expected" ]; then
    if [ "$expected" != "$identity" ]; then
      log "[$slice] identity divergence between fetch.sh and the builder:"
      log "  expected (fetch.sh):    $expected"
      log "  local computation:      $identity"
      log "  local identity inputs:"
      VELMUX_IDENTITY_DEBUG=1 \
        compute_identity ios "slice=$slice" "deployment=$DEPLOYMENT_TARGET" \
        "svt=$SLICE_WANT_SVT" > /dev/null
      die "[$slice] the builder computed a different build identity than "
          "its caller; refusing to record a mismatching identity"
    fi
  fi
  if [ -f "$SLICE_PREFIX/lib/pkgconfig/libavcodec.pc" ] && [ -f "$manifest" ] \
     && identity_matches "$SLICE_PREFIX" "$identity"; then
    log "[$slice] build up to date (identity verified)"
    fixup_pkgconfig_prefix "$SLICE_PREFIX"
    return 0
  fi
  if [ -d "$SLICE_PREFIX" ] && ! identity_matches "$SLICE_PREFIX" "$identity"; then
    log "[$slice] recorded build identity does not match the current tree; rebuilding"
    rm -rf "$SLICE_PREFIX"
  fi

  local build_tmp="$SRC_DIR/build-$SLICE_TAG"
  mkdir -p "$build_tmp" "$SLICE_PREFIX"
  make_wrappers

  # Hermetic pkg-config resolution for the whole slice build: every
  # cmake/meson find-module that consults pkg-config (notably CMake's FindPNG
  # inside freetype) must only ever see this slice's prefix. Without
  # PKG_CONFIG_LIBDIR, Homebrew packages on the CI runner leak into generated
  # .pc files: observed failure was freetype2.pc gaining "Requires: libpng"
  # (Homebrew's), which then broke FFmpeg's --static harfbuzz check with
  # "Package 'libpng', required by 'freetype2', not found".
  export PKG_CONFIG_PATH="$SLICE_PREFIX/lib/pkgconfig"
  export PKG_CONFIG_LIBDIR="$SLICE_PREFIX/lib/pkgconfig"

  log "[$slice] building (sdk=$SLICE_SDK arch=$SLICE_ARCH)"

  # --- zlib (cmake iOS) --------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libz.a" ]; then
    log "[$slice] building zlib"
    zlib_tb="$(fetch_tarball zlib)"
    zlib_src="$(extract_tarball "$zlib_tb")"
    zlib_build="$build_tmp/zlib"
    rm -rf "$zlib_build"
    cmake -G Ninja -S "$zlib_src" -B "$zlib_build" \
      -DCMAKE_SYSTEM_NAME=iOS \
      -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX" >/dev/null
    cmake --build "$zlib_build" >/dev/null
    cmake --install "$zlib_build" >/dev/null
    # zlib's CMake build installs no pkg-config file (only autotools does);
    # freetype2.pc's "Requires: zlib" needs one, so write it explicitly with
    # the pinned version.
    mkdir -p "$SLICE_PREFIX/lib/pkgconfig"
    cat > "$SLICE_PREFIX/lib/pkgconfig/zlib.pc" <<ZPC
prefix=$SLICE_PREFIX
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
    log "[$slice] zlib installed"
  fi

  # --- x264 (autoconf, per-slice source tree) ------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libx264.a" ]; then
    log "[$slice] building x264"
    x264_tb="$(fetch_tarball x264)"
    x264_src="$(extract_tarball "$x264_tb" "$SLICE_TAG")"
    (cd "$x264_src" && ./configure \
       --prefix="$SLICE_PREFIX" --enable-static --enable-pic --disable-cli \
       --host="$SLICE_AUTOCONF_HOST" \
       --extra-cflags="-arch $SLICE_ARCH $SLICE_MIN_FLAG" \
       --extra-ldflags="-arch $SLICE_ARCH $SLICE_MIN_FLAG" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] x264 installed"
  fi

  # --- x265 (cmake cross; HEVC software encode for the FFmpeg backend) -----
  if [ ! -f "$SLICE_PREFIX/lib/libx265.a" ]; then
    log "[$slice] building x265 (8-bit and 10-bit)"
    x265_tb="$(fetch_tarball x265)"
    x265_src="$(extract_tarball "$x265_tb" "$SLICE_TAG")"
    patch_x265_cmake4_compat "$x265_src"
    x265_build="$build_tmp/x265"
    rm -rf "$x265_build"
    mkdir -p "$x265_build/8bit" "$x265_build/10bit"
    # ENABLE_ASSEMBLY=OFF: preserve the proven Apple cross intrinsics route.
    local -a x265_cmake=(-G Ninja
      -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT="$SLICE_SDK"
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH"
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX"
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX"
      -DENABLE_SHARED=OFF -DENABLE_CLI=OFF -DENABLE_ASSEMBLY=OFF)
    # Follow pinned x265's multilib recipe: Main10 has a private namespace;
    # Main8 owns the exported API and dispatches x265_api_get(10) to it.
    cmake "${x265_cmake[@]}" -S "$x265_src/source" -B "$x265_build/10bit" \
      -DHIGH_BIT_DEPTH=ON -DMAIN12=OFF -DEXPORT_C_API=OFF >/dev/null
    cmake --build "$x265_build/10bit" >/dev/null
    ln -sf ../10bit/libx265.a "$x265_build/8bit/libx265_main10.a"
    cmake "${x265_cmake[@]}" -S "$x265_src/source" -B "$x265_build/8bit" \
      -DHIGH_BIT_DEPTH=OFF -DEXPORT_C_API=ON -DLINKED_10BIT=ON \
      -DEXTRA_LIB=x265_main10.a -DEXTRA_LINK_FLAGS=-L. >/dev/null
    cmake --build "$x265_build/8bit" >/dev/null
    # Use Apple's archive combiner, not GNU ar flags or a Homebrew libtool.
    # Install only after the archive contains both implementations.
    (cd "$x265_build/8bit" &&
      mv libx265.a libx265_main.a
      xcrun -sdk "$SLICE_SDK" libtool -static -o libx265.a libx265_main.a libx265_main10.a)
    cmake --install "$x265_build/8bit" >/dev/null
    sanitize_x265_pc "$SLICE_PREFIX"
    log "[$slice] x265 installed"
  fi
  # x265's cross-generated pc omits libc++ from its static C link closure.
  # FFmpeg probes through the C compiler, which does not add it implicitly.
  # Repair reused prefixes too; adding it only during installation would
  # leave a previously installed archive unusable after a configure failure.
  append_pc_libs "$SLICE_PREFIX" x265 -lc++

  # --- lame (autoconf) -------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libmp3lame.a" ]; then
    log "[$slice] building lame"
    lame_tb="$(fetch_tarball lame)"
    lame_src="$(extract_tarball "$lame_tb" "$SLICE_TAG")"
    (cd "$lame_src" && ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic \
       --disable-dependency-tracking --disable-gtktest --disable-nasm \
       --disable-frontend \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] lame installed"
  fi

  # --- opus (autoconf) ----------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libopus.a" ]; then
    log "[$slice] building opus"
    opus_tb="$(fetch_tarball opus)"
    opus_src="$(extract_tarball "$opus_tb" "$SLICE_TAG")"
    (cd "$opus_src" && ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic \
       --disable-doc --disable-extra-programs \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] opus installed"
  fi

  # --- libogg + libvorbis (autoconf) -----------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libogg.a" ]; then
    log "[$slice] building libogg"
    ogg_tb="$(fetch_tarball libogg)"
    ogg_src="$(extract_tarball "$ogg_tb" "$SLICE_TAG")"
    (cd "$ogg_src" && ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] libogg installed"
  fi
  if [ ! -f "$SLICE_PREFIX/lib/libvorbis.a" ]; then
    log "[$slice] building libvorbis"
    vorbis_tb="$(fetch_tarball libvorbis)"
    vorbis_src="$(extract_tarball "$vorbis_tb" "$SLICE_TAG")"
    (cd "$vorbis_src" && PKG_CONFIG_PATH="$SLICE_PREFIX/lib/pkgconfig" ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic --disable-docs \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     # libvorbis's configure unconditionally injects the legacy
     # -force_cpusubtype_ALL flag on Darwin, which current Apple linkers
     # reject; strip it from the generated Makefiles (portable sed -i.bak).
     find . -name Makefile -exec sed -i.bak 's/-force_cpusubtype_ALL //g' {} + && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] libvorbis installed"
  fi

  # --- libvpx (custom configure) ------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libvpx.a" ]; then
    log "[$slice] building libvpx"
    vpx_tb="$(fetch_tarball libvpx)"
    vpx_src="$(extract_tarball "$vpx_tb" "$SLICE_TAG")"
    # vpx's die() prints to stdout; capture and dump on failure. AS must be
    # the clang wrapper (no standalone `as` in Xcode; clang assembles with -c,
    # which vpx's android branch adds for *clang AS; the darwin branch sets
    # its own flags).
    local vpx_log="$build_tmp/vpx-configure-$SLICE_TAG.log"
    local vpx_as="$CC"
    if [ "$SLICE_ARCH" = "x86_64" ]; then
      # x86 asm is nasm syntax; installed via brew in the workflow.
      command -v nasm >/dev/null 2>&1 && vpx_as="nasm"
    fi
    if ! (cd "$vpx_src" && AS="$vpx_as" LD="$CC" ./configure \
       --target="$SLICE_VPX_TARGET" \
       --prefix="$SLICE_PREFIX" \
       --disable-shared --enable-static --enable-pic \
       --disable-unit-tests --disable-examples --disable-tools --disable-docs \
       --disable-install-docs --disable-webm-io --enable-vp9-highbitdepth \
       --extra-cflags="-arch $SLICE_ARCH $SLICE_MIN_FLAG" \
       >"$vpx_log" 2>&1); then
      cat "$vpx_log" >&2
      if [ -f "$vpx_src/config.log" ]; then
        echo "--- vpx config.log (tail) ---" >&2
        tail -n 80 "$vpx_src/config.log" >&2
      fi
      die "[$slice] libvpx configure failed (full log above)"
    fi
    (cd "$vpx_src" && make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] libvpx installed"
  fi

  # --- dav1d (meson cross) --------------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libdav1d.a" ]; then
    log "[$slice] building dav1d"
    dav1d_tb="$(fetch_tarball dav1d)"
    dav1d_src="$(extract_tarball "$dav1d_tb")"
    dav1d_build="$build_tmp/dav1d"
    rm -rf "$dav1d_build"
    local cross_file="$build_tmp/dav1d-cross.meson"
    write_meson_cross > "$cross_file"
    local native_file="$build_tmp/native.meson"
    write_meson_native > "$native_file"
    local dav1d_log="$build_tmp/dav1d-setup-$SLICE_TAG.log"
    if ! env -u CC -u CXX meson setup "$dav1d_build" "$dav1d_src" \
      --prefix="$SLICE_PREFIX" --libdir=lib \
      --default-library=static --buildtype=release -Db_staticpic=true \
      --cross-file "$cross_file" --native-file "$native_file" \
      -Denable_tools=false -Denable_tests=false >"$dav1d_log" 2>&1; then
      cat "$dav1d_log" >&2
      die "[$slice] dav1d meson setup failed (full log above)"
    fi
    if ! ninja -C "$dav1d_build" >"$dav1d_log" 2>&1; then
      tail -n 60 "$dav1d_log" >&2
      die "[$slice] dav1d ninja build failed (tail above)"
    fi
    meson install -C "$dav1d_build" >"$dav1d_log" 2>&1
    log "[$slice] dav1d installed"
  fi

  # --- SVT-AV1 (cmake iOS; arm64 slices) --------------------------------------------------------
  if [ "$SLICE_WANT_SVT" = "1" ]; then
    if [ ! -f "$SLICE_PREFIX/lib/libSvtAv1Enc.a" ]; then
      log "[$slice] building SVT-AV1"
      svt_tb="$(fetch_tarball svt-av1)"
      svt_src="$(extract_tarball "$svt_tb")"
      svt_build="$build_tmp/svt-av1"
      rm -rf "$svt_build"
      cmake -G Ninja -S "$svt_src" -B "$svt_build" \
        -DCMAKE_SYSTEM_NAME=iOS \
        -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
        -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
        -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
        -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX" \
        -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DBUILD_APPS=OFF \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 >/dev/null
      cmake --build "$svt_build" >/dev/null
      cmake --install "$svt_build" >/dev/null
      log "[$slice] SVT-AV1 installed"
    fi
  else
    log "[$slice] SVT-AV1 skipped for this slice"
  fi

  # --- freetype (cmake iOS) ------------------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libfreetype.a" ]; then
    log "[$slice] building freetype"
    ft_tb="$(fetch_tarball freetype)"
    ft_src="$(extract_tarball "$ft_tb")"
    ft_build="$build_tmp/freetype"
    rm -rf "$ft_build"
    cmake -G Ninja -S "$ft_src" -B "$ft_build" \
      -DCMAKE_SYSTEM_NAME=iOS \
      -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX" \
      -DBUILD_SHARED_LIBS=OFF -DFT_DISABLE_ZLIB=FALSE \
      -DFT_REQUIRE_ZLIB=TRUE -DFT_DISABLE_BZIP2=TRUE -DFT_DISABLE_BROTLI=TRUE \
      -DFT_DISABLE_HARFBUZZ=TRUE -DFT_DISABLE_PNG=TRUE >/dev/null
    cmake --build "$ft_build" >/dev/null
    cmake --install "$ft_build" >/dev/null
    log "[$slice] freetype installed"
  fi

  # --- libpng (cmake iOS) ------------------------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libpng16.a" ]; then
    log "[$slice] building libpng"
    png_tb="$(fetch_tarball libpng)"
    png_src="$(extract_tarball "$png_tb")"
    png_build="$build_tmp/libpng"
    rm -rf "$png_build"
    cmake -G Ninja -S "$png_src" -B "$png_build" \
      -DCMAKE_SYSTEM_NAME=iOS \
      -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX" \
      -DBUILD_SHARED_LIBS=OFF -DPNG_TESTS=OFF -DPNG_TOOLS=OFF \
      -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_FRAMEWORK=OFF \
      -DPNG_EXECUTABLES=OFF >/dev/null
    cmake --build "$png_build" >/dev/null
    cmake --install "$png_build" >/dev/null
    log "[$slice] libpng installed"
  fi

  # --- harfbuzz (cmake iOS, with freetype) ----------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libharfbuzz.a" ]; then
    log "[$slice] building harfbuzz"
    hb_tb="$(fetch_tarball harfbuzz)"
    hb_src="$(extract_tarball "$hb_tb")"
    hb_build="$build_tmp/harfbuzz"
    rm -rf "$hb_build"
    # harfbuzz 10.2.0's CMake port has a dead HB_IOS branch: the variable is
    # never defined anywhere in its build files, so iOS builds fall into the
    # macOS else-branch, whose ApplicationServices framework does not exist
    # in the iOS SDK. find_library then records no frameworks at all, and
    # FFmpeg's --static pkg-config check fails with undefined _CTFont*
    # symbols (static archives defer resolution, so harfbuzz itself builds).
    # CoreText integration is disabled instead: harfbuzz's internal shaper —
    # the one libass drives with its own font sources — is unaffected.
    cmake -G Ninja -S "$hb_src" -B "$hb_build" \
      -DCMAKE_SYSTEM_NAME=iOS \
      -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_FIND_ROOT_PATH="$SLICE_PREFIX" \
      -DHB_HAVE_CORETEXT=OFF \
      -DBUILD_SHARED_LIBS=OFF -DHB_BUILD_UTILS=OFF -DHB_BUILD_TESTS=OFF \
      -DHB_HAVE_GLIB=OFF -DHB_HAVE_FREETYPE=ON >/dev/null
    cmake --build "$hb_build" >/dev/null
    cmake --install "$hb_build" >/dev/null
    log "[$slice] harfbuzz installed"
  fi

  # --- fribidi (meson cross) ----------------------------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libfribidi.a" ]; then
    log "[$slice] building fribidi"
    fribidi_tb="$(fetch_tarball fribidi)"
    fribidi_src="$(extract_tarball "$fribidi_tb")"
    fribidi_build="$build_tmp/fribidi"
    rm -rf "$fribidi_build"
    local fribidi_cross="$build_tmp/fribidi-cross.meson"
    write_meson_cross > "$fribidi_cross"
    local native_file="$build_tmp/native.meson"
    write_meson_native > "$native_file"
    local fribidi_log="$build_tmp/fribidi-setup-$SLICE_TAG.log"
    if ! env -u CC -u CXX meson setup "$fribidi_build" "$fribidi_src" \
      --prefix="$SLICE_PREFIX" --libdir=lib \
      --default-library=static --buildtype=release -Db_staticpic=true \
      --cross-file "$fribidi_cross" --native-file "$native_file" \
      -Ddocs=false -Dtests=false >"$fribidi_log" 2>&1; then
      cat "$fribidi_log" >&2
      die "[$slice] fribidi meson setup failed (full log above)"
    fi
    if ! ninja -C "$fribidi_build" >"$fribidi_log" 2>&1; then
      tail -n 60 "$fribidi_log" >&2
      die "[$slice] fribidi ninja build failed (tail above)"
    fi
    meson install -C "$fribidi_build" >"$fribidi_log" 2>&1
    log "[$slice] fribidi installed"
  fi

  # --- libass (autoconf) ----------------------------------------------------------------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libass.a" ]; then
    log "[$slice] building libass"
    ass_tb="$(fetch_tarball libass)"
    ass_src="$(extract_tarball "$ass_tb" "$SLICE_TAG")"
    # libass uses plain autoconf (no --extra-cflags); flags travel via env.
    (cd "$ass_src" && \
     CFLAGS="-arch $SLICE_ARCH $SLICE_MIN_FLAG -O3 -fPIC -I$SLICE_PREFIX/include" \
     LDFLAGS="-arch $SLICE_ARCH $SLICE_MIN_FLAG -L$SLICE_PREFIX/lib" \
     PKG_CONFIG_PATH="$SLICE_PREFIX/lib/pkgconfig" \
     PKG_CONFIG_LIBDIR="$SLICE_PREFIX/lib/pkgconfig" \
     ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic \
       --disable-fontconfig --disable-require-system-font-provider \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] libass installed"
  fi

  # --- zimg (autoconf cross, in-tree per slice; zscale for HDR tone mapping) -----------------
  if [ ! -f "$SLICE_PREFIX/lib/libzimg.a" ]; then
    log "[$slice] building zimg"
    ensure_build_libtool "$OUT_DIR/tools"
    zimg_tb="$(fetch_tarball zimg)"
    zimg_src="$(extract_tarball "$zimg_tb" "$SLICE_TAG")"
    # The archive ships no configure: regenerate once per slice source tree.
    # autogen output is captured — a swallowed failure here would only
    # surface as a confusing configure error later.
    local zimg_autogen_log="$build_tmp/zimg-autogen-$SLICE_TAG.log"
    if ! (cd "$zimg_src" && ./autogen.sh >"$zimg_autogen_log" 2>&1); then
      cat "$zimg_autogen_log" >&2
      die "[$slice] zimg autogen failed (log above)"
    fi
    (cd "$zimg_src" && \
     STL_LIBS="-lc++ -lm" CC="$CC" CXX="$CXX" \
     CFLAGS="-arch $SLICE_ARCH $SLICE_MIN_FLAG -O3 -fPIC -I$SLICE_PREFIX/include" \
     CXXFLAGS="-arch $SLICE_ARCH $SLICE_MIN_FLAG -O3 -fPIC -I$SLICE_PREFIX/include" \
     LDFLAGS="-arch $SLICE_ARCH $SLICE_MIN_FLAG -L$SLICE_PREFIX/lib" \
     ./configure \
       --prefix="$SLICE_PREFIX" --disable-shared --enable-static --with-pic \
       --host="$SLICE_AUTOCONF_HOST" \
       >/dev/null && \
     make -j"$JOBS" >/dev/null && make install >/dev/null)
    log "[$slice] zimg installed"
  fi
  # zimg defaults to GNU libstdc++, which is unavailable in current Apple
  # SDKs. Normalize retained pc files as well as setting STL_LIBS above.
  python3 - "$SLICE_PREFIX/lib/pkgconfig/zimg.pc" <<'ZIMG_PC'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
updated = text.replace("-lstdc++", "-lc++")
if updated != text:
    path.write_text(updated)
ZIMG_PC
  append_pc_libs "$SLICE_PREFIX" zimg -lc++ -lm

  # --- libwebp (cmake cross; still WebP output, mandate 29.3) --------------------------------
  if [ ! -f "$SLICE_PREFIX/lib/libwebp.a" ]; then
    log "[$slice] building libwebp"
    webp_tb="$(fetch_tarball libwebp)"
    webp_src="$(extract_tarball "$webp_tb" "$SLICE_TAG")"
    local webp_build="$build_tmp/webp"
    rm -rf "$webp_build"
    cmake -G Ninja -S "$webp_src" -B "$webp_build" \
      -DCMAKE_OSX_SYSROOT="$SLICE_SDK" \
      -DCMAKE_OSX_ARCHITECTURES="$SLICE_ARCH" \
      -DCMAKE_OSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$SLICE_PREFIX" \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DBUILD_SHARED_LIBS=OFF \
      -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF \
      -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_GIF2WEBP=OFF \
      -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF \
      -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF \
      -DWEBP_BUILD_EXTRAS=OFF >/dev/null
    cmake --build "$webp_build" >/dev/null
    cmake --install "$webp_build" >/dev/null
    log "[$slice] libwebp installed"
  fi

  # --- FFmpeg (out of tree) -----------------------------------------------------------------------------------------
  ffmpeg_tb="$(fetch_ffmpeg)"
  ffmpeg_src="$(extract_tarball "$ffmpeg_tb")"
  patch_ffmpeg_aiff_layouts "$ffmpeg_src"
  local ffbuild="$build_tmp/ffmpeg"
  if [ ! -f "$SLICE_PREFIX/lib/libavcodec.a" ]; then
    log "[$slice] configuring FFmpeg"
    rm -rf "$ffbuild"
    mkdir -p "$ffbuild"
    local svt_flag=""
    if [ "$SLICE_WANT_SVT" = "1" ]; then
      svt_flag="--enable-libsvtav1"
    fi
    local configure_log="$build_tmp/ffmpeg-configure-$SLICE_TAG.log"
    # pkg-config must resolve every dependency from the slice prefix.
    if ! (cd "$ffbuild" && PKG_CONFIG_PATH="$SLICE_PREFIX/lib/pkgconfig" \
        PKG_CONFIG_LIBDIR="$SLICE_PREFIX/lib/pkgconfig" \
        "$ffmpeg_src/configure" \
       --prefix="$SLICE_PREFIX" \
       --pkg-config-flags=--static \
       --enable-cross-compile \
       --target-os=darwin \
       --arch="$SLICE_ARCH" \
       --cc="$CC" --cxx="$CXX" \
       --ar="$AR" --ranlib="$RANLIB" --nm="$NM" --strip="$STRIP" \
       --extra-cflags="-arch $SLICE_ARCH $SLICE_MIN_FLAG -I$SLICE_PREFIX/include -fvisibility=hidden" \
       --extra-ldflags="-arch $SLICE_ARCH $SLICE_MIN_FLAG -L$SLICE_PREFIX/lib" \
       --disable-shared --enable-static --enable-pic \
       --disable-debug --disable-doc \
       --disable-ffplay \
       --disable-network \
       --disable-avdevice \
       --disable-autodetect \
       --disable-x86asm \
       --enable-pthreads \
       --enable-videotoolbox \
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
      die "[$slice] FFmpeg configure failed (full log above)"
    fi
    log "[$slice] building FFmpeg with $JOBS jobs"
    if ! (cd "$ffbuild" && make -j"$JOBS" >"$configure_log" 2>&1); then
      tail -n 80 "$configure_log" >&2
      die "[$slice] FFmpeg build failed (tail above)"
    fi
    (cd "$ffbuild" && make install >/dev/null)
    log "[$slice] FFmpeg installed to $SLICE_PREFIX"
  fi

  # --- Capability manifest from the actual build tree -------------------------------------------------------------
  if [ ! -f "$manifest" ] || [ "$BUILDER_ROOT/manifest-from-config.py" -nt "$manifest" ]; then
    log "[$slice] generating capability manifest"
    python3 "$BUILDER_ROOT/manifest-from-config.py" \
      "$ffbuild" "$ffmpeg_src" "$FFMPEG_VERSION" "$manifest" >&2
  fi

  # --- Validate against the Velmux feature requirements ------------------------------------------------------------
  local exempt=""
  if [ "$SLICE_WANT_SVT" = "0" ]; then
    exempt="--exempt libsvtav1"
  fi
  log "[$slice] validating build"
  python3 "$BUILDER_ROOT/validate.py" $exempt "$manifest" >&2

  unset CC CXX AR RANLIB STRIP NM PKG_CONFIG_PATH PKG_CONFIG_LIBDIR
  write_identity "$SLICE_PREFIX" "$identity" \
    "ios FFmpeg, slice=$slice deployment=$DEPLOYMENT_TARGET, jobs=$JOBS"
  fixup_pkgconfig_prefix "$SLICE_PREFIX"
  log "[$slice] build complete: $SLICE_PREFIX"
}

# ---------------------------------------------------------------------------
# Build every slice, then assemble the publishable layouts.
# ---------------------------------------------------------------------------
build_one_slice device-arm64
build_one_slice sim-arm64
if [ "$WITH_SIM_X86_64" = "1" ]; then
  build_one_slice sim-x86_64
fi

log "assembling iphoneos layout"
DEVICE_OUT="$OUT_DIR/ios/iphoneos"
rm -rf "$DEVICE_OUT"
mkdir -p "$DEVICE_OUT"
cp -R "$OUT_DIR/ios/slices/ios-device-arm64/include" "$DEVICE_OUT/"
cp -R "$OUT_DIR/ios/slices/ios-device-arm64/lib" "$DEVICE_OUT/"
mkdir -p "$DEVICE_OUT/share/velmux"
cp "$OUT_DIR/ios/slices/ios-device-arm64/share/velmux/ffmpeg-capabilities.json" \
   "$DEVICE_OUT/share/velmux/"
cp "$OUT_DIR/ios/slices/ios-device-arm64/BUILD-IDENTITY" "$DEVICE_OUT/"

SIM_ARM64="$OUT_DIR/ios/slices/ios-sim-arm64"
SIM_OUT="$OUT_DIR/ios/iphonesimulator"
SIM_ARCHS="arm64"
rm -rf "$SIM_OUT"
if [ "$WITH_SIM_X86_64" = "1" ]; then
  log "assembling iphonesimulator layout (lipo arm64 + x86_64; opt-in)"
  SIM_X86="$OUT_DIR/ios/slices/ios-sim-x86_64"
  SIM_ARCHS="arm64 x86_64"
  mkdir -p "$SIM_OUT/include" "$SIM_OUT/lib" "$SIM_OUT/share/velmux"
  cp -R "$SIM_ARM64/include/." "$SIM_OUT/include/"
  cp "$SIM_ARM64/share/velmux/ffmpeg-capabilities.json" \
     "$SIM_OUT/share/velmux/"
  for lib in "$SIM_ARM64"/lib/*.a; do
    name="$(basename "$lib")"
    if [ -f "$SIM_X86/lib/$name" ]; then
      lipo -create "$lib" "$SIM_X86/lib/$name" -output "$SIM_OUT/lib/$name"
    else
      # Slice-specific library (e.g. SVT-AV1 on arm64 only): ship as-is.
      cp "$lib" "$SIM_OUT/lib/$name"
    fi
  done
  # pkg-config files are consumed at build time only; ship the arm64 ones.
  cp -R "$SIM_ARM64/lib/pkgconfig" "$SIM_OUT/lib/"
  # The merged layout's identity keeps the sim-arm64 identity as line 1
  # (the slice the headers, .pc files and manifest come from — the identity
  # fetch.sh verifies for reuse) and records the merge honestly: the arch
  # list and the x86_64 slice's own identity are part of the provenance.
  {
    cat "$SIM_ARM64/BUILD-IDENTITY"
    printf 'merged-architectures: %s\n' "$SIM_ARCHS"
    printf 'x86_64-slice-identity: %s\n' "$(identity_recorded "$SIM_X86")"
  } > "$SIM_OUT/BUILD-IDENTITY"
else
  # Default: the simulator layout IS the single-arch arm64 slice — same
  # assembly shape as the device layout, with the slice's own identity
  # describing exactly what is shipped.
  log "assembling iphonesimulator layout (arm64 slice)"
  mkdir -p "$SIM_OUT/share/velmux"
  cp -R "$SIM_ARM64/include" "$SIM_OUT/"
  cp -R "$SIM_ARM64/lib" "$SIM_OUT/"
  cp "$SIM_ARM64/share/velmux/ffmpeg-capabilities.json" \
     "$SIM_OUT/share/velmux/"
  cp "$SIM_ARM64/BUILD-IDENTITY" "$SIM_OUT/"
fi

AGGREGATE="$OUT_DIR/ios/manifest.json"
log "aggregating manifests"
python3 - "$OUT_DIR/ios" "$FFMPEG_VERSION" "$DEPLOYMENT_TARGET" "$SIM_ARCHS" <<'PY'
import json, sys
from pathlib import Path

out_dir = Path(sys.argv[1])
version, target, sim_archs = sys.argv[2], sys.argv[3], sys.argv[4].split()

def load(rel):
    return json.loads(
        (out_dir / rel / "share" / "velmux" / "ffmpeg-capabilities.json").read_text())

device = load("iphoneos")
sim = load("iphonesimulator")
result = {
    "schema": 1,
    "target": "ios",
    "ffmpeg": version,
    "deploymentTarget": target,
    "iphoneos": {
        "archs": ["arm64"],
        "manifest": "iphoneos/share/velmux/ffmpeg-capabilities.json",
        "decoders": len(device["decoders"]),
        "encoders": len(device["encoders"]),
        "av1Encode": "libsvtav1" in {e["name"] for e in device["encoders"]},
        "configuration": device["configuration"],
    },
    "iphonesimulator": {
        "archs": sim_archs,
        "manifest": "iphonesimulator/share/velmux/ffmpeg-capabilities.json",
        "decoders": len(sim["decoders"]),
        "encoders": len(sim["encoders"]),
        "av1Encode": "libsvtav1" in {e["name"] for e in sim["encoders"]},
        "configuration": sim["configuration"],
    },
}
(out_dir / "manifest.json").write_text(json.dumps(result, indent=1) + "\n")
print("aggregated iphoneos + iphonesimulator (" + ", ".join(sim_archs) + ")")
PY

log "ios build complete: $OUT_DIR/ios"
