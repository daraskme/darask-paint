//! 低レベルラスタ演算(ARCHITECTURE.md §5, v2: §14.1)。
//!
//! すべて純関数的(`Surface` とプリミティブ引数のみ)。M2 で `stamp_round` /
//! `stroke_segment` / `blend_over` を実装し、M3 で直線・矩形・楕円・
//! flood fill・アンチエイリアス用のカバレッジ計算を追加した。境界矩形は
//! `history.rs` がストローク前のタイル退避に、呼び出し側の `Document::mark_dirty`
//! がテクスチャ部分更新にそれぞれ使う。
//!
//! v2 (ARCHITECTURE.md §14.1)で `Document`(レイヤーを持つ)への直接依存を
//! やめ、`Surface`(幅・高さ・ピクセルバッファへの可変参照)を受け取る形に
//! リファクタした。**raster.rs はレイヤーを一切知らない**: 呼び出し側
//! (tools/*)がアクティブレイヤーのバッファを `Surface` として渡す。
//! 各関数は「実際に触れた(境界クランプ済みの)矩形」を返すようになった
//! (以前は `Document::mark_dirty` を内部で直接呼んでいたが、`Document` を
//! 知らなくなったため、dirty のマージは呼び出し側の責務になった)。
//!
//! ピクセルアクセスは常に `Surface::get_pixel`/`set_pixel` 経由で行い、
//! 境界外書き込みでパニックしないという方針(CLAUDE.md 鉄則)を守る。

use crate::document::IRect;

/// v2 §14.1: raster.rs が操作する対象。呼び出し側(tools/*)がアクティブ
/// レイヤーのピクセルバッファをこれに包んで渡す。`Document`/`Layer` を
/// 一切参照しないことで、raster.rs をレイヤー概念から独立させる。
pub struct Surface<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a mut [u8],
}

impl<'a> Surface<'a> {
    /// `(x, y)` のピクセル値を読む。範囲外なら `None`(パニックしない)。
    pub fn get_pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        self.pixels
            .get(idx..idx + 4)
            .map(|s| [s[0], s[1], s[2], s[3]])
    }

    /// `(x, y)` にピクセル値を書く。範囲外なら何もしない(パニックしない)。
    pub fn set_pixel(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        if let Some(slice) = self.pixels.get_mut(idx..idx + 4) {
            slice.copy_from_slice(&color);
        }
    }

    /// `flood_fill` の `before_write` コールバックが CoW タイル退避のために
    /// 読み取り専用でバッファ全体を見るための借用(`&mut self` を経由せず
    /// `&self` から取れるようにする)。
    pub fn as_slice(&self) -> &[u8] {
        &self.pixels[..]
    }
}

/// `stamp_round` が実際に触れうる矩形(画像境界へのクランプ前)。
/// ストローク開始前のタイル退避(`history::History::ensure_tiles_saved`)に
/// 使うため、実際にピクセルを書く前に呼べる純関数として独立させてある。
pub fn stamp_bounds(cx: f32, cy: f32, radius: f32) -> IRect {
    let r = radius.max(0.0);
    IRect {
        x0: (cx - r).floor() as i32,
        y0: (cy - r).floor() as i32,
        x1: (cx + r).ceil() as i32 + 1,
        y1: (cy + r).ceil() as i32 + 1,
    }
}

/// `stroke_segment` が実際に触れうる矩形(画像境界へのクランプ前)。
/// 線分上のどのスタンプも、始点・終点の外接矩形を半径ぶん広げた矩形の
/// 内側に収まる。
pub fn segment_bounds(from: (f32, f32), to: (f32, f32), radius: f32) -> IRect {
    stamp_bounds(from.0, from.1, radius).union(&stamp_bounds(to.0, to.1, radius))
}

/// ハードエッジ判定(`stamp_round`/`stamp_pencil_coverage` 共通)で保証する
/// 実効最小半径(`tools/mod.rs` の「1px ブラシでも何かしら塗れるよう最小
/// 半径を設ける」という意図、`MIN_BRUSH_SIZE`=1.0 → 最小半径 0.5 に対応)。
///
/// 画素中心 `(x+0.5, y+0.5)` は、クリック位置(任意の点)から最大で
/// `√2/2 ≈ 0.7071`(画素セルの対角線の半分)離れうる。半径がこれ未満だと、
/// 画素境界の交点付近(面積比で約 21%)をクリックしたとき最寄り画素の中心
/// にすら届かず 1 画素も塗られない — それでも `stamp_bounds` は非空なので、
/// 何も描かれないクリックで無意味な undo 単位・`modified` フラグだけが
/// 立ってしまう。`stamp_soft_coverage` の `outer = r.max(inner + 0.5)` と
/// 同じ考え方の下駄。`√2/2 ≈ 0.70710678` そのものだと最寄り画素中心が
/// ちょうど境界上(`dist == radius`)になり f32 の丸め誤差で等号判定が
/// 揺れうるため、わずかに大きい値にして安全マージンを持たせる。
const MIN_HARD_EDGE_RADIUS: f32 = 0.708;

/// ハードエッジ判定に使う実効半径(`MIN_HARD_EDGE_RADIUS` 未満を底上げする)。
/// `stamp_bounds`(境界矩形の計算)は据え置いてよい — 境界は `ceil(...)+1`
/// の余裕を持たせてあるため、この下駄で実効半径が最大 `√2/2` まで増えても
/// 既存の矩形計算で十分にカバーされる(raster.rs のテストで確認)。
fn hard_edge_radius(radius: f32) -> f32 {
    radius.max(0.0).max(MIN_HARD_EDGE_RADIUS)
}

