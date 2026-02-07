#[test]
fn controls_doc_has_stable_editor_s_and_r_mapping() {
    let text = std::fs::read_to_string("docs/CONTROLS.md").expect("read docs/CONTROLS.md");
    assert!(
        text.contains("`S`: 表示モード切り替え") || text.contains("`S`: View"),
        "controls doc must define S as view switch"
    );
    assert!(
        text.contains("`R`: Zero Cross Snap") || text.contains("`R`: Zero Cross"),
        "controls doc must define R as zero-cross toggle"
    );
}

#[cfg(feature = "kittest")]
mod kittest_contracts {
    use neowaves::kittest::harness_default;

    #[test]
    fn debug_summary_marks_missing_latency_samples() {
        let harness = harness_default();
        let summary = harness.state().test_debug_summary_text();
        assert!(summary.contains("select_to_preview_ms: n=0"));
        assert!(summary.contains("select_to_play_ms: n=0"));
        assert!(summary.contains("warning: select_to_preview_ms has no samples"));
        assert!(summary.contains("warning: select_to_play_ms has no samples"));
    }
}
