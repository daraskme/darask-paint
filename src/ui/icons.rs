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
        ToolKind::Gradient => gradient_icon(painter, rect, color),
        ToolKind::Picker => picker_icon(painter, rect, color),
        ToolKind::Select => select_icon(painter, rect, color),
        ToolKind::Pan => pan_icon(painter, rect, color),
        ToolKind::Move => move_icon(painter, rect, color),
        ToolKind::Zoom => zoom_icon(painter, rect, color),
        ToolKind::Text => text_icon(painter, rect, color),
        ToolKind::EllipseSelect => ellipse_select_icon(painter, rect, color),
        ToolKind::Lasso => lasso_icon(painter, rect, color),
        ToolKind::MagicWand => magic_wand_icon(painter, rect, color),
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

/// グラデーション = 濃淡の帯+ドラッグ方向を示す矢印(v4 §23)。塗りつぶし
/// (バケツ)とは対照的な、明確に「連続的な変化」を思わせる意匠にする。
fn gradient_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let box_rect = Rect::from_min_max(p(rect, 0.16, 0.24), p(rect, 0.84, 0.62));

    // 不透明→透明の帯(SPEC §23 の「グラデーション」の見た目そのもの)。
    const BANDS: i32 = 6;
    for i in 0..BANDS {
        let t0 = i as f32 / BANDS as f32;
        let t1 = (i + 1) as f32 / BANDS as f32;
        let alpha = (255.0 * (1.0 - i as f32 / (BANDS - 1) as f32))
            .round()
            .clamp(0.0, 255.0) as u8;
        let band_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
        let band_rect = Rect::from_min_max(
            pos2(box_rect.min.x + t0 * box_rect.width(), box_rect.min.y),
            pos2(box_rect.min.x + t1 * box_rect.width(), box_rect.max.y),
        );
        painter.rect_filled(band_rect, 0.0, band_color);
    }
    painter.rect_stroke(box_rect, 1.0, st, egui::StrokeKind::Middle);

    // ドラッグ方向を示す矢印(始点→終点、SPEC §23: 「ドラッグで始点→終点」)。
    let arrow_from = p(rect, 0.20, 0.82);
    let arrow_to = p(rect, 0.80, 0.82);
    painter.line_segment([arrow_from, arrow_to], st);
    arrow_head(
        painter,
        arrow_to,
        (arrow_to - arrow_from).normalized(),
        rect.width() * 0.09,
        color,
    );
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

/// 楕円選択 = 破線楕円(v4 §22)。`select_icon`(矩形の破線枠)と対になる
/// 意匠にし、Shift+M で巡回する 2 つのツールが一目で見分けられるようにする。
fn ellipse_select_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.5);
    let radius = vec2(rect.width() * 0.34, rect.height() * 0.30);
    let steps = 32;
    let path: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32 * std::f32::consts::TAU;
            center + vec2(radius.x * t.cos(), radius.y * t.sin())
        })
        .collect();
    let dash = rect.width() * 0.09;
    let gap = rect.width() * 0.06;
    painter.extend(Shape::dashed_line(&path, st, dash, gap));
}

/// なげなわ = 不定形の破線ループ+持ち手(v4 §22)。矩形/楕円の規則的な
/// 破線枠とは違う「手描きの投げ縄」の輪郭であることを一目で示す。
fn lasso_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let loop_points = [
        p(rect, 0.32, 0.30),
        p(rect, 0.54, 0.16),
        p(rect, 0.78, 0.28),
        p(rect, 0.84, 0.50),
        p(rect, 0.64, 0.68),
        p(rect, 0.40, 0.64),
        p(rect, 0.22, 0.48),
        p(rect, 0.26, 0.34),
    ];
    let dash = rect.width() * 0.08;
    let gap = rect.width() * 0.055;
    let mut path = loop_points.to_vec();
    path.push(loop_points[0]);
    painter.extend(Shape::dashed_line(&path, st, dash, gap));
    // ループの結び目から伸びる持ち手(実線)。
    painter.line_segment([loop_points[0], p(rect, 0.14, 0.86)], st);
}

/// 自動選択 = 魔法の杖(先端のきらめき、v4 §22)。バケツ(塗りつぶし)と
/// 判定基準は同じだが、道具としての見た目は明確に区別する。
fn magic_wand_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tip = p(rect, 0.76, 0.24);
    let handle = p(rect, 0.24, 0.82);
    painter.line_segment([handle, tip], st);
    sparkle_cross(painter, tip, rect.width() * 0.13, st);
    sparkle_cross(painter, p(rect, 0.34, 0.30), rect.width() * 0.06, st);
}

