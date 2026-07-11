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

use eframe::egui;

use crate::tools::shapes::ShapeMode;
use crate::tools::ToolKind;

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
    /// 現在のツールが矩形/楕円のときだけ `Some`(そのツール自身の `mode` の
    /// 可変参照)。
    pub shape_mode: Option<&'a mut ShapeMode>,
    pub fill_tolerance: &'a mut u8,
    /// SPEC §19: テキストツールのフォントサイズ(8–144px、デフォルト 24)。
    pub text_font_size: &'a mut f32,
}

pub fn show(ui: &mut egui::Ui, state: OptionsBarCtx) {
    let OptionsBarCtx {
        tool,
        brush_size,
        brush_hardness,
        brush_opacity,
        pencil_mode,
        shape_mode,
        fill_tolerance,
        text_font_size,
    } = state;

    egui::Panel::top("options_bar")
        .exact_size(44.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("サイズ:");
                ui.add(egui::Slider::new(brush_size, 1.0..=64.0).suffix("px"));

                ui.separator();
                show_tool_specific(
                    ui,
                    tool,
                    brush_hardness,
                    brush_opacity,
                    pencil_mode,
                    shape_mode,
                    fill_tolerance,
                    text_font_size,
                );
            });
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
    shape_mode: Option<&mut ShapeMode>,
    fill_tolerance: &mut u8,
    text_font_size: &mut f32,
) {
    match tool {
        ToolKind::Pen | ToolKind::Eraser => {
            show_brush_engine_options(ui, tool, brush_hardness, brush_opacity, pencil_mode);
        }
        ToolKind::Rect | ToolKind::Ellipse => {
            if let Some(mode) = shape_mode {
                ui.label("モード:");
                ui.radio_value(mode, ShapeMode::Outline, ShapeMode::Outline.label());
                ui.radio_value(mode, ShapeMode::Fill, ShapeMode::Fill.label());
                ui.radio_value(mode, ShapeMode::Both, ShapeMode::Both.label());
            }
        }
        ToolKind::Fill => {
            ui.label("許容値:");
            let mut tol = *fill_tolerance as i32;
            if ui.add(egui::Slider::new(&mut tol, 0..=255)).changed() {
                *fill_tolerance = tol.clamp(0, 255) as u8;
            }
        }
        // SPEC §19: 「オプションバー: フォントサイズ 8–144px(デフォルト 24)」。
        ToolKind::Text => {
            ui.label("フォントサイズ:");
            ui.add(egui::Slider::new(text_font_size, 8.0..=144.0).suffix("px"));
        }
        ToolKind::Line
        | ToolKind::Picker
        | ToolKind::Select
        | ToolKind::Pan
        | ToolKind::Move
        | ToolKind::Zoom => {
            ui.weak("(ツール固有オプションなし)");
        }
    }
}

/// SPEC §17: ブラシ(旧ペン)・消しゴム共通のストロークエンジンオプション
/// (硬さ・不透明度/強さ・鉛筆モード)。
fn show_brush_engine_options(
    ui: &mut egui::Ui,
    tool: ToolKind,
    brush_hardness: &mut u8,
    brush_opacity: &mut u8,
    pencil_mode: &mut bool,
) {
    // SPEC §17: 「鉛筆モード…硬さ無視」なので、鉛筆モード中は硬さスライダー
    // を無効化(グレーアウト)する。
    ui.add_enabled_ui(!*pencil_mode, |ui| {
        ui.label("硬さ:");
        let mut hardness = *brush_hardness as i32;
        if ui
            .add(egui::Slider::new(&mut hardness, 0..=100).suffix("%"))
            .changed()
        {
            *brush_hardness = hardness.clamp(0, 100) as u8;
        }
    });

    // SPEC §17: 消しゴムでは同じ値を「強さ」というラベルで表示する。
    let opacity_label = if tool == ToolKind::Eraser {
        "強さ:"
    } else {
        "不透明度:"
    };
    ui.label(opacity_label);
    let mut opacity = *brush_opacity as i32;
    if ui
        .add(egui::Slider::new(&mut opacity, 1..=100).suffix("%"))
        .changed()
    {
        *brush_opacity = opacity.clamp(1, 100) as u8;
    }

    ui.checkbox(pencil_mode, "鉛筆モード");
}
