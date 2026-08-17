//! アプリアイコン(SPEC §29、ARCHITECTURE.md §16.8)。
//!
//! 元絵は `assets/icon-256.png`。ここは**純粋関数**で任意サイズへ縮小し、
//! 角丸マスクをかけて RGBA を返す。
//! - `examples/gen_icon.rs` が ICO を書き出す
//! - `main.rs` がウィンドウ/タスクバーアイコンに同じ絵を渡す

const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// `size × size` の RGBA8(straight alpha)アイコン画素を生成する。
///
/// `size` は 0 でも(空の `Vec` を返すだけで)パニックしない。
/// 角は角丸マスクで透明になる。
pub fn generate_icon_rgba(size: u32) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }
    let Some(img) = image::load_from_memory(ICON_PNG).ok() else {
        return fallback_rounded(size);
    };
    let rgba = img.to_rgba8();
    let scaled = if rgba.width() == size && rgba.height() == size {
        rgba
    } else {
        image::imageops::resize(&rgba, size, size, image::imageops::FilterType::Triangle)
    };
    let mut buf = scaled.into_raw();
    apply_rounded_mask(&mut buf, size);
    buf
}

fn apply_rounded_mask(buf: &mut [u8], size: u32) {
    let s = size as f32;
    let half = s / 2.0;
    let radius = (s * 0.22).max(1.0).min(half);
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let cov = coverage_from_sdist(rounded_box_sdist(px - half, py - half, half, radius));
            let idx = (y as usize * size as usize + x as usize) * 4;
            let alpha = (buf[idx + 3] as f32 * cov).round().clamp(0.0, 255.0) as u8;
            buf[idx + 3] = alpha;
        }
    }
}

fn fallback_rounded(size: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (size as usize) * (size as usize) * 4];
    const BG: [u8; 3] = [0x18, 0x22, 0x34];
    let s = size as f32;
    let half = s / 2.0;
    let radius = (s * 0.22).max(1.0).min(half);
    for y in 0..size {
        for x in 0..size {
            let cov = coverage_from_sdist(rounded_box_sdist(
                x as f32 + 0.5 - half,
                y as f32 + 0.5 - half,
                half,
                radius,
            ));
            if cov <= 0.0 {
                continue;
            }
            let idx = (y as usize * size as usize + x as usize) * 4;
            buf[idx] = BG[0];
            buf[idx + 1] = BG[1];
            buf[idx + 2] = BG[2];
            buf[idx + 3] = (255.0 * cov).round().clamp(0.0, 255.0) as u8;
        }
    }
    buf
}

fn rounded_box_sdist(px: f32, py: f32, half: f32, radius: f32) -> f32 {
    let qx = px.abs() - half + radius;
    let qy = py.abs() - half + radius;
    qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - radius
}

fn coverage_from_sdist(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
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
        assert_eq!(buf[3], 0, "角丸の外側(角)は完全透明のはず");
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
        let sx = (size as f32 * 0.42) as usize;
        let sy = (size as f32 * 0.48) as usize;
        let stroke_idx = (sy * size as usize + sx) * 4;
        let stroke_rgb = [buf[stroke_idx], buf[stroke_idx + 1], buf[stroke_idx + 2]];

        let bx = (size as f32 * 0.16) as usize;
        let by = (size as f32 * 0.16) as usize;
        let bg_idx = (by * size as usize + bx) * 4;
        let bg_rgb = [buf[bg_idx], buf[bg_idx + 1], buf[bg_idx + 2]];

        assert_ne!(
            stroke_rgb, bg_rgb,
            "筆/ストローク上の画素は背景色と異なるはず"
        );
    }

    #[test]
    fn all_requested_ico_sizes_produce_correctly_sized_opaque_and_transparent_pixels() {
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
