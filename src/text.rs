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

/// v12 §52.2: グリフのカバレッジ(0.0–1.0)を受け取る書き込み先。
///
/// レイアウト(どこに何のグリフを置くか)と「カバレッジをどう使うか」を
/// 分離するための唯一の接点。RGBA へ直接焼き込む `GlyphTarget` と、
/// 袋文字のために**カバレッジ配列**を作る `CoverageMap` の 2 つが実装する
/// (ARCHITECTURE.md §22.3b: 「レイアウト側をカバレッジ配列を返す内部関数へ
/// 分離し、色を焼き込む処理を最後段に集約する」)。
trait CoverageSink {
    fn put(&mut self, x: i32, y: i32, coverage: f32);
}

/// ラスタライズ先の RGBA バッファ(寸法・塗り色つき)。袋文字 OFF の経路は
/// v12 §52 までと**同じ演算**でここへ直接焼き込む(結果はバイト一致)。
struct GlyphTarget<'a> {
    buffer: &'a mut [u8],
    width: u32,
    height: u32,
    color: [u8; 4],
}

impl CoverageSink for GlyphTarget<'_> {
    /// カバレッジ 1 画素を合成する(範囲外は捨てる = パニックしない)。
    fn put(&mut self, x: i32, y: i32, coverage: f32) {
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
}

/// v12 §52.2: 色を持たないカバレッジ配列(袋文字の縁取りが必要とする素材)。
/// 重なりは source-over(`c + v(1-c)`)で累積する。
struct CoverageMap {
    width: u32,
    height: u32,
    data: Vec<f32>,
}

impl CoverageMap {
    fn new(width: u32, height: u32) -> Result<Self, TextRasterError> {
        let len = (width as usize)
            .checked_mul(height as usize)
            .ok_or(TextRasterError::TooLarge)?;
        let mut data = Vec::new();
        data.try_reserve_exact(len)
            .map_err(|_| TextRasterError::TooLarge)?;
        data.resize(len, 0.0);
        Ok(Self {
            width,
            height,
            data,
        })
    }
}

impl CoverageSink for CoverageMap {
    fn put(&mut self, x: i32, y: i32, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        let Some(slot) = self.data.get_mut(idx) else {
            return;
        };
        let v = coverage.clamp(0.0, 1.0);
        *slot += v * (1.0 - *slot);
    }
}

/// グリフを `(x, baseline_y)` に描く(横書き・縦書きの非回転文字で共通)。
fn draw_glyph(
    sink: &mut dyn CoverageSink,
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
        sink.put(origin_x + gx as i32, origin_y + gy as i32, coverage);
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
    sink: &mut dyn CoverageSink,
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
            sink.put(origin_x + nx as i32, origin_y + ny as i32, c);
        }
    }
    Ok(())
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

/// フォント + 正規化済みのオプション(レイアウト計算の共通の前提)。
struct Prepared<'a> {
    font: FontRef<'a>,
    px_size: f32,
    char_spacing: f32,
    line_spacing: f32,
}

fn prepare<'a>(
    font_bytes: &'a [u8],
    px_size: f32,
    char_spacing: f32,
    line_spacing: f32,
) -> Result<Prepared<'a>, TextRasterError> {
    let font = FontRef::try_from_slice_and_index(font_bytes, FONT_COLLECTION_INDEX)
        .map_err(|_| TextRasterError::Font)?;
    Ok(Prepared {
        font,
        px_size: sanitize_px_size(px_size),
        char_spacing: sanitize_spacing(char_spacing),
        line_spacing: sanitize_spacing(line_spacing),
    })
}

/// 横書きのレイアウト結果(寸法とベースライン計算に必要な値)。
struct HorizontalLayout {
    width: u32,
    height: u32,
    line_height: f32,
    ascent: f32,
}

/// 1 行ぶんのグリフを並べ、行末の x(= 行の幅)を返す。`on_glyph` は
/// 描画パスでだけ `Some` になる(寸法計算パスと描画パスで**同じコード**を
/// 通し、レイアウトのずれを防ぐ)。
fn layout_line(
    prepared: &Prepared<'_>,
    line: &str,
    mut on_glyph: Option<&mut dyn FnMut(GlyphId, f32)>,
) -> f32 {
    let scaled = prepared.font.as_scaled(prepared.px_size);
    let mut cursor_x = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for ch in line.chars() {
        let id = prepared.font.glyph_id(ch);
        if let Some(prev_id) = prev {
            cursor_x += scaled.kern(prev_id, id);
            // v12 §52: 文字間は字送りへの加算(文字と文字の**間**にだけ
            // 入れるので、行末に余白が付かない)。
            cursor_x += prepared.char_spacing;
        }
        if let Some(callback) = on_glyph.as_deref_mut() {
            callback(id, cursor_x);
        }
        cursor_x += scaled.h_advance(id);
        prev = Some(id);
    }
    cursor_x
}

