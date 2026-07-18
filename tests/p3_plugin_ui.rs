#[cfg(feature = "kittest")]
mod p3_plugin_ui {
    use neowaves::kittest::harness_with_startup;
    use neowaves::StartupConfig;

    #[test]
    fn plugin_manager_window_renders_without_plugins_or_worker() {
        let mut harness = harness_with_startup(StartupConfig::default());
        harness.run_steps(2);
        harness.state_mut().test_set_plugin_manager_open(true);
        // The window must render (empty catalog, no worker binary in the
        // test environment) without panicking, across several frames.
        harness.run_steps(5);
        assert!(harness.state().test_plugin_manager_open());
        harness.state_mut().test_set_plugin_manager_open(false);
        harness.run_steps(2);
        assert!(!harness.state().test_plugin_manager_open());
    }
}
