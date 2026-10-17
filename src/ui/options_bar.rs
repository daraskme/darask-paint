//! オプションバー(SPEC §3 の画面構成、§4 の内容)。
//!
//! M3 で実配線した:
//! - ブラシサイズスライダー(1–64px、SPEC §4)。
//! - ツール固有オプション: 矩形/楕円の枠線のみ/塗りつぶし/両方モード、
//!   塗りつぶしの許容値。
//!
//! v2 M3(ARCHITECTURE.md §14.7/§14.8)で色関連(スウォッチ・最近使った色)
//! を右パネルの `color_panel` へ移設した。サイズ・ツール固有オプションだけ
//! がここに残る。
//!
//! v3(SPEC §17、ARCHITECTURE.md §15.5 V3-M1)でペンの「アンチエイリアス」
//! チェックボックスを廃止し、ブラシ(旧ペン)・消しゴム共通の硬さ・不透明度
//! スライダーと鉛筆モードチェックボックスに置き換えた。
//!
//! v13 §60: 先頭に現在のツール(アイコン+名前+ショートカット)を出し、
//! スライダーを「ラベル+アンバーのつまみ+数値欄」、択一を切り替えボタン、
//! チェックボックスを角丸トグルにした。サイズはサイズを使うツール(ブラシ・
//! 消しゴム・直線・矩形・楕円・選択ブラシ)のときだけ出す。幅が足りなければ
//! 2 段に折り返す(段数は前フレームの実測で決め、変わったフレームだけ
//! 再描画を 1 回予約する)。

use eframe::egui;

use crate::ui::{icons, theme, toolbar, widgets};

use crate::keymap::{self, Action};
use crate::raster::GradientKind;
use crate::settings;
use crate::tools::gradient::GradientColors;
use crate::tools::shapes::ShapeMode;
use crate::tools::{LassoMode, ToolKind};

/// `show` に渡すオプションバーの状態一式。app.rs が各フィールドの持ち主
/// (`DaraskApp`/各 `Tool`)から可変参照を集めて構築する。
pub struct OptionsBarCtx<'a> {
    pub tool: ToolKind,
    pub brush_size: &'a mut f32,
    /// SPEC §17: 硬さ 0–100%(ブラシ/消しゴム共通)。
    pub brush_hardness: &'a mut u8,
    /// SPEC §17: 不透明度 1–100%(ブラシ/消しゴム共通。消しゴムでは
    /// 「強さ」というラベルで表示する)。
    pub brush_opacity: &'a mut u8,
    /// SPEC §17: 鉛筆モード(ブラシ/消しゴム共通)。
    pub pencil_mode: &'a mut bool,
    /// SPEC §25: ブラシ/消しゴム/鉛筆共通のスムージング(0–100%)。
    pub brush_smoothing: &'a mut u8,
    /// 現在のツールが矩形/楕円のときだけ `Some`(そのツール自身の `mode` の
    /// 可変参照)。
    pub shape_mode: Option<&'a mut ShapeMode>,
    pub fill_tolerance: &'a mut u8,
    /// v4 §23: グラデーションの種類(線形/円形)。
    pub gradient_kind: &'a mut GradientKind,
    /// v4 §23: グラデーションの色(プライマリ→セカンダリ/プライマリ→透明)。
    pub gradient_colors: &'a mut GradientColors,
    /// SPEC §19: テキストツールのフォントサイズ(8–144px、デフォルト 24)。
    pub text_font_size: &'a mut f32,
    /// v12 §52: 縦書き(既定 OFF)。
    pub text_vertical: &'a mut bool,
    /// v12 §52: 文字間 0〜50px(縦横とも字送りへの加算)。
    pub text_char_spacing: &'a mut u8,
    /// v12 §52: 行間 0〜100px(横=行送り・縦=列間への加算)。
    pub text_line_spacing: &'a mut u8,
    /// v12 §52.2: 袋文字(縁取り。既定 OFF)。
    pub text_outline: &'a mut bool,
    /// v12 §52.2: 縁の太さ 1〜20px(既定 3)。
    pub text_outline_width: &'a mut u8,
    /// v12 §52.2: 縁に使われる色(セカンダリ)。表示専用 — どの色が縁に
    /// なるかを一目で分かるようにスウォッチで見せる。
    pub secondary_color: egui::Color32,
    /// v4 §22: なげなわの自由/多角形モード(Shift+L で切替)。表示専用
    /// (直接編集はできない。ここに出すのは ARCHITECTURE.md §16.10-10:
    /// 「巡回系はオプションバーの整合を忘れない」ため)。
    pub lasso_mode: LassoMode,
    /// v4 §22: 自動選択の許容値(0–255、塗りつぶしと同じ意味)。
    pub magic_wand_tolerance: &'a mut u8,
    /// v10 §46: 「透明な選択」(選択系・移動ツールのチェックボックス。
    /// ON のとき浮動化・貼り付けでセカンダリ色の画素を選択から除外する)。
    pub transparent_selection: &'a mut bool,
    /// v12 §51.2: 選択ブラシの現在のモード(`true` = 消去 = Alt 押下中)。
    /// 表示専用(モードは Alt の押下で決まり、UI から直接は切り替えない)。
    pub select_brush_erase: bool,
}

