# Velmux

An offline Android and iOS media utility under active development. Milestones 1 and 2 are verified. Basic Video/Audio Converter workflows, real previews, persistent SQLite jobs and verified native publication work end to end; see PROGRESS.md for inspected CI evidence and physical-device/hardware-testing limitations. Milestone 3 is explicitly authorized and in progress; Milestone 4 is not authorized.

Rust owns the engine and operational policy. The authorized Milestone 3 migration uses independent Kotlin/Jetpack Compose/Material 3 Android and Swift/SwiftUI iOS interfaces with direct generated UniFFI Kotlin/Swift bindings. Native providers retain operating-system facilities. Historical M1/M2 evidence predates this migration. Final native Android and direct SwiftUI layout/build/runtime gates passed at 6a6f8fe. Subsequent converter changes require their affected runtime checks. See REQUIREMENTS.md for the complete product scope, IMPLEMENTATION.md for decisions, and PROGRESS.md for actual verification evidence.

Substantial builds run in GitHub Actions. Native libraries are constructed from pinned, checksum-verified sources; generated bindings and build products are excluded from Git. No releases are published by this repository.

## Development commands

Substantial commands below are intended for CI or a deliberately provisioned build machine. Use Bash 5 for Android commands, JDK 21, Rust 1.99.1, Python 3.9 or newer, the pinned Android NDK, and Xcode 16.4 on a real Mac for Apple targets. Build entry points require unset CC/CXX/CFLAGS/CXXFLAGS/CPPFLAGS/LDFLAGS; native compiler changes belong in the hashed repository recipe. Direct dependency wrappers have the same clean-environment requirement. CMake 3.31.6, Ninja 1.11.1.3, Meson 1.7.2, pkg-config, NASM and GNU autotools are required for native construction. `scripts/ci/setup-native.sh` provisions disposable CI workers only.

```sh
bash scripts/ci/check-sources.sh
bash scripts/test.sh host
bash scripts/build.sh android
bash scripts/test.sh android          # requires a connected supported emulator/device
bash scripts/build.sh ios-device      # unsigned compilation, not distribution
bash scripts/build.sh ios-simulator
bash scripts/test.sh ios-simulator
```

Apple entrypoints use `scripts/ci/apple-native-app.sh` and `scripts/ci/verify-native-ios.sh`. Foundation integration also executes `scripts/ci/verify-swift-interop.sh` against the actual device/simulator archives, preserving direct binding and native provider lifetime coverage. These commands run on provisioned macOS CI.

The Native dependencies workflow constructs identity-keyed artifacts. Foundation integration reuses those exact artifacts and verifies Rust, Android APK packaging, emulator integration, Apple device compilation and simulator execution. Source checks run automatically; integration dispatch is explicit during foundation bring-up. Workflow files alone are not acceptance evidence; see PROGRESS.md for inspected runs.

The final physical source roots are `core/`, `android/` and `ios/`, with lowercase native `src/ui/` directories. The obsolete shared UI and Kotlin Apple bridge have been retired after the functional native replacement passed its runtime/media gate. Final source and build-path regressions passed at 6a6f8fe; later M3 changes remain subject to renewed verification. Each native application session owns one Rust engine for the process; screens do not own shutdown. Preserve in-progress Rust work and user media through this transition. See IMPLEMENTATION.md for sequencing and PROGRESS.md for actual state.

Velmux has proprietary commercialization intent. The development repository remains public for now for GitHub Actions allowances; this is not an open-source product or an application distribution channel. No project license, commercial licensing arrangement, release or store preparation is authorized.

Build products remain under ignored `build/`, module `build/`, `core/target/` and the FFmpeg builder's generated directories. Upstream dependency sources/checksums and reuse provenance are recorded in `scripts/ffmpeg-builder/versions.json` and `third_party/ffmpeg-provenance.md`. GPL components are enabled; nonfree components are excluded. Third-party licensing information remains unchanged; commercial distribution compatibility decisions are deferred to the owner.
