#!/usr/bin/env bash
# Shared helpers for the Velmux FFmpeg builder.
# Targets: host (Linux/macOS), android (NDK), ios (Xcode).
#
# NOTE on error handling: callers capture function output via $( ) command
# substitution. Plain `set -e` does NOT propagate into command substitutions
# unless `inherit_errexit` is enabled — without it, failed builds silently
# continued and "installed" messages lied (caught in CI). Enabled below,
# guarded for bash < 4.4.
set -euo pipefail
if (shopt -s inherit_errexit) 2>/dev/null; then
  shopt -s inherit_errexit
fi

BUILDER_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOWNLOADS_DIR="${VELMUX_DOWNLOADS_DIR:-$BUILDER_ROOT/downloads}"
SRC_DIR="${VELMUX_SRC_DIR:-$BUILDER_ROOT/src}"
OUT_DIR="${VELMUX_OUT_DIR:-$BUILDER_ROOT/out}"

JOBS="${VELMUX_FFMPEG_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)}"

# NOTE: log() writes to stderr on purpose — functions that print captured
# values (fetch_tarball, extract_tarball, json_field) must keep stdout clean.
log() { printf '[velmux-ffmpeg] %s\n' "$*" >&2; }
die() { printf '[velmux-ffmpeg] ERROR: %s\n' "$*" >&2; exit 1; }

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool '$1' not found in PATH"
}

# Optional extra tool locations (e.g. a user venv carrying cmake/ninja/meson
# when the system does not). Set VELMUX_TOOL_PATH to a colon-separated list.
if [ -n "${VELMUX_TOOL_PATH:-}" ]; then
  case ":$PATH:" in
    *":$VELMUX_TOOL_PATH:"*) ;;
    *) export PATH="$VELMUX_TOOL_PATH:$PATH" ;;
  esac
fi

# Ensure cmake and ninja are usable (dav1d, fribidi and friends build with
# them; FFmpeg's configure accepts ninja makefiles).
ensure_cmake_ninja() {
  command -v cmake >/dev/null 2>&1 || die "cmake not found"
  command -v ninja >/dev/null 2>&1 || die "ninja not found"
}

# ensure_meson: dav1d builds with meson.
ensure_meson() {
  command -v meson >/dev/null 2>&1 || die "meson not found (install with: pip install meson)"
}

# sha256_of <file> — portable digest (GNU coreutils, macOS shasum, openssl).
# macOS Sequoia ships a BSD-style sha256sum without GNU's checksum-file mode,
# so verify by digest comparison instead of `sha256sum -c`.
sha256_of() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | cut -d' ' -f1
  else
    openssl dgst -sha256 -r "$file" | cut -d' ' -f1
  fi
}

# verify_sha256 <expected> <file> — true when the file matches the pin.
verify_sha256() {
  [ "$(sha256_of "$2")" = "$1" ]
}

# json_field <file> <dot.path> — minimal JSON reader using python3.
# Prints nothing when the path does not exist.
json_field() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as fh:
        doc = json.load(fh)
    node = doc
    for part in sys.argv[2].split('.'):
        node = node[part]
except (KeyError, TypeError, json.JSONDecodeError, OSError):
    sys.exit(0)
if isinstance(node, list):
    for item in node:
        print(item)
else:
    print(node)
PY
}

# patch_ffmpeg_aiff_layouts <ffmpeg-source-dir> — preserve actual CoreAudio
# speaker identities in AIFF, with an exact, idempotent pinned-source repair.
patch_ffmpeg_aiff_layouts() {
  python3 "$BUILDER_ROOT/patch-aiff-layouts.py" "$1"
  log "verified FFmpeg AIFF speaker-description repair"
}

