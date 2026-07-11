//! 選択・フローティング(SPEC §6、ARCHITECTURE.md §7)。
//!
//! `Selection`/`Floating` は ARCHITECTURE.md §7 が定めるデータ構造そのもの。
//! これらのライフサイクル(浮動化・移動・確定・削除)は `Document`/`History`
//! の両方に触れる必要があり、かつ複数フレームにまたがる状態(`app.rs` の
//! `select_drag`)を伴うため、`Tool` トレイトには乗せず(手のひら・スポイトと
//! 同様、`tools/mod.rs` のコメント参照)`app.rs` が直接オーケストレーション
//! する。このモジュールはその純粋な計算部分(矩形演算・画素の抽出/合成)だけを
//! 提供する。
//!
//! 座標変換や `Document` への読み書きは常に境界チェック済みの経路
//! (`Document::get_pixel`/`set_pixel`)を通し、キャンバス外へのはみ出し
//! (SPEC §6:「浮動片はキャンバス外にはみ出してよい(確定時にクリップ)」)
//! でパニックしないことを保証する。

use eframe::egui::{self, pos2, vec2, Pos2, Rect};

use crate::document::{Document, IRect};
use crate::raster;

/// 矩形選択(ARCHITECTURE.md §7)。
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub rect: IRect,
}

/// 浮動片(ARCHITECTURE.md §7、v2 §14.6 でスケールハンドルに対応)。
pub struct Floating {
    /// 現在の表示・合成に使うピクセル(拡縮ハンドルでリサイズされるたびに
    /// `original` から再サンプリングして置き換えられる、SPEC §16:
    /// 「累積劣化させない」)。
    pub pixels: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 画像座標(f32、キャンバス外もはみ出し可)。
    pub pos: Pos2,
    /// 浮動化時に透明で埋めた元領域(undo 一体化用、ARCHITECTURE.md §7)。
    /// クリップボードからの貼り付け(SPEC §6)で作られた浮動片は元領域を
    /// 持たないため `None`。
    ///
    /// `app.rs` の実装では、この情報を後から読み返す代わりに、浮動化した
    /// 瞬間(`begin_floating_from_selection`)に `History::ensure_tiles_saved`
    /// を直接呼んで元ピクセルを退避しているため、フィールド自体は読まれない
    /// (ARCHITECTURE.md §7 が定めるデータ構造としては保持する)。
    #[allow(dead_code)]
    pub cut_from: Option<IRect>,
    /// `canvas_view` がテクスチャをキャッシュ/再利用するための識別子。
    /// 生成時に一意な値を割り当てる。`pixels` の内容が変わったとき
    /// (ハンドルでリサイズされたとき、ARCHITECTURE.md §14.6)は
    /// 呼び出し側が新しい id を割り当てること — `canvas_view::draw_floating`
    /// は id が変わったときだけテクスチャを作り直す。
    pub id: u64,
    /// 浮動化した瞬間の元ピクセル(ARCHITECTURE.md §14.6: 「拡縮は浮動化時の
    /// 元ピクセルから毎回バイリニアで再サンプリングする(累積劣化させない)」)。
    /// `pixels`/`w`/`h` がハンドルドラッグで何度変わっても、この 3 フィールド
    /// (`original`/`orig_w`/`orig_h`)は生成後不変。
    pub original: Vec<u8>,
    pub orig_w: u32,
    pub orig_h: u32,
}

impl Floating {
    /// `pixels` を `original` としても保持する形で `Floating` を作る
    /// (通常の生成経路はすべてこれを通し、`original`/`orig_w`/`orig_h` を
    /// 手で書き忘れることを防ぐ)。
    pub fn new(
        pixels: Vec<u8>,
        w: u32,
        h: u32,
        pos: Pos2,
        cut_from: Option<IRect>,
        id: u64,
    ) -> Self {
        let original = pixels.clone();
        Self {
            pixels,
            w,
            h,
            pos,
            cut_from,
            id,
            original,
            orig_w: w,
            orig_h: h,
        }
    }
}

