use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=VELMUX_FFMPEG_PREFIX");
    println!("cargo:rerun-if-changed=src/ffmpeg/wrapper.h");
    let target = env::var("TARGET").unwrap();
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    let slice = match target.as_str() {
        "aarch64-linux-android" => "android/arm64-v8a",
        "x86_64-linux-android" => "android/x86_64",
        "aarch64-apple-ios" => "ios/iphoneos",
        "aarch64-apple-ios-sim" => "ios/iphonesimulator",
        _ if target == env::var("HOST").unwrap() => "host",
        _ => panic!("unsupported Rust target: {target}"),
    };
    let prefix = env::var_os("VELMUX_FFMPEG_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("scripts/ffmpeg-builder/out").join(slice));
    let prefix = prefix
        .canonicalize()
        .expect("build the pinned FFmpeg prefix first");
    // Never discover an unrelated system FFmpeg, especially during cross compilation.
    env::set_var("PKG_CONFIG_LIBDIR", prefix.join("lib/pkgconfig"));
    env::set_var("PKG_CONFIG_PATH", prefix.join("lib/pkgconfig"));
    env::set_var("PKG_CONFIG_ALLOW_CROSS", "1");
    let mut includes = Vec::new();
    for name in [
        "libavfilter",
        "libavformat",
        "libavcodec",
        "libswscale",
        "libswresample",
        "libavutil",
    ] {
        let library = pkg_config::Config::new()
            .statik(true)
            .probe(name)
            .unwrap_or_else(|error| panic!("{name} in {}: {error}", prefix.display()));
        includes.extend(library.include_paths);
    }
    let mut builder = bindgen::Builder::default()
        .header("src/ffmpeg/wrapper.h")
        .allowlist_function("av_version_info|av_codec_iterate|av_codec_is_encoder|av_codec_is_decoder|av_muxer_iterate|av_demuxer_iterate|av_filter_iterate")
        .opaque_type("AV.*")
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for include in includes {
        builder = builder.clang_arg(format!("-I{}", include.display()));
    }
    builder
        .generate()
        .expect("generate FFmpeg declarations from the selected target headers")
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("ffmpeg.rs"))
        .expect("write generated FFmpeg declarations");
}