# patch_x265_cmake4_compat <x265-source-dir> — make x265 3.6's CMake files
# work under CMake >= 4 (and stay fine under 3.x). Upstream 3.6 pins
# CMP0025/CMP0054 to OLD behaviors that CMake 4 removed and declares
# pre-3.5 minimums in two CMakeLists; the NEW behaviors are correct for
# any modern CMake, so the pins are dropped and the minimums raised (the
# same compatibility approach distributions carry for x265 under CMake 4).
# Implemented in python: the sed rewrite of this patch silently matched
# nothing (ERE treats the patterns' literal parentheses as grouping) while
# still logging "patched" — python's plain string replace cannot lie.
patch_x265_cmake4_compat() {
  local src="$1"
  local cm="$src/source/CMakeLists.txt"
  local hdr="$src/source/dynamicHDR10/CMakeLists.txt"
  [ -f "$cm" ] || die "x265 CMakeLists not found: $cm"
  python3 - "$cm" "$hdr" <<'PY'
import re, sys
for path in sys.argv[1:]:
    try:
        s = open(path, encoding="utf-8").read()
    except FileNotFoundError:
        continue
    out = s.replace("cmake_policy(SET CMP0025 OLD)",
                    "# CMP0025 pin removed for CMake 4 compat")
    out = out.replace("cmake_policy(SET CMP0054 OLD)",
                      "# CMP0054 pin removed for CMake 4 compat")
    out = re.sub(r"cmake_minimum_required\s*\(\s*VERSION\s*2\.8\.[0-9]*\)",
                 "cmake_minimum_required(VERSION 3.5)", out)
    if out != s:
        open(path, "w", encoding="utf-8").write(out)
PY
  grep -q "CMP0025 pin removed" "$cm" \
    || die "x265 CMake 4 patch did not apply to $cm"
  log "patched x265 CMakeLists for CMake 4 compatibility"
}

# sanitize_x265_pc <prefix> — clean the generated x265.pc after install.
# x265's CMakeLists turns CMAKE_CXX_IMPLICIT_LINK_LIBRARIES into
# Libs.private by prepending -l to every entry; cross toolchains that
# supply an already-prefixed driver ("-l:libunwind.a", as the Android
# NDK does for its unwinder) produce a malformed doubled entry
# ("-l-l:libunwind.a") that no linker can resolve — FFmpeg's configure
# then reports "x265 not found using pkg-config". The cross linker adds
# its unwinder implicitly anyway, so the entry is simply dropped.
sanitize_x265_pc() {
  local pc="$1/lib/pkgconfig/x265.pc"
  [ -f "$pc" ] || return 0
  python3 - "$pc" <<'PY'
import re, sys
path = sys.argv[1]
s = open(path, encoding="utf-8").read()
out = re.sub(r"\s-l-l:\S+", "", s)
out = re.sub(r"\s-l:libunwind\.a", "", out)
if out != s:
    open(path, "w", encoding="utf-8").write(out)
PY
  log "sanitized x265.pc implicit-lib entries"
}

# append_pc_libs <prefix> <pc-name> <libs...> — append missing -l entries to
# a staged .pc's Libs line. Needed on Android: bionic keeps the math
# functions in libm (glibc/libSystem fold them into libc), so probes that
# link libwebp/zimg WITHOUT -lm fail on undefined pow/log10/expf and
# FFmpeg's configure reports the dependency as missing. No-op (and no
# duplicate) when the entry is already there.
append_pc_libs() {
  local prefix="$1" name="$2"; shift 2
  local pc="$prefix/lib/pkgconfig/$name.pc"
  [ -f "$pc" ] || return 0
  python3 - "$pc" "$@" <<'PY'
import sys
path = sys.argv[1]
want = sys.argv[2:]
s = open(path, encoding="utf-8").read()
lines = s.splitlines()
out = []
appended = []
for line in lines:
    if line.startswith("Libs:") and not line.startswith("Libs.private:"):
        for w in want:
            if f" {w} " not in f" {line} ":
                line = line.rstrip() + " " + w
                appended.append(w)
    out.append(line)
if appended:
    open(path, "w", encoding="utf-8").write("\n".join(out) + "\n")
PY
  log "patched $name.pc (probe link closure)"
}

