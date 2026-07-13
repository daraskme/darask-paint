//! グラデーションツール(SPEC §23、ARCHITECTURE.md §16.4)。
//!
//! ドラッグで始点→終点、離して確定(1 undo 単位)。ドラッグ中は
//! `draw_preview` が示す始点→終点の線に加え、キャンバス自体にライブ
//! プレビューを描く(ARCHITECTURE.md §16.4: 「ドラッグ中はライブプレビュー
//! …開始時スナップショット=StrokeRecorder の CoW タイルから再合成」)。
//! 塗りつぶし(§16.3 の巡回対象)と同様、選択(`ctx.clip`)があればその中だけ、
//! アクティブレイヤー対象。
//!
//! ライブプレビューの実装は、色調補正のモーダルプレビュー
//! (ARCHITECTURE.md §16.5)と全く同じ考え方を使う: `Down` で
//! `History::begin_stroke`/`ensure_tiles_saved` により対象領域全体(選択
//! bbox、無ければアクティブレイヤー全体)の「触る前」のピクセルを退避して
//! おき、`Drag`/`Up` のたびに**その退避済みの元ピクセルから毎回計算し直して
//! 書く**(スライダーを往復しても劣化する累積適用にならない、
//! ARCHITECTURE.md §16.10-4 と同じ原則)。

use eframe::egui::{self, Color32, PointerButton, Pos2};

use super::{color_to_straight_rgba, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;
use crate::document::IRect;
use crate::raster::{self, GradientKind};

/// SPEC §23: 「色: プライマリ→セカンダリ / プライマリ→透明」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientColors {
    PrimaryToSecondary,
    PrimaryToTransparent,
}

impl GradientColors {
    pub fn label(self) -> &'static str {
        match self {
            GradientColors::PrimaryToSecondary => "プライマリ→セカンダリ",
            GradientColors::PrimaryToTransparent => "プライマリ→透明",
        }
    }

    /// 実際に補間する 2 色(straight-alpha RGBA8)を解決する。
    fn endpoints(self, primary: Color32, secondary: Color32) -> ([u8; 4], [u8; 4]) {
        let p = color_to_straight_rgba(primary);
        match self {
            GradientColors::PrimaryToSecondary => (p, color_to_straight_rgba(secondary)),
            GradientColors::PrimaryToTransparent => (p, [p[0], p[1], p[2], 0]),
        }
    }
}

#[derive(Clone, Copy)]
struct DragState {
    start: Pos2,
    current: Pos2,
}

pub struct GradientTool {
    drag: Option<DragState>,
    /// SPEC §23: オプションバーの「種類」(線形/円形)。
    pub kind: GradientKind,
    /// SPEC §23: オプションバーの「色」。
    pub colors: GradientColors,
}

impl GradientTool {
    pub fn new() -> Self {
        Self {
            drag: None,
            kind: GradientKind::Linear,
            colors: GradientColors::PrimaryToSecondary,
        }
    }

    /// グラデーションが対象とする領域(SPEC §23: 「選択範囲があればその中
    /// だけ」。無ければアクティブレイヤー全体)。
    fn target_rect(ctx: &ToolCtx) -> IRect {
        ctx.clip.map(|c| c.bbox).unwrap_or(IRect {
            x0: 0,
            y0: 0,
            x1: ctx.doc.width as i32,
            y1: ctx.doc.height as i32,
        })
    }

    /// `target_rect` 全域を、`History` に退避済みの「ストローク開始前」の
    /// ピクセルから毎回再計算して書く(累積適用にならない、モジュール冒頭の
    /// コメント参照)。
    fn apply(&self, ctx: &mut ToolCtx, drag: DragState) {
        let bounds = Self::target_rect(ctx).clamp_to(ctx.doc.width, ctx.doc.height);
        if bounds.is_empty() {
            return;
        }
        let (c0, c1) = self.colors.endpoints(ctx.primary, ctx.secondary);
        let p0 = (drag.start.x, drag.start.y);
        let p1 = (drag.current.x, drag.current.y);
        let kind = self.kind;
        // v4-M2 性能改善(ARCHITECTURE.md §16.1、`OriginalPixelCursor` の
        // ドキュメント参照): 対象領域全体の画素ループで 1 個のカーソルを
        // 使い回し、行ごとに `stroke.tiles` の `HashMap` を引き直さない。
        let mut original_cursor = ctx.history.original_pixel_cursor();
        let mut surface = ctx.doc.active_surface_mut(ctx.clip);
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                let Some(original) = original_cursor.get(x, y) else {
                    continue;
                };
                let p = (x as f32 + 0.5, y as f32 + 0.5);
                let t = raster::gradient_span(kind, p0, p1, p);
                let gradient_color = raster::lerp_color(c0, c1, t);
                let blended = raster::blend_over(original, gradient_color);
                surface.set_pixel(x, y, blended);
            }
        }
        ctx.doc.mark_dirty(bounds);
    }
}

