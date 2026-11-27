# Implementation sequence

The full product and acceptance requirements are in REQUIREMENTS.md. Authorization currently ends at Milestone 2.

Milestone 1 is complete; its inspected source, host, Android and Apple evidence is recorded in PROGRESS.md. Milestone 2 is in progress under explicit authorization; Milestones 3–7 remain pending.

| Milestone | Dependency | Acceptance boundary |
| --- | --- | --- |
| 1. Engineering foundation | Independent repository and reference audit | Pinned native dependencies, custom layout, generated interop, real FFmpeg/mobile calls and callbacks, host/mobile CI evidence |
| 2. End-to-end foundation | Verified M1 | Real video/audio conversion from native selection to verified publication, shared configuration, previews, jobs and storage ownership |
| 3. Complete converters | Verified M2 | Every converter option, compression/target size, metadata, audio/subtitle behavior and truthful estimates independently verified |
| 4. Graphical manipulation | Verified M3 | Physical crop, precise trim/cut/split/merge, real gestures/thumbnails/waveforms and content verification |
| 5. Extraction and presets | Verified M4 | All extraction methods, exact frame/track identity, portable presets and partial multi-output publication |
| 6. Reliability and refinement | Verified M5 | Actual retained-work recovery, lifecycle/publication/cleanup failures, accessibility and measured performance |
| 7. Independent acceptance | Verified M6 | Requirement audit, corrected defects, realistic media/state/UI tests and clean CI reproduction |

## Foundation decisions

- Preserve com.velmux.mobile on Android/iOS, Android API 29 and iOS 15.0 from reference identity. The selected Compose version supports retaining that floor; target compilation remains the acceptance check.
- Use Kotlin 2.2.20, Compose 1.9.3, AGP 8.10.1, Gradle 8.14.3, JDK 21, Rust 1.99.1, Gobley 0.3.7 and UniFFI 0.29.4 as an explicitly pinned integration candidate. Compatibility is a verification gate, not a claim inferred from version numbers. Android NDK 27.2.12479018; Apple builds select Xcode 16.4.
- Gobley generates common Kotlin bindings plus JVM/Native implementations. Swift hosts the exported Compose controller and supplies native services through a small Kotlin-exported interface. It does not link a second Rust library or create a second engine. Swift-to-Rust interoperability therefore traverses the same generated Kotlin binding instance.
- Generate a narrow FFmpeg function allowlist from the actual target headers using bindgen; link the complete static pkg-config closure. This avoids handwritten ABI declarations and confines unsafe registry iteration to core/src/ffmpeg. Expand safe resource ownership only when real processing requires it. Target sysroots are supplied explicitly; Android uses one shared NDK C++ runtime packaged by Gobley, with ELF alignment checked in the actual APK.
- Rust native linking uses pkg-config only for dependency discovery. Only real archives inside the managed FFmpeg prefix are bundled; platform SDK libraries stay dynamic. No explicit SDK link-search directories override the NDK driver’s API-first lookup. These boundaries prevent platform libc globals from being duplicated inside the engine. Android uses exactly one shared C++ runtime.
- Foundation diagnostics exercise direct linked library registries, provider callbacks, records, errors, cancellation and worker-thread calls. They do not constitute the product media catalog or pretend to perform conversion.
- Host SVT-AV1 disables GCC IPO/LTO: Rust archive bundling did not preserve usable GCC LTO symbols. Normal machine-code archives pass the real host integration suite; mobile Clang recipes retain their validated configuration.
- Android initially targets arm64-v8a devices and x86_64 emulators. Apple initially targets arm64 devices and arm64 simulators. Other architectures are not claimed.
- Preserve the reference FFmpeg dependency construction closure and inline cross-compilation fixes, with exact source provenance. Improve cache identity and verify target registries. Do not carry over old application transport or C++ processing.
- Future configuration uses deterministic Rust catalog rules over already inspected sources; full execution planning belongs at submission. No mutable remote-style configuration sessions or global cache revisions.
- Future recovery validates actual retained artifacts and preserves manual pause intent. Cancellation must acknowledge worker termination before resource deletion. Publication and transient alerts remain distinct from durable job semantics.

## Primary references