/// 横書きの寸法計算パス。結果が空(幅か高さが 0)なら `None`。
fn measure_horizontal(
    prepared: &Prepared<'_>,
    text: &str,
) -> Result<Option<HorizontalLayout>, TextRasterError> {
    let scaled = prepared.font.as_scaled(prepared.px_size);
    // v12 §52: 行間は行送りへの加算(字送りへの加算は `layout_line`)。
    let line_height = ((scaled.height() + scaled.line_gap()) * LINE_HEIGHT_FACTOR).max(1.0)
        + prepared.line_spacing;

    // 行数だけ先に数え、上限を超える入力は**確保する前に**弾く
    // (`line_height >= 1` なので、行数が上限を超えれば高さも必ず超える)。
    let line_count = text.split('\n').count();
    if line_count > MAX_TEXT_DIMENSION as usize {
        return Err(TextRasterError::TooLarge);
    }

    let mut max_x = 0.0f32;
    for line in text.split('\n') {
        max_x = max_x.max(layout_line(prepared, line, None));
    }

    let width = to_dimension(max_x)?;
    let height = to_dimension(line_height * line_count as f32)?;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    Ok(Some(HorizontalLayout {
        width,
        height,
        line_height,
        ascent: scaled.ascent(),
    }))
}

/// 横書きの描画パス(カバレッジの送り先は呼び出し側が決める)。
fn draw_horizontal(
    prepared: &Prepared<'_>,
    text: &str,
    layout: &HorizontalLayout,
    sink: &mut dyn CoverageSink,
) {
    for (row, line) in text.split('\n').enumerate() {
        let baseline_y = layout.ascent + row as f32 * layout.line_height;
        let mut draw = |id: GlyphId, x: f32| {
            draw_glyph(sink, &prepared.font, id, prepared.px_size, x, baseline_y);
        };
        layout_line(prepared, line, Some(&mut draw));
    }
}

/// 縦書きのレイアウト結果。
struct VerticalLayout {
    width: u32,
    height: u32,
    ascent: f32,
    /// セルの「文字ぶん」の高さ(回転・句読点の中心合わせに使う)。
    cell_core: f32,
    /// 実際の字送り(= セル高 + 文字間)。
    cell_advance: f32,
    /// 全角 1 文字ぶんの幅(空列のセル幅)。
    full_width: f32,
    col_widths: Vec<f32>,
    total_width: f32,
}

/// 縦書きの寸法計算パス。結果が空なら `None`。
fn measure_vertical(
    prepared: &Prepared<'_>,
    text: &str,
) -> Result<Option<VerticalLayout>, TextRasterError> {
    let scaled = prepared.font.as_scaled(prepared.px_size);
    let cell_core = (scaled.height() + scaled.line_gap()).max(1.0);
    let cell_advance = cell_core + prepared.char_spacing;
    // 全角 1 文字ぶんの幅(空列のセル幅)。表意文字スペース(U+3000)の
    // advance を使い、フォントに無ければセル高で代用する。
    let full_width = {
        let advance = scaled.h_advance(prepared.font.glyph_id('\u{3000}'));
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
            width = width.max(scaled.h_advance(prepared.font.glyph_id(ch)));
            cells += 1;
        }
        // 空列は全角 1 文字ぶんの幅のセルを占める(SPEC §52)。
        col_widths.push(width.max(if cells == 0 { full_width } else { 1.0 }));
        max_cells = max_cells.max(cells.max(1));
    }

    let total_width: f32 = col_widths.iter().sum::<f32>()
        + prepared.line_spacing * (column_count.saturating_sub(1)) as f32;
    // 空列も 1 セルぶんの高さを占める(SPEC §52)。末尾の文字間は含めない。
    let total_height = cell_advance * max_cells as f32 - prepared.char_spacing;

    let width = to_dimension(total_width)?;
    let height = to_dimension(total_height)?;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    Ok(Some(VerticalLayout {
        width,
        height,
        ascent: scaled.ascent(),
        cell_core,
        cell_advance,
        full_width,
        col_widths,
        total_width,
    }))
}

/// 縦書きの描画パス(カバレッジの送り先は呼び出し側が決める)。
fn draw_vertical(
    prepared: &Prepared<'_>,
    text: &str,
    layout: &VerticalLayout,
    sink: &mut dyn CoverageSink,
) -> Result<(), TextRasterError> {
    let font = &prepared.font;
    let px_size = prepared.px_size;
    let scaled = font.as_scaled(px_size);
    // 右から左へ列を配置する(最初の列が最も右)。
    let mut right = layout.total_width;
    for (col_index, column) in text.split('\n').enumerate() {
        let col_width = layout
            .col_widths
            .get(col_index)
            .copied()
            .unwrap_or(layout.full_width);
        let x = right - col_width;
        for (cell, ch) in column.chars().enumerate() {
            let id = font.glyph_id(ch);
            let baseline_y = layout.ascent + cell as f32 * layout.cell_advance;
            if VERTICAL_ROTATED_CHARS.contains(ch) {
                // セル中心を軸に 90° 回転。
                let cx = x + col_width / 2.0;
                let cy = baseline_y - layout.ascent + layout.cell_core / 2.0;
                draw_glyph_rotated(sink, font, id, px_size, cx, cy)?;
            } else if VERTICAL_PUNCT_CHARS.contains(ch) {
                // 句読点は右上寄せ(描画起点を右へ半セル・上へ半セルずらす)。
                let punct_x = x + col_width / 2.0;
                let mut punct_y = baseline_y - layout.cell_core / 2.0;
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
                draw_glyph(sink, font, id, px_size, punct_x, punct_y);
            } else {
                // セル内で水平センタリング。
                let advance = scaled.h_advance(id);
                draw_glyph(
                    sink,
                    font,
                    id,
                    px_size,
                    x + (col_width - advance) / 2.0,
                    baseline_y,
                );
            }
        }
        right = x - prepared.line_spacing;
    }
    Ok(())
}

