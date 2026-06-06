# NeoWaves Code Map

このフォルダは、初見の実装者が「どの機能はどこを見るか」を辿るためのコードマップです。
詳細な計画文書は `docs/` に置き、ここでは現在の Rust ソースの探索入口をフォルダツリーとして整理します。

表記方針:
- 「Session」は `.nwsess` の状態保存ファイルを指します。
- コード上の `project*` 命名は legacy で、意味としては Session 保存・復元です。
- 大きな UI 面や backend は段階分割中のため、巨大ファイルには責務境界も併記します。

## Tree

```text
code_map/
  README.md
  src/
    README.md
    app/
      README.md
      ops/
        README.md
      render/
        README.md
      ui/
        README.md
    plugin/
      README.md
  flows/
    README.md
  change_lookup/
    README.md
  performance/
    README.md
```

## Entry Points

- [src](src/README.md): top-level crate map。`main.rs`、`lib.rs`、`cli.rs`、`app.rs`、audio 系、marker 系。
- [src/app](src/app/README.md): GUI app の中核 state、frame orchestration、型、debug、theme。
- [src/app/ops](src/app/ops/README.md): `*_ops.rs` 系の操作ロジック、worker drain、I/O 起動。
- [src/app/ui](src/app/ui/README.md): egui UI 面。topbar、list、editor、effect graph、debug など。
- [src/app/render](src/app/render/README.md): 波形、spectrogram、overlay、music feature 描画。
- [src/plugin](src/plugin/README.md): VST3 / CLAP、worker protocol、plugin GUI worker。
- [flows](flows/README.md): 起動、scan、検索、list preview、editor、session、CLI の主要フロー。
- [change_lookup](change_lookup/README.md): 変更したい内容から見るファイルを逆引きする表。
- [performance](performance/README.md): UI / Audio 高速化調査で読む順番と巨大ファイルの扱い。

## Related Docs

- `docs/REFACTOR_PLAN.md`: `app.rs` / `logic.rs` の分割方針と関数マップ。
- `docs/PERFORMANCE_SCALABILITY_PLAN.md`: 大規模データや性能改善の方針。
- `docs/UI_PERFORMANCE_IMPROVEMENT_CANDIDATES_20260606.md`: UI / Audio 性能改善候補の具体リスト。
- `docs/NWPROJ_PLAN.md`: Session 保存形式の設計背景。
- `docs/CLI_COMMAND_REFERENCE.md`: headless CLI の利用・検証入口。
- `AGENTS.md`: この repo での作業原則、Cargo workflow、重要な注意点。
