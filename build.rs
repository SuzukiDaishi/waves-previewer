fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-msvc") && rustflags_force_static_crt() {
        panic!(
            "Detected '+crt-static' in rustflags. NeoWaves expects MSVC dynamic CRT (/MD) \
             because ONNX Runtime and bundled native deps are linked that way. \
             Remove '+crt-static' from RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS and rebuild."
        );
    }

    if std::env::var_os("CARGO_FEATURE_MP3_LAME").is_some() {
        build_lame_shared(&target);
    }

    #[cfg(target_os = "windows")]
    {
        windows_exe_info::icon::icon_ico("icons/icon.ico");
        windows_exe_info::versioninfo::VersionInfo::from_cargo_env_ex(
            Some("NeoWaves Audio List Editor"),
            Some("NeoWaves"),
            None,
            None,
        )
        .link()
        .expect("failed to link version info");
    }
}

const LAME_DIR: &str = "vendor/lame-3.100";

fn build_lame_shared(target: &str) {
    println!("cargo:rerun-if-changed={LAME_DIR}");
    if target.ends_with("windows-msvc") {
        #[cfg(target_os = "windows")]
        build_lame_dll_windows();
        #[cfg(not(target_os = "windows"))]
        panic!("cross-building the bundled LAME DLL from a non-Windows host is not supported");
    } else {
        #[cfg(unix)]
        build_lame_shared_unix(target);
        #[cfg(not(unix))]
        panic!("dynamic LAME build is unsupported for target {target}");
    }
}

#[cfg(unix)]
fn build_lame_shared_unix(target: &str) {
    let mut config = autotools::Config::new(LAME_DIR);
    let host = std::env::var("HOST").expect("HOST is not set");
    if host != target {
        if target.contains("android") {
            config.config_option("host", Some(target));
        } else if target.contains("apple") {
            let apple_host = if target.starts_with("aarch64") {
                "arm-apple-darwin"
            } else {
                "x86_64-apple-darwin"
            };
            config.config_option("host", Some(apple_host));
        }
    }
    if target.contains("android") || target.contains("ios") {
        config.cflag("-DSTDC_HEADERS");
    }
    let result = config
        .disable("decoder", None)
        .enable_shared()
        .disable_static()
        .disable("rpath", None)
        .disable("frontend", None)
        .disable("gtktest", None)
        .with("pic", None)
        .fast_build(true)
        .build();
    println!("cargo:rustc-link-search=native={}/lib", result.display());
    println!("cargo:rustc-link-lib=dylib=mp3lame");
}

#[cfg(target_os = "windows")]
fn build_lame_dll_windows() {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let lame_dir = Path::new(LAME_DIR);
    let include_msvc = out_dir.join("lame_include_msvc");
    std::fs::create_dir_all(&include_msvc).expect("create generated LAME include directory");
    std::fs::copy(lame_dir.join("configMS.h"), include_msvc.join("config.h"))
        .expect("copy LAME config.h");

    let sources = [
        "libmp3lame/bitstream.c",
        "libmp3lame/encoder.c",
        "libmp3lame/fft.c",
        "libmp3lame/gain_analysis.c",
        "libmp3lame/id3tag.c",
        "libmp3lame/lame.c",
        "libmp3lame/newmdct.c",
        "libmp3lame/presets.c",
        "libmp3lame/psymodel.c",
        "libmp3lame/quantize_pvt.c",
        "libmp3lame/vector/xmm_quantize_sub.c",
        "libmp3lame/quantize.c",
        "libmp3lame/reservoir.c",
        "libmp3lame/set_get.c",
        "libmp3lame/tables.c",
        "libmp3lame/takehiro.c",
        "libmp3lame/util.c",
        "libmp3lame/vbrquantize.c",
        "libmp3lame/VbrTag.c",
        "libmp3lame/version.c",
    ];

    let compiler = cc::Build::new().get_compiler();
    if !compiler.is_like_msvc() {
        panic!("the Windows LAME DLL build requires an MSVC-like compiler");
    }

    let mut objects = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        let stem = Path::new(source)
            .file_stem()
            .and_then(OsStr::to_str)
            .expect("LAME source has a file stem");
        let object = out_dir.join(format!("lame_{index:02}_{stem}.obj"));
        let mut command = compiler.to_command();
        command
            .arg("/nologo")
            .arg("/c")
            .arg("/O2")
            .arg("/MD")
            .arg("/W0")
            .arg("/DTAKEHIRO_IEEE754_HACK")
            .arg("/DFLOAT8=float")
            .arg("/DREAL_IS_FLOAT=1")
            .arg("/DBS_FORMAT=BINARY")
            .arg("/DHAVE_CONFIG_H")
            .arg(format!("/I{}", lame_dir.join("include").display()))
            .arg(format!("/I{}", include_msvc.display()))
            .arg(format!("/I{}", lame_dir.join("libmp3lame").display()))
            .arg(format!("/Fo{}", object.display()))
            .arg(lame_dir.join(source));
        run_native(&mut command, "compile LAME source");
        objects.push(object);
    }

    // The upstream .def also exports the optional decoder API. Generate an
    // encoder-only copy so linking does not create unresolved exports.
    let original_def = std::fs::read_to_string(lame_dir.join("include/lame.def"))
        .expect("read LAME export definition");
    let encoder_def = original_def
        .lines()
        .filter(|line| {
            let symbol = line.trim_start();
            !symbol.starts_with("lame_decode") && !symbol.starts_with("hip_")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let def_path = out_dir.join("libmp3lame.def");
    std::fs::write(&def_path, format!("{encoder_def}\n")).expect("write LAME export file");

    let dll_path = out_dir.join("libmp3lame.dll");
    let import_lib = out_dir.join("mp3lame.lib");
    let mut link = compiler.to_command();
    link.arg("/nologo")
        .arg("/LD")
        .arg(format!("/Fe{}", dll_path.display()))
        .args(&objects)
        .arg("/link")
        .arg(format!("/DEF:{}", def_path.display()))
        .arg(format!("/IMPLIB:{}", import_lib.display()));
    run_native(&mut link, "link libmp3lame.dll");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=mp3lame");

    // Put the DLL next to regular binaries and Cargo's test binaries. An
    // installed user may replace it with any ABI-compatible libmp3lame build.
    if let Some(profile_dir) = out_dir.ancestors().nth(3) {
        copy_lame_dll(&dll_path, profile_dir);
        copy_lame_dll(&dll_path, &profile_dir.join("deps"));
    }

    fn run_native(command: &mut Command, what: &str) {
        let status = command
            .status()
            .unwrap_or_else(|err| panic!("{what}: {err}"));
        if !status.success() {
            panic!("{what} failed with {status}");
        }
    }

    fn copy_lame_dll(source: &Path, destination_dir: &Path) {
        std::fs::create_dir_all(destination_dir).expect("create LAME DLL destination");
        std::fs::copy(source, destination_dir.join("libmp3lame.dll"))
            .expect("copy libmp3lame.dll beside executable");
    }
}

fn rustflags_force_static_crt() -> bool {
    let encoded = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let plain = std::env::var("RUSTFLAGS").unwrap_or_default();
    let mut flags = Vec::new();
    if !encoded.is_empty() {
        flags.extend(encoded.split('\u{1f}').map(str::to_string));
    }
    if !plain.is_empty() {
        flags.extend(plain.split_whitespace().map(str::to_string));
    }
    flags.into_iter().any(|f| {
        let t = f.trim().to_ascii_lowercase();
        t.contains("+crt-static") || t == "/mt" || t.starts_with("/mt")
    })
}