/// `magic_wand_icon` のきらめき(大きさ違いの十字を 2 個描くだけの最小意匠)。
fn sparkle_cross(painter: &Painter, center: Pos2, size: f32, stroke: Stroke) {
    painter.line_segment([center - vec2(size, 0.0), center + vec2(size, 0.0)], stroke);
    painter.line_segment([center - vec2(0.0, size), center + vec2(0.0, size)], stroke);
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

/// v6 §33/§34(ARCHITECTURE.md §18.1/§18.2): 設定(歯車)。`ToolKind` の
/// 仲間ではない専用ボタン(`ui/toolbar.rs` の設定ボタン)のためのアイコンで、
/// `paint_tool_icon` の match とは独立させる(設定はツールではなく、
/// 切り替え可能な状態を持たないため)。外周リング+放射状の歯+内側の穴、
/// という古典的な歯車の意匠。
pub fn paint_settings_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.5);
    let outer = rect.width() * 0.32;
    let ring = outer * 0.62;
    let hole = outer * 0.28;

    const TEETH: usize = 8;
    for i in 0..TEETH {
        let angle = i as f32 / TEETH as f32 * std::f32::consts::TAU;
        let dir = vec2(angle.cos(), angle.sin());
        painter.line_segment([center + dir * ring, center + dir * outer], st);
    }
    painter.circle_stroke(center, ring, st);
    painter.circle_stroke(center, hole, st);
}

// -- v6 §33(ARCHITECTURE.md §18.1): メニューバーの全展開アイコン -------------
//
// 従来のドロップダウンメニュー各項目を常時表示のアイコンボタンに置き換える
// (`ui/menu.rs` 参照)。`paint_tool_icon`/`paint_settings_icon` と同じ流儀
// (線幅 ~1.5pt・単色・`rect` に対する相対座標)で、`ToolKind` を持たない
// ボタン専用の意匠を 1 関数 1 アイコンで用意する。以下の小さなヘルパーは
// 複数のアイコンで繰り返す意匠(十字/バツ/矢じり/層の積み重ね)をまとめる。

/// `+` マーク(新規・レイヤー追加が使う)。`sparkle_cross` と同じ十字だが、
/// 意味上の別名として置く(呼び出し側の意図を読みやすくするため)。
fn plus_mark(painter: &Painter, center: Pos2, size: f32, stroke: Stroke) {
    sparkle_cross(painter, center, size, stroke);
}

/// `×` マーク(45°回転した十字。削除・閉じる系アイコンが使う)。
fn cross_mark(painter: &Painter, center: Pos2, size: f32, stroke: Stroke) {
    painter.line_segment(
        [center - vec2(size, size), center + vec2(size, size)],
        stroke,
    );
    painter.line_segment(
        [center - vec2(size, -size), center + vec2(size, -size)],
        stroke,
    );
}

/// 矩形の層を `count` 枚、右下へ少しずつずらして重ねる(レイヤー系アイコンの
/// 共通意匠)。返り値は最前面(最後に描いた)矩形(マークの位置決めに使う)。
fn stacked_rects(painter: &Painter, rect: Rect, color: Color32, count: usize) -> Rect {
    let st = line_stroke(rect, color);
    let step = vec2(rect.width() * 0.11, rect.height() * 0.11);
    let size = vec2(rect.width() * 0.56, rect.height() * 0.40);
    let base_min = p(rect, 0.16, 0.18);
    let mut last = Rect::from_min_size(base_min, size);
    for i in 0..count {
        let r = Rect::from_min_size(base_min + step * i as f32, size);
        painter.rect_stroke(r, 1.5, st, egui::StrokeKind::Middle);
        last = r;
    }
    last
}

/// ファイル = 新規: ページの輪郭(右上の角を折る)+中央の「+」(SPEC §33)。
pub fn paint_new_document_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let fold = rect.width() * 0.16;
    let tl = p(rect, 0.26, 0.14);
    let tr = p(rect, 0.74, 0.14);
    let fold_top = pos2(tr.x - fold, tr.y);
    let fold_bottom = pos2(tr.x, tr.y + fold);
    let br = p(rect, 0.74, 0.86);
    let bl = p(rect, 0.26, 0.86);
    painter.add(Shape::closed_line(
        vec![tl, fold_top, fold_bottom, br, bl],
        st,
    ));
    painter.line_segment([fold_top, fold_bottom], st);
    plus_mark(painter, p(rect, 0.5, 0.58), rect.width() * 0.11, st);
}

