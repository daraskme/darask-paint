//! アンドゥ/リドゥ(ARCHITECTURE.md §6, v2: §14.2)。タイル Copy-on-Write 方式。
//!
//! 1 ストローク・1 図形・1 塗りつぶし・1 貼り付け確定 = 1 undo 単位(SPEC §9)。
//! `History::begin_stroke` → (ツールが描くたびに) `ensure_tiles_saved` →
//! `commit_stroke` という流れで、ストローク開始前の元ピクセルをタイル単位で
//! 遅延退避し、ストローク確定時にタッチしたタイル群の外接矩形から
//! `HistoryOp::Patch` を作って push する。
//!
//! v2(ARCHITECTURE.md §14.2)でレイヤーに対応した:
//! - `Patch` はどのレイヤーに対する変更かを `layer: usize` として持つ。
//!   `begin_stroke(layer)` で記録し、`commit_stroke` がそのまま `Patch` に
//!   焼き込む(レイヤー切替は「先に確定」を経由するため、ストローク中に
//!   `layer` が変わることはない)。
//! - サイズが変わる操作(resize/crop/rotate/画像の統合等)は、単一バッファの
//!   `Replace` ではなく全レイヤー+寸法のスナップショットを持つ `ReplaceAll`
//!   になった。
//!
//! CoW タイルの退避(`ensure_saved`)自体は生のピクセルバッファ
//! (`width`/`height`/`pixels`)だけを見る、`Document`/`Layer` を知らない
//! ロジックのまま据え置く(`raster::flood_fill` の `before_write` コールバックが
//! `Surface` からしか読めないため)。

use std::collections::HashMap;

use crate::document::{composite_two, DocSnapshot, Document, IRect, Layer};

/// タイルサイズ(ARCHITECTURE.md §6: 256×256)。
const TILE_SIZE: i32 = 256;

/// アンドゥ履歴の合計メモリ上限(SPEC §9: 256MB)。
const MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// 上限を超えても必ず保持する直近の件数(SPEC §9)。
const MIN_KEEP: usize = 10;

/// 1 回のアンドゥ単位。
///
/// v2(ARCHITECTURE.md §14.2)でレイヤー構造の変更用に軽量な op を追加した:
/// 全レイヤーの前後スナップショットを取る `ReplaceAll` は寸法・レイヤー構成が
/// 丸ごと変わりうる操作(resize/crop/rotate/反転/画像の統合)にだけ使い、
/// 1 枚のレイヤーの追加・複製・削除・入れ替え・下と結合は、影響するレイヤー
/// (最大 2 枚)だけを保持する専用の op にする。これにより、多レイヤー・
/// 大サイズのドキュメントで「新規レイヤー」を 1 回押しただけで履歴が
/// 全レイヤー×2 分のメモリを消費し 256MB 上限を単独で超過する、という
/// 問題を避ける。
pub enum HistoryOp {
    /// 部分パッチ(ストローク/図形/塗りつぶし/貼り付け確定など)。`layer` は
    /// このパッチが適用される `Document::layers` の添字(ARCHITECTURE.md
    /// §14.2)。
    Patch {
        layer: usize,
        rect: IRect,
        before: Vec<u8>,
        after: Vec<u8>,
    },
    /// 新規の空(透明)レイヤー追加(`layer_add`)。undo=`index` の削除、
    /// redo=`name` から空レイヤーを再構築(常に透明なので画素データは持たない)。
    AddLayer {
        index: usize,
        name: String,
        /// 追加前にアクティブだったレイヤー添字(undo で復元する)。
        before_active: usize,
    },
    /// アクティブレイヤーの複製(`layer_duplicate`)。undo=`index` の削除、
    /// redo=保持している `layer`(複製結果そのもの)を再挿入する。
    DuplicateLayer {
        index: usize,
        layer: Layer,
        before_active: usize,
    },
    /// レイヤー削除(`layer_delete`)。undo=`layer` を `index` へ復元。
    RemoveLayer {
        index: usize,
        layer: Layer,
        before_active: usize,
    },
    /// レイヤーの入れ替え(`layer_move_up`/`layer_move_down`)。`from`/`to` は
    /// スワップする 2 添字(スワップは自身の逆操作なので undo/redo とも同じ
    /// スワップを行い、アクティブ添字だけを向きに応じて書き分ける)。
    MoveLayer { from: usize, to: usize },
    /// 下と結合(`layer_merge_down`)。undo=結合前の 2 レイヤーへ復元、
    /// redo=`lower_before`/`upper` から結合結果を再計算する(結合結果自体は
    /// 保持せず、必要になったときに再合成することでメモリを節約する)。
    MergeDown {
        /// 結合前のアクティブ(上)レイヤーの添字。結合後の下レイヤーは
        /// `index - 1`。
        index: usize,
        upper: Layer,
        lower_before: Layer,
    },
    /// サイズ・レイヤー構成が丸ごと変わりうる操作(resize/crop/rotate/
    /// canvas resize/貼り付けによるドキュメント全体置き換え/画像の統合等)。
    ReplaceAll {
        before: DocSnapshot,
        after: DocSnapshot,
    },
}

