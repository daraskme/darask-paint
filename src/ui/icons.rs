//! ツールアイコンのベクター描画(SPEC §15、ARCHITECTURE.md §14.4)。
//!
//! 画像アセット・絵文字フォントは使わず、`egui::Painter` の図形プリミティブ
//! だけでツール分のアイコンを組み立てる(画像アセットの追加禁止、CLAUDE.md
//! 鉄則)。v1 の 9 ツールに加え、v3 §18(ARCHITECTURE.md §15.2)で移動・
//! ズームを追加した。`rect` は正方形前提で、内部はすべて `rect` に対する相対座標
//! (0.0..1.0)から組み立てる(ARCHITECTURE.md §14.4)ので、ボタンサイズが
//! 変わっても比率を保ったまま描画できる。色は呼び出し側(`toolbar.rs`)が
//! 選択状態に応じて渡す 1 色のみを使う(egui のテキスト色/アクセント色に
//! 追従、SPEC §15)。
//!
//! 幾何計算のうち `quad_bezier`/`rounded_polygon` は `egui::Painter` に依存
//! しない純関数なのでユニットテストできる(ARCHITECTURE.md の
//! 「純関数(テスト必須)と描画/入力を分離する」方針、`color_wheel.rs` と同じ
//! 考え方)。

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

use crate::tools::ToolKind;

/// SPEC §15: 「線幅 ~1.5pt」。既定のアイコン外接矩形(20×20 目安)に対する
/// 比率として持ち、`rect` の実サイズに合わせてスケールする。
const STROKE_WIDTH_FRAC: f32 = 1.5 / 20.0;

fn stroke_width(rect: Rect) -> f32 {
    (rect.width().min(rect.height()) * STROKE_WIDTH_FRAC).max(1.0)
}

fn line_stroke(rect: Rect, color: Color32) -> Stroke {
    Stroke::new(stroke_width(rect), color)
}

/// `rect` に対する相対座標(0.0..1.0)から画面座標を作る。
fn p(rect: Rect, fx: f32, fy: f32) -> Pos2 {
    pos2(
        rect.min.x + fx * rect.width(),
        rect.min.y + fy * rect.height(),
    )
}

/// 2 次ベジェ曲線上の点(`rounded_polygon` が角の丸めに使う純関数)。
fn quad_bezier(p0: Pos2, control: Pos2, p1: Pos2, t: f32) -> Pos2 {
    let mt = 1.0 - t;
    pos2(
        mt * mt * p0.x + 2.0 * mt * t * control.x + t * t * p1.x,
        mt * mt * p0.y + 2.0 * mt * t * control.y + t * t * p1.y,
    )
}

/// 多角形 `vertices`(閉路)の各頂点を、半径 `radius` で丸めた点列を返す
/// (各頂点を 2 次ベジェで「面取り」する古典的な手法)。`segments` は角 1 個
/// あたりの分割数。消しゴムアイコン(SPEC §15: 「傾いた角丸矩形」)に使う。
fn rounded_polygon(vertices: &[Pos2], radius: f32, segments: usize) -> Vec<Pos2> {
    let n = vertices.len();
    if n < 3 {
        return vertices.to_vec();
    }
    let mut out = Vec::with_capacity(n * (segments + 1));
    for i in 0..n {
        let prev = vertices[(i + n - 1) % n];
        let cur = vertices[i];
        let next = vertices[(i + 1) % n];
        let to_prev = prev - cur;
        let to_next = next - cur;
        let len_prev = to_prev.length();
        let len_next = to_next.length();
        if len_prev <= f32::EPSILON || len_next <= f32::EPSILON {
            out.push(cur);
            continue;
        }
        let r = radius.min(len_prev * 0.5).min(len_next * 0.5);
        let start = cur + to_prev / len_prev * r;
        let end = cur + to_next / len_next * r;
        for s in 0..=segments {
            let t = s as f32 / segments as f32;
            out.push(quad_bezier(start, cur, end, t));
        }
    }
    out
}

