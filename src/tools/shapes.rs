//! 直線・矩形・楕円ツール(SPEC §4、ARCHITECTURE.md §5)。
//!
//! いずれもドラッグ中はドキュメントに触れず、`draw_preview` でプレビューだけ
//! 描く。離した(`ToolEvent::Up`)時点で `raster.rs` の対応する関数を 1 回
//! 呼んで確定する(1 undo 単位、ARCHITECTURE.md §6)。Shift を押しながらの
//! ドラッグは、直近の `Drag`/`Down` の modifiers を見てその場で拘束
//! (直線=45°単位、矩形/楕円=正方形/正円)し、確定時は最後に拘束済みの点を
//! そのまま使う(Up イベント自体は modifiers を持たないため)。

use eframe::egui::{self, Color32, PointerButton, Pos2};

use super::{brush_radius, color_bytes_for, color_to_straight_rgba, Tool, ToolCtx, ToolEvent};
use crate::canvas_view::CanvasView;
use crate::document::IRect;
use crate::raster;

/// 矩形/楕円の塗りモード(SPEC §4: 「モード: 枠線のみ / 塗りつぶし /
/// 両方(枠=プライマリ、中=セカンダリ)」)。直線には意味を持たない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeMode {
    Outline,
    Fill,
    Both,
}

