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

use crate::document::{IRect, SelMask};

/// v2 §14.1: raster.rs が操作する対象。呼び出し側(tools/*)がアクティブ
/// レイヤーのピクセルバッファをこれに包んで渡す。`Document`/`Layer` を
/// 一切参照しないことで、raster.rs をレイヤー概念から独立させる。
pub struct Surface<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a mut [u8],
    /// v4 §16.3/§21(ARCHITECTURE.md): 描画クリップ。選択があるときだけ
    /// `Some` になる。`set_pixel` だけがこれを見るので、`stamp_round` /
    /// `stroke_segment` / `fill_rect` / `fill_ellipse` / 枠線描画など
    /// `set_pixel` 経由で書くすべての関数が、この 1 箇所を変えるだけで
    /// 自動的にクリップに従う。`None` なら従来どおり(ARCHITECTURE.md
    /// §16.10-2: 「選択が無いときのコストがゼロであること」)。
    pub clip: Option<&'a SelMask>,
}

impl<'a> Surface<'a> {
    /// `(x, y)` のピクセル値を読む。範囲外なら `None`(パニックしない)。
    /// クリップは見ない(ブラシの「元ピクセル」参照など、書き込みを伴わない
    /// 読み取りは常にクリップの影響を受けない)。
    pub fn get_pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        self.pixels
            .get(idx..idx + 4)
            .map(|s| [s[0], s[1], s[2], s[3]])
    }

    /// `(x, y)` にピクセル値を書く。範囲外、または `clip` があってその画素が
    /// 選択されていなければ何もしない(パニックしない)。
    pub fn set_pixel(&mut self, x: i32, y: i32, color: [u8; 4]) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        if let Some(clip) = self.clip {
            if !clip.contains(x, y) {
                return;
            }
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
    // v4 §16.3/§21(ARCHITECTURE.md): 「塗りつぶしの連結探索は clip 外を
    // 壁として扱う」。クリップ外の画素は `is_open` が常に偽を返すことで、
    // 探索自体がそこを越えて広がらない(結果として書き込みも自然に
    // クリップされる。開始点自身がクリップ外なら、下の `bounds` 初期化を
    // `Option` にしてあるおかげで何も塗らず touched も空になる)。
    let is_open = |surface: &Surface, visited: &[bool], x: i32, y: i32| {
        !visited[idx(x, y)]
            && surface.clip.is_none_or(|clip| clip.contains(x, y))
            && surface
                .get_pixel(x, y)
                .is_some_and(|p| color_within_tolerance(p, target, tolerance))
    };

    // v4 §16.3: 開始点自身がクリップで塞がれている(=一度も `is_open` が
    // 真にならない)場合に touched が非空(1x1)を返してしまわないよう、
    // 実際に何か塗った時点で初めて値が入る `Option` にした(以前は
    // `(x, y, x+1, y+1)` で事前に種を蒔いてから union していたが、
    // クリップが無ければ最初のスパンに必ず開始点自身が含まれるため、
    // この変更は既存の(クリップ無し)挙動には影響しない)。
    let mut bounds: Option<IRect> = None;
    let mut stack = vec![(x, y)];

    while let Some((sx, sy)) = stack.pop() {
        if !is_open(surface, &visited, sx, sy) {
            // 既に別のスパンとして訪問済み、または(先読みシードが後で
            // 無効になった)対象外。
            continue;
        }
        let mut xl = sx;
        while xl > 0 && is_open(surface, &visited, xl - 1, sy) {
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
        // v4 §16.1: スパン全体を 1 回の行スライスで書く(以前は `xl..=xr` を
        // 画素ごとに `set_pixel` していた — 呼ぶたびに境界チェック +
        // `y*w+x` のインデックス計算をしていた)。スパンは `is_open` の
        // 判定で既に `[0, w)` 内であることが保証されているので、
        // `visited`/`surface.pixels` それぞれ 1 回の範囲アクセスで済む。
        let span_start = sy as usize * w as usize + xl as usize;
        let span_len = (xr - xl + 1) as usize;
        if let Some(v) = visited.get_mut(span_start..span_start + span_len) {
            v.fill(true);
        }
        let byte_start = span_start * 4;
        let byte_len = span_len * 4;
        if let Some(row) = surface.pixels.get_mut(byte_start..byte_start + byte_len) {
            for px in row.chunks_exact_mut(4) {
                px.copy_from_slice(&color);
            }
        }
        bounds = Some(match bounds {
            Some(b) => b.union(&span_rect),
            None => span_rect,
        });

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
    bounds.unwrap_or(empty)
}

// ---------------------------------------------------------------------------
// v4 §16.3/§22: 自動選択(マジックワンド)。`flood_fill` と全く同じ連結判定
// (`color_within_tolerance`、4-way スキャンライン、スタック使用)を使うが、
// ピクセルへ書き込む代わりに「訪問済み=選択済み」の判定結果をそのまま
// `SelMask` として返す(ARCHITECTURE.md §16.3: 「flood_mask(自動選択。既存
// flood fill の visit を流用)」)。`flood_fill` のように `Surface` を書き
// 換える必要が無いため `&Surface`(不変借用)だけで完結でき、`flood_fill`
// のような読み取り/書き込みの入れ替わりに伴う借用の取り回しが不要になる分、
// 独立した実装にした方が単純になる。
// ---------------------------------------------------------------------------

/// クリックした画素から許容値内の連結領域を選択マスクにする(SPEC §22:
/// 「自動選択…flood fill と同じ判定」)。開始点が範囲外、またはドキュメント
/// が 0×0 なら空マスク(パニックしない)。`surface.clip` があれば(v4 §16.3:
/// 「塗りつぶしの連結探索は clip 外を壁として扱う」と同様)クリップ外へは
/// 広がらない。
pub fn flood_mask(surface: &Surface, x: i32, y: i32, tolerance: u8) -> SelMask {
    let Some(target) = surface.get_pixel(x, y) else {
        return SelMask::empty();
    };
    let w = surface.width as i32;
    let h = surface.height as i32;
    if w <= 0 || h <= 0 {
        return SelMask::empty();
    }

    let mut visited = vec![false; w as usize * h as usize];
    let idx = |x: i32, y: i32| y as usize * w as usize + x as usize;
    let is_open = |visited: &[bool], x: i32, y: i32| {
        !visited[idx(x, y)]
            && surface.clip.is_none_or(|clip| clip.contains(x, y))
            && surface
                .get_pixel(x, y)
                .is_some_and(|p| color_within_tolerance(p, target, tolerance))
    };

    let mut bounds: Option<IRect> = None;
    let mut stack = vec![(x, y)];
    while let Some((sx, sy)) = stack.pop() {
        if !is_open(&visited, sx, sy) {
            continue;
        }
        let mut xl = sx;
        while xl > 0 && is_open(&visited, xl - 1, sy) {
            xl -= 1;
        }
        let mut xr = sx;
        while xr + 1 < w && is_open(&visited, xr + 1, sy) {
            xr += 1;
        }

        let span_rect = IRect {
            x0: xl,
            y0: sy,
            x1: xr + 1,
            y1: sy + 1,
        };
        let span_start = sy as usize * w as usize + xl as usize;
        let span_len = (xr - xl + 1) as usize;
        if let Some(v) = visited.get_mut(span_start..span_start + span_len) {
            v.fill(true);
        }
        bounds = Some(match bounds {
            Some(b) => b.union(&span_rect),
            None => span_rect,
        });

        for ny in [sy - 1, sy + 1] {
            if ny < 0 || ny >= h {
                continue;
            }
            let mut in_run = false;
            for xx in xl..=xr {
                if is_open(&visited, xx, ny) {
                    if !in_run {
                        stack.push((xx, ny));
                        in_run = true;
                    }
                } else {
                    in_run = false;
                }
            }
        }
    }

    let Some(bbox) = bounds else {
        return SelMask::empty();
    };
    let bw = bbox.width() as usize;
    let bh = bbox.height() as usize;
    let mut mask = vec![0u8; bw * bh];
    for yy in 0..bh {
        let src_row = (bbox.y0 as usize + yy) * w as usize + bbox.x0 as usize;
        let dst_row = yy * bw;
        for xx in 0..bw {
            mask[dst_row + xx] = if visited[src_row + xx] { 255 } else { 0 };
        }
    }
    SelMask { bbox, mask }
}

// ---------------------------------------------------------------------------
// v4 §16.4/§23: グラデーション(tools/gradient.rs が使う)。
// ---------------------------------------------------------------------------

/// SPEC §23: 「種類: 線形 / 円形」。`gradient_span` の補間形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
}

/// 始点 `p0` → 終点 `p1` に対する画像座標 `p` の補間係数 `t`(0.0–1.0、
/// クランプ済み。SPEC §23: 「始点前/終点後はクランプ(端色で埋める)」)。
///
/// - 線形: `p0→p1` の直線への正射影(内積 / 距離二乗)。
/// - 円形: `p0` からの距離 / `|p0-p1|`(半径)。
///
/// `p0 == p1`(ドラッグ距離 0)の退化ケースは、線形・円形どちらも `t = 0.0`
/// (始点色一色)を返す(0 除算を避けつつ、無意味な巨大値より見た目が安定する)。
pub fn gradient_span(kind: GradientKind, p0: (f32, f32), p1: (f32, f32), p: (f32, f32)) -> f32 {
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return 0.0;
    }
    match kind {
        GradientKind::Linear => {
            let vx = p.0 - p0.0;
            let vy = p.1 - p0.1;
            ((vx * dx + vy * dy) / len2).clamp(0.0, 1.0)
        }
        GradientKind::Radial => {
            let radius = len2.sqrt();
            let dist = ((p.0 - p0.0).powi(2) + (p.1 - p0.1).powi(2)).sqrt();
            (dist / radius).clamp(0.0, 1.0)
        }
    }
}

