# Velmux engineering instructions

Read PROGRESS.md first, then the relevant REQUIREMENTS.md and IMPLEMENTATION.md sections.
Only Milestone 1 is authorized until explicitly changed by the owner. Preserve all later requirements without implementing them early. Keep records current at integration points and before stopping.

- Develop exclusively in developerasaad/Velmux-Rebuilt, main, with independent history. Original developerasaad/Velmux is read-only reference; only its FFmpeg construction dependency closure may be reused. Never copy application code or import history.
- Repository-local commit identity: developerasaad <developer.asaad@gmail.com>. No global Git changes, force pushes, releases, tags, store preparation, or project license changes. Preserve user changes and all original/published media. Keep credentials out of files and logs.
- Rust owns media rules, configuration, validation, orchestration, jobs, SQLite state, recovery and storage policy. Native Kotlin/Swift implement OS facilities as providers. Shared Compose owns presentation. One authoritative engine, one Rust crate, one shared UI module.
- Physical boundaries: UI/, core/, platforms/android/, platforms/ios/. Use custom Kotlin source mappings, not conventional nested source-set directories. Generated bindings and artifacts stay ignored. Direct FFmpeg libraries only; no CLI production wrapper, C++ application engine, JSON/JNI transport framework or web substitute.
- GitHub Actions is the substantial build/test environment. Local inspection, formatting and focused checks are appropriate; do not compile FFmpeg or full mobile apps locally routinely. Use actual macOS CI for Apple builds. Preserve proven cross-compilation fixes and complete artifact identities.
- Prefer simple maintained components and bounded work. Treat all media/paths/metadata as untrusted. Review unsafe/FFI ownership and cancellation carefully. Use subagents selectively for bounded investigations, isolated components or consequential review; retain interface and integration ownership.
- Require actual evidence: compilation alone is not media correctness; manifest flags are not runtime capabilities; restart is not resume; file existence is not a valid checkpoint. Never expose placeholder functionality as complete.
- Distinguish pending, in-progress, implemented/unverified, verified and blocked. Inspect actual workflow results tied to commits. Record limitations precisely. Never weaken gates for a green build.
- English only for communication, code comments, documentation and commits. Keep docs practical and complementary. Retrieve relevant context instead of repeatedly loading the entire specification.
