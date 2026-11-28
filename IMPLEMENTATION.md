# Implementation sequence

The full product and acceptance requirements are in REQUIREMENTS.md. Authorization currently ends at Milestone 3.

Milestones 1 and 2 are complete and accepted; their inspected host, Android and Apple evidence is recorded in PROGRESS.md. Milestone 3 is explicitly authorized and in progress. Milestones 4–7 remain pending and require further authorization.

| Milestone | Dependency | Acceptance boundary |
| --- | --- | --- |
| 1. Engineering foundation | Independent repository and reference audit | Pinned native dependencies, custom layout, generated interop, real FFmpeg/mobile calls and callbacks, host/mobile CI evidence |
| 2. End-to-end foundation | Verified M1 | Real video/audio conversion from native selection to verified publication, shared configuration, previews, jobs and storage ownership |
| 3. Complete converters | Verified M2 | Every converter option, compression/target size, metadata, audio/subtitle behavior and truthful estimates independently verified |
| 4. Graphical manipulation | Verified M3 | Physical crop, precise trim/cut/split/merge, real gestures/thumbnails/waveforms and content verification |
| 5. Extraction and presets | Verified M4 | All extraction methods, exact frame/track identity, portable presets and partial multi-output publication |
| 6. Reliability and refinement | Verified M5 | Actual retained-work recovery, lifecycle/publication/cleanup failures, accessibility and measured performance |
| 7. Independent acceptance | Verified M6 | Requirement audit, corrected defects, realistic media/state/UI tests and clean CI reproduction |

## Current architecture and migration decisions

The 28 November 2026 amendment replaces the presentation architecture within Milestone 3. Final roots are core/, android/ and ios/, with lowercase native src/ui directories. Android adapts the existing Kotlin UI to native Jetpack Compose/Material 3 and direct generated UniFFI Kotlin. iOS implements SwiftUI with direct generated UniFFI Swift, retaining Swift document/security-scope/publication/playback providers. Rust, direct FFmpeg, SQLite, accepted requests, source ownership, storage policy and native construction/linking corrections remain authoritative and are preserved.

The native session on each platform owns exactly one process-lifetime engine, request cancellation, callback adapters and observation delivery. Screens own transient state and native navigation, never media policy or engine shutdown. The iOS app must have no Kotlin/Native, Compose framework or Kotlin-exported controller in its accepted final dependency closure.

Sequence: preserve the current worktree; amend requirements/records; verify generated Kotlin and direct Swift calls/callbacks/lifetimes against the real engine; adapt Android and implement equivalent SwiftUI functional workflows; adopt neutral native themes and deliberate layouts; renew affected M1/M2 runtime/media/UI gates; remove obsolete shared infrastructure after replacements are demonstrated; then complete the remaining M3 functionality. Do not replace the application with diagnostics or claim migration from compilation alone.

Temporary bridge/build coexistence is allowed only to prove replacements before deletion. Update source paths, Gradle/Xcode, scripts/workflows/tests/ignore rules and exact cache identities together. App/UI edits must not invalidate FFmpeg dependencies. Use actual macOS CI for Apple builds, with unchanged iOS15.0 and Android API29 floors.

Use neutral platform semantic surfaces/text/actions, native typography/controls and restrained grouping. Reference colors are in REQUIREMENTS.md; orange #E8833A remains primarily identity. Preserve System/Light/Dark persistence, error semantics and stale-event prevention. Native design equivalence replaces pixel-identical UI and mandatory custom dialogs.

The seven-milestone roadmap above remains intact. Historical decisions below explain the accepted M1/M2 implementation; their shared presentation and framework descriptions are superseded, not instructions to restore that architecture.

## Preserved foundation and historical decisions