/// 2 点(ドラッグの始点・終点、画像座標)から半開区間の `IRect` を作る。
pub fn irect_from_points(a: Pos2, b: Pos2) -> IRect {
    IRect {
        x0: a.x.min(b.x).floor() as i32,
        y0: a.y.min(b.y).floor() as i32,
        x1: a.x.max(b.x).ceil() as i32,
        y1: a.y.max(b.y).ceil() as i32,
    }
}

/// 画像座標の点 `p` が半開区間の矩形 `rect` に含まれるか。
pub fn rect_contains(rect: IRect, p: Pos2) -> bool {
    p.x >= rect.x0 as f32 && p.x < rect.x1 as f32 && p.y >= rect.y0 as f32 && p.y < rect.y1 as f32
}

/// 浮動片が現在合成される先の矩形(`pos`/`w`/`h` から算出、画像境界への
/// クランプ前)。
pub fn floating_target_rect(floating: &Floating) -> IRect {
    let x0 = floating.pos.x.round() as i32;
    let y0 = floating.pos.y.round() as i32;
    IRect {
        x0,
        y0,
        x1: x0 + floating.w as i32,
        y1: y0 + floating.h as i32,
    }
}

/// `doc` の `rect` 領域を切り出す(境界内であること前提だが、呼び出し側が
/// `clamp_to` 済みでなくても範囲外は透明として扱いパニックしない)。
pub fn extract_region(doc: &Document, rect: IRect) -> Vec<u8> {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let px = doc
                .get_pixel(rect.x0 + x as i32, rect.y0 + y as i32)
                .unwrap_or([0, 0, 0, 0]);
            let idx = (y * w + x) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

/// `rect`(画像境界へクランプ)を透明で埋める(SPEC §6: 浮動化時の元領域の
/// クリア、および Delete での消去)。
pub fn clear_region_transparent(doc: &mut Document, rect: IRect) {
    let clipped = rect.clamp_to(doc.width, doc.height);
    if clipped.is_empty() {
        return;
    }
    for y in clipped.y0..clipped.y1 {
        for x in clipped.x0..clipped.x1 {
            doc.set_pixel(x, y, [0, 0, 0, 0]);
        }
    }
    doc.mark_dirty(clipped);
}

/// 浮動片を現在位置に合成する(SPEC §6:「浮動片をその位置に合成し」)。
/// straight-alpha の source-over(`raster::blend_over`)で合成し、キャンバス
/// 外にはみ出た部分は自動的にクリップされる。実際に触れた(クランプ後の)
/// 矩形を返す(`History::ensure_tiles_saved` は呼び出し側が先に行うこと)。
pub fn composite_floating(doc: &mut Document, floating: &Floating) -> IRect {
    let target = floating_target_rect(floating);
    let clipped = target.clamp_to(doc.width, doc.height);
    if clipped.is_empty() {
        return clipped;
    }
    let src_w = floating.w as usize;
    for y in clipped.y0..clipped.y1 {
        let sy = (y - target.y0) as usize;
        for x in clipped.x0..clipped.x1 {
            let sx = (x - target.x0) as usize;
            let idx = (sy * src_w + sx) * 4;
            let Some(src) = floating.pixels.get(idx..idx + 4) else {
                continue;
            };
            let src_px = [src[0], src[1], src[2], src[3]];
            let dst_px = doc.get_pixel(x, y).unwrap_or([0, 0, 0, 0]);
            doc.set_pixel(x, y, raster::blend_over(dst_px, src_px));
        }
    }
    doc.mark_dirty(clipped);
    clipped
}

// ---------------------------------------------------------------------------
// v2 §16 / ARCHITECTURE.md §14.6: 選択・浮動片のスケールハンドル。
// ---------------------------------------------------------------------------

/// SPEC §16: 「約 7pt 角」。スクリーン論理ポイント単位(ズームに関係なく
/// 一定の大きさで表示・判定する)。
pub const HANDLE_SIZE: f32 = 7.0;
/// SPEC §16: 「最小 1px、最大 8192px」。
pub const MIN_FLOATING_SIZE: f32 = 1.0;
pub const MAX_FLOATING_SIZE: f32 = 8192.0;

/// 選択矩形・浮動片の外周に出す 8 個のハンドル(四隅+各辺中央、SPEC §16)。
/// `ALL` は角ハンドルを先に並べる(`hit_handle` が重なった場合に角を優先する
/// ため、ARCHITECTURE.md §14.6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    Top,
    Right,
    Bottom,
    Left,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::TopRight,
        Handle::BottomRight,
        Handle::BottomLeft,
        Handle::Top,
        Handle::Right,
        Handle::Bottom,
        Handle::Left,
    ];

    /// このハンドル自身の矩形上の相対位置(0.0/0.5/1.0)。辺ハンドルは長辺
    /// 方向の成分が 0.5(中央)になる。`resize_floating_rect` はこれを使って
    /// 「0.5 の軸は動かさない(=辺ハンドルはその軸に沿ってのみ伸縮する)」を
    /// 判定する。
    pub fn fraction(self) -> (f32, f32) {
        match self {
            Handle::TopLeft => (0.0, 0.0),
            Handle::TopRight => (1.0, 0.0),
            Handle::BottomRight => (1.0, 1.0),
            Handle::BottomLeft => (0.0, 1.0),
            Handle::Top => (0.5, 0.0),
            Handle::Right => (1.0, 0.5),
            Handle::Bottom => (0.5, 1.0),
            Handle::Left => (0.0, 0.5),
        }
    }
}

