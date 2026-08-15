//! 選択範囲の修復(SPEC §53、ARCHITECTURE.md §22.4)。
//!
//! Telea 2004「An Image Inpainting Technique Based on the Fast Marching
//! Method」の**純 Rust 再実装**(依存追加なし、`std::collections::BinaryHeap`
//! だけを使う)。IOpaint が cv2 経由で提供する `INPAINT_TELEA` に相当する
//! 位置づけだが、**ピクセル完全一致は謳わない**(SPEC §53: 「小さな不要物・
//! 文字・傷向け。AI 品質は目標としない」)。
//!
//! # アルゴリズム
//!
//! ## 1. 距離場(Fast Marching Method)
//!
//! 修復対象(マスクが非 0)を **INSIDE**、それ以外を **KNOWN**(距離 T = 0 で
//! 確定済み)とし、KNOWN に隣接する INSIDE 画素の暫定距離を eikonal 方程式
//! `|∇T| = 1` から求めて最小ヒープへ積む。以後は次を繰り返す:
//!
//! 1. ヒープから T が最小の要素を取り出す。**確定済み(KNOWN)** または
//!    **より良い距離で積み直された古い要素**(`entry.t > t[i]`)なら捨てる
//!    (lazy deletion)。
//! 2. そうでなければその画素を **KNOWN へ遷移**させ(= 距離が確定)、
//!    その時点で色を塗る。
//! 3. 4 近傍のうち未確定(INSIDE / BAND)のものを **すべて解き直し**、距離が
//!    減っていれば更新してヒープへ積み直す(decrease-key 相当)。
//!
//! eikonal を解くときに参照するのは **KNOWN のみ**(暫定値の BAND は使わない)。
//! この 2 点 —「pop で KNOWN へ遷移」と「BAND の距離改善」— が揃って初めて
//! T が最小到達時刻になる(= 正しい FMM になる)。
//!
//! ## 2. 画素の色(Telea の一次外挿)
//!
//! 距離が確定した画素 `p` の色は、半径 `radius` の円内にある **KNOWN** 画素
//! `q` からの**一次外挿の加重平均**で決める(Telea 2004 式 (2)):
//!
//! ```text
//! I(p) = Σ w(p,q) · [ I(q) + ∇I(q)·(p−q) ]  /  Σ w(p,q)
//! w(p,q) = dir(p,q) · dst(p,q) · lev(p,q)
//!   dir = |r̂ · N̂|          r = p − q、N = ∇T(p)(等距離線に沿う画素を優先)
//!   dst = 1 / |r|²          (近い画素ほど強い)
//!   lev = 1 / (1 + |T(p) − T(q)|)  (同じ等距離線上の画素ほど強い)
//! ```
//!
//! **単なる色の加重平均ではない**ことが要点で、`∇I(q)·(p−q)` の項があるため
//! 一次勾配(グラデーション)がそのまま穴の中へ伸びる。`dir` は既に `|r|` で
//! 正規化してあるので、距離項は Telea の通り `1/|r|²`(`1/|r|³` にすると
//! 合成重みが `1/|r|⁴` 相当になってしまう)。
//!
//! α は **premultiplied**(`R·a, G·a, B·a, a`)で外挿・平均してから straight へ
//! 戻す(半透明画素の RGB が過大評価されない)。
//!
//! この関数は純粋(スレッド・egui・`Document` を一切知らない)なので、
//! `app.rs` がワーカースレッドへそのまま渡せる(SPEC §53 のワーカー実行)。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// SPEC §53: 未知(選択)画素数の上限。超過は呼び出し側がトーストで拒否する
/// (縮小を促す)。4,000,000 = 2000×2000 相当。
pub const MAX_INPAINT_PIXELS: usize = 4_000_000;

/// SPEC §53: 「半径 5px 固定」。
pub const INPAINT_RADIUS: f32 = 5.0;

/// FMM の状態。`KNOWN` = 距離も色も確定、`BAND` = ヒープ上の暫定値、
/// `INSIDE` = まだ前線が届いていない。
const KNOWN: u8 = 0;
const BAND: u8 = 1;
const INSIDE: u8 = 2;

/// 未知領域の初期距離(実際の距離場より十分大きい有限値。∞ を使わないのは
/// NaN を作らないため)。
const FAR_T: f32 = 1.0e6;

/// 方向重みの下限(`dir` が 0 になって重みが消えるのを防ぐ。OpenCV の
/// Telea 実装と同じ考え方)。
const MIN_DIRECTION_WEIGHT: f32 = 1.0e-6;

/// 1 画素あたりにヒープへ積みうる要素数の上限。lazy deletion では
/// 「初回 + 4 近傍が確定するたびの改善」= 高々 5 回なので、安全側に 8 倍で
/// 見ておき、これを超えたら(= 想定外の暴走)`OutOfMemory` で打ち切る。
const MAX_HEAP_ENTRIES_PER_PIXEL: usize = 8;