/// ファイル = 開く: 取っ手付きフォルダ(SPEC §33)。
pub fn paint_open_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tab_tl = p(rect, 0.16, 0.24);
    let tab_mid = p(rect, 0.30, 0.24);
    let tab_step = p(rect, 0.38, 0.34);
    let tr = p(rect, 0.84, 0.34);
    let br = p(rect, 0.80, 0.78);
    let bl = p(rect, 0.20, 0.78);
    let bl_up = p(rect, 0.16, 0.34);
    painter.add(Shape::closed_line(
        vec![tab_tl, tab_mid, tab_step, tr, br, bl, bl_up],
        st,
    ));
}

/// ファイル = 最近使ったファイル: 時計(履歴を示す、SPEC §33 の唯一の例外
/// ボタン。クリックでポップアップを開く、`ui/menu.rs` 参照)。
pub fn paint_recent_files_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.52);
    let radius = rect.width() * 0.32;
    painter.circle_stroke(center, radius, st);
    painter.line_segment([center, center - vec2(0.0, radius * 0.62)], st);
    painter.line_segment([center, center + vec2(radius * 0.42, 0.0)], st);
}

/// ファイル = 上書き保存: フロッピーディスク(SPEC §33)。
pub fn paint_save_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let body = Rect::from_min_max(p(rect, 0.18, 0.16), p(rect, 0.82, 0.84));
    painter.rect_stroke(body, 1.5, st, egui::StrokeKind::Middle);
    // 書き込み禁止ノッチ(右上)。
    let notch = Rect::from_min_max(p(rect, 0.58, 0.16), p(rect, 0.72, 0.34));
    painter.rect_stroke(notch, 0.0, st, egui::StrokeKind::Middle);
    // ラベル領域(下部の横線)。
    let label = Rect::from_min_max(p(rect, 0.30, 0.58), p(rect, 0.70, 0.78));
    painter.rect_stroke(label, 0.0, st, egui::StrokeKind::Middle);
}

/// ファイル = 名前を付けて保存: フロッピーディスク+右上の小さな星印
/// (「別名」であることを示す、SPEC §33)。
pub fn paint_save_as_icon(painter: &Painter, rect: Rect, color: Color32) {
    paint_save_icon(painter, rect, color);
    let st = line_stroke(rect, color);
    let star = p(rect, 0.86, 0.20);
    let r = rect.width() * 0.07;
    for i in 0..3 {
        let angle = i as f32 / 3.0 * std::f32::consts::PI;
        let dir = vec2(angle.cos(), angle.sin());
        painter.line_segment([star - dir * r, star + dir * r], st);
    }
}

/// ファイル = タブを閉じる: タブ形状+「×」(SPEC §33)。
pub fn paint_close_tab_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tl = p(rect, 0.18, 0.30);
    let tl_up = p(rect, 0.22, 0.20);
    let tr_up = p(rect, 0.62, 0.20);
    let tr = p(rect, 0.66, 0.30);
    let r = p(rect, 0.82, 0.30);
    let br = p(rect, 0.82, 0.76);
    let bl = p(rect, 0.18, 0.76);
    painter.add(Shape::closed_line(
        vec![tl, tl_up, tr_up, tr, r, br, bl],
        st,
    ));
    cross_mark(painter, p(rect, 0.5, 0.54), rect.width() * 0.10, st);
}

/// ファイル = 終了: 戸口+外向き矢印(SPEC §33、古典的な「ログアウト」意匠)。
pub fn paint_exit_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tl = p(rect, 0.22, 0.18);
    let bl = p(rect, 0.22, 0.82);
    let tr = p(rect, 0.50, 0.18);
    let br = p(rect, 0.50, 0.82);
    painter.line_segment([tl, bl], st);
    painter.line_segment([tl, tr], st);
    painter.line_segment([bl, br], st);
    let arrow_from = p(rect, 0.40, 0.5);
    let arrow_to = p(rect, 0.84, 0.5);
    painter.line_segment([arrow_from, arrow_to], st);
    arrow_head(
        painter,
        arrow_to,
        vec2(1.0, 0.0),
        rect.width() * 0.11,
        color,
    );
}

/// 編集 = 元に戻す: 反時計回りの弧+矢じり(SPEC §33)。
pub fn paint_undo_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.56, 0.56);
    let radius = rect.width() * 0.28;
    let steps = 24;
    let start_angle = std::f32::consts::PI * 1.15;
    let end_angle = std::f32::consts::PI * 0.15;
    let path: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let angle = start_angle + (end_angle - start_angle) * t;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(path.clone(), st));
    let tip = path[0];
    let dir = (path[0] - path[1]).normalized();
    arrow_head(painter, tip, dir, rect.width() * 0.11, color);
}