- Preserve com.velmux.mobile on Android/iOS, Android API 29 and iOS 15.0 from reference identity. Native API availability and target compilation remain acceptance checks.
- Use Kotlin 2.2.20, Compose 1.9.3, AGP 8.10.1, Gradle 8.14.3, JDK 21, Rust 1.99.1, Gobley 0.3.7 and UniFFI 0.29.4 as an explicitly pinned integration candidate. Compatibility is a verification gate, not a claim inferred from version numbers. Android NDK 27.2.12479018; Apple builds select Xcode 16.4.
- Direct UniFFI Kotlin/Swift bindings are the amended interoperability boundary. The previously verified Kotlin-mediated Swift bridge is retained temporarily only until direct Swift calls and callbacks are demonstrated. Android-only Gobley reuse is being evaluated for its proven NDK variants and packaging; it may not keep KMP or Kotlin/Native in the final architecture.
- Generate a narrow FFmpeg function allowlist from the actual target headers using bindgen; link the complete static pkg-config closure. This avoids handwritten ABI declarations and confines unsafe registry iteration to core/src/ffmpeg. Expand safe resource ownership only when real processing requires it. Target sysroots are supplied explicitly; Android uses one shared NDK C++ runtime packaged by Gobley, with ELF alignment checked in the actual APK.
- Rust native linking uses pkg-config only for dependency discovery. Only real archives inside the managed FFmpeg prefix are bundled; platform SDK libraries stay dynamic. No explicit SDK link-search directories override the NDK driver’s API-first lookup. These boundaries prevent platform libc globals from being duplicated inside the engine. Android uses exactly one shared C++ runtime.
- Foundation diagnostics exercise direct linked library registries, provider callbacks, records, errors, cancellation and worker-thread calls. They do not constitute the product media catalog or pretend to perform conversion.
- Host SVT-AV1 disables GCC IPO/LTO: Rust archive bundling did not preserve usable GCC LTO symbols. Normal machine-code archives pass the real host integration suite; mobile Clang recipes retain their validated configuration.
- Android initially targets arm64-v8a devices and x86_64 emulators. Apple initially targets arm64 devices and arm64 simulators. Other architectures are not claimed.
- Preserve the reference FFmpeg dependency construction closure and inline cross-compilation fixes, with exact source provenance. Improve cache identity and verify target registries. Do not carry over old application transport or C++ processing.
- Future configuration uses deterministic Rust catalog rules over already inspected sources; full execution planning belongs at submission. No mutable remote-style configuration sessions or global cache revisions.
- Future recovery validates actual retained artifacts and preserves manual pause intent. Cancellation must acknowledge worker termination before resource deletion. Publication and transient alerts remain distinct from durable job semantics.

## Historical interoperability references

