//! 色相リング + SV 三角形ウィジェット(SPEC §14 項目2、ARCHITECTURE.md §14.3)。
//!
//! 純関数(角度・重心座標・HSV↔RGB 変換、すべてテスト対象)と、実際の
//! 描画・入力処理(`ColorWheelState::ui`)を分離する(ARCHITECTURE.md §14.3
//! 「純関数(テスト必須)と描画/入力を分離する」)。
//!
//! 角度系: 色相 h∈[0,360)。12 時方向が 0°、時計回り(SPEC §14)。
//! 内接する固定向きの正三角形: 頂点は 上=純色相(S=1,V=1)/ 左下=黒(V=0)/
//! 右下=白(S=0,V=1)。S/V ↔ 重心座標の変換は ARCHITECTURE.md §14.3 の式
//! (a=S*V, c=V*(1-S), b=1-V; 逆変換 V=a+c, S=V>0?a/V:0)をそのまま使う。
//!
//! ARCHITECTURE.md §14.9-1(v2 の落とし穴): 「RGB↔HSV 往復で S=0 や V=0 の
//! ときに色相が失われ、三角形ドラッグ中にマーカーが飛ぶ」ため、ドラッグ中は
//! この `ColorWheelState` が保持する HSV を正とし、ドラッグ終了後も
//! (彩度がほぼ 0 の間は)前回の色相を保持する(`sync_if_idle`)。

use eframe::egui::{self, pos2, vec2, Color32, PointerButton, Pos2, Vec2};

/// SPEC §14: 「直径約170px」。
pub const DIAMETER: f32 = 170.0;
const RING_THICKNESS: f32 = 18.0;
const TRIANGLE_GAP: f32 = 8.0;
/// リング分割数(ARCHITECTURE.md §14.3: 「72 分割の三角形ストリップ」)。
const RING_SEGMENTS: usize = 72;
/// マーカーの半径(スクリーン論理ポイント)。
const MARKER_RADIUS: f32 = 5.0;
/// 彩度がこれ未満のときは色相情報が実質失われているとみなし、既存の色相を
/// 保持する(ARCHITECTURE.md §14.9-1)。
const HUE_PRESERVE_EPS: f32 = 1.0 / 255.0;
/// 三角形のヒット判定の緩さ(ARCHITECTURE.md §14.9-6: 「外周ぎわのデッド
/// ゾーンを作らない」)。重心座標の各成分がこの値以上負でも三角形内とみなす。
const TRIANGLE_HIT_SLACK: f32 = 0.12;

// ---------------------------------------------------------------------
// 純関数: HSV ↔ RGB(標準的な非ガンマ変換。テスト対象)
// ---------------------------------------------------------------------

/// RGB(0–255) → HSV(h: 0..360, s/v: 0..1)。
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let v = max;
    let s = if max > 0.0 { delta / max } else { 0.0 };
    let h = if delta <= f32::EPSILON {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta).rem_euclid(6.0))
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    (h.rem_euclid(360.0), s, v)
}

