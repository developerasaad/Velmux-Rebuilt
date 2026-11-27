# Progress

## Authorization and status

Milestone 1 — Engineering foundation is complete and accepted against its scoped gate on 27 November 2026. Milestone 2 is explicitly authorized and in progress. Milestones 3–7 remain pending and unauthorized. No conversion workflows are available or presented as complete.

## Verified foundation

- Independent history on main, origin developerasaad/Velmux-Rebuilt, repository-local author/committer developerasaad <developer.asaad@gmail.com>. Push, Actions inspection and dispatch work. Original Velmux remains read-only reference.
- REQUIREMENTS.md matches the complete supplied specification byte-for-byte. The required physical layout, application identity and shared palette are established; see IMPLEMENTATION.md and docs/reference-findings.md.
- FFmpeg 7.1.5 source digest and official detached signature checked. Complete authorized construction closure retained, including cross-toolchain fixes; no original application implementation imported.
- Target-specific native identities cover source, flags, architectures and toolchains. Nine regression assertions exercise isolation and invalidation. Ordinary application changes reuse native dependencies.
- Generated Gobley/UniFFI interoperability connects the shared Compose shell to direct linked FFmpeg libraries. Android Application and iOS AppDelegate each own one process-lifetime session; screen disposal does not close it. Native Kotlin/Swift providers supply real private-storage paths and callbacks.

| Evidence | Source | Inspected result |
| --- | --- | --- |
| [Source checks 45935873278](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45935873278) | `de58d8f` | Script parsing, nine identity regressions, Rust formatting and custom Gradle configuration passed |
| [Native dependencies 45927805349](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45927805349) | `ef805cc` | Android arm64-v8a/x86_64 and Apple arm64 device/simulator construction and compiled-component validation passed |
| [Host integration job](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45934049976/job/140293050933) | `22f65bd` | Four real Rust/FFmpeg integration tests and Clippy passed after the explicit native-link policy was introduced |
| [Android integration 45935435915](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45935435915) | `dd87364` | Release build, both ABI closures, ELF/APK 16 KiB alignment, platform libc/shared C++ checks and all three instrumentation tests on each of API 29 and API 35 passed |
| [Apple integration 45935875430](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45935875430) | `de58d8f` | Unsigned arm64 device build and all five arm64 iOS 18.5 simulator tests passed; visible-controller assertions and uncaught-Kotlin-exception gate passed |

The host tests establish real linked FFmpeg registries, record values, native-provider calls, shared engine lifetime, typed pre-call/mid-callback cancellation, invalid-storage errors and concurrent cursor isolation. Android tests establish native host launch/activity recreation with the same application session, actual FFmpeg calls, callbacks, basic type conversion, typed cancellation/storage errors and idempotent close. Both API-level test executions are present in the inspected job log; the retained XML is the later API 35 execution.

The inspected Apple log and retained XCTest result establish visible shared-controller launch using the AppDelegate session, real Swift-to-Kotlin-to-Rust-to-FFmpeg calls, provider callbacks on a worker thread, typed cancellation/storage errors, catchable closed-session errors and idempotent close. All five tests passed with zero failures; no uncaught Kotlin exception was logged.

The host result remains applicable: subsequent build-script changes only alter Android SDK search paths; the latest source change concerns Apple launch verification. No unchanged expensive host rebuild was required. Independent final source review found no new consequential ownership, callback or cancellation defect.

## Corrected integration defects

- Host SVT-AV1 GCC IPO/LTO archives lost usable symbols during Rust archive bundling. Host LTO is disabled; normal archives pass the Rust suite. Mobile recipes retain their verified configuration.
- Android startup initially crashed before instrumentation. [Symbolication](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45932734608) traced an x265 constructor through page-size initialization into bundled Bionic libc. [Independent Clang probes](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45935213040) reproduced the cause: a generic NDK library search path selected static SDK archives ahead of API-specific shared stubs. Only managed FFmpeg-prefix archives are now bundled, SDK lookup follows the NDK driver's API-first ordering, and exactly one shared C++ runtime is packaged. The APK gate checks dynamic libc and rejects bundled Bionic symbols; both emulator runs now pass.
- Apple x265/zimg C++ and libass iconv closure, arm64-only simulator architecture selection and SDK-matched simulator selection were corrected. Swift error annotations keep closed-session/native failures catchable. The first green XCTest run still logged a Compose plist sanity exception; final acceptance therefore additionally checks the required plist key, visible controller and absence of uncaught Kotlin exceptions.
- Bindgen opaque declarations, pinned NDK resolution, process-owned engine lifetime, test-directory ignore collision and source-only compiler-cache identity were corrected and verified by subsequent relevant checks.

## Acceptance decision

