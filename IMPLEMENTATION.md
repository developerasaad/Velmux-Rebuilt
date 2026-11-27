# Implementation sequence

The full product and acceptance requirements are in REQUIREMENTS.md. Authorization currently ends at Milestone 1.

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
- Rust native linking uses pkg-config only for dependency discovery. Only real archives inside the managed FFmpeg prefix are bundled; platform SDK libraries stay dynamic. This prevents NDK libc globals from being duplicated inside the engine. Android uses exactly one shared C++ runtime.
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