/// ワーカーへ渡す修復の入力一式(選択 bbox + 半径マージンの切り出し)。
pub struct InpaintInput {
    /// 切り出した領域の RGBA8(straight alpha、行優先、`width*height*4`)。
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 修復対象マスク(`width*height`、非 0 = 未知 = 塗り直す画素)。
    pub mask: Vec<u8>,
    /// 参照する近傍の半径(px)。
    pub radius: f32,
}

/// 修復結果(入力と同じ寸法の RGBA8)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InpaintOutput {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// 修復が実行できなかった理由(すべてトースト文言を持つ = パニックしない)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InpaintError {
    /// 寸法とバッファ長が食い違う(呼び出し側の組み立てミス)。
    InvalidInput,
    /// 未知画素が `MAX_INPAINT_PIXELS` を超えた。
    TooManyPixels,
    /// 既知画素が 1 つも無い(全面選択)。参照元が無いので修復できない。
    NothingToSampleFrom,
    /// 作業用バッファを確保できなかった。
    OutOfMemory,
}

impl InpaintError {
    /// トースト用の日本語メッセージ(SPEC §53)。
    pub fn message(self) -> &'static str {
        match self {
            InpaintError::InvalidInput => "修復対象の領域を組み立てられませんでした",
            InpaintError::TooManyPixels => {
                "選択範囲が広すぎます(修復できるのは約 400 万画素までです。選択を小さくしてください)"
            }
            InpaintError::NothingToSampleFrom => {
                "全体が選択されているため修復できません(周囲の画素を参照できません)"
            }
            InpaintError::OutOfMemory => "修復に必要なメモリを確保できませんでした",
        }
    }
}

/// 最小ヒープの要素。`BinaryHeap` は最大ヒープなので `Ord` を反転させる。
///
/// `f32` は `Ord` を持たないため `total_cmp`(SPEC §53:
/// 「`partial_cmp().unwrap()` 禁止」)を使い、同値のときは**添字**で
/// タイブレークする(同じ入力なら必ず同じ順序 = 決定性)。
#[derive(Debug, Clone, Copy, PartialEq)]
struct HeapEntry {
    t: f32,
    index: usize,
}

impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .t
            .total_cmp(&self.t)
            .then_with(|| other.index.cmp(&self.index))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn try_vec<T: Clone>(value: T, len: usize) -> Result<Vec<T>, InpaintError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| InpaintError::OutOfMemory)?;
    v.resize(len, value);
    Ok(v)
}

/// 4 近傍の添字(画像外は生成しない)。
fn neighbors4(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let mut out = [usize::MAX; 4];
    let mut count = 0;
    if x > 0 {
        out[count] = y * w + (x - 1);
        count += 1;
    }
    if x + 1 < w {
        out[count] = y * w + (x + 1);
        count += 1;
    }
    if y > 0 {
        out[count] = (y - 1) * w + x;
        count += 1;
    }
    if y + 1 < h {
        out[count] = (y + 1) * w + x;
        count += 1;
    }
    out.into_iter().take(count)
}

/// Fast Marching Method の状態(距離場と前線)。色を持たないので、テストから
/// 距離場だけを取り出して解析解と突き合わせられる。
struct Fmm {
    w: usize,
    h: usize,
    /// `KNOWN` / `BAND` / `INSIDE`。
    flags: Vec<u8>,
    /// 境界からの距離(KNOWN は確定値、BAND は暫定値、INSIDE は `FAR_T`)。
    t: Vec<f32>,
    heap: BinaryHeap<HeapEntry>,
    /// ヒープ要素数の上限(これを超えたら `OutOfMemory`)。
    max_entries: usize,
    /// 距離改善(decrease-key)の回数。「BAND の距離が実際に改善されている」
    /// ことをテストで確かめるために数えている。
    improvements: usize,
}

impl Fmm {
    /// マスク(非 0 = 未知)から初期状態を作り、初期バンドをヒープへ積む。
    fn new(mask: &[u8], w: usize, h: usize) -> Result<Self, InpaintError> {
        let len = w.checked_mul(h).ok_or(InpaintError::InvalidInput)?;
        if mask.len() != len {
            return Err(InpaintError::InvalidInput);
        }
        let max_entries = len
            .checked_mul(MAX_HEAP_ENTRIES_PER_PIXEL)
            .ok_or(InpaintError::OutOfMemory)?;
        let mut flags = try_vec(KNOWN, len)?;
        let mut t = try_vec(0.0f32, len)?;
        for i in 0..len {
            if mask[i] != 0 {
                flags[i] = INSIDE;
                t[i] = FAR_T;
            }
        }
        let mut fmm = Fmm {
            w,
            h,
            flags,
            t,
            heap: BinaryHeap::new(),
            max_entries,
            improvements: 0,
        };
        // 既知領域に接する未知画素が初期バンド。`relax` は 4 近傍の KNOWN を
        // すべて見て解くので、同じ画素を別の KNOWN 隣接から再度当たっても
        // 結果は変わらない(= `INSIDE` のものだけ見れば十分)。
        for y in 0..h {
            for x in 0..w {
                if fmm.flags[y * w + x] != KNOWN {
                    continue;
                }
                for n in neighbors4(x, y, w, h) {
                    if fmm.flags[n] == INSIDE {
                        fmm.relax(n)?;
                    }
                }
            }
        }
        Ok(fmm)
    }

