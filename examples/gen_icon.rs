//! アイコン生成スクリプト(ARCHITECTURE.md §16.8「アイコン」、SPEC §29
//! 「exe アイコン」)。
//!
//! `cargo run --example gen_icon` で `assets/icon.ico`(16/24/32/48/64/128/256px
//! を含むマルチサイズ ICO)を再生成し、リポジトリにコミットする。絵は
//! `src/icon.rs` の `generate_icon_rgba`(`assets/icon-256.png` を縮小して
//! 角丸マスク)を `#[path]` で取り込むため、`build.rs`・`main.rs` と
//! 同じ見た目になる。
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

    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| (size, icon::generate_icon_rgba(size)))
        .collect();
    let frames: Vec<IcoFrame<'_>> = images
        .iter()
        .map(|(size, rgba)| {
            // 16〜48px は ICO 用 BMP。Explorer のファイルアイコンが
            // PNG-only ICO を拾い損ねることがある。
            if *size <= 48 {
                IcoFrame::with_encoded(
                    encode_ico_bmp_bgra(rgba, *size),
                    *size,
                    *size,
                    ExtendedColorType::Rgba8,
                )
                .unwrap_or_else(|e| panic!("{size}px フレームの BMP 化に失敗しました: {e}"))
            } else {
                IcoFrame::as_png(rgba, *size, *size, ExtendedColorType::Rgba8).unwrap_or_else(|e| {
                    panic!("{size}px フレームの PNG エンコードに失敗しました: {e}")
                })
            }
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

/// ICO 内の 32bpp BMP(BITMAPINFOHEADER + 下から上の BGRA + AND マスク)。
fn encode_ico_bmp_bgra(rgba: &[u8], size: u32) -> Vec<u8> {
    let xor_len = (size as usize)
        .saturating_mul(size as usize)
        .saturating_mul(4);
    let and_row = size.div_ceil(32) * 4;
    let and_len = (and_row as usize).saturating_mul(size as usize);
    let mut out = Vec::with_capacity(40 + xor_len + and_len);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(xor_len as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.push(rgba[i + 2]);
            out.push(rgba[i + 1]);
            out.push(rgba[i]);
            out.push(rgba[i + 3]);
        }
    }
    out.resize(out.len() + and_len, 0);
    out
}
