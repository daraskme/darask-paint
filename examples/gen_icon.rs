//! アイコン生成スクリプト(ARCHITECTURE.md §16.8「アイコン」、SPEC §29
//! 「exe アイコン」)。
//!
//! `cargo run --example gen_icon` で `assets/icon.ico`(16/24/32/48/64/128/256px
//! を含むマルチサイズ ICO)を再生成し、リポジトリにコミットする。絵は
//! `src/icon.rs` の `generate_icon_rgba` を `#[path]` でそのまま取り込んで
//! 使うため、`build.rs`(exe への埋め込み)・`main.rs`(ウィンドウ/タスクバー
//! アイコン)と完全に同じ見た目になる。
//!
//! これは開発者がローカルで手動実行する生成ツールであり、配布される
//! アプリ本体の実行経路には含まれない。そのため CLAUDE.md「I/O・ユーザー
//! 入力経路で unwrap() しない」の対象外として、失敗時は素直に panic して
//! 原因を表示する。

#[path = "../src/icon.rs"]
mod icon;

use std::fs::File;
use std::io::BufWriter;

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::ExtendedColorType;

/// ARCHITECTURE.md §16.8: 「16/24/32/48/64/128/256px を含む assets/icon.ico
/// を生成」。
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    let out_path = std::path::Path::new("assets/icon.ico");
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("{} の作成に失敗しました: {e}", dir.display()));
    }

    let frames: Vec<IcoFrame<'static>> = SIZES
        .iter()
        .map(|&size| {
            let rgba = icon::generate_icon_rgba(size);
            IcoFrame::as_png(&rgba, size, size, ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("{size}px フレームの PNG エンコードに失敗しました: {e}"))
        })
        .collect();

    let file = File::create(out_path)
        .unwrap_or_else(|e| panic!("{} の作成に失敗しました: {e}", out_path.display()));
    IcoEncoder::new(BufWriter::new(file))
        .encode_images(&frames)
        .unwrap_or_else(|e| panic!("ICO のエンコードに失敗しました: {e}"));

    println!(
        "{} に {} 種のサイズを書き出しました。",
        out_path.display(),
        SIZES.len()
    );
}
