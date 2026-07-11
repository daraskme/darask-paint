//! 塗りつぶし(SPEC §4: スキャンライン flood fill。許容値スライダー 0–255
//! (デフォルト 0)。連結領域のみ)。
//!
//! クリック(`ToolEvent::Down`)で即座に確定する 1 ショットのツール
//! (ドラッグ/プレビューはない)。左クリック=プライマリ色、右クリック=
//! セカンダリ色(ペンと同じ慣習)。

use eframe::egui::{self, Color32, PointerButton};

use super::{color_bytes_for, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;
use crate::raster;

pub struct FillTool {
    /// 許容値(SPEC §4: 0–255、デフォルト 0)。オプションバーのスライダーから
    /// 配線する。
    pub tolerance: u8,
}

impl FillTool {
    pub fn new() -> Self {
        Self { tolerance: 0 }
    }
}

impl Default for FillTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FillTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        let ToolEvent::Down { img, button, .. } = ev else {
            return;
        };
        if !matches!(button, PointerButton::Primary | PointerButton::Secondary) {
            return;
        }
        let x = img.x.floor() as i32;
        let y = img.y.floor() as i32;
        let Some(start_color) = ctx.doc.get_pixel(x, y) else {
            return;
        };
        let color = color_bytes_for(ctx, button, false);
        if start_color == color {
            // SPEC §5(raster.rs): 開始色と塗色が同一なら no-op。ここで
            // 早期に抜けることで、無駄な undo 単位(履歴の空エントリ)を
            // 積まないようにする。
            return;
        }

        ctx.history.begin_stroke(ctx.doc.active);
        // raster::flood_fill に「これから塗る 1 スパン」の通知コールバックを
        // 渡し、undo 用の CoW タイル退避(History::ensure_tiles_saved_buf)を
        // 書き込み直前にその場で行う。こうすることで、あらかじめ全域を
        // 読み取り専用でもう一度スキャンし直す(旧実装、4000×4000 全面塗り
        // で 1 クリックあたり約 2 倍のコストになっていた、raster.rs 冒頭の
        // コメント参照)必要がなくなる。raster.rs は引き続き History を
        // 一切知らない(コールバック注入のみ)。v2: raster.rs はレイヤーも
        // 知らないため `Surface` を経由する(ARCHITECTURE.md §14.1)。
        let width = ctx.doc.width;
        let height = ctx.doc.height;
        let history = &mut *ctx.history;
        let touched = {
            let mut surf = ctx.doc.active_surface_mut();
            raster::flood_fill(&mut surf, x, y, color, self.tolerance, |s, rect| {
                history.ensure_tiles_saved_buf(width, height, s.as_slice(), rect);
            })
        };
        ctx.doc.mark_dirty(touched);
        ctx.history.commit_stroke(ctx.doc);

        let used = if button == PointerButton::Secondary {
            ctx.secondary
        } else {
            ctx.primary
        };
        ctx.used_colors.push(used);
    }

    fn draw_preview(
        &self,
        _painter: &egui::Painter,
        _view: &CanvasView,
        _primary: Color32,
        _secondary: Color32,
        _brush_size: f32,
    ) {
        // クリックで即座に確定するツールなのでプレビューはない。
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}
