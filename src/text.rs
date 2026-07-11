//! テキストツールのラスタライズ(SPEC §19、ARCHITECTURE.md §15.3)。
//!
//! `ab_glyph` でシステム日本語フォント(§9 と同じ探索順・同じファイル、
//! SPEC §19: 「フォントは UI と同じシステム日本語フォント 1 種のみ」)から
//! 文字列をアンチエイリアス付きでラスタライズする純関数を提供する。
//! `ab_glyph::FontRef` はバイト列を borrow するだけの薄い型なので、
//! `app.rs` が保持する `Arc<Vec<u8>>` から呼び出しのたびに軽量に構築し直す
//! (TTC のテーブルオフセットを読むだけで、グリフごとのラスタライズと比べて
//! 無視できるコスト)。`FontRef` 自体を `DaraskApp` に保持しないのはバイト列
//! への自己参照になってしまうため(ARCHITECTURE.md §15.3: 「App はフォント
//! バイト列を `Arc<Vec<u8>>` で保持し…`FontRef` を作る」)。
//!
//! 落とし穴(ARCHITECTURE.md §15.6-4)の注記: egui 0.35 は内部のテキスト
//! シェイピングに `ab_glyph` ではなく `harfrust`/`read-fonts` を使っている
//! (`cargo tree -i ab_glyph` で確認済み、依存グラフに egui からの経路は
//! 無い)。そのため CLAUDE.md/ARCHITECTURE.md が前提とする「egui と同じ
//! バージョンに `=` 固定」は文字どおりには実行できない — 該当バージョンが
//! 存在しない。実装では `ab_glyph` 自身のバージョンを `=` で固定すること
//! (Cargo.toml 参照)でドリフトを防ぐという代替判断をした(挙動仕様は
//! 変えていない、CLAUDE.md の「実 API に合わせる」方針の延長)。

use ab_glyph::{point, Font, FontRef, GlyphId, ScaleFont};

use crate::raster;

/// 日本語フォントの探索順(ARCHITECTURE.md §9)。最初に読めたものを使う。
/// フォントはバンドルしない(バイナリサイズ・起動時間のため)。UI 表示用の
/// フォント読み込み(`app.rs::setup_japanese_fonts`)とテキストツールの
/// ラスタライズ(`rasterize_text`)の両方がこの一箇所を参照する(見た目の
/// 統一、SPEC §19: 「フォントは UI と同じシステム日本語フォント 1 種のみ」)。
pub(crate) const JAPANESE_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\YuGothM.ttc",
    r"C:\Windows\Fonts\meiryo.ttc",
    r"C:\Windows\Fonts\msgothic.ttc",
];

/// `JAPANESE_FONT_CANDIDATES` を順に試し、最初に読めたバイト列を返す。
/// 全滅した場合(Win11 では起きない想定、ARCHITECTURE.md §9-4)は `None`
/// (パニックしない、CLAUDE.md 鉄則)。
pub(crate) fn load_font_bytes() -> Option<Vec<u8>> {
    JAPANESE_FONT_CANDIDATES
        .iter()
        .find_map(|path| std::fs::read(path).ok())
}

/// `.ttc`(フォントコレクション)内でのインデックス。ARCHITECTURE.md §9:
/// 「通常 0」。UI 日本語フォント読み込みと同じ index を使う。
const FONT_COLLECTION_INDEX: u32 = 0;

/// ARCHITECTURE.md §15.3: 「行送り = (ascent−descent+line_gap)×1.1 目安」。
/// `ScaleFont::height()` は `ascent - descent` を返すので、これに
/// `line_gap` を足して 1.1 倍する。
const LINE_HEIGHT_FACTOR: f32 = 1.1;

