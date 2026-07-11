//! 消しゴム(SPEC §4: 丸筆ハードエッジで透明(alpha=0)にする)。

use eframe::egui::{self, Color32};

use super::{StrokeTool, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;

pub struct EraserTool {
    stroke: StrokeTool,
}

impl EraserTool {
    pub fn new() -> Self {
        Self {
            stroke: StrokeTool::new(),
        }
    }
}

impl Default for EraserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for EraserTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        // 消しゴムには「色」の概念がないため、確定してもリターン値
        // (どのボタンで確定したか)は無視してよい(最近使った色には積まない)。
        let _ = self.stroke.handle(ev, ctx, true);
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        // 消しゴムには「色」の概念がないため、確定してもリターン値は無視して
        // よい(最近使った色には積まない、`event` と同じ扱い)。
        let _ = self.stroke.cancel(ctx);
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
