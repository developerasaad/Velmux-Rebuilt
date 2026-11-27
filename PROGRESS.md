# Progress

## Authorization and status

Milestone 1 — Engineering foundation: in progress. Milestones 2–7 are pending and unauthorized.

## Verified

- Workspace initially contained only .DS_Store and no Git history or configuration.
- New GitHub repository was empty; local main has independent history and only the Velmux-Rebuilt origin.
- Effective author/committer are developerasaad <developer.asaad@gmail.com>, configured locally.
- Git push and Actions dispatch independently succeeded. Initial native dependency run: [45926113131](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45926113131), source `9421523`.
- The original repository is an ignored read-only filtered clone, inspected through Git objects.
- REQUIREMENTS.md preserves the full authoritative mandate verbatim.

## In progress

- Reference audit complete; decisions recorded in IMPLEMENTATION.md and docs/reference-findings.md.
- Retrieved the authorized native dependency closure, updated FFmpeg to signed 7.1.5, strengthened identities/filter validation, and retained cross-compilation fixes. No local FFmpeg build.
- Rust diagnostic library, generated binding configuration, native providers, shared shell and focused host/mobile tests implemented but not yet compiled or run.
- Independent source review found and corrected activity-scoped engine ownership and unannotated Swift exception propagation; runtime regression tests remain pending.
- Initial Android CI selected the hosted default NDK 27.3 rather than the required 27.2. Identity validation rejected it before construction. CI now explicitly resolves the pinned NDK; host/Apple construction continues without an unchanged rerun.
- Local source checks passed: full specification byte equality, shell/Python parsing, Xcode project/plist syntax and Rust formatting. These do not prove linking or runtime correctness.

## CI evidence

- [Source checks 45926839454](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45926839454), `3814f83`: passed on attempt 2. Attempt 1 failed Maven plugin resolution; the published pinned POM was independently reachable and the unchanged retry passed.
- [Android native dependencies 45926413147](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45926413147), `baed2e9`: both arm64-v8a and x86_64 construction/compiled-component validation passed. x86_64 reported 512 decoders, 195 encoders, 177 muxers and 350 demuxers. These are build-tree capability checks, not mobile runtime or hardware-processing proof.
- Initial Apple job in run 45926113131 failed x265 C linking because its pkg-config closure omitted libc++. A focused x265/zimg Apple runtime correction is implemented; Apple construction/link/runtime evidence is pending.
- Host native construction in [45926113131](https://github.com/developerasaad/Velmux-Rebuilt/actions/runs/45926113131), `9421523`: passed its actual runtime registry probe and validation. The overall run failed because of the separately recorded Android environment and Apple link errors.

- Host and Android integration have been dispatched against `8e29cf7` before the unrelated Apple-builder correction, preserving exact matching native artifact identities. Source revisions remain explicit; no cached artifact is relabeled.

- First host/Android integration attempts reached Rust compilation and found omitted opaque-type declarations in the generated bindgen output. The function allowlist now explicitly includes the four required opaque types. Neither attempt established runtime correctness.
- The superseded Apple integration attempt was canceled before its known Rust compile failure, so dependency construction can be saved independently before app verification.
- Native identity schema 3 hashes each complete target recipe rather than unrelated builders, plus both host C/C++ compilers. This deliberately invalidates initial keys once; artifacts are rebuilt, never relabeled. Subsequent target-only recipe edits preserve other targets' caches.

- Independently reran all nine identity isolation/tool-version/PATH regression assertions locally after inspecting the fixture script. These are cache-rule tests with tool stubs, not native compiler verification. They now run in Source checks.
- Public build/test entry points and CI setup enforce the selected clean compiler environment. Android commands require Bash 5; unverified custom compiler flag environments are not supported.

## Acceptance still pending

Dependency construction and target capability verification; Android/iOS linking, launch, callbacks, cancellation and lifetime tests; host tests; actual CI run inspection. No product tools or physical-device behavior are claimed.

## Next authorized action

Continue Milestone 1 integration and verification. Do not begin Milestone 2.