/// `screen_rect`(選択/浮動片の外周、スクリーン論理ポイント座標)から
/// `Handle::ALL` と同じ順序で 8 個のハンドル矩形を求める。
pub fn handle_rects(screen_rect: Rect) -> [Rect; 8] {
    let mut out = [Rect::NOTHING; 8];
    for (i, handle) in Handle::ALL.iter().enumerate() {
        let (fx, fy) = handle.fraction();
        let center = pos2(
            screen_rect.min.x + fx * screen_rect.width(),
            screen_rect.min.y + fy * screen_rect.height(),
        );
        out[i] = Rect::from_center_size(center, vec2(HANDLE_SIZE, HANDLE_SIZE));
    }
    out
}

/// `pos`(スクリーン論理ポイント座標)がどのハンドルに当たっているか。
/// `handle_rects` と同じ順序(角優先)で判定するため、7pt 角のハンドル同士が
/// 小さい選択で重なっても角ハンドルが優先される
/// (ARCHITECTURE.md §14.9-6 と同じ「デッドゾーンを作らない」思想)。
pub fn hit_handle(handles: &[Rect; 8], pos: Pos2) -> Option<Handle> {
    for (i, handle) in Handle::ALL.iter().enumerate() {
        if handles[i].contains(pos) {
            return Some(*handle);
        }
    }
    None
}

/// ハンドルホバー/ドラッグ中に表示するリサイズカーソル(SPEC §16)。
pub fn handle_cursor(handle: Handle) -> egui::CursorIcon {
    match handle {
        Handle::TopLeft | Handle::BottomRight => egui::CursorIcon::ResizeNwSe,
        Handle::TopRight | Handle::BottomLeft => egui::CursorIcon::ResizeNeSw,
        Handle::Left | Handle::Right => egui::CursorIcon::ResizeHorizontal,
        Handle::Top | Handle::Bottom => egui::CursorIcon::ResizeVertical,
    }
}

