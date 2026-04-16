---
name: plugin-draft-preview
description: Use this skill when the user wants to scan plugins, inspect a candidate plugin, set a session plugin draft, preview it headlessly, or apply it with NeoWaves CLI. Trigger for requests about plugin catalog search, plugin parameter review, headless preview, or applying a plugin draft to a session target.
---

# Plugin Draft Preview

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Handle plugin work in this order:

1. Refresh the catalog
2. List and probe the candidate
3. Inspect the session draft
4. Set or adjust the draft
5. Preview
6. Apply

## Workflow

```powershell
neowaves.exe --cli plugin scan
neowaves.exe --cli plugin list --filter OTT
neowaves.exe --cli plugin probe --plugin "C:\Program Files\Common Files\VST3\OTT.vst3"
neowaves.exe --cli plugin session inspect --session C:\work\mix.nwsess
neowaves.exe --cli plugin session set --session C:\work\mix.nwsess --plugin "C:\Program Files\Common Files\VST3\OTT.vst3" --param mix=0.5
neowaves.exe --cli plugin session preview --session C:\work\mix.nwsess
neowaves.exe --cli plugin session apply --session C:\work\mix.nwsess
```

## Defaults

- Prefer `plugin probe` before `plugin session set` so parameter names are grounded in the catalog result.
- Prefer `plugin session preview` before `plugin session apply`.
- Prefer `plugin search-path add` or `plugin scan` if the catalog is empty.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for the `plugin` namespace.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the plugin workflow.