- [Gobley Android/iOS integration](https://gobley.dev/docs/tutorial/) and [Cargo configuration](https://gobley.dev/docs/gradle-plugins/cargo/)
- [Kotlin compatibility table](https://kotlinlang.org/docs/multiplatform/multiplatform-compatibility-guide.html) and [Compose 1.9 changes](https://kotlinlang.org/docs/multiplatform/whats-new-compose-190.html)
- [bindgen allowlisting](https://rust-lang.github.io/rust-bindgen/allowlisting.html)

SQLite will enter through the Rust-owned durable store in Milestone 2; a maintained rusqlite binding with bundled SQLite avoids separate platform schemas. No database or unused SQLite dependency is introduced before that store exists. FFmpeg construction retains the complete pinned software-codec chain, while runtime hardware availability and 10-bit x265 encoding remain separate verification obligations.

CI acceptance and unresolved compatibility facts are recorded in PROGRESS.md.

## Milestone 2 implementation order

1. Native descriptor/security-scope leases and bounded Rust inspection; keep seekable originals direct and stage only nonseekable inputs.
2. Deterministic Rust configuration and actual codec/container validation; immutable accepted requests in bundled SQLite through rusqlite 0.43.0.
3. One bounded Rust worker, actual direct-library copying/encoding/filtering/muxing, finalized-output inspection and native publication reservation/receipt.
4. Wire Home, converter workspaces, native preview with lazy fallback, Jobs and essential Settings to those operations.
5. Verify realistic conversions independently on the host and through Android/iOS runtime tests, including failure/cancel/restart/publication ownership; retain M1 regressions.

The initial exposed formats are MP4/Matroska for video and M4A/MP3/WAV 16-bit/FLAC for audio. Further catalog options remain required in Milestone 3 and are not advertised early. Accepted streams are preserved or rejected with a compatibility error, never silently discarded. Hardware routes remain separately unverified; software processing provides the initial deterministic route where its accepted semantics are supported.

rusqlite's bundled SQLite keeps one Rust-owned operational schema. Its selected dependency closure requires thiserror 2.0.21; that patch update is included for compatibility. Native OS adapters implement document leases and publication primitives, with app/job policy retained in Rust. Publication reservations are persisted before saving, and completion is committed only after a confirmed user-accessible result. Interrupted work is explicitly restartable rather than labeled resumed.

The initial explicit H.264 quality control uses x264 constant-quality encoding. Direct software FFmpeg preserves the accepted filtering/timing/track semantics for this route; compatible streams copy without encoding. Hardware-route availability remains a separate capability and is not inferred from compiled codec names. There is no user backend selector or universal backend ranking.

Accepted configurations and job records live in one Rust-owned SQLite store with one bounded worker. Workspace inspection handles are transient; accepted requests retain their own durable native locators and immutable source facts. Completion and cancellation transitions serialize around worker ownership; verified saving retries compare retained size/hash/media and reconcile actual native publication before requiring a working path. Native open/share failures return to the shared custom alert lifecycle.

Android verification uses a separate test-only fixture APK under tests/android-fixtures. Instrumentation APK dependencies are deduplicated against the target app, leaving remotely launched Kotlin fixture providers without their runtime. Isolating manufactured sources in a standalone test package solves that actual class-loader failure and exercises real document grants without adding providers or fixture assets to product builds.

Apple shared frameworks are cached only under an exact identity covering their Rust/UI source, dependency lockfiles, Gradle/toolchain configuration and verified native dependency identity. Device Release and simulator Debug caches are independent. Native Swift or XCTest-only edits can reuse the unchanged shared binary; source or native ABI changes invalidate it. Direct development commands rebuild unless a CI-owned identity matches. Host/app linkage and runtime tests still execute after reuse.

iOS picker verification uses a simulator-only native fixture app with a separate sandbox and Files-visible Documents. UI tests select those generated originals through the actual system picker; the host runner checks hashes before and after conversion. Fixture seeding uses exclusive creation and preserves unidentified pre-existing data. Neither platform's fixture provider/app is part of the released product target.

Source inspection has a shared 512 KiB metadata snapshot budget across container, stream and chapter records. Native FFmpeg dictionaries remain untouched for processing. Nonseekable cache stages have live ownership references; startup and explicit cache cleanup reconcile only owned, unused stages and preserve unknown entries. A video resize compensates sample aspect ratio for encoder alignment rather than stretching the display aspect ratio.

Defaults are selected from the same source-aware Rust catalog shown by the workspace. Unsupported auxiliary-track/container and chapter combinations are excluded before acceptance. Matroska is the initial chapter-preserving route; unsupported audio chapter sources receive an explicit compatibility error. HDR compatibility previews disclose their SDR display transformation separately from accepted output requirements.