pub fn show(ui: &mut egui::Ui, state: OptionsBarCtx) {
    let OptionsBarCtx {
        tool,
        brush_size,
        brush_hardness,
        brush_opacity,
        pencil_mode,
        brush_smoothing,
        shape_mode,
        fill_tolerance,
        gradient_kind,
        gradient_colors,
        text_font_size,
        text_vertical,
        text_char_spacing,
        text_line_spacing,
        text_outline,
        text_outline_width,
        secondary_color,
        lasso_mode,
        magic_wand_tolerance,
        transparent_selection,
        select_brush_erase,
    } = state;

    let rows_id = egui::Id::new("darask_options_bar_rows");
    let rows: usize = ui
        .ctx()
        .data(|d| d.get_temp(rows_id))
        .unwrap_or(1)
        .clamp(1, 3);
    let height = BAR_PAD_Y * 2.0 + ROW_H * rows as f32 + ROW_GAP * (rows - 1) as f32;
    egui::Panel::top("options_bar")
        .exact_size(height)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG_PANEL)
                .inner_margin(egui::Margin::symmetric(14, BAR_PAD_Y as i8)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.painter().hline(
                full.x_range().expand(14.0),
                full.bottom() + BAR_PAD_Y - 0.5,
                egui::Stroke::new(1.0, theme::LINE),
            );
            let content = ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, ROW_GAP);
                tool_chip(ui, tool);
                widgets::vertical_divider(ui, 20.0);
                if uses_brush_size(tool) {
                    widgets::float_slider_field(ui, "サイズ", brush_size, 1.0..=64.0, " px", 72.0);
                }
                show_tool_specific(
                    ui,
                    tool,
                    brush_hardness,
                    brush_opacity,
                    pencil_mode,
                    brush_smoothing,
                    shape_mode,
                    fill_tolerance,
                    gradient_kind,
                    gradient_colors,
                    text_font_size,
                    text_vertical,
                    text_char_spacing,
                    text_line_spacing,
                    text_outline,
                    text_outline_width,
                    secondary_color,
                    lasso_mode,
                    magic_wand_tolerance,
                    transparent_selection,
                    select_brush_erase,
                );
            });
            let needed = ((content.response.rect.height() + ROW_GAP) / (ROW_H + ROW_GAP))
                .round()
                .max(1.0) as usize;
            if needed != rows {
                ui.ctx().data_mut(|d| d.insert_temp(rows_id, needed));
                ui.ctx().request_repaint();
            }
        });
}