/// 編集 = やり直し: `paint_undo_icon` を左右反転した意匠(SPEC §33)。
pub fn paint_redo_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.44, 0.56);
    let radius = rect.width() * 0.28;
    let steps = 24;
    let start_angle = std::f32::consts::PI * -0.15;
    let end_angle = std::f32::consts::PI * -1.15;
    let path: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let angle = start_angle + (end_angle - start_angle) * t;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(path.clone(), st));
    let tip = path[0];
    let dir = (path[0] - path[1]).normalized();
    arrow_head(painter, tip, dir, rect.width() * 0.11, color);
}

/// 編集 = 切り取り: はさみ(SPEC §33)。
pub fn paint_cut_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let apex = p(rect, 0.24, 0.20);
    let handle_a = p(rect, 0.78, 0.22);
    let handle_b = p(rect, 0.78, 0.80);
    painter.line_segment([apex, handle_a], st);
    painter.line_segment([apex, handle_b], st);
    let r = rect.width() * 0.07;
    painter.circle_stroke(handle_a, r, st);
    painter.circle_stroke(handle_b, r, st);
}

/// 編集 = コピー: 重なった 2 枚の矩形の輪郭(塗りつぶさず線だけを重ねる、
/// 一般的な「コピー」アイコンの意匠、SPEC §33)。
pub fn paint_copy_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let back = Rect::from_min_max(p(rect, 0.30, 0.16), p(rect, 0.78, 0.62));
    let front = Rect::from_min_max(p(rect, 0.20, 0.36), p(rect, 0.68, 0.82));
    painter.rect_stroke(back, 1.0, st, egui::StrokeKind::Middle);
    painter.rect_stroke(front, 1.0, st, egui::StrokeKind::Middle);
}

/// 編集 = 貼り付け: クリップボード(上部タブ+本体+中の横線)(SPEC §33)。
pub fn paint_paste_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let body = Rect::from_min_max(p(rect, 0.22, 0.24), p(rect, 0.78, 0.84));
    painter.rect_stroke(body, 1.5, st, egui::StrokeKind::Middle);
    let tab = Rect::from_min_max(p(rect, 0.40, 0.16), p(rect, 0.60, 0.28));
    painter.rect_stroke(tab, 1.0, st, egui::StrokeKind::Middle);
    for t in [0.42, 0.56, 0.70] {
        painter.line_segment([p(rect, 0.32, t), p(rect, 0.68, t)], st);
    }
}

/// 編集 = 削除: ゴミ箱(SPEC §33)。
pub fn paint_delete_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let tl = p(rect, 0.26, 0.30);
    let tr = p(rect, 0.74, 0.30);
    let br = p(rect, 0.68, 0.84);
    let bl = p(rect, 0.32, 0.84);
    painter.add(Shape::closed_line(vec![tl, tr, br, bl], st));
    painter.line_segment([p(rect, 0.18, 0.30), p(rect, 0.82, 0.30)], st);
    let lid = Rect::from_min_max(p(rect, 0.40, 0.16), p(rect, 0.60, 0.30));
    painter.rect_stroke(lid, 0.0, st, egui::StrokeKind::Middle);
    for t in [0.42, 0.50, 0.58] {
        painter.line_segment([p(rect, t, 0.40), p(rect, t, 0.74)], st);
    }
}

/// 編集 = すべて選択: 四隅の角括弧(SPEC §33。選択ツールの破線枠とは違う
/// 意匠にして「メニュー操作」であることを区別する)。
pub fn paint_select_all_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    corner_brackets(painter, rect, st);
}

/// `paint_select_all_icon`/`paint_deselect_icon`/`paint_crop_icon` が共有する
/// 四隅の L 字括弧。
fn corner_brackets(painter: &Painter, rect: Rect, st: Stroke) {
    let arm = rect.width() * 0.16;
    let corners = [
        (p(rect, 0.18, 0.22), vec2(1.0, 0.0), vec2(0.0, 1.0)),
        (p(rect, 0.82, 0.22), vec2(-1.0, 0.0), vec2(0.0, 1.0)),
        (p(rect, 0.18, 0.78), vec2(1.0, 0.0), vec2(0.0, -1.0)),
        (p(rect, 0.82, 0.78), vec2(-1.0, 0.0), vec2(0.0, -1.0)),
    ];
    for (corner, dx, dy) in corners {
        painter.line_segment([corner, corner + dx * arm], st);
        painter.line_segment([corner, corner + dy * arm], st);
    }
}

/// 編集 = 選択解除: 四隅の角括弧+斜線(SPEC §33。「選択が無くなる」ことを
/// 表す打ち消し線)。
pub fn paint_deselect_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    corner_brackets(painter, rect, st);
    painter.line_segment([p(rect, 0.20, 0.80), p(rect, 0.80, 0.20)], st);
}

