//! ペン(SPEC §4: 丸筆・ハードエッジ(デフォルト)。オプションでアンチエイリアス
//! ON/OFF。左ドラッグ=プライマリ色、右ドラッグ=セカンダリ色)。
//!
//! アンチエイリアス ON 時は ARCHITECTURE.md §5 のカバレッジマスク方式で描く:
//! ストローク中はマスク(ドキュメント寸法の `Vec<u8>`、遅延確保・再利用)に
//! スタンプのカバレッジを `max` で書き込み、undo 用に退避された「ストローク
//! 開始前」のピクセル(history.rs の CoW タイル、`History::original_pixel`)
//! を元画像として `out = blend_over(元, color(alpha=coverage))` で毎回
//! 合成し直す。スタンプの重ね塗りによる縁の濃度ムラを防ぐため
//! (ハードエッジ時はマスク不要で `StrokeTool` が直接描く)。

use eframe::egui::{self, Color32, PointerButton};

use super::{brush_radius, color_bytes_for, StrokeTool, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;
use crate::document::IRect;
use crate::raster;

/// AA ストローク中の一時状態。
struct AaStroke {
    button: PointerButton,
    last: (f32, f32),
    color: [u8; 4],
    /// これまでにこのストロークで触れた領域の外接矩形(ストローク終了時に
    /// マスクをクリアする範囲として使う)。
    touched: Option<IRect>,
}

pub struct PenTool {
    stroke: StrokeTool,
    /// アンチエイリアス ON/OFF(オプションバーの「アンチエイリアス」
    /// チェックボックスから配線、SPEC §4)。デフォルトは OFF(ハードエッジ)。
    pub aa: bool,
    aa_stroke: Option<AaStroke>,
    /// カバレッジマスク(ドキュメント全体、遅延確保・再利用)。
    /// AA ストロークが進行していない間は全画素 0 に保たれる不変条件を守る
    /// (`finish` でストロークが触れた範囲だけをクリアする)。
    mask: Vec<u8>,
    mask_size: (u32, u32),
}

impl PenTool {
    pub fn new() -> Self {
        Self {
            stroke: StrokeTool::new(),
            aa: false,
            aa_stroke: None,
            mask: Vec::new(),
            mask_size: (0, 0),
        }
    }

    fn ensure_mask_capacity(&mut self, doc_w: u32, doc_h: u32) {
        let size = (doc_w, doc_h);
        if self.mask_size != size {
            self.mask = vec![0u8; doc_w as usize * doc_h as usize];
            self.mask_size = size;
        }
    }

    /// 1 スタンプぶんのカバレッジをマスクに `max` で書き込み、影響を受けた
    /// 画素を「ストローク開始前のピクセル」から都度合成し直す。
    fn apply_stamp(
        &mut self,
        ctx: &mut ToolCtx,
        cx: f32,
        cy: f32,
        radius: f32,
        color: [u8; 4],
        touched: &mut Option<IRect>,
    ) {
        self.ensure_mask_capacity(ctx.doc.width, ctx.doc.height);
        let bounds = raster::stamp_bounds(cx, cy, radius).clamp_to(ctx.doc.width, ctx.doc.height);
        if bounds.is_empty() {
            return;
        }
        ctx.history.ensure_tiles_saved(ctx.doc, bounds);

        let mask_w = ctx.doc.width as usize;
        // `ctx.doc`(Surface 経由でアクティブレイヤーのバッファを借用)と
        // `ctx.history`(タイル退避の読み取り)は ToolCtx の別フィールドなので
        // 同時に借用してよい(既存コードの `let doc = &mut *ctx.doc;` と同じ
        // 分割借用のイディオム)。
        let history = &*ctx.history;
        let mut surface = ctx.doc.active_surface_mut();
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                let coverage_here = raster::stamp_coverage(cx, cy, radius, x, y);
                let idx = y as usize * mask_w + x as usize;
                let Some(slot) = self.mask.get_mut(idx) else {
                    continue;
                };
                if coverage_here > *slot {
                    *slot = coverage_here;
                }
                let coverage = *slot;
                let original = history
                    .original_pixel(x, y)
                    .or_else(|| surface.get_pixel(x, y))
                    .unwrap_or([0, 0, 0, 0]);
                let alpha = ((color[3] as u32 * coverage as u32) / 255) as u8;
                let blended = raster::blend_over(original, [color[0], color[1], color[2], alpha]);
                surface.set_pixel(x, y, blended);
            }
        }
        drop(surface);
        ctx.doc.mark_dirty(bounds);
        *touched = Some(match touched {
            Some(t) => t.union(&bounds),
            None => bounds,
        });
    }

    /// `raster::stroke_segment` と同じ間隔ポリシーでスタンプを並べる
    /// (ARCHITECTURE.md §5: 間隔 <= max(1px, radius/2))。
    fn apply_segment(
        &mut self,
        ctx: &mut ToolCtx,
        from: (f32, f32),
        to: (f32, f32),
        radius: f32,
        color: [u8; 4],
        touched: &mut Option<IRect>,
    ) {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let dist = (dx * dx + dy * dy).sqrt();
        let step = (radius / 2.0).max(1.0);
        let steps = (dist / step).ceil().max(1.0) as u32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = from.0 + dx * t;
            let y = from.1 + dy * t;
            self.apply_stamp(ctx, x, y, radius, color, touched);
        }
    }

    /// ストロークが触れた矩形ぶんだけマスクを 0 に戻す(次のストロークに
    /// 備えて「アイドル時は全画素 0」の不変条件を保つ)。
    fn clear_mask_region(&mut self, rect: IRect) {
        let rect = rect.clamp_to(self.mask_size.0, self.mask_size.1);
        if rect.is_empty() {
            return;
        }
        let w = self.mask_size.0 as usize;
        for y in rect.y0..rect.y1 {
            let start = y as usize * w + rect.x0 as usize;
            let end = start + rect.width() as usize;
            if let Some(slice) = self.mask.get_mut(start..end) {
                slice.fill(0);
            }
        }
    }

    /// AA モードのイベント処理。ストロークが確定したら、その色を選んだ
    /// ボタンを返す(呼び出し側が「最近使った色」に記録するため)。
    fn handle_aa(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) -> Option<PointerButton> {
        match ev {
            ToolEvent::Down { img, button, .. } => {
                if !matches!(button, PointerButton::Primary | PointerButton::Secondary) {
                    return None;
                }
                let color = color_bytes_for(ctx, button, false);
                let radius = brush_radius(ctx.brush_size);
                ctx.history.begin_stroke(ctx.doc.active);
                let mut touched = None;
                self.apply_stamp(ctx, img.x, img.y, radius, color, &mut touched);
                self.aa_stroke = Some(AaStroke {
                    button,
                    last: (img.x, img.y),
                    color,
                    touched,
                });
                None
            }
            ToolEvent::Drag { img, button, .. } => {
                let Some(state) = self.aa_stroke.as_ref() else {
                    return None;
                };
                if state.button != button {
                    return None;
                }
                let from = state.last;
                let color = state.color;
                let mut touched = state.touched;
                let radius = brush_radius(ctx.brush_size);
                self.apply_segment(ctx, from, (img.x, img.y), radius, color, &mut touched);
                if let Some(state) = self.aa_stroke.as_mut() {
                    state.last = (img.x, img.y);
                    state.touched = touched;
                }
                None
            }
            ToolEvent::Up { button, .. } => {
                let Some(state) = self.aa_stroke.take() else {
                    return None;
                };
                if state.button != button {
                    // 別ボタンの Up(通常は起きないが安全側に倒す): 状態を戻す。
                    self.aa_stroke = Some(state);
                    return None;
                }
                ctx.history.commit_stroke(ctx.doc);
                if let Some(bbox) = state.touched {
                    self.clear_mask_region(bbox);
                }
                Some(button)
            }
            ToolEvent::Hover { .. } => None,
        }
    }
}