    /// ヒープへ積む。`BinaryHeap::push` の再確保は infallible なので、
    /// **押す前に** `try_reserve` で容量を確保しておく(SPEC §53 の
    /// fallible allocation 要件)。要素数の上限も併せて守る。
    fn push(&mut self, entry: HeapEntry) -> Result<(), InpaintError> {
        if self.heap.len() >= self.max_entries {
            return Err(InpaintError::OutOfMemory);
        }
        if self.heap.len() == self.heap.capacity() {
            let extra = self.heap.capacity().max(64);
            self.heap
                .try_reserve(extra)
                .map_err(|_| InpaintError::OutOfMemory)?;
        }
        self.heap.push(entry);
        Ok(())
    }

    /// `index` の暫定距離を **KNOWN のみ**から解き直し、減っていれば更新して
    /// ヒープへ積み直す(lazy deletion 方式の decrease-key)。
    fn relax(&mut self, index: usize) -> Result<(), InpaintError> {
        debug_assert!(self.flags[index] != KNOWN);
        let (x, y) = (index % self.w, index / self.w);
        let candidate = self.solve_eikonal(x, y);
        if candidate < self.t[index] {
            if self.flags[index] == BAND {
                self.improvements += 1;
            }
            self.t[index] = candidate;
            self.flags[index] = BAND;
            self.push(HeapEntry {
                t: candidate,
                index,
            })?;
        }
        Ok(())
    }

    /// eikonal 方程式 `|∇T| = 1` を 2 方向の組で解く(Telea 2004 の `solve`)。
    /// 参照するのは **KNOWN(距離確定)のみ**。どちらも未確定なら「解なし」を
    /// 意味する大きな値を返す。
    fn solve_pair(&self, a: Option<usize>, b: Option<usize>) -> f32 {
        let known_a = a.filter(|i| self.flags[*i] == KNOWN);
        let known_b = b.filter(|i| self.flags[*i] == KNOWN);
        match (known_a, known_b) {
            (Some(ia), Some(ib)) => {
                let (t1, t2) = (self.t[ia], self.t[ib]);
                let diff = t1 - t2;
                let disc = 2.0 - diff * diff;
                if disc > 0.0 {
                    let r = disc.sqrt();
                    let s = (t1 + t2 - r) * 0.5;
                    if s >= t1 && s >= t2 {
                        return s;
                    }
                    let s = s + r;
                    if s >= t1 && s >= t2 {
                        return s;
                    }
                }
                FAR_T
            }
            (Some(ia), None) => 1.0 + self.t[ia],
            (None, Some(ib)) => 1.0 + self.t[ib],
            (None, None) => FAR_T,
        }
    }

    /// `(x, y)` の T を 4 象限の組み合わせから求める(最小値を採用)。
    fn solve_eikonal(&self, x: usize, y: usize) -> f32 {
        let left = (x > 0).then(|| y * self.w + (x - 1));
        let right = (x + 1 < self.w).then(|| y * self.w + (x + 1));
        let up = (y > 0).then(|| (y - 1) * self.w + x);
        let down = (y + 1 < self.h).then(|| (y + 1) * self.w + x);
        let mut best = FAR_T;
        for (a, b) in [(left, up), (right, up), (left, down), (right, down)] {
            let candidate = self.solve_pair(a, b);
            if candidate < best {
                best = candidate;
            }
        }
        best
    }

    /// 距離場 T の勾配(中心差分。**KNOWN 以外**の側は片側差分へ落とす)。
    /// 前線の進行方向 `N` になる。
    fn gradient_t(&self, x: usize, y: usize) -> (f32, f32) {
        let sample = |sx: usize, sy: usize| -> Option<f32> {
            let i = sy * self.w + sx;
            (self.flags[i] == KNOWN).then(|| self.t[i])
        };
        let left = (x > 0).then(|| sample(x - 1, y)).flatten();
        let right = (x + 1 < self.w).then(|| sample(x + 1, y)).flatten();
        let up = (y > 0).then(|| sample(x, y - 1)).flatten();
        let down = (y + 1 < self.h).then(|| sample(x, y + 1)).flatten();
        let center = self.t[y * self.w + x];
        let gx = match (left, right) {
            (Some(l), Some(r)) => (r - l) * 0.5,
            (Some(l), None) => center - l,
            (None, Some(r)) => r - center,
            (None, None) => 0.0,
        };
        let gy = match (up, down) {
            (Some(u), Some(d)) => (d - u) * 0.5,
            (Some(u), None) => center - u,
            (None, Some(d)) => d - center,
            (None, None) => 0.0,
        };
        (gx, gy)
    }