impl HistoryOp {
    fn byte_size(&self) -> usize {
        match self {
            HistoryOp::Patch { before, after, .. } => before.len() + after.len(),
            HistoryOp::AddLayer { name, .. } => name.len(),
            HistoryOp::DuplicateLayer { layer, .. } | HistoryOp::RemoveLayer { layer, .. } => {
                layer.pixels.len()
            }
            HistoryOp::MoveLayer { .. } => 0,
            HistoryOp::MergeDown {
                upper,
                lower_before,
                ..
            } => upper.pixels.len() + lower_before.pixels.len(),
            HistoryOp::ReplaceAll { before, after } => {
                snapshot_bytes(before) + snapshot_bytes(after)
            }
        }
    }
}

fn snapshot_bytes(snap: &DocSnapshot) -> usize {
    snap.layers.iter().map(|l| l.pixels.len()).sum()
}

/// ストローク中にタイル単位で「触る前」のピクセルを退避しておく記録係。
struct TileSnapshot {
    rect: IRect,
    pixels: Vec<u8>,
}

struct StrokeRecorder {
    /// このストロークが対象とするレイヤー(開始時の `Document::active`)。
    layer: usize,
    tiles: HashMap<(i32, i32), TileSnapshot>,
    /// これまでに `ensure_tiles_saved` に渡された矩形の外接矩形。
    touched: Option<IRect>,
}

impl StrokeRecorder {
    fn new(layer: usize) -> Self {
        Self {
            layer,
            tiles: HashMap::new(),
            touched: None,
        }
    }

    /// `rect`(これから raster 関数が書き込む予定の領域)が触れるタイルを、
    /// まだ退避していなければ `pixels`(このストロークが対象とするレイヤーの
    /// 現在のバッファ)から複写して保存する。
    fn ensure_saved(&mut self, width: u32, height: u32, pixels: &[u8], rect: IRect) {
        let rect = rect.clamp_to(width, height);
        if rect.is_empty() {
            return;
        }
        self.touched = Some(match self.touched {
            Some(existing) => existing.union(&rect),
            None => rect,
        });

        let tx0 = rect.x0.div_euclid(TILE_SIZE);
        let ty0 = rect.y0.div_euclid(TILE_SIZE);
        let tx1 = (rect.x1 - 1).div_euclid(TILE_SIZE);
        let ty1 = (rect.y1 - 1).div_euclid(TILE_SIZE);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                self.tiles.entry((tx, ty)).or_insert_with(|| {
                    let tile_rect = IRect {
                        x0: tx * TILE_SIZE,
                        y0: ty * TILE_SIZE,
                        x1: (tx + 1) * TILE_SIZE,
                        y1: (ty + 1) * TILE_SIZE,
                    }
                    .clamp_to(width, height);
                    TileSnapshot {
                        rect: tile_rect,
                        pixels: copy_region(pixels, width, tile_rect),
                    }
                });
            }
        }
    }

    /// ストローク確定。触れた領域がなければ `None`(1 px も塗らなかった等)。
    fn finish(self, width: u32, height: u32, pixels: &[u8]) -> Option<HistoryOp> {
        let bbox = self.touched?.clamp_to(width, height);
        if bbox.is_empty() {
            return None;
        }
        let after = copy_region(pixels, width, bbox);
        // before は「現在のバッファ」から始め、退避済みタイルで上書きする。
        // タイルが退避されていない = そのタイルは一度も書き込まれていない
        // ということなので、現在値がそのまま元の値になる。
        let mut before = after.clone();
        let bbox_w = bbox.width() as usize;
        for snap in self.tiles.values() {
            let overlap = intersect(snap.rect, bbox);
            if overlap.is_empty() {
                continue;
            }
            let tile_w = snap.rect.width() as usize;
            for y in overlap.y0..overlap.y1 {
                let src_start = ((y - snap.rect.y0) as usize * tile_w
                    + (overlap.x0 - snap.rect.x0) as usize)
                    * 4;
                let dst_start =
                    ((y - bbox.y0) as usize * bbox_w + (overlap.x0 - bbox.x0) as usize) * 4;
                let len = overlap.width() as usize * 4;
                before[dst_start..dst_start + len]
                    .copy_from_slice(&snap.pixels[src_start..src_start + len]);
            }
        }
        Some(HistoryOp::Patch {
            layer: self.layer,
            rect: bbox,
            before,
            after,
        })
    }
}