# ensure_build_libtool <prefix> — guarantee a host-usable libtoolize for
# autogen-based dependencies (zimg). CI images ship libtool; hosts without
# it get a prefix-local build from the pinned tarball (nasm-style
# fallback). Exports PATH/ACLOCAL_PATH so autoreconf finds the macros.
#
# libtool itself is a HOST tool: its build must not see any caller's cross
# environment. The iOS builders export the cross CC/CXX/AR wrapper AND put
# the wrapper dir (with cc/clang/clang++ symlinks) FIRST on PATH; Xcode
# script phases export SDKROOT/deployment targets. Any of those leaking
# into this configure compiles the test program for iOS and aborts with
# "cannot run C compiled programs" — so the build runs with a controlled
# system PATH, the absolute host compiler when the platform has one, and
# every cross variable unset.
ensure_build_libtool() {
  local prefix="$1"
  if command -v libtoolize >/dev/null 2>&1; then
    return 0
  fi
  if [ ! -x "$prefix/bin/libtoolize" ]; then
    log "no system libtoolize; building libtool into $prefix"
    local tb src host_path host_cc clean_env
    tb="$(fetch_tarball libtool)"
    src="$(extract_tarball "$tb")"
    host_path="/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"
    host_cc=""
    if [ -x /usr/bin/clang ]; then host_cc=/usr/bin/clang; fi
    # Options first, then assignments: valid for both GNU and BSD env.
    clean_env=(env -u CXX -u AR -u RANLIB -u STRIP -u NM -u CFLAGS -u CXXFLAGS \
       -u LDFLAGS -u SDKROOT -u MACOSX_DEPLOYMENT_TARGET \
       -u IPHONEOS_DEPLOYMENT_TARGET -u TVOS_DEPLOYMENT_TARGET \
       -u WATCHOS_DEPLOYMENT_TARGET -u XROS_DEPLOYMENT_TARGET \
       PATH="$host_path")
    [ -n "$host_cc" ] && clean_env+=("CC=$host_cc")
    (cd "$src" && "${clean_env[@]}" ./configure --prefix="$prefix" >/dev/null && \
     "${clean_env[@]}" \
       make -j"${VELMUX_FFMPEG_JOBS:-$(nproc 2>/dev/null || echo 4)}" >/dev/null && \
     "${clean_env[@]}" make install >/dev/null)
    # GNU autoreconf on Darwin probes glibtoolize (Homebrew's name, chosen
    # because Apple ships an unrelated /usr/bin/libtool); the from-source
    # install only provides libtoolize. Provide both names.
    if [ -x "$prefix/bin/libtoolize" ] && [ ! -e "$prefix/bin/glibtoolize" ]; then
      ln -s libtoolize "$prefix/bin/glibtoolize"
    fi
    log "libtool installed to $prefix"
  fi
  export PATH="$prefix/bin:$PATH"
  [ -d "$prefix/share/aclocal" ] && \
    export ACLOCAL_PATH="${ACLOCAL_PATH:+$ACLOCAL_PATH:}$prefix/share/aclocal"
}

# ua_for_url <url> — print the User-Agent to use for a host, if any.
# Some hosts reject plain curl/wget requests and need a browser-like UA.
# Verified per host from real networks: code.videolan.org rejects the
# default curl UA, and gitlab.com (observed 2026-11) answers 403 to the
# default UA from some networks while serving the identical pinned bytes
# to a browser UA — so both get the browser UA. Hosts that prefer the
# default print nothing.
ua_for_url() {
  case "$1" in
    https://code.videolan.org/* | http://code.videolan.org/* | \
    https://gitlab.com/* | http://gitlab.com/*)
      printf '%s' "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"
      ;;
  esac
}