impl ShapeMode {
    pub fn label(self) -> &'static str {
        match self {
            ShapeMode::Outline => "枠線のみ",
            ShapeMode::Fill => "塗りつぶし",
            ShapeMode::Both => "両方",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeKind {
    Line,
    Rect,
    Ellipse,
}

#[derive(Clone, Copy)]
struct DragState {
    start: Pos2,
    current: Pos2,
    button: PointerButton,
}

/// 直線・矩形・楕円で共通のドラッグ→確定ロジックを持つツール本体。
/// `ToolKind` 側で 3 つの独立したインスタンス(`line`/`rect`/`ellipse`)として
/// 持つ(SPEC の各ツールはショートカットもオプション別状態も独立している)。
pub struct ShapeTool {
    kind: ShapeKind,
    drag: Option<DragState>,
    /// 矩形/楕円のみ意味を持つ(SPEC §4)。デフォルトは「枠線のみ」。
    pub mode: ShapeMode,
}

impl ShapeTool {
    fn new(kind: ShapeKind) -> Self {
        Self {
            kind,
            drag: None,
            mode: ShapeMode::Outline,
        }
    }

    pub fn new_line() -> Self {
        Self::new(ShapeKind::Line)
    }

    pub fn new_rect() -> Self {
        Self::new(ShapeKind::Rect)
    }

    pub fn new_ellipse() -> Self {
        Self::new(ShapeKind::Ellipse)
    }

    /// Shift 拘束(SPEC §4: 直線は 45°単位、矩形は正方形、楕円は正円)。
    fn constrain(&self, start: Pos2, current: Pos2, shift: bool) -> Pos2 {
        if !shift {
            return current;
        }
        match self.kind {
            ShapeKind::Line => snap_angle_45(start, current),
            ShapeKind::Rect | ShapeKind::Ellipse => snap_square(start, current),
        }
    }

    fn commit(&mut self, ctx: &mut ToolCtx, drag: DragState) {
        let radius = brush_radius(ctx.brush_size);
        let thickness = radius * 2.0;
        let rect = (drag.start.x, drag.start.y, drag.current.x, drag.current.y);

        match self.kind {
            ShapeKind::Line => {
                let color = color_bytes_for(ctx, drag.button, false);
                let bounds = raster::segment_bounds(
                    (drag.start.x, drag.start.y),
                    (drag.current.x, drag.current.y),
                    radius,
                );
                ctx.history.begin_stroke(ctx.doc.active);
                ctx.history.ensure_tiles_saved(ctx.doc, bounds);
                let touched = {
                    let mut surface = ctx.doc.active_surface_mut(ctx.clip);
                    raster::stroke_segment(
                        &mut surface,
                        (drag.start.x, drag.start.y),
                        (drag.current.x, drag.current.y),
                        radius,
                        color,
                        false,
                    )
                };
                ctx.doc.mark_dirty(touched);
                // ARCHITECTURE.md §18.3 の対応表: 「直線」。
                ctx.history.commit_stroke(ctx.doc, "直線");
                ctx.used_colors.push(button_color(ctx, drag.button));
            }
            ShapeKind::Rect | ShapeKind::Ellipse => {
                let bounds = match self.kind {
                    ShapeKind::Rect => raster::rect_shape_bounds(rect, thickness),
                    ShapeKind::Ellipse => raster::ellipse_shape_bounds(rect, thickness),
                    ShapeKind::Line => unreachable!(),
                };
                ctx.history.begin_stroke(ctx.doc.active);
                ctx.history.ensure_tiles_saved(ctx.doc, bounds);

                let (outline, fill) = shape_colors(ctx, drag.button, self.mode);
                let mut touched: Option<IRect> = None;
                {
                    let mut surface = ctx.doc.active_surface_mut(ctx.clip);
                    if let Some(fill_color) = fill {
                        let t = match self.kind {
                            ShapeKind::Rect => raster::fill_rect(&mut surface, rect, fill_color),
                            ShapeKind::Ellipse => {
                                raster::fill_ellipse(&mut surface, rect, fill_color)
                            }
                            ShapeKind::Line => unreachable!(),
                        };
                        touched = Some(match touched {
                            Some(u) => u.union(&t),
                            None => t,
                        });
                    }
                    if let Some(outline_color) = outline {
                        let t = match self.kind {
                            ShapeKind::Rect => raster::draw_rect_outline(
                                &mut surface,
                                rect,
                                thickness,
                                outline_color,
                            ),
                            ShapeKind::Ellipse => raster::draw_ellipse_outline(
                                &mut surface,
                                rect,
                                thickness,
                                outline_color,
                            ),
                            ShapeKind::Line => unreachable!(),
                        };
                        touched = Some(match touched {
                            Some(u) => u.union(&t),
                            None => t,
                        });
                    }
                }
                if let Some(t) = touched {
                    ctx.doc.mark_dirty(t);
                }
                // ARCHITECTURE.md §18.3 の対応表: 「矩形」/「楕円」。
                let label = match self.kind {
                    ShapeKind::Rect => "矩形",
                    ShapeKind::Ellipse => "楕円",
                    ShapeKind::Line => unreachable!(),
                };
                ctx.history.commit_stroke(ctx.doc, label);

                match self.mode {
                    ShapeMode::Both => {
                        ctx.used_colors.push(ctx.secondary);
                        ctx.used_colors.push(ctx.primary);
                    }
                    ShapeMode::Outline | ShapeMode::Fill => {
                        ctx.used_colors.push(button_color(ctx, drag.button));
                    }
                }
            }
        }
    }
}

fn button_color(ctx: &ToolCtx, button: PointerButton) -> Color32 {
    if button == PointerButton::Secondary {
        ctx.secondary
    } else {
        ctx.primary
    }
}

/// 確定用の色解決。「両方」モードは常に 枠=プライマリ・中=セカンダリ
/// (SPEC §4)。それ以外(枠線のみ/塗りつぶし)はドラッグに使ったボタンの色
/// (左=プライマリ/右=セカンダリ、ペンと同じ慣習)を単色で使う。
fn shape_colors(
    ctx: &ToolCtx,
    button: PointerButton,
    mode: ShapeMode,
) -> (Option<[u8; 4]>, Option<[u8; 4]>) {
    match mode {
        ShapeMode::Outline => (Some(color_bytes_for(ctx, button, false)), None),
        ShapeMode::Fill => (None, Some(color_bytes_for(ctx, button, false))),
        ShapeMode::Both => (
            Some(color_to_straight_rgba(ctx.primary)),
            Some(color_to_straight_rgba(ctx.secondary)),
        ),
    }
}

/// プレビュー用の色解決(`shape_colors` と同じ規則、`Color32` のまま)。
fn preview_colors(
    kind: ShapeKind,
    mode: ShapeMode,
    button: PointerButton,
    primary: Color32,
    secondary: Color32,
) -> (Option<Color32>, Option<Color32>) {
    let single = if button == PointerButton::Secondary {
        secondary
    } else {
        primary
    };
    match kind {
        ShapeKind::Line => (Some(single), None),
        ShapeKind::Rect | ShapeKind::Ellipse => match mode {
            ShapeMode::Outline => (Some(single), None),
            ShapeMode::Fill => (None, Some(single)),
            ShapeMode::Both => (Some(primary), Some(secondary)),
        },
    }
}

/// ドラッグ中プレビューの線の太さ(スクリーン論理ポイント単位)。
///
/// M4 で発見・修正したバグ: 以前は `radius * 2.0 * zoom` を `egui::Stroke`
/// (論理ポイント単位)へそのまま渡していたため、`ppp`(画像px→物理pxの倍率)
/// で 1 回しか割っていない画像px→論理ポイント換算になっていなかった
/// (ARCHITECTURE.md §2 の `img_to_screen` は `zoom / ppp` を使う)。高 DPI
/// (`ppp > 1.0`)ではドラッグ中のプレビュー線が確定後の太さの `ppp` 倍で
/// 表示されてしまっていた。
fn preview_thickness_screen(radius: f32, zoom: f32, ppp: f32) -> f32 {
    (radius * 2.0 * zoom / ppp).max(1.0)
}

fn snap_angle_45(start: Pos2, current: Pos2) -> Pos2 {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    if dx == 0.0 && dy == 0.0 {
        return current;
    }
    let dist = (dx * dx + dy * dy).sqrt();
    let angle = dy.atan2(dx);
    let step = std::f32::consts::FRAC_PI_4;
    let snapped = (angle / step).round() * step;
    egui::pos2(
        start.x + dist * snapped.cos(),
        start.y + dist * snapped.sin(),
    )
}

/// v4 §22: 楕円選択・矩形選択(マリキー)の Shift ドラッグ拘束(正方形/正円)
/// でも同じ計算を使うため `pub(crate)`(`app.rs::select_drag_move` 参照)。
pub(crate) fn snap_square(start: Pos2, current: Pos2) -> Pos2 {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    let m = dx.abs().max(dy.abs());
    let sx = if dx < 0.0 { -m } else { m };
    let sy = if dy < 0.0 { -m } else { m };
    egui::pos2(start.x + sx, start.y + sy)
}

impl Tool for ShapeTool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx) {
        match ev {
            ToolEvent::Down { img, button, .. } => {
                if !matches!(button, PointerButton::Primary | PointerButton::Secondary) {
                    return;
                }
                self.drag = Some(DragState {
                    start: img,
                    current: img,
                    button,
                });
            }
            ToolEvent::Drag { img, button, mods } => {
                let Some(start) = self
                    .drag
                    .as_ref()
                    .and_then(|d| (d.button == button).then_some(d.start))
                else {
                    return;
                };
                let current = self.constrain(start, img, mods.shift);
                if let Some(drag) = self.drag.as_mut() {
                    drag.current = current;
                }
            }
            ToolEvent::Up { button, .. } => {
                let Some(drag) = self.drag.take() else {
                    return;
                };
                if drag.button != button {
                    // 別ボタンの Up: このドラッグは対象外。状態を捨てる
                    // (ボタンをまたいだドラッグは SPEC 上想定されていない)。
                    return;
                }
                self.commit(ctx, drag);
            }
            ToolEvent::Hover { .. } => {}
        }
    }