fn copy_region(pixels: &[u8], width: u32, rect: IRect) -> Vec<u8> {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut out = vec![0u8; w * h * 4];
    if w == 0 || h == 0 {
        return out;
    }
    for y in 0..h {
        let src_start = ((rect.y0 as usize + y) * width as usize + rect.x0 as usize) * 4;
        let out_start = y * w * 4;
        out[out_start..out_start + w * 4].copy_from_slice(&pixels[src_start..src_start + w * 4]);
    }
    out
}

fn paste_region(pixels: &mut [u8], width: u32, height: u32, rect: IRect, data: &[u8]) {
    let rect = rect.clamp_to(width, height);
    if rect.is_empty() {
        return;
    }
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    for y in 0..h {
        let dst_start = ((rect.y0 as usize + y) * width as usize + rect.x0 as usize) * 4;
        let src_start = y * w * 4;
        if dst_start + w * 4 <= pixels.len() && src_start + w * 4 <= data.len() {
            pixels[dst_start..dst_start + w * 4]
                .copy_from_slice(&data[src_start..src_start + w * 4]);
        }
    }
}

fn intersect(a: IRect, b: IRect) -> IRect {
    IRect {
        x0: a.x0.max(b.x0),
        y0: a.y0.max(b.y0),
        x1: a.x1.min(b.x1),
        y1: a.y1.min(b.y1),
    }
}

