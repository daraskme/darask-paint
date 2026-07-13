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

use crate::document::{Document, IRect, SelMask};
use crate::raster;

/// 選択(v4 §16.3/§21: マスク選択に一般化。矩形選択は「マスクが全 1 の
/// 矩形」として同じデータ構造に載る、ARCHITECTURE.md §16.10-1)。
///
/// `boundary` は `mask` から一度だけ計算した選択枠の境界線分
/// (ARCHITECTURE.md §16.3: 「選択変更時のみ再計算しキャッシュ」)。
/// `Selection` は生成後イミュータブル(常に丸ごと置き換えられる。フィールド
/// を個別に書き換える経路は無い)なので、`Selection::new` の時点で 1 回だけ
/// 計算しておけば毎フレームの再計算を避けられる。
#[derive(Clone)]
pub struct Selection {
    pub mask: SelMask,
    pub boundary: Vec<[Pos2; 2]>,
}

impl Selection {
    pub fn new(mask: SelMask) -> Self {
        let boundary = mask_boundary(&mask);
        Self { mask, boundary }
    }
}

/// 浮動片(ARCHITECTURE.md §7、v2 §14.6 でスケールハンドルに対応、
/// v4 §16.3 で非矩形マスクに対応)。
pub struct Floating {
    /// 現在の表示・合成に使うピクセル(拡縮ハンドルでリサイズされるたびに
    /// `original` から再サンプリングして置き換えられる、SPEC §16:
    /// 「累積劣化させない」)。
    pub pixels: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// `pixels` と同寸(`w*h` 要素、値は 0 か 255)。非矩形浮動片の合成に
    /// 使う(SPEC §21: 「浮動化・移動・スケールハンドル・…はマスク形状の
    /// まま動作する」)。v4 の全浮動片はここを経由するが、矩形選択・貼り付け・
    /// テキスト・移動ツール(選択なし)は全画素選択済みの全 255 マスクを持つ。
    pub mask: Vec<u8>,
    /// 画像座標(f32、キャンバス外もはみ出し可)。
    pub pos: Pos2,
    /// 浮動化時に切り出した元領域のマスク(undo 一体化用、ARCHITECTURE.md
    /// §7・§16.3)。クリップボードからの貼り付け(SPEC §6)で作られた浮動片は
    /// 元領域を持たないため `None`。
    ///
    /// v3 §18(Esc キャンセル、ARCHITECTURE.md §15.2)で `app.rs::
    /// cancel_floating` から読まれるようになった: `Some(mask)` なら、
    /// 浮動化した瞬間に `History::ensure_tiles_saved` で退避済みの CoW
    /// タイル(`History::restore_stroke_region`)から `mask.bbox` の元ピクセル
    /// を書き戻してから浮動片を破棄する(bbox 全体を復元しても、マスク外の
    /// 画素は浮動化時に一切変更していないため結果は同じ、かつ bbox 単位の
    /// 一括コピーの方が高速、v4 §16.1 のタイル一括コピーと同じ考え方)。
    /// `None`(クリップボード貼り付け)なら元に戻すべき領域自体が無いので、
    /// 単に浮動片を破棄するだけでよい。
    pub cut_from: Option<SelMask>,
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
    /// 浮動化した瞬間の元マスク(`original` と同じく不変。ハンドルでリサイズ
    /// するたびに `mask` はここから `resample_mask_nearest` で作り直す
    /// (累積劣化させない、`original`/`orig_w`/`orig_h` と対になる)。
    pub orig_mask: Vec<u8>,
}

impl Floating {
    /// `pixels`/`mask` をそれぞれ `original`/`orig_mask` としても保持する形で
    /// `Floating` を作る(通常の生成経路はすべてこれを通し、`original`系の
    /// フィールドを手で書き忘れることを防ぐ)。
    pub fn new(
        pixels: Vec<u8>,
        w: u32,
        h: u32,
        mask: Vec<u8>,
        pos: Pos2,
        cut_from: Option<SelMask>,
        id: u64,
    ) -> Self {
        let original = pixels.clone();
        let orig_mask = mask.clone();
        Self {
            pixels,
            w,
            h,
            mask,
            pos,
            cut_from,
            id,
            original,
            orig_w: w,
            orig_h: h,
            orig_mask,
        }
    }