/// 編集 = 自由変形: 矩形+四隅のハンドル(SPEC §16 のスケールハンドルと同じ
/// 意匠、SPEC §33)。
pub fn paint_free_transform_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let r = Rect::from_min_max(p(rect, 0.24, 0.24), p(rect, 0.76, 0.76));
    painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Middle);
    let handle = rect.width() * 0.06;
    for corner in [
        r.left_top(),
        r.right_top(),
        r.left_bottom(),
        r.right_bottom(),
    ] {
        let h = Rect::from_center_size(corner, vec2(handle, handle));
        painter.rect_filled(h, 0.0, color);
    }
}

/// 画像 = 画像サイズ変更: 矩形+右下の斜め両矢印(SPEC §33)。
pub fn paint_image_resize_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let r = Rect::from_min_max(p(rect, 0.18, 0.18), p(rect, 0.68, 0.68));
    painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Middle);
    let a = p(rect, 0.58, 0.58);
    let b = p(rect, 0.86, 0.86);
    painter.line_segment([a, b], st);
    let dir = (b - a).normalized();
    let head = rect.width() * 0.09;
    arrow_head(painter, b, dir, head, color);
    arrow_head(painter, a, -dir, head, color);
}

/// 画像 = キャンバスサイズ変更: 大きな破線枠(キャンバス)+左上基準の小さな
/// 実線矩形(既存画像、SPEC §33)。
pub fn paint_canvas_resize_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let outer = [
        p(rect, 0.16, 0.16),
        p(rect, 0.84, 0.16),
        p(rect, 0.84, 0.84),
        p(rect, 0.16, 0.84),
        p(rect, 0.16, 0.16),
    ];
    let dash = rect.width() * 0.06;
    let gap = rect.width() * 0.05;
    painter.extend(Shape::dashed_line(&outer, st, dash, gap));
    let inner = Rect::from_min_max(p(rect, 0.16, 0.16), p(rect, 0.54, 0.50));
    painter.rect_stroke(inner, 0.0, st, egui::StrokeKind::Middle);
}

/// 画像 = 選択範囲でトリミング: クロップツールの定番意匠(向かい合う 2 組の
/// L 字括弧、SPEC §33)。
pub fn paint_crop_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let arm = rect.width() * 0.30;
    // 左上の角。
    let tl = p(rect, 0.24, 0.18);
    painter.line_segment([tl, tl + vec2(0.0, arm)], st);
    painter.line_segment([tl, tl + vec2(arm, 0.0)], st);
    // 右下の角。
    let br = p(rect, 0.76, 0.82);
    painter.line_segment([br, br - vec2(0.0, arm)], st);
    painter.line_segment([br, br - vec2(arm, 0.0)], st);
}

/// 画像 = 選択範囲を新規タブに複製: 破線の矩形(選択)→矢印→タブ形状
/// (SPEC §31/§33)。
pub fn paint_duplicate_to_tab_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let sel = [
        p(rect, 0.08, 0.30),
        p(rect, 0.38, 0.30),
        p(rect, 0.38, 0.70),
        p(rect, 0.08, 0.70),
        p(rect, 0.08, 0.30),
    ];
    painter.extend(Shape::dashed_line(
        &sel,
        st,
        rect.width() * 0.045,
        rect.width() * 0.035,
    ));
    let arrow_from = p(rect, 0.44, 0.5);
    let arrow_to = p(rect, 0.62, 0.5);
    painter.line_segment([arrow_from, arrow_to], st);
    arrow_head(
        painter,
        arrow_to,
        vec2(1.0, 0.0),
        rect.width() * 0.07,
        color,
    );
    let tab = Rect::from_min_max(p(rect, 0.68, 0.30), p(rect, 0.94, 0.70));
    painter.rect_stroke(tab, 1.0, st, egui::StrokeKind::Middle);
}

/// 画像 = 左右反転: 中央の破線+左右を向く矢じり(SPEC §33)。
pub fn paint_flip_horizontal_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let mid = [p(rect, 0.5, 0.16), p(rect, 0.5, 0.84)];
    painter.extend(Shape::dashed_line(
        &mid,
        st,
        rect.width() * 0.05,
        rect.width() * 0.04,
    ));
    let left_from = p(rect, 0.42, 0.5);
    let right_from = p(rect, 0.58, 0.5);
    painter.line_segment([left_from, p(rect, 0.20, 0.5)], st);
    painter.line_segment([right_from, p(rect, 0.80, 0.5)], st);
    arrow_head(
        painter,
        p(rect, 0.20, 0.5),
        vec2(-1.0, 0.0),
        rect.width() * 0.09,
        color,
    );
    arrow_head(
        painter,
        p(rect, 0.80, 0.5),
        vec2(1.0, 0.0),
        rect.width() * 0.09,
        color,
    );
}

