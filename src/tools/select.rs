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
    /// 拡縮の再サンプリング元(ARCHITECTURE.md §14.6: 「拡縮は浮動化時の
    /// 元ピクセルから毎回バイリニアで再サンプリングする(累積劣化させない)」)。
    ///
    /// v8 レビュー修正③: 生成時には複製を持たず**空**にしておき、最初の
    /// 拡縮の直前に `ensure_resample_source` が現在の `pixels`/`mask`
    /// (生成後、拡縮以外では不変)を写して確定する。大半の浮動片は一度も
    /// 拡縮されないため、全画素の二重保持(選択の移動のたびに pixels+mask の
    /// 2 セット)を払わずに済む。一度確定した後は従来どおり不変。
    pub original: Vec<u8>,
    pub orig_w: u32,
    pub orig_h: u32,
    /// `original` と対になる元マスク(同じく最初の拡縮時に確定、不変)。
    pub orig_mask: Vec<u8>,
    /// v6 §33〜35(ARCHITECTURE.md §18.3 対応表): この浮動片が最終的に
    /// 合成される(`app.rs::flush_floating_keep_selection`)ときに History
    /// へ積む undo ラベル。生成経路によって変わる: 選択ドラッグ・移動
    /// ツール・自由変形は既定の "選択の移動"、クリップボード貼り付けは
    /// "貼り付け"、テキストの通常確定(`place_new_floating` 経由)は
    /// "テキスト"(いずれも `with_label` で既定から上書きする)。ツール
    /// 切替でテキスト編集が中断された場合だけは浮動片を経由せず直接合成
    /// する別経路(`commit_pending_text_edit_and_composite`)を使うため、
    /// そちらではこのフィールドは参照されない。
    pub label: &'static str,
    /// v8 レビュー修正(SPEC §18: 「完全復元」): 浮動化した瞬間の
    /// `Document::modified`。浮動化は未保存ガードのため即座に
    /// `modified = true` を立てる(`place_new_floating` のコメント参照)が、
    /// Esc キャンセル(`app.rs::cancel_floating`)や before==after で履歴に
    /// 何も積まれなかった確定は文書を一切変えないので、この値へ戻す —
    /// 戻さないと「保存済み文書で貼り付け→Esc」しただけで未保存表示
    /// (`*`)と終了確認が残り続ける。既定は安全側の `true`(戻さない)。
    /// 浮動片の保持中に履歴外の実変更(レイヤー名の確定など)が起きた
    /// 場合、その経路がここを `true` に汚染して復元を無効化する
    /// (`app.rs::commit_pending_layer_rename` 参照)。
    pub prev_modified: bool,
}

impl Floating {
    /// `Floating` を作る(通常の生成経路はすべてこれを通す)。
    /// `original`/`orig_mask` は空のまま(v8 レビュー修正③: 最初の拡縮時に
    /// `ensure_resample_source` が確定する遅延複製、フィールドコメント参照)。
    pub fn new(
        pixels: Vec<u8>,
        w: u32,
        h: u32,
        mask: Vec<u8>,
        pos: Pos2,
        cut_from: Option<SelMask>,
        id: u64,
    ) -> Self {
        Self {
            pixels,
            w,
            h,
            mask,
            pos,
            cut_from,
            id,
            original: Vec::new(),
            orig_w: w,
            orig_h: h,
            orig_mask: Vec::new(),
            // ARCHITECTURE.md §18.3: 選択ドラッグ・移動ツール・自由変形の
            // 浮動化はいずれもこの既定ラベルのまま(`with_label` で上書き
            // されるのは貼り付け/テキストの生成経路だけ)。
            label: "選択の移動",
            // 安全側の既定(キャンセルしても `modified` を戻さない)。実際の
            // 浮動化経路(`app.rs`)が浮動化直前の値で上書きする。
            prev_modified: true,
        }
    }

