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

/// v12 §52: 縦書きで**セル中心を軸に 90° 回転**させる文字(manga_tool の
/// `_VERT_ROTATE` をそのまま移植。全角のみ)。
const VERTICAL_ROTATED_CHARS: &str = "ー―─━…‥〜～「」『』（）【】〔〕〈〉《》｛｝";

/// v12 §52: 縦書きでセルの**右上寄せ**にする句読点(manga_tool の
/// `_VERT_PUNCT`)。
const VERTICAL_PUNCT_CHARS: &str = "。、";

/// v12 §52: ラスタライズ結果の 1 辺の上限。文書の上限(`MAX_DIMENSION`)と
/// 同じにして、確定後の浮動片が扱えない大きさにならないようにする。
const MAX_TEXT_DIMENSION: u32 = crate::document::MAX_DIMENSION;

/// v12 §52: ラスタライズの失敗理由(SPEC §52: 「寸法・確保は checked 演算と
/// し、失敗はエラーを返す」)。呼び出し側は `message()` をトーストに出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRasterError {
    /// フォントのバイト列を解析できなかった(壊れたフォント)。
    Font,
    /// 寸法が上限を超える、または確保に失敗した。
    TooLarge,
}

impl TextRasterError {
    /// トースト用の日本語メッセージ。
    pub fn message(self) -> &'static str {
        match self {
            TextRasterError::Font => "フォントを読み込めないため、テキストを描画できません",
            TextRasterError::TooLarge => {
                "テキストが大きすぎます(文字数・サイズ・文字間を減らしてください)"
            }
        }
    }
}

/// ラスタライズ結果 `(幅, 高さ, RGBA8 straight-alpha)`。幅・高さが 0 のときは
/// バッファも空(SPEC §19: 「空文字列の確定は何もしない」の判定に使う)。
type RasterizedText = (u32, u32, Vec<u8>);

/// `px_size` を有限かつ 1.0 以上へ正規化する(NaN/∞ 対策)。
fn sanitize_px_size(px_size: f32) -> f32 {
    if px_size.is_finite() {
        px_size.max(1.0)
    } else {
        1.0
    }
}

/// 0 以上の有限値へ正規化する(文字間・行間はマイナスにしない)。
fn sanitize_spacing(spacing: f32) -> f32 {
    if spacing.is_finite() {
        spacing.max(0.0)
    } else {
        0.0
    }
}

/// f32 のレイアウト寸法を u32 の画素数へ(上限超過は `TooLarge`)。
fn to_dimension(value: f32) -> Result<u32, TextRasterError> {
    if !value.is_finite() || value < 0.0 {
        return Err(TextRasterError::TooLarge);
    }
    let ceil = value.ceil();
    if ceil > MAX_TEXT_DIMENSION as f32 {
        return Err(TextRasterError::TooLarge);
    }
    Ok(ceil as u32)
}

/// SPEC §52: 「寸法・確保は checked 演算とし、失敗はエラーを返す」。
/// `width*height*4` を checked で計算し、`try_reserve` で確保する。
fn allocate_buffer(width: u32, height: u32) -> Result<Vec<u8>, TextRasterError> {
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(TextRasterError::TooLarge)?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| TextRasterError::TooLarge)?;
    buffer.resize(len, 0);
    Ok(buffer)
}

/// ラスタライズ先のバッファ(寸法・塗り色つき)。グリフ描画のヘルパーを
/// メソッドとして持たせ、横書き・縦書きの両方から同じ経路で書く。
struct GlyphTarget<'a> {
    buffer: &'a mut [u8],
    width: u32,
    height: u32,
    color: [u8; 4],
}

