# Native dependency infrastructure provenance

The files under `scripts/ffmpeg-builder/` and the FFmpeg resolver/wrappers
under `scripts/ffmpeg/` originate from the read-only engineering reference
[developerasaad/Velmux](https://github.com/developerasaad/Velmux/tree/5971e648c303dd9f044e6c5ac67c852fe46e886f),
revision `5971e648c303dd9f044e6c5ac67c852fe46e886f`. The required shared build
helpers were retained under `scripts/lib/`; unrelated application, release,
CMake engine, platform and test implementations were not imported.

`versions.json` is the complete upstream source/version/SHA-256 manifest.
It covers FFmpeg 7.1.5, x264, x265, dav1d, SVT-AV1, libvpx, LAME, Opus,
Ogg/Vorbis, libass, FreeType, HarfBuzz, FriBidi, zlib, libpng, libwebp,
zimg, and build-time NASM/libtool. SQLite belongs to the Rust dependency
graph and is not part of this pipeline. Source archives, their embedded
license/notice files, unpacked source and build products remain generated
artifacts, outside source control. Preserve upstream notices when packaging
these libraries. Source digests are inherited pins; the original manifest
explicitly records their initial trust-on-first-use origin.

The pipeline enables GPL components required by x264/x265 and does not enable
nonfree dependencies. This provenance record makes no decision about the
application license and grants no release or distribution authority.

## Retained engineering fixes

- Separate per-target source/build trees and hermetic cross pkg-config paths.
- x265 CMake 4 compatibility and malformed NDK unwind-library pc repair.
- Android libm/sharpyuv static closure fixes and zlib pkg-config generation.
- Correct libvpx assembler/toolchain selection; removal of obsolete Android
  static executable link assumptions; Apple simulator SDK reassertion.
- Native Meson compiler isolation, complete Apple deployment-variable
  sanitation, and host-only libtool fallback with Darwin glibtoolize alias.
- Pinned mirror/checksum handling, bounded download retries, useful failure
  diagnostics, portable prefix relocation including pkg-config symlinks.

Relevant reference commits are `76ecde7` (Android static probe closure),
`932824e` (C++ runtime packaging and Apple environment isolation), `314d874`
(host libtool isolation), and `fea0a05` (Darwin libtool alias/diagnostics).
The new Android host must independently package the NDK C++ runtime required
by its actual Rust/FFmpeg link; copying scripts does not prove APK correctness.

## Greenfield adaptations and verification boundaries

The resolver uses NDK `27.2.12479018`, Android API 29 and iOS 15.0. Only
Android arm64-v8a and x86_64 are selected by project policy. Apple construction
provides arm64 device and simulator slices. Direct commands are:

```sh
bash scripts/ffmpeg/fetch.sh host
bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
bash scripts/ffmpeg/build-ios.sh
```

Identity matching covers the complete script/manifest/probe/validator closure,
policy arguments, build tool versions, build host, exact NDK revision or full
Xcode/SDK versions, and the host compiler/libc where applicable. Install build
tools and resolve the selected NDK before calculating CI cache identities.
Host libraries are explicitly position independent for linkage into Rust
shared libraries. Artifacts require the complete static library closure.

The runtime probe now enumerates actual libavfilter filters and supported
hardware device API types. Cross manifests enumerate generated compiled
component/filter lists and identify that evidence as build-time evidence.
Validation requires real filter entries; configure flags alone cannot establish
filter availability. Runtime mobile enumeration and real processing are still
necessary. Compiled hardware API support does not establish physical device
availability or successful accelerated processing.

The inherited x265 build does not establish 10-bit HEVC encoding. HE-AAC/AMR
encoding and every container/codec pairing also require explicit later catalog
verification. Android explicitly enables FFmpeg JNI/MediaCodec and Apple explicitly enables
VideoToolbox, matching the selected release configure dependencies. Target
compilation, runtime capability checks and real native processing must still
be verified before claiming acceleration.

## FFmpeg maintenance update

The inherited FFmpeg 7.1.1 pin was advanced within the same stable branch to
7.1.5, listed by the [official downloads page](https://ffmpeg.org/download.html)
as the current 7.1 maintenance release (2026-06-20). This keeps the tested major
API family while incorporating upstream maintenance fixes. The exact official
archive SHA-256 is
`de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f`.
The official detached release signature was verified with OpenSSL against the
RSA signing key fingerprint published on that page:
`FCF986EA15E6E293A5644F10B4322F04D67658D8`. Packet extraction was performed
in an ignored, one-time verification script; OpenSSL performed the actual
RSA/SHA-256 signature verification. No verification helper or source archive
is part of the application. All other dependency pins remain unchanged.

The release archive's own `configure` confirms Android MediaCodec depends on
`android` and `mediandk`; it probes `ANativeWindow_acquire` with `-landroid` and
`AMediaFormat_new` with `-lmediandk`. JNI uses the NDK's `jni.h` and pthreads.
The Apple branch explicitly enables VideoToolbox and probes the actual SDK
framework. These flags enable compiled API support and do not establish
physical hardware availability.

## Apple static link closure correction

The first new Apple CI build exposed missing C++ runtime declarations in x265's cross-generated pkg-config file. The Apple builder explicitly includes `-lc++`, and zimg uses `STL_LIBS=-lc++ -lm` rather than its GNU libstdc++ default. Retained-prefix repair is idempotent and runs outside installation guards. This correction is Apple-specific; previously verified Android/host artifacts remain identified by their original source revisions, without relabeling their cache identities.

Artifact identity schema 3 covers the complete target-specific construction closure and shared helpers. It excludes unrelated target builders, includes both host C/C++ compiler versions, and intentionally invalidates initial schema-2 artifacts once. Old artifacts are not relabeled. Native caches are saved immediately after construction, before independent application checks.

Supported construction uses the selected clean CI compiler environment, with no inherited CC/CXX/CFLAGS/CXXFLAGS/CPPFLAGS/LDFLAGS overrides. Public build/test entry points and CI setup reject those overrides; custom native compiler changes belong in the hashed recipe. Android developer commands require Bash 5: Apple's system Bash 3.2 misparses the inherited nested command substitution used for the SVT identity policy. Apple CI construction uses its verified shell path; Android construction is verified on Ubuntu/Bash 5.