/// ハードエッジの丸筆スタンプ。`erase` なら alpha=0(透明)を書く。
/// 中心 `(cx, cy)` は画像ピクセル座標(浮動小数)。ピクセル中心
/// `(x+0.5, y+0.5)` と中心の距離が `radius` 以下なら塗る(`radius` が
/// `MIN_HARD_EDGE_RADIUS` 未満のときは底上げする、上記コメント参照)。
/// 実際に触れた(境界クランプ済みの)矩形を返す(呼び出し側が `dirty` に
/// マージする)。
pub fn stamp_round(
    surface: &mut Surface,
    cx: f32,
    cy: f32,
    radius: f32,
    color: [u8; 4],
    erase: bool,
) -> IRect {
    let bounds = stamp_bounds(cx, cy, radius).clamp_to(surface.width, surface.height);
    if bounds.is_empty() {
        return bounds;
    }
    let r2 = hard_edge_radius(radius).powi(2);
    let write = if erase { [0, 0, 0, 0] } else { color };
    for y in bounds.y0..bounds.y1 {
        for x in bounds.x0..bounds.x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                surface.set_pixel(x, y, write);
            }
        }
    }
    bounds
}

/// 線分 `from` → `to` に沿って `stamp_round` を並べて塗る
/// (ARCHITECTURE.md §5: 間隔 ≤ max(1px, radius/2))。
/// ポインタイベント間の間隔が開いても線が途切れないようにするための関数
/// (SPEC §4 のペン/消しゴムの挙動)。触れた矩形の厳密な合併
/// (`segment_bounds` と一致する、raster.rs のテストで検証)を返す。
pub fn stroke_segment(
    surface: &mut Surface,
    from: (f32, f32),
    to: (f32, f32),
    radius: f32,
    color: [u8; 4],
    erase: bool,
) -> IRect {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let dist = (dx * dx + dy * dy).sqrt();
    let step = (radius / 2.0).max(1.0);
    let steps = (dist / step).ceil().max(1.0) as u32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = from.0 + dx * t;
        let y = from.1 + dy * t;
        stamp_round(surface, x, y, radius, color, erase);
    }
    segment_bounds(from, to, radius).clamp_to(surface.width, surface.height)
}