/// 塗り無しの矢じり(三角形)を `tip` に、`dir` 方向を向けて描く
/// (手のひらアイコンの十字矢印が使う)。
fn arrow_head(painter: &Painter, tip: Pos2, dir: Vec2, size: f32, color: Color32) {
    let perp = vec2(-dir.y, dir.x);
    let base = tip - dir * size;
    let a = base + perp * size * 0.55;
    let b = base - perp * size * 0.55;
    painter.add(Shape::convex_polygon(vec![tip, a, b], color, Stroke::NONE));
}

/// SPEC §15 のツールアイコンを `rect`(正方形前提)の中に描く。
pub fn paint_tool_icon(kind: ToolKind, painter: &Painter, rect: Rect, color: Color32) {
    match kind {
        ToolKind::Pen => pencil_icon(painter, rect, color),
        ToolKind::Eraser => eraser_icon(painter, rect, color),
        ToolKind::Line => line_icon(painter, rect, color),
        ToolKind::Rect => rect_icon(painter, rect, color),
        ToolKind::Ellipse => ellipse_icon(painter, rect, color),
        ToolKind::Fill => fill_icon(painter, rect, color),
        ToolKind::Picker => picker_icon(painter, rect, color),
        ToolKind::Select => select_icon(painter, rect, color),
        ToolKind::Pan => pan_icon(painter, rect, color),
        ToolKind::Move => move_icon(painter, rect, color),
        ToolKind::Zoom => zoom_icon(painter, rect, color),
        ToolKind::Text => text_icon(painter, rect, color),
    }
}

/// ペン = 鉛筆(SPEC §15)。胴体+先端の五角形の輪郭に、先端付近の削り目を
/// 表す 1 本の横線を足す。
fn pencil_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tail = p(rect, 0.18, 0.86);
    let tip = p(rect, 0.86, 0.16);
    let dir = (tip - tail).normalized();
    let perp = vec2(-dir.y, dir.x);
    let half_w = rect.width() * 0.075;
    let taper = tail + (tip - tail) * 0.78;

    let a = tail + perp * half_w;
    let b = taper + perp * half_w;
    let c = tip;
    let d = taper - perp * half_w;
    let e = tail - perp * half_w;
    painter.add(Shape::closed_line(vec![a, b, c, d, e], st));
    // 削り目(芯と木部の境界)。
    painter.line_segment([b, d], st);
}

/// 消しゴム = 傾いた角丸矩形(SPEC §15)。二色消しゴムを思わせる分割線を
/// 1 本添える。
fn eraser_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.52, 0.56);
    let half = vec2(rect.width() * 0.28, rect.height() * 0.17);
    let angle: f32 = -0.55;
    let (s, c) = angle.sin_cos();
    let rotate = |v: Vec2| vec2(v.x * c - v.y * s, v.x * s + v.y * c);

    let corners: Vec<Pos2> = [
        vec2(-half.x, -half.y),
        vec2(half.x, -half.y),
        vec2(half.x, half.y),
        vec2(-half.x, half.y),
    ]
    .into_iter()
    .map(|v| center + rotate(v))
    .collect();

    let corner_radius = half.x.min(half.y) * 0.4;
    let outline = rounded_polygon(&corners, corner_radius, 5);
    painter.add(Shape::closed_line(outline, st));

    // 上辺・下辺を 38% の位置で結ぶ仕切り線(色境界)。
    let t = 0.38;
    let top = corners[0] + (corners[1] - corners[0]) * t;
    let bottom = corners[3] + (corners[2] - corners[3]) * t;
    painter.line_segment([top, bottom], st);
}

/// 直線 = 斜め線(SPEC §15)。
fn line_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    painter.line_segment([p(rect, 0.18, 0.82), p(rect, 0.82, 0.18)], st);
}

/// 矩形 = 矩形枠(SPEC §15)。
fn rect_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let r = Rect::from_min_max(p(rect, 0.20, 0.28), p(rect, 0.80, 0.76));
    painter.rect_stroke(r, 2.0, st, egui::StrokeKind::Middle);
}