/// ハンドルドラッグから新しい浮動片の矩形(画像座標)を求める純関数
/// (ARCHITECTURE.md §14.6、SPEC §16)。
///
/// - `anchor` はドラッグ開始時に固定した反対側の隅/辺(画像座標、SPEC §16:
///   「アンカーは反対側の隅/辺」)。
/// - `start_w`/`start_h`/`start_center` はドラッグ開始時点の浮動片の大きさ・
///   中心(Shift 縦横比固定時、動かない軸を中心基準で伸縮させるために使う)。
/// - `pointer` は現在のポインタ位置(画像座標)。
/// - `lock_aspect` は Shift 押下(SPEC §16: 「Shift で縦横比固定」)。
/// - 戻り値は `(新しい pos, 新しい w, 新しい h)`。w/h は `min_size..=max_size`
///   にクランプ済み。
#[allow(clippy::too_many_arguments)]
pub fn resize_floating_rect(
    handle: Handle,
    anchor: Pos2,
    start_w: f32,
    start_h: f32,
    start_center: Pos2,
    pointer: Pos2,
    lock_aspect: bool,
    min_size: f32,
    max_size: f32,
) -> (Pos2, f32, f32) {
    let (fx, fy) = handle.fraction();
    let x_free = fx != 0.5;
    let y_free = fy != 0.5;

    let mut new_w = if x_free {
        if fx > 0.5 {
            pointer.x - anchor.x
        } else {
            anchor.x - pointer.x
        }
    } else {
        start_w
    };
    let mut new_h = if y_free {
        if fy > 0.5 {
            pointer.y - anchor.y
        } else {
            anchor.y - pointer.y
        }
    } else {
        start_h
    };

    if lock_aspect && start_w > 0.0 && start_h > 0.0 {
        let ratio = start_w / start_h;
        if x_free && y_free {
            // 角ハンドル: どちらか変化の大きい方の軸に合わせて揃える。
            let scale = (new_w / start_w).max(new_h / start_h);
            new_w = start_w * scale;
            new_h = start_h * scale;
        } else if x_free {
            new_h = new_w / ratio;
        } else if y_free {
            new_w = new_h * ratio;
        }
    }

    new_w = new_w.clamp(min_size, max_size);
    new_h = new_h.clamp(min_size, max_size);

    let new_x = if x_free {
        if fx > 0.5 {
            anchor.x
        } else {
            anchor.x - new_w
        }
    } else {
        // 辺ハンドル(縦方向のみ自由)。Shift で縦横比が固定され幅も変わって
        // いる場合は中心を基準に伸縮する。変わっていなければ
        // `start_center.x - start_w / 2.0` は元の pos.x に一致する。
        start_center.x - new_w / 2.0
    };
    let new_y = if y_free {
        if fy > 0.5 {
            anchor.y
        } else {
            anchor.y - new_h
        }
    } else {
        start_center.y - new_h / 2.0
    };

    (pos2(new_x, new_y), new_w, new_h)
}