# download <name> — fetch + verify the pinned tarball recorded in versions.json.
# Tries the canonical URL first, then mirrors. Skips when the cached file
# already matches its checksum. The User-Agent is chosen per host
# (see ua_for_url).
fetch_tarball() {
  local name="$1"
  local version url sha256 fname
  version="$(json_field "$BUILDER_ROOT/versions.json" "dependencies.$name.version")"
  url="$(json_field "$BUILDER_ROOT/versions.json" "dependencies.$name.url" | head -1)"
  sha256="$(json_field "$BUILDER_ROOT/versions.json" "dependencies.$name.sha256")"
  local override
  override="$(json_field "$BUILDER_ROOT/versions.json" "dependencies.$name.file" | head -1)"
  if [ -n "${override:-}" ] && [ "$override" != "None" ]; then
    fname="$override"
  else
    fname="$(basename "$url")"
  fi
  mkdir -p "$DOWNLOADS_DIR"
  local dest="$DOWNLOADS_DIR/$fname"

  if [ -f "$dest" ] && verify_sha256 "$sha256" "$dest"; then
    log "using cached $fname"
    echo "$dest"
    return 0
  fi

  local urls=("$url")
  while IFS= read -r mirror; do
    [ -n "$mirror" ] && urls+=("$mirror")
  done < <(json_field "$BUILDER_ROOT/versions.json" "dependencies.$name.mirrors")

  local ok=0 ua
  for u in "${urls[@]}"; do
    log "downloading $name $version from $u"
    ua="$(ua_for_url "$u")"
    local ua_args=()
    [ -n "$ua" ] && ua_args=(-A "$ua")
    # ${arr[@]+...}: bash 3.2 (macOS runners) treats the expansion of an
    # EMPTY array under set -u as an unbound variable; expand conditionally.
    # --retry-all-errors: plain --retry only covers slow/transient failures
    # (timeouts, 5xx); a DNS resolver hiccup, TLS handshake failure, or an
    # instant 4xx fails in milliseconds and used to kill the whole build.
    if curl -fsL ${ua_args[@]+"${ua_args[@]}"} \
        --retry 3 --retry-all-errors --connect-timeout 30 \
        --speed-limit 1024 --speed-time 60 --max-time 900 \
        -o "$dest" "$u"; then ok=1; break; fi
    rm -f "$dest"
  done
  [ "$ok" = "1" ] || die "failed to download $name from any listed URL"

  verify_sha256 "$sha256" "$dest" || die "checksum mismatch for $fname (expected $sha256, got $(sha256_of "$dest"))"
  log "verified $fname"
  echo "$dest"
}

fetch_ffmpeg() {
  local version url sha256 fname
  version="$(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.version")"
  url="$(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.url" | head -1)"
  sha256="$(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.sha256")"
  fname="$(basename "$url")"
  mkdir -p "$DOWNLOADS_DIR"
  local dest="$DOWNLOADS_DIR/$fname"

  if [ -f "$dest" ] && verify_sha256 "$sha256" "$dest"; then
    log "using cached $fname"
    echo "$dest"
    return 0
  fi

  local urls=("$url")
  while IFS= read -r mirror; do
    [ -n "$mirror" ] && urls+=("$mirror")
  done < <(json_field "$BUILDER_ROOT/versions.json" "ffmpeg.mirrors")

  local ok=0
  for u in "${urls[@]}"; do
    log "downloading ffmpeg $version from $u"
    # Stall protection: some mirrors accept the connection then stall the
    # body forever (nasm.us did exactly that from CI networks).
    # --retry-all-errors: see fetch_tarball — instant failures (DNS, TLS,
    # connection refused, HTTP 4xx) must be retried too, not just timeouts.
    if curl -fL --retry 3 --retry-all-errors --connect-timeout 30 \
        --speed-limit 1024 --speed-time 60 --max-time 900 \
        -o "$dest" "$u"; then ok=1; break; fi
    rm -f "$dest"
  done
  [ "$ok" = "1" ] || die "failed to download ffmpeg from any listed URL"

  verify_sha256 "$sha256" "$dest" || die "checksum mismatch for $fname (expected $sha256, got $(sha256_of "$dest"))"
  log "verified $fname"
  echo "$dest"
}