    /// 前線を進める。距離が確定した画素ごとに `on_finalize` を呼ぶ
    /// (色を塗るのはここ = T が確定し、より近い画素がすべて既知になった
    /// 時点)。`ctx` を分けてあるので、テストからは「確定順の記録」など
    /// 色以外の観測にも使える。
    fn run<C>(
        &mut self,
        ctx: &mut C,
        mut on_finalize: impl FnMut(&mut C, &Fmm, usize),
    ) -> Result<(), InpaintError> {
        while let Some(entry) = self.heap.pop() {
            let index = entry.index;
            // lazy deletion: 確定済み / より良い距離で積み直された古い要素。
            if self.flags[index] == KNOWN || entry.t > self.t[index] {
                continue;
            }
            // **先に色を塗ってから** KNOWN へ遷移させる。順序を逆にすると、
            // まだ塗っていないこの画素が「既知」として近傍の勾配計算に混ざり、
            // 穴の中の元の色(消したい傷そのもの)が滲み出す。
            on_finalize(ctx, self, index);
            self.flags[index] = KNOWN;
            let (x, y) = (index % self.w, index / self.w);
            for n in neighbors4(x, y, self.w, self.h) {
                if self.flags[n] != KNOWN {
                    self.relax(n)?;
                }
            }
        }
        Ok(())
    }
}

/// premultiplied の 4 成分(`R·a, G·a, B·a, a`)。α も 0..255 尺度に揃えて
/// あるので、4 成分を同じ式で外挿・平均できる。
#[inline]
fn premultiplied(pixels: &[u8], index: usize) -> [f32; 4] {
    let p = &pixels[index * 4..index * 4 + 4];
    let a = p[3] as f32 * (1.0 / 255.0);
    [
        p[0] as f32 * a,
        p[1] as f32 * a,
        p[2] as f32 * a,
        p[3] as f32,
    ]
}

/// 塗る側(画素バッファ)。`Fmm::run` の `ctx` として渡す。
struct Canvas {
    pixels: Vec<u8>,
    radius: f32,
}

impl Canvas {
    /// `q = (x, y)` の premultiplied 値を、そこからの相対位置 `rel = p − q` へ
    /// **一次外挿**する(`I(q) + ∇I(q)·rel`)。勾配は KNOWN 画素だけから中心
    /// 差分(端・未確定側は片側差分、両側とも無ければ 0)で求める。
    fn extrapolated(&self, fmm: &Fmm, x: usize, y: usize, rel: (f32, f32)) -> [f32; 4] {
        let (w, h) = (fmm.w, fmm.h);
        let center = premultiplied(&self.pixels, y * w + x);
        let sample = |sx: usize, sy: usize| -> Option<[f32; 4]> {
            let i = sy * w + sx;
            (fmm.flags[i] == KNOWN).then(|| premultiplied(&self.pixels, i))
        };
        let left = (x > 0).then(|| sample(x - 1, y)).flatten();
        let right = (x + 1 < w).then(|| sample(x + 1, y)).flatten();
        let up = (y > 0).then(|| sample(x, y - 1)).flatten();
        let down = (y + 1 < h).then(|| sample(x, y + 1)).flatten();
        let mut out = center;
        for (c, slot) in out.iter_mut().enumerate() {
            let gx = match (left, right) {
                (Some(l), Some(r)) => (r[c] - l[c]) * 0.5,
                (Some(l), None) => center[c] - l[c],
                (None, Some(r)) => r[c] - center[c],
                (None, None) => 0.0,
            };
            let gy = match (up, down) {
                (Some(u), Some(d)) => (d[c] - u[c]) * 0.5,
                (Some(u), None) => center[c] - u[c],
                (None, Some(d)) => d[c] - center[c],
                (None, None) => 0.0,
            };
            *slot = center[c] + gx * rel.0 + gy * rel.1;
        }
        out
    }