    /// 全画素選択済み(矩形)の浮動片を作る便利コンストラクタ。v4 時点でも
    /// 矩形のまま浮動化する経路(クリップボード貼り付け・テキスト確定・
    /// 選択なしの移動ツール/自由変形)が使う(SPEC §21: 「既存の矩形選択は
    /// マスクが全 1 の矩形として同一コードパスに載せ替える」の浮動片版)。
    pub fn new_rect(
        pixels: Vec<u8>,
        w: u32,
        h: u32,
        pos: Pos2,
        cut_from: Option<SelMask>,
        id: u64,
    ) -> Self {
        let mask = vec![255u8; (w as usize) * (h as usize)];
        Self::new(pixels, w, h, mask, pos, cut_from, id)
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

/// 画像座標の点 `p` が `mask` で選択されている画素に含まれるか(v4 §16.3:
/// 「選択内部をドラッグ→浮動化」の判定を bbox だけでなく実際のマスク形状で
/// 行うための、`rect_contains` のマスク版)。矩形選択(全 1 マスク)では
/// `rect_contains(mask.bbox, p)` と完全に一致する(浮動小数の画素境界丸めは
/// どちらも同じ `floor` 相当になる)。
pub fn point_in_mask(mask: &SelMask, p: Pos2) -> bool {
    mask.contains(p.x.floor() as i32, p.y.floor() as i32)
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

/// `rect`(境界内であること前提だが、呼び出し側が `clamp_to` 済みでなくても
/// 範囲外は透明として扱いパニックしない)全体を選択済みとみなす
/// `SelMask`(SPEC §21: 「既存の矩形選択はマスクが全 1 の矩形として同一
/// コードパスに載せ替える」)。空矩形なら `SelMask::empty()`。
pub fn rect_mask(rect: IRect) -> SelMask {
    if rect.is_empty() {
        return SelMask::empty();
    }
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    SelMask {
        bbox: rect,
        mask: vec![255u8; w * h],
    }
}

/// `rect` に内接する楕円のマスク(SPEC §22: 「楕円選択」)。
/// `raster::fill_ellipse` と全く同じ判定式(`(x+0.5-cx)^2/rx^2 +
/// (y+0.5-cy)^2/ry^2 <= 1`)を使うため、同じ外接矩形の楕円図形と選択で
/// 見た目が一致する。`rx`/`ry` のどちらかが 0 以下(矩形が退化している)
/// なら空マスクを返す(パニックしない)。
pub fn ellipse_mask(rect: IRect) -> SelMask {
    if rect.is_empty() {
        return SelMask::empty();
    }
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut mask = vec![0u8; w * h];
    let cx = (rect.x0 + rect.x1) as f32 / 2.0;
    let cy = (rect.y0 + rect.y1) as f32 / 2.0;
    let rx = rect.width() as f32 / 2.0;
    let ry = rect.height() as f32 / 2.0;
    if rx > 0.0 && ry > 0.0 {
        for y in 0..h {
            let ny = (rect.y0 + y as i32) as f32 + 0.5 - cy;
            let ny = ny / ry;
            if ny.abs() > 1.0 {
                continue;
            }
            let row = y * w;
            for x in 0..w {
                let nx = ((rect.x0 + x as i32) as f32 + 0.5 - cx) / rx;
                if nx * nx + ny * ny <= 1.0 {
                    mask[row + x] = 255;
                }
            }
        }
    }
    SelMask { bbox: rect, mask }
}

/// 多角形(なげなわ、SPEC §22)のマスク。偶奇規則のスキャンライン法
/// (ARCHITECTURE.md §16.3: 「polygon_mask(偶奇規則スキャンライン)」)。
/// `points` は画像座標の頂点列で、**自動的に最後の点から最初の点へ閉じる**
/// (自由なげなわの軌跡・多角形なげなわの頂点列のどちらも、呼び出し側は
/// 明示的に閉じずにそのまま渡してよい)。頂点が 3 未満なら空マスク。
///
/// 各画素はその中心(`x+0.5, y+0.5`)がその行を横切る辺との交点の集合に
/// 対して奇数番目〜偶数番目の区間にあるかどうかで内外判定する(標準的な
/// 偶奇規則ポリゴン塗りつぶし。頂点をちょうど通る水平線での二重カウントを
/// 避けるため、辺の判定条件は `(a.y <= py) != (b.y <= py)` という片側閉区間
/// にしてある)。
pub fn polygon_mask(points: &[Pos2]) -> SelMask {
    if points.len() < 3 {
        return SelMask::empty();
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for p in points {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let bbox = IRect {
        x0: min_x.floor() as i32,
        y0: min_y.floor() as i32,
        x1: max_x.ceil() as i32,
        y1: max_y.ceil() as i32,
    };
    if bbox.is_empty() {
        return SelMask::empty();
    }
    let w = bbox.width() as usize;
    let h = bbox.height() as usize;
    let mut mask = vec![0u8; w * h];
    let n = points.len();
    let mut xs: Vec<f32> = Vec::new();
    for y in 0..h {
        let py = bbox.y0 as f32 + y as f32 + 0.5;
        xs.clear();
        for i in 0..n {
            let a = points[i];
            let b = points[(i + 1) % n];
            if (a.y <= py) != (b.y <= py) {
                let t = (py - a.y) / (b.y - a.y);
                xs.push(a.x + t * (b.x - a.x));
            }
        }
        xs.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
        let row = y * w;
        let mut inside = false;
        let mut xi = 0;
        for x in 0..w {
            let px = bbox.x0 as f32 + x as f32 + 0.5;
            while xi < xs.len() && xs[xi] <= px {
                inside = !inside;
                xi += 1;
            }
            if inside {
                mask[row + x] = 255;
            }
        }
    }
    SelMask { bbox, mask }
}

/// `mask` の `bbox` 領域を切り出す(v4 §16.3: 「浮動化: mask の画素だけ
/// 複写し」)。マスク外(`bbox` 内だが選択されていない画素)は透明のまま
/// 残す(矩形選択=全 1 マスクのときは従来どおり全画素を複写する)。境界内
/// であること前提だが、範囲外は透明として扱いパニックしない。
pub fn extract_region(doc: &Document, mask: &SelMask) -> Vec<u8> {
    let rect = mask.bbox;
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            if mask.mask.get(y * w + x).copied().unwrap_or(0) == 0 {
                continue;
            }
            let px = doc
                .get_pixel(rect.x0 + x as i32, rect.y0 + y as i32)
                .unwrap_or([0, 0, 0, 0]);
            let idx = (y * w + x) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

/// `mask` で選択されている画素だけを透明で埋める(v4 §16.3: 「元領域は
/// mask の画素だけ透明化」。SPEC §6: 浮動化時の元領域のクリア、および
/// Delete での消去。矩形選択=全 1 マスクのときは従来どおり矩形全体を
/// クリアする)。`mask.bbox` は画像境界へクランプしてから使う。
pub fn clear_region_transparent(doc: &mut Document, mask: &SelMask) {
    let clipped = mask.bbox.clamp_to(doc.width, doc.height);
    if clipped.is_empty() {
        return;
    }
    let mask_w = mask.bbox.width() as usize;
    for y in clipped.y0..clipped.y1 {
        let my = (y - mask.bbox.y0) as usize;
        for x in clipped.x0..clipped.x1 {
            let mx = (x - mask.bbox.x0) as usize;
            if mask.mask.get(my * mask_w + mx).copied().unwrap_or(0) == 0 {
                continue;
            }
            doc.set_pixel(x, y, [0, 0, 0, 0]);
        }
    }
    doc.mark_dirty(clipped);
}

/// 浮動片を現在位置に合成する(SPEC §6:「浮動片をその位置に合成し」)。
/// straight-alpha の source-over(`raster::blend_over`)で合成し、キャンバス
/// 外にはみ出た部分は自動的にクリップされる。`floating.mask` が 0 の画素は
/// 合成しない(v4 §16.3: 「確定合成も mask 経由」。矩形選択=全 1 マスクの
/// ときは従来どおりピクセルの alpha だけで自然にクリップされる)。実際に
/// 触れた(クランプ後の)矩形を返す(`History::ensure_tiles_saved` は
/// 呼び出し側が先に行うこと)。
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
            let midx = sy * src_w + sx;
            if floating.mask.get(midx).copied().unwrap_or(0) == 0 {
                continue;
            }
            let idx = midx * 4;
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

/// 選択枠の描画用境界線分(v4 §16.3: 「選択画素と非選択画素の境界」)。
/// 連続する画素境界を 1 本の線分にまとめる(1 画素ずつ別々の線分にすると
/// 破線の位相が画素ごとにリセットされて事実上ベタ塗りに見えてしまう上、
/// 巨大選択で線分数が爆発する)。矩形(全 1 マスク)なら必ずちょうど 4 本の
/// 線分になり、これまでの `draw_dashed_rect` の見た目と一致する。
///
/// 画素 `(x, y)` は画像座標で `[x, x+1) x [y, y+1)` を占めるものとして、
/// 4 近傍(bbox 外は「非選択」扱い)との境界を走査する
/// (ARCHITECTURE.md §16.10-9: 「境界線分抽出は選択確定時のみ」呼ぶ前提の
/// コスト — 呼び出し側は `Selection::new` で 1 回だけ計算してキャッシュする)。
pub fn mask_boundary(mask: &SelMask) -> Vec<[Pos2; 2]> {
    let mut segments = Vec::new();
    if mask.is_empty() {
        return segments;
    }
    let bbox = mask.bbox;
    let w = bbox.width();
    let h = bbox.height();
    let sel = |lx: i32, ly: i32| -> bool {
        if lx < 0 || ly < 0 || lx >= w || ly >= h {
            return false;
        }
        mask.mask[ly as usize * w as usize + lx as usize] != 0
    };

    // 水平方向の境界(上端・下端)は行ごとに走査し、連続する x 区間を 1 本に
    // まとめる。
    for ly in 0..h {
        push_runs(
            w,
            &mut segments,
            |lx| sel(lx, ly) && !sel(lx, ly - 1),
            |a, b| {
                let img_y = (bbox.y0 + ly) as f32;
                [
                    pos2((bbox.x0 + a) as f32, img_y),
                    pos2((bbox.x0 + b) as f32, img_y),
                ]
            },
        );
        push_runs(
            w,
            &mut segments,
            |lx| sel(lx, ly) && !sel(lx, ly + 1),
            |a, b| {
                let img_y = (bbox.y0 + ly + 1) as f32;
                [
                    pos2((bbox.x0 + a) as f32, img_y),
                    pos2((bbox.x0 + b) as f32, img_y),
                ]
            },
        );
    }
    // 垂直方向の境界(左端・右端)は列ごとに走査する。
    for lx in 0..w {
        push_runs(
            h,
            &mut segments,
            |ly| sel(lx, ly) && !sel(lx - 1, ly),
            |a, b| {
                let img_x = (bbox.x0 + lx) as f32;
                [
                    pos2(img_x, (bbox.y0 + a) as f32),
                    pos2(img_x, (bbox.y0 + b) as f32),
                ]
            },
        );
        push_runs(
            h,
            &mut segments,
            |ly| sel(lx, ly) && !sel(lx + 1, ly),
            |a, b| {
                let img_x = (bbox.x0 + lx + 1) as f32;
                [
                    pos2(img_x, (bbox.y0 + a) as f32),
                    pos2(img_x, (bbox.y0 + b) as f32),
                ]
            },
        );
    }
    segments
}

/// `mask_boundary` の内部ヘルパー: `0..len` を `is_edge` で走査し、連続する
/// `true` の区間ごとに `make_segment(start, end)` を 1 回呼んで `out` に積む。
fn push_runs(
    len: i32,
    out: &mut Vec<[Pos2; 2]>,
    mut is_edge: impl FnMut(i32) -> bool,
    make_segment: impl Fn(i32, i32) -> [Pos2; 2],
) {
    let mut run_start: Option<i32> = None;
    for i in 0..=len {
        let edge = i < len && is_edge(i);
        match (edge, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                out.push(make_segment(s, i));
                run_start = None;
            }
            _ => {}
        }
    }
}

/// ハンドル拡縮時にマスクを再サンプリングする(SPEC §16/v4 §16.3: 「ピクセル
/// は bilinear、マスクは nearest」。マスクは 0/255 の 2 値なので nearest で
/// ボケさせない)。`resample_bilinear` と同じ零サイズガード(パニックしない)。
pub fn resample_mask_nearest(mask: &[u8], w: u32, h: u32, new_w: u32, new_h: u32) -> Vec<u8> {
    if new_w == 0 || new_h == 0 {
        return Vec::new();
    }
    if w == 0 || h == 0 || mask.len() < (w as usize * h as usize) {
        return vec![0u8; new_w as usize * new_h as usize];
    }
    let mut out = vec![0u8; new_w as usize * new_h as usize];
    let scale_x = w as f32 / new_w as f32;
    let scale_y = h as f32 / new_h as f32;
    for ny in 0..new_h {
        let sy = (((ny as f32 + 0.5) * scale_y).floor()).clamp(0.0, h as f32 - 1.0) as usize;
        for nx in 0..new_w {
            let sx = (((nx as f32 + 0.5) * scale_x).floor()).clamp(0.0, w as f32 - 1.0) as usize;
            out[ny as usize * new_w as usize + nx as usize] = mask[sy * w as usize + sx];
        }
    }
    out
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
    fn point_in_mask_matches_rect_contains_for_a_rect_mask() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        };
        let mask = rect_mask(rect);
        for p in [
            pos2(0.0, 0.0),
            pos2(9.9, 9.9),
            pos2(10.0, 5.0),
            pos2(-0.1, 5.0),
        ] {
            assert_eq!(
                point_in_mask(&mask, p),
                rect_contains(rect, p),
                "mismatch at {p:?}"
            );
        }
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
        let region = extract_region(&doc, &rect_mask(rect));
        assert_eq!(region.len(), 4 * 4 * 4);
        // (3,4) はこの領域内の (1,1)。
        let (row, col, width) = (1usize, 1usize, 4usize);
        let idx = (row * width + col) * 4;
        assert_eq!(&region[idx..idx + 4], &[9, 9, 9, 255]);
    }

    #[test]
    fn extract_region_masked_leaves_unselected_pixels_transparent() {
        // v4 §16.3: 「mask の画素だけ複写」。矩形の左半分だけ選択された
        // マスクなら、右半分は(元画素が不透明でも)透明で埋まっているはず。
        let mut doc = Document::new(10, 10, Background::White);
        let bbox = IRect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 2,
        };
        let mut mask = vec![255u8; 8];
        for y in 0..2usize {
            mask[y * 4 + 2] = 0;
            mask[y * 4 + 3] = 0;
        }
        let sel = SelMask { bbox, mask };
        let region = extract_region(&doc, &sel);
        assert_eq!(&region[0..4], &[255, 255, 255, 255]); // (0,0) 選択済み
        assert_eq!(&region[8..12], &[0, 0, 0, 0]); // (2,0) 非選択
        let _ = &mut doc; // 未使用警告回避(get_pixel を経由しないため)。
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
        clear_region_transparent(&mut doc, &rect_mask(rect));
        assert_eq!(doc.get_pixel(3, 3), Some([0, 0, 0, 0]));
        assert_eq!(doc.get_pixel(0, 0), Some([255, 255, 255, 255]));
        assert_eq!(doc.get_pixel(6, 6), Some([255, 255, 255, 255]));
    }

    #[test]
    fn clear_region_transparent_masked_only_clears_selected_pixels() {
        let mut doc = Document::new(10, 10, Background::White);
        let bbox = IRect {
            x0: 2,
            y0: 2,
            x1: 4,
            y1: 3,
        };
        // (2,2) だけ選択、(3,2) は非選択。
        let sel = SelMask {
            bbox,
            mask: vec![255, 0],
        };
        clear_region_transparent(&mut doc, &sel);
        assert_eq!(doc.get_pixel(2, 2), Some([0, 0, 0, 0]));
        assert_eq!(doc.get_pixel(3, 2), Some([255, 255, 255, 255]));
    }

    #[test]
    fn composite_floating_blends_over_existing_pixels() {
        let mut doc = Document::new(10, 10, Background::White);
        let floating = Floating::new_rect(
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
        let floating =
            Floating::new_rect([1u8, 2, 3, 255].repeat(9), 3, 3, pos2(-1.0, -1.0), None, 2);
        let touched = composite_floating(&mut doc, &floating);
        // 画像は 0..4 x 0..4 なので、はみ出した左上は自動的にクリップされる。
        assert_eq!(
            (touched.x0, touched.y0, touched.x1, touched.y1),
            (0, 0, 2, 2)
        );
        assert_eq!(doc.get_pixel(0, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn composite_floating_skips_pixels_outside_mask() {
        // v4 §16.3: 「確定合成も mask 経由」。マスク外の画素は、たとえ
        // pixels 側に不透明な値が入っていても合成されない。
        let mut doc = Document::new(4, 4, Background::White);
        let pixels = [9u8, 9, 9, 255].repeat(4); // 2x2 全画素不透明
        let mask = vec![255, 0, 0, 255]; // 左上・右下だけ選択
        let floating = Floating::new(pixels, 2, 2, mask, pos2(0.0, 0.0), None, 1);
        composite_floating(&mut doc, &floating);
        assert_eq!(doc.get_pixel(0, 0), Some([9, 9, 9, 255]));
        assert_eq!(
            doc.get_pixel(1, 0),
            Some([255, 255, 255, 255]),
            "masked-out pixel must not be composited even though its source pixel is opaque"
        );
        assert_eq!(doc.get_pixel(1, 1), Some([9, 9, 9, 255]));
    }

    #[test]
    fn floating_target_rect_uses_rounded_position() {
        let floating = Floating::new_rect(vec![0; 4], 1, 1, pos2(2.6, 2.4), None, 3);
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
        let pixels = [10u8, 20, 30, 255].repeat(4); // 2x2 flat color
        let out = resample_bilinear(&pixels, 2, 2, 6, 6);
        assert_eq!(out.len(), 6 * 6 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn resample_bilinear_downsize_keeps_dimensions_correct() {
        let pixels = [1u8, 2, 3, 4].repeat(16); // 4x4 flat color
        let out = resample_bilinear(&pixels, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [1, 2, 3, 4]));
    }

    #[test]
    fn resample_bilinear_zero_output_size_does_not_panic() {
        let pixels = [1u8, 2, 3, 4].repeat(4);
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
        let floating = Floating::new_rect(vec![1, 2, 3, 4], 1, 1, pos2(0.0, 0.0), None, 9);
        assert_eq!(floating.original, floating.pixels);
        assert_eq!((floating.orig_w, floating.orig_h), (1, 1));
        assert_eq!(floating.orig_mask, floating.mask);
        assert_eq!(floating.mask, vec![255u8]);
    }

    // -- v4 §16.3/§21: マスク選択の純関数(ARCHITECTURE.md §16.3) -------------

    #[test]
    fn rect_mask_is_all_255_over_the_rect_area() {
        let rect = IRect {
            x0: 1,
            y0: 2,
            x1: 4,
            y1: 5,
        };
        let m = rect_mask(rect);
        assert_eq!(m.bbox, rect);
        assert_eq!(m.mask.len(), 3 * 3);
        assert!(m.mask.iter().all(|&v| v == 255));
        assert!(m.contains(2, 3));
        assert!(!m.contains(0, 0), "outside bbox must not be selected");
    }

    #[test]
    fn rect_mask_of_empty_rect_is_empty() {
        let rect = IRect {
            x0: 5,
            y0: 5,
            x1: 5,
            y1: 5,
        };
        let m = rect_mask(rect);
        assert!(m.is_empty());
        assert!(m.mask.is_empty());
    }

    #[test]
    fn selmask_get_and_contains_are_false_outside_bbox() {
        let m = rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 4,
            y1: 4,
        });
        assert_eq!(m.get(2, 2), 255);
        assert_eq!(m.get(10, 10), 0);
        assert_eq!(m.get(-1, -1), 0);
        assert!(!m.contains(10, 10));
    }

    #[test]
    fn selmask_clamp_to_reindexes_a_shrunk_bbox() {
        // 選択の一部だけがドキュメント範囲内に残る状況(防御的な安全弁)。
        let bbox = IRect {
            x0: -2,
            y0: 0,
            x1: 2,
            y1: 2,
        };
        // 左半分(x<0)だけ非選択、右半分(x>=0)は選択、というマスクを作る。
        let mut mask = vec![0u8; 4 * 2];
        for y in 0..2usize {
            mask[y * 4 + 2] = 255;
            mask[y * 4 + 3] = 255;
        }
        let sel = SelMask { bbox, mask };
        let clamped = sel.clamp_to(10, 10);
        assert_eq!(
            (
                clamped.bbox.x0,
                clamped.bbox.y0,
                clamped.bbox.x1,
                clamped.bbox.y1
            ),
            (0, 0, 2, 2)
        );
        assert!(clamped.contains(0, 0));
        assert!(clamped.contains(1, 1));
    }

    #[test]
    fn selmask_clamp_to_zero_size_document_is_empty() {
        let sel = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 5,
        });
        let clamped = sel.clamp_to(0, 0);
        assert!(clamped.is_empty());
    }

    #[test]
    fn mask_boundary_of_a_rect_mask_is_exactly_four_segments() {
        // SPEC §21: 既存の矩形選択はマスクが全 1 の矩形として同一コード
        // パスに載る。矩形の境界は上下左右の 4 本の(連続した)線分に
        // まとまるはず(1 画素ごとに分割されない)。
        let rect = IRect {
            x0: 2,
            y0: 3,
            x1: 12,
            y1: 8,
        };
        let segments = mask_boundary(&rect_mask(rect));
        assert_eq!(
            segments.len(),
            4,
            "expected exactly 4 merged edge segments for a rectangular mask, got {}",
            segments.len()
        );
        // 4 本の線分の合計長は矩形の周長に一致するはず。
        let total_len: f32 = segments.iter().map(|[a, b]| (*b - *a).length()).sum();
        let perimeter = 2.0 * (rect.width() + rect.height()) as f32;
        assert!((total_len - perimeter).abs() < 1e-3);
    }

    #[test]
    fn mask_boundary_of_a_full_4000x4000_selection_is_correct_and_terminates_quickly() {
        // ARCHITECTURE.md §16.10-9: 「楕円/多角形マスクの境界線分抽出は選択
        // 確定時のみ(毎フレーム再計算しない)。巨大選択(4000×4000 全選択)
        // でも境界抽出 < 50ms」。`cargo test` はデバッグビルドのため、実際の
        // 50ms 目標そのものではなく緩い上限で回帰検知だけを行う(raster.rs の
        // flood_fill 4000x4000 テストと同じ方針)。Ctrl+A の実行経路
        // (`app.rs::select_all` → `Selection::new` → `mask_boundary`)を模す。
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 4000,
            y1: 4000,
        };
        let mask = rect_mask(rect);
        let start = std::time::Instant::now();
        let segments = mask_boundary(&mask);
        let elapsed = start.elapsed();
        assert_eq!(segments.len(), 4);
        assert!(
            elapsed.as_secs() < 10,
            "mask_boundary took suspiciously long on a full 4000x4000 selection \
             (possible regression): {elapsed:?}"
        );
    }

    #[test]
    fn mask_boundary_of_empty_mask_is_empty() {
        assert!(mask_boundary(&SelMask::empty()).is_empty());
    }

    #[test]
    fn mask_boundary_handles_an_l_shape_without_panicking() {
        // 非矩形マスク(L 字)でも境界抽出がパニックせず、選択画素の総周長と
        // 一致する本数の情報を返すことだけを確認する(具体的な巡回順序は
        // 問わない)。
        // 2x2 のうち (1,1) だけ非選択の L 字。
        let bbox = IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        };
        let mask = vec![255, 255, 255, 0];
        let sel = SelMask { bbox, mask };
        let segments = mask_boundary(&sel);
        assert!(!segments.is_empty());
        // 各線分は水平か垂直のどちらか(斜めは無い)。
        for [a, b] in &segments {
            assert!(
                (a.x - b.x).abs() < 1e-6 || (a.y - b.y).abs() < 1e-6,
                "boundary segments must be axis-aligned, got {a:?}-{b:?}"
            );
        }
    }

    #[test]
    fn resample_mask_nearest_upsize_preserves_binary_values() {
        let mask = vec![255u8, 0, 0, 255]; // 2x2 チェッカー
        let out = resample_mask_nearest(&mask, 2, 2, 4, 4);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&v| v == 0 || v == 255));
    }

    #[test]
    fn resample_mask_nearest_zero_output_size_does_not_panic() {
        let mask = vec![255u8; 4];
        let out = resample_mask_nearest(&mask, 2, 2, 0, 5);
        assert!(out.is_empty());
    }

    #[test]
    fn resample_mask_nearest_zero_input_size_does_not_panic() {
        let out = resample_mask_nearest(&[], 0, 0, 3, 3);
        assert_eq!(out.len(), 9);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn selection_new_precomputes_boundary_matching_mask_boundary() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 5,
        };
        let mask = rect_mask(rect);
        let expected = mask_boundary(&mask);
        let selection = Selection::new(mask);
        assert_eq!(selection.boundary.len(), expected.len());
    }

    // -- V4-M3/SPEC §22: 楕円選択・なげなわの純関数 -----------------------

    #[test]
    fn ellipse_mask_selects_center_but_not_corner() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        };
        let m = ellipse_mask(rect);
        assert_eq!(m.bbox, rect);
        assert!(
            m.contains(5, 5),
            "center of inscribed circle must be selected"
        );
        assert!(
            !m.contains(0, 0),
            "corner of the bounding box must be outside the inscribed circle"
        );
        assert!(
            !m.contains(9, 9),
            "opposite corner must also be outside the circle"
        );
    }

    #[test]
    fn ellipse_mask_is_symmetric_for_a_square_bbox() {
        // 正円(Shift ドラッグ相当)なら左右・上下対称であるはず。
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        };
        let m = ellipse_mask(rect);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    m.contains(x, y),
                    m.contains(7 - x, y),
                    "expected horizontal symmetry at ({x},{y})"
                );
                assert_eq!(
                    m.contains(x, y),
                    m.contains(x, 7 - y),
                    "expected vertical symmetry at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn ellipse_mask_of_empty_rect_is_empty() {
        let m = ellipse_mask(IRect {
            x0: 3,
            y0: 3,
            x1: 3,
            y1: 3,
        });
        assert!(m.is_empty());
    }

    #[test]
    fn ellipse_mask_matches_raster_fill_ellipse_inclusion() {
        // ellipse_mask は raster::fill_ellipse と同じ判定式を使うため、
        // 同じ外接矩形なら同じ画素集合になるはず(SPEC §22 の見た目一致)。
        use crate::raster::{self, Surface};
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 11,
            y1: 7,
        };
        let mask = ellipse_mask(rect);
        let w = rect.width() as u32;
        let h = rect.height() as u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let mut surface = Surface {
            width: w,
            height: h,
            pixels: &mut pixels,
            clip: None,
        };
        raster::fill_ellipse(
            &mut surface,
            (0.0, 0.0, w as f32, h as f32),
            [255, 0, 0, 255],
        );
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let painted = surface.get_pixel(x, y) == Some([255, 0, 0, 255]);
                assert_eq!(
                    mask.contains(x, y),
                    painted,
                    "mismatch at ({x},{y}) between ellipse_mask and fill_ellipse"
                );
            }
        }
    }

    #[test]
    fn polygon_mask_of_an_axis_aligned_square_matches_rect_mask() {
        let points = [
            pos2(0.0, 0.0),
            pos2(4.0, 0.0),
            pos2(4.0, 4.0),
            pos2(0.0, 4.0),
        ];
        let poly = polygon_mask(&points);
        let rect = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        });
        assert_eq!(poly.bbox, rect.bbox);
        assert_eq!(poly.mask, rect.mask);
    }

    #[test]
    fn polygon_mask_of_a_triangle_selects_interior_not_far_corner() {
        // 直角三角形 (0,0)-(6,0)-(0,6): 内部は概ね x+y <= 6。
        let points = [pos2(0.0, 0.0), pos2(6.0, 0.0), pos2(0.0, 6.0)];
        let m = polygon_mask(&points);
        assert!(
            m.contains(1, 1),
            "near the right-angle corner must be inside"
        );
        assert!(
            !m.contains(5, 5),
            "far corner of the bounding box must be outside the triangle"
        );
    }

    #[test]
    fn polygon_mask_auto_closes_from_last_point_to_first() {
        // 最後の点を明示的に始点へ戻さなくても、自動的に閉じたものとして
        // 扱われる(自由なげなわの軌跡は開いたままでよい)。
        let closed = [
            pos2(0.0, 0.0),
            pos2(5.0, 0.0),
            pos2(5.0, 5.0),
            pos2(0.0, 5.0),
            pos2(0.0, 0.0),
        ];
        let open = [
            pos2(0.0, 0.0),
            pos2(5.0, 0.0),
            pos2(5.0, 5.0),
            pos2(0.0, 5.0),
        ];
        assert_eq!(polygon_mask(&closed).mask, polygon_mask(&open).mask);
    }

    #[test]
    fn polygon_mask_fewer_than_three_points_is_empty() {
        assert!(polygon_mask(&[]).is_empty());
        assert!(polygon_mask(&[pos2(0.0, 0.0)]).is_empty());
        assert!(polygon_mask(&[pos2(0.0, 0.0), pos2(1.0, 1.0)]).is_empty());
    }

    #[test]
    fn polygon_mask_degenerate_zero_area_does_not_panic() {
        // 全点が同一直線上(面積 0)でもパニックしない。
        let points = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(2.0, 0.0)];
        let m = polygon_mask(&points);
        // 面積 0 なので何も選択されなくてよいが、パニックしないことが主眼。
        let _ = m.mask.iter().any(|&v| v != 0);
    }
}