/// `c0` から `c1` へ straight-alpha RGBA8 のまま線形補間する(`gradient_span`
/// が返す `t` をそのまま渡す)。
pub fn lerp_color(c0: [u8; 4], c1: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mut out = [0u8; 4];
    for i in 0..4 {
        let a = c0[i] as f32;
        let b = c1[i] as f32;
        out[i] = (a + (b - a) * t).round().clamp(0.0, 255.0) as u8;
    }
    out
}

// ---------------------------------------------------------------------------
// v4 §16.5/§24: 色調補正(app.rs が History 経由のスナップショット/即時
// 適用ループと組み合わせて使う純関数群)。RGB のみを変更し、アルファは
// 常に不変(SPEC §24)。
// ---------------------------------------------------------------------------

/// SPEC §24: 「階調の反転…RGB反転、アルファ不変」。
pub fn invert_pixel(px: [u8; 4]) -> [u8; 4] {
    [255 - px[0], 255 - px[1], 255 - px[2], px[3]]
}

/// SPEC §24: 「グレースケール化…Rec.709 輝度」。係数は ITU-R BT.709 の
/// 輝度式(0.2126 R + 0.7152 G + 0.0722 B)。
pub fn grayscale_pixel(px: [u8; 4]) -> [u8; 4] {
    let l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32)
        .round()
        .clamp(0.0, 255.0) as u8;
    [l, l, l, px[3]]
}