/// 楕円 = 楕円枠(SPEC §15)。
fn ellipse_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.52);
    let radius = vec2(rect.width() * 0.30, rect.height() * 0.24);
    painter.add(Shape::ellipse_stroke(center, radius, st));
}

/// 塗りつぶし = バケツ+滴(SPEC §15)。バケツは取っ手付きの台形の輪郭、
/// 滴は塗りつぶした円で表す。
fn fill_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tl = p(rect, 0.14, 0.50);
    let tr = p(rect, 0.60, 0.50);
    let br = p(rect, 0.52, 0.82);
    let bl = p(rect, 0.22, 0.82);
    painter.add(Shape::closed_line(vec![tl, tr, br, bl], st));

    // 取っ手(2 次ベジェで弧を近似)。
    let handle_control = p(rect, 0.37, 0.32);
    let handle: Vec<Pos2> = (0..=8)
        .map(|i| quad_bezier(tl, handle_control, tr, i as f32 / 8.0))
        .collect();
    painter.add(Shape::line(handle, st));

    // 滴。
    painter.circle_filled(p(rect, 0.78, 0.32), rect.width() * 0.10, color);
}

/// スポイト = 斜めのスポイト(SPEC §15)。軸+球部+採取した色の滴。
fn picker_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tip = p(rect, 0.20, 0.88);
    let shaft_top = p(rect, 0.62, 0.46);
    painter.line_segment([tip, shaft_top], st);
    painter.circle_stroke(p(rect, 0.70, 0.36), rect.width() * 0.14, st);
    painter.circle_filled(p(rect, 0.17, 0.90), rect.width() * 0.045, color);
}

/// 選択 = 破線矩形(SPEC §15)。
fn select_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tl = p(rect, 0.16, 0.20);
    let tr = p(rect, 0.84, 0.20);
    let br = p(rect, 0.84, 0.80);
    let bl = p(rect, 0.16, 0.80);
    let path = [tl, tr, br, bl, tl];
    let dash = rect.width() * 0.10;
    let gap = rect.width() * 0.07;
    painter.extend(Shape::dashed_line(&path, st, dash, gap));
}

/// 手のひら = 十字矢印(SPEC §15、「手」の代替表現)。
fn pan_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let up = p(rect, 0.5, 0.14);
    let down = p(rect, 0.5, 0.86);
    let left = p(rect, 0.14, 0.5);
    let right = p(rect, 0.86, 0.5);
    painter.line_segment([up, down], st);
    painter.line_segment([left, right], st);

    let head = rect.width() * 0.10;
    arrow_head(painter, up, vec2(0.0, -1.0), head, color);
    arrow_head(painter, down, vec2(0.0, 1.0), head, color);
    arrow_head(painter, left, vec2(-1.0, 0.0), head, color);
    arrow_head(painter, right, vec2(1.0, 0.0), head, color);
}

/// 移動 = 矢印カーソル(v3 §18)。手のひらの十字矢印とは異なる意匠にして
/// 一目で区別できるようにする(古典的な「矢じり」カーソルの輪郭、4 頂点)。
fn move_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tip = p(rect, 0.18, 0.14);
    let tail = p(rect, 0.50, 0.86);
    let notch = p(rect, 0.58, 0.58);
    let barb = p(rect, 0.86, 0.46);
    painter.add(Shape::closed_line(vec![tip, tail, notch, barb], st));
}

/// ズーム = 虫眼鏡(v3 §18: 「カーソルは虫眼鏡」と同じ意匠)。内側に
/// 「+」を添えて拡大鏡であることを示す。
fn zoom_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.42, 0.42);
    let radius = rect.width() * 0.24;
    painter.circle_stroke(center, radius, st);
    let inner = radius * 0.5;
    painter.line_segment([center - vec2(inner, 0.0), center + vec2(inner, 0.0)], st);
    painter.line_segment([center - vec2(0.0, inner), center + vec2(0.0, inner)], st);
    let dir = vec2(1.0, 1.0).normalized();
    painter.line_segment([center + dir * radius, p(rect, 0.86, 0.86)], st);
}