    /// Telea 式 (2) の加重平均で `index` の色を決める(モジュール冒頭の解説)。
    fn fill_at(&mut self, fmm: &Fmm, index: usize) {
        let (w, h) = (fmm.w, fmm.h);
        let (x, y) = (index % w, index / w);
        let (nx, ny) = fmm.gradient_t(x, y);
        let grad_len = (nx * nx + ny * ny).sqrt();
        let center_t = fmm.t[index];
        let radius = self.radius.max(1.0);
        let ri = radius.ceil() as isize;
        let r2 = radius * radius;

        let mut sum_w = 0.0f32;
        let mut sum = [0.0f32; 4];

        for dy in -ri..=ri {
            let sy = y as isize + dy;
            if sy < 0 || sy >= h as isize {
                continue;
            }
            for dx in -ri..=ri {
                let sx = x as isize + dx;
                if sx < 0 || sx >= w as isize {
                    continue;
                }
                let dist2 = (dx * dx + dy * dy) as f32;
                if dist2 == 0.0 || dist2 > r2 {
                    continue;
                }
                let q = sy as usize * w + sx as usize;
                // 色が確定しているのは KNOWN だけ(BAND は距離すら暫定)。
                if fmm.flags[q] != KNOWN {
                    continue;
                }
                // r = p − q(参照画素から対象画素への向き)。
                let rel = (-(dx as f32), -(dy as f32));
                let dist = dist2.sqrt();
                // 方向: 進行方向 N = ∇T との角度。|r| で正規化済み。
                let dir = if grad_len > 0.0 {
                    ((rel.0 * nx + rel.1 * ny).abs() / (dist * grad_len)).max(MIN_DIRECTION_WEIGHT)
                } else {
                    1.0
                };
                // 距離: 1/|r|²(Telea 式 (3))。
                let dst = 1.0 / dist2;
                // レベル: 同じ等距離線上の画素ほど強い。
                let lev = 1.0 / (1.0 + (center_t - fmm.t[q]).abs());
                let weight = dir * dst * lev;
                if !weight.is_finite() || weight <= 0.0 {
                    continue;
                }
                let value = self.extrapolated(fmm, sx as usize, sy as usize, rel);
                for (slot, v) in sum.iter_mut().zip(value) {
                    *slot += weight * v;
                }
                sum_w += weight;
            }
        }

        if sum_w <= 0.0 {
            return;
        }
        let inv = 1.0 / sum_w;
        let alpha255 = (sum[3] * inv).clamp(0.0, 255.0);
        let idx = index * 4;
        if alpha255 <= 0.0 {
            self.pixels[idx..idx + 4].copy_from_slice(&[0, 0, 0, 0]);
            return;
        }
        // premultiplied 平均 → straight へ戻す。
        let alpha = alpha255 / 255.0;
        for (channel, premultiplied) in sum[..3].iter().enumerate() {
            let straight = (*premultiplied * inv) / alpha;
            self.pixels[idx + channel] = straight.round().clamp(0.0, 255.0) as u8;
        }
        self.pixels[idx + 3] = alpha255.round().clamp(0.0, 255.0) as u8;
    }
}