impl GlyphTarget<'_> {
    /// カバレッジ 1 画素を合成する(範囲外は捨てる = パニックしない)。
    fn blend(&mut self, x: i32, y: i32, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        let Some(dst) = self.buffer.get(idx..idx + 4) else {
            return;
        };
        let existing = [dst[0], dst[1], dst[2], dst[3]];
        let alpha = (self.color[3] as f32 * coverage.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, 255.0) as u8;
        let blended = raster::blend_over(
            existing,
            [self.color[0], self.color[1], self.color[2], alpha],
        );
        self.buffer[idx..idx + 4].copy_from_slice(&blended);
    }

    /// グリフを `(x, baseline_y)` に描く(横書き・縦書きの非回転文字で共通)。
    fn draw_glyph(
        &mut self,
        font: &FontRef<'_>,
        id: GlyphId,
        px_size: f32,
        x: f32,
        baseline_y: f32,
    ) {
        let glyph = id.with_scale_and_position(px_size, point(x, baseline_y));
        let Some(outlined) = font.outline_glyph(glyph) else {
            return;
        };
        let bounds = outlined.px_bounds();
        let origin_x = bounds.min.x.floor() as i32;
        let origin_y = bounds.min.y.floor() as i32;
        outlined.draw(|gx, gy, coverage| {
            self.blend(origin_x + gx as i32, origin_y + gy as i32, coverage);
        });
    }

    /// v12 §52: グリフを**セル中心 `(cx, cy)` で 90° 回転**して描く。
    ///
    /// グリフのカバレッジをいったん小バッファへラスタライズし、
    /// `rotate_coverage_cw`(時計回り 90°)で書き出す。これにより「ー」の
    /// ような横長の記号が縦長になる(SPEC §52 の回転文字)。
    ///
    /// 中間バッファも `try_reserve` で確保し、異常なグリフ寸法(壊れた
    /// フォント・極端な `px_size`)は `TooLarge` として返す(確保前に弾く)。
    fn draw_glyph_rotated(
        &mut self,
        font: &FontRef<'_>,
        id: GlyphId,
        px_size: f32,
        cx: f32,
        cy: f32,
    ) -> Result<(), TextRasterError> {
        let glyph = id.with_scale_and_position(px_size, point(0.0, 0.0));
        let Some(outlined) = font.outline_glyph(glyph) else {
            return Ok(());
        };
        let bounds = outlined.px_bounds();
        let gw = to_dimension(bounds.width())? as usize;
        let gh = to_dimension(bounds.height())? as usize;
        if gw == 0 || gh == 0 {
            return Ok(());
        }
        let len = gw.checked_mul(gh).ok_or(TextRasterError::TooLarge)?;
        let mut coverage: Vec<f32> = Vec::new();
        coverage
            .try_reserve_exact(len)
            .map_err(|_| TextRasterError::TooLarge)?;
        coverage.resize(len, 0.0);
        outlined.draw(|gx, gy, c| {
            let (gx, gy) = (gx as usize, gy as usize);
            if gx >= gw || gy >= gh {
                return;
            }
            coverage[gy * gw + gx] = c;
        });

        // 回転後は縦横が入れ替わる。中心 (cx, cy) に合わせて配置する。
        let (rot_w, rot_h) = (gh, gw);
        let origin_x = (cx - rot_w as f32 / 2.0).floor() as i32;
        let origin_y = (cy - rot_h as f32 / 2.0).floor() as i32;
        for gy in 0..gh {
            for gx in 0..gw {
                let c = coverage[gy * gw + gx];
                if c <= 0.0 {
                    continue;
                }
                let (nx, ny) = rotate_coverage_cw(gx, gy, gh);
                self.blend(origin_x + nx as i32, origin_y + ny as i32, c);
            }
        }
        Ok(())
    }
}