# extract <tarball> [tag] — unpack into SRC_DIR (optionally under a per-target
# subdirectory "tag" so cross builds never reuse an in-tree configure run),
# print the resulting source directory. Handles tarballs whose top-level
# directory name differs from the file name.
extract_tarball() {
  local tarball="$1"
  local tag="${2:-}"
  mkdir -p "$SRC_DIR"
  local dest_root="$SRC_DIR"
  local stamp="$tarball.extracted"
  if [ -n "$tag" ]; then
    dest_root="$SRC_DIR/$tag"
    mkdir -p "$dest_root"
    stamp="$SRC_DIR/$tag/$(basename "$tarball").extracted"
  fi
  if [ -f "$stamp" ] && [ -d "$(cat "$stamp")" ]; then
    cat "$stamp"
    return 0
  fi
  local tmp_marker
  tmp_marker="$(mktemp "$SRC_DIR/.extractXXXXXX")"
  tar -xf "$tarball" -C "$dest_root"
  local top
  # First entry name; `head -1` closes the pipe early (harmless tar noise on
  # some versions), so the pipeline must not be errexit-fatal.
  top="$(tar -tf "$tarball" 2>/dev/null | head -1 | cut -d/ -f1 || true)"
  local dir="$dest_root/$top"
  [ -d "$dir" ] || die "extraction of $tarball produced unexpected layout"
  echo "$dir" > "$stamp"
  rm -f "$tmp_marker"
  echo "$dir"
}

# with_pkg_config <prefix> <command...> — run a command with PKG_CONFIG_PATH
# pointing at <prefix>/lib/pkgconfig (and lib64 fallback).
with_pkg_config() {
  local prefix="$1"; shift
  PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/lib64/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" "$@"
}