- [Gobley Android/iOS integration](https://gobley.dev/docs/tutorial/) and [Cargo configuration](https://gobley.dev/docs/gradle-plugins/cargo/)
- Previous KMP compatibility research is historical only; final native toolchain compatibility requires direct Android and Apple build/runtime evidence.
- [bindgen allowlisting](https://rust-lang.github.io/rust-bindgen/allowlisting.html)

Milestone 2 introduces the Rust-owned durable store through rusqlite with bundled SQLite, avoiding separate platform schemas. The database dependency was introduced with its actual operational store. FFmpeg construction retains the complete pinned software-codec chain, while runtime hardware availability and 10-bit x265 encoding remain separate verification obligations.

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

Accepted configurations and job records live in one Rust-owned SQLite store with one bounded worker. Workspace inspection handles are transient; accepted requests retain their own durable native locators and immutable source facts. Completion and cancellation transitions serialize around worker ownership; verified saving retries compare retained size/hash/media and reconcile actual native publication before requiring a working path. Native open/share failures retain scoped error/feedback semantics through each native presentation lifecycle.

Android verification uses a separate test-only fixture APK under tests/android-fixtures. Instrumentation APK dependencies are deduplicated against the target app, leaving remotely launched Kotlin fixture providers without their runtime. Isolating manufactured sources in a standalone test package solves that actual class-loader failure and exercises real document grants without adding providers or fixture assets to product builds.

The historical Apple framework cache used exact source/toolchain/dependency identities. Its replacement must independently key Rust static libraries and generated Swift bindings by engine source, lockfile, binding generator, target SDK/toolchain and verified FFmpeg identity. Native UI-only edits must reuse compatible core artifacts while still executing app linkage and runtime tests. No cached artifact may be relabeled across the old and new bridges.

iOS picker verification uses a simulator-only native fixture app with a separate sandbox and Files-visible Documents. UI tests select those generated originals through the actual system picker; the host runner checks hashes before and after conversion. Fixture seeding uses exclusive creation and preserves unidentified pre-existing data. Neither platform's fixture provider/app is part of the released product target.

Source inspection has a shared 512 KiB metadata snapshot budget across container, stream and chapter records. Native FFmpeg dictionaries remain untouched for processing. Nonseekable cache stages have live ownership references; startup and explicit cache cleanup reconcile only owned, unused stages and preserve unknown entries. A video resize compensates sample aspect ratio for encoder alignment rather than stretching the display aspect ratio.

Defaults are selected from the same source-aware Rust catalog shown by the workspace. Unsupported auxiliary-track/container and chapter combinations are excluded before acceptance. Matroska is the initial chapter-preserving route; unsupported audio chapter sources receive an explicit compatibility error. HDR compatibility previews disclose their SDR display transformation separately from accepted output requirements.

Rust reference-counts durable source access separately from individual descriptors. In-flight operations and workspace snapshots retain transient references; unfinished accepted jobs retain durable references reconstructed from SQLite. Completion, cancellation acknowledgement, discard and expiry retire those references. Runtime closure preserves unfinished-job access. Native startup reconciliation only retires grants with our own OS receipts. Android acquires persistable read access at Rust's first reference, after the native picker delivers its transient grant; unrelated existing grants are preserved. iOS bookmark bytes remain in Rust-owned locators, and active security scopes remain descriptor-lease owned.

Configuration choices use a synchronous, cheap Rust rule query over inspected facts and current format/codec/sampling intent. Filename edits do not query it. Original sample rates/channel counts and explicit bitrates are omitted when the selected encoding route cannot support them; retained choices remain visible as conflicts rather than changing silently. Final core validation uses the same audio encoding-intent rules. Automatic bitrate is bounded by the actual AAC/MP3 sampling route. There are no asynchronous field-negotiation sessions or repeated probes.

Android grant cleanup coordinates with the OS picker handoff. A release is deferred while selection is open or while one delivered locator awaits its Rust claim; cancellation, a subsequent selection and startup reconciliation retire obsolete handoffs. This protects a new persistable offer from an older reference's release without assigning job policy to Kotlin. Unclaimed handoffs are bounded to one primary locator.

Historical M2 verification corrected orange/copper contrast and inspected native captures. The authorized neutral visual system supersedes those colors; equivalent rendered light/dark contrast and system-bar checks must be renewed after migration.

Native document selection accepts generic openable data, with operation-specific guidance. Provider MIME labels and OS UTI families cannot reliably describe every source the core can inspect. Rust remains responsible for identifying actual contents and required streams; the UI tests select a generated video through a generic-data document alias. Previews adapt to the available workspace height so native controls do not consume the entire landscape scroll viewport.

Native alerts must extract the authoritative typed core error detail rather than generated exception debug descriptions. Accepted-request submission temporarily locks configuration controls while leaving unrelated UI and engine work responsive. Mobile acceptance also independently decodes retained published outputs with system FFmpeg tools; those tools belong only to CI verification, never production processing.

The native presentation resolves its persisted light/dark appearance and updates the native host. Android applies system-bar icon styles and the corresponding window background, including transparent-bar behavior on API35. Runtime checks require rendered content/bar colors as well as icon-mode flags and preserve the setting across Activity recreation. iOS sanitization retains the complete accepted 180-byte filename stem plus its actual extension and reserves bounded space for native collision suffixes.

## Milestone 3 implementation sequence

1. Preserve M2 wire enum names, optional/defaulted record evolution and immutable accepted-plan semantics; guard operational schema versions without deleting records. Extend actual codec/container, geometry, rational-rate and audio representation processing in coherent tested slices.
2. Add the Rust-owned compression/rate-control and transformed-output expectations, then real video/audio adjustments with bounded reverse and measured audio processing.
3. Extend stable source/stream ownership into intended track composition, subtitles, metadata and artwork; compatibility uses that composition rather than forbidding an otherwise viable source.
4. Connect complete converter workspaces, truthful estimates, user-requested sample conversion and Target Not Met decisions to actual processing.
5. Verify real media progressively with independent inspection/decoding, retain M1/M2 regressions, then execute affected Android/iOS runtime and UI gates. Physical hardware limitations remain explicit.

Original is user intent, not a muxer name. Accepted jobs must persist the resolved representation needed for working paths, MIME, publication and restart. Additive configuration/plan evolution must continue reading existing accepted M2 requests without silently re-planning them under new defaults. New capabilities are exposed only as their processing paths become functional.

M3 precision builds extend the pinned dependency closure rather than replacing it: combined x265 Main8/Main10 follows the pinned upstream multilib recipe, and libvpx enables its existing VP9 high-bit-depth option. Compiled encoder declarations are queried once through bounded FFmpeg APIs and cached; they remain distinct from actual native hardware session availability. Route eligibility and output precision require runtime verification.


Expanded converter profiles are immutable accepted stream settings, separate from source inspection and inexpensive selector queries. A missing profile continues the M2 implementation rather than recomputing accepted intent. Current profiles record the selected encoder, pixel/sample representation, exact speaker layout, precision, rate-control options and explicit color conversion. Native hardware availability remains a separate runtime concern. Static HDR metadata is copied into independent encoder-owned decoded side data before initialization, and finalized output checks compare semantic pixel/HDR properties. The media matrix independently inspects raw HEVC headers to avoid mistaking copied container metadata for preserved HDR bitstream metadata.