const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 6.0;
const BAR_PAD_Y: f32 = 9.0;
/// スライダーのレールの幅(サイズ以外)。
const TRACK_W: f32 = 56.0;

/// ブラシサイズを使うツール(`app.rs` の `ToolCtx::brush_size` を読むもの)。
fn uses_brush_size(tool: ToolKind) -> bool {
    matches!(
        tool,
        ToolKind::Pen
            | ToolKind::Eraser
            | ToolKind::Line
            | ToolKind::Rect
            | ToolKind::Ellipse
            | ToolKind::SelectBrush
    )
}

/// 先頭の「いまのツール」(アクセント色のアイコン+名前+ショートカット)。
fn tool_chip(ui: &mut egui::Ui, tool: ToolKind) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(17.0, 17.0), egui::Sense::hover());
        icons::paint_tool_icon(tool, ui.painter(), rect, theme::ACCENT);
        ui.label(
            egui::RichText::new(toolbar::tool_name(tool))
                .strong()
                .color(theme::TEXT),
        );
        let shortcut = keymap::tool_shortcut_label(tool);
        if !shortcut.is_empty() {
            widgets::shortcut_badge(ui, &shortcut);
        }
    });
}

/// SPEC §3 の「ツール固有」領域: 現在のツールに応じたオプションを出す。
#[allow(clippy::too_many_arguments)]
fn show_tool_specific(
    ui: &mut egui::Ui,
    tool: ToolKind,
    brush_hardness: &mut u8,
    brush_opacity: &mut u8,
    pencil_mode: &mut bool,
    brush_smoothing: &mut u8,
    shape_mode: Option<&mut ShapeMode>,
    fill_tolerance: &mut u8,
    gradient_kind: &mut GradientKind,
    gradient_colors: &mut GradientColors,
    text_font_size: &mut f32,
    text_vertical: &mut bool,
    text_char_spacing: &mut u8,
    text_line_spacing: &mut u8,
    text_outline: &mut bool,
    text_outline_width: &mut u8,
    secondary_color: egui::Color32,
    lasso_mode: LassoMode,
    magic_wand_tolerance: &mut u8,
    transparent_selection: &mut bool,
    select_brush_erase: bool,
) {
    match tool {
        ToolKind::Pen | ToolKind::Eraser => {
            show_brush_engine_options(
                ui,
                tool,
                brush_hardness,
                brush_opacity,
                pencil_mode,
                brush_smoothing,
            );
        }
        ToolKind::Rect | ToolKind::Ellipse => {
            if let Some(mode) = shape_mode {
                widgets::field_label(ui, "モード");
                widgets::segmented(
                    ui,
                    mode,
                    &[
                        (ShapeMode::Outline, ShapeMode::Outline.label()),
                        (ShapeMode::Fill, ShapeMode::Fill.label()),
                        (ShapeMode::Both, ShapeMode::Both.label()),
                    ],
                );
            }
        }
        ToolKind::Fill => {
            u8_slider(ui, "許容値", fill_tolerance, 0, 255, "", 120.0);
        }
        ToolKind::Gradient => {
            widgets::field_label(ui, "種類");
            widgets::segmented(
                ui,
                gradient_kind,
                &[
                    (GradientKind::Linear, "線形"),
                    (GradientKind::Radial, "円形"),
                ],
            );
            widgets::field_label(ui, "色");
            widgets::segmented(
                ui,
                gradient_colors,
                &[
                    (
                        GradientColors::PrimaryToSecondary,
                        GradientColors::PrimaryToSecondary.label(),
                    ),
                    (
                        GradientColors::PrimaryToTransparent,
                        GradientColors::PrimaryToTransparent.label(),
                    ),
                ],
            );
        }
        ToolKind::Text => {
            widgets::float_slider_field(ui, "文字サイズ", text_font_size, 8.0..=144.0, " px", 80.0);
            widgets::toggle_chip(ui, text_vertical, "縦書き")
                .on_hover_text("ON: 右の列から左へ縦に組む(改行で新しい列が左に増えます)");
            u8_slider(
                ui,
                "文字間",
                text_char_spacing,
                0,
                settings::MAX_TEXT_CHAR_SPACING,
                " px",
                TRACK_W,
            );
            u8_slider(
                ui,
                "行間",
                text_line_spacing,
                0,
                settings::MAX_TEXT_LINE_SPACING,
                " px",
                TRACK_W,
            );
            widgets::toggle_chip(ui, text_outline, "袋文字")
                .on_hover_text("ON: 文字の外側にセカンダリ色の縁を付ける(塗りはプライマリ色)");
            let outline = *text_outline;
            ui.add_enabled_ui(outline, |ui| {
                ui.horizontal(|ui| {
                    u8_slider(
                        ui,
                        "縁",
                        text_outline_width,
                        settings::MIN_TEXT_OUTLINE_WIDTH,
                        settings::MAX_TEXT_OUTLINE_WIDTH,
                        " px",
                        TRACK_W,
                    );
                    outline_color_swatch(ui, secondary_color);
                });
            });
        }
        ToolKind::Lasso => {
            widgets::field_label(ui, &lasso_mode_label(lasso_mode));
            transparent_selection_checkbox(ui, transparent_selection);
        }
        ToolKind::MagicWand => {
            u8_slider(ui, "許容値", magic_wand_tolerance, 0, 255, "", 120.0);
            transparent_selection_checkbox(ui, transparent_selection);
        }
        ToolKind::SelectBrush => {
            widgets::field_label(ui, &select_brush_mode_label(select_brush_erase));
            transparent_selection_checkbox(ui, transparent_selection);
        }
        ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move => {
            transparent_selection_checkbox(ui, transparent_selection);
        }
        ToolKind::Line | ToolKind::Picker | ToolKind::Pan | ToolKind::Zoom => {
            ui.label(
                egui::RichText::new("このツールに設定項目はありません").color(theme::TEXT_FAINT),
            );
        }
    }
}