/// アンドゥ/リドゥスタックとメモリ会計、進行中ストロークの記録を持つ。
pub struct History {
    undo: Vec<HistoryOp>,
    redo: Vec<HistoryOp>,
    bytes_used: usize,
    stroke: Option<StrokeRecorder>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            bytes_used: 0,
            stroke: None,
        }
    }

    /// ストローク(1 undo 単位)の記録を開始する。`layer` はこのストロークが
    /// 対象とするレイヤー(通常は呼び出し時点の `Document::active`、
    /// ARCHITECTURE.md §14.2)。
    pub fn begin_stroke(&mut self, layer: usize) {
        self.stroke = Some(StrokeRecorder::new(layer));
    }

    /// ストローク記録中であれば、`rect` が触れるタイルを(ストロークが対象と
    /// する `doc` のアクティブレイヤーから)退避する。記録中でなければ何も
    /// しない(誤って stroke 外で呼ばれても安全)。
    pub fn ensure_tiles_saved(&mut self, doc: &Document, rect: IRect) {
        if let Some(stroke) = &mut self.stroke {
            let Some(layer) = doc.layers.get(stroke.layer) else {
                return;
            };
            stroke.ensure_saved(doc.width, doc.height, &layer.pixels, rect);
        }
    }

    /// `ensure_tiles_saved` のバッファ版。`raster::flood_fill` の
    /// `before_write` コールバックは `Surface`(生バッファ)しか持たないため、
    /// `Document` を経由できない箇所から使う(`tools/fill.rs`)。
    pub fn ensure_tiles_saved_buf(&mut self, width: u32, height: u32, pixels: &[u8], rect: IRect) {
        if let Some(stroke) = &mut self.stroke {
            stroke.ensure_saved(width, height, pixels, rect);
        }
    }

    /// 進行中のストロークにおける `(x, y)` の「ストローク開始前」のピクセル値を、
    /// 退避済みの CoW タイル(§6)から返す。まだそのタイルが退避されていない
    /// (= 一度も書き込まれていない = 現在値がそのまま元の値)場合や、
    /// ストローク記録中でない場合は `None`(ARCHITECTURE.md §5 のペン AA モードが、
    /// 毎スタンプ「元画像」から合成し直すために使う)。
    pub fn original_pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        let stroke = self.stroke.as_ref()?;
        let tx = x.div_euclid(TILE_SIZE);
        let ty = y.div_euclid(TILE_SIZE);
        let snap = stroke.tiles.get(&(tx, ty))?;
        let rect = snap.rect;
        if x < rect.x0 || x >= rect.x1 || y < rect.y0 || y >= rect.y1 {
            return None;
        }
        let w = rect.width() as usize;
        let idx = ((y - rect.y0) as usize * w + (x - rect.x0) as usize) * 4;
        snap.pixels
            .get(idx..idx + 4)
            .map(|s| [s[0], s[1], s[2], s[3]])
    }

    /// ストロークを確定し、触れた領域があれば 1 つの `Patch` として push する。
    ///
    /// `doc.modified` はここで実際に op が push されたときにだけ true にする
    /// (M4 で発見・修正したバグ: 以前は undo/redo の `apply_before`/
    /// `apply_after` でしか `modified` を立てておらず、通常の編集
    /// (ペン/消しゴム/図形/塗りつぶし/選択の確定など、すべてこの関数を
    /// 経由する)ではタイトルバーの `*` や未保存ガードが働かなかった)。
    pub fn commit_stroke(&mut self, doc: &mut Document) {
        if let Some(stroke) = self.stroke.take() {
            let layer_idx = stroke.layer;
            let Some(layer) = doc.layers.get(layer_idx) else {
                return;
            };
            let pixels = &layer.pixels;
            if let Some(op) = stroke.finish(doc.width, doc.height, pixels) {
                doc.modified = true;
                self.push(op);
            }
        }
    }

    /// 進行中のストローク記録を破棄する(何も push しない)。
    #[allow(dead_code)]
    pub fn cancel_stroke(&mut self) {
        self.stroke = None;
    }

    /// ストローク記録中(浮動片保持中を含む)か。
    ///
    /// M4 で発見・修正したバグ: `undo`/`redo` はこれを一切確認せずに呼べて
    /// いたため、ストローク中(CoW タイル退避済みだが未確定)に undo すると
    /// タイルの「退避時点のピクセル」がドキュメントの実状態と食い違い、
    /// 確定時に作られる `Patch` の `before` が破損する。M4 では app.rs 側の
    /// undo/redo ショートカット・メニューをこれでガード(丸ごとブロック)
    /// することで対処したが、v2 のレビューで SPEC §13 最終項
    /// (「浮動片やストローク進行中にはツール切替と同じ扱い(先に確定して
    /// から実行)」)に反すると判明し、app.rs 側は `commit_open_gesture()`
    /// で先に確定してから undo/redo するよう修正した(確定によって
    /// 「進行中」状態が解消されるため、このメソッドが警告する食い違いは
    /// 起こらなくなる)。このメソッド自体は「進行中ストロークがあるか」を
    /// 問い合わせる目的で今も使われている(例: 貼り付け直後の undo 有効
    /// 表示判定)。
    pub fn has_open_stroke(&self) -> bool {
        self.stroke.is_some()
    }

    /// 履歴に 1 op を積む。redo は新規 push でクリアする
    /// (ARCHITECTURE.md §6)。メモリ上限を超えたら直近 `MIN_KEEP` 件を残して
    /// 最古から破棄する(SPEC §9)。
    pub fn push(&mut self, op: HistoryOp) {
        for removed in self.redo.drain(..) {
            self.bytes_used = self.bytes_used.saturating_sub(removed.byte_size());
        }
        self.bytes_used += op.byte_size();
        self.undo.push(op);

        while self.bytes_used > MEMORY_LIMIT_BYTES && self.undo.len() > MIN_KEEP {
            let removed = self.undo.remove(0);
            self.bytes_used = self.bytes_used.saturating_sub(removed.byte_size());
        }
    }

    /// 直近の op を取り消して `doc` に適用する。何も無ければ `false`。
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(op) = self.undo.pop() else {
            return false;
        };
        apply_before(doc, &op);
        self.redo.push(op);
        true
    }

    /// 直近に取り消した op をやり直す。何も無ければ `false`。
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(op) = self.redo.pop() else {
            return false;
        };
        apply_after(doc, &op);
        self.undo.push(op);
        true
    }

    /// メニューの「元に戻す」有効/無効表示(M4)や統合テストで使う。
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// メニューの「やり直し」有効/無効表示(M4)や統合テストで使う。
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// `a`/`b` が両方とも範囲内なら入れ替える(範囲外ならパニックせず何もしない、
/// CLAUDE.md 鉄則)。`HistoryOp::MoveLayer` は自身の逆操作が同じスワップに
/// なるため、undo/redo 双方から呼ぶ。
fn swap_layers(doc: &mut Document, a: usize, b: usize) {
    if a < doc.layers.len() && b < doc.layers.len() {
        doc.layers.swap(a, b);
    }
}