    /// 生成後にラベルを差し替える(貼り付け/テキスト等、既定の
    /// "選択の移動" と異なる commit ラベルを持つ浮動片向け、
    /// ARCHITECTURE.md §18.3)。
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }

    /// v8 レビュー修正③: 最初の拡縮の直前に呼び、再サンプリング元
    /// (`original`/`orig_mask`)を現在の `pixels`/`mask` で確定する
    /// (フィールドコメント参照)。2 回目以降は何もしない(累積劣化させない
    /// 意味論は不変 — 元は常に「浮動化時点」の画素)。
    ///
    /// 浮動片の画素そのものを変える操作(v9 の反転/回転など)を行った場合は
    /// 呼び出し側が `reset_resample_source` で無効化し、次の拡縮が変換後の
    /// 画素を新しい元として確定し直す。
    pub fn ensure_resample_source(&mut self) {
        if self.original.is_empty() {
            self.original = self.pixels.clone();
            self.orig_mask = self.mask.clone();
            self.orig_w = self.w;
            self.orig_h = self.h;
        }
    }

    /// 再サンプリング元を破棄する(`ensure_resample_source` のコメント参照)。
    pub fn reset_resample_source(&mut self) {
        self.original = Vec::new();
        self.orig_mask = Vec::new();
        self.orig_w = self.w;
        self.orig_h = self.h;
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

/// チャンネル数 `ch` の行優先バッファを左右反転する(v9 §42 の浮動片変換用。
/// 画素=4ch とマスク=1ch を同じコードで扱う)。
fn flip_h_channels(buf: &mut [u8], w: usize, ch: usize) {
    if w == 0 || ch == 0 {
        return;
    }
    for row in buf.chunks_mut(w * ch) {
        let mut l = 0usize;
        let mut r = w - 1;
        while l < r {
            for c in 0..ch {
                row.swap(l * ch + c, r * ch + c);
            }
            l += 1;
            r -= 1;
        }
    }
}

/// 行優先バッファを上下反転する(行バイト数 `row_bytes` 単位のスワップ)。
fn flip_v_rows(buf: &mut [u8], row_bytes: usize, h: usize) {
    if row_bytes == 0 || h == 0 {
        return;
    }
    let mut top = 0usize;
    let mut bottom = h - 1;
    while top < bottom {
        let (a, b) = buf.split_at_mut(bottom * row_bytes);
        a[top * row_bytes..top * row_bytes + row_bytes].swap_with_slice(&mut b[0..row_bytes]);
        top += 1;
        bottom -= 1;
    }
}

/// チャンネル数 `ch` のバッファを右に 90° 回転する
/// (`document.rs::rotate_cw_buffer` と同じ写像 new(col,row)=old(row,h-1-col))。
fn rotate_cw_channels(w: usize, h: usize, buf: &[u8], ch: usize) -> Vec<u8> {
    let (new_w, new_h) = (h, w);
    let mut out = vec![0u8; buf.len()];
    for row in 0..new_h {
        for col in 0..new_w {
            let (x, y) = (row, h - 1 - col);
            let src = (y * w + x) * ch;
            let dst = (row * new_w + col) * ch;
            if let (Some(s), Some(d)) = (buf.get(src..src + ch), out.get_mut(dst..dst + ch)) {
                d.copy_from_slice(s);
            }
        }
    }
    out
}

/// 左に 90° 回転(`rotate_cw_channels` の逆写像 new(col,row)=old(w-1-row,col))。
fn rotate_ccw_channels(w: usize, h: usize, buf: &[u8], ch: usize) -> Vec<u8> {
    let (new_w, new_h) = (h, w);
    let mut out = vec![0u8; buf.len()];
    for row in 0..new_h {
        for col in 0..new_w {
            let (x, y) = (w - 1 - row, col);
            let src = (y * w + x) * ch;
            let dst = (row * new_w + col) * ch;
            if let (Some(s), Some(d)) = (buf.get(src..src + ch), out.get_mut(dst..dst + ch)) {
                d.copy_from_slice(s);
            }
        }
    }
    out
}

/// v9 §42: 浮動片の変換の種類(画像メニューの反転/回転を、浮動片がある
/// ときはその対象だけへ適用する — MS ペイント準拠)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatingTransform {
    FlipHorizontal,
    FlipVertical,
    RotateCw,
    RotateCcw,
}