/// `u8` の設定値を「ラベル+スライダー+数値欄」で編集する。
fn u8_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    min: u8,
    max: u8,
    suffix: &str,
    track_width: f32,
) {
    let mut v = *value as i32;
    if widgets::int_slider_field(
        ui,
        label,
        &mut v,
        min as i32..=max as i32,
        suffix,
        track_width,
        true,
    ) {
        *value = v.clamp(min as i32, max as i32) as u8;
    }
}

fn transparent_selection_checkbox(ui: &mut egui::Ui, value: &mut bool) {
    widgets::toggle_chip(ui, value, "透明な選択")
        .on_hover_text("ON: セカンダリ色と一致する画素を持ち上げない(貼り付けにも適用)");
}

/// 再発防止)。
fn lasso_mode_label(mode: LassoMode) -> String {
    let shortcut = keymap::label_for(Action::CycleLassoMode);
    format!("モード: {}({shortcut} で切替)", mode.label())
}

/// v12 §52.2: 「縁: セカンダリ色」の隣に出す小さな色見本(クリックしても
/// 何も起きない表示専用 — 色の変更は右パネルのカラーパネルで行う)。
fn outline_color_swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        // 半透明のセカンダリ色でも分かるよう、市松の上に載せる。
        let painter = ui.painter();
        let half = rect.size() / 2.0;
        for (ix, iy) in [(0, 0), (1, 1)] {
            let min = rect.min + egui::vec2(ix as f32 * half.x, iy as f32 * half.y);
            painter.rect_filled(
                egui::Rect::from_min_size(min, half),
                0.0,
                egui::Color32::from_gray(150),
            );
        }
        painter.rect_filled(rect, 2.0, color);
        painter.rect_stroke(
            rect,
            2.0,
            ui.visuals().widgets.inactive.bg_stroke,
            egui::StrokeKind::Middle,
        );
    }
    response.on_hover_text("縁の色(セカンダリ色。右パネルまたは X キーで入れ替え)");
}