impl Default for PenTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PenTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        let committed = if self.aa {
            self.handle_aa(ev, ctx)
        } else {
            self.stroke.handle(ev, ctx, false)
        };
        if let Some(button) = committed {
            let color = if button == PointerButton::Secondary {
                ctx.secondary
            } else {
                ctx.primary
            };
            ctx.used_colors.push(color);
        }
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        let committed = if self.aa {
            let Some(state) = self.aa_stroke.take() else {
                return;
            };
            ctx.history.commit_stroke(ctx.doc);
            if let Some(bbox) = state.touched {
                self.clear_mask_region(bbox);
            }
            Some(state.button)
        } else {
            self.stroke.cancel(ctx)
        };
        if let Some(button) = committed {
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
        // ハードエッジ/AA いずれもストローク中に即座にドキュメントへ確定する
        // ため、ドラッグ中の独立したプレビュー描画は不要(直線/図形ツールとは
        // 異なる)。
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Document};
    use crate::history::History;
    use eframe::egui::Pos2;

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
            used_colors: used,
        }
    }

    #[test]
    fn aa_stroke_paints_center_opaque_and_restores_on_undo() {
        let mut doc = Document::new(30, 30, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();

        let mut pen = PenTool::new();
        pen.aa = true;
        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            pen.event(
                ToolEvent::Down {
                    img: Pos2::new(15.0, 15.0),
                    button: PointerButton::Primary,
                    mods: Default::default(),
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
    fn aa_mask_is_cleared_after_stroke_so_next_stroke_starts_fresh() {
        let mut doc = Document::new(30, 30, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut pen = PenTool::new();
        pen.aa = true;

        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            pen.event(
                ToolEvent::Down {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Default::default(),
                },
                &mut c,
            );
            pen.event(
                ToolEvent::Up {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                },
                &mut c,
            );
        }
        assert!(
            pen.mask.iter().all(|&v| v == 0),
            "mask should be all-zero at rest"
        );
    }

    #[test]
    fn hard_edge_stroke_still_reports_used_color() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut pen = PenTool::new(); // aa = false (default)

        let mut c = ctx(&mut doc, &mut history, &mut used);
        pen.event(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Secondary,
                mods: Default::default(),
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
}