/// v9 §42: 浮動片へ変換を適用する(画素+マスクを同時に、純関数的)。
/// 回転は見た目の中心を維持したまま幅高を入れ替える。呼び出し側は適用後に
/// 新しいテクスチャ `id` の割り当てと `reset_resample_source`(拡縮の
/// 再サンプリング元の破棄 — 変換後の画素が新しい元になる)を行うこと
/// (`app.rs::try_transform_floating` 参照)。
pub fn transform_floating(floating: &mut Floating, transform: FloatingTransform) {
    let (w, h) = (floating.w as usize, floating.h as usize);
    match transform {
        FloatingTransform::FlipHorizontal => {
            flip_h_channels(&mut floating.pixels, w, 4);
            flip_h_channels(&mut floating.mask, w, 1);
        }
        FloatingTransform::FlipVertical => {
            flip_v_rows(&mut floating.pixels, w * 4, h);
            flip_v_rows(&mut floating.mask, w, h);
        }
        FloatingTransform::RotateCw | FloatingTransform::RotateCcw => {
            let cw = transform == FloatingTransform::RotateCw;
            floating.pixels = if cw {
                rotate_cw_channels(w, h, &floating.pixels, 4)
            } else {
                rotate_ccw_channels(w, h, &floating.pixels, 4)
            };
            floating.mask = if cw {
                rotate_cw_channels(w, h, &floating.mask, 1)
            } else {
                rotate_ccw_channels(w, h, &floating.mask, 1)
            };
            // 見た目の中心を維持して幅高を入れ替える(SPEC §16 のハンドル
            // 拡縮と同じ「中心基準」の感覚)。
            let center_x = floating.pos.x + floating.w as f32 / 2.0;
            let center_y = floating.pos.y + floating.h as f32 / 2.0;
            std::mem::swap(&mut floating.w, &mut floating.h);
            floating.pos = pos2(
                center_x - floating.w as f32 / 2.0,
                center_y - floating.h as f32 / 2.0,
            );
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

/// v8 レビュー修正: `rect_mask(rect).clamp_to(..)` と同値だが、**クリップ後の
/// 領域ぶんしか確保しない**。従来はドラッグの外接矩形全体(低ズームで
/// キャンバス外まで引くと文書サイズを大きく超えうる)をまず `Vec` で確保して
/// からクランプしていたため、極端なドラッグで巨大確保→OOM 中断(release は
/// `panic = "abort"`)を起こしえた(CLAUDE.md 鉄則「ユーザー入力経路で
/// パニックしない」違反)。選択の新規作成経路はすべてこの clipped 系を使う。
pub fn rect_mask_clipped(rect: IRect, clip: IRect) -> SelMask {
    let clipped = IRect {
        x0: rect.x0.max(clip.x0),
        y0: rect.y0.max(clip.y0),
        x1: rect.x1.min(clip.x1),
        y1: rect.y1.min(clip.y1),
    };
    rect_mask(clipped)
}

/// v8 レビュー修正: `ellipse_mask(rect).clamp_to(..)` と同値の clipped 版
/// (`rect_mask_clipped` のコメント参照)。楕円の中心・半径は**クリップ前の
/// `rect`**から計算する(クリップ後の矩形へ内接させ直すと別の楕円になって
/// しまう — `app.rs::select_up` の v4 レビュー修正コメントが説明する既知の
/// 罠。この関数はその「先に作ってからクリップ」の意味論を、確保だけ
/// クリップ後に行う形で維持する)。
pub fn ellipse_mask_clipped(rect: IRect, clip: IRect) -> SelMask {
    if rect.is_empty() {
        return SelMask::empty();
    }
    let bbox = IRect {
        x0: rect.x0.max(clip.x0),
        y0: rect.y0.max(clip.y0),
        x1: rect.x1.min(clip.x1),
        y1: rect.y1.min(clip.y1),
    };
    if bbox.is_empty() {
        return SelMask::empty();
    }
    let w = bbox.width() as usize;
    let h = bbox.height() as usize;
    let mut mask = vec![0u8; w * h];
    let cx = (rect.x0 + rect.x1) as f32 / 2.0;
    let cy = (rect.y0 + rect.y1) as f32 / 2.0;
    let rx = rect.width() as f32 / 2.0;
    let ry = rect.height() as f32 / 2.0;
    if rx > 0.0 && ry > 0.0 {
        for y in 0..h {
            let ny = (bbox.y0 + y as i32) as f32 + 0.5 - cy;
            let ny = ny / ry;
            if ny.abs() > 1.0 {
                continue;
            }
            let row = y * w;
            for x in 0..w {
                let nx = ((bbox.x0 + x as i32) as f32 + 0.5 - cx) / rx;
                if nx * nx + ny * ny <= 1.0 {
                    mask[row + x] = 255;
                }
            }
        }
    }
    SelMask { bbox, mask }
}

/// v8 レビュー修正: `polygon_mask(points).clamp_to(..)` と同値の clipped 版
/// (`rect_mask_clipped` のコメント参照)。偶奇規則の交点リストは行ごとに
/// 全頂点から作るため、クリップで左側の交点が bbox 外になっても、走査開始時の
/// `while` が先にそれらを消費して正しい内外状態から始まる(`polygon_mask` と
/// 同じ走査コード)。
pub fn polygon_mask_clipped(points: &[Pos2], clip: IRect) -> SelMask {
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
        x0: (min_x.floor() as i32).max(clip.x0),
        y0: (min_y.floor() as i32).max(clip.y0),
        x1: (max_x.ceil() as i32).min(clip.x1),
        y1: (max_y.ceil() as i32).min(clip.y1),
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

/// `rect` に内接する楕円のマスク(SPEC §22: 「楕円選択」)。
/// `raster::fill_ellipse` と全く同じ判定式(`(x+0.5-cx)^2/rx^2 +
/// (y+0.5-cy)^2/ry^2 <= 1`)を使うため、同じ外接矩形の楕円図形と選択で
/// 見た目が一致する。`rx`/`ry` のどちらかが 0 以下(矩形が退化している)
/// なら空マスクを返す(パニックしない)。
///
/// v8 レビュー修正後、本体コードは確保をクリップ後に限定する
/// `ellipse_mask_clipped` を使う。こちらは「作ってから `clamp_to`」との
/// バイト同値性を担保するテストの参照実装として残す
/// (`clipped_masks_match_build_then_clamp_for_all_shapes`。
/// `Document::active_pixels` と同じ「テスト専用に残す」流儀)。
#[allow(dead_code)]
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
///
/// v8 レビュー修正後、本体コードは `polygon_mask_clipped` を使う。こちらは
/// 同値性テストの参照実装として残す(`ellipse_mask` と同じ理由)。
#[allow(dead_code)]
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

/// v5 §31(ARCHITECTURE.md §17.5): 「選択範囲を新規タブに複製」の浮動片
/// ケース専用。`floating.pixels` をそのまま新規タブのレイヤーにすると、
/// マスク外の画素(ハンドルでリサイズした際の再サンプリングなどで不透明な
/// 値が残っていてもおかしくない、`composite_floating_skips_pixels_outside_mask`
/// 参照)が「選択されていないのに見えてしまう」ことになる。`composite_floating`
/// が実際の合成時にマスク外を無視するのと同じ意味論を、新規レイヤーの
/// ピクセルバッファ自体にも焼き込む(SPEC §31: 「その浮動片のピクセル
/// (mask込み)をそのまま新規タブの唯一のレイヤーにする」)。
pub fn floating_layer_pixels(floating: &Floating) -> Vec<u8> {
    let count = (floating.w as usize).saturating_mul(floating.h as usize);
    let mut out = vec![0u8; count * 4];
    for i in 0..count {
        if floating.mask.get(i).copied().unwrap_or(0) == 0 {
            continue;
        }
        let idx = i * 4;
        if let Some(src) = floating.pixels.get(idx..idx + 4) {
            out[idx..idx + 4].copy_from_slice(src);
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

/// v8 §37: 選択マスクの補集合(ドキュメント範囲内)。`width`×`height` の
/// 全画素のうち `mask` で選択されていない画素だけを選択した新しいマスクを
/// 返す。結果の bbox は非ゼロ画素を含む最小矩形へ詰める(`tighten_mask`) —
/// 「選択範囲でトリミング」「浮動化」等の bbox 依存の操作が、反転後の選択
/// (例: 左半分の反転=右半分)に対しても自然に働くようにするため。
/// 全画素が選択済みなら `SelMask::empty()`(SPEC §37: 「全選択の反転は
/// 選択解除と同じ」)。`mask` は先に `clamp_to` で範囲内へ切り詰めてから
/// 使うので、範囲外へはみ出した bbox を渡してもパニックしない。
pub fn invert_mask(mask: &SelMask, width: u32, height: u32) -> SelMask {
    if width == 0 || height == 0 {
        return SelMask::empty();
    }
    let full = IRect {
        x0: 0,
        y0: 0,
        x1: width as i32,
        y1: height as i32,
    };
    let clamped = mask.clamp_to(width, height);
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![255u8; w * h];
    if !clamped.is_empty() {
        let mw = clamped.bbox.width() as usize;
        for y in clamped.bbox.y0..clamped.bbox.y1 {
            let my = (y - clamped.bbox.y0) as usize;
            let row = &clamped.mask[my * mw..(my + 1) * mw];
            let out_start = y as usize * w + clamped.bbox.x0 as usize;
            let out_row = &mut out[out_start..out_start + mw];
            for (dst, &src) in out_row.iter_mut().zip(row) {
                if src != 0 {
                    *dst = 0;
                }
            }
        }
    }
    tighten_mask(SelMask {
        bbox: full,
        mask: out,
    })
}

/// 非ゼロ画素を含む最小の bbox へ詰め直す(全ゼロなら `SelMask::empty()`)。
/// `invert_mask` 専用のヘルパー(既存の選択生成経路 — 矩形/楕円/多角形/
/// flood — は生成時点で bbox がタイトなので不要)。
fn tighten_mask(mask: SelMask) -> SelMask {
    if mask.is_empty() {
        return SelMask::empty();
    }
    let w = mask.bbox.width() as usize;
    let h = mask.bbox.height() as usize;
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = h;
    let mut max_y = 0usize;
    for y in 0..h {
        let row = &mask.mask[y * w..(y + 1) * w];
        let Some(first) = row.iter().position(|&v| v != 0) else {
            continue;
        };
        // `first` が存在する行では `rposition` も必ず見つかる(同じ行を逆順に
        // 走査するだけ)が、不変条件に頼らず `first` へフォールバックする。
        let last = row.iter().rposition(|&v| v != 0).unwrap_or(first);
        min_x = min_x.min(first);
        max_x = max_x.max(last);
        min_y = min_y.min(y);
        max_y = y;
    }
    if min_y > max_y || min_x > max_x {
        return SelMask::empty();
    }
    if min_x == 0 && min_y == 0 && max_x + 1 == w && max_y + 1 == h {
        return mask;
    }
    let nw = max_x - min_x + 1;
    let nh = max_y - min_y + 1;
    let mut out = vec![0u8; nw * nh];
    for y in 0..nh {
        let start = (min_y + y) * w + min_x;
        out[y * nw..(y + 1) * nw].copy_from_slice(&mask.mask[start..start + nw]);
    }
    SelMask {
        bbox: IRect {
            x0: mask.bbox.x0 + min_x as i32,
            y0: mask.bbox.y0 + min_y as i32,
            x1: mask.bbox.x0 + (max_x + 1) as i32,
            y1: mask.bbox.y0 + (max_y + 1) as i32,
        },
        mask: out,
    }
}

/// v8 §38: `extract_region` の**合成結果**(可視レイヤー合成、
/// `Document::composite_pixel`)版。「結合部分をコピー」がスポイト
/// (SPEC §13: 「スポイトは合成結果から色を取る」)と同じ意味論で画素を
/// 読むために使う。呼び出し側は `Document::recompose_if_dirty` 等で
/// `composite` を最新化してから呼ぶこと。
pub fn extract_region_composite(doc: &Document, mask: &SelMask) -> Vec<u8> {
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
                .composite_pixel(rect.x0 + x as i32, rect.y0 + y as i32)
                .unwrap_or([0, 0, 0, 0]);
            let idx = (y * w + x) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

/// v8 §38: 抽出済みバッファ `out`(`region` = 抽出元の bbox、RGBA8・行優先)の
/// 上へ、浮動片を画面表示と同じ見た目(合成結果の上に source-over)で重ねる。
/// 「結合部分をコピー」中に浮動片がある場合、ドキュメントを一切変更せずに
/// 「見えているとおり」の画素を作るために使う(`composite_floating` と同じ
/// マスク・ブレンド規則だが、書き込み先が `Document` ではなくこのバッファ)。
pub fn overlay_floating_onto_region(out: &mut [u8], region: IRect, floating: &Floating) {
    let target = floating_target_rect(floating);
    let overlap = IRect {
        x0: region.x0.max(target.x0),
        y0: region.y0.max(target.y0),
        x1: region.x1.min(target.x1),
        y1: region.y1.min(target.y1),
    };
    if overlap.is_empty() {
        return;
    }
    let region_w = region.width() as usize;
    let src_w = floating.w as usize;
    for y in overlap.y0..overlap.y1 {
        let sy = (y - target.y0) as usize;
        let oy = (y - region.y0) as usize;
        for x in overlap.x0..overlap.x1 {
            let sx = (x - target.x0) as usize;
            let midx = sy * src_w + sx;
            if floating.mask.get(midx).copied().unwrap_or(0) == 0 {
                continue;
            }
            let sidx = midx * 4;
            let Some(src) = floating.pixels.get(sidx..sidx + 4) else {
                continue;
            };
            let src_px = [src[0], src[1], src[2], src[3]];
            let oidx = (oy * region_w + (x - region.x0) as usize) * 4;
            let Some(dst) = out.get_mut(oidx..oidx + 4) else {
                continue;
            };
            let dst_px = [dst[0], dst[1], dst[2], dst[3]];
            dst.copy_from_slice(&raster::blend_over(dst_px, src_px));
        }
    }
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

    // -- v8 レビュー修正: clipped マスク(確保をクリップ後に限定) ------------

    /// clipped 版は「作ってから `clamp_to`」の従来経路とバイト同値であること
    /// (`app.rs::select_up` の v4 レビュー修正が要求する楕円の意味論
    /// — クリップ前の外接矩形から方程式を評価する — を含む)。
    #[test]
    fn clipped_masks_match_build_then_clamp_for_all_shapes() {
        let doc_rect = IRect {
            x0: 0,
            y0: 0,
            x1: 20,
            y1: 15,
        };
        // キャンバスを大きくはみ出すドラッグ矩形。
        let drag = IRect {
            x0: -30,
            y0: -10,
            x1: 45,
            y1: 40,
        };
        let old_rect = rect_mask(drag).clamp_to(20, 15);
        let new_rect = rect_mask_clipped(drag, doc_rect);
        assert_eq!(old_rect.bbox, new_rect.bbox);
        assert_eq!(old_rect.mask, new_rect.mask);

        let old_ellipse = ellipse_mask(drag).clamp_to(20, 15);
        let new_ellipse = ellipse_mask_clipped(drag, doc_rect);
        assert_eq!(old_ellipse.bbox, new_ellipse.bbox);
        assert_eq!(old_ellipse.mask, new_ellipse.mask);

        let poly = [
            pos2(-25.0, -5.0),
            pos2(40.0, 2.0),
            pos2(30.0, 35.0),
            pos2(-10.0, 20.0),
        ];
        let old_poly = polygon_mask(&poly).clamp_to(20, 15);
        let new_poly = polygon_mask_clipped(&poly, doc_rect);
        assert_eq!(old_poly.bbox, new_poly.bbox);
        assert_eq!(old_poly.mask, new_poly.mask);
    }

    #[test]
    fn clipped_masks_allocate_only_the_intersection_for_huge_drags() {
        // v8 レビュー修正の本題: 途方もないドラッグ座標でも確保はクリップ
        // 後の寸法(ここでは 8×8)に留まり、OOM しない。
        let doc_rect = IRect {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        };
        let huge = IRect {
            x0: -2_000_000,
            y0: -2_000_000,
            x1: 2_000_000,
            y1: 2_000_000,
        };
        let mask = rect_mask_clipped(huge, doc_rect);
        assert_eq!(mask.mask.len(), 64);
        let ellipse = ellipse_mask_clipped(huge, doc_rect);
        assert_eq!(ellipse.mask.len(), 64);
        let poly = [
            pos2(-2_000_000.0, -2_000_000.0),
            pos2(2_000_000.0, -2_000_000.0),
            pos2(0.0, 2_000_000.0),
        ];
        let mask = polygon_mask_clipped(&poly, doc_rect);
        assert_eq!(mask.mask.len(), 64);
    }

    #[test]
    fn clipped_masks_entirely_outside_the_clip_are_empty() {
        let doc_rect = IRect {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        };
        let outside = IRect {
            x0: 10,
            y0: 10,
            x1: 20,
            y1: 20,
        };
        assert!(rect_mask_clipped(outside, doc_rect).is_empty());
        assert!(ellipse_mask_clipped(outside, doc_rect).is_empty());
        let poly = [pos2(10.0, 10.0), pos2(20.0, 10.0), pos2(15.0, 20.0)];
        assert!(polygon_mask_clipped(&poly, doc_rect).is_empty());
    }

    // -- v8 §37: 選択範囲を反転(invert_mask / tighten_mask) -----------------

    #[test]
    fn invert_mask_of_a_centered_rect_selects_the_complement_ring() {
        let rect = IRect {
            x0: 2,
            y0: 2,
            x1: 6,
            y1: 6,
        };
        let inverted = invert_mask(&rect_mask(rect), 10, 10);
        // 反転結果は 4 辺すべてに接するのでドキュメント全域が bbox のまま。
        assert_eq!((inverted.bbox.x0, inverted.bbox.y0), (0, 0));
        assert_eq!((inverted.bbox.x1, inverted.bbox.y1), (10, 10));
        // 元の選択内は非選択、外は選択。
        assert!(!inverted.contains(3, 3));
        assert!(!inverted.contains(2, 2));
        assert!(!inverted.contains(5, 5));
        assert!(inverted.contains(6, 6));
        assert!(inverted.contains(0, 0));
        assert!(inverted.contains(9, 9));
        assert!(inverted.contains(1, 4));
    }

    #[test]
    fn invert_mask_of_the_left_half_is_the_tight_right_half() {
        // bbox のタイト化: 左半分の反転は「右半分ちょうど」の bbox になる
        // (SPEC §37: 「選択範囲でトリミング」等の bbox 依存操作のため)。
        let left = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 10,
        });
        let inverted = invert_mask(&left, 10, 10);
        assert_eq!(
            (
                inverted.bbox.x0,
                inverted.bbox.y0,
                inverted.bbox.x1,
                inverted.bbox.y1
            ),
            (5, 0, 10, 10)
        );
        assert!(inverted.contains(5, 0));
        assert!(inverted.contains(9, 9));
        assert!(!inverted.contains(4, 5));
    }

    #[test]
    fn invert_mask_of_a_full_selection_is_empty() {
        let full = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        });
        assert!(invert_mask(&full, 8, 8).is_empty());
    }

    #[test]
    fn invert_mask_twice_round_trips_to_the_original_selection() {
        // 楕円のような非矩形マスクでも二重反転で元に戻る(クランプ済み範囲)。
        let ellipse = ellipse_mask(IRect {
            x0: 1,
            y0: 2,
            x1: 9,
            y1: 8,
        });
        let once = invert_mask(&ellipse, 10, 10);
        let twice = invert_mask(&once, 10, 10);
        for y in 0..10 {
            for x in 0..10 {
                assert_eq!(
                    twice.contains(x, y),
                    ellipse.contains(x, y),
                    "mismatch at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn invert_mask_clamps_an_out_of_bounds_selection_before_inverting() {
        // ドキュメント外へはみ出した bbox を渡してもパニックせず、範囲内の
        // 補集合になる。
        let mask = rect_mask(IRect {
            x0: -3,
            y0: -3,
            x1: 4,
            y1: 4,
        });
        let inverted = invert_mask(&mask, 8, 8);
        assert!(!inverted.contains(0, 0));
        assert!(!inverted.contains(3, 3));
        assert!(inverted.contains(4, 4));
        assert!(inverted.contains(7, 0));
    }

    #[test]
    fn invert_mask_of_an_empty_or_degenerate_document_does_not_panic() {
        let sel = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        });
        assert!(invert_mask(&sel, 0, 0).is_empty());
        assert!(invert_mask(&SelMask::empty(), 0, 5).is_empty());
        // 選択なしの反転=全選択(呼び出し側はこの経路を使わないが純関数の
        // 意味論として)。
        let all = invert_mask(&SelMask::empty(), 4, 4);
        assert_eq!((all.bbox.x1, all.bbox.y1), (4, 4));
        assert!(all.contains(0, 0) && all.contains(3, 3));
    }

    // -- v8 §38: 結合部分をコピー(extract_region_composite /
    // overlay_floating_onto_region) ------------------------------------------

    #[test]
    fn extract_region_composite_reads_the_merged_result_not_the_active_layer() {
        let mut doc = Document::new(4, 4, Background::Transparent);
        doc.set_pixel(1, 1, [255, 0, 0, 255]); // 背景レイヤーに赤
        assert!(doc.add_layer("上".to_owned()));
        doc.set_pixel(1, 1, [0, 0, 255, 255]); // 上のレイヤーに青
        doc.active = 0; // アクティブを背景(赤)に戻す
        doc.recomposite_full();
        let mask = rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        });
        let active = extract_region(&doc, &mask);
        let merged = extract_region_composite(&doc, &mask);
        // (1,1) は幅 4 の行優先で index 5、RGBA なので byte offset 20。
        let idx = 5 * 4;
        assert_eq!(&active[idx..idx + 4], &[255, 0, 0, 255], "active = 赤");
        assert_eq!(&merged[idx..idx + 4], &[0, 0, 255, 255], "合成 = 上の青");
    }

    #[test]
    fn extract_region_composite_respects_the_selection_mask() {
        let mut doc = Document::new(2, 1, Background::White);
        doc.recomposite_full();
        let sel = SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
            mask: vec![255, 0],
        };
        let merged = extract_region_composite(&doc, &sel);
        assert_eq!(&merged[0..4], &[255, 255, 255, 255], "選択画素は合成値");
        assert_eq!(&merged[4..8], &[0, 0, 0, 0], "非選択画素は透明");
    }

    #[test]
    fn overlay_floating_onto_region_blends_like_the_screen() {
        // 抽出済みの白い 2x1 バッファへ、半透明赤の浮動片(左画素のみ選択)を
        // 重ねる: 左は blend_over(白, 半透明赤)、右は白のまま。
        let region = IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 1,
        };
        let mut out = vec![255u8; 2 * 4];
        let floating = Floating::new(
            vec![255, 0, 0, 128, 255, 0, 0, 128],
            2,
            1,
            vec![255, 0],
            pos2(0.0, 0.0),
            None,
            1,
        );
        overlay_floating_onto_region(&mut out, region, &floating);
        let expected = raster::blend_over([255, 255, 255, 255], [255, 0, 0, 128]);
        assert_eq!(&out[0..4], &expected);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn overlay_floating_onto_region_clips_a_partially_overlapping_floating() {
        // region(0..2, 0..1)の右端 1 画素にだけ浮動片(pos=(1,0)、2x1)が
        // 重なるケース。はみ出し分は無視され、パニックしない。
        let region = IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 1,
        };
        let mut out = vec![0u8; 2 * 4];
        let floating = Floating::new_rect(
            vec![9, 9, 9, 255, 8, 8, 8, 255],
            2,
            1,
            pos2(1.0, 0.0),
            None,
            1,
        );
        overlay_floating_onto_region(&mut out, region, &floating);
        assert_eq!(&out[0..4], &[0, 0, 0, 0], "浮動片の外は不変");
        assert_eq!(&out[4..8], &[9, 9, 9, 255], "浮動片の 1 画素目だけ重なる");
    }

    // -- v5 §31(ARCHITECTURE.md §17.5): 選択範囲を新規タブに複製・浮動片 -----

    #[test]
    fn floating_layer_pixels_zeroes_out_pixels_outside_mask() {
        // `composite_floating_skips_pixels_outside_mask` と同じ意味論:
        // pixels 側が不透明な値を持っていても、mask==0 の画素は透明として
        // 扱う(SPEC §31: 「そのピクセル(mask込み)をそのまま」)。
        let pixels = [9u8, 9, 9, 255].repeat(4); // 2x2 全画素不透明
        let mask = vec![255, 0, 0, 255]; // 左上・右下だけ選択
        let floating = Floating::new(pixels, 2, 2, mask, pos2(0.0, 0.0), None, 1);
        let out = floating_layer_pixels(&floating);
        assert_eq!(&out[0..4], &[9, 9, 9, 255], "masked-in top-left kept");
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "masked-out top-right zeroed");
        assert_eq!(&out[8..12], &[0, 0, 0, 0], "masked-out bottom-left zeroed");
        assert_eq!(&out[12..16], &[9, 9, 9, 255], "masked-in bottom-right kept");
    }

    #[test]
    fn floating_layer_pixels_keeps_fully_masked_pixels_as_is() {
        let floating = Floating::new_rect(
            vec![1, 2, 3, 255, 4, 5, 6, 128],
            2,
            1,
            pos2(0.0, 0.0),
            None,
            1,
        );
        let out = floating_layer_pixels(&floating);
        assert_eq!(out, vec![1, 2, 3, 255, 4, 5, 6, 128]);
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
    fn floating_defers_the_resample_source_until_the_first_resize() {
        // v8 レビュー修正③: 生成時には original を複製しない(大半の浮動片は
        // 一度も拡縮されないため)。最初の拡縮の直前に
        // `ensure_resample_source` が現在の画素を確定し、以後は不変。
        let mut floating = Floating::new_rect(vec![1, 2, 3, 4], 1, 1, pos2(0.0, 0.0), None, 9);
        assert!(floating.original.is_empty(), "生成時は複製を持たない");
        assert!(floating.orig_mask.is_empty());
        assert_eq!(floating.mask, vec![255u8]);

        floating.ensure_resample_source();
        assert_eq!(floating.original, floating.pixels);
        assert_eq!((floating.orig_w, floating.orig_h), (1, 1));
        assert_eq!(floating.orig_mask, floating.mask);

        // 2 回目の呼び出しは何もしない(元は「浮動化時点」のまま)。
        floating.pixels = vec![9, 9, 9, 9];
        floating.ensure_resample_source();
        assert_eq!(floating.original, vec![1, 2, 3, 4]);

        // 明示的に破棄すれば次の確定で新しい画素が元になる(v9 の浮動片
        // 反転/回転向け)。
        floating.reset_resample_source();
        floating.ensure_resample_source();
        assert_eq!(floating.original, vec![9, 9, 9, 9]);
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