/// v12 §51.2: 選択ブラシのモード表示。Alt を押している間は「消去」になる
/// (`lasso_mode_label` と同じく、純関数にしてテストできるようにする)。
fn select_brush_mode_label(erase: bool) -> String {
    let mode = if erase { "消去" } else { "追加" };
    format!("モード: {mode}(Alt ドラッグで消去)")
}

/// SPEC §17: ブラシ(旧ペン)・消しゴム共通のストロークエンジンオプション
/// (硬さ・不透明度/強さ・鉛筆モード・SPEC §25 のスムージング)。
fn show_brush_engine_options(
    ui: &mut egui::Ui,
    tool: ToolKind,
    brush_hardness: &mut u8,
    brush_opacity: &mut u8,
    pencil_mode: &mut bool,
    brush_smoothing: &mut u8,
) {
    // SPEC §17: 「鉛筆モード…硬さ無視」なので、鉛筆モード中は硬さスライダー
    // を無効化(グレーアウト)する。
    let mut hardness = *brush_hardness as i32;
    if widgets::int_slider_field(
        ui,
        "硬さ",
        &mut hardness,
        0..=100,
        "%",
        TRACK_W,
        !*pencil_mode,
    ) {
        *brush_hardness = hardness.clamp(0, 100) as u8;
    }

    // SPEC §17: 消しゴムでは同じ値を「強さ」というラベルで表示する。
    let opacity_label = if tool == ToolKind::Eraser {
        "強さ"
    } else {
        "不透明度"
    };
    let mut opacity = *brush_opacity as i32;
    if widgets::int_slider_field(ui, opacity_label, &mut opacity, 1..=100, "%", TRACK_W, true) {
        *brush_opacity = opacity.clamp(1, 100) as u8;
    }

    // SPEC §25: 「スムージング(手ブレ補正)…ブラシ/消しゴム/鉛筆のオプション
    // に 0–100%」。鉛筆モードでも有効(硬さと違い、無視されない)。
    let mut smoothing = *brush_smoothing as i32;
    if widgets::int_slider_field(ui, "補正", &mut smoothing, 0..=100, "%", TRACK_W, true) {
        *brush_smoothing = smoothing.clamp(0, 100) as u8;
    }

    widgets::toggle_chip(ui, pencil_mode, "鉛筆モード")
        .on_hover_text("ON: アンチエイリアスなしの 1px 単位で描く(硬さは無視)");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v4 レビューで発見・修正したバグの回帰テスト: なげなわのオプション
    /// バー表示は `keymap::label_for(Action::CycleLassoMode)` から生成
    /// されなければならず、"Shift+L" を固定文字列で埋め込んではいけない
    /// (KEYMAP のバインドが将来変わった場合に自動で追随するように)。
    #[test]
    fn lasso_mode_label_is_derived_from_keymap_not_hardcoded() {
        let shortcut = keymap::label_for(Action::CycleLassoMode);
        assert_eq!(
            shortcut, "Shift+L",
            "sanity check on today's keymap.rs binding"
        );

        let label = lasso_mode_label(LassoMode::Freehand);
        assert!(
            label.contains(&shortcut),
            "the label must embed keymap::label_for's current value, not a literal copy: {label}"
        );
        assert_eq!(label, "モード: 自由(Shift+L で切替)");

        let label = lasso_mode_label(LassoMode::Polygon);
        assert_eq!(label, "モード: 多角形(Shift+L で切替)");
    }

    /// v12 §51.2: 選択ブラシのモード表示は Alt の押下に追随する。
    #[test]
    fn select_brush_mode_label_reflects_the_alt_modifier() {
        assert_eq!(
            select_brush_mode_label(false),
            "モード: 追加(Alt ドラッグで消去)"
        );
        assert_eq!(
            select_brush_mode_label(true),
            "モード: 消去(Alt ドラッグで消去)"
        );
    }
}
