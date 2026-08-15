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
    /// v12 §50.3: アルファロック(透明部分の保護)。`clip` と全く同じ設計で、
    /// **書き込みの唯一の集約点である `set_pixel` だけがこれを見る** ため、
    /// ブラシ/鉛筆/消しゴム/図形/グラデーション/色調補正など `set_pixel` 経由で
    /// 書くすべての経路が自動的にロックへ従う(`flood_fill` の行スライス
    /// 直書きだけは別途 alpha-aware にしてある)。
    ///
    /// `Document::active_surface_mut` がアクティブレイヤーの
    /// `Layer::alpha_lock` をそのまま渡す。浮動片の確定合成・貼り付け・
    /// テキスト確定は `Surface` ではなく `Document::set_pixel` を通るため、
    /// SPEC §50.3 の「適用しない」がそのまま成立する。
    pub alpha_lock: bool,
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
    ///
    /// v12 §50.3(アルファロック): `alpha_lock` が立っているときは
    /// - 書き込み先の α が 0 の画素 → **RGBA とも完全に不変**(何も書かない。
    ///   「書いてから α を戻す」と透明画素の RGB が汚れるため、
    ///   ARCHITECTURE.md §22.8-3 でこの実装は禁止されている)、
    /// - α が 0 より大きい画素 → **α は元の値のまま**、RGB だけを書く。
    ///
    /// `set_pixel` は「画素を `color` で**置き換える**」経路(図形の塗り・
    /// 色調補正・浮動片以外の一括代入など)専用。ロック時はカバレッジ 1 の
    /// 補間、すなわち RGB を丸ごと `color` の RGB にするのが置き換えの
    /// 忠実な対応物になる(ロック無しでも RGB は `color` になる)。
    ///
    /// 部分カバレッジで**合成**する経路(ブラシ/鉛筆のスタンプ・
    /// グラデーション)は `blend_pixel` を使うこと — こちらは source-over の
    /// 結果 RGB(比率が `dst_a` に依存する)ではなく、SPEC §50.3 が要求する
    /// 「RGB のみカバレッジ補間」を行う。
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
            if self.alpha_lock {
                if slice[3] == 0 {
                    return;
                }
                slice[0..3].copy_from_slice(&color[0..3]);
                return;
            }
            slice.copy_from_slice(&color);
        }
    }

    /// 部分カバレッジの合成書き込み(ブラシ/鉛筆のスタンプ・グラデーションの
    /// 共通経路)。`base` は「合成の土台にする画素」(ストローク開始前の
    /// 元画素)、`src_rgb`/`coverage`(0.0–1.0、ブラシ不透明度・ソース色の α を
    /// 掛け込んだ**実効カバレッジ**)が塗る色と被覆率。
    ///
    /// - アルファロック無し: 従来どおり `blend_over(base, [src_rgb, coverage])`
    ///   の結果をそのまま書く(v11 までと 1 バイトも変わらない)。
    /// - アルファロック有り(SPEC §50.3): `dst_a == 0` は**計算前にスキップ**、
    ///   それ以外は α を元値のまま固定し
    ///   `out_rgb = dst_rgb + coverage × (src_rgb − dst_rgb)` を書く。
    ///   source-over の結果 RGB は `dst_a` に依存して塗り色へ寄る度合いが
    ///   変わってしまうため、ロック時に流用してはいけない(同じ RGB で α だけ
    ///   違う 2 画素に同じカバレッジで塗ったとき、結果 RGB が食い違う)。
    pub fn blend_pixel(&mut self, x: i32, y: i32, base: [u8; 4], src_rgb: [u8; 3], coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        if let Some(clip) = self.clip {
            if !clip.contains(x, y) {
                return;
            }
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        let Some(slice) = self.pixels.get_mut(idx..idx + 4) else {
            return;
        };
        if self.alpha_lock {
            if slice[3] == 0 {
                return;
            }
            let coverage = coverage.clamp(0.0, 1.0);
            for c in 0..3 {
                let dst = base[c] as f32;
                let value = dst + coverage * (src_rgb[c] as f32 - dst);
                slice[c] = value.round().clamp(0.0, 255.0) as u8;
            }
            return;
        }
        let alpha = (coverage.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        let blended = blend_over(base, [src_rgb[0], src_rgb[1], src_rgb[2], alpha]);
        slice.copy_from_slice(&blended);
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
        let alpha_lock = surface.alpha_lock;
        if let Some(row) = surface.pixels.get_mut(byte_start..byte_start + byte_len) {
            // v12 §50.3: 塗りつぶしは `set_pixel` を通らない直接代入なので、
            // ここで同じアルファロック規則(α=0 は完全不変・それ以外は α 固定
            // で RGB のみ)を実装する。ロック無しは従来どおりの一括代入。
            if alpha_lock {
                for px in row.chunks_exact_mut(4) {
                    if px[3] == 0 {
                        continue;
                    }
                    px[0..3].copy_from_slice(&color[0..3]);
                }
            } else {
                for px in row.chunks_exact_mut(4) {
                    px.copy_from_slice(&color);
                }
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
    flood_mask_impl(surface, x, y, tolerance).0
}

fn flood_mask_impl(surface: &Surface, x: i32, y: i32, tolerance: u8) -> (SelMask, usize) {
    let Some(target) = surface.get_pixel(x, y) else {
        return (SelMask::empty(), 0);
    };
    let w = surface.width as i32;
    let h = surface.height as i32;
    if w <= 0 || h <= 0 {
        return (SelMask::empty(), 0);
    }

    let Some(mut visited) = SparseVisited::new(surface.width, surface.height) else {
        return (SelMask::empty(), 0);
    };
    let is_open = |visited: &SparseVisited, x: i32, y: i32| {
        !visited.contains(x, y)
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
        for xx in xl..=xr {
            visited.insert(xx, sy);
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
        return (SelMask::empty(), visited.allocated_tile_count());
    };
    let bw = bbox.width() as usize;
    let bh = bbox.height() as usize;
    let Some(mask_len) = bw.checked_mul(bh) else {
        return (SelMask::empty(), visited.allocated_tile_count());
    };
    let mut mask = Vec::new();
    if mask.try_reserve_exact(mask_len).is_err() {
        return (SelMask::empty(), visited.allocated_tile_count());
    }
    mask.resize(mask_len, 0u8);
    for yy in 0..bh {
        let dst_row = yy * bw;
        for xx in 0..bw {
            mask[dst_row + xx] = if visited.contains(bbox.x0 + xx as i32, bbox.y0 + yy as i32) {
                255
            } else {
                0
            };
        }
    }
    (SelMask { bbox, mask }, visited.allocated_tile_count())
}

const VISITED_TILE_SIZE: usize = 64;

struct SparseVisited {
    tiles_x: usize,
    tiles: Vec<Option<Box<[u64; VISITED_TILE_SIZE]>>>,
    allocated_tiles: usize,
}

impl SparseVisited {
    fn new(width: u32, height: u32) -> Option<Self> {
        let tiles_x = width.div_ceil(VISITED_TILE_SIZE as u32) as usize;
        let tiles_y = height.div_ceil(VISITED_TILE_SIZE as u32) as usize;
        let tile_count = tiles_x.checked_mul(tiles_y)?;
        let mut tiles = Vec::new();
        tiles.try_reserve_exact(tile_count).ok()?;
        tiles.resize_with(tile_count, || None);
        Some(Self {
            tiles_x,
            tiles,
            allocated_tiles: 0,
        })
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        let x = x as usize;
        let y = y as usize;
        let tile_index = (y / VISITED_TILE_SIZE) * self.tiles_x + x / VISITED_TILE_SIZE;
        let local_y = y % VISITED_TILE_SIZE;
        let bit = 1u64 << (x % VISITED_TILE_SIZE);
        self.tiles[tile_index]
            .as_ref()
            .is_some_and(|rows| rows[local_y] & bit != 0)
    }

    fn insert(&mut self, x: i32, y: i32) {
        let x = x as usize;
        let y = y as usize;
        let tile_index = (y / VISITED_TILE_SIZE) * self.tiles_x + x / VISITED_TILE_SIZE;
        let local_y = y % VISITED_TILE_SIZE;
        let bit = 1u64 << (x % VISITED_TILE_SIZE);
        let tile = &mut self.tiles[tile_index];
        if tile.is_none() {
            self.allocated_tiles += 1;
        }
        let rows = tile.get_or_insert_with(|| Box::new([0; VISITED_TILE_SIZE]));
        rows[local_y] |= bit;
    }

    fn allocated_tile_count(&self) -> usize {
        self.allocated_tiles
    }
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

// ---------------------------------------------------------------------------
// v12 §51.1: モザイク(画像メニューの色調補正グループ)
// ---------------------------------------------------------------------------

/// SPEC §51.1: 「自動」チェック時のブロックサイズ
/// (`長辺 >= 400 ? max(4, 長辺 / 100) : 4`。mosaic_editor の審査基準準拠)。
///
/// 長辺は**画像全体**の長辺(選択範囲ではない)。0×0 でもパニックしない。
pub fn auto_block_size(width: u32, height: u32) -> u32 {
    let long_side = width.max(height);
    if long_side >= 400 {
        (long_side / 100).max(4)
    } else {
        4
    }
}

/// SPEC §51.1: 原点 (0,0) 固定格子のブロック平均によるモザイク。
///
/// - **格子は画像全体に固定**(`block` の倍数境界)。選択の形に依存しないので、
///   隣接する領域に 2 回かけても格子がズレない。
/// - **平均には選択マスク外の画素も含む**(ブロック全体の平均)。読み取りは
///   `Surface::get_pixel`(クリップを見ない)なのでこれが自然に成立する。
/// - **置換は選択内の画素のみ**: 書き込みは `Surface::set_pixel` を通すので、
///   選択クリップ(`Surface::clip`)とアルファロック(§50.3: α 保存・
///   `dst_a == 0` スキップ)が 1 箇所で効く。
/// - 端の欠けブロックは**実在画素のみ**で平均する(画像外は数えない)。
/// - 平均は **α 加重**: RGB は α を重みとする加重平均、α は単純平均。
///   ブロックが全透明なら透明黒(RGB も 0)。
///
/// ブロックは互いに素で、各ブロックは「読み切ってから書く」ので、`surface` を
/// その場で書き換えても他のブロックの平均は汚れない(プレビューの再計算は
/// 呼び出し側が元画素を復元してから呼ぶこと — `app.rs` のモーダル参照)。
///
/// 実際に触れた(クランプ済みの)矩形を返す。`block` が 0 のときは 1 として
/// 扱う(パニックしない)。
pub fn apply_mosaic(surface: &mut Surface, region: IRect, block: u32) -> IRect {
    let bounds = region.clamp_to(surface.width, surface.height);
    if bounds.is_empty() {
        return bounds;
    }
    let block = block.max(1) as i32;

    // 格子(画像原点固定)のうち `bounds` に掛かるものだけを回す。
    let first_bx = bounds.x0.div_euclid(block);
    let last_bx = (bounds.x1 - 1).div_euclid(block);
    let first_by = bounds.y0.div_euclid(block);
    let last_by = (bounds.y1 - 1).div_euclid(block);

    let width = surface.width as i32;
    let height = surface.height as i32;

    for by in first_by..=last_by {
        // 平均を取る範囲は「ブロック ∩ 画像」(マスクも region も見ない)。
        let avg_y0 = (by * block).max(0);
        let avg_y1 = ((by + 1) * block).min(height);
        for bx in first_bx..=last_bx {
            let avg_x0 = (bx * block).max(0);
            let avg_x1 = ((bx + 1) * block).min(width);
            if avg_x0 >= avg_x1 || avg_y0 >= avg_y1 {
                continue;
            }

            let mut sum_rgb = [0f32; 3];
            let mut sum_a = 0f32;
            let mut count = 0f32;
            for y in avg_y0..avg_y1 {
                for x in avg_x0..avg_x1 {
                    let Some(px) = surface.get_pixel(x, y) else {
                        continue;
                    };
                    let a = px[3] as f32 / 255.0;
                    sum_rgb[0] += px[0] as f32 * a;
                    sum_rgb[1] += px[1] as f32 * a;
                    sum_rgb[2] += px[2] as f32 * a;
                    sum_a += a;
                    count += 1.0;
                }
            }
            if count <= 0.0 {
                continue;
            }
            let alpha = (sum_a / count * 255.0).round().clamp(0.0, 255.0) as u8;
            let mut color = [0u8, 0, 0, alpha];
            if sum_a > 0.0 {
                for c in 0..3 {
                    color[c] = (sum_rgb[c] / sum_a).round().clamp(0.0, 255.0) as u8;
                }
            }
            // 書き込みは「ブロック ∩ 対象領域」だけ(選択クリップとアルファ
            // ロックは `set_pixel` が見る)。
            let write_x0 = avg_x0.max(bounds.x0);
            let write_x1 = avg_x1.min(bounds.x1);
            let write_y0 = avg_y0.max(bounds.y0);
            let write_y1 = avg_y1.min(bounds.y1);
            for y in write_y0..write_y1 {
                for x in write_x0..write_x1 {
                    surface.set_pixel(x, y, color);
                }
            }
        }
    }
    bounds
}

/// SPEC §51.1: モザイクのプレビュー・確定で使う対象領域
/// (`選択 bbox`(無ければ全面)を**格子境界へ外側拡張**した矩形)。
///
/// 格子平均が bbox 外の画素を含むため、スナップショット(CoW 退避)も
/// この拡張後の矩形で取る(ARCHITECTURE.md §22.2)。`block` が 0 でも
/// パニックしない。
pub fn mosaic_grid_aligned_rect(rect: IRect, block: u32, width: u32, height: u32) -> IRect {
    let rect = rect.clamp_to(width, height);
    if rect.is_empty() {
        return rect;
    }
    let block = block.max(1) as i32;
    IRect {
        x0: rect.x0.div_euclid(block) * block,
        y0: rect.y0.div_euclid(block) * block,
        x1: (rect.x1 - 1).div_euclid(block) * block + block,
        y1: (rect.y1 - 1).div_euclid(block) * block + block,
    }
    .clamp_to(width, height)
}

// ---------------------------------------------------------------------------
// v12 §50.1: レイヤーサムネイルの縮小(ui/layers_panel.rs が使う純関数)
// ---------------------------------------------------------------------------

/// サムネイルに焼き込む市松模様の 1 マス(サムネイル画素単位)。
const THUMBNAIL_CHECKER_CELL: u32 = 4;
/// 市松の明色・暗色(`canvas_view::draw_checkerboard` と同じ階調)。
const THUMBNAIL_CHECKER_LIGHT: [u8; 3] = [205, 205, 205];
const THUMBNAIL_CHECKER_DARK: [u8; 3] = [165, 165, 165];

/// 1 出力画素あたりに読む元画素の最大数(軸ごと)。
///
/// box 平均は本来セル内の全画素を読むが、それでは 8192×8192 のレイヤー 1 枚の
/// サムネイル 1 枚に 6700 万画素の読み出しが必要になり、ストローク確定のたびに
/// 数百 ms の停止を招く(SPEC §0 の軽さ・§50.1 の「毎フレーム禁止」の趣旨に
/// 反する)。セル内を等間隔に間引いて最大 8×8=64 点の平均にすることで、
/// 元画像の大きさによらず 1 枚あたりの計算量を `tw*th*64` 以下に抑える。
/// セルが 8 画素以下(= 40×30 のサムネイルなら 320×240 までの元画像)の
/// ときは間引きが起きず、厳密な box 平均と完全に一致する。
const THUMBNAIL_MAX_SAMPLES_PER_AXIS: u32 = 8;

/// SPEC §50.1: レイヤーサムネイル用の縮小(box 平均・縦横比維持・市松下地の
/// 焼き込み)。戻り値は `(tw, th, RGBA バイト列)` で、α は常に 255
/// (市松を焼き込み済みなので、テクスチャ側で透明合成する必要がない)。
///
/// - 拡大はしない(元が `max_w`×`max_h` 以下ならそのままの寸法)。
/// - 平均は premultiplied で行い(半透明画素の RGB が過大評価されない)、
///   出力直前に straight へ戻してから市松の上に載せる。
/// - `pixels` の長さが足りない・寸法が 0 のときは空(`(0, 0, vec![])`)を返す
///   (パニックしない、CLAUDE.md 鉄則)。
pub fn thumbnail_rgba(
    pixels: &[u8],
    width: u32,
    height: u32,
    max_w: u32,
    max_h: u32,
) -> (u32, u32, Vec<u8>) {
    let empty = (0, 0, Vec::new());
    if width == 0 || height == 0 || max_w == 0 || max_h == 0 {
        return empty;
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(4));
    if expected.is_none_or(|len| pixels.len() < len) {
        return empty;
    }

    // 縦横比を保ったまま max_w×max_h に収める(拡大はしない)。
    let (tw, th) = if width <= max_w && height <= max_h {
        (width, height)
    } else {
        let scale = (max_w as f32 / width as f32).min(max_h as f32 / height as f32);
        (
            ((width as f32 * scale).round() as u32).clamp(1, max_w),
            ((height as f32 * scale).round() as u32).clamp(1, max_h),
        )
    };

    let mut out = vec![0u8; tw as usize * th as usize * 4];
    let w = width as u64;
    let h = height as u64;
    for oy in 0..th {
        let y0 = (oy as u64 * h / th as u64) as u32;
        let y1 = (((oy as u64 + 1) * h / th as u64) as u32)
            .max(y0 + 1)
            .min(height);
        let step_y = ((y1 - y0) / THUMBNAIL_MAX_SAMPLES_PER_AXIS).max(1);
        for ox in 0..tw {
            let x0 = (ox as u64 * w / tw as u64) as u32;
            let x1 = (((ox as u64 + 1) * w / tw as u64) as u32)
                .max(x0 + 1)
                .min(width);
            let step_x = ((x1 - x0) / THUMBNAIL_MAX_SAMPLES_PER_AXIS).max(1);

            let mut sum = [0f32; 3];
            let mut sum_a = 0f32;
            let mut count = 0f32;
            let mut y = y0;
            while y < y1 {
                let row = y as usize * width as usize;
                let mut x = x0;
                while x < x1 {
                    let idx = (row + x as usize) * 4;
                    if let Some(px) = pixels.get(idx..idx + 4) {
                        let a = px[3] as f32 / 255.0;
                        sum[0] += px[0] as f32 * a;
                        sum[1] += px[1] as f32 * a;
                        sum[2] += px[2] as f32 * a;
                        sum_a += a;
                    }
                    count += 1.0;
                    x += step_x;
                }
                y += step_y;
            }

            let alpha = if count > 0.0 { sum_a / count } else { 0.0 };
            let checker = if ((ox / THUMBNAIL_CHECKER_CELL) + (oy / THUMBNAIL_CHECKER_CELL))
                .is_multiple_of(2)
            {
                THUMBNAIL_CHECKER_LIGHT
            } else {
                THUMBNAIL_CHECKER_DARK
            };
            let out_idx = (oy as usize * tw as usize + ox as usize) * 4;
            for c in 0..3 {
                // premultiplied 平均 → straight へ戻す → 市松の上に載せる。
                let straight = if sum_a > 0.0 { sum[c] / sum_a } else { 0.0 };
                let value = straight * alpha + checker[c] as f32 * (1.0 - alpha);
                out[out_idx + c] = value.round().clamp(0.0, 255.0) as u8;
            }
            out[out_idx + 3] = 255;
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_flood_mask(surface: &Surface, x: i32, y: i32, tolerance: u8) -> SelMask {
        let Some(target) = surface.get_pixel(x, y) else {
            return SelMask::empty();
        };
        let width = surface.width as i32;
        let height = surface.height as i32;
        if width <= 0 || height <= 0 {
            return SelMask::empty();
        }
        let mut visited = vec![false; width as usize * height as usize];
        let index = |x: i32, y: i32| y as usize * width as usize + x as usize;
        let is_open = |visited: &[bool], x: i32, y: i32| {
            !visited[index(x, y)]
                && surface.clip.is_none_or(|clip| clip.contains(x, y))
                && surface
                    .get_pixel(x, y)
                    .is_some_and(|pixel| color_within_tolerance(pixel, target, tolerance))
        };
        let mut stack = vec![(x, y)];
        let mut bounds: Option<IRect> = None;
        while let Some((seed_x, seed_y)) = stack.pop() {
            if !is_open(&visited, seed_x, seed_y) {
                continue;
            }
            let mut left = seed_x;
            while left > 0 && is_open(&visited, left - 1, seed_y) {
                left -= 1;
            }
            let mut right = seed_x;
            while right + 1 < width && is_open(&visited, right + 1, seed_y) {
                right += 1;
            }
            let span_rect = IRect {
                x0: left,
                y0: seed_y,
                x1: right + 1,
                y1: seed_y + 1,
            };
            let span_start = seed_y as usize * width as usize + left as usize;
            let span_len = (right - left + 1) as usize;
            visited[span_start..span_start + span_len].fill(true);
            bounds = Some(bounds.map_or(span_rect, |bounds| bounds.union(&span_rect)));
            for next_y in [seed_y - 1, seed_y + 1] {
                if next_y < 0 || next_y >= height {
                    continue;
                }
                let mut in_run = false;
                for next_x in left..=right {
                    if is_open(&visited, next_x, next_y) {
                        if !in_run {
                            stack.push((next_x, next_y));
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
        let bbox_width = bbox.width() as usize;
        let bbox_height = bbox.height() as usize;
        let mut mask = vec![0; bbox_width * bbox_height];
        for mask_y in 0..bbox_height {
            for mask_x in 0..bbox_width {
                let source_x = bbox.x0 + mask_x as i32;
                let source_y = bbox.y0 + mask_y as i32;
                if visited[index(source_x, source_y)] {
                    mask[mask_y * bbox_width + mask_x] = 255;
                }
            }
        }
        SelMask { bbox, mask }
    }

    fn assert_masks_equal(actual: &SelMask, expected: &SelMask) {
        assert_eq!(actual.bbox, expected.bbox);
        assert_eq!(actual.mask, expected.mask);
    }

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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
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
            alpha_lock: false,
        };
        let _ = flood_mask(&s, 0, 0, 0);
        assert_eq!(buf, original);
    }

    #[test]
    fn sparse_visited_allocates_only_touched_tiles() {
        let Some(mut visited) = SparseVisited::new(128, 64) else {
            panic!("visited tile grid allocation failed");
        };
        visited.insert(0, 0);
        visited.insert(63, 63);
        assert_eq!(visited.allocated_tile_count(), 1);
        visited.insert(64, 0);
        assert_eq!(visited.allocated_tile_count(), 2);
        assert!(visited.contains(63, 63));
        assert!(!visited.contains(62, 63));
    }

    #[test]
    fn flood_mask_matches_dense_reference_on_random_images() {
        let mut state = 0x9e37_79b9u32;
        for case in 0..40 {
            let width = 5 + case % 12;
            let height = 4 + case % 9;
            let mut buffer = vec![0u8; width as usize * height as usize * 4];
            for pixel in buffer.chunks_exact_mut(4) {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                pixel.copy_from_slice(&[
                    (state >> 24) as u8,
                    (state >> 16) as u8,
                    (state >> 8) as u8,
                    255,
                ]);
            }
            let start_x = (state % width) as i32;
            let start_y = (state.rotate_left(7) % height) as i32;
            let tolerance = (state >> 27) as u8 * 8;
            let surface = Surface {
                width,
                height,
                pixels: &mut buffer,
                clip: None,
                alpha_lock: false,
            };
            let actual = flood_mask(&surface, start_x, start_y, tolerance);
            let expected = reference_flood_mask(&surface, start_x, start_y, tolerance);
            assert_masks_equal(&actual, &expected);
        }
    }

    #[test]
    fn flood_mask_includes_exact_tolerance_boundary() {
        let mut buffer = vec![10, 20, 30, 255, 20, 10, 40, 245, 21, 20, 30, 255];
        let surface = Surface {
            width: 3,
            height: 1,
            pixels: &mut buffer,
            clip: None,
            alpha_lock: false,
        };
        let mask = flood_mask(&surface, 0, 0, 10);
        assert_eq!(mask.bbox.x1, 2);
        assert!(mask.contains(1, 0));
        assert!(!mask.contains(2, 0));
    }

    #[test]
    fn flood_mask_thin_large_bbox_matches_reference_and_tile_count() {
        let width = 4097u32;
        let mut buffer = make_buffer(width, 1, [3, 4, 5, 255]);
        let surface = Surface {
            width,
            height: 1,
            pixels: &mut buffer,
            clip: None,
            alpha_lock: false,
        };
        let (actual, tile_count) = flood_mask_impl(&surface, 0, 0, 0);
        let expected = reference_flood_mask(&surface, 0, 0, 0);
        assert_masks_equal(&actual, &expected);
        assert_eq!(tile_count, 65);
    }

    #[test]
    fn flood_mask_respects_clip_hole() {
        let mut buffer = make_buffer(130, 2, [8, 9, 10, 255]);
        let mut clip_mask = vec![255u8; 130 * 2];
        clip_mask[64] = 0;
        clip_mask[130 + 64] = 0;
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 130,
                y1: 2,
            },
            mask: clip_mask,
        };
        let surface = Surface {
            width: 130,
            height: 2,
            pixels: &mut buffer,
            clip: Some(&clip),
            alpha_lock: false,
        };
        let actual = flood_mask(&surface, 0, 0, 0);
        let expected = reference_flood_mask(&surface, 0, 0, 0);
        assert_masks_equal(&actual, &expected);
        assert_eq!(actual.bbox.x1, 64);
    }

    #[test]
    fn flood_mask_tile_boundary_and_small_region_allocate_only_touched_tiles() {
        let mut boundary_buffer = make_buffer(66, 1, [0, 0, 0, 255]);
        for x in 63..=64 {
            let index = x * 4;
            boundary_buffer[index..index + 4].copy_from_slice(&[7, 7, 7, 255]);
        }
        let boundary_surface = Surface {
            width: 66,
            height: 1,
            pixels: &mut boundary_buffer,
            clip: None,
            alpha_lock: false,
        };
        let (boundary_mask, boundary_tiles) = flood_mask_impl(&boundary_surface, 63, 0, 0);
        assert_eq!(boundary_mask.mask, vec![255, 255]);
        assert_eq!(boundary_tiles, 2);

        let mut large_buffer = make_buffer(512, 512, [0, 0, 0, 255]);
        for y in 10..12 {
            for x in 10..12 {
                let index = (y * 512 + x) * 4;
                large_buffer[index..index + 4].copy_from_slice(&[1, 2, 3, 255]);
            }
        }
        let large_surface = Surface {
            width: 512,
            height: 512,
            pixels: &mut large_buffer,
            clip: None,
            alpha_lock: false,
        };
        let (mask, tile_count) = flood_mask_impl(&large_surface, 10, 10, 0);
        assert_eq!(mask.mask, vec![255; 4]);
        assert_eq!(tile_count, 1);
    }

    #[test]
    fn flood_mask_sparse_and_dense_performance_observation() {
        let width = 1024u32;
        let height = 1024u32;
        let mut buffer = make_buffer(width, height, [21, 22, 23, 255]);
        let surface = Surface {
            width,
            height,
            pixels: &mut buffer,
            clip: None,
            alpha_lock: false,
        };
        let sparse_start = std::time::Instant::now();
        let sparse = flood_mask(&surface, 0, 0, 0);
        let sparse_elapsed = sparse_start.elapsed();
        let dense_start = std::time::Instant::now();
        let dense = reference_flood_mask(&surface, 0, 0, 0);
        let dense_elapsed = dense_start.elapsed();
        assert_masks_equal(&sparse, &dense);
        eprintln!(
            "flood_mask 1024x1024 full region: sparse={sparse_elapsed:?}, dense={dense_elapsed:?}"
        );
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
            alpha_lock: false,
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
        eprintln!("flood_mask 4000x4000 full region: sparse={elapsed:?}");
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

    // -- v12 §50.3: アルファロック(透明部分の保護)----------------------

    /// `set_pixel` は書き込みの唯一の集約点なので、ここが正しければ
    /// ブラシ/鉛筆/図形/グラデーション/色調補正のすべてが従う。
    #[test]
    fn alpha_lock_skips_fully_transparent_pixels_entirely() {
        let mut buf = make_buffer(2, 1, [0, 0, 0, 0]);
        buf[4..8].copy_from_slice(&[10, 20, 30, 128]); // (1,0) は半透明。
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        s.set_pixel(0, 0, [200, 200, 200, 255]);
        assert_eq!(
            s.get_pixel(0, 0),
            Some([0, 0, 0, 0]),
            "dst_a==0 は RGBA とも完全不変(RGB を汚さない)"
        );
        s.set_pixel(1, 0, [200, 210, 220, 255]);
        assert_eq!(
            s.get_pixel(1, 0),
            Some([200, 210, 220, 128]),
            "dst_a>0 は α を保ったまま RGB だけ書く"
        );
    }

    #[test]
    fn alpha_lock_off_writes_rgba_exactly_as_before() {
        let mut buf = make_buffer(1, 1, [0, 0, 0, 0]);
        let mut s = Surface {
            width: 1,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: false,
        };
        s.set_pixel(0, 0, [200, 210, 220, 255]);
        assert_eq!(s.get_pixel(0, 0), Some([200, 210, 220, 255]));
    }

    #[test]
    fn alpha_lock_combines_with_the_selection_clip() {
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            mask: vec![255u8],
        };
        let mut buf = make_buffer(2, 1, [5, 5, 5, 200]);
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: Some(&clip),
            alpha_lock: true,
        };
        s.set_pixel(0, 0, [1, 2, 3, 255]);
        s.set_pixel(1, 0, [1, 2, 3, 255]);
        assert_eq!(s.get_pixel(0, 0), Some([1, 2, 3, 200]));
        assert_eq!(s.get_pixel(1, 0), Some([5, 5, 5, 200]), "クリップ外は不変");
    }

    /// SPEC §50.3: 「塗りつぶしの直接書き込み経路も alpha-aware に変更する」。
    #[test]
    fn flood_fill_is_alpha_aware_when_the_layer_is_alpha_locked() {
        // 左半分だけ不透明な帯。塗りつぶしは全域(許容値 255 で連結)を
        // 走査するが、透明画素は 1 バイトも変えてはいけない。
        let mut buf = make_buffer(4, 1, [0, 0, 0, 0]);
        for x in 0..2 {
            buf[x * 4..x * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
        let mut s = Surface {
            width: 4,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        flood_fill(&mut s, 0, 0, [255, 0, 0, 255], 255, |_, _| {});
        assert_eq!(s.get_pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(s.get_pixel(1, 0), Some([255, 0, 0, 255]));
        assert_eq!(
            s.get_pixel(2, 0),
            Some([0, 0, 0, 0]),
            "透明画素は RGBA とも不変"
        );
        assert_eq!(s.get_pixel(3, 0), Some([0, 0, 0, 0]));
    }

    #[test]
    fn flood_fill_preserves_alpha_of_semi_transparent_pixels_when_locked() {
        let mut buf = make_buffer(2, 1, [10, 10, 10, 100]);
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        flood_fill(&mut s, 0, 0, [1, 2, 3, 255], 0, |_, _| {});
        assert_eq!(s.get_pixel(0, 0), Some([1, 2, 3, 100]));
        assert_eq!(s.get_pixel(1, 0), Some([1, 2, 3, 100]));
    }

    /// v12 §50.3(追いレビュー①): ロック時の RGB は**カバレッジ補間**で
    /// あり、source-over の結果 RGB(比率が `dst_a` に依存する)ではない。
    /// 同じ RGB で α だけ違う 2 画素に同じカバレッジで塗ったら、出力 RGB は
    /// 一致し、α はそれぞれ元の値のまま残らなければならない。
    #[test]
    fn alpha_lock_interpolates_rgb_by_coverage_independently_of_dst_alpha() {
        let mut buf = make_buffer(2, 1, [100, 100, 100, 0]);
        buf[3] = 64;
        buf[7] = 192;
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        let src_rgb = [200, 0, 0];
        s.blend_pixel(0, 0, [100, 100, 100, 64], src_rgb, 0.5);
        s.blend_pixel(1, 0, [100, 100, 100, 192], src_rgb, 0.5);

        // 100 + 0.5*(200-100) = 150 / 100 + 0.5*(0-100) = 50
        assert_eq!(s.get_pixel(0, 0), Some([150, 50, 50, 64]));
        assert_eq!(s.get_pixel(1, 0), Some([150, 50, 50, 192]));

        // 参考: source-over の結果 RGB を流用していた旧実装だと、この 2 画素の
        // RGB は一致しない(dst_a に依存して塗り色へ寄る度合いが変わる)。
        let over_low = blend_over([100, 100, 100, 64], [200, 0, 0, 128]);
        let over_high = blend_over([100, 100, 100, 192], [200, 0, 0, 128]);
        assert_ne!(over_low[0], over_high[0]);
    }

    #[test]
    fn alpha_lock_blend_pixel_skips_transparent_and_clamps_coverage() {
        let mut buf = make_buffer(3, 1, [10, 20, 30, 0]);
        buf[7] = 255; // (1,0) は不透明。
        buf[11] = 128; // (2,0) は半透明。
        let mut s = Surface {
            width: 3,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        s.blend_pixel(0, 0, [10, 20, 30, 0], [255, 255, 255], 1.0);
        assert_eq!(
            s.get_pixel(0, 0),
            Some([10, 20, 30, 0]),
            "dst_a==0 は計算前にスキップ(RGB を汚さない)"
        );
        s.blend_pixel(1, 0, [10, 20, 30, 255], [255, 255, 255], 2.0);
        assert_eq!(
            s.get_pixel(1, 0),
            Some([255, 255, 255, 255]),
            "カバレッジは 0..1 にクランプされる"
        );
        s.blend_pixel(2, 0, [10, 20, 30, 128], [255, 255, 255], 0.0);
        assert_eq!(
            s.get_pixel(2, 0),
            Some([10, 20, 30, 128]),
            "カバレッジ 0 は元色のまま"
        );
    }

    /// ロックが無いときの `blend_pixel` は従来の `blend_over` と 1 バイトも
    /// 変わらない(ブラシ・グラデーションの既存挙動の回帰)。
    #[test]
    fn blend_pixel_without_alpha_lock_matches_blend_over_exactly() {
        for base in [[10u8, 200, 30, 255], [0, 0, 0, 0], [90, 90, 90, 128]] {
            for src in [[200u8, 20, 60, 255], [200, 20, 60, 128], [1, 2, 3, 0]] {
                let mut buf = base.to_vec();
                let mut s = Surface {
                    width: 1,
                    height: 1,
                    pixels: &mut buf,
                    clip: None,
                    alpha_lock: false,
                };
                s.blend_pixel(0, 0, base, [src[0], src[1], src[2]], src[3] as f32 / 255.0);
                assert_eq!(
                    s.get_pixel(0, 0),
                    Some(blend_over(base, src)),
                    "base={base:?} src={src:?}"
                );
            }
        }
    }

    // -- v12 §51.1: モザイク ---------------------------------------------

    /// テスト用: クリップ・ロック無しの `Surface` でモザイクをかける。
    fn mosaic_on(pixels: &mut Vec<u8>, w: u32, h: u32, region: IRect, block: u32) {
        let mut s = Surface {
            width: w,
            height: h,
            pixels,
            clip: None,
            alpha_lock: false,
        };
        apply_mosaic(&mut s, region, block);
    }

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn auto_block_size_matches_the_spec_thresholds() {
        // SPEC §51.1: 長辺 >= 400 なら max(4, 長辺/100)、未満なら 4。
        assert_eq!(auto_block_size(399, 100), 4);
        assert_eq!(auto_block_size(400, 100), 4, "400/100 = 4");
        assert_eq!(auto_block_size(499, 100), 4, "499/100 = 4(切り捨て)");
        assert_eq!(auto_block_size(500, 100), 5);
        // 長辺は幅・高さの大きい方。
        assert_eq!(auto_block_size(100, 500), 5);
        assert_eq!(auto_block_size(4000, 3000), 40);
        // 0×0 でもパニックせず既定値。
        assert_eq!(auto_block_size(0, 0), 4);
    }

    #[test]
    fn mosaic_averages_each_origin_aligned_block() {
        // 4×2 の画像を block=2 で。格子は (0,0) 起点なので
        // [0..2)x[0..2) と [2..4)x[0..2) の 2 ブロックになる。
        let mut buf = Vec::new();
        for v in [0u8, 100, 200, 255, 0, 100, 200, 255] {
            buf.extend_from_slice(&[v, v, v, 255]);
        }
        mosaic_on(
            &mut buf,
            4,
            2,
            IRect {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 2,
            },
            2,
        );
        // 左ブロック = (0+100+0+100)/4 = 50、右ブロック = (200+255+200+255)/4 = 227.5 → 228
        for (x, expected) in [(0u32, 50u8), (1, 50), (2, 228), (3, 228)] {
            assert_eq!(px(&buf, 4, x, 0)[0], expected, "x={x}");
            assert_eq!(px(&buf, 4, x, 1)[0], expected, "x={x}");
        }
    }

    #[test]
    fn mosaic_grid_is_anchored_at_the_image_origin_not_the_region() {
        // region が格子境界からずれていても、格子自体は (0,0) 固定。
        // 6×1・block=3 → ブロックは [0..3) と [3..6)。region=[2..5) を
        // 指定すると、x=2 は左ブロックの平均、x=3,4 は右ブロックの平均になる。
        let mut buf = Vec::new();
        for v in [0u8, 30, 60, 90, 120, 150] {
            buf.extend_from_slice(&[v, v, v, 255]);
        }
        mosaic_on(
            &mut buf,
            6,
            1,
            IRect {
                x0: 2,
                y0: 0,
                x1: 5,
                y1: 1,
            },
            3,
        );
        assert_eq!(px(&buf, 6, 0, 0)[0], 0, "region 外は不変");
        assert_eq!(px(&buf, 6, 1, 0)[0], 30, "region 外は不変");
        assert_eq!(px(&buf, 6, 2, 0)[0], 30, "左ブロック平均 (0+30+60)/3");
        assert_eq!(px(&buf, 6, 3, 0)[0], 120, "右ブロック平均 (90+120+150)/3");
        assert_eq!(px(&buf, 6, 4, 0)[0], 120);
        assert_eq!(px(&buf, 6, 5, 0)[0], 150, "region 外は不変");
    }

    #[test]
    fn mosaic_edge_block_averages_only_real_pixels() {
        // 5×1・block=3 → 端のブロックは 2 画素しかない。画像外は数えない。
        let mut buf = Vec::new();
        for v in [0u8, 0, 0, 100, 200] {
            buf.extend_from_slice(&[v, v, v, 255]);
        }
        mosaic_on(
            &mut buf,
            5,
            1,
            IRect {
                x0: 0,
                y0: 0,
                x1: 5,
                y1: 1,
            },
            3,
        );
        assert_eq!(px(&buf, 5, 0, 0)[0], 0);
        assert_eq!(px(&buf, 5, 3, 0)[0], 150, "(100+200)/2 = 150");
        assert_eq!(px(&buf, 5, 4, 0)[0], 150);
    }

    #[test]
    fn mosaic_average_is_alpha_weighted_and_fully_transparent_blocks_stay_clear() {
        // 2×1・block=2: 不透明な赤 + 完全透明な緑。
        let mut buf = vec![255, 0, 0, 255, 0, 255, 0, 0];
        mosaic_on(
            &mut buf,
            2,
            1,
            IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
            2,
        );
        // RGB は α 重み(透明な緑は寄与しない)→ 赤のまま。α は単純平均 = 128。
        assert_eq!(px(&buf, 2, 0, 0), [255, 0, 0, 128]);
        assert_eq!(px(&buf, 2, 1, 0), [255, 0, 0, 128]);

        // 全透明ブロックは透明黒(RGB も 0)。
        let mut clear = vec![9, 9, 9, 0, 8, 8, 8, 0];
        mosaic_on(
            &mut clear,
            2,
            1,
            IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
            2,
        );
        assert_eq!(px(&clear, 2, 0, 0), [0, 0, 0, 0]);
        assert_eq!(px(&clear, 2, 1, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn mosaic_replaces_only_selected_pixels_but_averages_across_the_whole_block() {
        // 2×1・block=2、選択は左の 1 画素だけ。平均は右(非選択)も含む。
        let mut buf = vec![0, 0, 0, 255, 200, 200, 200, 255];
        let clip = crate::document::SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            mask: vec![255u8],
        };
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: Some(&clip),
            alpha_lock: false,
        };
        apply_mosaic(
            &mut s,
            IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
            2,
        );
        assert_eq!(px(&buf, 2, 0, 0)[0], 100, "選択内は平均 (0+200)/2 で置換");
        assert_eq!(px(&buf, 2, 1, 0)[0], 200, "選択外は 1 バイトも変えない");
    }

    #[test]
    fn mosaic_respects_the_alpha_lock() {
        // v12 §50.3: α 保存・dst_a==0 は完全スキップ。
        let mut buf = vec![0, 0, 0, 0, 200, 200, 200, 128];
        let mut s = Surface {
            width: 2,
            height: 1,
            pixels: &mut buf,
            clip: None,
            alpha_lock: true,
        };
        apply_mosaic(
            &mut s,
            IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
            2,
        );
        assert_eq!(px(&buf, 2, 0, 0), [0, 0, 0, 0], "透明画素は不変");
        let after = px(&buf, 2, 1, 0);
        assert_eq!(after[3], 128, "α は元値のまま");
        assert_eq!(after[0], 200, "透明画素は平均へ寄与しない(α 加重)");
    }

    #[test]
    fn mosaic_degenerate_inputs_do_not_panic() {
        let mut buf = make_buffer(2, 2, [1, 2, 3, 255]);
        // block=0 は 1 として扱う(= 実質そのまま)。
        mosaic_on(
            &mut buf,
            2,
            2,
            IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2,
            },
            0,
        );
        assert_eq!(px(&buf, 2, 0, 0), [1, 2, 3, 255]);
        // 空の region・範囲外の region。
        let empty = IRect {
            x0: 5,
            y0: 5,
            x1: 5,
            y1: 5,
        };
        mosaic_on(&mut buf, 2, 2, empty, 4);
        mosaic_on(
            &mut buf,
            2,
            2,
            IRect {
                x0: -10,
                y0: -10,
                x1: 100,
                y1: 100,
            },
            4,
        );
    }

    #[test]
    fn mosaic_grid_aligned_rect_expands_outward_to_block_boundaries() {
        let doc = (100u32, 100u32);
        let r = IRect {
            x0: 10,
            y0: 21,
            x1: 33,
            y1: 44,
        };
        let expanded = mosaic_grid_aligned_rect(r, 10, doc.0, doc.1);
        assert_eq!((expanded.x0, expanded.y0), (10, 20));
        assert_eq!((expanded.x1, expanded.y1), (40, 50));
        // 画像境界でクランプされる。
        let edge = mosaic_grid_aligned_rect(
            IRect {
                x0: 95,
                y0: 95,
                x1: 100,
                y1: 100,
            },
            32,
            doc.0,
            doc.1,
        );
        assert_eq!((edge.x0, edge.y0, edge.x1, edge.y1), (64, 64, 100, 100));
        // 空矩形・block=0 でもパニックしない。
        assert!(mosaic_grid_aligned_rect(
            IRect {
                x0: 5,
                y0: 5,
                x1: 5,
                y1: 5
            },
            8,
            doc.0,
            doc.1
        )
        .is_empty());
        let z = mosaic_grid_aligned_rect(
            IRect {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 4,
            },
            0,
            doc.0,
            doc.1,
        );
        assert_eq!((z.x0, z.y0, z.x1, z.y1), (0, 0, 4, 4));
    }

    /// 4000×4000 の全面モザイクが現実的な時間で終わること(1 回の適用が
    /// 画素数に比例することの回帰検知)。ブロックが最小(2px = 最悪ケース:
    /// ブロック数が最大になる)でも同様であることも確認する。
    #[test]
    fn mosaic_full_4000x4000_terminates_quickly() {
        for block in [40u32, 2] {
            let mut buf = make_buffer(4000, 4000, [10, 20, 30, 255]);
            let start = std::time::Instant::now();
            mosaic_on(
                &mut buf,
                4000,
                4000,
                IRect {
                    x0: 0,
                    y0: 0,
                    x1: 4000,
                    y1: 4000,
                },
                block,
            );
            let elapsed = start.elapsed();
            assert_eq!(px(&buf, 4000, 0, 0), [10, 20, 30, 255]);
            assert!(
                elapsed < std::time::Duration::from_secs(5),
                "4000x4000・block={block} のモザイクに {elapsed:?} もかかっている"
            );
        }
    }

    // -- v12 §50.1: サムネイル縮小(`thumbnail_rgba`)---------------------

    #[test]
    fn thumbnail_keeps_aspect_ratio_and_never_upscales() {
        let (tw, th, _) = thumbnail_rgba(&make_buffer(80, 60, [0, 0, 0, 255]), 80, 60, 40, 30);
        assert_eq!((tw, th), (40, 30));
        // 横長: 幅が上限に張り付き、高さは比率で決まる。
        let (tw, th, _) = thumbnail_rgba(&make_buffer(400, 100, [0, 0, 0, 255]), 400, 100, 40, 30);
        assert_eq!((tw, th), (40, 10));
        // 縦長: 高さが上限に張り付く。
        let (tw, th, _) = thumbnail_rgba(&make_buffer(100, 400, [0, 0, 0, 255]), 100, 400, 40, 30);
        assert_eq!((tw, th), (8, 30));
        // 小さい画像は拡大しない。
        let (tw, th, _) = thumbnail_rgba(&make_buffer(5, 4, [0, 0, 0, 255]), 5, 4, 40, 30);
        assert_eq!((tw, th), (5, 4));
    }

    #[test]
    fn thumbnail_of_an_opaque_flat_color_is_that_color_everywhere() {
        let (tw, th, out) = thumbnail_rgba(&make_buffer(64, 64, [12, 34, 56, 255]), 64, 64, 40, 30);
        assert_eq!((tw, th), (30, 30));
        assert!(out.chunks_exact(4).all(|p| p == [12, 34, 56, 255]));
    }

    #[test]
    fn thumbnail_of_a_fully_transparent_layer_is_the_checkerboard() {
        let (tw, th, out) = thumbnail_rgba(&make_buffer(8, 8, [9, 9, 9, 0]), 8, 8, 40, 30);
        assert_eq!((tw, th), (8, 8));
        // 市松の 2 色だけが現れ、α は常に 255(下地を焼き込み済み)。
        assert!(out.chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(&out[0..3], &THUMBNAIL_CHECKER_LIGHT);
        let idx = THUMBNAIL_CHECKER_CELL as usize * 4;
        assert_eq!(&out[idx..idx + 3], &THUMBNAIL_CHECKER_DARK);
    }

    #[test]
    fn thumbnail_averages_the_source_box_exactly_for_small_images() {
        // 2×2 → 1×1 は 4 画素の平均そのもの(間引きが起きない寸法)。
        let mut buf = Vec::new();
        for px in [
            [0u8, 0, 0, 255],
            [100, 100, 100, 255],
            [200, 200, 200, 255],
            [255, 255, 255, 255],
        ] {
            buf.extend_from_slice(&px);
        }
        let (tw, th, out) = thumbnail_rgba(&buf, 2, 2, 1, 1);
        assert_eq!((tw, th), (1, 1));
        let expected = ((100 + 200 + 255) as f32 / 4.0).round() as u8;
        assert_eq!(out[0], expected);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn thumbnail_average_is_alpha_weighted() {
        // 透明画素の RGB は結果に寄与しない(premultiplied 平均)。
        let mut buf = Vec::new();
        buf.extend_from_slice(&[255u8, 0, 0, 255]);
        buf.extend_from_slice(&[0, 255, 0, 0]);
        let (_, _, out) = thumbnail_rgba(&buf, 2, 1, 1, 1);
        // α は 0.5、RGB は赤のまま → 市松と半々で合成される。
        let checker = THUMBNAIL_CHECKER_LIGHT[0] as f32;
        let expected_r = (255.0 * 0.5 + checker * 0.5).round() as u8;
        assert_eq!(out[0], expected_r);
        // 緑は「透明画素の 255」ではなく 0(寄与なし)と市松の合成になる。
        let expected_g = (0.0 * 0.5 + checker * 0.5).round() as u8;
        assert_eq!(out[1], expected_g, "透明な緑は混ざらない");
    }

    #[test]
    fn thumbnail_rejects_degenerate_inputs_without_panicking() {
        assert_eq!(thumbnail_rgba(&[], 0, 0, 40, 30), (0, 0, Vec::new()));
        assert_eq!(
            thumbnail_rgba(&[1, 2, 3, 4], 4, 4, 40, 30),
            (0, 0, Vec::new()),
            "画素長が寸法に足りない入力は空を返す"
        );
        assert_eq!(
            thumbnail_rgba(&make_buffer(4, 4, [0, 0, 0, 0]), 4, 4, 0, 30),
            (0, 0, Vec::new())
        );
    }

    /// 巨大なレイヤーでもサムネイル 1 枚の計算量が有界であること
    /// (間引き上限。SPEC §50.1「毎フレーム禁止」= 1 枚が重すぎてもいけない)。
    #[test]
    fn thumbnail_of_a_huge_layer_is_fast() {
        let buf = make_buffer(4000, 4000, [10, 20, 30, 255]);
        let start = std::time::Instant::now();
        let (tw, th, out) = thumbnail_rgba(&buf, 4000, 4000, 40, 30);
        let elapsed = start.elapsed();
        assert_eq!((tw, th), (30, 30));
        assert!(out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "4000x4000 のサムネイル 1 枚が {elapsed:?} もかかっている"
        );
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