/// 浮動化時の元ピクセル `pixels`(`w`×`h`)から `new_w`×`new_h` へバイリニア
/// 再サンプリングする(ARCHITECTURE.md §14.6: 「累積劣化させない」ため、
/// 呼び出し側は常にこの `original` を起点に呼ぶこと。前回リサイズ後の
/// `pixels` から再度縮小拡大を重ねてはいけない)。
///
/// 出力が空(`new_w`/`new_h` が 0)なら空ベクタ、入力が空なら透明で埋める
/// (どちらもパニックしない、CLAUDE.md 鉄則)。
pub fn resample_bilinear(pixels: &[u8], w: u32, h: u32, new_w: u32, new_h: u32) -> Vec<u8> {
    if new_w == 0 || new_h == 0 {
        return Vec::new();
    }
    if w == 0 || h == 0 || pixels.len() < (w as usize * h as usize * 4) {
        return vec![0u8; new_w as usize * new_h as usize * 4];
    }

    let get = |x: i32, y: i32| -> [u8; 4] {
        let cx = x.clamp(0, w as i32 - 1) as usize;
        let cy = y.clamp(0, h as i32 - 1) as usize;
        let idx = (cy * w as usize + cx) * 4;
        [
            pixels[idx],
            pixels[idx + 1],
            pixels[idx + 2],
            pixels[idx + 3],
        ]
    };

    let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
    let scale_x = w as f32 / new_w as f32;
    let scale_y = h as f32 / new_h as f32;
    for ny in 0..new_h {
        let sy = ((ny as f32 + 0.5) * scale_y - 0.5).clamp(0.0, h as f32 - 1.0);
        let y0 = sy.floor() as i32;
        let y1 = y0 + 1;
        let fy = sy - y0 as f32;
        for nx in 0..new_w {
            let sx = ((nx as f32 + 0.5) * scale_x - 0.5).clamp(0.0, w as f32 - 1.0);
            let x0 = sx.floor() as i32;
            let x1 = x0 + 1;
            let fx = sx - x0 as f32;
            let p00 = get(x0, y0);
            let p10 = get(x1, y0);
            let p01 = get(x0, y1);
            let p11 = get(x1, y1);
            let mut px = [0u8; 4];
            for (c, slot) in px.iter_mut().enumerate() {
                let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
                let bottom = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                *slot = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
            let idx = (ny as usize * new_w as usize + nx as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;
    use eframe::egui::pos2;

    #[test]
    fn irect_from_points_normalizes_and_rounds_outward() {
        let r = irect_from_points(pos2(5.4, 5.9), pos2(1.1, 1.6));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (1, 1, 6, 6));
    }

    #[test]
    fn rect_contains_half_open_bounds() {
        let r = IRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        };
        assert!(rect_contains(r, pos2(0.0, 0.0)));
        assert!(rect_contains(r, pos2(9.9, 9.9)));
        assert!(!rect_contains(r, pos2(10.0, 5.0)));
        assert!(!rect_contains(r, pos2(-0.1, 5.0)));
    }

    #[test]
    fn extract_region_copies_expected_pixels() {
        let mut doc = Document::new(10, 10, Background::Transparent);
        doc.set_pixel(3, 4, [9, 9, 9, 255]);
        let rect = IRect {
            x0: 2,
            y0: 3,
            x1: 6,
            y1: 7,
        };
        let region = extract_region(&doc, rect);
        assert_eq!(region.len(), 4 * 4 * 4);
        // (3,4) はこの領域内の (1,1)。
        let idx = (1 * 4 + 1) * 4;
        assert_eq!(&region[idx..idx + 4], &[9, 9, 9, 255]);
    }

    #[test]
    fn clear_region_transparent_only_affects_rect() {
        let mut doc = Document::new(10, 10, Background::White);
        let rect = IRect {
            x0: 2,
            y0: 2,
            x1: 5,
            y1: 5,
        };
        clear_region_transparent(&mut doc, rect);
        assert_eq!(doc.get_pixel(3, 3), Some([0, 0, 0, 0]));
        assert_eq!(doc.get_pixel(0, 0), Some([255, 255, 255, 255]));
        assert_eq!(doc.get_pixel(6, 6), Some([255, 255, 255, 255]));
    }

    #[test]
    fn composite_floating_blends_over_existing_pixels() {
        let mut doc = Document::new(10, 10, Background::White);
        let floating = Floating::new(
            vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
            2,
            2,
            pos2(4.0, 4.0),
            None,
            1,
        );
        let touched = composite_floating(&mut doc, &floating);
        assert_eq!(
            (touched.x0, touched.y0, touched.x1, touched.y1),
            (4, 4, 6, 6)
        );
        assert_eq!(doc.get_pixel(4, 4), Some([255, 0, 0, 255]));
        assert_eq!(doc.get_pixel(0, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn composite_floating_clips_to_canvas_bounds() {
        let mut doc = Document::new(4, 4, Background::Transparent);
        let floating = Floating::new([1u8, 2, 3, 255].repeat(9), 3, 3, pos2(-1.0, -1.0), None, 2);
        let touched = composite_floating(&mut doc, &floating);
        // 画像は 0..4 x 0..4 なので、はみ出した左上は自動的にクリップされる。
        assert_eq!(
            (touched.x0, touched.y0, touched.x1, touched.y1),
            (0, 0, 2, 2)
        );
        assert_eq!(doc.get_pixel(0, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn floating_target_rect_uses_rounded_position() {
        let floating = Floating::new(vec![0; 4], 1, 1, pos2(2.6, 2.4), None, 3);
        let rect = floating_target_rect(&floating);
        assert_eq!((rect.x0, rect.y0), (3, 2));
    }

    // -- v2 §16: スケールハンドル(ARCHITECTURE.md §14.6 受け入れ基準) -------

    #[test]
    fn handle_rects_places_corners_and_edge_midpoints() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
        let handles = handle_rects(rect);
        // Handle::ALL = [TopLeft, TopRight, BottomRight, BottomLeft, Top, Right, Bottom, Left]
        assert_eq!(handles[0].center(), pos2(0.0, 0.0));
        assert_eq!(handles[1].center(), pos2(100.0, 0.0));
        assert_eq!(handles[2].center(), pos2(100.0, 50.0));
        assert_eq!(handles[3].center(), pos2(0.0, 50.0));
        assert_eq!(handles[4].center(), pos2(50.0, 0.0));
        assert_eq!(handles[5].center(), pos2(100.0, 25.0));
        assert_eq!(handles[6].center(), pos2(50.0, 50.0));
        assert_eq!(handles[7].center(), pos2(0.0, 25.0));
        for h in handles {
            assert_eq!(h.width(), HANDLE_SIZE);
            assert_eq!(h.height(), HANDLE_SIZE);
        }
    }

    #[test]
    fn hit_handle_finds_the_handle_under_the_pointer() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let handles = handle_rects(rect);
        assert_eq!(
            hit_handle(&handles, pos2(100.0, 100.0)),
            Some(Handle::BottomRight)
        );
        assert_eq!(hit_handle(&handles, pos2(50.0, 0.0)), Some(Handle::Top));
        assert_eq!(hit_handle(&handles, pos2(50.0, 50.0)), None);
    }

    #[test]
    fn hit_handle_prefers_corner_when_overlapping_on_a_tiny_rect() {
        // 選択が小さいと角ハンドル(7pt 角)と辺ハンドルが重なる。
        // Top 辺ハンドルの中心(1,0)は TopLeft/TopRight の当たり判定にも
        // 収まるため、角が優先されることを確認する(ARCHITECTURE.md §14.6)。
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(2.0, 2.0));
        let handles = handle_rects(rect);
        assert_eq!(hit_handle(&handles, pos2(1.0, 0.0)), Some(Handle::TopLeft));
    }

    #[test]
    fn resize_floating_rect_corner_drag_keeps_opposite_corner_fixed() {
        // BottomRight を (10,10)-(30,20) から (50,40) までドラッグ:
        // 左上(anchor)が固定され、右下が追従する。
        let (pos, w, h) = resize_floating_rect(
            Handle::BottomRight,
            pos2(10.0, 10.0),
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(50.0, 40.0),
            false,
            1.0,
            8192.0,
        );
        assert_eq!(pos, pos2(10.0, 10.0));
        assert_eq!(w, 40.0);
        assert_eq!(h, 30.0);
    }

    #[test]
    fn resize_floating_rect_top_left_drag_keeps_bottom_right_fixed() {
        let (pos, w, h) = resize_floating_rect(
            Handle::TopLeft,
            pos2(30.0, 20.0), // anchor = 元の右下
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(5.0, 5.0),
            false,
            1.0,
            8192.0,
        );
        assert_eq!(pos, pos2(5.0, 5.0));
        assert_eq!(w, 25.0);
        assert_eq!(h, 15.0);
    }

    #[test]
    fn resize_floating_rect_edge_handle_only_changes_one_axis() {
        // Right ハンドル: 高さ・y は変化しない(SPEC §16: 辺=単軸)。
        let (pos, w, h) = resize_floating_rect(
            Handle::Right,
            pos2(10.0, 10.0), // anchor = 左辺
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(50.0, 999.0), // y は無視されるはず
            false,
            1.0,
            8192.0,
        );
        assert_eq!(w, 40.0);
        assert_eq!(h, 10.0);
        assert_eq!(pos, pos2(10.0, 10.0));
    }

    #[test]
    fn resize_floating_rect_clamps_to_min_and_max_size() {
        let (_, w, h) = resize_floating_rect(
            Handle::BottomRight,
            pos2(10.0, 10.0),
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(10.5, 10.2), // ほぼアンカー上 -> 最小サイズにクランプ
            false,
            1.0,
            8192.0,
        );
        assert_eq!(w, 1.0);
        assert_eq!(h, 1.0);

        let (_, w2, h2) = resize_floating_rect(
            Handle::BottomRight,
            pos2(10.0, 10.0),
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(999_999.0, 999_999.0),
            false,
            1.0,
            8192.0,
        );
        assert_eq!(w2, 8192.0);
        assert_eq!(h2, 8192.0);
    }

    #[test]
    fn resize_floating_rect_shift_locks_aspect_ratio_on_corner_drag() {
        // 元の比率 2:1(20x10)。右下ハンドルを縦横不揃いにドラッグしても、
        // 比率が保たれること。
        let (_, w, h) = resize_floating_rect(
            Handle::BottomRight,
            pos2(10.0, 10.0),
            20.0,
            10.0,
            pos2(20.0, 15.0),
            pos2(50.0, 20.0), // 幅は+40相当、高さは+10相当(比率不揃い)
            true,
            1.0,
            8192.0,
        );
        assert!(
            (w / h - 2.0).abs() < 1e-4,
            "expected 2:1 aspect, got {w}x{h}"
        );
    }

    #[test]
    fn resize_floating_rect_shift_locks_aspect_ratio_on_edge_drag_and_centers_perpendicular_axis() {
        let start_center = pos2(20.0, 15.0);
        let (pos, w, h) = resize_floating_rect(
            Handle::Right,
            pos2(10.0, 10.0),
            20.0,
            10.0,
            start_center,
            pos2(50.0, 15.0),
            true,
            1.0,
            8192.0,
        );
        assert!(
            (w / h - 2.0).abs() < 1e-4,
            "expected 2:1 aspect, got {w}x{h}"
        );
        // 垂直方向は中心基準で伸縮する。
        assert!((pos.y + h / 2.0 - start_center.y).abs() < 1e-3);
    }

    #[test]
    fn resample_bilinear_upsize_preserves_flat_color() {
        let pixels = vec![10u8, 20, 30, 255].repeat(4); // 2x2 flat color
        let out = resample_bilinear(&pixels, 2, 2, 6, 6);
        assert_eq!(out.len(), 6 * 6 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn resample_bilinear_downsize_keeps_dimensions_correct() {
        let pixels = vec![1u8, 2, 3, 4].repeat(16); // 4x4 flat color
        let out = resample_bilinear(&pixels, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [1, 2, 3, 4]));
    }

    #[test]
    fn resample_bilinear_zero_output_size_does_not_panic() {
        let pixels = vec![1u8, 2, 3, 4].repeat(4);
        let out = resample_bilinear(&pixels, 2, 2, 0, 5);
        assert!(out.is_empty());
    }

    #[test]
    fn resample_bilinear_zero_input_size_does_not_panic() {
        let out = resample_bilinear(&[], 0, 0, 3, 3);
        assert_eq!(out.len(), 3 * 3 * 4);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn floating_new_captures_original_pixels_at_construction_time() {
        let floating = Floating::new(vec![1, 2, 3, 4], 1, 1, pos2(0.0, 0.0), None, 9);
        assert_eq!(floating.original, floating.pixels);
        assert_eq!((floating.orig_w, floating.orig_h), (1, 1));
    }
}