/// SPEC §53: Telea(FMM)による修復。マスクが非 0 の画素だけを塗り直し、
/// それ以外は入力のまま返す。
pub fn telea_inpaint(input: InpaintInput) -> Result<InpaintOutput, InpaintError> {
    let InpaintInput {
        pixels,
        width,
        height,
        mask,
        radius,
    } = input;
    if width == 0 || height == 0 {
        return Err(InpaintError::InvalidInput);
    }
    let len = (width as usize)
        .checked_mul(height as usize)
        .ok_or(InpaintError::InvalidInput)?;
    let byte_len = len.checked_mul(4).ok_or(InpaintError::InvalidInput)?;
    if pixels.len() != byte_len || mask.len() != len {
        return Err(InpaintError::InvalidInput);
    }
    let radius = if radius.is_finite() {
        radius.clamp(1.0, 64.0)
    } else {
        INPAINT_RADIUS
    };

    let unknown = mask.iter().filter(|m| **m != 0).count();
    if unknown == 0 {
        // 塗るものが無い(呼び出し側が空選択を渡した)。入力をそのまま返す。
        return Ok(InpaintOutput {
            width,
            height,
            pixels,
        });
    }
    if unknown > MAX_INPAINT_PIXELS {
        return Err(InpaintError::TooManyPixels);
    }
    if unknown == len {
        return Err(InpaintError::NothingToSampleFrom);
    }

    let mut fmm = Fmm::new(&mask, width as usize, height as usize)?;
    let mut canvas = Canvas { pixels, radius };
    fmm.run(&mut canvas, Canvas::fill_at)?;

    Ok(InpaintOutput {
        width,
        height,
        pixels: canvas.pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w*h` の一様な RGBA バッファ。
    fn solid(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&color);
        }
        v
    }

    fn mask_rect(w: u32, h: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<u8> {
        let mut m = vec![0u8; (w * h) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                m[(y * w + x) as usize] = 255;
            }
        }
        m
    }

    fn px(out: &InpaintOutput, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * out.width + x) * 4) as usize;
        [
            out.pixels[i],
            out.pixels[i + 1],
            out.pixels[i + 2],
            out.pixels[i + 3],
        ]
    }

    fn run(pixels: Vec<u8>, w: u32, h: u32, mask: Vec<u8>) -> Result<InpaintOutput, InpaintError> {
        telea_inpaint(InpaintInput {
            pixels,
            width: w,
            height: h,
            mask,
            radius: INPAINT_RADIUS,
        })
    }

    /// 画素値を `f(x, y)` で作る RGBA バッファ(α は 255)。
    fn build(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    // -- FMM(距離場)------------------------------------------------------

    /// 全行にまたがる縦帯のマスクなら、距離場は左右の境界からの 1 次元距離
    /// `min(x − x0 + 1, x1 − x)` と厳密に一致する(解析解との突き合わせ)。
    #[test]
    fn the_distance_field_matches_the_analytic_solution() {
        let (w, h) = (40usize, 9usize);
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 10..30 {
                mask[y * w + x] = 255;
            }
        }
        let mut fmm = Fmm::new(&mask, w, h).expect("初期化できる");
        fmm.run(&mut (), |_, _, _| {}).expect("完走する");
        for y in 0..h {
            for x in 10..30 {
                let expected = ((x as f32) - 9.0).min(30.0 - (x as f32));
                let got = fmm.t[y * w + x];
                assert!(
                    (got - expected).abs() < 1.0e-3,
                    "T({x},{y}) = {got}(解析解 {expected})"
                );
            }
        }
    }

    /// 画素は必ず**距離が小さい順**に確定する(FMM の定義そのもの)。
    #[test]
    fn pixels_are_finalised_in_ascending_distance_order() {
        let (w, h) = (28usize, 28usize);
        let mut mask = vec![0u8; w * h];
        for y in 6..22 {
            for x in 6..22 {
                mask[y * w + x] = 255;
            }
        }
        let mut fmm = Fmm::new(&mask, w, h).expect("初期化できる");
        let mut order: Vec<f32> = Vec::new();
        fmm.run(&mut order, |order, fmm, i| order.push(fmm.t[i]))
            .expect("完走する");
        assert_eq!(order.len(), 16 * 16, "未知画素はすべて確定する");
        for pair in order.windows(2) {
            assert!(
                pair[1] >= pair[0] - 1.0e-6,
                "確定順が距離昇順になっていない: {} → {}",
                pair[0],
                pair[1]
            );
        }
    }

    /// 凹形状(コの字)では BAND の暫定距離が実際に改善される(decrease-key)。
    /// 完走後はどの画素も解き直しで縮まない = eikonal の不動点になっている。
    #[test]
    fn a_concave_mask_improves_band_distances_and_reaches_a_fixed_point() {
        let (w, h) = (24usize, 24usize);
        let mut mask = vec![0u8; w * h];
        // コの字(左の縦棒 + 上下の横棒)。前線が内側で回り込む。
        for y in 4..20 {
            for x in 4..8 {
                mask[y * w + x] = 255;
            }
        }
        for x in 4..20 {
            for y in 4..8 {
                mask[y * w + x] = 255;
            }
            for y in 16..20 {
                mask[y * w + x] = 255;
            }
        }
        let mut fmm = Fmm::new(&mask, w, h).expect("初期化できる");
        fmm.run(&mut (), |_, _, _| {}).expect("完走する");
        assert!(
            fmm.improvements > 0,
            "暫定距離の改善(decrease-key)が一度も起きていない"
        );
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if mask[i] == 0 {
                    continue;
                }
                let again = fmm.solve_eikonal(x, y);
                assert!(
                    again >= fmm.t[i] - 1.0e-3,
                    "({x},{y}) はまだ縮む: {} → {again}",
                    fmm.t[i]
                );
            }
        }
    }

    /// ヒープは要素数の上限を超えたら確保せずエラーにする(fallible)。
    #[test]
    fn the_heap_refuses_to_grow_beyond_the_entry_cap() {
        let (w, h) = (8usize, 8usize);
        let mut mask = vec![0u8; w * h];
        mask[3 * w + 3] = 255;
        let mut fmm = Fmm::new(&mask, w, h).expect("初期化できる");
        fmm.max_entries = fmm.heap.len();
        assert_eq!(
            fmm.push(HeapEntry { t: 0.0, index: 0 }),
            Err(InpaintError::OutOfMemory)
        );
    }

    // -- Telea(色の外挿)--------------------------------------------------

    #[test]
    fn solid_region_is_restored_exactly() {
        // SPEC §53: 単色領域の完全復元。周囲が一様なら、穴も同じ色で埋まる。
        let (w, h) = (24u32, 24u32);
        let color = [37u8, 111, 200, 255];
        let mut pixels = solid(w, h, color);
        let mask = mask_rect(w, h, 9, 9, 15, 15);
        // 穴の中身を別の色にしておく(修復で消えるはず)。
        for y in 9..15 {
            for x in 9..15 {
                let i = ((y * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let out = run(pixels, w, h, mask).expect("ok");
        for y in 9..15 {
            for x in 9..15 {
                assert_eq!(px(&out, x, y), color, "({x},{y}) が復元されていない");
            }
        }
    }

    /// **一次外挿が効いていることの判別テスト**(単純加重平均との差)。
    ///
    /// 横方向の一次勾配に穴を開けて修復する。単純な色の加重平均なら穴の中は
    /// ほぼ平坦(左右の平均値)になるが、Telea の `I(q) + ∇I(q)·(p−q)` なら
    /// **勾配がそのまま続く**。そこで「元の値との誤差」だけでなく
    /// 「行方向に単調増加していて、傾きが元と同じであること」を直接確かめる。
    #[test]
    fn a_linear_gradient_keeps_its_slope_instead_of_flattening() {
        let (w, h) = (40u32, 20u32);
        let value = |x: u32| -> u8 { (20 + x * 5).min(255) as u8 };
        let pixels = build(w, h, |x, _| {
            let v = value(x);
            [v, v, v, 255]
        });
        let mask = mask_rect(w, h, 16, 6, 24, 14);
        let out = run(pixels, w, h, mask).expect("ok");

        let mut max_error = 0i32;
        for y in 6..14 {
            let mut previous: Option<i32> = None;
            for x in 16..24 {
                let got = px(&out, x, y)[0] as i32;
                max_error = max_error.max((got - value(x) as i32).abs());
                if let Some(p) = previous {
                    assert!(
                        got > p,
                        "({x},{y}) で勾配が途切れた(平坦化している): {p} → {got}"
                    );
                }
                previous = Some(got);
            }
        }
        assert!(
            max_error <= 6,
            "一次勾配の復元誤差が大きすぎる: {max_error}"
        );

        // 傾きが元と同じ(= 5/px)であることを直接確認する。単純平均なら
        // ここはほぼ 0 になる。
        let left = px(&out, 16, 10)[0] as i32;
        let right = px(&out, 23, 10)[0] as i32;
        let slope = (right - left) as f32 / 7.0;
        assert!(
            (slope - 5.0).abs() < 1.0,
            "穴の中の傾きが元と違う: {slope}(期待 5.0)"
        );
    }

    /// RGBA の 3 チャンネルが**別々の一次平面**でも、それぞれの平面が保たれる。
    #[test]
    fn a_linear_rgb_plane_is_reproduced_per_channel() {
        let (w, h) = (32u32, 32u32);
        let plane = |x: u32, y: u32| -> [u8; 4] {
            [
                (10 + x * 3).min(255) as u8,
                (20 + y * 4).min(255) as u8,
                (30 + x * 2 + y).min(255) as u8,
                255,
            ]
        };
        let pixels = build(w, h, plane);
        let mask = mask_rect(w, h, 13, 13, 19, 19);
        let out = run(pixels, w, h, mask).expect("ok");
        let mut max_error = 0i32;
        for y in 13..19 {
            for x in 13..19 {
                let got = px(&out, x, y);
                let want = plane(x, y);
                for c in 0..3 {
                    max_error = max_error.max((got[c] as i32 - want[c] as i32).abs());
                }
                assert_eq!(got[3], 255, "α が変わっている");
            }
        }
        assert!(max_error <= 6, "平面の復元誤差が大きすぎる: {max_error}");
    }

    /// 斜めエッジをまたぐ穴を埋めても、エッジの両側が中間色へ潰れない。
    #[test]
    fn a_diagonal_edge_is_not_blurred_into_a_flat_patch() {
        let (w, h) = (32u32, 32u32);
        let dark = 40u8;
        let light = 210u8;
        let pixels = build(w, h, |x, y| {
            let v = if x + y < 30 { dark } else { light };
            [v, v, v, 255]
        });
        let mask = mask_rect(w, h, 12, 12, 18, 18);
        let out = run(pixels, w, h, mask).expect("ok");
        // エッジからじゅうぶん離れた穴の隅は、それぞれの側の色を保つ。
        let inside = px(&out, 12, 12)[0];
        let outside = px(&out, 17, 17)[0];
        assert!(inside < 100, "暗い側が明るく潰れた: {inside}");
        assert!(outside > 150, "明るい側が暗く潰れた: {outside}");
        assert!(
            outside as i32 - inside as i32 > 80,
            "エッジのコントラストが失われた: {inside} / {outside}"
        );
    }

    /// 細い傷(2px 幅の斜め線)はほぼ完全に消える。
    #[test]
    fn a_thin_scratch_is_removed_almost_exactly() {
        let (w, h) = (48u32, 48u32);
        let value = |x: u32, y: u32| -> u8 { (30 + x * 2 + y).min(255) as u8 };
        let original = build(w, h, |x, y| {
            let v = value(x, y);
            [v, v, v, 255]
        });
        let mut pixels = original.clone();
        let mut mask = vec![0u8; (w * h) as usize];
        for t in 4..44u32 {
            for d in 0..2u32 {
                let x = (t + d).min(w - 1);
                let i = ((t * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
                mask[(t * w + x) as usize] = 255;
            }
        }
        let out = run(pixels, w, h, mask.clone()).expect("ok");
        let mut max_error = 0i32;
        for i in 0..(w * h) as usize {
            if mask[i] == 0 {
                continue;
            }
            max_error = max_error.max((out.pixels[i * 4] as i32 - original[i * 4] as i32).abs());
        }
        assert!(max_error <= 8, "細線の復元誤差が大きすぎる: {max_error}");
    }

    #[test]
    fn full_mask_is_rejected() {
        // SPEC §53: 全面選択(既知画素ゼロ)はエラー。
        let (w, h) = (8u32, 8u32);
        let mask = mask_rect(w, h, 0, 0, w, h);
        assert_eq!(
            run(solid(w, h, [1, 2, 3, 255]), w, h, mask),
            Err(InpaintError::NothingToSampleFrom)
        );
    }

    #[test]
    fn single_pixel_mask_is_filled_from_its_neighbours() {
        let (w, h) = (9u32, 9u32);
        let color = [10u8, 20, 30, 255];
        let mut pixels = solid(w, h, color);
        let i = ((4 * w + 4) * 4) as usize;
        pixels[i..i + 4].copy_from_slice(&[200, 200, 200, 255]);
        let mask = mask_rect(w, h, 4, 4, 5, 5);
        let out = run(pixels, w, h, mask).expect("ok");
        assert_eq!(px(&out, 4, 4), color);
    }

    #[test]
    fn mask_touching_the_image_edge_is_handled() {
        // 画像端に接する選択(4 近傍が欠ける)でもパニックせず埋まる。
        let (w, h) = (12u32, 12u32);
        let color = [90u8, 140, 60, 255];
        let mut pixels = solid(w, h, color);
        for y in 0..3 {
            for x in 0..3 {
                let i = ((y * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        let mask = mask_rect(w, h, 0, 0, 3, 3);
        let out = run(pixels, w, h, mask).expect("ok");
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(px(&out, x, y), color, "端の ({x},{y}) が埋まっていない");
            }
        }
    }

    #[test]
    fn too_many_unknown_pixels_is_rejected() {
        // SPEC §53: 未知画素数の上限(400 万)。
        let side = 2001u32; // 2001² = 4,004,001 > 4,000,000
        let w = side;
        let h = side;
        // 端 1 列だけ既知にして「全面選択」ではない状態にする。
        let mut mask = vec![255u8; (w * h) as usize];
        for y in 0..h {
            mask[(y * w) as usize] = 0;
        }
        let result = telea_inpaint(InpaintInput {
            pixels: vec![0u8; (w as usize * h as usize) * 4],
            width: w,
            height: h,
            mask,
            radius: INPAINT_RADIUS,
        });
        assert_eq!(result, Err(InpaintError::TooManyPixels));
    }

    #[test]
    fn invalid_input_is_rejected_without_panicking() {
        assert_eq!(
            telea_inpaint(InpaintInput {
                pixels: vec![0u8; 8],
                width: 4,
                height: 4,
                mask: vec![0u8; 16],
                radius: 5.0,
            }),
            Err(InpaintError::InvalidInput)
        );
        assert_eq!(
            telea_inpaint(InpaintInput {
                pixels: Vec::new(),
                width: 0,
                height: 0,
                mask: Vec::new(),
                radius: 5.0,
            }),
            Err(InpaintError::InvalidInput)
        );
    }

    #[test]
    fn empty_mask_returns_the_input_unchanged() {
        let (w, h) = (6u32, 6u32);
        let pixels = solid(w, h, [5, 6, 7, 255]);
        let out = run(pixels.clone(), w, h, vec![0u8; (w * h) as usize]).expect("ok");
        assert_eq!(out.pixels, pixels);
    }

    #[test]
    fn the_same_input_always_produces_the_same_output() {
        // SPEC §53: 同一入力の決定性(ヒープのタイブレークを添字で固定)。
        let (w, h) = (32u32, 32u32);
        let pixels = build(w, h, |x, y| {
            [
                (x * 7 % 256) as u8,
                (y * 5 % 256) as u8,
                ((x + y) * 3 % 256) as u8,
                255,
            ]
        });
        let mask = mask_rect(w, h, 10, 10, 22, 22);
        let first = run(pixels.clone(), w, h, mask.clone()).expect("ok");
        for _ in 0..3 {
            let again = run(pixels.clone(), w, h, mask.clone()).expect("ok");
            assert_eq!(again, first, "同じ入力なのに結果が変わった");
        }
    }

    #[test]
    fn semi_transparent_neighbours_are_averaged_premultiplied() {
        // 透明な画素の RGB は結果に寄与しない(premultiplied 平均)。
        let (w, h) = (9u32, 9u32);
        let pixels = build(w, h, |x, _| {
            // 左半分は不透明な赤、右半分は完全透明な緑。
            if x < w / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 0]
            }
        });
        let mask = mask_rect(w, h, 4, 4, 5, 5);
        let out = run(pixels, w, h, mask).expect("ok");
        let filled = px(&out, 4, 4);
        assert!(filled[1] < 40, "透明な緑が RGB に混ざっている: {filled:?}");
    }

    #[test]
    fn heap_entries_pop_in_ascending_distance_with_index_tiebreak() {
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
        heap.push(HeapEntry { t: 2.0, index: 5 });
        heap.push(HeapEntry { t: 1.0, index: 9 });
        heap.push(HeapEntry { t: 1.0, index: 3 });
        assert_eq!(heap.pop(), Some(HeapEntry { t: 1.0, index: 3 }));
        assert_eq!(heap.pop(), Some(HeapEntry { t: 1.0, index: 9 }));
        assert_eq!(heap.pop(), Some(HeapEntry { t: 2.0, index: 5 }));
    }
}
