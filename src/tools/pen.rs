//! ブラシ(旧ペン、SPEC §17)。丸筆・常時アンチエイリアス(鉛筆モードなら
//! 2値スタンプ)。左ドラッグ=プライマリ色、右ドラッグ=セカンダリ色
//! (SPEC §4)。
//!
//! 硬さ・不透明度・鉛筆モード・Shift+クリック連結・PS opacity 意味論は
//! すべて `tools/brush.rs` の共通ストロークエンジン(`BrushEngine`)に
//! 委譲する(ARCHITECTURE.md §15.1: 「ブラシ(旧ペン)・消しゴムを共通の
//! ストロークエンジンに刷新する」)。旧「アンチエイリアス」チェックボックス
//! (v2)は廃止し、ブラシは常時 AA になった(鉛筆モードのみ非 AA)。

use eframe::egui::{self, Color32, PointerButton};

use super::brush::{BrushEngine, BrushParams};
use super::{brush_radius, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;

pub struct PenTool {
    engine: BrushEngine,
}

impl PenTool {
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
            erase: false,
            smoothing: ctx.smoothing,
        }
    }

    /// ドキュメント差し替え時に呼ぶ(`BrushEngine::reset_for_new_document`
    /// 参照)。
    pub fn reset_for_new_document(&mut self) {
        self.engine.reset_for_new_document();
    }
}

impl Default for PenTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PenTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        let params = Self::params(ctx);
        if let Some(button) = self.engine.handle(ev, ctx, params) {
            let color = if button == PointerButton::Secondary {
                ctx.secondary
            } else {
                ctx.primary
            };
            ctx.used_colors.push(color);
        }
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        if let Some(button) = self.engine.cancel(ctx, "ブラシ") {
            let color = if button == PointerButton::Secondary {
                ctx.secondary
            } else {
                ctx.primary
            };
            ctx.used_colors.push(color);
        }
    }

    fn draw_preview(
        &self,
        _painter: &egui::Painter,
        _view: &CanvasView,
        _primary: Color32,
        _secondary: Color32,
        _brush_size: f32,
    ) {
        // ストローク中に即座にドキュメントへ確定するため、ドラッグ中の独立
        // したプレビュー描画は不要(直線/図形ツールとは異なる)。ブラシ半径の
        // 円カーソル自体は `app.rs` がツール非依存に描く(SPEC §17)。
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
    use eframe::egui::{Modifiers, Pos2};

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
            smoothing: 0.0,
            used_colors: used,
            clip: None,
        }
    }

    #[test]
    fn stroke_paints_center_opaque_and_restores_on_undo() {
        let mut doc = Document::new(30, 30, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();

        let mut pen = PenTool::new();
        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            pen.event(
                ToolEvent::Down {
                    img: Pos2::new(15.0, 15.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
            );
            pen.event(
                ToolEvent::Up {
                    img: Pos2::new(15.0, 15.0),
                    button: PointerButton::Primary,
                },
                &mut c,
            );
        }
        assert_eq!(used, vec![Color32::from_rgb(10, 20, 30)]);
        assert_ne!(doc.active_pixels(), original.as_slice());

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "undo should byte-exactly restore"
        );
    }

    #[test]
    fn secondary_button_reports_secondary_color_as_used() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut pen = PenTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        pen.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Secondary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        pen.event(
            ToolEvent::Up {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Secondary,
            },
            &mut c,
        );
        assert_eq!(used, vec![Color32::from_rgb(200, 100, 50)]);
    }

    #[test]
    fn pencil_mode_via_ctx_flag_produces_no_partial_alpha() {
        let mut doc = Document::new(30, 30, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut pen = PenTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        c.pencil = true;
        pen.event(
            ToolEvent::Down {
                img: Pos2::new(15.0, 15.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        pen.event(
            ToolEvent::Up {
                img: Pos2::new(15.0, 15.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );
        for y in 10..20 {
            for x in 10..20 {
                let a = doc.get_pixel(x, y).unwrap()[3];
                assert!(a == 0 || a == 255, "expected binary alpha, got {a}");
            }
        }
    }

    #[test]
    fn cancel_mid_drag_commits_partial_stroke_as_undo_unit() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut pen = PenTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        pen.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        assert!(c.history.has_open_stroke());
        pen.cancel(&mut c);
        assert!(!c.history.has_open_stroke());
        assert!(c.history.can_undo());
    }
}
