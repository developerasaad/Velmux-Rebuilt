# Velmux

An offline Android and iOS media utility under greenfield development. The currently authorized work is the engineering foundation; no conversion workflows are available yet.

Rust owns the engine, Compose Multiplatform shares the UI, and Kotlin/Swift hosts provide operating-system facilities. See REQUIREMENTS.md for the complete product scope, IMPLEMENTATION.md for decisions, and PROGRESS.md for actual verification evidence.

Substantial builds run in GitHub Actions. Native libraries are constructed from pinned, checksum-verified sources; generated bindings and build products are excluded from Git. No releases are published by this repository.

## Development commands

Substantial commands below are intended for CI or a deliberately provisioned build machine. Use JDK 21, Rust 1.99.1, Python 3, the pinned Android NDK, and Xcode 16.4 on a real Mac for Apple targets. CMake 3.31.6, Ninja 1.11.1.3, Meson 1.7.2, pkg-config, NASM and GNU autotools are required for native construction. `scripts/ci/setup-native.sh` provisions disposable CI workers only.

```sh
bash scripts/ci/check-sources.sh
bash scripts/test.sh host
bash scripts/build.sh android
bash scripts/test.sh android          # requires a connected supported emulator/device
bash scripts/build.sh ios-device      # unsigned compilation, not distribution
bash scripts/build.sh ios-simulator
bash scripts/test.sh ios-simulator
```

The Native dependencies workflow constructs identity-keyed artifacts. Foundation integration reuses those exact artifacts and verifies Rust, Android APK packaging, emulator integration, Apple device compilation and simulator execution. Source checks run automatically; integration dispatch is explicit during foundation bring-up. Workflow files alone are not acceptance evidence; see PROGRESS.md for inspected runs.

The physical source roots are `core/`, `UI/src/`, `UI/integration/`, `platforms/android/` and `platforms/ios/`. Swift calls the exported shared controller/session; that same session owns the Rust engine reached through Gobley-generated Kotlin bindings. Android's Application and iOS's AppDelegate own the process-lifetime session. Screens do not own engine shutdown.

Build products remain under ignored `build/`, module `build/`, `core/target/` and the FFmpeg builder's generated directories. Upstream dependency sources/checksums and reuse provenance are recorded in `scripts/ffmpeg-builder/versions.json` and `third_party/ffmpeg-provenance.md`. GPL components are enabled; nonfree components are excluded. No application licensing or distribution decision is made here.