/// `text` を `font_bytes`(TTF/TTC のバイト列)を使って `px_size` ピクセルの
/// アンチエイリアス付きでラスタライズし、`color`(straight-alpha RGBA)で
/// 塗る。戻り値は `(幅, 高さ, RGBA8 straight-alpha バッファ)`。
///
/// - 空文字列、フォント解析の失敗、レイアウト結果の幅/高さが 0 になる場合は
///   `(0, 0, Vec::new())` を返す(SPEC §19: 「空文字列の確定は何もしない」の
///   判定に呼び出し側がそのまま使える)。
/// - 複数行対応(`\n` 区切り、SPEC §19)。行送りはフォントメトリクス準拠。
/// - `h_advance` + カーニングでグリフを横に並べる。
/// - 境界チェック済みでパニックしない(CLAUDE.md 鉄則: I/O・ユーザー入力
///   経路で unwrap しない)。異常なフォント/巨大なグリフでラスタライズ結果が
///   計算済みのバッファ範囲をはみ出しても、はみ出した画素は黙って捨てる。
pub fn rasterize_text(
    font_bytes: &[u8],
    text: &str,
    px_size: f32,
    color: [u8; 4],
) -> (u32, u32, Vec<u8>) {
    if text.is_empty() {
        return (0, 0, Vec::new());
    }
    let px_size = if px_size.is_finite() {
        px_size.max(1.0)
    } else {
        1.0
    };
    let Ok(font) = FontRef::try_from_slice_and_index(font_bytes, FONT_COLLECTION_INDEX) else {
        return (0, 0, Vec::new());
    };
    let scaled = font.as_scaled(px_size);
    let line_height = ((scaled.height() + scaled.line_gap()) * LINE_HEIGHT_FACTOR).max(1.0);
    let ascent = scaled.ascent();

    struct Positioned {
        id: GlyphId,
        x: f32,
        y: f32,
    }
    let mut positioned: Vec<Positioned> = Vec::new();
    let mut max_x = 0.0f32;
    let lines: Vec<&str> = text.split('\n').collect();
    for (row, line) in lines.iter().enumerate() {
        let mut cursor_x = 0.0f32;
        let baseline_y = ascent + row as f32 * line_height;
        let mut prev: Option<GlyphId> = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(prev_id) = prev {
                cursor_x += scaled.kern(prev_id, id);
            }
            positioned.push(Positioned {
                id,
                x: cursor_x,
                y: baseline_y,
            });
            cursor_x += scaled.h_advance(id);
            prev = Some(id);
        }
        max_x = max_x.max(cursor_x);
    }

    let width = max_x.ceil().max(0.0) as u32;
    let height = (line_height * lines.len() as f32).ceil().max(0.0) as u32;
    if width == 0 || height == 0 {
        return (0, 0, Vec::new());
    }

    let mut buffer = vec![0u8; width as usize * height as usize * 4];
    for glyph in positioned {
        let g = glyph
            .id
            .with_scale_and_position(px_size, point(glyph.x, glyph.y));
        let Some(outlined) = font.outline_glyph(g) else {
            continue;
        };
        let bounds = outlined.px_bounds();
        let origin_x = bounds.min.x.floor() as i32;
        let origin_y = bounds.min.y.floor() as i32;
        outlined.draw(|gx, gy, coverage| {
            let x = origin_x + gx as i32;
            let y = origin_y + gy as i32;
            if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
                return;
            }
            let idx = (y as usize * width as usize + x as usize) * 4;
            let Some(dst) = buffer.get(idx..idx + 4) else {
                return;
            };
            let existing = [dst[0], dst[1], dst[2], dst[3]];
            let alpha = (color[3] as f32 * coverage.clamp(0.0, 1.0))
                .round()
                .clamp(0.0, 255.0) as u8;
            let blended = raster::blend_over(existing, [color[0], color[1], color[2], alpha]);
            buffer[idx..idx + 4].copy_from_slice(&blended);
        });
    }

    (width, height, buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 開発機(Windows)に必ず存在するシステム日本語フォントを読み込む。
    /// 一つも読めなければテストをスキップする(このプロジェクトは Windows
    /// 専用だが、フォント欠如でビルド自体は壊さない、ARCHITECTURE.md §9-4
    /// と同じ「見つからなければ警告して続行」方針をテストにも適用する)。
    fn load_test_font() -> Option<Vec<u8>> {
        load_font_bytes()
    }

    #[test]
    fn empty_string_produces_nothing() {
        let (w, h, pixels) = rasterize_text(&[0u8; 4], "", 24.0, [0, 0, 0, 255]);
        assert_eq!((w, h), (0, 0));
        assert!(pixels.is_empty());
    }

    #[test]
    fn invalid_font_bytes_produce_nothing_without_panicking() {
        let (w, h, pixels) = rasterize_text(&[1, 2, 3, 4], "A", 24.0, [0, 0, 0, 255]);
        assert_eq!((w, h), (0, 0));
        assert!(pixels.is_empty());
    }

    #[test]
    fn whitespace_with_invalid_font_does_not_panic() {
        // 空文字列(len == 0)だけが SPEC §19 の「何もしない」対象。空白は
        // 早期リターンの対象外だが、フォント解析に失敗すれば同じく
        // (0,0,空) になり、パニックしないことを確認する。
        let (w, h, pixels) = rasterize_text(&[0u8; 4], " ", 24.0, [0, 0, 0, 255]);
        assert_eq!((w, h), (0, 0));
        assert!(pixels.is_empty());
    }

    #[test]
    fn ascii_text_produces_nonzero_pixels() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, pixels) = rasterize_text(&font, "A", 24.0, [0, 0, 0, 255]);
        assert!(w > 0 && h > 0);
        assert!(
            pixels.chunks_exact(4).any(|p| p[3] > 0),
            "expected at least one covered pixel"
        );
    }

    #[test]
    fn japanese_glyph_produces_nonzero_pixels() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, pixels) = rasterize_text(&font, "あ", 32.0, [255, 0, 0, 255]);
        assert!(w > 0 && h > 0);
        assert!(
            pixels.chunks_exact(4).any(|p| p[3] > 0),
            "expected at least one covered pixel for a Japanese glyph"
        );
    }

    #[test]
    fn multiline_text_is_taller_than_single_line() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (_, h1, _) = rasterize_text(&font, "A", 24.0, [0, 0, 0, 255]);
        let (_, h2, _) = rasterize_text(&font, "A\nB", 24.0, [0, 0, 0, 255]);
        assert!(h2 > h1, "two lines ({h2}) should be taller than one ({h1})");
    }

    #[test]
    fn color_alpha_scales_glyph_coverage() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, opaque) = rasterize_text(&font, "A", 40.0, [10, 20, 30, 255]);
        let (_, _, half) = rasterize_text(&font, "A", 40.0, [10, 20, 30, 128]);
        assert_eq!((w, h), (w, h));
        let max_opaque_alpha = opaque.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        let max_half_alpha = half.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        assert!(
            max_half_alpha < max_opaque_alpha,
            "half-alpha color should produce lower peak coverage alpha"
        );
    }
}