fn apply_before(doc: &mut Document, op: &HistoryOp) {
    match op {
        HistoryOp::Patch {
            layer,
            rect,
            before,
            ..
        } => {
            let (w, h) = (doc.width, doc.height);
            if let Some(l) = doc.layers.get_mut(*layer) {
                paste_region(&mut l.pixels, w, h, *rect, before);
            }
            doc.mark_dirty(*rect);
        }
        HistoryOp::AddLayer {
            index,
            before_active,
            ..
        }
        | HistoryOp::DuplicateLayer {
            index,
            before_active,
            ..
        } => {
            // 追加/複製の undo=挿入した位置を削除し、アクティブ添字を戻す。
            if *index < doc.layers.len() {
                doc.layers.remove(*index);
            }
            doc.active = *before_active;
            doc.mark_all_dirty();
        }
        HistoryOp::RemoveLayer {
            index,
            layer,
            before_active,
        } => {
            // 削除の undo=退避しておいたレイヤーを元の位置へ復元する。
            let idx = (*index).min(doc.layers.len());
            doc.layers.insert(idx, layer.clone());
            doc.active = *before_active;
            doc.mark_all_dirty();
        }
        HistoryOp::MoveLayer { from, to } => {
            swap_layers(doc, *from, *to);
            doc.active = *from;
            doc.mark_all_dirty();
        }
        HistoryOp::MergeDown {
            index,
            upper,
            lower_before,
        } => {
            // 結合の undo=結合前の 2 レイヤー(下は元の状態、上を再挿入)へ戻す。
            let lower_idx = index.saturating_sub(1);
            if lower_idx < doc.layers.len() {
                doc.layers[lower_idx] = lower_before.clone();
            }
            let insert_idx = (*index).min(doc.layers.len());
            doc.layers.insert(insert_idx, upper.clone());
            doc.active = *index;
            doc.mark_all_dirty();
        }
        HistoryOp::ReplaceAll { before, .. } => {
            doc.apply_snapshot(before);
        }
    }
    doc.modified = true;
}

