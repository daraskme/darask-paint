//! 消しゴム(SPEC §17)。ブラシと同じストロークエンジン(`tools/brush.rs`)を
//! 使い、不透明度は「強さ」として扱う(カバレッジ×強さぶんアルファを
//! 減らす)。硬さ・鉛筆モード・Shift+クリック連結もブラシと共通。

use eframe::egui::{self, Color32};

use super::brush::{BrushEngine, BrushParams};
use super::{brush_radius, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;

pub struct EraserTool {
    engine: BrushEngine,
}

impl EraserTool {
    pub fn new() -> Self {
        Self {
            engine: BrushEngine::new(),
        }
    }

    fn params(ctx: &ToolCtx) -> BrushParams {
        BrushParams {
            radius: brush_radius(ctx.brush_size),
            hardness: ctx.hardness,
            opacity: ctx.opacity,
            pencil: ctx.pencil,
            erase: true,
        }
    }

    /// ドキュメント差し替え時に呼ぶ(`BrushEngine::reset_for_new_document`
    /// 参照)。
    pub fn reset_for_new_document(&mut self) {
        self.engine.reset_for_new_document();
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
        let params = Self::params(ctx);
        let _ = self.engine.handle(ev, ctx, params);
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        // 消しゴムには「色」の概念がないため、確定してもリターン値は無視して
        // よい(最近使った色には積まない、`event` と同じ扱い)。
        let _ = self.engine.cancel(ctx);
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
        // 実際に使われるのは app.rs の `cursor_for_active_tool`
        // (ブラシ半径の円カーソルに置き換わる、SPEC §17)。`Tool` トレイトの
        // 要求を満たすためのフォールバック値。
        egui::CursorIcon::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Document};
    use crate::history::History;
    use eframe::egui::{Modifiers, PointerButton, Pos2};

    fn ctx<'a>(
        doc: &'a mut Document,
        history: &'a mut History,
        used: &'a mut Vec<Color32>,
    ) -> ToolCtx<'a> {
        ToolCtx {
            doc,
            history,
            primary: Color32::from_rgb(10, 20, 30),
            secondary: Color32::from_rgb(200, 100, 50),
            brush_size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            pencil: false,
            used_colors: used,
        }
    }

    #[test]
    fn full_strength_erase_makes_pixel_transparent_and_does_not_record_used_color() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut eraser = EraserTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        eraser.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        eraser.event(
            ToolEvent::Up {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );
        assert_eq!(doc.get_pixel(5, 5), Some([255, 255, 255, 0]));
        assert!(used.is_empty(), "eraser must not push to recent colors");
    }

    #[test]
    fn partial_strength_leaves_partial_alpha() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut eraser = EraserTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        c.opacity = 0.3; // 「強さ」30%。
        eraser.event(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        eraser.event(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );
        let alpha = doc.get_pixel(10, 10).unwrap()[3];
        assert!(
            alpha > 170 && alpha < 190,
            "expected ~70% alpha remaining, got {alpha}"
        );
    }

    #[test]
    fn undo_restores_erased_pixels_byte_exactly() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();
        let mut eraser = EraserTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        eraser.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        eraser.event(
            ToolEvent::Up {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );
        assert_ne!(doc.active_pixels(), original.as_slice());
        assert!(history.undo(&mut doc));
        assert_eq!(doc.active_pixels(), original.as_slice());
    }

    #[test]
    fn cancel_mid_drag_commits_partial_erase_as_undo_unit() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut eraser = EraserTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        eraser.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        assert!(c.history.has_open_stroke());
        eraser.cancel(&mut c);
        assert!(!c.history.has_open_stroke());
        assert!(c.history.can_undo());
    }
}