The repository/identity/history, durable records, custom layout, pinned compatible toolchains, native construction/packaging, both mobile links, real host calls/callbacks and relevant host/emulator/simulator checks satisfy the Milestone 1 gate. Logs and test artifacts were inspected rather than relying only on workflow status. Final documentation-only changes do not alter the verified binaries. No unresolved blocker remains within this milestone. This Milestone 1 acceptance remains the regression baseline for the now-authorized Milestone 2 work.

## Limits and next boundary

Physical devices are unavailable. Emulator/simulator results do not establish physical hardware behavior, accelerated processing, media correctness or performance. Native component validation proves compiled capability, not successful hardware execution. The inherited x265 build does not establish 10-bit HEVC encoding. Android runtime evidence covers x86_64 emulators; arm64 packaging/linking is verified, with physical execution pending. Apple simulator evidence targets arm64 iOS 18.5; minimum-OS device runtime is not claimed.

Product catalogs, SQLite operational state and actual media workflows belong to later milestones. A maintained Rust-owned SQLite binding with bundled SQLite is planned when the durable store is introduced. Milestone 2 is now authorized; do not begin Milestone 3 without explicit authorization.

## Milestone 2 integration

Authorization is limited to Milestone 2. Implementing: descriptor-backed media access and inspection, Rust catalog/configuration, direct library conversion, SQLite jobs, native publication and playback adapters. These new paths are not yet verified. Existing Milestone 1 tests remain enabled.

The first integrated unit introduces source/OS contracts and a real processing/store implementation. Focused host compilation/tests precede application wiring. Android and iOS adapters have source checks only; native picker interaction, player behavior and publication still require runtime evidence. Remaining M2 work includes shared application/workspaces/Jobs/Settings, preview fallback, independent media verification, cancellation/lifecycle/restart failure coverage and complete cross-platform acceptance. No Milestone 3 functionality is authorized.

### Current Milestone 2 verification

- `38296d0` introduced descriptor leases, inspection, SQLite scheduling and native adapters. Its first host build found an explicit raw-pointer reference requirement; corrected in `c823fbe`.
- `c823fbe` connected Home/converters/Jobs/Settings and lazy preview processing. [Source checks 45957182516](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45957182516) passed on attempt 2 after a transient plugin-repository resolution failure.
- Host job in [45957183609](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45957183609) passed nine internal tests and 12 of 14 new real-media Engine cases. Remaining failures concern rotation inspection and positive-start Matroska duration. The latter is corrected using actual demuxer header timing semantics; execution proof is pending. No failing assertion was removed.
- Android in that run failed expanded FFmpeg binding compilation on target-specific C character signedness; buffers now use the target C character type. Apple verification is still running. No mobile Milestone 2 runtime acceptance is claimed.
- Additional native converter/picker/playback tests and independent preview verification are implemented but pending execution. Generated fixture binaries and independent probe facts are retained as CI evidence. The local repository-specific Rust override remains incomplete; no local mobile/native build is used as acceptance evidence.

Still required: correct observed failures, obtain independent full media verification, execute both mobile converter integrations and native UI/picker/player tests, inspect real screen captures, and review every Milestone 2 acceptance item. Milestone 2 is not complete.

### Continued verification and corrections

- `515b69b` / [45963364244](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45963364244): host inspection and all sixteen real-media Engine cases passed, as did the four M1 integration tests. Independent verification caught a zero-duration last CFR video packet. The generated rotation fixture now has a display matrix confirmed by the independent system probe; no inspection assertion was weakened.
- `f5cfb46` / [45963825389](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45963825389): the first duration correction alone still failed independent frame-rate verification. Inspection of actual MP4 sample timing found 44 regular samples and one zero-duration last sample. The FFmpeg encoder requires AV_CODEC_FLAG_FRAME_DURATION to preserve correctly rescaled frame durations; `f6b785b` adds that flag and is undergoing focused host verification in [45964275365](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45964275365).
- Android `a152085` release packaging passed; instrumented-test compilation found an unsupported UiAutomator selector call, corrected in `515b69b`. Current Android runtime verification is pending. iOS `a152085` reached Swift host compilation and exposed nullable throwing Objective-C return ambiguity plus Unit block signatures. Publication status now uses a non-null native wrapper, and Swift callbacks use Void; execution verification remains pending.
- Source-aware catalogs omit incompatible Copy routes and unavailable HDR-to-H.264 choices. Equal sampling/layout/size settings no longer require unnecessary encoding; explicit channel-layout transformations are honored. Portable filenames are validated before accepting a job. New direct-core assertions cover compatibility and filenames.
- Milestone 2 remains in progress. Final acceptance still requires independent media verification, both mobile runtimes, inspected UI/picker/player evidence and review of the complete gate.