/// テキスト = 「T」字(v3 §19)。横棒+縦棒の 2 本線だけの最小意匠。他の
/// アイコンと同じ 1.5pt 線幅・単色で、フォント/絵文字は使わない(SPEC §15
/// の「画像アセット・絵文字フォントは使わない」を v3 追加ツールにも適用)。
fn text_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    painter.line_segment([p(rect, 0.20, 0.22), p(rect, 0.80, 0.22)], st);
    painter.line_segment([p(rect, 0.5, 0.22), p(rect, 0.5, 0.82)], st);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_bezier_endpoints_match_control_points() {
        let p0 = pos2(0.0, 0.0);
        let c = pos2(5.0, 10.0);
        let p1 = pos2(10.0, 0.0);
        assert_eq!(quad_bezier(p0, c, p1, 0.0), p0);
        assert_eq!(quad_bezier(p0, c, p1, 1.0), p1);
    }

    #[test]
    fn quad_bezier_midpoint_is_between_control_and_endpoints() {
        let p0 = pos2(0.0, 0.0);
        let c = pos2(10.0, 10.0);
        let p1 = pos2(20.0, 0.0);
        let mid = quad_bezier(p0, c, p1, 0.5);
        // t=0.5 のベジェ中点は (p0/4 + c/2 + p1/4)。
        assert!((mid.x - 10.0).abs() < 1e-4);
        assert!((mid.y - 5.0).abs() < 1e-4);
    }

    #[test]
    fn rounded_polygon_produces_the_expected_point_count() {
        let square = vec![
            pos2(0.0, 0.0),
            pos2(10.0, 0.0),
            pos2(10.0, 10.0),
            pos2(0.0, 10.0),
        ];
        let rounded = rounded_polygon(&square, 2.0, 4);
        assert_eq!(rounded.len(), square.len() * (4 + 1));
    }

    #[test]
    fn rounded_polygon_stays_within_the_original_bounding_box() {
        let square = vec![
            pos2(0.0, 0.0),
            pos2(10.0, 0.0),
            pos2(10.0, 10.0),
            pos2(0.0, 10.0),
        ];
        let rounded = rounded_polygon(&square, 3.0, 6);
        for pt in rounded {
            assert!((-1e-3..=10.0 + 1e-3).contains(&pt.x));
            assert!((-1e-3..=10.0 + 1e-3).contains(&pt.y));
        }
    }

    #[test]
    fn rounded_polygon_degenerate_input_does_not_panic() {
        assert_eq!(rounded_polygon(&[], 3.0, 4), Vec::<Pos2>::new());
        let two = vec![pos2(0.0, 0.0), pos2(1.0, 1.0)];
        assert_eq!(rounded_polygon(&two, 3.0, 4), two);
        // 同一点が連続しても(長さ 0 の辺)パニックしない。
        let degenerate = vec![pos2(0.0, 0.0), pos2(0.0, 0.0), pos2(5.0, 5.0)];
        let out = rounded_polygon(&degenerate, 1.0, 3);
        assert!(!out.is_empty());
    }

    #[test]
    fn stroke_width_scales_with_rect_size_and_has_a_floor() {
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 4.0));
        assert!(stroke_width(small) >= 1.0);
        let big = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0));
        assert!(stroke_width(big) > stroke_width(small));
    }

    #[test]
    fn p_maps_unit_square_to_rect_corners() {
        let rect = Rect::from_min_max(pos2(10.0, 20.0), pos2(30.0, 60.0));
        assert_eq!(p(rect, 0.0, 0.0), pos2(10.0, 20.0));
        assert_eq!(p(rect, 1.0, 1.0), pos2(30.0, 60.0));
        assert_eq!(p(rect, 0.5, 0.5), pos2(20.0, 40.0));
    }
}
