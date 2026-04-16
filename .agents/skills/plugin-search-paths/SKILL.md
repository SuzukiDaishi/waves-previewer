---
name: plugin-search-paths
description: Use this skill when the user wants to inspect or manage NeoWaves plugin search paths before scanning plugins. Trigger for requests about missing plugins, plugin discovery directories, adding or removing VST paths, resetting search paths, or preparing the catalog for a later scan.
---

# Plugin Search Paths

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Control the plugin discovery directories before running `plugin scan`.

## Workflow

List current paths:

```powershell
neowaves.exe --cli plugin search-path list
```

Add a directory:

```powershell
neowaves.exe --cli plugin search-path add --path "C:\Program Files\Common Files\VST3"
```

Remove a directory by path:

```powershell
neowaves.exe --cli plugin search-path remove --path "C:\Program Files\Common Files\VST3"
```

Remove by index:

```powershell
neowaves.exe --cli plugin search-path remove --index 0
```

Reset to defaults:

```powershell
neowaves.exe --cli plugin search-path reset
```

Refresh the catalog after changes:

```powershell
neowaves.exe --cli plugin scan
```

## Defaults

- Prefer `list` before changing paths so the result is auditable.
- Prefer `plugin scan` immediately after changing paths.
- Use path removal when the exact directory is known; use index removal only when the returned list is stable and short.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `plugin search-path` and `plugin scan`.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the plugin workflow.
