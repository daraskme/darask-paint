# Darask Paint

超高速起動・軽量・シンプルな Windows 用ラスタ画像編集ソフト(Krita の起動待ちにうんざりした人向け)。
Rust + eframe/egui。単一 exe。UI は日本語。

## ドキュメント
- [docs/SPEC.md](docs/SPEC.md) — 機能仕様(何を作るか)。迷ったらここが正。
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — 設計(どう作るか)、モジュール構成、マイルストーン。

## コマンド
- ビルド: `cargo build`(開発)/ `cargo build --release`(配布)
- テスト: `cargo test`
- 起動ベンチ兼スモークテスト(GUI 対話不要・自動終了): PowerShell で `$env:DARASK_BENCH='1'; cargo run` → `bench.txt` に起動ミリ秒が書かれる

## 鉄則
- 最優先は「起動速度・軽さ・シンプルさ」。機能追加より起動 300ms 以内(目標 160ms)を守る。
- 依存 crate は eframe / image / rfd / arboard / ab_glyph のみ。build-dependency は winresource のみ。追加しない。
- ビルドはエラー 0・警告 0。`cargo clippy --all-targets -- -D warnings` グリーン(v4 以降)。`cargo fmt` 適用。純粋ロジックにはテスト。
- I/O・ユーザー入力経路で `unwrap()` しない。パニックせずトーストで通知。
- 無条件の `request_repaint()` 禁止(アイドル CPU 0% 要件)。
- CI は GitHub Actions(.github/workflows/)。リリースは `v*` タグ push で自動作成。
