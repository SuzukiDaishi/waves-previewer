use serde_json::{json, Value};

use super::registry::ActionRegistry;

#[derive(Clone, Copy, Debug)]
pub struct DomainCoverage {
    pub domain: &'static str,
    pub actions: &'static [&'static str],
    pub withheld_reason: Option<&'static str>,
}

const COVERAGE: &[DomainCoverage] = &[
    DomainCoverage {
        domain: "Context",
        actions: &["context.get", "jobs.list"],
        withheld_reason: None,
    },
    DomainCoverage {
        domain: "List",
        actions: &[
            "list.inspect_selection",
            "list.query",
            "list.select",
        ],
        withheld_reason: Some(
            "Import, delete, and rename remain foreground UI flows because they broaden filesystem scope.",
        ),
    },
    DomainCoverage {
        domain: "Allowlisted UI controls",
        actions: &[
            "ui.controls.list",
            "ui.text.set",
            "ui.checkbox.set",
            "ui.button.press",
        ],
        withheld_reason: Some(
            "Only stable declared control IDs are exposed; arbitrary coordinates, raw keystrokes, dialogs, credentials, and unrestricted widgets remain unavailable.",
        ),
    },
    DomainCoverage {
        domain: "Workspace",
        actions: &["workspace.show"],
        withheld_reason: None,
    },
    DomainCoverage {
        domain: "Transport",
        actions: &["transport.play_pause", "transport.stop"],
        withheld_reason: Some(
            "Seek and audition scheduling are withheld until their active-source preconditions share one typed contract.",
        ),
    },
    DomainCoverage {
        domain: "Editor",
        actions: &[
            "editor.inspect",
            "editor.gain.apply",
            "editor.trim.apply",
            "loop.set",
            "markers.add",
        ],
        withheld_reason: Some(
            "Other DSP tools remain in their existing preview/apply workers until each has a bounded schema and diff.",
        ),
    },
    DomainCoverage {
        domain: "Metadata",
        actions: &["labels.accept"],
        withheld_reason: Some(
            "Cloud analysis can be requested through allowlisted UI controls, but audio upload still follows the foreground policy and confirmed metadata remains review-gated.",
        ),
    },
    DomainCoverage {
        domain: "Session",
        actions: &["session.inspect"],
        withheld_reason: Some(
            "Open, Save As, and close-with-unsaved-changes remain foreground UI flows.",
        ),
    },
    DomainCoverage {
        domain: "Effect Graph",
        actions: &[],
        withheld_reason: Some(
            "Graph mutation is withheld until node and edge diffs can be previewed and revision-bound.",
        ),
    },
    DomainCoverage {
        domain: "Export",
        actions: &[],
        withheld_reason: Some(
            "Export is withheld because destination selection and overwrite confirmation must remain foreground.",
        ),
    },
    DomainCoverage {
        domain: "Recording",
        actions: &[],
        withheld_reason: Some(
            "Microphone configuration and recording start always require foreground device consent.",
        ),
    },
    DomainCoverage {
        domain: "Plugins",
        actions: &[],
        withheld_reason: Some(
            "Plugin scan, probe, and apply remain isolated UI/CLI workflows with their existing crash boundary.",
        ),
    },
    DomainCoverage {
        domain: "External metadata",
        actions: &[],
        withheld_reason: Some(
            "External table attachment remains a foreground merge-review workflow because it introduces a new data scope.",
        ),
    },
    DomainCoverage {
        domain: "Filesystem and credentials",
        actions: &[],
        withheld_reason: Some(
            "Raw filesystem, shell, credential reads, forced termination, and unrestricted paths are permanently undeclared.",
        ),
    },
];

pub fn manifest_value(registry: &ActionRegistry) -> Value {
    let domains = COVERAGE
        .iter()
        .map(|entry| {
            json!({
                "domain": entry.domain,
                "actions": entry.actions,
                "withheld_reason": entry.withheld_reason
            })
        })
        .collect::<Vec<_>>();
    json!({
        "registered_action_count": registry.iter().count(),
        "domains": domains
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn coverage_manifest_accounts_for_every_registered_action() {
        let registry = ActionRegistry::default();
        let covered = COVERAGE
            .iter()
            .flat_map(|entry| entry.actions.iter().copied())
            .collect::<BTreeSet<_>>();
        let registered = registry
            .iter()
            .map(|descriptor| descriptor.name)
            .filter(|name| *name != "actions.coverage")
            .collect::<BTreeSet<_>>();
        assert_eq!(covered, registered);
        for entry in COVERAGE {
            assert!(
                !entry.actions.is_empty() || entry.withheld_reason.is_some(),
                "{} must expose an Action or a reason",
                entry.domain
            );
        }
    }
}
