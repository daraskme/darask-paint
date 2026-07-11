//! スポイト(SPEC §4: 左クリック=プライマリに、右クリック=セカンダリに取得)。
//!
//! `ToolCtx` は色を読み取り専用の値としてしか持たない(書き込む手段がない)
//! ため、実際の色サンプリングは `app.rs::sample_eyedropper_color` に集約して
//! ある(Alt+クリックの一時スポイトと同じ経路。app.rs のディスパッチ層が
//! `ToolKind::Picker` を特別扱いして呼ぶ)。このツール自体はカーソル形状の
//! 提供のみを担う。

use eframe::egui::{self, Color32};

use super::{Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;

#[derive(Default)]
pub struct PickerTool;

impl PickerTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for PickerTool {
    fn event(&mut self, _ev: ToolEvent, _ctx: &mut ToolCtx) {
        // 実処理は app.rs::dispatch_canvas_events が行う(上記コメント参照)。
    }

    fn draw_preview(
        &self,
        _painter: &egui::Painter,
        _view: &CanvasView,
        _primary: Color32,
        _secondary: Color32,
        _brush_size: f32,
    ) {
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}