    fn cancel(&mut self, ctx: &mut ToolCtx) {
        // ツール切替時にドラッグ中の図形があれば、Up が来た場合と同様に
        // 直近のドラッグ位置で確定する(ARCHITECTURE.md §6 の「1 図形 =
        // 1 undo 単位」を、ツール切替という中断経路でも守るため)。
        if let Some(drag) = self.drag.take() {
            self.commit(ctx, drag);
        }
    }

    fn draw_preview(
        &self,
        painter: &egui::Painter,
        view: &CanvasView,
        primary: Color32,
        secondary: Color32,
        brush_size: f32,
    ) {
        let Some(drag) = &self.drag else {
            return;
        };
        let radius = brush_radius(brush_size);
        let thickness_screen = preview_thickness_screen(radius, view.zoom, view.ppp());
        let a = view.img_to_screen_pos(drag.start);
        let b = view.img_to_screen_pos(drag.current);
        let (outline, fill) = preview_colors(self.kind, self.mode, drag.button, primary, secondary);

        match self.kind {
            ShapeKind::Line => {
                if let Some(c) = outline {
                    painter.line_segment([a, b], egui::Stroke::new(thickness_screen, c));
                }
            }
            ShapeKind::Rect => {
                let rect = egui::Rect::from_two_pos(a, b);
                if let Some(c) = fill {
                    painter.rect_filled(rect, 0.0, c);
                }
                if let Some(c) = outline {
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(thickness_screen, c),
                        egui::StrokeKind::Middle,
                    );
                }
            }
            ShapeKind::Ellipse => {
                let center = egui::pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                let radius_vec = egui::vec2((b.x - a.x).abs() / 2.0, (b.y - a.y).abs() / 2.0);
                if let Some(c) = fill {
                    painter.add(egui::Shape::ellipse_filled(center, radius_vec, c));
                }
                if let Some(c) = outline {
                    painter.add(egui::Shape::ellipse_stroke(
                        center,
                        radius_vec,
                        egui::Stroke::new(thickness_screen, c),
                    ));
                }
            }
        }
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_angle_45_snaps_near_horizontal_to_exactly_horizontal() {
        let start = Pos2::new(0.0, 0.0);
        let current = Pos2::new(10.0, 1.0); // ほぼ水平
        let snapped = snap_angle_45(start, current);
        assert!(
            snapped.y.abs() < 1e-3,
            "expected y snapped to 0, got {snapped:?}"
        );
    }