/// HSV(h: 0..360, s/v: 0..1、範囲外は内部でクランプ) → RGB(0–255)。
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |ch: f32| ((ch + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

// ---------------------------------------------------------------------
// 純関数: 角度・リング・三角形の幾何(ARCHITECTURE.md §14.3、テスト対象)
// ---------------------------------------------------------------------

/// 12 時方向を 0° とし、時計回りに増える角度の単位ベクトル。
fn dir(deg: f32) -> Vec2 {
    let rad = deg.to_radians();
    vec2(rad.sin(), -rad.cos())
}

/// 色相 `hue_deg`(0..360)に対応するリング上の位置。
pub fn pos_on_ring(center: Pos2, radius: f32, hue_deg: f32) -> Pos2 {
    center + dir(hue_deg) * radius
}

/// リング上の位置から色相(0..360)を求める(`pos_on_ring` の逆変換)。
/// `pos == center` のときは 0 を返す(未定義だが、パニックはしない)。
pub fn hue_from_pos(center: Pos2, pos: Pos2) -> f32 {
    let d = pos - center;
    if d.length_sq() <= f32::EPSILON {
        return 0.0;
    }
    d.x.atan2(-d.y).to_degrees().rem_euclid(360.0)
}

/// 三角形の 3 頂点(ARCHITECTURE.md §14.3: 上=純色相 / 左下=黒 / 右下=白)。
/// 戻り値は `(p_hue, p_black, p_white)`。
pub fn triangle_vertices(center: Pos2, r_in: f32) -> (Pos2, Pos2, Pos2) {
    let p_hue = center + dir(0.0) * r_in;
    let p_black = center + dir(240.0) * r_in;
    let p_white = center + dir(120.0) * r_in;
    (p_hue, p_black, p_white)
}

/// 点 `p` の三角形 `(a, b, c)` に対する重心座標 `(wa, wb, wc)`(`wa+wb+wc == 1`、
/// 三角形の外なら負の成分を含みうる)。
fn barycentric(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> (f32, f32, f32) {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() <= f32::EPSILON {
        return (1.0, 0.0, 0.0);
    }
    let wb = (d11 * d20 - d01 * d21) / denom;
    let wc = (d00 * d21 - d01 * d20) / denom;
    let wa = 1.0 - wb - wc;
    (wa, wb, wc)
}

/// `pos` が三角形 `(p_hue, p_black, p_white)` の内側(緩め判定、
/// ARCHITECTURE.md §14.9-6)にあるか。
fn triangle_contains_loosely(pos: Pos2, p_hue: Pos2, p_black: Pos2, p_white: Pos2) -> bool {
    let (a, b, c) = barycentric(pos, p_hue, p_black, p_white);
    a >= -TRIANGLE_HIT_SLACK && b >= -TRIANGLE_HIT_SLACK && c >= -TRIANGLE_HIT_SLACK
}

/// 三角形内の位置から S/V を求める(ARCHITECTURE.md §14.3)。三角形の外に
/// あっても、負の重心座標を 0 にクランプしてから正規化することで最近傍へ
/// 丸める(「外れてもクランプ」)。戻り値は常に `[0,1]` に収まる。
pub fn sv_from_pos(pos: Pos2, p_hue: Pos2, p_black: Pos2, p_white: Pos2) -> (f32, f32) {
    let (a, b, c) = barycentric(pos, p_hue, p_black, p_white);
    let (a, _b, c) = if a < 0.0 || b < 0.0 || c < 0.0 {
        let (ca, cb, cc) = (a.max(0.0), b.max(0.0), c.max(0.0));
        let sum = ca + cb + cc;
        if sum > f32::EPSILON {
            (ca / sum, cb / sum, cc / sum)
        } else {
            (1.0, 0.0, 0.0)
        }
    } else {
        (a, b, c)
    };
    let v = (a + c).clamp(0.0, 1.0);
    let s = if v > f32::EPSILON {
        (a / v).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (s, v)
}

/// S/V から三角形内の位置を求める(`sv_from_pos` の逆変換の片道)。
pub fn sv_to_pos(s: f32, v: f32, p_hue: Pos2, p_black: Pos2, p_white: Pos2) -> Pos2 {
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let a = s * v;
    let c = v * (1.0 - s);
    let b = 1.0 - v;
    pos2(
        a * p_hue.x + b * p_black.x + c * p_white.x,
        a * p_hue.y + b * p_black.y + c * p_white.y,
    )
}

// ---------------------------------------------------------------------
// ウィジェット本体
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Hue,
    Sv,
}

/// カラーホイールの編集中状態(ARCHITECTURE.md §14.3)。`DaraskApp` が
/// フィールドとして保持し、フレームをまたいで持ち越す。
pub struct ColorWheelState {
    pub h: f32,
    pub s: f32,
    pub v: f32,
    /// `None` = ジェスチャなし。`Some(None)` = ジェスチャ進行中だが開始点が
    /// リング帯にも三角形にも当たらなかった(デッドゾーン、ボタンを離す
    /// まで「無効」で固定)。`Some(Some(mode))` = ジェスチャ進行中で
    /// モードが固定された状態(SPEC §14 項目2: 「ドラッグ開始領域で確定し、
    /// 外れてもクランプ」)。
    drag_mode: Option<Option<DragMode>>,
}

impl ColorWheelState {
    pub fn new() -> Self {
        Self {
            h: 0.0,
            s: 0.0,
            v: 0.0,
            drag_mode: None,
        }
    }

    /// ドラッグ中でなければ `color` の RGB から HSV を同期する。彩度が
    /// ほぼ 0(グレー/黒)のときは色相が数値的に無意味になるため、既存の
    /// 色相をそのまま保持する(ARCHITECTURE.md §14.9-1)。
    pub fn sync_if_idle(&mut self, color: Color32) {
        if self.drag_mode.is_some() {
            return;
        }
        let [r, g, b, _a] = color.to_srgba_unmultiplied();
        let (h, s, v) = rgb_to_hsv(r, g, b);
        self.v = v;
        self.s = s;
        if s > HUE_PRESERVE_EPS {
            self.h = h;
        }
    }

    /// ウィジェットを描画し、ドラッグ入力を処理して `primary` の RGB を
    /// 更新する(アルファは呼び出し側の管理、SPEC §14: 「編集対象は常に
    /// プライマリ」)。
    pub fn ui(&mut self, ui: &mut egui::Ui, primary: &mut Color32) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(vec2(DIAMETER, DIAMETER), egui::Sense::click_and_drag());
        let center = rect.center();
        let outer_r = DIAMETER / 2.0;
        let inner_r = outer_r - RING_THICKNESS;
        let tri_r = inner_r - TRIANGLE_GAP;
        let (p_hue, p_black, p_white) = triangle_vertices(center, tri_r);

        // v2 レビューで発見・修正したバグ2件:
        // (1) `Sense::click_and_drag` の `interact_pointer_pos()` はボタン
        //     種別を区別しないため、右クリック・右ドラッグ・中ドラッグでも
        //     プライマリ色が変わってしまっていた(パレット/ペン/スポイト等、
        //     アプリ全体の「右=セカンダリ」の慣習と衝突する)。プライマリ
        //     ボタンが押されている間だけ入力を受け付ける。
        // (2) デッドゾーン(リング帯にも三角形にも当たらない領域)で
        //     押下を開始した場合、以前は `drag_mode` が `None` のままだった
        //     ため、ボタンを離さずにポインタをリング帯/三角形へ移動させた
        //     瞬間にヒットテストが再実行され、後付けでモードが確定して
        //     しまっていた(SPEC §14 項目2「ドラッグ開始領域で確定」/
        //     ARCHITECTURE.md §14.3「離すまで維持」違反)。`resolve_drag_mode`
        //     は「デッドゾーン」自体も `Some(None)` として押下中は固定し、
        //     ボタンを離すまで再判定しない。
        let primary_down = ui.input(|i| i.pointer.button_down(PointerButton::Primary));
        let pointer_pos = response.interact_pointer_pos();
        self.drag_mode = resolve_drag_mode(
            self.drag_mode,
            primary_down,
            pointer_pos,
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );

        if let (true, Some(pos), Some(Some(mode))) = (primary_down, pointer_pos, self.drag_mode) {
            match mode {
                DragMode::Hue => self.h = hue_from_pos(center, pos),
                DragMode::Sv => {
                    let (s, v) = sv_from_pos(pos, p_hue, p_black, p_white);
                    self.s = s;
                    self.v = v;
                }
            }
            let (r, g, b) = hsv_to_rgb(self.h, self.s, self.v);
            // ecolor 0.35 の `Color32::from_rgba_unmultiplied` は alpha==0 の
            // とき RGB を捨てて `TRANSPARENT` を返す(共通ケース最適化)ため、
            // ここで直接使うとアルファが 0 の間ホイールドラッグが `primary`
            // に反映されなくなる。RGB を保つ `rgba_unmultiplied_keep_rgb` を
            // 使う(`color_panel::alpha_slider` と同じ問題、同じ対策)。
            *primary = rgba_unmultiplied_keep_rgb(r, g, b, primary.a());
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            draw_ring(&painter, center, inner_r, outer_r);
            draw_triangle(&painter, p_hue, p_black, p_white, self.h);
            let ring_marker = pos_on_ring(center, (inner_r + outer_r) / 2.0, self.h);
            draw_marker(&painter, ring_marker);
            let sv_marker = sv_to_pos(self.s, self.v, p_hue, p_black, p_white);
            draw_marker(&painter, sv_marker);
        }

        response
    }
}

/// `ColorWheelState::ui` のドラッグモード決定を、egui の `Ui`/`Response` を
/// 必要としない純関数として切り出したもの(ユニットテスト用、
/// ARCHITECTURE.md §14.3 の「ドラッグ開始位置で固定し、離すまで維持」を
/// 直接検証できるようにする)。
///
/// - `primary_down` が `false`(ボタンが離れている、または右/中ボタンしか
///   押されていない)なら常に `None` を返す(ジェスチャなし)。
/// - `current` が `Some(_)` なら(デッドゾーンとして固定された `Some(None)`
///   も含め)そのまま維持する — 一度確定したモードは、ボタンを離すまで
///   ポインタがどこへ動いても変わらない。
/// - `current` が `None` で `primary_down` かつポインタ位置があるときだけ、
///   ヒットテストして新しいモードを確定する。
#[allow(clippy::too_many_arguments)]
fn resolve_drag_mode(
    current: Option<Option<DragMode>>,
    primary_down: bool,
    pos: Option<Pos2>,
    center: Pos2,
    inner_r: f32,
    outer_r: f32,
    p_hue: Pos2,
    p_black: Pos2,
    p_white: Pos2,
) -> Option<Option<DragMode>> {
    if !primary_down {
        return None;
    }
    let pos = pos?;
    if let Some(existing) = current {
        return Some(existing);
    }
    let dist = pos.distance(center);
    let mode = if dist >= inner_r && dist <= outer_r {
        Some(DragMode::Hue)
    } else if triangle_contains_loosely(pos, p_hue, p_black, p_white) {
        Some(DragMode::Sv)
    } else {
        None
    };
    Some(mode)
}

/// `Color32::from_rgba_unmultiplied` の代替: alpha==0 のときも RGB を保持
/// する。
///
/// ecolor 0.35 の `from_rgba_unmultiplied` は alpha==0 のとき「共通ケースの
/// 最適化」として無条件に `Color32::TRANSPARENT`(RGB も 0)を返すため、
/// 一度アルファを 0 にすると選んだ色相が失われてしまう
/// (ecolor-0.35.0/src/color32.rs の `from_rgba_unmultiplied` 参照、
/// `Cargo.lock` で 0.35.0 に固定されていることを確認済み)。
/// `from_rgba_premultiplied` にはこの最適化が無く 4 バイトをそのまま
/// 保持し、`to_srgba_unmultiplied` も alpha==0 のときは無条件にそのバイト列を
/// そのまま返す(ゼロ除算を避ける共通ケース最適化)ため、alpha==0 の間だけ
/// こちらを使えば r,g,b を往復できる。alpha!=0 のときは通常どおり
/// `from_rgba_unmultiplied`(内部でガンマ正しい事前乗算 LUT を使う)を使う。
pub fn rgba_unmultiplied_keep_rgb(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    if a == 0 {
        Color32::from_rgba_premultiplied(r, g, b, 0)
    } else {
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }
}

impl Default for ColorWheelState {
    fn default() -> Self {
        Self::new()
    }
}

/// 色相リングを 72 分割の三角形ストリップ(`Mesh`)で描く(ARCHITECTURE.md
/// §14.3)。頂点色は内周・外周とも `hsv(h,1,1)` で、角度方向のみ変化する。
fn draw_ring(painter: &egui::Painter, center: Pos2, inner_r: f32, outer_r: f32) {
    let mut mesh = egui::Mesh::default();
    for i in 0..=RING_SEGMENTS {
        let hue = 360.0 * i as f32 / RING_SEGMENTS as f32;
        let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
        let color = Color32::from_rgb(r, g, b);
        mesh.colored_vertex(pos_on_ring(center, inner_r, hue), color);
        mesh.colored_vertex(pos_on_ring(center, outer_r, hue), color);
        if i < RING_SEGMENTS {
            let base = (i * 2) as u32;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 2, base + 3);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// SV 三角形を頂点色 1 枚のメッシュで描く(ARCHITECTURE.md §14.3:
/// 「頂点色(純色相/黒/白)1 枚のメッシュ(RGB 線形補間で標準的な見た目)」)。
fn draw_triangle(painter: &egui::Painter, p_hue: Pos2, p_black: Pos2, p_white: Pos2, hue: f32) {
    let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(p_hue, Color32::from_rgb(r, g, b));
    mesh.colored_vertex(p_black, Color32::BLACK);
    mesh.colored_vertex(p_white, Color32::WHITE);
    mesh.add_triangle(0, 1, 2);
    painter.add(egui::Shape::mesh(mesh));
}

/// 白フチ+黒フチの小円マーカー(背景を問わず視認できるように、SPEC §14 の
/// 意図をリング/三角形のマーカーにも適用する)。
fn draw_marker(painter: &egui::Painter, pos: Pos2) {
    painter.circle_stroke(pos, MARKER_RADIUS, egui::Stroke::new(2.5, Color32::WHITE));
    painter.circle_stroke(pos, MARKER_RADIUS, egui::Stroke::new(1.0, Color32::BLACK));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    // -- HSV ↔ RGB ------------------------------------------------------

    #[test]
    fn hsv_rgb_roundtrip() {
        let samples: &[(u8, u8, u8)] = &[
            (0, 0, 0),
            (255, 255, 255),
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (255, 255, 0),
            (0, 255, 255),
            (255, 0, 255),
            (128, 64, 32),
            (17, 200, 90),
            (200, 17, 240),
        ];
        for &(r, g, b) in samples {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!(
                (r as i32 - r2 as i32).abs() <= 1
                    && (g as i32 - g2 as i32).abs() <= 1
                    && (b as i32 - b2 as i32).abs() <= 1,
                "roundtrip mismatch for ({r},{g},{b}): got ({r2},{g2},{b2}) via hsv ({h},{s},{v})"
            );
        }
    }

    #[test]
    fn hsv_pure_hues_match_expected_rgb() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), (0, 0, 255));
    }

    // -- 角度・リング -----------------------------------------------------

    #[test]
    fn pos_on_ring_matches_clock_convention() {
        // SPEC §14: 「0°=赤を12時方向、時計回り」。
        let center = pos2(100.0, 100.0);
        let up = pos_on_ring(center, 50.0, 0.0);
        assert!(
            approx(up.x, 100.0, 1e-3) && approx(up.y, 50.0, 1e-3),
            "{up:?}"
        );
        let right = pos_on_ring(center, 50.0, 90.0);
        assert!(
            approx(right.x, 150.0, 1e-3) && approx(right.y, 100.0, 1e-3),
            "{right:?}"
        );
        let down = pos_on_ring(center, 50.0, 180.0);
        assert!(
            approx(down.x, 100.0, 1e-3) && approx(down.y, 150.0, 1e-3),
            "{down:?}"
        );
        let left = pos_on_ring(center, 50.0, 270.0);
        assert!(
            approx(left.x, 50.0, 1e-3) && approx(left.y, 100.0, 1e-3),
            "{left:?}"
        );
    }

    #[test]
    fn hue_pos_roundtrip() {
        let center = pos2(37.0, -12.0);
        for i in 0..24 {
            let h = i as f32 * 15.0;
            for radius in [10.0, 50.0, 85.0] {
                let pos = pos_on_ring(center, radius, h);
                let h2 = hue_from_pos(center, pos);
                let diff = (h2 - h).rem_euclid(360.0);
                let diff = diff.min(360.0 - diff);
                assert!(diff < 1e-2, "h={h} radius={radius} got h2={h2}");
            }
        }
    }

    // -- 三角形・重心座標 --------------------------------------------------

    #[test]
    fn triangle_vertices_match_fixed_orientation() {
        // ARCHITECTURE.md §14.3: 上=純色相 / 左下=黒 / 右下=白。
        let center = pos2(0.0, 0.0);
        let (p_hue, p_black, p_white) = triangle_vertices(center, 10.0);
        assert!(p_hue.y < center.y, "P_hue must be above center: {p_hue:?}");
        assert!(approx(p_hue.x, 0.0, 1e-3));
        assert!(
            p_black.x < center.x && p_black.y > center.y,
            "P_black must be lower-left: {p_black:?}"
        );
        assert!(
            p_white.x > center.x && p_white.y > center.y,
            "P_white must be lower-right: {p_white:?}"
        );
    }

    #[test]
    fn sv_corners_match_expected_vertices() {
        let (p_hue, p_black, p_white) = triangle_vertices(pos2(0.0, 0.0), 10.0);
        let at_hue = sv_to_pos(1.0, 1.0, p_hue, p_black, p_white); // S=1,V=1
        assert!(at_hue.distance(p_hue) < 1e-3, "{at_hue:?} vs {p_hue:?}");
        let at_black = sv_to_pos(0.0, 0.0, p_hue, p_black, p_white); // V=0
        assert!(
            at_black.distance(p_black) < 1e-3,
            "{at_black:?} vs {p_black:?}"
        );
        let at_white = sv_to_pos(0.0, 1.0, p_hue, p_black, p_white); // S=0,V=1
        assert!(
            at_white.distance(p_white) < 1e-3,
            "{at_white:?} vs {p_white:?}"
        );
    }

    #[test]
    fn sv_roundtrip() {
        let (p_hue, p_black, p_white) = triangle_vertices(pos2(5.0, 8.0), 63.0);
        for s10 in 0..=10 {
            for v10 in 2..=10 {
                // v=0 は黒 1 点に潰れるため S を復元できない(既知の限界、
                // ARCHITECTURE.md §14.9-1)。v>=0.2 のみ検証する。
                let s = s10 as f32 / 10.0;
                let v = v10 as f32 / 10.0;
                let pos = sv_to_pos(s, v, p_hue, p_black, p_white);
                let (s2, v2) = sv_from_pos(pos, p_hue, p_black, p_white);
                assert!(approx(s, s2, 1e-2), "s={s} s2={s2} v={v}");
                assert!(approx(v, v2, 1e-2), "v={v} v2={v2} s={s}");
            }
        }
    }

    #[test]
    fn sv_from_pos_clamps_outside_triangle() {
        let (p_hue, p_black, p_white) = triangle_vertices(pos2(0.0, 0.0), 10.0);
        // 三角形から大きく外れた点でもパニックせず [0,1] に収まる。
        let far = pos2(1000.0, -1000.0);
        let (s, v) = sv_from_pos(far, p_hue, p_black, p_white);
        assert!((0.0..=1.0).contains(&s));
        assert!((0.0..=1.0).contains(&v));
    }

    #[test]
    fn triangle_hit_test_has_no_dead_zone_at_edges() {
        let (p_hue, p_black, p_white) = triangle_vertices(pos2(0.0, 0.0), 60.0);
        // 三角形のちょうど外側ギリギリ(重心を少しだけ外側へ)でもヒットする。
        let centroid = pos2(
            (p_hue.x + p_black.x + p_white.x) / 3.0,
            (p_hue.y + p_black.y + p_white.y) / 3.0,
        );
        let just_outside = pos2(
            p_white.x + (p_white.x - centroid.x) * 0.05,
            p_white.y + (p_white.y - centroid.y) * 0.05,
        );
        assert!(triangle_contains_loosely(
            just_outside,
            p_hue,
            p_black,
            p_white
        ));
    }

    // -- HSV 状態の同期(色相保持) ------------------------------------------

    #[test]
    fn sync_preserves_hue_when_desaturated() {
        let mut state = ColorWheelState::new();
        state.h = 200.0;
        state.s = 0.8;
        state.v = 0.8;
        // グレーに同期しても色相は保持される(ARCHITECTURE.md §14.9-1)。
        state.sync_if_idle(Color32::from_rgb(128, 128, 128));
        assert!(approx(state.h, 200.0, 1e-3));
        assert!(approx(state.s, 0.0, 1e-3));
    }

    #[test]
    fn sync_updates_hue_when_saturated() {
        let mut state = ColorWheelState::new();
        state.h = 200.0;
        state.sync_if_idle(Color32::from_rgb(255, 0, 0));
        assert!(approx(state.h, 0.0, 1e-3));
        assert!(approx(state.s, 1.0, 1e-3));
        assert!(approx(state.v, 1.0, 1e-3));
    }

    // -- v2 レビューで発見・修正したバグ: ドラッグモードのデッドゾーン固定 --

    #[test]
    fn resolve_drag_mode_locks_dead_zone_for_the_rest_of_the_gesture() {
        // SPEC §14 項目2 / ARCHITECTURE.md §14.3: 「ドラッグ開始領域で確定し、
        // 外れてもクランプ」「離すまで維持」。デッドゾーン(リング帯にも
        // 三角形にも当たらない)で押下開始した場合、ボタンを離さずに
        // ポインタをリング帯へ移動させても `None`(無効)のまま固定される
        // べき。
        let center = pos2(0.0, 0.0);
        let (p_hue, p_black, p_white) = triangle_vertices(center, 40.0);
        let inner_r = 60.0;
        let outer_r = 85.0;

        let dead_zone_pos = pos2(200.0, 200.0); // リング外周のさらに外側。
        let mode = resolve_drag_mode(
            None,
            true,
            Some(dead_zone_pos),
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );
        assert_eq!(
            mode,
            Some(None),
            "dead zone press must resolve to a locked no-op"
        );

        // ボタンを離さずリング帯上へポインタを移動しても、既に確定した
        // `Some(None)` を維持したまま(後付けで Hue になったりしない)。
        let on_ring_pos = pos_on_ring(center, (inner_r + outer_r) / 2.0, 30.0);
        let still_locked = resolve_drag_mode(
            mode,
            true,
            Some(on_ring_pos),
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );
        assert_eq!(
            still_locked,
            Some(None),
            "mode must stay locked for the rest of the gesture"
        );
    }

    #[test]
    fn resolve_drag_mode_locks_hue_or_sv_for_the_rest_of_the_gesture() {
        let center = pos2(0.0, 0.0);
        let (p_hue, p_black, p_white) = triangle_vertices(center, 40.0);
        let inner_r = 60.0;
        let outer_r = 85.0;

        let ring_pos = pos_on_ring(center, (inner_r + outer_r) / 2.0, 90.0);
        let mode = resolve_drag_mode(
            None,
            true,
            Some(ring_pos),
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );
        assert_eq!(mode, Some(Some(DragMode::Hue)));

        // ポインタが三角形の内側へ移動しても、リングで確定した Hue モードの
        // ままであるべき(モードが後から Sv に切り替わってはいけない)。
        let still_hue = resolve_drag_mode(
            mode,
            true,
            Some(p_hue),
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );
        assert_eq!(still_hue, Some(Some(DragMode::Hue)));
    }

    #[test]
    fn resolve_drag_mode_ignores_non_primary_buttons() {
        // v2 レビューで発見・修正したバグ: 右/中ボタンのドラッグでモードが
        // 確定してしまうと、右クリック=セカンダリの慣習と衝突する。
        let center = pos2(0.0, 0.0);
        let (p_hue, p_black, p_white) = triangle_vertices(center, 40.0);
        let inner_r = 60.0;
        let outer_r = 85.0;
        let ring_pos = pos_on_ring(center, (inner_r + outer_r) / 2.0, 0.0);

        let mode = resolve_drag_mode(
            None,
            false,
            Some(ring_pos),
            center,
            inner_r,
            outer_r,
            p_hue,
            p_black,
            p_white,
        );
        assert_eq!(
            mode, None,
            "non-primary button presses must never engage the wheel"
        );
    }

    #[test]
    fn resolve_drag_mode_resets_on_release() {
        let center = pos2(0.0, 0.0);
        let (p_hue, p_black, p_white) = triangle_vertices(center, 40.0);
        let locked = Some(Some(DragMode::Hue));
        let after_release = resolve_drag_mode(
            locked, false, None, center, 60.0, 85.0, p_hue, p_black, p_white,
        );
        assert_eq!(after_release, None);
    }

    // -- v2 レビューで発見・修正したバグ: alpha=0 で RGB が消える -----------

    #[test]
    fn rgba_unmultiplied_keep_rgb_preserves_rgb_at_zero_alpha() {
        let color = rgba_unmultiplied_keep_rgb(200, 100, 50, 0);
        assert_eq!(color.to_srgba_unmultiplied(), [200, 100, 50, 0]);
        assert_ne!(
            color,
            Color32::TRANSPARENT,
            "plain from_rgba_unmultiplied would collapse this to TRANSPARENT"
        );
    }

    #[test]
    fn rgba_unmultiplied_keep_rgb_matches_normal_conversion_for_nonzero_alpha() {
        let kept = rgba_unmultiplied_keep_rgb(200, 100, 50, 128);
        let normal = Color32::from_rgba_unmultiplied(200, 100, 50, 128);
        assert_eq!(kept, normal);
    }

    #[test]
    fn rgba_unmultiplied_keep_rgb_matches_normal_conversion_for_full_alpha() {
        let kept = rgba_unmultiplied_keep_rgb(200, 100, 50, 255);
        let normal = Color32::from_rgba_unmultiplied(200, 100, 50, 255);
        assert_eq!(kept, normal);
    }
}
