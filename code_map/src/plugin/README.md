# src/plugin

`src/plugin/` は VST3 / CLAP plugin backend と worker protocol を持ちます。
GUI app 側の入口は `src/app/plugin_ops.rs`、effect graph 側の実行接続は `src/app/effect_graph_ops.rs` も合わせて見ます。

## Plugin Map

| パス | 主な責務 | まず見る場面 |
|---|---|---|
| `src/plugin/mod.rs` | plugin module の束ね | backend 全体の入口を見る |
| `src/plugin/protocol.rs` | worker と app の protocol | worker command / response を追加する |
| `src/plugin/client.rs` | app 側 client | plugin worker への要求送信を追う |
| `src/plugin/worker.rs` | audio processing worker | headless plugin 実行や apply を追う |
| `src/plugin/gui_worker.rs` | plugin GUI worker | plugin editor window / GUI 分離を追う |
| `src/plugin/backends/vst3.rs` | VST3 backend | VST3 scan / load / process を追う |
| `src/plugin/backends/clap.rs` | CLAP backend | CLAP scan / load / process を追う |
| `src/plugin/backends/generic.rs` | backend 共通補助 | VST3/CLAP 共通化を見る |
| `src/bin/neowaves_plugin_worker.rs` | plugin worker binary | worker 起動問題を見る |
| `src/bin/neowaves_plugin_gui_worker.rs` | plugin GUI worker binary | plugin GUI 起動問題を見る |

## Related App Modules

- `src/app/plugin_ops.rs`: scan、session draft、preview/apply の GUI/CLI 側入口。
- `src/app/effect_graph_ops.rs`: effect graph から plugin node を実行する経路。
- `src/app/ui/effect_graph.rs`: effect graph の UI。
- `src/app/cli_ops.rs`: headless plugin command 実装。