    #[test]
    fn snap_angle_45_snaps_45_degree_diagonal() {
        let start = Pos2::new(0.0, 0.0);
        let current = Pos2::new(10.0, 9.5); // ほぼ 45°
        let snapped = snap_angle_45(start, current);
        assert!(
            (snapped.x - snapped.y).abs() < 1e-2,
            "expected x == y, got {snapped:?}"
        );
    }

    #[test]
    fn snap_angle_45_preserves_distance() {
        let start = Pos2::new(5.0, 5.0);
        let current = Pos2::new(20.0, 13.0);
        let dist_before = (current - start).length();
        let snapped = snap_angle_45(start, current);
        let dist_after = (snapped - start).length();
        assert!((dist_before - dist_after).abs() < 1e-2);
    }

    #[test]
    fn snap_square_makes_equal_width_height_preserving_sign() {
        let start = Pos2::new(10.0, 10.0);
        let current = Pos2::new(30.0, 15.0); // dx=20, dy=5
        let snapped = snap_square(start, current);
        let dx = snapped.x - start.x;
        let dy = snapped.y - start.y;
        assert!((dx.abs() - dy.abs()).abs() < 1e-5);
        assert_eq!(dx.signum(), 1.0);
        assert_eq!(dy.signum(), 1.0);
        assert!((dx.abs() - 20.0).abs() < 1e-5);
    }

    #[test]
    fn snap_square_handles_negative_directions() {
        let start = Pos2::new(10.0, 10.0);
        let current = Pos2::new(2.0, 4.0); // dx=-8, dy=-6
        let snapped = snap_square(start, current);
        let dx = snapped.x - start.x;
        let dy = snapped.y - start.y;
        assert!((dx.abs() - dy.abs()).abs() < 1e-5);
        assert!(dx < 0.0 && dy < 0.0);
    }

    #[test]
    fn preview_thickness_screen_accounts_for_pixels_per_point() {
        // 高 DPI (ppp=2.0) では画面上の太さは画像px換算の半分になるはず
        // (確定後の見た目、img_to_screen と同じ zoom/ppp 換算に一致させる)。
        let t_lodpi = preview_thickness_screen(4.0, 1.0, 1.0);
        let t_hidpi = preview_thickness_screen(4.0, 1.0, 2.0);
        assert!((t_lodpi - 8.0).abs() < 1e-5);
        assert!((t_hidpi - 4.0).abs() < 1e-5);
    }

    #[test]
    fn preview_thickness_screen_has_a_minimum_of_one_point() {
        let t = preview_thickness_screen(0.1, 0.1, 4.0);
        assert_eq!(t, 1.0);
    }

    #[test]
    fn no_shift_leaves_point_unconstrained() {
        let tool = ShapeTool::new_line();
        let start = Pos2::new(0.0, 0.0);
        let current = Pos2::new(7.0, 3.0);
        assert_eq!(tool.constrain(start, current, false), current);
    }
}