/// v12 §52: カバレッジ座標の**時計回り 90°**写像(`(gx, gy)` →
/// `(gh - 1 - gy, gx)`。= transpose + 水平反転)。回転後の寸法は
/// `(gh, gw)` と縦横が入れ替わる。
///
/// 純関数として切り出してあるので、非対称なカバレッジを直接与えて
/// 「どちら回りか」をテストできる(縦横比だけでは向きが判別できない)。
fn rotate_coverage_cw(gx: usize, gy: usize, gh: usize) -> (usize, usize) {
    (gh.saturating_sub(1).saturating_sub(gy), gx)
}

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
    char_spacing: f32,
    line_spacing: f32,
) -> Result<RasterizedText, TextRasterError> {
    if text.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let px_size = sanitize_px_size(px_size);
    let char_spacing = sanitize_spacing(char_spacing);
    let line_spacing = sanitize_spacing(line_spacing);
    let font = FontRef::try_from_slice_and_index(font_bytes, FONT_COLLECTION_INDEX)
        .map_err(|_| TextRasterError::Font)?;
    let scaled = font.as_scaled(px_size);
    // v12 §52: 行間は行送りへの加算(字送りへの加算は下の `char_spacing`)。
    let line_height =
        ((scaled.height() + scaled.line_gap()) * LINE_HEIGHT_FACTOR).max(1.0) + line_spacing;
    let ascent = scaled.ascent();

    // 行数だけ先に数え、上限を超える入力は**確保する前に**弾く
    // (`line_height >= 1` なので、行数が上限を超えれば高さも必ず超える)。
    let line_count = text.split('\n').count();
    if line_count > MAX_TEXT_DIMENSION as usize {
        return Err(TextRasterError::TooLarge);
    }

    // v12 §52(追いレビュー②): グリフ位置を `Vec` に貯めず、
    // 「寸法計算パス → 描画パス」の 2 パスにする(巨大な貼り付けで
    // 未検査の中間確保が起きないようにするため)。両パスで同じ
    // クロージャ `advance_line` を使い、レイアウトのずれを防ぐ。
    let layout_line = |line: &str, mut on_glyph: Option<&mut dyn FnMut(GlyphId, f32)>| -> f32 {
        let mut cursor_x = 0.0f32;
        let mut prev: Option<GlyphId> = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(prev_id) = prev {
                cursor_x += scaled.kern(prev_id, id);
                // v12 §52: 文字間は字送りへの加算(文字と文字の**間**にだけ
                // 入れるので、行末に余白が付かない)。
                cursor_x += char_spacing;
            }
            if let Some(callback) = on_glyph.as_deref_mut() {
                callback(id, cursor_x);
            }
            cursor_x += scaled.h_advance(id);
            prev = Some(id);
        }
        cursor_x
    };

    let mut max_x = 0.0f32;
    for line in text.split('\n') {
        max_x = max_x.max(layout_line(line, None));
    }

    let width = to_dimension(max_x)?;
    let height = to_dimension(line_height * line_count as f32)?;
    if width == 0 || height == 0 {
        return Ok((0, 0, Vec::new()));
    }

    let mut buffer = allocate_buffer(width, height)?;
    let mut target = GlyphTarget {
        buffer: &mut buffer,
        width,
        height,
        color,
    };
    for (row, line) in text.split('\n').enumerate() {
        let baseline_y = ascent + row as f32 * line_height;
        let mut draw = |id: GlyphId, x: f32| {
            target.draw_glyph(&font, id, px_size, x, baseline_y);
        };
        layout_line(line, Some(&mut draw));
    }

    Ok((width, height, buffer))
}