impl Default for GradientTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for GradientTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        match ev {
            ToolEvent::Down { img, button, .. } => {
                // SPEC §23 は色をオプションバーで選ぶため、ペン/図形のような
                // 「右ドラッグ=セカンダリ色」の慣習は適用しない(SPEC の
                // 表に記載が無い)。左ドラッグのみを受け付ける。
                if button != PointerButton::Primary {
                    return;
                }
                let bounds = Self::target_rect(ctx).clamp_to(ctx.doc.width, ctx.doc.height);
                ctx.history.begin_stroke(ctx.doc.active);
                if !bounds.is_empty() {
                    ctx.history.ensure_tiles_saved(ctx.doc, bounds);
                }
                let drag = DragState {
                    start: img,
                    current: img,
                };
                self.apply(ctx, drag);
                self.drag = Some(drag);
            }
            ToolEvent::Drag { img, button, .. } => {
                if button != PointerButton::Primary {
                    return;
                }
                let Some(drag) = self.drag.as_mut() else {
                    return;
                };
                drag.current = img;
                let snapshot = *drag;
                self.apply(ctx, snapshot);
            }
            ToolEvent::Up { img, button } => {
                if button != PointerButton::Primary {
                    return;
                }
                let Some(mut drag) = self.drag.take() else {
                    return;
                };
                drag.current = img;
                self.apply(ctx, drag);
                ctx.history.commit_stroke(ctx.doc);
                ctx.used_colors.push(ctx.primary);
                if self.colors == GradientColors::PrimaryToSecondary {
                    ctx.used_colors.push(ctx.secondary);
                }
            }
            ToolEvent::Hover { .. } => {}
        }
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        // ツール切替時にドラッグ中のグラデーションがあれば、直近のドラッグ
        // 位置で確定する(ARCHITECTURE.md §6「1 操作 = 1 undo 単位」を、
        // ツール切替という中断経路でも守るため。`ShapeTool::cancel` と同じ
        // 方針)。
        if let Some(drag) = self.drag.take() {
            self.apply(ctx, drag);
            ctx.history.commit_stroke(ctx.doc);
        }
    }

    fn draw_preview(
        &self,
        painter: &egui::Painter,
        view: &CanvasView,
        _primary: Color32,
        _secondary: Color32,
        _brush_size: f32,
    ) {
        // SPEC §23: 「ドラッグ中はプレビュー線」。実際の色の変化はライブ
        // プレビュー(`apply` が毎フレーム書く)がキャンバス画像そのものに
        // 反映するので、ここでは始点→終点をブラシカーソルと同じ二重線
        // (白+黒)で示すだけでよい。
        let Some(drag) = &self.drag else {
            return;
        };
        let a = view.img_to_screen_pos(drag.start);
        let b = view.img_to_screen_pos(drag.current);
        painter.line_segment([a, b], egui::Stroke::new(3.0, egui::Color32::WHITE));
        painter.line_segment([a, b], egui::Stroke::new(1.0, egui::Color32::BLACK));
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Document, SelMask};
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
            primary: Color32::from_rgba_unmultiplied(255, 0, 0, 255),
            secondary: Color32::from_rgba_unmultiplied(0, 0, 255, 255),
            brush_size: 4.0,
            hardness: 1.0,
            opacity: 1.0,
            pencil: false,
            smoothing: 0.0,
            used_colors: used,
            clip: None,
        }
    }

    #[test]
    fn drag_paints_a_gradient_from_primary_to_secondary_and_restores_on_undo() {
        let mut doc = Document::new(20, 10, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();
        let mut tool = GradientTool::new();

        // gradient_span はほかの raster.rs 関数(stamp_round 等)と同じく
        // 画素中心 (x+0.5, y+0.5) で評価するため、ちょうど画素中心に一致する
        // 座標でドラッグして厳密一致のアサーションを書けるようにする。
        let mut c = ctx(&mut doc, &mut history, &mut used);
        tool.event(
            ToolEvent::Down {
                img: Pos2::new(0.5, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        tool.event(
            ToolEvent::Up {
                img: Pos2::new(19.5, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );

        assert_eq!(doc.get_pixel(0, 5), Some([255, 0, 0, 255]));
        assert_eq!(doc.get_pixel(19, 5), Some([0, 0, 255, 255]));
        // SPEC §5: 「1 回の確定で 2 色使うツールは複数 push してよい」
        // (矩形/楕円の「両方」モードと同じ規則)。
        assert_eq!(
            used,
            vec![
                Color32::from_rgba_unmultiplied(255, 0, 0, 255),
                Color32::from_rgba_unmultiplied(0, 0, 255, 255),
            ]
        );

        assert!(history.undo(&mut doc));
        assert_eq!(doc.active_pixels(), original.as_slice());
    }

    #[test]
    fn primary_to_transparent_fades_alpha_over_the_original_background() {
        let mut doc = Document::new(20, 4, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut tool = GradientTool::new();
        tool.colors = GradientColors::PrimaryToTransparent;

        // 画素中心に一致する座標でドラッグする(上のテストと同じ理由)。
        let mut c = ctx(&mut doc, &mut history, &mut used);
        tool.event(
            ToolEvent::Down {
                img: Pos2::new(0.5, 2.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        tool.event(
            ToolEvent::Up {
                img: Pos2::new(19.5, 2.0),
                button: PointerButton::Primary,
            },
            &mut c,
        );

        // 始点はプライマリ色そのもの、終点は透明(=元の白背景がそのまま
        // 見える、blend_over の性質)。
        assert_eq!(doc.get_pixel(0, 2), Some([255, 0, 0, 255]));
        assert_eq!(doc.get_pixel(19, 2), Some([255, 255, 255, 255]));
        // セカンダリは使われないので「最近使った色」には積まない。
        assert_eq!(used, vec![Color32::from_rgba_unmultiplied(255, 0, 0, 255)]);
    }

    #[test]
    fn radial_gradient_center_is_primary_and_beyond_radius_is_secondary() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut tool = GradientTool::new();
        tool.kind = GradientKind::Radial;

        // 中心を画素 (10,10) の中心(10.5, 10.5)ちょうどに合わせる(距離 0 を
        // 保証するため)。
        let mut c = ctx(&mut doc, &mut history, &mut used);
        tool.event(
            ToolEvent::Down {
                img: Pos2::new(10.5, 10.5),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        tool.event(
            ToolEvent::Up {
                img: Pos2::new(15.5, 10.5), // 半径 5。
                button: PointerButton::Primary,
            },
            &mut c,
        );

        assert_eq!(doc.get_pixel(10, 10), Some([255, 0, 0, 255]));
        // 半径の外(距離 10 > 半径 5)は端色(セカンダリ)にクランプされる。
        assert_eq!(doc.get_pixel(0, 10), Some([0, 0, 255, 255]));
    }

    #[test]
    fn gradient_is_clipped_to_the_selection_mask() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut tool = GradientTool::new();

        let clip = SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 20,
            },
            mask: vec![255u8; 10 * 20],
        };

        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            c.clip = Some(&clip);
            tool.event(
                ToolEvent::Down {
                    img: Pos2::new(0.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
            );
            tool.event(
                ToolEvent::Up {
                    img: Pos2::new(19.0, 10.0),
                    button: PointerButton::Primary,
                },
                &mut c,
            );
        }

        // クリップ内(x<10)は変化しているはず。
        assert_ne!(doc.get_pixel(5, 10), Some([255, 255, 255, 255]));
        // クリップ外(x>=10)は元のまま。
        assert_eq!(doc.get_pixel(15, 10), Some([255, 255, 255, 255]));
    }

    #[test]
    fn cancel_mid_drag_commits_the_current_preview_as_one_undo_unit() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut tool = GradientTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        tool.event(
            ToolEvent::Down {
                img: Pos2::new(0.0, 0.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        assert!(c.history.has_open_stroke());
        tool.cancel(&mut c);
        assert!(!c.history.has_open_stroke());
        assert!(c.history.can_undo());
    }

    #[test]
    fn right_button_drag_does_nothing() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();
        let mut tool = GradientTool::new();

        let mut c = ctx(&mut doc, &mut history, &mut used);
        tool.event(
            ToolEvent::Down {
                img: Pos2::new(0.0, 0.0),
                button: PointerButton::Secondary,
                mods: Modifiers::NONE,
            },
            &mut c,
        );
        tool.event(
            ToolEvent::Up {
                img: Pos2::new(9.0, 9.0),
                button: PointerButton::Secondary,
            },
            &mut c,
        );
        assert_eq!(doc.active_pixels(), original.as_slice());
        assert!(!history.has_open_stroke());
    }
}