/// straight-alpha の source-over 合成(ARCHITECTURE.md §5)。
/// `dst`/`src`/戻り値はいずれも straight alpha の RGBA8。
/// ペンの AA モード(tools/pen.rs)や v2 のレイヤー合成
/// (`Document::recomposite`)が使う。
pub fn blend_over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let src_a = src[3] as f32 / 255.0;
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let mix = |s: u8, d: u8| -> u8 {
        let s = s as f32 / 255.0;
        let d = d as f32 / 255.0;
        let out = (s * src_a + d * dst_a * (1.0 - src_a)) / out_a;
        (out * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [
        mix(src[0], dst[0]),
        mix(src[1], dst[1]),
        mix(src[2], dst[2]),
        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

// ---------------------------------------------------------------------------
// v3 §17/ARCHITECTURE.md §15.1: ブラシ/消しゴム共通ストロークエンジンの
// カバレッジ計算(tools/brush.rs が使う)。
// ---------------------------------------------------------------------------

/// ソフトブラシのカバレッジ(SPEC §17: 「硬さ 0–100%。半径 r に対し
/// r×硬さ までカバレッジ 1、そこから外周 r までなめらかに減衰
/// (smoothstep)」)。`hardness` は 0.0–1.0。
///
/// 硬さ 100% (`hardness == 1.0`) でも輪郭がジャギーにならないよう、減衰帯
/// の幅は少なくとも 0.5px 確保する(ARCHITECTURE.md §15.1: 「ブラシは常時
/// AA」。旧 `stamp_coverage` が `r + 0.5` を下限にしていたのと同じ考え方の
/// 一般化)。
pub fn stamp_soft_coverage(cx: f32, cy: f32, radius: f32, hardness: f32, x: i32, y: i32) -> u8 {
    let dx = x as f32 + 0.5 - cx;
    let dy = y as f32 + 0.5 - cy;
    let dist = (dx * dx + dy * dy).sqrt();
    let r = radius.max(0.0);
    let h = hardness.clamp(0.0, 1.0);
    let inner = r * h;
    let outer = r.max(inner + 0.5);
    if dist <= inner {
        return 255;
    }
    if dist >= outer {
        return 0;
    }
    let t = ((dist - inner) / (outer - inner)).clamp(0.0, 1.0);
    // smoothstep(1 -> 0): 3t^2 - 2t^3 を「255 から 0 へ」の向きで使う。
    let smooth = t * t * (3.0 - 2.0 * t);
    ((1.0 - smooth) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// 鉛筆モードの 2 値スタンプ(SPEC §17: 「アンチエイリアスなしの2値スタンプ
/// (ピクセルアート用)。硬さ無視」)。`stamp_round` の判定式と同じ(半径の
/// 底上げも含めて `hard_edge_radius` を共有する)だが、カバレッジ値
/// (0 または 255)として返すため `tools/brush.rs` の不透明度合成ロジックを
/// ブラシ/鉛筆で共通化できる。
pub fn stamp_pencil_coverage(cx: f32, cy: f32, radius: f32, x: i32, y: i32) -> u8 {
    let dx = x as f32 + 0.5 - cx;
    let dy = y as f32 + 0.5 - cy;
    let dist2 = dx * dx + dy * dy;
    if dist2 <= hard_edge_radius(radius).powi(2) {
        255
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// M3: 直線・矩形・楕円(tools/shapes.rs が使う)
// ---------------------------------------------------------------------------

/// `(x0, y0, x1, y1)` を `x0<=x1, y0<=y1` になるよう正規化する。
fn normalize_rect(rect: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (a, b, c, d) = rect;
    (a.min(c), b.min(d), a.max(c), b.max(d))
}

/// 矩形/楕円の外接矩形に太さぶんの余白を足した、実際に触れうる画像座標の
/// 矩形(ストローク前のタイル退避に使う、境界クランプ前)。
pub fn rect_shape_bounds(rect: (f32, f32, f32, f32), thickness: f32) -> IRect {
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let pad = (thickness / 2.0).max(0.0).ceil() as i32 + 1;
    IRect {
        x0: x0.floor() as i32 - pad,
        y0: y0.floor() as i32 - pad,
        x1: x1.ceil() as i32 + pad,
        y1: y1.ceil() as i32 + pad,
    }
}

/// 楕円は外接矩形の内側に収まるため、境界計算は矩形と同じでよい。
pub fn ellipse_shape_bounds(rect: (f32, f32, f32, f32), thickness: f32) -> IRect {
    rect_shape_bounds(rect, thickness)
}

/// 矩形の枠線(SPEC §4: 太さ=ブラシサイズ)。`stroke_segment` ベースで
/// 4 辺を辿るので端(角)は自然に丸くなる(ARCHITECTURE.md §5)。触れた
/// 矩形群の厳密な合併を返す。
pub fn draw_rect_outline(
    surface: &mut Surface,
    rect: (f32, f32, f32, f32),
    thickness: f32,
    color: [u8; 4],
) -> IRect {
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let radius = (thickness / 2.0).max(0.5);
    let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
    let mut touched: Option<IRect> = None;
    for w in corners.windows(2) {
        let t = stroke_segment(surface, w[0], w[1], radius, color, false);
        touched = Some(match touched {
            Some(u) => u.union(&t),
            None => t,
        });
    }
    touched.unwrap_or(IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    })
}

/// 矩形の内部塗りつぶし(SPEC §4「塗りつぶし」モード用。flood fill とは無関係)。
pub fn fill_rect(surface: &mut Surface, rect: (f32, f32, f32, f32), color: [u8; 4]) -> IRect {
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let bounds = IRect {
        x0: x0.round() as i32,
        y0: y0.round() as i32,
        x1: x1.round() as i32,
        y1: y1.round() as i32,
    }
    .clamp_to(surface.width, surface.height);
    if bounds.is_empty() {
        return bounds;
    }
    for y in bounds.y0..bounds.y1 {
        for x in bounds.x0..bounds.x1 {
            surface.set_pixel(x, y, color);
        }
    }
    bounds
}

/// 楕円の枠線。媒介変数法で境界上の点を求め、`stroke_segment` で結ぶ
/// (ARCHITECTURE.md §5: 「楕円は媒介変数 or 中点法」「枠線は stroke_segment
/// ベース」)。触れた矩形群の厳密な合併を返す。
pub fn draw_ellipse_outline(
    surface: &mut Surface,
    rect: (f32, f32, f32, f32),
    thickness: f32,
    color: [u8; 4],
) -> IRect {
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let rx = (x1 - x0) / 2.0;
    let ry = (y1 - y0) / 2.0;
    let radius = (thickness / 2.0).max(0.5);

    if rx <= 0.0 || ry <= 0.0 {
        // 縦横どちらかが 0 の退化ケース: 点(スタンプ 1 つ)として描く。
        return stamp_round(surface, cx, cy, radius, color, false);
    }

    // Ramanujan の近似式でおおよその周長を求め、ブラシ半径から刻み数を決める
    // (`stroke_segment` の間隔ポリシー: 間隔 <= max(1px, radius/2) に合わせる)。
    let h = ((rx - ry) / (rx + ry)).powi(2);
    let perimeter =
        std::f32::consts::PI * (rx + ry) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
    let step = (radius / 2.0).max(1.0);
    let steps = ((perimeter.max(1.0)) / step).ceil().max(8.0) as u32;

    let point_at = |t: f32| -> (f32, f32) {
        let angle = t * std::f32::consts::TAU;
        (cx + rx * angle.cos(), cy + ry * angle.sin())
    };

    let mut prev = point_at(0.0);
    let mut touched: Option<IRect> = None;
    for i in 1..=steps {
        let cur = point_at(i as f32 / steps as f32);
        let t = stroke_segment(surface, prev, cur, radius, color, false);
        touched = Some(match touched {
            Some(u) => u.union(&t),
            None => t,
        });
        prev = cur;
    }
    touched.unwrap_or(IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    })
}

/// 楕円の内部塗りつぶし。走査線ごとに `(x-cx)^2/rx^2 + (y-cy)^2/ry^2 <= 1` を
/// 満たす範囲を塗る。
pub fn fill_ellipse(surface: &mut Surface, rect: (f32, f32, f32, f32), color: [u8; 4]) -> IRect {
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let rx = (x1 - x0) / 2.0;
    let ry = (y1 - y0) / 2.0;
    let empty = IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };
    if rx <= 0.0 || ry <= 0.0 {
        return empty;
    }

    let bounds = IRect {
        x0: x0.floor() as i32,
        y0: y0.floor() as i32,
        x1: x1.ceil() as i32,
        y1: y1.ceil() as i32,
    }
    .clamp_to(surface.width, surface.height);
    if bounds.is_empty() {
        return bounds;
    }
    for y in bounds.y0..bounds.y1 {
        let ny = (y as f32 + 0.5 - cy) / ry;
        if ny.abs() > 1.0 {
            continue;
        }
        for x in bounds.x0..bounds.x1 {
            let nx = (x as f32 + 0.5 - cx) / rx;
            if nx * nx + ny * ny <= 1.0 {
                surface.set_pixel(x, y, color);
            }
        }
    }
    bounds
}

// ---------------------------------------------------------------------------
// M3: 塗りつぶし(flood fill、tools/fill.rs が使う)
// ---------------------------------------------------------------------------

/// 各チャンネル差の最大値で許容値判定する(SPEC §4: 「許容値は各チャンネル差の
/// 最大値で判定」)。
fn color_within_tolerance(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    let diff = |i: usize| (a[i] as i32 - b[i] as i32).unsigned_abs();
    diff(0).max(diff(1)).max(diff(2)).max(diff(3)) <= tolerance as u32
}

/// スキャンライン法の塗りつぶし(SPEC §4: 連結領域のみ。tolerance 0–255。
/// スタック使用、再帰禁止: ARCHITECTURE.md §5)。開始色と塗色が同一なら
/// no-op(SPEC §5 のラスタ演算節)。実際に触れた(クランプ後の)外接矩形を
/// 返す。
///
/// 典型的な「スパンをスタックに積む」スキャンライン法(Wikipedia の
/// Flood fill 記事の 4-way scanline アルゴリズムと同型): 1 つの (x, y) を
/// pop したら、その行を左右いっぱいに伸ばして 1 スパン(`xl..=xr`)を確定し、
/// 訪問済みにする。上下の行はそのスパンの範囲だけを走査し、まだ訪問して
/// いない連続区間ごとに先頭の 1 点だけをシード(次に pop する候補)として
/// 積む(区間内の残りは pop 時に xl/xr の伸長で自然に回収される)。
///
/// `before_write` は、新しく確定した 1 スパン(まだ元の色のまま)の矩形を
/// 添えて、そのスパンを実際に書き換える直前に 1 回呼ばれる。
/// `tools/fill.rs` はこれを使って undo 用の CoW タイル退避
/// (`History::ensure_tiles_saved_buf`)を書き込みの直前にその場で行う。これに
/// より、あらかじめ全域を読み取り専用でもう一度スキャンし直す(旧実装の
/// `flood_fill_bounds` による二重スキャン。4000×4000 全面塗りの実測で
/// 1 クリックあたり約 2 倍のコストになっていた)必要も、訪問画素の座標を
/// `Vec<(i32,i32)>` に貯めておく(同条件で約 128MB の一時確保)必要もない。
/// raster.rs は引き続き `history` モジュールを一切知らない(コールバック
/// 注入のみ)。
///
/// `is_open` は「まだ訪問しておらず、かつ許容値内で目的色に一致する」かを
/// 判定する。訪問済み(= 書き込み済み)の画素は二度と読み返さないため、
/// 読み取りと書き込みを同じ `surface` に対して同一スキャンの中で行っても
/// 正しさは崩れない。
pub fn flood_fill(
    surface: &mut Surface,
    x: i32,
    y: i32,
    color: [u8; 4],
    tolerance: u8,
    mut before_write: impl FnMut(&Surface, IRect),
) -> IRect {
    let empty = IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };
    let Some(target) = surface.get_pixel(x, y) else {
        return empty;
    };
    if target == color {
        return empty;
    }
    let w = surface.width as i32;
    let h = surface.height as i32;

    let mut visited = vec![false; w as usize * h as usize];
    let idx = |x: i32, y: i32| y as usize * w as usize + x as usize;
    let is_open = |surface: &Surface, visited: &[bool], x: i32, y: i32| {
        !visited[idx(x, y)]
            && surface
                .get_pixel(x, y)
                .is_some_and(|p| color_within_tolerance(p, target, tolerance))
    };

    let mut bounds = IRect {
        x0: x,
        y0: y,
        x1: x + 1,
        y1: y + 1,
    };
    let mut stack = vec![(x, y)];

    while let Some((sx, sy)) = stack.pop() {
        if !is_open(surface, &visited, sx, sy) {
            // 既に別のスパンとして訪問済み、または(先読みシードが後で
            // 無効になった)対象外。
            continue;
        }
        let mut xl = sx;
        while xl - 1 >= 0 && is_open(surface, &visited, xl - 1, sy) {
            xl -= 1;
        }
        let mut xr = sx;
        while xr + 1 < w && is_open(surface, &visited, xr + 1, sy) {
            xr += 1;
        }

        let span_rect = IRect {
            x0: xl,
            y0: sy,
            x1: xr + 1,
            y1: sy + 1,
        };
        before_write(surface, span_rect);
        for x in xl..=xr {
            visited[idx(x, sy)] = true;
            surface.set_pixel(x, sy, color);
        }
        bounds.x0 = bounds.x0.min(xl);
        bounds.x1 = bounds.x1.max(xr + 1);
        bounds.y0 = bounds.y0.min(sy);
        bounds.y1 = bounds.y1.max(sy + 1);

        for ny in [sy - 1, sy + 1] {
            if ny < 0 || ny >= h {
                continue;
            }
            // スパン `xl..=xr` の真上/真下だけを走査し、まだ訪問していない
            // 連続区間ごとに先頭の 1 点をシードとして積む。
            let mut in_run = false;
            for x in xl..=xr {
                if is_open(surface, &visited, x, ny) {
                    if !in_run {
                        stack.push((x, ny));
                        in_run = true;
                    }
                } else {
                    in_run = false;
                }
            }
        }
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 塗りつぶされたバッファを持つテスト用 `Surface` を作る。
    fn make_buffer(width: u32, height: u32, fill: [u8; 4]) -> Vec<u8> {
        let count = (width as usize).saturating_mul(height as usize);
        let mut pixels = Vec::with_capacity(count.saturating_mul(4));
        for _ in 0..count {
            pixels.extend_from_slice(&fill);
        }
        pixels
    }

    #[test]
    fn stamp_bounds_covers_circle_extent() {
        let b = stamp_bounds(10.0, 10.0, 3.0);
        assert!(b.x0 <= 7 && b.x1 >= 13);
        assert!(b.y0 <= 7 && b.y1 >= 13);
    }

    #[test]
    fn stamp_round_paints_center_pixel() {
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        stamp_round(&mut s, 10.0, 10.0, 3.0, [255, 0, 0, 255], false);
        assert_eq!(s.get_pixel(10, 10), Some([255, 0, 0, 255]));
    }

    #[test]
    fn stamp_round_respects_radius_boundary() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        let (cx, cy, r) = (20.0, 20.0, 5.0);
        stamp_round(&mut s, cx, cy, r, [255, 0, 0, 255], false);
        // 中心から半径+2px 離れたピクセルは塗られていないはず。
        assert_eq!(s.get_pixel(20 + 8, 20), Some([0, 0, 0, 0]));
        assert_eq!(s.get_pixel(20, 20 + 8), Some([0, 0, 0, 0]));
        // 中心のすぐ隣(半径内)は塗られているはず。
        assert_eq!(s.get_pixel(20 + 2, 20), Some([255, 0, 0, 255]));
    }

    #[test]
    fn stamp_round_erase_sets_transparent() {
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        stamp_round(&mut s, 5.0, 5.0, 2.0, [0, 0, 0, 0], true);
        assert_eq!(s.get_pixel(5, 5), Some([0, 0, 0, 0]));
    }

    #[test]
    fn stamp_round_at_edge_does_not_panic_and_clips() {
        let mut buf = make_buffer(10, 10, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        // 画像外の中心・画像の四隅すべてで OOB 書き込みが起きないこと。
        stamp_round(&mut s, -5.0, -5.0, 4.0, [1, 2, 3, 4], false);
        stamp_round(&mut s, 0.0, 0.0, 4.0, [1, 2, 3, 4], false);
        stamp_round(&mut s, 9.0, 9.0, 4.0, [1, 2, 3, 4], false);
        stamp_round(&mut s, 100.0, 100.0, 4.0, [1, 2, 3, 4], false);
        assert_eq!(buf.len(), 10 * 10 * 4);
    }

    #[test]
    fn stamp_round_on_zero_size_surface_does_not_panic() {
        let mut buf: Vec<u8> = Vec::new();
        let mut s = Surface {
            width: 0,
            height: 0,
            pixels: &mut buf,
        };
        stamp_round(&mut s, 0.0, 0.0, 3.0, [1, 2, 3, 4], false);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn stamp_round_returns_touched_bounds() {
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        let touched = stamp_round(&mut s, 10.0, 10.0, 3.0, [255, 0, 0, 255], false);
        assert!(!touched.is_empty());
    }

    // -- v3 レビューで発見・修正したバグ: 半径 0.5(1px ブラシ)の鉛筆/
    // ハードスタンプは、クリック位置が全画素中心から 0.5px 超だと 1 画素も
    // 塗らない(`hard_edge_radius` の下駄で解消)。-----------------------

    #[test]
    fn stamp_round_at_minimum_radius_still_paints_when_clicked_on_a_pixel_corner() {
        // (10.0, 10.0) は画素の角(4 近傍の画素中心はすべて距離 √2/2 ≈
        // 0.707)。半径 0.5(SPEC §17/tools/mod.rs の最小ブラシ半径)のまま
        // だと dist² = 0.5 > radius² = 0.25 でどの画素も塗られなかった。
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        stamp_round(&mut s, 10.0, 10.0, 0.5, [255, 0, 0, 255], false);
        let neighbors = [(9, 9), (10, 9), (9, 10), (10, 10)];
        assert!(
            neighbors
                .iter()
                .any(|&(x, y)| s.get_pixel(x, y) == Some([255, 0, 0, 255])),
            "a click on a pixel corner with the minimum brush radius must still paint at \
             least the nearest pixel, not silently do nothing"
        );
    }

    #[test]
    fn stamp_round_at_minimum_radius_does_not_paint_far_away_pixels() {
        // 下駄を入れても暴走して離れた画素まで塗らないことの対照テスト。
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        stamp_round(&mut s, 10.0, 10.0, 0.5, [255, 0, 0, 255], false);
        assert_eq!(s.get_pixel(13, 10), Some([0, 0, 0, 0]));
        assert_eq!(s.get_pixel(10, 13), Some([0, 0, 0, 0]));
    }

    #[test]
    fn stamp_pencil_coverage_at_minimum_radius_covers_a_pixel_corner_click() {
        let neighbors = [(9, 9), (10, 9), (9, 10), (10, 10)];
        assert!(
            neighbors
                .iter()
                .any(|&(x, y)| stamp_pencil_coverage(10.0, 10.0, 0.5, x, y) == 255),
            "pencil mode at the minimum brush radius must still cover the nearest pixel \
             when clicked exactly on a pixel corner"
        );
    }

    #[test]
    fn stroke_segment_paints_both_endpoints() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        stroke_segment(&mut s, (5.0, 5.0), (30.0, 30.0), 2.0, [1, 2, 3, 4], false);
        assert_eq!(s.get_pixel(5, 5), Some([1, 2, 3, 4]));
        assert_eq!(s.get_pixel(30, 30), Some([1, 2, 3, 4]));
    }

    #[test]
    fn stroke_segment_has_no_gaps_along_the_line() {
        let mut buf = make_buffer(60, 10, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 60,
            height: 10,
            pixels: &mut buf,
        };
        stroke_segment(&mut s, (0.0, 5.0), (59.0, 5.0), 3.0, [1, 2, 3, 4], false);
        for x in 0..60 {
            assert_ne!(s.get_pixel(x, 5), Some([0, 0, 0, 0]), "gap found at x={x}");
        }
    }

    #[test]
    fn stroke_segment_same_point_still_paints() {
        let mut buf = make_buffer(10, 10, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        stroke_segment(&mut s, (5.0, 5.0), (5.0, 5.0), 2.0, [9, 9, 9, 9], false);
        assert_eq!(s.get_pixel(5, 5), Some([9, 9, 9, 9]));
    }

    #[test]
    fn stroke_segment_out_of_bounds_does_not_panic() {
        let mut buf = make_buffer(10, 10, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        stroke_segment(
            &mut s,
            (-20.0, -20.0),
            (30.0, 30.0),
            5.0,
            [1, 2, 3, 4],
            false,
        );
        assert_eq!(buf.len(), 10 * 10 * 4);
    }

    #[test]
    fn stroke_segment_returns_bounds_matching_segment_bounds() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        let touched = stroke_segment(&mut s, (5.0, 5.0), (30.0, 20.0), 2.0, [1, 2, 3, 4], false);
        let expected = segment_bounds((5.0, 5.0), (30.0, 20.0), 2.0).clamp_to(40, 40);
        assert_eq!(touched, expected);
    }

    #[test]
    fn blend_over_opaque_src_replaces_dst() {
        let out = blend_over([10, 20, 30, 255], [200, 100, 50, 255]);
        assert_eq!(out, [200, 100, 50, 255]);
    }

    #[test]
    fn blend_over_transparent_src_leaves_dst_unchanged() {
        let out = blend_over([10, 20, 30, 255], [200, 100, 50, 0]);
        assert_eq!(out, [10, 20, 30, 255]);
    }

    #[test]
    fn blend_over_half_alpha_mixes_channels() {
        // dst 不透明白 + src 半透明黒 -> 概ね中間のグレーになる。
        let out = blend_over([255, 255, 255, 255], [0, 0, 0, 128]);
        assert_eq!(out[3], 255);
        assert!((120..=135).contains(&out[0]));
    }

    #[test]
    fn blend_over_both_transparent_is_transparent() {
        let out = blend_over([0, 0, 0, 0], [0, 0, 0, 0]);
        assert_eq!(out, [0, 0, 0, 0]);
    }

    #[test]
    fn blend_over_transparent_dst_yields_src_exactly() {
        // v2: Document::recomposite が透明の初期値から積み上げるため、
        // 単一の不透明レイヤーの合成結果がそのレイヤーの画素と厳密に一致する
        // ことに依存する(io.rs のラウンドトリップテスト参照)。
        let out = blend_over([0, 0, 0, 0], [12, 34, 56, 78]);
        assert_eq!(out, [12, 34, 56, 78]);
    }

    // -- stamp_soft_coverage / stamp_pencil_coverage (v3 §17 ストロークエンジン) --

    #[test]
    fn stamp_soft_coverage_is_full_at_exact_center_regardless_of_hardness() {
        // 中心を画素中心(10.5, 10.5)ちょうどに合わせる(dist=0 を保証する
        // ため。中心が画素境界からずれていると硬さ 0% では dist>inner=0 に
        // なり得る)。
        for hardness in [0.0, 0.3, 1.0] {
            let cov = stamp_soft_coverage(10.5, 10.5, 4.0, hardness, 10, 10);
            assert_eq!(cov, 255, "hardness={hardness}");
        }
    }

    #[test]
    fn stamp_soft_coverage_is_zero_well_outside_radius() {
        let cov = stamp_soft_coverage(10.0, 10.0, 4.0, 1.0, 20, 10);
        assert_eq!(cov, 0);
    }

    #[test]
    fn stamp_soft_coverage_feathers_near_boundary_at_full_hardness() {
        // 硬さ 100% でも(ジャギー防止のため)輪郭付近は 0.5px 幅のフェザーが
        // 残る(ARCHITECTURE.md §15.1: 「ブラシは常時 AA」)。フェザー帯は
        // dist∈(4.0, 4.5) の狭い範囲なので、軸に沿った画素(整数距離になる)
        // ではなく斜め方向の画素を使う。
        let cov = stamp_soft_coverage(10.5, 10.5, 4.0, 1.0, 14, 11);
        assert!(cov > 0 && cov < 255, "expected partial coverage, got {cov}");
    }

    #[test]
    fn stamp_soft_coverage_falloff_boundary_is_full_inside_inner_radius() {
        // SPEC §17: 「半径 r に対し r×硬さ までカバレッジ 1」。硬さ 50% の
        // ブラシは、内側半径(r*0.5=2px)のすぐ内側では常に満コート。
        let cov = stamp_soft_coverage(10.0, 10.0, 4.0, 0.5, 11, 10);
        assert_eq!(cov, 255);
    }

    #[test]
    fn stamp_soft_coverage_falloff_decreases_monotonically_outward() {
        // r*硬さ から外周 r にかけて、カバレッジは単調に減少するはず
        // (smoothstep の性質、ARCHITECTURE.md §15.1)。
        let mut prev = 255u8;
        for x in 12..=18 {
            let cov = stamp_soft_coverage(10.0, 10.0, 8.0, 0.25, x, 10);
            assert!(cov <= prev, "coverage rose at x={x}: {cov} > {prev}");
            prev = cov;
        }
    }

    #[test]
    fn stamp_soft_coverage_hardness_zero_still_covers_exact_center() {
        // 硬さ 0% は「中心 1 点だけ完全不透明、そこから即座に減衰」になる
        // (inner=0)。中心を画素中心(10.5, 10.5)ちょうどに合わせれば、その
        // 画素は依然として満コート。
        let cov = stamp_soft_coverage(10.5, 10.5, 4.0, 0.0, 10, 10);
        assert_eq!(cov, 255);
        let cov_edge = stamp_soft_coverage(10.5, 10.5, 4.0, 0.0, 14, 10);
        assert_eq!(cov_edge, 0);
    }

    #[test]
    fn stamp_pencil_coverage_is_binary_not_feathered() {
        // SPEC §17: 「アンチエイリアスなしの2値スタンプ」。境界のすぐ内側/
        // 外側で 0 か 255 のどちらかにしかならない。
        for x in 5..15 {
            let cov = stamp_pencil_coverage(10.0, 10.0, 4.0, x, 10);
            assert!(
                cov == 0 || cov == 255,
                "expected binary coverage, got {cov}"
            );
        }
        assert_eq!(stamp_pencil_coverage(10.0, 10.0, 4.0, 10, 10), 255);
        assert_eq!(stamp_pencil_coverage(10.0, 10.0, 4.0, 20, 10), 0);
    }

    // -- rect / ellipse -------------------------------------------------------

    #[test]
    fn draw_rect_outline_paints_all_four_edges() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        draw_rect_outline(&mut s, (5.0, 5.0, 30.0, 20.0), 2.0, [255, 0, 0, 255]);
        // 4 辺の中点付近が塗られていること。
        assert_ne!(s.get_pixel(17, 5), Some([0, 0, 0, 0])); // 上辺
        assert_ne!(s.get_pixel(17, 20), Some([0, 0, 0, 0])); // 下辺
        assert_ne!(s.get_pixel(5, 12), Some([0, 0, 0, 0])); // 左辺
        assert_ne!(s.get_pixel(30, 12), Some([0, 0, 0, 0])); // 右辺
                                                             // 中央は塗られていない(枠線のみ)。
        assert_eq!(s.get_pixel(17, 12), Some([0, 0, 0, 0]));
    }

    #[test]
    fn fill_rect_fills_interior_and_not_beyond() {
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        fill_rect(&mut s, (5.0, 5.0, 10.0, 10.0), [1, 2, 3, 4]);
        assert_eq!(s.get_pixel(7, 7), Some([1, 2, 3, 4]));
        assert_eq!(s.get_pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(s.get_pixel(10, 10), Some([0, 0, 0, 0]));
    }

    #[test]
    fn fill_rect_handles_reversed_corners() {
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        fill_rect(&mut s, (10.0, 10.0, 5.0, 5.0), [1, 2, 3, 4]);
        assert_eq!(s.get_pixel(7, 7), Some([1, 2, 3, 4]));
    }

    #[test]
    fn shape_drawing_out_of_bounds_does_not_panic() {
        let mut buf = make_buffer(10, 10, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        draw_rect_outline(&mut s, (-20.0, -20.0, 50.0, 50.0), 4.0, [1, 2, 3, 4]);
        fill_rect(&mut s, (-20.0, -20.0, 50.0, 50.0), [1, 2, 3, 4]);
        draw_ellipse_outline(&mut s, (-20.0, -20.0, 50.0, 50.0), 4.0, [1, 2, 3, 4]);
        fill_ellipse(&mut s, (-20.0, -20.0, 50.0, 50.0), [1, 2, 3, 4]);
        assert_eq!(buf.len(), 10 * 10 * 4);
    }

    #[test]
    fn fill_ellipse_paints_center_not_corners() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        fill_ellipse(&mut s, (5.0, 5.0, 35.0, 35.0), [9, 9, 9, 255]);
        assert_eq!(s.get_pixel(20, 20), Some([9, 9, 9, 255]));
        // 外接矩形の角は楕円の外なので塗られていない。
        assert_eq!(s.get_pixel(5, 5), Some([0, 0, 0, 0]));
    }

    #[test]
    fn draw_ellipse_outline_paints_boundary_not_center() {
        let mut buf = make_buffer(40, 40, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 40,
            height: 40,
            pixels: &mut buf,
        };
        draw_ellipse_outline(&mut s, (5.0, 5.0, 35.0, 35.0), 2.0, [9, 9, 9, 255]);
        // 中心付近は塗られていない(枠線のみ)。
        assert_eq!(s.get_pixel(20, 20), Some([0, 0, 0, 0]));
        // 上端境界付近(中心 x, 外接矩形の上端 y)は塗られている。
        assert_ne!(s.get_pixel(20, 5), Some([0, 0, 0, 0]));
    }

    #[test]
    fn draw_ellipse_outline_degenerate_zero_height_does_not_panic() {
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
        };
        draw_ellipse_outline(&mut s, (2.0, 10.0, 18.0, 10.0), 2.0, [1, 2, 3, 4]);
        assert_eq!(buf.len(), 20 * 20 * 4);
    }

    // -- flood fill -------------------------------------------------------

    #[test]
    fn flood_fill_fills_connected_region_only() {
        // 10x10 の白地に、x=5 の列だけ黒い縦の壁を作って左右を分断する。
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        for y in 0..10 {
            s.set_pixel(5, y, [0, 0, 0, 255]);
        }
        flood_fill(&mut s, 0, 0, [255, 0, 0, 255], 0, |_, _| {});
        assert_eq!(s.get_pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(s.get_pixel(4, 9), Some([255, 0, 0, 255]));
        // 壁の右側は塗られていないはず。
        assert_eq!(s.get_pixel(6, 0), Some([255, 255, 255, 255]));
        assert_eq!(s.get_pixel(9, 9), Some([255, 255, 255, 255]));
        // 壁自体も塗られていない。
        assert_eq!(s.get_pixel(5, 5), Some([0, 0, 0, 255]));
    }

    #[test]
    fn flood_fill_respects_tolerance_threshold() {
        let mut buf = make_buffer(4, 1, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 4,
            height: 1,
            pixels: &mut buf,
        };
        s.set_pixel(2, 0, [240, 240, 240, 255]); // 白との差 15
        s.set_pixel(3, 0, [200, 200, 200, 255]); // 白との差 55

        flood_fill(&mut s, 0, 0, [0, 255, 0, 255], 20, |_, _| {});
        // 差15の画素は許容値20以内なので塗られる。
        assert_eq!(s.get_pixel(2, 0), Some([0, 255, 0, 255]));
        // 差55の画素は許容値20を超えるので塗られない。
        assert_eq!(s.get_pixel(3, 0), Some([200, 200, 200, 255]));
    }

    #[test]
    fn flood_fill_same_color_is_noop() {
        let mut buf = make_buffer(4, 4, [255, 255, 255, 255]);
        let before = buf.clone();
        let mut s = Surface {
            width: 4,
            height: 4,
            pixels: &mut buf,
        };
        let touched = flood_fill(&mut s, 0, 0, [255, 255, 255, 255], 0, |_, _| {});
        assert_eq!(buf, before);
        assert!(touched.is_empty());
    }

    #[test]
    fn flood_fill_out_of_bounds_does_not_panic() {
        let mut buf = make_buffer(4, 4, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 4,
            height: 4,
            pixels: &mut buf,
        };
        flood_fill(&mut s, -1, -1, [1, 2, 3, 4], 0, |_, _| {});
        flood_fill(&mut s, 100, 100, [1, 2, 3, 4], 0, |_, _| {});
        assert_eq!(buf.len(), 4 * 4 * 4);
    }

    #[test]
    fn flood_fill_on_zero_size_surface_does_not_panic() {
        let mut buf: Vec<u8> = Vec::new();
        let mut s = Surface {
            width: 0,
            height: 0,
            pixels: &mut buf,
        };
        flood_fill(&mut s, 0, 0, [1, 2, 3, 4], 0, |_, _| {});
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn flood_fill_returns_the_touched_bounds() {
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
        };
        let touched = flood_fill(&mut s, 0, 0, [1, 2, 3, 4], 0, |_, _| {});
        assert_eq!(
            (touched.x0, touched.y0, touched.x1, touched.y1),
            (0, 0, 10, 10)
        );
    }

    #[test]
    fn flood_fill_calls_before_write_while_span_still_holds_original_color() {
        // M4 で発見・修正したバグ: 以前は「触れる領域を求める読み取り専用の
        // 事前スキャン」と「実際に塗る本スキャン」の 2 回スキャンしていた
        // (raster.rs 冒頭のコメント参照)。`before_write` コールバックが、
        // そのスパンを実際に書き換えるより前に、かつまだ元の色のままの
        // 状態で呼ばれることを確認する(`tools/fill.rs` がここで
        // タイル退避を行うことに依存している)。
        let mut buf = make_buffer(6, 1, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 6,
            height: 1,
            pixels: &mut buf,
        };
        let mut snapshots: Vec<Vec<[u8; 4]>> = Vec::new();
        flood_fill(&mut s, 0, 0, [1, 2, 3, 255], 0, |surf, rect| {
            let row: Vec<[u8; 4]> = (rect.x0..rect.x1)
                .map(|x| surf.get_pixel(x, rect.y0).unwrap())
                .collect();
            snapshots.push(row);
        });
        // 1 行だけの連結領域なので 1 スパンにまとまるはず。
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].iter().all(|&p| p == [255, 255, 255, 255]));
        // コールバックの後には実際に新しい色で塗られている。
        assert_eq!(s.get_pixel(0, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn flood_fill_4000x4000_is_correct_and_terminates() {
        // ARCHITECTURE.md §5: 4000x4000 全面でも 100ms 未満(リリースビルド)。
        // `cargo test` はデバッグ(最適化なし)でビルドされ、境界チェック付き
        // ピクセルアクセスの定数倍がリリースの数十倍になりうるため、ここでは
        // 秒単位の緩い上限で「無限ループ/O(n^2) 的な劣化がないこと」だけを
        // 保証する。100ms 目標そのものはリリースビルドで別途確認する
        // (このタスクの最終確認で `cargo build --release` 版を計測済み)。
        let mut buf = make_buffer(4000, 4000, [255, 255, 255, 255]);
        let mut s = Surface {
            width: 4000,
            height: 4000,
            pixels: &mut buf,
        };
        let start = std::time::Instant::now();
        flood_fill(&mut s, 0, 0, [1, 2, 3, 4], 0, |_, _| {});
        let elapsed = start.elapsed();
        assert_eq!(s.get_pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(s.get_pixel(3999, 3999), Some([1, 2, 3, 4]));
        assert_eq!(s.get_pixel(2000, 2000), Some([1, 2, 3, 4]));
        assert!(
            elapsed.as_secs() < 10,
            "flood_fill took suspiciously long (possible infinite loop / O(n^2)): {elapsed:?}"
        );
    }
}
