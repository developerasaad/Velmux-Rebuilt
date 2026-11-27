# Progress

## Authorization

Milestone 1 — Engineering foundation is in progress. Milestones 2–7 remain pending and unauthorized. No processing tools are presented as complete.

## Verified foundation

- Independent history on main, origin developerasaad/Velmux-Rebuilt, repository-local author/committer developerasaad <developer.asaad@gmail.com>. Push, Actions inspection and dispatch work independently. Original Velmux is read-only reference.
- REQUIREMENTS.md matches the full supplied specification byte-for-byte. The required responsibility layout, identity and palette are established; see IMPLEMENTATION.md and docs/reference-findings.md.
- Pinned FFmpeg 7.1.5 archive digest and official detached signature checked. Complete authorized construction closure retained, including cross-toolchain fixes; no original application implementation imported.
- Target-specific native identity isolation, shared-input invalidation and generated-PATH/tool-version tests pass locally and in CI. Public build/test entry points require the selected clean compiler environment; Android commands require Bash 5.

| Evidence | Source | Inspected result |
| --- | --- | --- |
| [Source checks 45931075236](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45931075236) | `662af3d` | Passed script parsing, nine cache-identity regressions, Rust formatting and custom Gradle configuration |
| [Native dependencies 45927805349](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45927805349) | `ef805cc` | Android arm64-v8a/x86_64 and Apple arm64 device/simulator construction and compiled-component validation passed; host C probe also passed |
| [Host integration 45930002333](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45930002333) | `c16faf5` | Corrected host SVT recipe constructed and saved; four real Rust/FFmpeg integration tests and Clippy passed |
| [Android integration 45930005832](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45930005832) | `c16faf5` | Generated bindings, release/debug builds, both ABI closures and ELF/APK 16 KiB alignment passed; API 29 process crashed before tests |
| [Apple integration 45929262928](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45929262928) | `c9aab29` | Rust/Kotlin static framework and Swift host compiled; final app link failed on libass iconv dependency |

The host Rust tests demonstrate linked FFmpeg registries, record values, native-provider calls, shared engine lifetime, typed cancellation/access errors and concurrent cursor isolation. Native build-tree manifests are not physical hardware or actual accelerated-processing evidence.

## Implemented, pending mobile runtime acceptance

- Gobley/UniFFI bindings, one process-owned engine per native host, shared Compose diagnostics shell, custom source roots and native storage providers.
- Android lifecycle recreation, native callbacks, typed errors and cancellation instrumentation tests; Swift worker-thread/provider/error/host tests.
- Apple libass iconv system dependency is now explicitly linked. Apple compiler/dependency caches and Android tombstones/symbolication are retained for focused diagnosis.

## Active verification and defects

- Android [45931078568](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45931078568), `662af3d`: running with full native-crash evidence capture. Earlier logcat confirms SIGSEGV during Application startup before test execution. Cause remains under investigation; no successful Android runtime claim.
- Apple [45931075511](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45931075511), `662af3d`: running corrected device-host linkage, followed by simulator integration.
- Host GCC IPO was incompatible with Rust native-archive bundling. SVT host LTO is explicitly disabled; the successful test run establishes this correction. Mobile recipes are unchanged by that fix.
- Earlier failures corrected: hosted default NDK mismatch, missing bindgen opaque declarations, Apple x265/zimg C++ closure, activity-owned engine lifetime and unannotated Swift error propagation. A test-directory collision with the build ignore rule was corrected; source checks now pass.
- No external access/runner blocker currently prevents work. Remaining failures are integration defects to resolve within Milestone 1.

## Current corrections awaiting runtime evidence

- [Native symbolication 45932734608](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45932734608), `95c440d`, traced the startup fault to `__bionic_getauxval` / `page_size` from an x265 global constructor. The core ELF had no dynamic libc dependency: an explicit generic NDK library search path selected static platform archives ahead of API-specific shared stubs. Rust now bundles only genuine managed-prefix archives, keeps platform libraries dynamic, normalizes Android C++ runtime imports, and lets the NDK driver preserve API-first SDK library lookup. The release APK gate checks libc dependency and rejects bundled Bionic symbols. Independent source review completed; execution proof pending.
- Apple [45931075511](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45931075511), `662af3d`: unsigned device app build passed. Simulator compilation attempted unsupported x86_64 alongside arm64; the project now explicitly selects arm64, matching the Kotlin/Rust/native artifacts, and selects an SDK-matched runtime. Actual simulator tests remain pending.
- Gobley 0.3.7 is retained. An upstream JNA regression was investigated, but symbolication identified platform-library bundling rather than evidence for changing the binding generator.

- [Independent native link probes 45935213040](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45935213040), `74d2362`, reproduced the NDK path defect with tiny Clang shared-library builds: default/API-first lookup uses shared libc; explicitly prioritizing the generic ABI directory pulls static Bionic. The broad SDK search path has been removed.
- iOS job in [45934049976](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45934049976), `22f65bd`, passed the device build and all five simulator interoperability tests. Inspection also found a Compose plist sanity exception despite green XCTest status. The required high-refresh plist key and a stronger visible-controller launch assertion are added; the shell now rejects uncaught Kotlin exceptions in the simulator log.

## Acceptance remaining

Demonstrate real Android and iOS host-to-Rust-to-FFmpeg calls and provider callbacks at runtime, cancellation/errors/lifetime behavior, Android API 29/newer launch and iOS simulator launch, plus successful unsigned Apple device-target linkage. Inspect final workflow evidence and review the integrated result before accepting Milestone 1.

Physical devices are unavailable. Hardware-specific behavior, accelerated processing, media correctness and performance are unverified. The inherited x265 build does not establish 10-bit HEVC encoding. Product catalog, SQLite operational state and processing workflows belong to subsequent authorized milestones.

## Next authorized action

Resolve the Android native startup crash, complete Apple linking/simulator verification, and finish the Milestone 1 acceptance gate. Do not begin Milestone 2.
