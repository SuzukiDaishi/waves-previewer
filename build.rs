fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-msvc") && rustflags_force_static_crt() {
        panic!(
            "Detected '+crt-static' in rustflags. NeoWaves expects MSVC dynamic CRT (/MD) \
             because ONNX Runtime and bundled native deps are linked that way. \
             Remove '+crt-static' from RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS and rebuild."
        );
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
