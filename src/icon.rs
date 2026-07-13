//! アプリアイコンのベクター生成(SPEC §29、ARCHITECTURE.md §16.8)。
//!
//! 「角丸正方形+筆のストローク」程度のシンプルな図形をコードで描く
//! (画像アセットを持ち込まない)。ここは**純粋関数のみ**を置き、
//! - `examples/gen_icon.rs`(`#[path]` でこのファイルをそのまま取り込み、
//!   `image::codecs::ico` で `assets/icon.ico` を書き出す)
//! - `main.rs`(`eframe::egui::ViewportBuilder::with_icon` に同じ絵を渡す)
//!
//! の両方から同じ絵になるよう共有する(ARCHITECTURE.md §16.8:
//! 「eframe の viewport.with_icon にも同じ絵(生成関数を共有)を設定する」)。
//!
//! 他の `src` モジュールに依存しない(`examples/gen_icon.rs` がこのファイルを
//! 単体で `#[path]` include できるようにするため)。

/// `size × size` の RGBA8(straight alpha)アイコン画素を生成する。
///
/// `size` は 0 でも(空の `Vec` を返すだけで)パニックしない。`IconData`
/// (egui)・ICO(Windows)のどちらも正方形かつ幅が 4 の倍数を推奨するが、
/// この関数自体はどんな `size` でも安全に動作する。
///
/// デザイン: 角丸正方形の背景(青)+ 左下から右上へ斜めに引かれた
/// 筆ストローク(白)+ ストローク始端の絵の具の滴(橙)。1px 相当の帯で
/// 縁をなめらかにする(小サイズでの視認性のため)。
pub fn generate_icon_rgba(size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (size as usize) * (size as usize) * 4];
    if size == 0 {
        return buf;
    }
    let s = size as f32;
    let half = s / 2.0;
    // 角丸半径: 小サイズでも潰れないよう下限を設ける。
    let radius = (s * 0.22).max(1.0).min(half);

    const BG: [u8; 3] = [0x1E, 0x88, 0xE5]; // 青(SPEC §14 パレットの「青」)
    const STROKE: [u8; 3] = [0xFA, 0xFA, 0xFA]; // ほぼ白
    const DRIP: [u8; 3] = [0xFB, 0x8C, 0x00]; // 橙(SPEC §14 パレットの「橙」)

    // 筆ストローク: 左下 → 右上の対角線。
    let p0 = (s * 0.28, s * 0.74);
    let p1 = (s * 0.76, s * 0.26);
    let stroke_half_w = (s * 0.09).max(0.5);

    // ストローク始端の滴。
    let drip_center = (s * 0.23, s * 0.79);
    let drip_r = (s * 0.075).max(0.4);

    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let rect_d = rounded_box_sdist(px - half, py - half, half, radius);
            let rect_cov = coverage_from_sdist(rect_d);
            if rect_cov <= 0.0 {
                continue; // 透明のまま(市松の必要はない、単なる矩形外)。
            }

            let mut rgb = BG;

            let stroke_d = dist_to_segment(px, py, p0.0, p0.1, p1.0, p1.1) - stroke_half_w;
            let stroke_cov = coverage_from_sdist(stroke_d);
            rgb = lerp_rgb(rgb, STROKE, stroke_cov);

            let dx = px - drip_center.0;
            let dy = py - drip_center.1;
            let drip_d = (dx * dx + dy * dy).sqrt() - drip_r;
            let drip_cov = coverage_from_sdist(drip_d);
            rgb = lerp_rgb(rgb, DRIP, drip_cov);

            let alpha = (255.0 * rect_cov).round().clamp(0.0, 255.0) as u8;
            let idx = (y as usize * size as usize + x as usize) * 4;
            buf[idx] = rgb[0];
            buf[idx + 1] = rgb[1];
            buf[idx + 2] = rgb[2];
            buf[idx + 3] = alpha;
        }
    }
    buf
}

/// 角丸正方形の符号付き距離(Inigo Quilez の rounded-box SDF)。
/// `(px, py)` は正方形の中心を原点とした座標、`half` は角丸を含む全体の
/// 半径(= 一辺/2)、`radius` は角の丸め半径。負値が内側。
fn rounded_box_sdist(px: f32, py: f32, half: f32, radius: f32) -> f32 {
    let qx = px.abs() - half + radius;
    let qy = py.abs() - half + radius;
    qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - radius
}

/// 符号付き距離から被覆率(0..1)へ。境界の前後 0.5px でなめらかに遷移する。
fn coverage_from_sdist(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// 点 `(px, py)` から線分 `(x0,y0)-(x1,y1)` までの距離。
fn dist_to_segment(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return ((px - x0).powi(2) + (py - y0).powi(2)).sqrt();
    }
    let t = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
    let cx = x0 + t * dx;
    let cy = y0 + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
    ]
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_length_matches_size_squared_times_4() {
        for size in [1u32, 16, 32, 256] {
            let buf = generate_icon_rgba(size);
            assert_eq!(buf.len(), (size as usize) * (size as usize) * 4);
        }
    }

    #[test]
    fn zero_size_does_not_panic_and_is_empty() {
        let buf = generate_icon_rgba(0);
        assert!(buf.is_empty());
    }

    #[test]
    fn corners_are_transparent_due_to_rounding() {
        let size = 64u32;
        let buf = generate_icon_rgba(size);
        let idx = 0usize; // (0,0) の alpha
        assert_eq!(buf[idx + 3], 0, "角丸の外側(角)は完全透明のはず");
    }

    #[test]
    fn center_is_fully_opaque() {
        let size = 64u32;
        let buf = generate_icon_rgba(size);
        let cx = (size / 2) as usize;
        let cy = (size / 2) as usize;
        let idx = (cy * size as usize + cx) * 4;
        assert_eq!(buf[idx + 3], 255, "正方形中心は完全不透明のはず");
    }

    #[test]
    fn stroke_pixel_differs_from_plain_background_pixel() {
        let size = 64u32;
        let buf = generate_icon_rgba(size);
        // ストローク線分の中点付近。
        let sx = (size as f32 * 0.52) as usize;
        let sy = (size as f32 * 0.5) as usize;
        let stroke_idx = (sy * size as usize + sx) * 4;
        let stroke_rgb = [buf[stroke_idx], buf[stroke_idx + 1], buf[stroke_idx + 2]];

        // ストロークからもドリップからも離れた矩形内部の一点(左上寄り)。
        let bx = (size as f32 * 0.15) as usize;
        let by = (size as f32 * 0.15) as usize;
        let bg_idx = (by * size as usize + bx) * 4;
        let bg_rgb = [buf[bg_idx], buf[bg_idx + 1], buf[bg_idx + 2]];

        assert_ne!(stroke_rgb, bg_rgb, "ストローク上の画素は背景色と異なるはず");
    }

    #[test]
    fn all_requested_ico_sizes_produce_correctly_sized_opaque_and_transparent_pixels() {
        // examples/gen_icon.rs が生成する全サイズで、少なくとも 1 画素は
        // 完全不透明・1 画素は完全透明であること(潰れて単色にならない)。
        for size in [16u32, 24, 32, 48, 64, 128, 256] {
            let buf = generate_icon_rgba(size);
            assert_eq!(buf.len(), (size as usize) * (size as usize) * 4);
            let has_opaque = buf.chunks_exact(4).any(|p| p[3] == 255);
            let has_transparent = buf.chunks_exact(4).any(|p| p[3] == 0);
            assert!(has_opaque, "size={size} に不透明画素がない");
            assert!(has_transparent, "size={size} に透明画素がない");
        }
    }
}