/// 画像 = 上下反転: `paint_flip_horizontal_icon` を 90°回した意匠(SPEC §33)。
pub fn paint_flip_vertical_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let mid = [p(rect, 0.16, 0.5), p(rect, 0.84, 0.5)];
    painter.extend(Shape::dashed_line(
        &mid,
        st,
        rect.width() * 0.05,
        rect.width() * 0.04,
    ));
    painter.line_segment([p(rect, 0.5, 0.42), p(rect, 0.5, 0.20)], st);
    painter.line_segment([p(rect, 0.5, 0.58), p(rect, 0.5, 0.80)], st);
    arrow_head(
        painter,
        p(rect, 0.5, 0.20),
        vec2(0.0, -1.0),
        rect.width() * 0.09,
        color,
    );
    arrow_head(
        painter,
        p(rect, 0.5, 0.80),
        vec2(0.0, 1.0),
        rect.width() * 0.09,
        color,
    );
}

/// 画像 = 右に90°回転 / 左に90°回転が共有する円弧矢印。`clockwise` で向きを
/// 決める。
fn rotate_arc_icon(painter: &Painter, rect: Rect, color: Color32, clockwise: bool) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.56);
    let radius = rect.width() * 0.28;
    let steps = 20;
    let (start_angle, end_angle) = if clockwise {
        (
            std::f32::consts::PI * 1.2,
            std::f32::consts::PI * 1.2 + std::f32::consts::PI * 1.5,
        )
    } else {
        (
            std::f32::consts::PI * 1.8,
            std::f32::consts::PI * 1.8 - std::f32::consts::PI * 1.5,
        )
    };
    let path: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let angle = start_angle + (end_angle - start_angle) * t;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(path.clone(), st));
    let tip = path[path.len() - 1];
    let dir = (tip - path[path.len() - 2]).normalized();
    arrow_head(painter, tip, dir, rect.width() * 0.10, color);
    let r = rect.width() * 0.18;
    painter.rect_stroke(
        Rect::from_center_size(center, vec2(r, r)),
        1.0,
        st,
        egui::StrokeKind::Middle,
    );
}

/// 画像 = 右に90°回転(SPEC §33)。
pub fn paint_rotate_cw_icon(painter: &Painter, rect: Rect, color: Color32) {
    rotate_arc_icon(painter, rect, color, true);
}

/// 画像 = 左に90°回転(SPEC §33)。
pub fn paint_rotate_ccw_icon(painter: &Painter, rect: Rect, color: Color32) {
    rotate_arc_icon(painter, rect, color, false);
}

/// 画像 = 明るさ・コントラスト: 半分だけ塗った円(古典的なコントラスト
/// アイコン、SPEC §33)。
pub fn paint_brightness_contrast_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.5);
    let radius = rect.width() * 0.32;
    painter.circle_stroke(center, radius, st);
    let steps = 16;
    let mut half: Vec<Pos2> = vec![center];
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let angle = std::f32::consts::FRAC_PI_2 + t * std::f32::consts::PI;
        half.push(center + vec2(angle.cos(), angle.sin()) * radius);
    }
    painter.add(Shape::convex_polygon(half, color, Stroke::NONE));
}

/// 画像 = 色相・彩度・明度: 3 分割の色相リング(SPEC §33。`color_wheel.rs` の
/// リングを単色・簡略化した意匠)。
pub fn paint_hue_saturation_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.5);
    let radius = rect.width() * 0.32;
    painter.circle_stroke(center, radius, st);
    for i in 0..3 {
        let angle = i as f32 / 3.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let dir = vec2(angle.cos(), angle.sin());
        painter.line_segment([center + dir * radius * 0.4, center + dir * radius], st);
    }
}

/// 画像 = 階調の反転: 円を左右で塗り分ける(SPEC §33)。
pub fn paint_invert_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.5);
    let radius = rect.width() * 0.32;
    painter.circle_stroke(center, radius, st);
    let steps = 16;
    let mut half: Vec<Pos2> = vec![center];
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let angle = -std::f32::consts::FRAC_PI_2 + t * std::f32::consts::PI;
        half.push(center + vec2(angle.cos(), angle.sin()) * radius);
    }
    painter.add(Shape::convex_polygon(half, color, Stroke::NONE));
}

