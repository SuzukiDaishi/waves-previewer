# Linux デバッグガイド（Windows前提プロジェクト向け）

このプロジェクトは Windows を主対象にしていますが、Linux でも「ある程度のデバッグ」を可能にするための手順をまとめます。

## 目的

- Linux 環境でのビルド確認
- デコード/非UIロジックの検証
- UIの最低限の起動確認（環境依存）

## 前提

- Rust toolchain が導入済み
- `pkg-config` が利用可能
- OpenGL/X11/Wayland 系の開発ライブラリが利用可能
- ALSA 開発ライブラリが利用可能（`cpal` 依存）

## 例: Ubuntu 系で必要になりやすいパッケージ

```bash
sudo apt-get update
sudo apt-get install -y \
  pkg-config \
  libasound2-dev \
  libx11-dev \
  libwayland-dev \
  libxkbcommon-dev \
  libgl1-mesa-dev
```

## Cargo feature 方針

本リポジトリでは Linux 上で `winit` プラットフォーム未選択エラーを避けるため、`glow` / `wgpu` feature に `eframe/x11` と `eframe/wayland` を含めています。

- `glow` feature: `eframe/glow + eframe/x11 + eframe/wayland`
- `wgpu` feature: `eframe/wgpu + eframe/x11 + eframe/wayland`

## 実行例

```bash
cargo check
cargo test
cargo run -- --help
```

## 注意

- Linux はデバッグ用途のサポートであり、Windows と同等の本番保証を目的としません。
- オーディオ関連は OS/ドライバ/ライブラリ差分で挙動が変わるため、最終品質確認は Windows で実施してください。

