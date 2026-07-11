//! オプションバー(SPEC §3 の画面構成、§4 の内容)。
//!
//! M3 で実配線した:
//! - ブラシサイズスライダー(1–64px、SPEC §4)。
//! - ツール固有オプション: ペンのアンチエイリアス ON/OFF、矩形/楕円の
//!   枠線のみ/塗りつぶし/両方モード、塗りつぶしの許容値。
//!
//! v2 M3(ARCHITECTURE.md §14.7/§14.8)で色関連(スウォッチ・最近使った色)
//! を右パネルの `color_panel` へ移設した。サイズ・ツール固有オプションだけ
//! がここに残る。

use eframe::egui;

use crate::tools::shapes::ShapeMode;
use crate::tools::ToolKind;

/// `show` に渡すオプションバーの状態一式。app.rs が各フィールドの持ち主
/// (`DaraskApp`/各 `Tool`)から可変参照を集めて構築する。
pub struct OptionsBarCtx<'a> {
    pub tool: ToolKind,
    pub brush_size: &'a mut f32,
    pub pen_aa: &'a mut bool,
    /// 現在のツールが矩形/楕円のときだけ `Some`(そのツール自身の `mode` の
    /// 可変参照)。
    pub shape_mode: Option<&'a mut ShapeMode>,
    pub fill_tolerance: &'a mut u8,
}

pub fn show(ui: &mut egui::Ui, state: OptionsBarCtx) {
    let OptionsBarCtx {
        tool,
        brush_size,
        pen_aa,
        shape_mode,
        fill_tolerance,
    } = state;

    egui::Panel::top("options_bar")
        .exact_size(44.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("サイズ:");
                ui.add(egui::Slider::new(brush_size, 1.0..=64.0).suffix("px"));

                ui.separator();
                show_tool_specific(ui, tool, pen_aa, shape_mode, fill_tolerance);
            });
        });
}

/// SPEC §3 の「ツール固有」領域: 現在のツールに応じたオプションを出す。
fn show_tool_specific(
    ui: &mut egui::Ui,
    tool: ToolKind,
    pen_aa: &mut bool,
    shape_mode: Option<&mut ShapeMode>,
    fill_tolerance: &mut u8,
) {
    match tool {
        ToolKind::Pen => {
            ui.checkbox(pen_aa, "アンチエイリアス");
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
        ToolKind::Line | ToolKind::Eraser | ToolKind::Picker | ToolKind::Select | ToolKind::Pan => {
            ui.weak("(ツール固有オプションなし)");
        }
    }
}