/// SPEC §24: 「明るさ・コントラスト…各 −100〜+100」用の 256 要素 LUT
/// (ARCHITECTURE.md §16.5: 「LUT を作ってから行スライスで適用」)。
/// `brightness`/`contrast` は -100..=100 を期待する(範囲外はクランプ)。
///
/// コントラストは古典的な「傾き」補正式(`factor = 259*(c+255) /
/// (255*(259-c))`、c は -255..255)を使う。`contrast` (-100..100) を
/// `c = contrast * 2.55` で -255..255 に写像してから適用する: `contrast=0` で
/// `factor=1.0`(無補正)、`contrast=100` で急峻な傾き(ほぼ二値化)、
/// `contrast=-100` で `factor≈0`(128 の単色フラットに近づく)になる。
/// 明るさは 128 を中心にした傾き補正の**後**に単純な加算オフセットとして効く。
pub fn brightness_contrast_lut(brightness: i32, contrast: i32) -> [u8; 256] {
    let brightness = brightness.clamp(-100, 100) as f32;
    let contrast = contrast.clamp(-100, 100) as f32;
    let c255 = contrast * 2.55;
    let factor = (259.0 * (c255 + 255.0)) / (255.0 * (259.0 - c255)).max(1e-3);
    let offset = brightness * 2.55;
    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let v = factor * (i as f32 - 128.0) + 128.0 + offset;
        *slot = v.round().clamp(0.0, 255.0) as u8;
    }
    lut
}