# host_has_system <pkg-config-name> — true when the host provides a system lib.
host_has_system() {
  pkg-config --exists "$1" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Build identity (artifact reuse contract)
#
# Every produced artifact records a BUILD-IDENTITY file: a hash over all
# inputs that determine its contents — the pinned versions (versions.json),
# the builder scripts themselves (configure flags live in them), the
# target/ABI/slice, the policy knobs (API level, deployment target, feature
# switches) and the toolchain revision that produced it. Consumers reuse an
# artifact ONLY when the recorded identity matches the identity computed
# from the current tree; anything else rebuilds. This prevents the
# "blindly consume the latest artifact" failure mode.
# ---------------------------------------------------------------------------

# _hash_input_line <label> <value-or-file>
_hash_input_line() {
  printf '%s=' "$1"
  if [ -f "$2" ]; then
    sha256_of "$2"
  else
    printf '%s' "$2"
  fi
  printf '\n'
}

# compute_identity <target> <extra-k=v...> — print the identity hash.
# Extra key=value pairs encode target policy knobs (api=29, deployment=15.0,
# svt=1, ...). Toolchain identity is appended automatically per target:
#   host  -> cc -dumpversion
#   android -> NDK Pkg.Revision from ANDROID_NDK_HOME/ROOT source.properties
#   ios   -> xcodebuild -version (first line)
#
# VELMUX_IDENTITY_DEBUG=1 prints every input line to stderr before hashing:
# an identity mismatch between two computations of the same tree was once
# observed on CI with no way to tell WHICH input diverged (observed
# 2026-11-24: fetch.sh expected f74b3f31..., the builder recorded
# a6e23b0c... seconds later, same checkout); with the dump the diverging
# line (file hash, knob, or toolchain string) is visible in the log.
compute_identity() {
  local target="$1"; shift
  python3 - "$BUILDER_ROOT" "$target" "$@" <<'IDENTITY'
import hashlib, json, os, pathlib, platform, subprocess, sys
builder = pathlib.Path(sys.argv[1])
root = builder.parent.parent
target = sys.argv[2]
inputs = {"schema": 3, "target": target, "policy": sorted(sys.argv[3:])}
# Hash the complete target recipe. An Apple-only correction must not evict
# valid Android/host artifacts; shared helper or version changes affect all.
files = [builder / "lib.sh", builder / "versions.json", builder / "validate.py",
         builder / "patch-aiff-layouts.py",
         builder / f"build-{target}.sh",
         root / "scripts/ffmpeg/fetch.sh", root / "scripts/ffmpeg/identity.sh",
         root / "scripts/lib/common.sh", root / "scripts/lib/config.sh"]
if target == "host":
    files += [builder / "probe/velmux-ffmpeg-probe.c", root / "scripts/ffmpeg/build-linux.sh"]
elif target in {"android", "ios"}:
    files += [builder / "manifest-from-config.py", root / f"scripts/ffmpeg/build-{target}.sh"]
    if target == "ios":
        files += [builder / "package-ios-xcframework.sh"]
else:
    raise SystemExit("unknown identity target: " + target)
inputs["sources"] = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted(files)}
# In-process source builds prepend generated cross wrappers and pinned
# host tools to PATH. They must not change the external toolchain identity
# between slices; their source identities are already included above.
tool_environment = os.environ.copy()
generated = [pathlib.Path(os.environ.get("VELMUX_OUT_DIR", str(builder / "out"))).resolve(),
             pathlib.Path(os.environ.get("VELMUX_SRC_DIR", str(builder / "src"))).resolve()]
def external_path(value):
    resolved = pathlib.Path(value).resolve()
    return not any(resolved.is_relative_to(path) for path in generated)
tool_environment["PATH"] = os.pathsep.join(p for p in os.get_exec_path() if external_path(p))
def command(args):
    try:
        return subprocess.check_output(args, stderr=subprocess.STDOUT, text=True,
                                       env=tool_environment).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise SystemExit("cannot establish native toolchain identity: " + repr(args)) from exc
inputs["buildHost"] = {"os": platform.system(), "arch": platform.machine()}
inputs["buildTools"] = {name: command([name, "--version"]).splitlines()[0]
                       for name in ("cmake", "ninja", "meson", "pkg-config")}
# Assemblers and source-generation tools also affect the archive contents.
# Missing optional tools have explicit identity markers; builders retain
# their pinned source fallbacks where provided.
import shutil
inputs["optionalBuildTools"] = {
    name: command([name, "--version"]).splitlines()[0] if shutil.which(name, path=tool_environment["PATH"]) else "absent"
    for name in ("nasm", "autoconf", "automake", "libtoolize", "glibtoolize")
}
if target == "host":
    inputs["compiler"] = command([os.environ.get("CC", "cc"), "--version"])
    inputs["cxxCompiler"] = command([os.environ.get("CXX", "c++"), "--version"])
    if platform.system() == "Linux":
        inputs["libc"] = command(["getconf", "GNU_LIBC_VERSION"])
    zlib_environment = tool_environment.copy()
    # Query the system provider, not an already generated native prefix.
    zlib_environment.pop("PKG_CONFIG_PATH", None)
    zlib_environment.pop("PKG_CONFIG_LIBDIR", None)
    zlib = subprocess.run(["pkg-config", "--modversion", "zlib"],
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                          text=True, env=zlib_environment)
    inputs["hostZlib"] = zlib.stdout.strip() if zlib.returncode == 0 else "pinned-source-fallback"
    if zlib.returncode == 0:
        libdir = subprocess.check_output(["pkg-config", "--variable=libdir", "zlib"],
                                        text=True, env=zlib_environment).strip()
        libraries = sorted(pathlib.Path(libdir).glob("libz.*"))
        inputs["hostZlibLibraries"] = {
            path.name: hashlib.sha256(path.read_bytes()).hexdigest()
            for path in libraries if path.is_file()
        }
elif target == "android":
    ndk = os.environ.get("ANDROID_NDK_HOME", os.environ.get("ANDROID_NDK_ROOT", ""))
    properties = pathlib.Path(ndk) / "source.properties"
    if not ndk or not properties.is_file():
        raise SystemExit("ANDROID_NDK_HOME must identify the selected NDK before computing identity")
    inputs["ndk"] = properties.read_text().strip()
elif target == "ios":
    inputs["xcode"] = command(["xcodebuild", "-version"])
    inputs["sdk"] = {sdk: command(["xcrun", "--sdk", sdk, "--show-sdk-version"])
                     for sdk in ("iphoneos", "iphonesimulator")}
else:
    raise SystemExit("unknown identity target: " + target)
serialized = json.dumps(inputs, sort_keys=True, separators=(",", ":"))
if os.environ.get("VELMUX_IDENTITY_DEBUG") == "1":
    print(serialized, file=sys.stderr)
print(hashlib.sha256(serialized.encode()).hexdigest())
IDENTITY
}