/// 画像 = グレースケール化: 濃淡が変化する 4 枚のタイル(SPEC §33)。
pub fn paint_grayscale_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    const TILES: i32 = 4;
    let box_rect = Rect::from_min_max(p(rect, 0.16, 0.34), p(rect, 0.84, 0.66));
    for i in 0..TILES {
        let t0 = i as f32 / TILES as f32;
        let t1 = (i + 1) as f32 / TILES as f32;
        let alpha = (255.0 * (i as f32 + 1.0) / TILES as f32)
            .round()
            .clamp(0.0, 255.0) as u8;
        let tile_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
        let tile_rect = Rect::from_min_max(
            pos2(box_rect.min.x + t0 * box_rect.width(), box_rect.min.y),
            pos2(box_rect.min.x + t1 * box_rect.width(), box_rect.max.y),
        );
        painter.rect_filled(tile_rect, 0.0, tile_color);
    }
    painter.rect_stroke(box_rect, 1.0, st, egui::StrokeKind::Middle);
}

/// レイヤー = 新規レイヤー: 積み重なった矩形+「+」(SPEC §33)。
pub fn paint_layer_add_icon(painter: &Painter, rect: Rect, color: Color32) {
    let front = stacked_rects(painter, rect, color, 2);
    let st = line_stroke(rect, color);
    plus_mark(
        painter,
        front.center() + vec2(0.0, front.height() * 0.05),
        rect.width() * 0.10,
        st,
    );
}

/// レイヤー = レイヤーを複製: 積み重なった矩形 3 枚(複製されたことを示す、
/// SPEC §33)。
pub fn paint_layer_duplicate_icon(painter: &Painter, rect: Rect, color: Color32) {
    stacked_rects(painter, rect, color, 3);
}

/// レイヤー = レイヤーを削除: 積み重なった矩形+「×」(SPEC §33)。
pub fn paint_layer_delete_icon(painter: &Painter, rect: Rect, color: Color32) {
    let front = stacked_rects(painter, rect, color, 2);
    let st = line_stroke(rect, color);
    cross_mark(
        painter,
        front.center() + vec2(0.0, front.height() * 0.05),
        rect.width() * 0.09,
        st,
    );
}

/// レイヤー = 上へ移動 / 下へ移動が共有するシェブロン矢印。
fn chevron_icon(painter: &Painter, rect: Rect, color: Color32, up: bool) {
    let st = line_stroke(rect, color);
    let (tip_y, base_y) = if up { (0.22, 0.62) } else { (0.78, 0.38) };
    let tip = p(rect, 0.5, tip_y);
    let left = p(rect, 0.28, base_y);
    let right = p(rect, 0.72, base_y);
    painter.line_segment([left, tip], st);
    painter.line_segment([right, tip], st);
    let stem_y = if up { 0.86 } else { 0.14 };
    painter.line_segment([p(rect, 0.5, base_y), p(rect, 0.5, stem_y)], st);
}

/// レイヤー = 上へ移動(SPEC §33)。
pub fn paint_layer_move_up_icon(painter: &Painter, rect: Rect, color: Color32) {
    chevron_icon(painter, rect, color, true);
}

/// レイヤー = 下へ移動(SPEC §33)。
pub fn paint_layer_move_down_icon(painter: &Painter, rect: Rect, color: Color32) {
    chevron_icon(painter, rect, color, false);
}

/// レイヤー = 下と結合: 2 枚の層+下向き矢印(結合先へ合流する様子、
/// SPEC §33)。
pub fn paint_layer_merge_down_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let upper = Rect::from_min_max(p(rect, 0.24, 0.14), p(rect, 0.76, 0.40));
    let lower = Rect::from_min_max(p(rect, 0.24, 0.66), p(rect, 0.76, 0.92));
    painter.rect_stroke(upper, 1.0, st, egui::StrokeKind::Middle);
    painter.rect_stroke(lower, 1.0, st, egui::StrokeKind::Middle);
    let arrow_from = p(rect, 0.5, 0.42);
    let arrow_to = p(rect, 0.5, 0.60);
    painter.line_segment([arrow_from, arrow_to], st);
    arrow_head(
        painter,
        arrow_to,
        vec2(0.0, 1.0),
        rect.width() * 0.09,
        color,
    );
}

/// レイヤー = 画像の統合: 積み重なった層が 1 枚に収束する(SPEC §33)。
pub fn paint_layer_flatten_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    for i in 0..3 {
        let y = 0.16 + i as f32 * 0.10;
        let inset = i as f32 * 0.04;
        let r = Rect::from_min_max(p(rect, 0.24 + inset, y), p(rect, 0.76 - inset, y + 0.16));
        painter.rect_stroke(r, 1.0, st, egui::StrokeKind::Middle);
    }
    let bottom = Rect::from_min_max(p(rect, 0.20, 0.68), p(rect, 0.80, 0.88));
    painter.rect_stroke(bottom, 1.0, st, egui::StrokeKind::Middle);
    painter.line_segment([p(rect, 0.5, 0.46), p(rect, 0.5, 0.64)], st);
    arrow_head(
        painter,
        p(rect, 0.5, 0.64),
        vec2(0.0, 1.0),
        rect.width() * 0.07,
        color,
    );
}