/// `brightness_contrast_lut` で作った LUT を 1 画素へ適用する(アルファ不変)。
pub fn apply_lut_pixel(px: [u8; 4], lut: &[u8; 256]) -> [u8; 4] {
    [
        lut[px[0] as usize],
        lut[px[1] as usize],
        lut[px[2] as usize],
        px[3],
    ]
}

/// RGB(0..255) → HSL(h: 0..360, s/l: 0..1)。
fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    if delta <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l <= 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };
    let h = if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    (h.rem_euclid(360.0), s, l)
}

/// HSL → RGB(0..255)。`rgb_to_hsl` の逆変換。
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s <= f32::EPSILON {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return (v, v, v);
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// SPEC §24: 「色相・彩度・明度…色相 −180〜+180、彩度/明度 −100〜+100」。
/// `dh` は度数のオフセット(360 で周回)、`ds`/`dl` は -100..100 を
/// 彩度・明度それぞれの -1.0..1.0 オフセットとして加算しクランプする。
/// アルファは不変。
pub fn adjust_hsl_pixel(px: [u8; 4], dh: i32, ds: i32, dl: i32) -> [u8; 4] {
    let (h, s, l) = rgb_to_hsl(px[0], px[1], px[2]);
    let h = (h + dh as f32).rem_euclid(360.0);
    let s = (s + ds as f32 / 100.0).clamp(0.0, 1.0);
    let l = (l + dl as f32 / 100.0).clamp(0.0, 1.0);
    let (r, g, b) = hsl_to_rgb(h, s, l);
    [r, g, b, px[3]]
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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
            clip: None,
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

    // -- v4 §16.3/§21: 描画クリップ(`Surface::clip`) ---------------------------

    #[test]
    fn set_pixel_outside_clip_is_a_no_op() {
        // ARCHITECTURE.md §16.3: 「図形・塗りつぶし…の書き込みで mask==0 の
        // 画素をスキップ」。`stamp_round`/`fill_rect`/`stroke_segment` 等は
        // すべて `set_pixel` 経由で書くため、ここ 1 箇所を確認すれば全部が
        // クリップに従うことを保証できる。
        let mut buf = make_buffer(10, 10, [0, 0, 0, 0]);
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 10,
            },
            mask: vec![255u8; 5 * 10],
        };
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
            clip: Some(&clip),
        };
        s.set_pixel(2, 2, [1, 2, 3, 255]); // クリップ内。
        s.set_pixel(7, 2, [9, 9, 9, 255]); // クリップ外。
        assert_eq!(s.get_pixel(2, 2), Some([1, 2, 3, 255]));
        assert_eq!(
            s.get_pixel(7, 2),
            Some([0, 0, 0, 0]),
            "set_pixel outside the clip mask must be a no-op"
        );
    }

    #[test]
    fn set_pixel_with_no_clip_behaves_exactly_as_before() {
        // ARCHITECTURE.md §16.10-2: 「選択が無いときのコストがゼロ」。
        // `clip: None` は従来どおり全域に書ける。
        let mut buf = make_buffer(4, 4, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 4,
            height: 4,
            pixels: &mut buf,
            clip: None,
        };
        s.set_pixel(3, 3, [7, 7, 7, 255]);
        assert_eq!(s.get_pixel(3, 3), Some([7, 7, 7, 255]));
    }

    #[test]
    fn stamp_round_does_not_paint_outside_the_clip() {
        // 個別の raster 関数を直接確認(`set_pixel` を経由することの傍証)。
        let mut buf = make_buffer(20, 20, [0, 0, 0, 0]);
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 20,
            },
            mask: vec![255u8; 10 * 20],
        };
        let mut s = Surface {
            width: 20,
            height: 20,
            pixels: &mut buf,
            clip: Some(&clip),
        };
        // 中心をクリップ境界(x=10)にまたがせて描く。
        stamp_round(&mut s, 10.0, 10.0, 5.0, [255, 0, 0, 255], false);
        assert_ne!(
            s.get_pixel(6, 10),
            Some([0, 0, 0, 0]),
            "inside the clip should be painted"
        );
        assert_eq!(
            s.get_pixel(13, 10),
            Some([0, 0, 0, 0]),
            "outside the clip must not be painted even though it's within the stamp radius"
        );
    }

    #[test]
    fn flood_fill_does_not_cross_the_clip_boundary() {
        // ARCHITECTURE.md §16.3: 「塗りつぶしの連結探索は clip 外を壁として
        // 扱う」。壁になる色の境界が無くても、クリップ境界自体が壁になる。
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 10,
            },
            mask: vec![255u8; 5 * 10],
        };
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
            clip: Some(&clip),
        };
        let touched = flood_fill(&mut s, 0, 0, [1, 2, 3, 255], 0, |_, _| {});
        assert_eq!(s.get_pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(s.get_pixel(4, 9), Some([1, 2, 3, 255]));
        assert_eq!(
            s.get_pixel(5, 0),
            Some([255, 255, 255, 255]),
            "the clip boundary must stop the flood fill even though the color matches"
        );
        assert!(touched.x1 <= 5, "touched bounds must not cross the clip");
    }

    #[test]
    fn flood_fill_seed_outside_clip_paints_nothing() {
        // v4 §16.3: クリック位置(種)自体がクリップ外なら、何も塗らず
        // touched も空になる(1x1 の偽陽性を返さない、raster.rs の実装
        // コメント参照)。
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 10,
            },
            mask: vec![255u8; 5 * 10],
        };
        let mut s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
            clip: Some(&clip),
        };
        // (7, 0) はクリップ外。
        let touched = flood_fill(&mut s, 7, 0, [1, 2, 3, 255], 0, |_, _| {});
        assert!(
            touched.is_empty(),
            "a seed point outside the clip must yield an empty touched rect, got {touched:?}"
        );
        assert_eq!(s.get_pixel(7, 0), Some([255, 255, 255, 255]));
    }

    // -- V4-M3/SPEC §22: 自動選択(flood_mask) ------------------------------

    #[test]
    fn flood_mask_selects_connected_region_only() {
        // 左半分が赤、右半分が青の 10x10。左上をクリックすると左半分だけが
        // 選択されるはず(`flood_fill_fills_connected_region_only` と同じ
        // 配置)。
        let mut buf = vec![0u8; 10 * 10 * 4];
        for y in 0..10 {
            for x in 0..10 {
                let idx = (y * 10 + x) * 4;
                let color = if x < 5 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                buf[idx..idx + 4].copy_from_slice(&color);
            }
        }
        let s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
            clip: None,
        };
        let mask = flood_mask(&s, 0, 0, 0);
        assert_eq!(
            mask.bbox,
            IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 10
            }
        );
        assert!(mask.contains(0, 0));
        assert!(mask.contains(4, 9));
        assert!(!mask.contains(5, 0), "must not cross into the blue half");
    }

    #[test]
    fn flood_mask_respects_tolerance_threshold() {
        let mut buf = make_buffer(4, 1, [0, 0, 0, 255]);
        // (2,0) はわずかに離れた色。
        buf[2 * 4..2 * 4 + 4].copy_from_slice(&[20, 20, 20, 255]);
        buf[3 * 4..3 * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
        let s = Surface {
            width: 4,
            height: 1,
            pixels: &mut buf,
            clip: None,
        };
        // tolerance 10: (2,0) の差 20 は許容値を超えるので選択は途切れる。
        let strict = flood_mask(&s, 0, 0, 10);
        assert!(strict.contains(0, 0));
        assert!(strict.contains(1, 0));
        assert!(!strict.contains(2, 0));
        assert!(!strict.contains(3, 0));

        // tolerance 30: 全域が許容範囲内なので繋がる。
        let loose = flood_mask(&s, 0, 0, 30);
        assert!(loose.contains(3, 0));
    }

    #[test]
    fn flood_mask_out_of_bounds_seed_does_not_panic() {
        let mut buf = make_buffer(4, 4, [1, 2, 3, 4]);
        let s = Surface {
            width: 4,
            height: 4,
            pixels: &mut buf,
            clip: None,
        };
        assert!(flood_mask(&s, -1, -1, 0).is_empty());
        assert!(flood_mask(&s, 100, 100, 0).is_empty());
    }

    #[test]
    fn flood_mask_on_zero_size_surface_does_not_panic() {
        let mut buf: Vec<u8> = Vec::new();
        let s = Surface {
            width: 0,
            height: 0,
            pixels: &mut buf,
            clip: None,
        };
        assert!(flood_mask(&s, 0, 0, 0).is_empty());
    }

    #[test]
    fn flood_mask_does_not_cross_the_clip_boundary() {
        let mut buf = make_buffer(10, 10, [255, 255, 255, 255]);
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 10,
            },
            mask: vec![255u8; 5 * 10],
        };
        let s = Surface {
            width: 10,
            height: 10,
            pixels: &mut buf,
            clip: Some(&clip),
        };
        let mask = flood_mask(&s, 0, 0, 0);
        assert!(mask.contains(4, 9));
        assert!(
            !mask.contains(5, 0),
            "the clip boundary must stop the selection even though the color matches"
        );
    }

    #[test]
    fn flood_mask_does_not_mutate_the_surface() {
        // flood_mask は選択マスクを返すだけで、flood_fill と違いピクセルは
        // 一切書き換えない。
        let mut buf = make_buffer(6, 6, [7, 8, 9, 255]);
        let original = buf.clone();
        let s = Surface {
            width: 6,
            height: 6,
            pixels: &mut buf,
            clip: None,
        };
        let _ = flood_mask(&s, 0, 0, 0);
        assert_eq!(buf, original);
    }

    #[test]
    fn flood_mask_4000x4000_is_correct_and_terminates() {
        // flood_fill_4000x4000_is_correct_and_terminates と同じ回帰検知
        // (デバッグビルドでの緩い上限)。
        let w = 4000usize;
        let h = 4000usize;
        let mut buf = vec![0u8; w * h * 4];
        for chunk in buf.chunks_exact_mut(4) {
            chunk.copy_from_slice(&[10, 20, 30, 255]);
        }
        let s = Surface {
            width: w as u32,
            height: h as u32,
            pixels: &mut buf,
            clip: None,
        };
        let start = std::time::Instant::now();
        let mask = flood_mask(&s, 0, 0, 0);
        let elapsed = start.elapsed();
        assert_eq!(
            mask.bbox,
            IRect {
                x0: 0,
                y0: 0,
                x1: w as i32,
                y1: h as i32
            }
        );
        assert!(
            elapsed.as_secs() < 10,
            "flood_mask took suspiciously long (possible regression): {elapsed:?}"
        );
    }

    // -- v4 §16.4/§23: グラデーション -----------------------------------------

    #[test]
    fn gradient_span_linear_is_zero_at_start_and_one_at_end() {
        let p0 = (0.0, 0.0);
        let p1 = (10.0, 0.0);
        assert_eq!(gradient_span(GradientKind::Linear, p0, p1, p0), 0.0);
        assert_eq!(gradient_span(GradientKind::Linear, p0, p1, p1), 1.0);
        assert!((gradient_span(GradientKind::Linear, p0, p1, (5.0, 0.0)) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn gradient_span_linear_clamps_before_start_and_after_end() {
        let p0 = (0.0, 0.0);
        let p1 = (10.0, 0.0);
        assert_eq!(
            gradient_span(GradientKind::Linear, p0, p1, (-5.0, 0.0)),
            0.0
        );
        assert_eq!(
            gradient_span(GradientKind::Linear, p0, p1, (15.0, 0.0)),
            1.0
        );
    }

    #[test]
    fn gradient_span_linear_ignores_perpendicular_offset() {
        // 線形は始点→終点の直線への正射影なので、直線に垂直な方向にどれだけ
        // 離れていても t は変わらない(SPEC §23 の「線形」の定義)。
        let p0 = (0.0, 0.0);
        let p1 = (10.0, 0.0);
        let on_axis = gradient_span(GradientKind::Linear, p0, p1, (5.0, 0.0));
        let off_axis = gradient_span(GradientKind::Linear, p0, p1, (5.0, 100.0));
        assert!((on_axis - off_axis).abs() < 1e-5);
    }

    #[test]
    fn gradient_span_radial_is_zero_at_center_and_one_at_radius() {
        let p0 = (10.0, 10.0);
        let p1 = (20.0, 10.0); // 半径 10。
        assert_eq!(gradient_span(GradientKind::Radial, p0, p1, p0), 0.0);
        assert!((gradient_span(GradientKind::Radial, p0, p1, p1) - 1.0).abs() < 1e-5);
        // 半径の半分の距離(方向は自由)は t=0.5。
        assert!((gradient_span(GradientKind::Radial, p0, p1, (10.0, 15.0)) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn gradient_span_radial_clamps_beyond_radius() {
        let p0 = (0.0, 0.0);
        let p1 = (5.0, 0.0);
        assert_eq!(
            gradient_span(GradientKind::Radial, p0, p1, (100.0, 0.0)),
            1.0
        );
    }

    #[test]
    fn gradient_span_degenerate_zero_length_drag_returns_zero() {
        let p0 = (3.0, 4.0);
        assert_eq!(gradient_span(GradientKind::Linear, p0, p0, (9.0, 9.0)), 0.0);
        assert_eq!(gradient_span(GradientKind::Radial, p0, p0, (9.0, 9.0)), 0.0);
    }

    #[test]
    fn lerp_color_endpoints_and_midpoint() {
        let c0 = [0, 0, 0, 255];
        let c1 = [255, 255, 255, 255];
        assert_eq!(lerp_color(c0, c1, 0.0), c0);
        assert_eq!(lerp_color(c0, c1, 1.0), c1);
        assert_eq!(lerp_color(c0, c1, 0.5), [128, 128, 128, 255]);
    }

    #[test]
    fn lerp_color_clamps_out_of_range_t() {
        let c0 = [10, 20, 30, 40];
        let c1 = [200, 150, 100, 250];
        assert_eq!(lerp_color(c0, c1, -1.0), c0);
        assert_eq!(lerp_color(c0, c1, 2.0), c1);
    }

    // -- v4 §16.5/§24: 色調補正 ------------------------------------------------

    #[test]
    fn invert_pixel_flips_rgb_and_keeps_alpha() {
        assert_eq!(invert_pixel([0, 128, 255, 200]), [255, 127, 0, 200]);
    }

    #[test]
    fn grayscale_pixel_uses_rec709_luma_and_keeps_alpha() {
        let px = grayscale_pixel([0, 255, 0, 123]);
        // 緑単色の Rec.709 輝度は 0.7152*255 ≈ 182。
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
        assert!((175..=190).contains(&(px[0] as i32)));
        assert_eq!(px[3], 123);
    }

    #[test]
    fn grayscale_pixel_of_gray_is_unchanged() {
        assert_eq!(grayscale_pixel([128, 128, 128, 255]), [128, 128, 128, 255]);
    }

    #[test]
    fn brightness_contrast_lut_is_identity_at_zero_zero() {
        let lut = brightness_contrast_lut(0, 0);
        for i in [0usize, 1, 64, 128, 200, 255] {
            assert_eq!(lut[i], i as u8, "identity mismatch at {i}");
        }
    }

    #[test]
    fn brightness_contrast_lut_max_brightness_pushes_toward_white() {
        let lut = brightness_contrast_lut(100, 0);
        assert_eq!(lut[0], 255);
        assert_eq!(lut[255], 255);
    }

    #[test]
    fn brightness_contrast_lut_min_brightness_pushes_toward_black() {
        let lut = brightness_contrast_lut(-100, 0);
        assert_eq!(lut[255], 0);
        assert_eq!(lut[0], 0);
    }

    #[test]
    fn brightness_contrast_lut_min_contrast_flattens_toward_mid_gray() {
        let lut = brightness_contrast_lut(0, -100);
        // コントラスト -100 は傾きがほぼ 0 になり、全画素が中間グレー付近に
        // 潰れる(SPEC §24 の「コントラスト」の直感どおり)。
        assert!((lut[0] as i32 - 128).abs() <= 2);
        assert!((lut[255] as i32 - 128).abs() <= 2);
    }

    #[test]
    fn brightness_contrast_lut_max_contrast_pushes_toward_extremes() {
        let lut = brightness_contrast_lut(0, 100);
        assert!(lut[200] > 200);
        assert!(lut[50] < 50);
    }

    #[test]
    fn brightness_contrast_lut_clamps_out_of_range_inputs() {
        let clamped = brightness_contrast_lut(500, -500);
        let exact = brightness_contrast_lut(100, -100);
        assert_eq!(clamped, exact);
    }

    #[test]
    fn apply_lut_pixel_preserves_alpha() {
        let lut = brightness_contrast_lut(0, 0);
        assert_eq!(apply_lut_pixel([10, 20, 30, 77], &lut), [10, 20, 30, 77]);
    }

    #[test]
    fn adjust_hsl_pixel_zero_delta_is_a_no_op_within_rounding() {
        let px = [12, 200, 90, 255];
        let out = adjust_hsl_pixel(px, 0, 0, 0);
        for i in 0..3 {
            assert!(
                (out[i] as i32 - px[i] as i32).abs() <= 1,
                "channel {i}: {} vs {}",
                out[i],
                px[i]
            );
        }
        assert_eq!(out[3], 255);
    }

    #[test]
    fn adjust_hsl_pixel_grayscale_input_is_immune_to_hue_shift() {
        // 彩度 0(グレー)は色相を変えても変化しない(HSL の定義どおり)。
        let px = [128, 128, 128, 255];
        let out = adjust_hsl_pixel(px, 90, 0, 0);
        assert_eq!(out, px);
    }

    #[test]
    fn adjust_hsl_pixel_lightness_100_is_white() {
        let px = [50, 60, 70, 255];
        let out = adjust_hsl_pixel(px, 0, 0, 100);
        assert_eq!(out, [255, 255, 255, 255]);
    }

    #[test]
    fn adjust_hsl_pixel_lightness_minus_100_is_black() {
        let px = [50, 60, 70, 255];
        let out = adjust_hsl_pixel(px, 0, 0, -100);
        assert_eq!(out, [0, 0, 0, 255]);
    }

    #[test]
    fn adjust_hsl_pixel_saturation_minus_100_desaturates() {
        let px = [220, 40, 40, 255]; // 彩度の高い赤。
        let out = adjust_hsl_pixel(px, 0, -100, 0);
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
    }

    #[test]
    fn adjust_hsl_pixel_preserves_alpha() {
        let out = adjust_hsl_pixel([10, 20, 30, 44], 45, 10, -10);
        assert_eq!(out[3], 44);
    }

    #[test]
    fn rgb_hsl_roundtrip_is_approximately_stable() {
        let samples = [
            (0, 0, 0),
            (255, 255, 255),
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (123, 45, 200),
            (10, 200, 150),
        ];
        for (r, g, b) in samples {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let (r2, g2, b2) = hsl_to_rgb(h, s, l);
            assert!((r as i32 - r2 as i32).abs() <= 1, "r: {r} vs {r2}");
            assert!((g as i32 - g2 as i32).abs() <= 1, "g: {g} vs {g2}");
            assert!((b as i32 - b2 as i32).abs() <= 1, "b: {b} vs {b2}");
        }
    }
}