# identity_recorded <out-dir> — the hash recorded in BUILD-IDENTITY (empty
# when absent or unreadable).
identity_recorded() {
  local f="$1/BUILD-IDENTITY"
  if [ -f "$f" ]; then
    head -1 "$f" 2>/dev/null || true
  fi
}

# identity_matches <out-dir> <expected-hash>
identity_matches() {
  [ "$(identity_recorded "$1")" = "$2" ]
}

# write_identity <out-dir> <hash> <description> — records the identity plus
# a human-readable provenance block for debugging.
write_identity() {
  local dir="$1" hash="$2" desc="$3"
  mkdir -p "$dir"
  {
    printf '%s\n' "$hash"
    printf 'built: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || true)"
    printf '%s\n' "$desc"
  } > "$dir/BUILD-IDENTITY"
}

# fixup_pkgconfig_prefix <dir> — make restored artifacts position-independent:
# FFmpeg's generated .pc files bake the build-time prefix into absolute
# paths; rewrite prefix= to the directory the artifact actually lives in
# now. Everything else in the .pc files is ${prefix}-relative.
#
# Symlink-safe and portable (macOS BSD tools + GNU): some pins install .pc
# names as SYMLINKS (libpng 1.6.46's cmake install ships libpng.pc ->
# libpng16.pc). BSD sed refuses in-place editing of symlinks ("in-place
# editing only works for regular files"), and GNU sed -i would silently
# replace the link while leaving the target stale — desynchronizing the
# two names. Instead: read through the link, rewrite into a temp file and
# atomically move it over the ENTRY — the result is a self-contained
# regular .pc with the correct prefix under every name, on every platform.
fixup_pkgconfig_prefix() {
  local dir="$1" pc
  [ -d "$dir" ] || return 0
  local abs
  abs="$(cd "$dir" && pwd)"
  for pc in "$dir"/lib/pkgconfig/*.pc "$dir"/lib64/pkgconfig/*.pc; do
    [ -e "$pc" ] || continue
    # Resolve one level of symlink portably (no readlink -f: macOS had it
    # only from 12.3). Loop guards against pathological chains.
    local target="$pc" guard
    guard=0
    while [ -L "$target" ] && [ "$guard" -lt 8 ]; do
      local link
      link="$(readlink "$target")"
      case "$link" in
        /*) target="$link" ;;
        *) target="$(dirname "$target")/$link" ;;
      esac
      guard=$((guard + 1))
    done
    [ -f "$target" ] || continue
    sed "s|^prefix=.*|prefix=$abs|" "$target" > "$pc.tmp" \
      && mv -f "$pc.tmp" "$pc" \
      || { rm -f "$pc.tmp"; die "fixup_pkgconfig_prefix: failed to rewrite $pc"; }
  done
}