fn apply_after(doc: &mut Document, op: &HistoryOp) {
    match op {
        HistoryOp::Patch {
            layer, rect, after, ..
        } => {
            let (w, h) = (doc.width, doc.height);
            if let Some(l) = doc.layers.get_mut(*layer) {
                paste_region(&mut l.pixels, w, h, *rect, after);
            }
            doc.mark_dirty(*rect);
        }
        HistoryOp::AddLayer { index, name, .. } => {
            let (width, height) = (doc.width, doc.height);
            let idx = (*index).min(doc.layers.len());
            doc.layers.insert(
                idx,
                Layer {
                    name: name.clone(),
                    visible: true,
                    opacity: 255,
                    pixels: vec![0u8; width as usize * height as usize * 4],
                },
            );
            doc.active = idx;
            doc.mark_all_dirty();
        }
        HistoryOp::DuplicateLayer { index, layer, .. } => {
            let idx = (*index).min(doc.layers.len());
            doc.layers.insert(idx, layer.clone());
            doc.active = idx;
            doc.mark_all_dirty();
        }
        HistoryOp::RemoveLayer { index, .. } => {
            if *index < doc.layers.len() {
                doc.layers.remove(*index);
            }
            doc.active = (*index).min(doc.layers.len().saturating_sub(1));
            doc.mark_all_dirty();
        }
        HistoryOp::MoveLayer { from, to } => {
            swap_layers(doc, *from, *to);
            doc.active = *to;
            doc.mark_all_dirty();
        }
        HistoryOp::MergeDown {
            index,
            upper,
            lower_before,
        } => {
            // 結合の redo=保持している 2 レイヤーから結合結果を再計算する
            // (結合済みの画素そのものは保持しない、メモリ節約)。
            let lower_idx = index.saturating_sub(1);
            let (width, height) = (doc.width, doc.height);
            if lower_idx < doc.layers.len() {
                let merged = composite_two(lower_before, upper, width, height);
                doc.layers[lower_idx] = Layer {
                    name: lower_before.name.clone(),
                    visible: true,
                    opacity: 255,
                    pixels: merged,
                };
            }
            if *index < doc.layers.len() {
                doc.layers.remove(*index);
            }
            doc.active = lower_idx.min(doc.layers.len().saturating_sub(1));
            doc.mark_all_dirty();
        }
        HistoryOp::ReplaceAll { after, .. } => {
            doc.apply_snapshot(after);
        }
    }
    doc.modified = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;
    use crate::raster::{self, Surface};

    /// テストから `ctx.doc.active_surface_mut()` 相当を作るヘルパー
    /// (raster 関数はいずれも `Surface` を要求するため)。
    fn surface(doc: &mut Document) -> Surface<'_> {
        doc.active_surface_mut()
    }

    #[test]
    fn stroke_undo_restores_exact_bytes() {
        let mut doc = Document::new(20, 20, Background::White);
        let original = doc.active_pixels().to_vec();

        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(
            &mut surface(&mut doc),
            10.0,
            10.0,
            4.0,
            [255, 0, 0, 255],
            false,
        );
        history.commit_stroke(&mut doc);

        assert_ne!(
            doc.active_pixels(),
            original.as_slice(),
            "stroke should have changed pixels"
        );
        assert!(history.can_undo());

        let undone = history.undo(&mut doc);
        assert!(undone);
        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "undo should restore exact bytes"
        );
    }

    #[test]
    fn stroke_redo_restores_after_state() {
        let mut doc = Document::new(20, 20, Background::White);

        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(
            &mut surface(&mut doc),
            10.0,
            10.0,
            4.0,
            [255, 0, 0, 255],
            false,
        );
        history.commit_stroke(&mut doc);
        let after_stroke = doc.active_pixels().to_vec();

        history.undo(&mut doc);
        assert!(history.can_redo());
        let redone = history.redo(&mut doc);
        assert!(redone);
        assert_eq!(
            doc.active_pixels(),
            after_stroke.as_slice(),
            "redo should restore after-state"
        );
    }

    #[test]
    fn multi_segment_stroke_spanning_multiple_tiles_round_trips() {
        // タイルサイズ(256)をまたぐストロークで、途中のタイルが未退避のまま
        // 復元されても正しく元に戻ることを確認する。
        let mut doc = Document::new(300, 300, Background::Transparent);
        let original = doc.active_pixels().to_vec();

        let mut history = History::new();
        history.begin_stroke(doc.active);

        let segments = [
            ((10.0, 10.0), (10.0, 10.0)),
            ((10.0, 10.0), (280.0, 10.0)),
            ((280.0, 10.0), (280.0, 280.0)),
        ];
        for (from, to) in segments {
            let bounds = raster::segment_bounds(from, to, 3.0);
            history.ensure_tiles_saved(&doc, bounds);
            raster::stroke_segment(&mut surface(&mut doc), from, to, 3.0, [1, 2, 3, 255], false);
        }
        history.commit_stroke(&mut doc);

        let after_stroke = doc.active_pixels().to_vec();
        assert_ne!(after_stroke, original);

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "undo must byte-exactly restore"
        );

        assert!(history.redo(&mut doc));
        assert_eq!(doc.active_pixels(), after_stroke.as_slice());
    }

    #[test]
    fn commit_without_any_touch_pushes_nothing() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);
        history.commit_stroke(&mut doc);
        assert!(!history.can_undo());
    }

    #[test]
    fn undo_redo_on_empty_history_returns_false() {
        let mut doc = Document::new(4, 4, Background::White);
        let mut history = History::new();
        assert!(!history.undo(&mut doc));
        assert!(!history.redo(&mut doc));
    }

    #[test]
    fn push_clears_redo_stack() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut doc = Document::new(4, 4, Background::White);
        let mut history = History::new();

        history.push(HistoryOp::Patch {
            layer: 0,
            rect,
            before: vec![0, 0, 0, 0],
            after: vec![1, 1, 1, 1],
        });
        history.undo(&mut doc);
        assert!(history.can_redo());

        history.push(HistoryOp::Patch {
            layer: 0,
            rect,
            before: vec![1, 1, 1, 1],
            after: vec![2, 2, 2, 2],
        });
        assert!(!history.can_redo(), "new push must clear redo stack");
    }

    #[test]
    fn memory_limit_discards_oldest_but_keeps_last_ten() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();

        // それぞれ大きめの op を積んで 256MB を大きく超えさせる。
        let big = vec![0u8; 30 * 1024 * 1024]; // 30MB
        for _ in 0..15 {
            history.push(HistoryOp::Patch {
                layer: 0,
                rect,
                before: big.clone(),
                after: vec![1, 1, 1, 1],
            });
        }

        // 直近 10 件は必ず残る。
        assert_eq!(history.undo.len(), MIN_KEEP);
        assert!(history.bytes_used <= MEMORY_LIMIT_BYTES.max(MIN_KEEP * (big.len() + 4)));
    }

    #[test]
    fn memory_limit_never_drops_below_min_keep_even_if_over_limit() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        // 直近10件だけで既に上限を超えるサイズにする。
        let huge = vec![0u8; 40 * 1024 * 1024]; // 40MB * 10 > 256MB
        for _ in 0..10 {
            history.push(HistoryOp::Patch {
                layer: 0,
                rect,
                before: huge.clone(),
                after: vec![1],
            });
        }
        assert_eq!(history.undo.len(), MIN_KEEP);
    }

    #[test]
    fn original_pixel_returns_pre_stroke_value_after_write() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        // タイル退避後にドキュメントを書き換えても、original_pixel は
        // 退避時点(書き換え前)の値を返し続ける。
        raster::stamp_round(&mut surface(&mut doc), 10.0, 10.0, 4.0, [1, 2, 3, 4], false);
        assert_eq!(history.original_pixel(10, 10), Some([255, 255, 255, 255]));
        assert_eq!(doc.get_pixel(10, 10), Some([1, 2, 3, 4]));
    }

    #[test]
    fn original_pixel_is_none_before_tile_saved_or_outside_stroke() {
        let mut history = History::new();
        // ストローク記録中でなければ None。
        assert_eq!(history.original_pixel(5, 5), None);

        history.begin_stroke(0);
        // まだそのタイルを退避していなければ None。
        assert_eq!(history.original_pixel(5, 5), None);
    }

    #[test]
    fn has_open_stroke_tracks_begin_and_commit() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        assert!(!history.has_open_stroke());

        history.begin_stroke(doc.active);
        assert!(history.has_open_stroke());

        history.commit_stroke(&mut doc);
        assert!(!history.has_open_stroke());
    }

    #[test]
    fn has_open_stroke_is_false_after_cancel() {
        let mut history = History::new();
        history.begin_stroke(0);
        assert!(history.has_open_stroke());
        history.cancel_stroke();
        assert!(!history.has_open_stroke());
    }

    #[test]
    fn commit_stroke_sets_modified_only_when_something_was_touched() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();

        history.begin_stroke(doc.active);
        history.commit_stroke(&mut doc);
        assert!(!doc.modified, "touching nothing must not set modified");

        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(5.0, 5.0, 2.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(&mut surface(&mut doc), 5.0, 5.0, 2.0, [1, 2, 3, 4], false);
        history.commit_stroke(&mut doc);
        assert!(doc.modified, "a real edit must set modified");
    }

    // -- v2: レイヤー対応(ARCHITECTURE.md §14.2) ---------------------------

    #[test]
    fn patch_records_and_restores_the_correct_layer() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.layers
            .push(crate::document::Layer::filled("上", 4, 4, [0, 0, 0, 0]));
        doc.active = 1;

        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(2.0, 2.0, 1.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(&mut surface(&mut doc), 2.0, 2.0, 1.0, [9, 9, 9, 255], false);
        history.commit_stroke(&mut doc);

        // 下層(背景)は触れられていないはず。
        doc.active = 0;
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));

        assert!(history.undo(&mut doc));
        doc.active = 1;
        assert_eq!(doc.get_pixel(2, 2), Some([0, 0, 0, 0]));
    }

    #[test]
    fn replace_all_round_trips_layer_structure() {
        let mut doc = Document::new(4, 4, Background::White);
        let before = doc.snapshot();

        doc.layers
            .push(crate::document::Layer::filled("上", 4, 4, [1, 2, 3, 4]));
        doc.active = 1;
        let after = doc.snapshot();

        let mut history = History::new();
        history.push(HistoryOp::ReplaceAll {
            before,
            after: after.clone(),
        });

        assert!(history.undo(&mut doc));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);

        assert!(history.redo(&mut doc));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[1].pixels[0..4], [1, 2, 3, 4]);
    }

    #[test]
    fn replace_all_resizes_composite_buffer() {
        let mut doc = Document::new(4, 4, Background::White);
        let before = doc.snapshot();
        doc.resize_canvas(8, 6);
        let after = doc.snapshot();

        let mut history = History::new();
        history.push(HistoryOp::ReplaceAll { before, after });

        assert!(history.undo(&mut doc));
        assert_eq!(doc.width, 4);
        assert_eq!(doc.composite.len(), 4 * 4 * 4);

        assert!(history.redo(&mut doc));
        assert_eq!(doc.width, 8);
        assert_eq!(doc.composite.len(), 8 * 6 * 4);
    }

    // -- v2 レビューで発見・修正したバグ: レイヤー構造操作が全て ReplaceAll
    // (全レイヤー×before/after の全画素スナップショット)だった
    // (ARCHITECTURE.md §14.2 の軽量 op: AddLayer/DuplicateLayer/RemoveLayer/
    // MoveLayer/MergeDown)。以下は各 op 単体の undo/redo が正しいことの
    // ユニットテスト(app.rs の layer_* メソッドは既存テストで機能的に
    // 検証済み)。--------------------------------------------------------

    #[test]
    fn add_layer_history_round_trips() {
        let mut doc = Document::new(2, 2, Background::White);
        let before_active = doc.active_index();
        assert!(doc.add_layer("レイヤー 1".to_owned()));
        let index = doc.active_index();

        let mut history = History::new();
        history.push(HistoryOp::AddLayer {
            index,
            name: "レイヤー 1".to_owned(),
            before_active,
        });

        assert_eq!(doc.layers.len(), 2);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, before_active);

        assert!(history.redo(&mut doc));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, index);
        assert_eq!(doc.layers[index].name, "レイヤー 1");
        assert!(doc.layers[index].pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn duplicate_layer_history_round_trips_pixels() {
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.set_pixel(0, 0, [7, 8, 9, 255]);
        let before_active = doc.active_index();
        assert!(doc.duplicate_active_layer());
        let index = doc.active_index();
        let layer = doc.layers[index].clone();

        let mut history = History::new();
        history.push(HistoryOp::DuplicateLayer {
            index,
            layer,
            before_active,
        });

        assert!(history.undo(&mut doc));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, before_active);

        assert!(history.redo(&mut doc));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, index);
        assert_eq!(doc.layers[index].pixels[0..4], [7, 8, 9, 255]);
    }

    #[test]
    fn remove_layer_history_round_trips_preserving_full_layer_state() {
        let mut doc = Document::new(2, 2, Background::White);
        let mut upper = crate::document::Layer::filled("上", 2, 2, [9, 9, 9, 200]);
        upper.opacity = 128;
        doc.layers.push(upper);
        doc.active = 1;

        let before_active = doc.active_index();
        let removed = doc.layers[1].clone();
        assert!(doc.remove_active_layer());

        let mut history = History::new();
        history.push(HistoryOp::RemoveLayer {
            index: 1,
            layer: removed,
            before_active,
        });

        assert_eq!(doc.layers.len(), 1);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, 1);
        assert_eq!(
            doc.layers[1].opacity, 128,
            "opacity must be restored exactly"
        );
        assert_eq!(doc.layers[1].pixels[0..4], [9, 9, 9, 200]);

        assert!(history.redo(&mut doc));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
    }

    #[test]
    fn move_layer_history_round_trips() {
        let mut doc = Document::new(1, 1, Background::White);
        doc.layers
            .push(crate::document::Layer::filled("上", 1, 1, [1, 1, 1, 1]));
        // move_active_layer_down(idx=1) が行うのと同じスワップを直接再現する。
        doc.layers.swap(0, 1);
        doc.active = 0;

        let mut history = History::new();
        history.push(HistoryOp::MoveLayer { from: 1, to: 0 });

        assert!(history.undo(&mut doc));
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[1].pixels, vec![1, 1, 1, 1]);
        assert_eq!(doc.layers[0].name, "背景");

        assert!(history.redo(&mut doc));
        assert_eq!(doc.active, 0);
        assert_eq!(doc.layers[0].pixels, vec![1, 1, 1, 1]);
    }

    #[test]
    fn merge_down_history_round_trip_restores_originals_and_redo_recomputes() {
        let mut doc = Document::new(1, 1, Background::Transparent);
        doc.layers[0] = crate::document::Layer::filled("下", 1, 1, [255, 255, 255, 255]);
        let mut upper = crate::document::Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        upper.opacity = 128; // 約50%黒
        doc.layers.push(upper.clone());
        doc.active = 1;

        let lower_before = doc.layers[0].clone();
        assert!(doc.merge_active_down());
        let merged_pixels = doc.layers[0].pixels.clone();
        assert_eq!(doc.layers.len(), 1);

        let mut history = History::new();
        history.push(HistoryOp::MergeDown {
            index: 1,
            upper,
            lower_before: lower_before.clone(),
        });

        assert!(history.undo(&mut doc));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[0].pixels, lower_before.pixels);
        assert_eq!(
            doc.layers[1].opacity, 128,
            "upper's opacity must be restored"
        );

        assert!(history.redo(&mut doc));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
        assert_eq!(
            doc.layers[0].pixels, merged_pixels,
            "redo must recompute the exact same merged pixels from the stored source layers"
        );
    }
}
