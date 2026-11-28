# Reference findings


Historical reference audit: the 28 November 2026 native architecture/layout/neutral-design amendment in REQUIREMENTS.md supersedes earlier presentation/palette decisions below. Original application code remains read-only reference.
Read-only audit of `developerasaad/Velmux` at `5971e648c303dd9f044e6c5ac67c852fe46e886f`.
Only FFmpeg construction infrastructure is reused; application findings inform fresh design.

| Evidence | Consequence for this implementation |
| --- | --- |
| `932824e`: Android launch lacked libc++_shared although compilation passed | Inspect APK dependency closure per ABI and execute emulator calls |
| iOS run [45797783728](https://github.com/developerasaad/Velmux/actions/runs/45797783728) failed with missing `velmux_abi`; `4c68720`/`5971e64` fixed naming/search paths without complete linkage | A single generated-binding engine linked through the shared framework; require launch and callbacks |
| `cfd7f36`, `932824e`, `314d874`, `fea0a05`: cross SDK/deployment environment contaminated host tools | Retain environment sanitation and Darwin libtool fixes in dependency closure |
| `core/src/configuration/sessions.cpp` candidate updates invoked full planning; `WorkflowController.kt` globally invalidated selector caches | Cheap typed Rust configuration rules; submit-time full validation |
| `sessions.cpp` inferred WebP support from MJPEG and generic backend presence | Verify exact compiled registries; separate compatibility from hardware availability |
| `core/src/compression/compression.cpp` estimated only first audio track and invented 1080p/30 fps facts | Account for every accepted stream; preserve unknowns and report justified ranges |
| `FrameEditors.kt` modes lacked actual decoded-frame identity controls | Implement actual frame selection in its authorized milestone |
| `core/src/recovery/recovery.cpp` accepted position-only/half-size artifacts and resumed manually paused work | Validate reusable media artifacts and retain manual pause intent |
| `EngineClient.kt` global banner and retained old errors | Scope and consume transient events; durable job failures are separate |
| Android `SourcePorter` copied all content URIs; iOS copied into temporary storage; `cb405b2` records an import helper moving originals | Native durable access and offsets first; staged copies only when required; verify originals remain intact |
| `234508d` introduced batch processing | Do not inherit this incompatible product scope |

Identity and colors were inspected independently of UI implementation. Both platform identifiers are `com.velmux.mobile`; iOS minimum is 15.0. Copper is `#E8833A`, light paper `#FFF8F3`, light ink `#2A211B`, dark background `#17120E`, dark surface `#211A14`, dark text `#F2E9E1`.
