# Direct native interoperability

`build-apple.sh <iphoneos|iphonesimulator> <Debug|Release>` produces the existing
Rust engine archive, generated Swift/header/modulemap, actual native linker
requirements and artifact identities under `build/ios/<sdk>/<configuration>/`.
Construct or restore the verified Apple FFmpeg prefixes first. Substantial
commands belong on the provisioned macOS CI runner.
`TARGET-IDENTITY` records a canonical content hash over engine sources/lockfile,
generator inputs, exact native construction record, compiler/Xcode/SDK and the
produced archive/binding hashes. These outputs must be consumed together.

`generate-swift.sh <archive> <output>` builds the official UniFFI 0.29.4 CLI
example from checksum-pinned upstream source and its unchanged Cargo.lock under
ignored `build/tools`. It parses the actual Apple static archive; no host engine
dylib, second application crate, global tool installation or host FFmpeg build
is required. Generated API checksums remain enabled.
The tool source retains upstream's MPL-2.0 license and notices in its ignored
extraction directory; this does not introduce or change the Velmux product license.

`scripts/ci/verify-swift-interop.sh` is temporary migration verification. It
builds/links both arm64 Apple targets at iOS 15.0, then executes the simulator
smoke through `simctl spawn`. The proof covers real FFmpeg calls, typed records,
errors, callbacks, explicit cancellation, one Engine, callback release and actual
AAC encoding/publication independently decoded through AVFoundation. Its native
file provider uses only a disposable CI-owned directory; production document
grants, security scope, SwiftUI workflows and physical hardware need their own
renewed integration evidence. The existing iOS production bridge is untouched.

The generated Swift can be compiled into the native application module with its
FFI modulemap supplied to Clang. Link `libvelmux_core.a` once and preserve the
reported system closure, including C++/iconv. Do not also link the old shared
framework or merge another FFmpeg copy into that process. No generated files or
compiled dependencies belong in handwritten source control.

Primary pinned references: [metadata extraction](https://github.com/mozilla/uniffi-rs/blob/v0.29.4/uniffi_bindgen/src/macro_metadata/extract.rs),
[upstream CLI helper](https://github.com/mozilla/uniffi-rs/tree/v0.29.4/examples/app/uniffi-bindgen-cli),
[Swift ownership](https://github.com/mozilla/uniffi-rs/blob/v0.29.4/uniffi_bindgen/src/bindings/swift/templates/ObjectTemplate.swift),
[callback/error handling](https://github.com/mozilla/uniffi-rs/blob/v0.29.4/uniffi_bindgen/src/bindings/swift/templates/Helpers.swift).