/// v12 §52: 縦書きのラスタライズ(manga_tool 準拠の簡略移植 — ピクセル完全
/// 一致は謳わない)。
///
/// - `\n` で**列**に分割し、**最初の列が最も右**(Enter で新しい列が左に増える)。
///   列間 = `line_spacing`。
/// - 縦の字送りは**固定送り**(`ascent - descent + line_gap` + `char_spacing`)。
///   グリフごとの advance ではない(manga_tool の `fm.height() + spacing` 準拠)。
/// - 列の幅はその列の最大 advance。**空列は全角 1 文字ぶんの幅**のセルを占める。
/// - 回転文字(`VERTICAL_ROTATED_CHARS`)はセル中心で 90° 回転、句読点
///   (`VERTICAL_PUNCT_CHARS`)はセルの右上寄せ、その他はセル内で水平センタリング。
///
/// 戻り値・エラーの規則は `rasterize_text` と同じ。
pub fn rasterize_text_vertical(
    font_bytes: &[u8],
    text: &str,
    px_size: f32,
    color: [u8; 4],
    char_spacing: f32,
    line_spacing: f32,
) -> Result<RasterizedText, TextRasterError> {
    if text.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let px_size = sanitize_px_size(px_size);
    let char_spacing = sanitize_spacing(char_spacing);
    let line_spacing = sanitize_spacing(line_spacing);
    let font = FontRef::try_from_slice_and_index(font_bytes, FONT_COLLECTION_INDEX)
        .map_err(|_| TextRasterError::Font)?;
    let scaled = font.as_scaled(px_size);
    let ascent = scaled.ascent();
    // セルの「文字ぶん」の高さ(回転・句読点の中心合わせに使う)と、実際の
    // 字送り(= セル高 + 文字間)。
    let cell_core = (scaled.height() + scaled.line_gap()).max(1.0);
    let cell_advance = cell_core + char_spacing;
    // 全角 1 文字ぶんの幅(空列のセル幅)。表意文字スペース(U+3000)の
    // advance を使い、フォントに無ければセル高で代用する。
    let full_width = {
        let advance = scaled.h_advance(font.glyph_id('\u{3000}'));
        if advance > 0.0 {
            advance
        } else {
            cell_core
        }
    };

    // v12 §52(追いレビュー②): 列ごとの文字列は保持せず(`Vec<Vec<char>>` を
    // 作らない)、列幅だけを検査付きで確保した `Vec<f32>` に持って、描画時に
    // 文字列を再走査する。列数は先に数え、上限超過は**確保する前に**弾く
    // (列幅は必ず 1px 以上なので、列数が上限を超えれば幅も必ず超える)。
    let column_count = text.split('\n').count();
    if column_count > MAX_TEXT_DIMENSION as usize {
        return Err(TextRasterError::TooLarge);
    }
    let mut col_widths: Vec<f32> = Vec::new();
    col_widths
        .try_reserve_exact(column_count)
        .map_err(|_| TextRasterError::TooLarge)?;
    let mut max_cells = 1usize;
    for column in text.split('\n') {
        let mut width = 0.0f32;
        let mut cells = 0usize;
        for ch in column.chars() {
            width = width.max(scaled.h_advance(font.glyph_id(ch)));
            cells += 1;
        }
        // 空列は全角 1 文字ぶんの幅のセルを占める(SPEC §52)。
        col_widths.push(width.max(if cells == 0 { full_width } else { 1.0 }));
        max_cells = max_cells.max(cells.max(1));
    }

    let total_width: f32 =
        col_widths.iter().sum::<f32>() + line_spacing * (column_count.saturating_sub(1)) as f32;
    // 空列も 1 セルぶんの高さを占める(SPEC §52)。末尾の文字間は含めない。
    let total_height = cell_advance * max_cells as f32 - char_spacing;

    let width = to_dimension(total_width)?;
    let height = to_dimension(total_height)?;
    if width == 0 || height == 0 {
        return Ok((0, 0, Vec::new()));
    }
    let mut buffer = allocate_buffer(width, height)?;
    let mut target = GlyphTarget {
        buffer: &mut buffer,
        width,
        height,
        color,
    };

    // 右から左へ列を配置する(最初の列が最も右)。
    let mut right = total_width;
    for (col_index, column) in text.split('\n').enumerate() {
        let col_width = col_widths.get(col_index).copied().unwrap_or(full_width);
        let x = right - col_width;
        for (cell, ch) in column.chars().enumerate() {
            let id = font.glyph_id(ch);
            let baseline_y = ascent + cell as f32 * cell_advance;
            if VERTICAL_ROTATED_CHARS.contains(ch) {
                // セル中心を軸に 90° 回転。
                let cx = x + col_width / 2.0;
                let cy = baseline_y - ascent + cell_core / 2.0;
                target.draw_glyph_rotated(&font, id, px_size, cx, cy)?;
            } else if VERTICAL_PUNCT_CHARS.contains(ch) {
                // 句読点は右上寄せ(描画起点を右へ半セル・上へ半セルずらす)。
                let punct_x = x + col_width / 2.0;
                let mut punct_y = baseline_y - cell_core / 2.0;
                // 列の 1 セル目だけは、半セル上げるとインクが画像の上端より
                // 外へ出て切れてしまう(参照実装は余白付きのウィジェットへ
                // 描くので気づかない)。はみ出したぶんだけ下げて、インクが
                // セル内に収まるようにする(他のセルでは補正は働かない)。
                let probe = id.with_scale_and_position(px_size, point(punct_x, punct_y));
                if let Some(outlined) = font.outline_glyph(probe) {
                    let top = outlined.px_bounds().min.y;
                    if top < 0.0 {
                        punct_y -= top;
                    }
                }
                target.draw_glyph(&font, id, px_size, punct_x, punct_y);
            } else {
                // セル内で水平センタリング。
                let advance = scaled.h_advance(id);
                target.draw_glyph(
                    &font,
                    id,
                    px_size,
                    x + (col_width - advance) / 2.0,
                    baseline_y,
                );
            }
        }
        right = x - line_spacing;
    }

    Ok((width, height, buffer))
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

    /// 既定オプション(文字間 0・行間 0)の横書き。
    fn plain(font: &[u8], text: &str, px: f32) -> Result<RasterizedText, TextRasterError> {
        rasterize_text(font, text, px, [0, 0, 0, 255], 0.0, 0.0)
    }

    /// 既定オプションの縦書き。
    fn vertical(font: &[u8], text: &str, px: f32) -> Result<RasterizedText, TextRasterError> {
        rasterize_text_vertical(font, text, px, [0, 0, 0, 255], 0.0, 0.0)
    }

    /// 不透明画素(インク)の外接矩形 `(x0, y0, x1, y1)`。無ければ `None`。
    fn ink_bounds(w: u32, h: u32, pixels: &[u8]) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                if pixels.get(idx + 3).copied().unwrap_or(0) == 0 {
                    continue;
                }
                bounds = Some(match bounds {
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                    None => (x, y, x + 1, y + 1),
                });
            }
        }
        bounds
    }

    #[test]
    fn empty_string_produces_nothing() {
        let (w, h, pixels) = plain(&[0u8; 4], "", 24.0).expect("空文字列はエラーではない");
        assert_eq!((w, h), (0, 0));
        assert!(pixels.is_empty());
        let (w, h, pixels) = vertical(&[0u8; 4], "", 24.0).expect("縦書きも同じ");
        assert_eq!((w, h), (0, 0));
        assert!(pixels.is_empty());
    }

    #[test]
    fn invalid_font_bytes_produce_an_error_without_panicking() {
        // v12 §52 で `Result` 化した(以前は黙って (0,0,空) を返していた)。
        assert_eq!(plain(&[1, 2, 3, 4], "A", 24.0), Err(TextRasterError::Font));
        assert_eq!(
            vertical(&[1, 2, 3, 4], "あ", 24.0),
            Err(TextRasterError::Font)
        );
    }

    #[test]
    fn whitespace_with_invalid_font_does_not_panic() {
        assert_eq!(plain(&[0u8; 4], " ", 24.0), Err(TextRasterError::Font));
    }

    #[test]
    fn ascii_text_produces_nonzero_pixels() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, pixels) = plain(&font, "A", 24.0).expect("ラスタライズできる");
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
        let (w, h, pixels) =
            rasterize_text(&font, "あ", 32.0, [255, 0, 0, 255], 0.0, 0.0).expect("ok");
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
        let (_, h1, _) = plain(&font, "A", 24.0).expect("ok");
        let (_, h2, _) = plain(&font, "A\nB", 24.0).expect("ok");
        assert!(h2 > h1, "two lines ({h2}) should be taller than one ({h1})");
    }

    #[test]
    fn color_alpha_scales_glyph_coverage() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, opaque) =
            rasterize_text(&font, "A", 40.0, [10, 20, 30, 255], 0.0, 0.0).expect("ok");
        let (_, _, half) =
            rasterize_text(&font, "A", 40.0, [10, 20, 30, 128], 0.0, 0.0).expect("ok");
        assert_eq!((w, h), (w, h));
        let max_opaque_alpha = opaque.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        let max_half_alpha = half.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0);
        assert!(
            max_half_alpha < max_opaque_alpha,
            "half-alpha color should produce lower peak coverage alpha"
        );
    }

    // -- v12 §52(追いレビュー): 旧実装との一致・回転の向き・句読点の量 ----

    /// v11(Phase 2 まで)の `rasterize_text` そのままの参照実装。
    /// 文字間・行間 0 の横書きが**寸法・RGBA バッファまで完全一致**すること
    /// を確かめるためだけに置く(2 パス化・checked 確保のリファクタで
    /// 見た目が変わっていないことの回帰検知)。
    fn legacy_rasterize_text(
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

    #[test]
    fn horizontal_without_spacing_matches_the_pre_v12_implementation_byte_for_byte() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let cases: [(&str, f32, [u8; 4]); 5] = [
            ("A", 24.0, [0, 0, 0, 255]),
            ("Hello, world!", 18.0, [10, 20, 30, 255]),
            ("あいうえお", 32.0, [255, 0, 0, 255]),
            ("一行目\n二行目\n三行目", 24.0, [0, 0, 255, 200]),
            ("Mixed 混在 123", 40.0, [7, 8, 9, 128]),
        ];
        for (text, px, color) in cases {
            let legacy = legacy_rasterize_text(&font, text, px, color);
            let current = rasterize_text(&font, text, px, color, 0.0, 0.0).expect("ok");
            assert_eq!(
                (current.0, current.1),
                (legacy.0, legacy.1),
                "寸法が旧実装と一致しない: {text:?}"
            );
            assert!(
                current.2 == legacy.2,
                "RGBA バッファが旧実装と一致しない: {text:?}"
            );
        }
    }

    #[test]
    fn rotate_coverage_cw_maps_an_asymmetric_pattern_clockwise() {
        // 2 列 × 3 行(gw=2, gh=3)の非対称パターン。値は「元の位置」を表す。
        //   src:            回転後(時計回り 90°、寸法は 3x2):
        //   (0,0) (1,0)      (0,2) (0,1) (0,0)
        //   (0,1) (1,1)  =>  (1,2) (1,1) (1,0)
        //   (0,2) (1,2)
        let (gw, gh) = (2usize, 3usize);
        let mut rotated = vec![None; gw * gh];
        for gy in 0..gh {
            for gx in 0..gw {
                let (nx, ny) = rotate_coverage_cw(gx, gy, gh);
                // 回転後の寸法は (gh, gw) = (3, 2)。
                assert!(nx < gh && ny < gw, "回転後の座標が範囲外: ({nx},{ny})");
                rotated[ny * gh + nx] = Some((gx, gy));
            }
        }
        // 上段(ny=0)は元の左列(gx=0)を下から上へ並べたもの = 時計回り。
        assert_eq!(rotated[0], Some((0, 2)));
        assert_eq!(rotated[1], Some((0, 1)));
        assert_eq!(rotated[2], Some((0, 0)));
        // 下段(ny=1)は元の右列(gx=1)。
        assert_eq!(rotated[3], Some((1, 2)));
        assert_eq!(rotated[4], Some((1, 1)));
        assert_eq!(rotated[5], Some((1, 0)));
        // 反時計回りだったら左上に来るのは (1,0) のはずで、上の期待値と食い違う。
        assert_ne!(rotated[0], Some((1, 0)), "反時計回りになっている");
    }

    #[test]
    fn vertical_punctuation_is_shifted_exactly_half_a_cell_right_and_up() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let px = 48.0f32;
        let scaled_font =
            FontRef::try_from_slice_and_index(&font, FONT_COLLECTION_INDEX).expect("font");
        let scaled = scaled_font.as_scaled(px);
        let cell_core = (scaled.height() + scaled.line_gap()).max(1.0);

        let cell_advance = cell_core;
        for ch in ['。', '、'] {
            let advance = scaled.h_advance(scaled_font.glyph_id(ch));
            // 基準(横書き): 原点 x=0・ベースライン y=ascent に描かれる。
            let text = ch.to_string();
            let (hw, hh, horizontal) =
                rasterize_text(&font, &text, px, [0, 0, 0, 255], 0.0, 0.0).expect("horizontal ok");
            let (hx0, hy0, _, _) = ink_bounds(hw, hh, &horizontal).expect("ink");

            // 縦書きの **2 セル目**で測る(1 セル目は上端のはみ出し補正が
            // 働くため、純粋な半セル移動を見るには不適)。1 セル目には
            // インクを持たない全角スペース(U+3000)を置き、画像に残る
            // インクが 2 セル目だけになるようにする。
            let two_cells = format!("\u{3000}{ch}");
            let (vw, vh, vertical_px) =
                rasterize_text_vertical(&font, &two_cells, px, [0, 0, 0, 255], 0.0, 0.0)
                    .expect("vertical ok");
            let (vx0, vy0, _, _) = ink_bounds(vw, vh, &vertical_px).expect("2 セル目のインク");

            // 期待: x は +advance/2(列幅 == advance)、y は基準ベースラインから
            // −cell_core/2。2 セル目の基準ベースラインは 1 セルぶん下。
            let dx = vx0 as f32 - hx0 as f32;
            let dy = vy0 as f32 - (hy0 as f32 + cell_advance);
            let expected_dx = advance / 2.0;
            let expected_dy = -cell_core / 2.0;
            // 描画原点の floor による ±1px の差は許容する。
            assert!(
                (dx - expected_dx).abs() <= 1.5,
                "{ch}: 右へ半セル(期待 {expected_dx:.2}px、実測 {dx:.2}px)"
            );
            assert!(
                (dy - expected_dy).abs() <= 1.5,
                "{ch}: 上へ半セル(期待 {expected_dy:.2}px、実測 {dy:.2}px)"
            );
        }
    }

    /// 列の 1 セル目の句読点は、半セル上げるとインクが画像外へ出てしまう。
    /// はみ出しぶんだけ下げてインクを失わないこと(上端に接する)。
    #[test]
    fn leading_vertical_punctuation_is_not_clipped_at_the_top() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (hw, hh, horizontal) =
            rasterize_text(&font, "。", 48.0, [0, 0, 0, 255], 0.0, 0.0).expect("ok");
        let (_, hy0, _, hy1) = ink_bounds(hw, hh, &horizontal).expect("ink");
        let (vw, vh, vertical_px) =
            rasterize_text_vertical(&font, "。", 48.0, [0, 0, 0, 255], 0.0, 0.0).expect("ok");
        let (_, vy0, _, vy1) = ink_bounds(vw, vh, &vertical_px).expect("ink");
        assert_eq!(vy0, 0, "上端に接する(それより上へは出さない)");
        assert_eq!(
            vy1 - vy0,
            hy1 - hy0,
            "インクの高さが切り取られていない(横書きと同じ)"
        );
    }

    // -- v12 §52: 文字間・行間(横書きにも適用)------------------------------

    #[test]
    fn horizontal_char_spacing_widens_only_between_characters() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w0, _, _) = plain(&font, "AAA", 24.0).expect("ok");
        let (w1, _, _) = rasterize_text(&font, "AAA", 24.0, [0, 0, 0, 255], 10.0, 0.0).expect("ok");
        // 3 文字なら文字間は 2 箇所ぶんだけ増える。
        assert_eq!(w1, w0 + 20, "文字間は文字と文字の間にだけ入る");
        // 1 文字なら増えない(行末に余白を付けない)。
        let (single0, _, _) = plain(&font, "A", 24.0).expect("ok");
        let (single1, _, _) =
            rasterize_text(&font, "A", 24.0, [0, 0, 0, 255], 10.0, 0.0).expect("ok");
        assert_eq!(single0, single1);
    }

    #[test]
    fn horizontal_line_spacing_adds_to_the_line_advance() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (_, h0, _) = plain(&font, "A\nB", 24.0).expect("ok");
        let (_, h1, _) =
            rasterize_text(&font, "A\nB", 24.0, [0, 0, 0, 255], 0.0, 12.0).expect("ok");
        // 2 行 × 行間 12px = 24px 増える(行送りへの加算)。
        assert_eq!(h1, h0 + 24);
    }

    // -- v12 §52: 縦書き ----------------------------------------------------

    #[test]
    fn vertical_text_is_taller_than_wide_for_a_single_column() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, pixels) = vertical(&font, "あいう", 24.0).expect("ok");
        assert!(h > w, "1 列の縦書きは縦長になる: {w}x{h}");
        assert!(pixels.chunks_exact(4).any(|p| p[3] > 0));
    }

    #[test]
    fn vertical_columns_are_laid_out_right_to_left() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 1 列目だけに文字があると、インクは右半分に寄る。
        let (w, h, pixels) = vertical(&font, "あ\n", 24.0).expect("ok");
        let (x0, _, x1, _) = ink_bounds(w, h, &pixels).expect("インクがある");
        assert!(
            x0 >= w / 2,
            "最初の列は最も右に置かれる: ink x {x0}..{x1}, width {w}"
        );

        // 2 列目だけに文字があると、逆に左半分へ寄る。
        let (w, h, pixels) = vertical(&font, "\nあ", 24.0).expect("ok");
        let (x0, _, x1, _) = ink_bounds(w, h, &pixels).expect("インクがある");
        assert!(
            x1 <= w / 2 + 1,
            "2 列目は左側に置かれる: ink x {x0}..{x1}, width {w}"
        );
    }

    #[test]
    fn vertical_empty_column_occupies_one_full_width_cell() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 空列も 1 セルぶんの幅を占める(SPEC §52)。
        let (w_one, _, _) = vertical(&font, "あ", 24.0).expect("ok");
        let (w_two, _, _) = vertical(&font, "あ\n", 24.0).expect("ok");
        assert!(w_two > w_one, "空列のぶんだけ広がる: {w_one} -> {w_two}");
        // 空列だけのテキストでも寸法が出る(高さは 1 セル)。
        let (w, h, _) = vertical(&font, "\n", 24.0).expect("ok");
        assert!(w > 0 && h > 0);
    }

    #[test]
    fn vertical_rotated_char_swaps_ink_width_and_height() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 「ー」は横書きでは横長、縦書きでは 90° 回転して縦長になる。
        let (hw, hh, horizontal_px) = plain(&font, "ー", 48.0).expect("ok");
        let (hx0, hy0, hx1, hy1) = ink_bounds(hw, hh, &horizontal_px).expect("インクがある");
        let (h_ink_w, h_ink_h) = (hx1 - hx0, hy1 - hy0);
        assert!(
            h_ink_w > h_ink_h,
            "横書きの「ー」は横長: {h_ink_w}x{h_ink_h}"
        );

        let (vw, vh, vertical_px) = vertical(&font, "ー", 48.0).expect("ok");
        let (vx0, vy0, vx1, vy1) = ink_bounds(vw, vh, &vertical_px).expect("インクがある");
        let (v_ink_w, v_ink_h) = (vx1 - vx0, vy1 - vy0);
        assert!(
            v_ink_h > v_ink_w,
            "縦書きの「ー」は 90° 回転して縦長になる: {v_ink_w}x{v_ink_h}"
        );
    }

    #[test]
    fn vertical_punctuation_is_placed_at_the_top_right_of_the_cell() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 「あ。」の 2 セル目(句読点)は、通常配置よりも右上へ寄る。
        let (w, h, pixels) = vertical(&font, "。", 48.0).expect("ok");
        let (x0, y0, x1, y1) = ink_bounds(w, h, &pixels).expect("インクがある");
        let cx = (x0 + x1) / 2;
        let cy = (y0 + y1) / 2;
        assert!(
            cx * 2 > w,
            "句読点のインクはセルの右寄り: center {cx}, width {w}"
        );
        assert!(
            cy * 2 < h,
            "句読点のインクはセルの上寄り: center {cy}, height {h}"
        );
    }

    #[test]
    fn vertical_char_spacing_and_line_spacing_change_the_size() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w0, h0, _) = vertical(&font, "あい\nうえ", 24.0).expect("ok");
        // 文字間 = 縦の字送りへの加算(2 文字なので 1 箇所ぶん)。
        let (w1, h1, _) =
            rasterize_text_vertical(&font, "あい\nうえ", 24.0, [0, 0, 0, 255], 10.0, 0.0)
                .expect("ok");
        assert_eq!(w1, w0, "文字間は幅を変えない");
        assert_eq!(h1, h0 + 10, "文字間は縦の字送りに加算される");

        // 行間 = 列間への加算(2 列なので 1 箇所ぶん)。
        let (w2, h2, _) =
            rasterize_text_vertical(&font, "あい\nうえ", 24.0, [0, 0, 0, 255], 0.0, 16.0)
                .expect("ok");
        assert_eq!(h2, h0, "行間は高さを変えない");
        assert_eq!(w2, w0 + 16, "行間は列間に加算される");
    }

    #[test]
    fn oversized_text_is_rejected_instead_of_allocating() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 8192px を超える縦書き(文字数 × 字送り)は Err(TooLarge)。
        let text: String = std::iter::repeat_n('あ', 4000).collect();
        assert_eq!(
            rasterize_text_vertical(&font, &text, 144.0, [0, 0, 0, 255], 0.0, 0.0),
            Err(TextRasterError::TooLarge)
        );
        // 横書きも同様(1 行が長すぎる場合)。
        assert_eq!(
            rasterize_text(&font, &text, 144.0, [0, 0, 0, 255], 0.0, 0.0),
            Err(TextRasterError::TooLarge)
        );
        // 文字間だけでも上限を超えうる。
        assert_eq!(
            rasterize_text(&font, "あああ", 24.0, [0, 0, 0, 255], 50_000.0, 0.0),
            Err(TextRasterError::TooLarge)
        );
    }

    #[test]
    fn non_finite_sizes_and_spacings_do_not_panic() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (w, h, _) =
            rasterize_text_vertical(&font, "あ", f32::NAN, [0, 0, 0, 255], f32::NAN, f32::NAN)
                .expect("NaN は既定値へ丸める");
        assert!(w > 0 && h > 0);
        let (w, h, _) = rasterize_text(
            &font,
            "あ",
            f32::INFINITY,
            [0, 0, 0, 255],
            -5.0,
            f32::NEG_INFINITY,
        )
        .expect("∞・負値も丸める");
        assert!(w > 0 && h > 0);
    }

    #[test]
    fn vertical_and_horizontal_produce_different_layouts_for_the_same_text() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let (hw, hh, _) = plain(&font, "あいう", 24.0).expect("ok");
        let (vw, vh, _) = vertical(&font, "あいう", 24.0).expect("ok");
        assert!(hw > hh, "横書きは横長");
        assert!(vh > vw, "縦書きは縦長");
    }
}