/// `text` を `font_bytes`(TTF/TTC のバイト列)を使って `px_size` ピクセルの
/// アンチエイリアス付きでラスタライズし、`color`(straight-alpha RGBA)で
/// 塗る。戻り値は `(幅, 高さ, RGBA8 straight-alpha バッファ)`。
///
/// - 空文字列、レイアウト結果の幅/高さが 0 になる場合は `(0, 0, Vec::new())`
///   を返す(SPEC §19: 「空文字列の確定は何もしない」の判定に使える)。
///   フォント解析の失敗・寸法超過は `Err`(v12 §52)。
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
    let prepared = prepare(font_bytes, px_size, char_spacing, line_spacing)?;
    let Some(layout) = measure_horizontal(&prepared, text)? else {
        return Ok((0, 0, Vec::new()));
    };
    let mut buffer = allocate_buffer(layout.width, layout.height)?;
    let mut target = GlyphTarget {
        buffer: &mut buffer,
        width: layout.width,
        height: layout.height,
        color,
    };
    draw_horizontal(&prepared, text, &layout, &mut target);
    Ok((layout.width, layout.height, buffer))
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
    let prepared = prepare(font_bytes, px_size, char_spacing, line_spacing)?;
    let Some(layout) = measure_vertical(&prepared, text)? else {
        return Ok((0, 0, Vec::new()));
    };
    let mut buffer = allocate_buffer(layout.width, layout.height)?;
    let mut target = GlyphTarget {
        buffer: &mut buffer,
        width: layout.width,
        height: layout.height,
        color,
    };
    draw_vertical(&prepared, text, &layout, &mut target)?;
    Ok((layout.width, layout.height, buffer))
}

/// v12 §52.2: 色を焼き込む前の**カバレッジ配列**を返す(袋文字の素材)。
/// `vertical` で横書き/縦書きのレイアウトを選ぶ(同じ設定が両方で効くよう、
/// 分岐はここ 1 箇所だけ)。戻り値は `(幅, 高さ, 0.0–1.0 のカバレッジ)`。
pub fn text_coverage(
    font_bytes: &[u8],
    text: &str,
    px_size: f32,
    char_spacing: f32,
    line_spacing: f32,
    vertical: bool,
) -> Result<(u32, u32, Vec<f32>), TextRasterError> {
    if text.is_empty() {
        return Ok((0, 0, Vec::new()));
    }
    let prepared = prepare(font_bytes, px_size, char_spacing, line_spacing)?;
    if vertical {
        let Some(layout) = measure_vertical(&prepared, text)? else {
            return Ok((0, 0, Vec::new()));
        };
        let mut map = CoverageMap::new(layout.width, layout.height)?;
        draw_vertical(&prepared, text, &layout, &mut map)?;
        Ok((map.width, map.height, map.data))
    } else {
        let Some(layout) = measure_horizontal(&prepared, text)? else {
            return Ok((0, 0, Vec::new()));
        };
        let mut map = CoverageMap::new(layout.width, layout.height)?;
        draw_horizontal(&prepared, text, &layout, &mut map);
        Ok((map.width, map.height, map.data))
    }
}

// ---------------------------------------------------------------------------
// v12 §52.2: 袋文字(縁取り)
// ---------------------------------------------------------------------------

/// 距離場の「まだ種から遠い」初期値(画素数より十分大きい値。∞ を使わず
/// 有限値にして NaN を避ける)。
const DISTANCE_FIELD_FAR: i32 = 1 << 20;

/// 距離場の種(= 文字の内側)とみなすカバレッジのしきい値。
///
/// `ab_glyph` のラスタライザはグリフの外接矩形内に **1e-7 級のごく小さい
/// カバレッジ**を大量に返す(RGBA へ焼くと α が 0 に丸まるので従来は
/// 無害だった)。`> 0.0` を種にすると外接矩形がまるごと「文字」と見なされ、
/// 縁が字形ではなく**四角い箱**になってしまう(実機の目視で発見)。
/// 見た目の輪郭である 50% 等高線を種にすることで、縁が字形に沿う。
const DISTANCE_FIELD_INK_THRESHOLD: f32 = 0.5;