/// 表示 = 拡大: 虫眼鏡+「+」(ツールバーの `zoom_icon`(ズームツール、
/// 持ち手つき)とは区別する簡略版、SPEC §33)。
pub fn paint_zoom_in_icon(painter: &Painter, rect: Rect, color: Color32) {
    zoom_glass_icon(painter, rect, color, true);
}

/// 表示 = 縮小: 虫眼鏡+「−」(SPEC §33)。
pub fn paint_zoom_out_icon(painter: &Painter, rect: Rect, color: Color32) {
    zoom_glass_icon(painter, rect, color, false);
}

fn zoom_glass_icon(painter: &Painter, rect: Rect, color: Color32, plus: bool) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.42, 0.42);
    let radius = rect.width() * 0.26;
    painter.circle_stroke(center, radius, st);
    painter.line_segment(
        [
            center + vec2(radius * 0.7, radius * 0.7),
            p(rect, 0.88, 0.88),
        ],
        st,
    );
    let inner = radius * 0.5;
    painter.line_segment([center - vec2(inner, 0.0), center + vec2(inner, 0.0)], st);
    if plus {
        painter.line_segment([center - vec2(0.0, inner), center + vec2(0.0, inner)], st);
    }
}

/// 表示 = 100%: 同じ大きさの矩形 2 枚(拡縮されていないことを示す、SPEC §33)。
pub fn paint_zoom_100_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let a = Rect::from_min_max(p(rect, 0.14, 0.30), p(rect, 0.44, 0.70));
    let b = Rect::from_min_max(p(rect, 0.56, 0.30), p(rect, 0.86, 0.70));
    painter.rect_stroke(a, 0.0, st, egui::StrokeKind::Middle);
    painter.rect_stroke(b, 0.0, st, egui::StrokeKind::Middle);
    painter.line_segment([p(rect, 0.48, 0.44), p(rect, 0.52, 0.44)], st);
    painter.line_segment([p(rect, 0.48, 0.56), p(rect, 0.52, 0.56)], st);
}

/// 表示 = ウィンドウに合わせる: 中心から四隅へ広がる矢印(SPEC §33)。
pub fn paint_fit_window_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let outer = Rect::from_min_max(p(rect, 0.16, 0.16), p(rect, 0.84, 0.84));
    painter.rect_stroke(outer, 0.0, st, egui::StrokeKind::Middle);
    let center = outer.center();
    let head = rect.width() * 0.08;
    for corner in [
        outer.left_top(),
        outer.right_top(),
        outer.left_bottom(),
        outer.right_bottom(),
    ] {
        let mid = center + (corner - center) * 0.4;
        let dir = (corner - center).normalized();
        painter.line_segment([mid, corner - dir * head * 0.6], st);
        arrow_head(painter, corner - dir * head * 0.6, dir, head, color);
    }
}

/// 表示 = ピクセルグリッド表示: 3×3 の格子(SPEC §33。トグル状態は
/// `ui/menu.rs` が `.selected` ハイライトで表示するので、アイコン自体は
/// 常に同じ意匠でよい)。
pub fn paint_pixel_grid_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let outer = Rect::from_min_max(p(rect, 0.18, 0.18), p(rect, 0.82, 0.82));
    painter.rect_stroke(outer, 0.0, st, egui::StrokeKind::Middle);
    for t in [1.0 / 3.0, 2.0 / 3.0] {
        let x = outer.min.x + t * outer.width();
        painter.line_segment([pos2(x, outer.min.y), pos2(x, outer.max.y)], st);
        let y = outer.min.y + t * outer.height();
        painter.line_segment([pos2(outer.min.x, y), pos2(outer.max.x, y)], st);
    }
}

/// その他 = バージョン情報: 丸に「i」(SPEC §33)。
pub fn paint_about_icon(painter: &Painter, rect: Rect, color: Color32) {
    let st = line_stroke(rect, color);
    let center = p(rect, 0.5, 0.52);
    let radius = rect.width() * 0.32;
    painter.circle_stroke(center, radius, st);
    let dot = p(rect, 0.5, 0.36);
    painter.circle_filled(dot, rect.width() * 0.035, color);
    painter.line_segment([p(rect, 0.5, 0.46), p(rect, 0.5, 0.68)], st);
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
