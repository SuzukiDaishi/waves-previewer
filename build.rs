fn main() {
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