/// 種(インク)からの**ユークリッド距離**の場を作る(8SSEDT: 最近傍種への
/// オフセットベクトルを前方・後方の 2 パスで伝播させる古典的アルゴリズム)。
///
/// ARCHITECTURE.md §22.3b は「2 パスのチャンファー距離変換」を指定しているが、
/// チャンファー(3-4 近似)は対角方向の距離を 6% ほど過小評価するため、
/// 角が真円にならず「対角の到達距離 ≤ 半径」を保証できない。同じ 2 パス・
/// 同じ `O(w*h)` で**実質厳密なユークリッド距離**が得られる 8SSEDT を採用した
/// (計算量の要件は満たしつつ、SPEC §52.2 の「角は丸く」をテスト可能な形で
/// 満たすための選択)。
fn distance_field(
    coverage: &[f32],
    width: usize,
    height: usize,
) -> Result<Vec<f32>, TextRasterError> {
    let len = width.checked_mul(height).ok_or(TextRasterError::TooLarge)?;
    let mut grid: Vec<(i32, i32)> = Vec::new();
    grid.try_reserve_exact(len)
        .map_err(|_| TextRasterError::TooLarge)?;
    let mut seeds = 0usize;
    let mut max_coverage = 0.0f32;
    for i in 0..len {
        let cov = coverage.get(i).copied().unwrap_or(0.0);
        max_coverage = max_coverage.max(cov);
        let inked = cov >= DISTANCE_FIELD_INK_THRESHOLD;
        if inked {
            seeds += 1;
        }
        grid.push(if inked {
            (0, 0)
        } else {
            (DISTANCE_FIELD_FAR, DISTANCE_FIELD_FAR)
        });
    }
    // 追いレビュー②: 極小フォント・細い記号では全画素が 50% 未満になりうる。
    // そのまま種ゼロで進むと「文字は見えているのに縁だけ消える」ので、
    // 見えているカバレッジがあるなら**最も濃い画素**を種にフォールバックする。
    if seeds == 0 && max_coverage > 0.0 {
        for (i, cell) in grid.iter_mut().enumerate() {
            if coverage.get(i).copied().unwrap_or(0.0) >= max_coverage {
                *cell = (0, 0);
            }
        }
    }

    let dist2 =
        |c: (i32, i32)| -> i64 { (c.0 as i64) * (c.0 as i64) + (c.1 as i64) * (c.1 as i64) };
    let compare = |grid: &mut Vec<(i32, i32)>, idx: usize, other: usize, dx: i32, dy: i32| {
        let Some(&(ox, oy)) = grid.get(other) else {
            return;
        };
        let candidate = (ox + dx, oy + dy);
        let Some(current) = grid.get_mut(idx) else {
            return;
        };
        if dist2(candidate) < dist2(*current) {
            *current = candidate;
        }
    };

    // 前方パス(左上→右下)。
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if x > 0 {
                compare(&mut grid, idx, idx - 1, 1, 0);
            }
            if y > 0 {
                compare(&mut grid, idx, idx - width, 0, 1);
                if x > 0 {
                    compare(&mut grid, idx, idx - width - 1, 1, 1);
                }
                if x + 1 < width {
                    compare(&mut grid, idx, idx - width + 1, -1, 1);
                }
            }
        }
        // 行内の右→左(前方パスの仕上げ)。
        for x in (0..width.saturating_sub(1)).rev() {
            let idx = y * width + x;
            compare(&mut grid, idx, idx + 1, -1, 0);
        }
    }
    // 後方パス(右下→左上)。
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let idx = y * width + x;
            if x + 1 < width {
                compare(&mut grid, idx, idx + 1, -1, 0);
            }
            if y + 1 < height {
                compare(&mut grid, idx, idx + width, 0, -1);
                if x + 1 < width {
                    compare(&mut grid, idx, idx + width + 1, -1, -1);
                }
                if x > 0 {
                    compare(&mut grid, idx, idx + width - 1, 1, -1);
                }
            }
        }
        for x in 1..width {
            let idx = y * width + x;
            compare(&mut grid, idx, idx - 1, 1, 0);
        }
    }

    let mut out: Vec<f32> = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| TextRasterError::TooLarge)?;
    out.extend(grid.iter().map(|c| (dist2(*c) as f64).sqrt() as f32));
    Ok(out)
}

