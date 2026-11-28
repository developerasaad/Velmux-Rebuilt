use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

fn managed_archive(path: &Path, prefix: &Path) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    if !canonical.starts_with(prefix) {
        return false;
    }
    let mut magic = [0; 8];
    File::open(&canonical)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(
        &magic,
        b"!<arch>\n",
        "expected a complete native archive: {}",
        canonical.display()
    );
    println!("cargo:rerun-if-changed={}", canonical.display());
    true
}

fn emit_library(name: &str, prefix: &Path, android: bool, cpp_emitted: &mut bool) {
    assert!(
        !name.starts_with(':'),
        "unexpected verbatim system library: {name}"
    );
    if android && ["c++", "stdc++", "c++abi", "c++_static", "c++_shared"].contains(&name) {
        if !*cpp_emitted {
            println!("cargo:rustc-link-lib=dylib=c++_shared");
            *cpp_emitted = true;
        }
        return;
    }
    let system = [
        "c", "m", "dl", "pthread", "rt", "util", "resolv", "log", "android", "mediandk", "System",
        "c++", "stdc++", "gcc", "gcc_s", "atomic", "unwind",
    ]
    .contains(&name);
    // pkg-config's cross-system path heuristics can classify NDK libc.a as
    // vendored. Bundling it duplicates Bionic globals and crashes in constructors.
    let bundled =
        !system && managed_archive(&prefix.join("lib").join(format!("lib{name}.a")), prefix);
    if [
        "avfilter",
        "avformat",
        "avcodec",
        "swscale",
        "swresample",
        "avutil",
    ]
    .contains(&name)
    {
        assert!(
            bundled,
            "required FFmpeg archive missing from managed prefix: {name}"
        );
    }
    let kind = if bundled { "static" } else { "dylib" };
    println!("cargo:rustc-link-lib={kind}={name}");
}

fn main() {
    for name in [
        "VELMUX_FFMPEG_PREFIX",
        "ANDROID_NDK_ROOT",
        "ANDROID_NDK_HOME",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
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
    println!(
        "cargo:rerun-if-changed={}",
        prefix.join("BUILD-IDENTITY").display()
    );
    let mut includes = Vec::new();
    let mut defines = BTreeMap::new();
    let android = target.ends_with("linux-android");
    let ndk_root = android.then(|| {
        PathBuf::from(
            env::var_os("ANDROID_NDK_ROOT")
                .or_else(|| env::var_os("ANDROID_NDK_HOME"))
                .expect("Android builds require the pinned NDK"),
        )
    });
    let mut cpp_emitted = false;
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
            .cargo_metadata(false)
            .probe(name)
            .unwrap_or_else(|error| panic!("{name} in {}: {error}", prefix.display()));
        for directory in &library.link_paths {
            // The NDK driver searches API-specific shared stubs before generic
            // static SDK archives. An explicit -L into the SDK reverses that order.
            if ndk_root
                .as_ref()
                .is_some_and(|root| directory.starts_with(root))
            {
                continue;
            }
            println!("cargo:rustc-link-search=native={}", directory.display());
        }
        for name in &library.libs {
            emit_library(name, &prefix, android, &mut cpp_emitted);
        }
        for file in &library.link_files {
            assert!(
                managed_archive(file, &prefix),
                "unexpected external native archive: {}",
                file.display()
            );
            println!(
                "cargo:rustc-link-search=native={}",
                file.parent().unwrap().display()
            );
            println!(
                "cargo:rustc-link-lib=static:+verbatim={}",
                file.file_name().unwrap().to_str().unwrap()
            );
        }
        for directory in &library.framework_paths {
            println!("cargo:rustc-link-search=framework={}", directory.display());
        }
        for framework in &library.frameworks {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        for arguments in &library.ld_args {
            assert!(
                !arguments
                    .iter()
                    .any(|argument| ["-Bstatic", "--whole-archive", "--push-state"]
                        .contains(&argument.as_str())),
                "unexpected archive policy in {arguments:?}"
            );
            if !arguments.is_empty() {
                println!("cargo:rustc-link-arg=-Wl,{}", arguments.join(","));
            }
        }
        includes.extend(library.include_paths);
        defines.extend(library.defines);
    }
    if android && !cpp_emitted {
        println!("cargo:rustc-link-lib=dylib=c++_shared");
    }
    let mut builder = bindgen::Builder::default()
        .header("src/ffmpeg/wrapper.h")
        .allowlist_function("av_.*|avcodec_.*|avformat_.*|avio_.*|avfilter_.*|av_buffersrc_.*|av_buffersink_.*|sws_.*|swr_.*")
        .allowlist_type("AV.*|SwsContext|SwrContext")
        .allowlist_var("AV.*|LIBAV.*|SWS.*|SWR.*|FF_QP2LAMBDA")
        .prepend_enum_name(false)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    if target.ends_with("linux-android") {
        let ndk = env::var_os("ANDROID_NDK_ROOT")
            .or_else(|| env::var_os("ANDROID_NDK_HOME"))
            .map(PathBuf::from)
            .expect("Android builds require the pinned NDK");
        let toolchains = ndk.join("toolchains/llvm/prebuilt");
        let sysroot = std::fs::read_dir(toolchains)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("sysroot"))
            .find(|path| path.is_dir())
            .expect("NDK sysroot");
        builder = builder
            .clang_arg(format!("--sysroot={}", sysroot.display()))
            .clang_arg(format!("--target={target}29"));
    } else if target == "aarch64-apple-ios" || target == "aarch64-apple-ios-sim" {
        let (sdk, clang_target) = if target.ends_with("-sim") {
            ("iphonesimulator", "arm64-apple-ios15.0-simulator")
        } else {
            ("iphoneos", "arm64-apple-ios15.0")
        };
        let output = Command::new("xcrun")
            .args(["--sdk", sdk, "--show-sdk-path"])
            .output()
            .expect("xcrun must locate the target SDK");
        assert!(output.status.success(), "target SDK resolution failed");
        let sdk_path = String::from_utf8(output.stdout).unwrap();
        builder = builder
            .clang_arg(format!("--sysroot={}", sdk_path.trim()))
            .clang_arg(format!("--target={clang_target}"));
    }
    for (name, value) in defines {
        builder = builder.clang_arg(match value {
            Some(value) => format!("-D{name}={value}"),
            None => format!("-D{name}"),
        });
    }
    for include in includes {
        builder = builder.clang_arg(format!("-I{}", include.display()));
    }
    builder
        .generate()
        .expect("generate FFmpeg declarations from the selected target headers")
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("ffmpeg.rs"))
        .expect("write generated FFmpeg declarations");
}