/// v12 §52.2: 袋文字(縁取り)。`coverage`(`text_coverage` の出力)を
/// `radius` px ぶん膨張させた領域から**文字自身の被覆を差し引いた**部分に
/// `outline` 色を置き、その上に `fill` 色で文字を描く。
///
/// - 出力は四方に `ceil(radius)` 拡張した寸法(縁が切れない)。
/// - 縁のカバレッジは `clamp(radius + 0.5 - d, 0, 1)`(境界 1px で AA)。
/// - `outline_cov = max(0, outline_cov - glyph_cov)` なので、**塗りと縁は
///   重ならない**(半透明のプライマリ色でも縁が透けて濁らない)。
/// - 合成順は 縁 → 塗り。
/// - 距離場・カバレッジ・出力バッファはすべて `try_reserve` で確保し、
///   膨張後の寸法が上限を超えれば `TooLarge`。
pub fn outline_text(
    coverage: &[f32],
    width: u32,
    height: u32,
    radius: f32,
    fill: [u8; 4],
    outline: [u8; 4],
) -> Result<RasterizedText, TextRasterError> {
    if width == 0 || height == 0 {
        return Ok((0, 0, Vec::new()));
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .ok_or(TextRasterError::TooLarge)?;
    if coverage.len() < expected {
        // 呼び出し側の取り違え(寸法とバッファの不一致)。パニックせず拒否する。
        return Err(TextRasterError::TooLarge);
    }
    let radius = sanitize_spacing(radius);
    let pad = to_dimension(radius.ceil())?;
    // 膨張後の寸法で上限を判定する(SPEC §52.2)。
    let out_width = to_dimension(width as f32 + pad as f32 * 2.0)?;
    let out_height = to_dimension(height as f32 + pad as f32 * 2.0)?;
    let out_len = (out_width as usize)
        .checked_mul(out_height as usize)
        .ok_or(TextRasterError::TooLarge)?;

    // 四方へ広げたカバレッジ(中央に元のカバレッジを置く)。
    let mut padded: Vec<f32> = Vec::new();
    padded
        .try_reserve_exact(out_len)
        .map_err(|_| TextRasterError::TooLarge)?;
    padded.resize(out_len, 0.0);
    for y in 0..height as usize {
        let src = y * width as usize;
        let dst = (y + pad as usize) * out_width as usize + pad as usize;
        for x in 0..width as usize {
            padded[dst + x] = coverage[src + x].clamp(0.0, 1.0);
        }
    }

    let distance = distance_field(&padded, out_width as usize, out_height as usize)?;
    let mut buffer = allocate_buffer(out_width, out_height)?;
    for (i, &glyph_cov) in padded.iter().enumerate().take(out_len) {
        let d = distance.get(i).copied().unwrap_or(f32::MAX);
        // 縁は「膨張領域 − 文字自身」。境界 1px は AA。
        let outline_cov = ((radius + 0.5 - d).clamp(0.0, 1.0) - glyph_cov).max(0.0);
        if outline_cov <= 0.0 && glyph_cov <= 0.0 {
            continue;
        }
        // 追いレビュー①: `outline_cov` と `glyph_cov` は差し引き済み =
        // **同じ画素の中で重ならない面積被覆**。これを source-over で 2 回
        // 重ねると総カバレッジが不足し(例: 0.5 と 0.5 で α が 0.75 にしか
        // ならない)、縁と塗りの境界に透過の溝ができる。premultiplied で
        // 面積寄与を**加算**し、最後に straight-alpha へ戻す。
        let outline_alpha = (outline[3] as f32 / 255.0) * outline_cov;
        let fill_alpha = (fill[3] as f32 / 255.0) * glyph_cov;
        let alpha = (outline_alpha + fill_alpha).clamp(0.0, 1.0);
        let idx = i * 4;
        let Some(dst) = buffer.get_mut(idx..idx + 4) else {
            continue;
        };
        if alpha <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let premultiplied = outline[c] as f32 * outline_alpha + fill[c] as f32 * fill_alpha;
            dst[c] = (premultiplied / alpha).round().clamp(0.0, 255.0) as u8;
        }
        dst[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Ok((out_width, out_height, buffer))
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

    // -- v12 §52.2: 袋文字(縁取り)------------------------------------

    /// `w*h` のカバレッジ配列(`inked` が真の画素だけ 1.0)。
    fn coverage_map(w: u32, h: u32, inked: impl Fn(u32, u32) -> bool) -> Vec<f32> {
        let mut out = vec![0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                if inked(x, y) {
                    out[(y * w + x) as usize] = 1.0;
                }
            }
        }
        out
    }

    fn pixel(w: u32, buffer: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]
    }

    #[test]
    fn outline_expands_the_buffer_by_the_radius_on_every_side() {
        let cov = coverage_map(4, 6, |_, _| true);
        let (w, h, _) =
            outline_text(&cov, 4, 6, 3.0, [0, 0, 0, 255], [255, 255, 255, 255]).expect("ok");
        assert_eq!((w, h), (4 + 6, 6 + 6), "四方に ceil(radius) ぶん広がる");

        // 小数の半径は切り上げ。
        let (w, h, _) =
            outline_text(&cov, 4, 6, 2.2, [0, 0, 0, 255], [255, 255, 255, 255]).expect("ok");
        assert_eq!((w, h), (4 + 6, 6 + 6));
    }

    #[test]
    fn outline_surrounds_the_glyph_on_all_four_sides() {
        // 中央の 1 画素だけインク。半径 3。
        let (iw, ih) = (5u32, 5u32);
        let cov = coverage_map(iw, ih, |x, y| x == 2 && y == 2);
        let radius = 3.0f32;
        let (w, h, buffer) =
            outline_text(&cov, iw, ih, radius, [0, 0, 0, 255], [255, 0, 0, 255]).expect("ok");
        let pad = 3u32;
        let (cx, cy) = (pad + 2, pad + 2);
        assert_eq!((w, h), (iw + pad * 2, ih + pad * 2));

        // 上下左右とも: radius−1 までは不透明、radius でちょうど境界(AA)、
        // radius+1 より外は透明。
        for (dx, dy) in [(0i32, -1i32), (0, 1), (-1, 0), (1, 0)] {
            let at = |k: i32| {
                pixel(
                    w,
                    &buffer,
                    (cx as i32 + dx * k) as u32,
                    (cy as i32 + dy * k) as u32,
                )
            };
            let inner = at(2);
            assert_eq!(inner[3], 255, "({dx},{dy})×2 は縁の内側: {inner:?}");
            assert_eq!([inner[0], inner[1], inner[2]], [255, 0, 0], "縁の色が違う");
            let edge = at(3);
            assert!(edge[3] > 0, "({dx},{dy})×3 は縁の境界(AA): {edge:?}");
            let outside = at(4);
            assert_eq!(outside[3], 0, "({dx},{dy})×4 は半径の外: {outside:?}");
        }
    }

    #[test]
    fn outline_does_not_show_through_a_semi_transparent_fill() {
        // SPEC §52.2: 縁は文字の外側だけ(重なりを差し引く)。半透明の塗りでも
        // 縁色が透けて濁らない。
        let (iw, ih) = (5u32, 5u32);
        let cov = coverage_map(iw, ih, |x, y| x == 2 && y == 2);
        let fill = [0u8, 0, 255, 128];
        let (w, _, buffer) = outline_text(&cov, iw, ih, 2.0, fill, [255, 0, 0, 255]).expect("ok");
        let pad = 2u32;
        let px = pixel(w, &buffer, pad + 2, pad + 2);
        assert_eq!(px, fill, "塗りの画素は塗り色そのもの(縁が下に無い): {px:?}");
    }

    #[test]
    fn outline_corners_are_round_not_square() {
        // SPEC §52.2: 角は丸く(距離場に基づく膨張)。単一画素のインクに
        // 半径 5 をかけると、対角方向の到達距離は radius を超えない
        // (正方形膨張なら (5,5) = 7.07px 先まで塗られてしまう)。
        let (iw, ih) = (3u32, 3u32);
        let cov = coverage_map(iw, ih, |x, y| x == 1 && y == 1);
        let radius = 5.0f32;
        let (w, _, buffer) =
            outline_text(&cov, iw, ih, radius, [0, 0, 0, 0], [255, 255, 255, 255]).expect("ok");
        let pad = 5u32;
        let (cx, cy) = (pad + 1, pad + 1);

        // 軸方向は radius ぶん届く。
        for (dx, dy) in [(5i32, 0i32), (-5, 0), (0, 5), (0, -5)] {
            let px = pixel(w, &buffer, (cx as i32 + dx) as u32, (cy as i32 + dy) as u32);
            assert!(px[3] > 0, "軸方向 ({dx},{dy}) に縁が無い");
        }
        // 対角方向で「真の距離 > radius + 0.5」の画素は塗られない。
        for k in 1..=5i32 {
            let distance = (k as f32) * std::f32::consts::SQRT_2;
            let px = pixel(w, &buffer, (cx as i32 + k) as u32, (cy as i32 + k) as u32);
            if distance > radius + 0.5 {
                assert_eq!(
                    px[3], 0,
                    "対角 ({k},{k}) = {distance:.2}px は radius {radius} を超えるので塗らない"
                );
            } else {
                assert!(px[3] > 0, "対角 ({k},{k}) = {distance:.2}px には縁が要る");
            }
        }
    }

    /// 実機の目視で見つけた回帰: `ab_glyph` はグリフの外接矩形内に 1e-7 級の
    /// ごく小さいカバレッジを大量に返す。これを「文字」と見なすと、縁が
    /// 字形ではなく**四角い箱**になる(RGBA へ焼くと α 0 に丸まるので
    /// 従来の経路では見えなかった)。50% 等高線を種にすることで防ぐ。
    #[test]
    fn outline_ignores_negligible_coverage_noise_around_the_glyph_box() {
        let (iw, ih) = (7u32, 7u32);
        // 中央 1 画素だけが本物のインク。周囲は「ほぼ 0」のノイズ。
        let mut cov = vec![5.96e-8f32; (iw * ih) as usize];
        cov[(3 * iw + 3) as usize] = 1.0;
        let radius = 2.0f32;
        let (w, _, buffer) =
            outline_text(&cov, iw, ih, radius, [0, 0, 0, 255], [255, 255, 255, 255]).expect("ok");
        let pad = 2u32;
        // 箱状に膨張していたら、外接矩形の角(元画像の左上 + 半径)にも
        // 縁が乗ってしまう。字形沿いなら中心から遠い角は透明のまま。
        let corner = pixel(w, &buffer, pad, pad);
        assert_eq!(
            corner[3], 0,
            "外接矩形の角に縁が出ている(ノイズを種にしている): {corner:?}"
        );
        // 中心のすぐ隣には縁がある。
        let near = pixel(w, &buffer, pad + 3 + 2, pad + 3);
        assert!(near[3] > 0, "字形の周りには縁が要る: {near:?}");
    }

    /// 追いレビュー①: `outline_cov` と `glyph_cov` は**重ならない面積被覆**
    /// なので、加算(premultiplied)で合成しなければ境界に透過の溝ができる。
    /// 中央画素のカバレッジ 0.5・周囲は十分内側(膨張カバレッジ 1.0)という
    /// 状況で、不透明な塗り+不透明な縁なら出力 α は 255 になる。
    #[test]
    fn outline_and_fill_areas_add_up_without_a_transparent_seam() {
        // 3x3 の中央だけ 0.5(= 種になる)。半径 2 なら中央も膨張領域の内側。
        let (iw, ih) = (3u32, 3u32);
        let mut cov = vec![0f32; (iw * ih) as usize];
        cov[(iw + 1) as usize] = 0.5;
        let (w, _, buffer) =
            outline_text(&cov, iw, ih, 2.0, [255, 0, 0, 255], [0, 0, 255, 255]).expect("ok");
        let pad = 2u32;
        let px = pixel(w, &buffer, pad + 1, pad + 1);
        assert_eq!(
            px[3], 255,
            "縁 0.5 + 塗り 0.5 で完全不透明になる(溝ができない): {px:?}"
        );
        // RGB は面積比 1:1 の混色(premultiplied 加算)。
        assert_eq!(px[0], 128, "赤(塗り)の寄与: {px:?}");
        assert_eq!(px[2], 128, "青(縁)の寄与: {px:?}");

        // 半透明の塗り(α=128)でも premultiplied の期待値どおりになる。
        let (w, _, buffer) =
            outline_text(&cov, iw, ih, 2.0, [255, 0, 0, 128], [0, 0, 255, 255]).expect("ok");
        let px = pixel(w, &buffer, pad + 1, pad + 1);
        // fill_alpha = 128/255*0.5 ≈ 0.2510、outline_alpha = 0.5
        // α = 0.7510 → 192、R = 255*0.2510/0.7510 ≈ 85、B = 255*0.5/0.7510 ≈ 170
        assert_eq!(px[3], 192, "α = (0.5 + 0.251) * 255: {px:?}");
        assert!(
            (px[0] as i32 - 85).abs() <= 1,
            "R が premultiplied 期待値: {px:?}"
        );
        assert!(
            (px[2] as i32 - 170).abs() <= 1,
            "B が premultiplied 期待値: {px:?}"
        );
    }

    /// 追いレビュー②: 全画素が 50% 未満(極小フォント・細い記号)でも、
    /// 見えているカバレッジがあるなら縁は出る。
    #[test]
    fn outline_still_appears_when_every_pixel_is_below_the_ink_threshold() {
        for cov in [vec![0.49f32], vec![0.1f32, 0.49, 0.1]] {
            let width = cov.len() as u32;
            let (w, _, buffer) =
                outline_text(&cov, width, 1, 2.0, [0, 0, 0, 255], [255, 255, 255, 255])
                    .expect("ok");
            let has_outline = buffer
                .chunks_exact(4)
                .any(|p| p[3] > 0 && p[0] > 200 && p[1] > 200 && p[2] > 200);
            assert!(
                has_outline,
                "カバレッジ {cov:?} で縁が消えている(種のフォールバックが効いていない)"
            );
            // 塗りも残っている(縁だけになっていない)。
            let has_fill = buffer.chunks_exact(4).any(|p| p[3] > 0 && p[0] < 200);
            assert!(has_fill, "塗りが消えている: {cov:?}");
            let _ = w;
        }
    }

    #[test]
    fn outline_rejects_oversized_results_and_mismatched_input() {
        // 膨張後の寸法で上限判定(8192 を超える)。
        let cov = vec![0f32; 4];
        assert_eq!(
            outline_text(&cov, 2, 2, 9000.0, [0, 0, 0, 255], [0, 0, 0, 255]),
            Err(TextRasterError::TooLarge)
        );
        // 寸法とバッファ長の不一致は拒否(パニックしない)。
        assert_eq!(
            outline_text(&cov, 100, 100, 1.0, [0, 0, 0, 255], [0, 0, 0, 255]),
            Err(TextRasterError::TooLarge)
        );
        // 空入力は空を返す。
        assert_eq!(
            outline_text(&[], 0, 0, 3.0, [0, 0, 0, 255], [0, 0, 0, 255]),
            Ok((0, 0, Vec::new()))
        );
    }

    #[test]
    fn text_coverage_matches_the_alpha_of_the_plain_rasterizer() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        // 不透明色で焼いた RGBA の α と、カバレッジ配列は(丸めの範囲で)一致する。
        for vertical in [false, true] {
            let (cw, ch, cov) =
                text_coverage(&font, "あA", 32.0, 0.0, 0.0, vertical).expect("coverage");
            let (rw, rh, rgba) = if vertical {
                rasterize_text_vertical(&font, "あA", 32.0, [0, 0, 0, 255], 0.0, 0.0)
            } else {
                rasterize_text(&font, "あA", 32.0, [0, 0, 0, 255], 0.0, 0.0)
            }
            .expect("rgba");
            assert_eq!((cw, ch), (rw, rh), "寸法が一致する(vertical={vertical})");
            for (i, c) in cov.iter().enumerate() {
                let alpha = rgba[i * 4 + 3] as f32 / 255.0;
                assert!(
                    (alpha - c).abs() <= 2.0 / 255.0,
                    "画素 {i} のカバレッジが α と食い違う: {c} vs {alpha}"
                );
            }
        }
    }

    #[test]
    fn outlined_vertical_text_is_larger_than_the_plain_one_and_has_outline_color() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let radius = 4.0f32;
        let (pw, ph, _) =
            rasterize_text_vertical(&font, "あ\nい", 32.0, [0, 0, 0, 255], 0.0, 0.0).expect("ok");
        let (cw, chh, cov) = text_coverage(&font, "あ\nい", 32.0, 0.0, 0.0, true).expect("ok");
        let (ow, oh, buffer) =
            outline_text(&cov, cw, chh, radius, [0, 0, 0, 255], [0, 255, 0, 255]).expect("ok");
        assert_eq!((ow, oh), (pw + 8, ph + 8), "縦書きでも四方へ広がる");
        assert!(
            buffer
                .chunks_exact(4)
                .any(|p| p[3] > 0 && p[1] > 200 && p[0] < 50),
            "縁色(緑)の画素がある"
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

    /// v12 §52.2(追いレビュー③)の参照実装: **Phase 3 コミット時点
    /// (e1114dc)の縦書きラスタライザ**をテスト専用に固定したもの。
    /// `CoverageSink` リファクタで縦書きの出力が変わっていないことを
    /// バイト単位で確かめるためだけに置く(描画のピクセル合成も当時のまま
    /// ローカルに持ち、現在の描画ヘルパーには依存しない)。
    struct LegacyTarget<'a> {
        buffer: &'a mut [u8],
        width: u32,
        height: u32,
        color: [u8; 4],
    }

    impl LegacyTarget<'_> {
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
            let mut coverage = vec![0f32; len];
            outlined.draw(|gx, gy, c| {
                let (gx, gy) = (gx as usize, gy as usize);
                if gx >= gw || gy >= gh {
                    return;
                }
                coverage[gy * gw + gx] = c;
            });
            let (rot_w, rot_h) = (gh, gw);
            let origin_x = (cx - rot_w as f32 / 2.0).floor() as i32;
            let origin_y = (cy - rot_h as f32 / 2.0).floor() as i32;
            for gy in 0..gh {
                for gx in 0..gw {
                    let c = coverage[gy * gw + gx];
                    if c <= 0.0 {
                        continue;
                    }
                    // 時計回り 90°(当時と同じ写像)。
                    let nx = gh - 1 - gy;
                    let ny = gx;
                    self.blend(origin_x + nx as i32, origin_y + ny as i32, c);
                }
            }
            Ok(())
        }
    }

    fn legacy_rasterize_text_vertical(
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
        let cell_core = (scaled.height() + scaled.line_gap()).max(1.0);
        let cell_advance = cell_core + char_spacing;
        let full_width = {
            let advance = scaled.h_advance(font.glyph_id('\u{3000}'));
            if advance > 0.0 {
                advance
            } else {
                cell_core
            }
        };

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
            col_widths.push(width.max(if cells == 0 { full_width } else { 1.0 }));
            max_cells = max_cells.max(cells.max(1));
        }

        let total_width: f32 =
            col_widths.iter().sum::<f32>() + line_spacing * (column_count.saturating_sub(1)) as f32;
        let total_height = cell_advance * max_cells as f32 - char_spacing;

        let width = to_dimension(total_width)?;
        let height = to_dimension(total_height)?;
        if width == 0 || height == 0 {
            return Ok((0, 0, Vec::new()));
        }
        let mut buffer = allocate_buffer(width, height)?;
        let mut target = LegacyTarget {
            buffer: &mut buffer,
            width,
            height,
            color,
        };

        let mut right = total_width;
        for (col_index, column) in text.split('\n').enumerate() {
            let col_width = col_widths.get(col_index).copied().unwrap_or(full_width);
            let x = right - col_width;
            for (cell, ch) in column.chars().enumerate() {
                let id = font.glyph_id(ch);
                let baseline_y = ascent + cell as f32 * cell_advance;
                if VERTICAL_ROTATED_CHARS.contains(ch) {
                    let cx = x + col_width / 2.0;
                    let cy = baseline_y - ascent + cell_core / 2.0;
                    target.draw_glyph_rotated(&font, id, px_size, cx, cy)?;
                } else if VERTICAL_PUNCT_CHARS.contains(ch) {
                    let punct_x = x + col_width / 2.0;
                    let mut punct_y = baseline_y - cell_core / 2.0;
                    let probe = id.with_scale_and_position(px_size, point(punct_x, punct_y));
                    if let Some(outlined) = font.outline_glyph(probe) {
                        let top = outlined.px_bounds().min.y;
                        if top < 0.0 {
                            punct_y -= top;
                        }
                    }
                    target.draw_glyph(&font, id, px_size, punct_x, punct_y);
                } else {
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

    /// 追いレビュー③: 縦書きも `CoverageSink` リファクタ前(e1114dc)と
    /// **寸法・RGBA バッファまでバイト一致**すること。回転文字・句読点・
    /// 先頭句読点の補正・空列・複数列・半透明色・文字間/行間を網羅する。
    #[test]
    fn vertical_matches_the_phase3_implementation_byte_for_byte() {
        let Some(font) = load_test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let cases: [(&str, f32, [u8; 4], f32, f32); 9] = [
            // 通常文字のみ。
            ("あいう", 32.0, [0, 0, 0, 255], 0.0, 0.0),
            // 回転文字(長音・括弧)。
            ("ー「あ」", 40.0, [0, 0, 0, 255], 0.0, 0.0),
            // 句読点(2 セル目以降)。
            ("あ。い、", 36.0, [0, 0, 0, 255], 0.0, 0.0),
            // 先頭句読点(上端はみ出し補正が働くケース)。
            ("。あ", 48.0, [0, 0, 0, 255], 0.0, 0.0),
            // 空列を含む複数列。
            ("あ\n\nい", 24.0, [0, 0, 0, 255], 0.0, 0.0),
            // 複数列 + 混在。
            ("縦書き\nテスト。\nー〜", 28.0, [0, 0, 0, 255], 0.0, 0.0),
            // 半透明色。
            ("あい", 32.0, [10, 200, 30, 128], 0.0, 0.0),
            // 文字間・行間あり。
            ("あい\nうえ", 32.0, [0, 0, 0, 255], 7.0, 13.0),
            // 全部入り。
            ("ー。\nあ\n「い」、", 44.0, [200, 10, 10, 200], 3.0, 9.0),
        ];
        for (text, px, color, cs, ls) in cases {
            let legacy =
                legacy_rasterize_text_vertical(&font, text, px, color, cs, ls).expect("legacy ok");
            let current =
                rasterize_text_vertical(&font, text, px, color, cs, ls).expect("current ok");
            assert_eq!(
                (current.0, current.1),
                (legacy.0, legacy.1),
                "寸法が Phase 3 実装と一致しない: {text:?}"
            );
            assert!(
                current.2 == legacy.2,
                "RGBA バッファが Phase 3 実装と一致しない: {text:?}"
            );
        }
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
