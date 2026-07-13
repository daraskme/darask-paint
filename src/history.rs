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

/// v6 §34/ARCHITECTURE.md §18.2・§18.3: 「元に戻す履歴の保持数」の既定値。
/// 設定ダイアログ(v6-M2)が `Settings::max_undo_steps` を注入するまでの
/// `History::new()` の既定値としてここで定義する(v6-M1 時点では設定
/// ダイアログ自体が無いため、`History` はこの既定のまま動く)。
const DEFAULT_MAX_STEPS: usize = 50;

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
    ///
    /// v3 レビューで発見・修正したバグ: 以前は `rect`(触れたタイル群の
    /// 外接矩形 1 個)/`before`/`after` の単一フィールドだった。離れた
    /// 2 領域を触る操作(例: 4000×4000 ドキュメントで 10×10 の選択範囲を
    /// 左上から右下へ移動)では、外接矩形がほぼ全面になり実際の変更画素数
    /// (約 200px)に対して before+after 合計 128MB 級の `Patch` が積まれ、
    /// 1 回の操作で 256MB 上限の半分を消費して既存の undo 履歴が大量破棄
    /// されうる問題があった。実際に触れたタイル(256×256、
    /// `StrokeRecorder::tiles`)ごとに `PatchRegion` を分けて持つことで、
    /// メモリ使用量を「触れたタイルの合計サイズ」に比例させる(復元の
    /// バイト正確性は不変、`history.rs` のテスト参照)。
    Patch {
        layer: usize,
        regions: Vec<PatchRegion>,
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

/// `HistoryOp::Patch` の 1 タイルぶんの部分領域(`HistoryOp::Patch` の
/// ドキュメントコメント参照)。`rect` は触れたタイルの矩形(256×256、画像
/// 境界でクランプ済み)、`before`/`after` はその矩形内のピクセル。
pub struct PatchRegion {
    rect: IRect,
    before: Vec<u8>,
    after: Vec<u8>,
}

impl HistoryOp {
    fn byte_size(&self) -> usize {
        match self {
            HistoryOp::Patch { regions, .. } => {
                regions.iter().map(|r| r.before.len() + r.after.len()).sum()
            }
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
}

impl StrokeRecorder {
    fn new(layer: usize) -> Self {
        Self {
            layer,
            tiles: HashMap::new(),
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
    ///
    /// `HistoryOp::Patch` のドキュメントコメント参照: 触れたタイル
    /// (`self.tiles`)ごとに個別の `PatchRegion` を作る(離れた 2 領域を
    /// 触る操作で外接矩形 1 個ぶんのメモリを浪費しないため)。
    ///
    /// ARCHITECTURE.md §15.2: 「空レイヤー(全透明)でも動作(確定時
    /// before==after 抑制が効く)」。移動ツール/自由変形/選択ドラッグで
    /// ドラッグしても実際には画素が 1 つも変わらなかった場合(全透明レイヤーの
    /// 移動、元の位置に戻して確定、等)、タイルごとに `before` と `after` を
    /// 比較し、バイト一致するタイルは region に含めない(そのタイルは真に
    /// 無変化なので、undo 側で貼り戻す必要がない)。全タイルが無変化なら
    /// `Patch` 自体を push しない(`doc.modified` も立てない)。
    fn finish(self, width: u32, pixels: &[u8]) -> Option<HistoryOp> {
        if self.tiles.is_empty() {
            return None;
        }
        let mut regions = Vec::with_capacity(self.tiles.len());
        for snap in self.tiles.into_values() {
            // `snap.rect` は退避時点(`ensure_saved`)で既に画像境界へ
            // クランプ済み。ストローク中にドキュメント寸法が変わらないこと
            // は v3 §15.6 落とし穴2 の不変条件(サイズ変更中はストロークが
            // 開いていない)で保証されるため、ここでの再クランプは不要
            // (むしろ `width`/`height` が万一食い違った場合に `snap.pixels`
            // の寸法と `rect` の寸法がずれて `copy_region`/`paste_region` が
            // 静かに破損したデータを作ってしまう方が危険)。
            let rect = snap.rect;
            if rect.is_empty() {
                continue;
            }
            let after = copy_region(pixels, width, rect);
            if after == snap.pixels {
                // このタイルは実質無変化(ARCHITECTURE.md §15.2)。
                continue;
            }
            regions.push(PatchRegion {
                rect,
                before: snap.pixels,
                after,
            });
        }
        if regions.is_empty() {
            return None;
        }
        Some(HistoryOp::Patch {
            layer: self.layer,
            regions,
        })
    }
}

/// `History::original_pixel_cursor` が返す、ホットループ向けのタイル探索
/// キャッシュ(v4-M2 性能改善、ARCHITECTURE.md §16.1)。
///
/// 背景: `tools/brush.rs::apply_stamp`(ブラシ/消しゴムの 1 スタンプの画素
/// ループ)や `tools/gradient.rs`・`app.rs::reapply_tone_preview`(色調補正の
/// ライブプレビュー)は、影響を受ける各画素ごとに「ストローク開始前の値」を
/// 求める必要があるが、そのたびに素朴な `original_pixel(x, y)` を呼ぶと
/// `stroke.tiles`(タイル 256×256 ごとの `HashMap`)を毎画素で引き直すことに
/// なる。1 スタンプの範囲は通常わずか数タイルしか跨がないため、実測
/// (release ビルド、4000×4000・64px ブラシの高速ドラッグ)でこの
/// `HashMap` 探索がホットパスの支配的コストになっていた(1400px 移動の
/// ドラッグで約 34ms、キャンバス対角線相当で約 123ms、SPEC §28 の
/// 「フレーム 16ms 以下」を大きく超えていた)。
///
/// このカーソルは「直前に解決したタイル座標 → そのタイルの
/// `TileSnapshot` への参照」を保持し、次のアクセスが同じタイル内であれば
/// `HashMap::get` を再度呼ばない(呼び出し側は 1 スタンプ/1 行の処理開始時に
/// 一度だけ `original_pixel_cursor()` を呼び、その画素ループ全体で使い回す)。
/// `History::original_pixel(x, y)`(旧 API、意味論はここに引き継いだ)と
/// 完全に同じ値を返す単なる呼び出し側キャッシュであり、ストローク非記録中や
/// まだ退避されていないタイルでは `None` を返す。
pub struct OriginalPixelCursor<'a> {
    stroke: Option<&'a StrokeRecorder>,
    cached: Option<((i32, i32), &'a TileSnapshot)>,
}

impl<'a> OriginalPixelCursor<'a> {
    /// `(x, y)` の「ストローク開始前」のピクセル値。直前の呼び出しと同じ
    /// タイル(256×256)内であれば `HashMap` を再探索しない。
    pub fn get(&mut self, x: i32, y: i32) -> Option<[u8; 4]> {
        let stroke = self.stroke?;
        let tx = x.div_euclid(TILE_SIZE);
        let ty = y.div_euclid(TILE_SIZE);
        let snap = match self.cached {
            Some((key, snap)) if key == (tx, ty) => snap,
            _ => {
                let snap = stroke.tiles.get(&(tx, ty))?;
                self.cached = Some(((tx, ty), snap));
                snap
            }
        };
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

/// v6 §33〜35(ARCHITECTURE.md §18.3): 履歴パネル表示用に、1 回の undo 単位
/// (`HistoryOp`)へ呼び出し元由来の短い日本語ラベル(例: 「ブラシ」「塗り
/// つぶし」「画像サイズ変更」)を添えたもの。`push`/`commit_stroke` を
/// 呼ぶすべての箇所がラベルを渡す(ARCHITECTURE.md §18.3 の対応表参照)。
/// ラベル自体は表示専用でメモリ会計(`HistoryOp::byte_size`)には含めない
/// (短い日本語文字列数バイト〜十数バイトであり、SPEC §9 の 256MB 上限は
/// 元々ピクセルデータを対象にした値のため)。
pub struct HistoryEntry {
    pub op: HistoryOp,
    /// v6-M3(ARCHITECTURE.md §18.4): `History::undo_labels`/
    /// `redo_labels_reversed` 経由で履歴パネル(`ui/history_panel.rs`)が
    /// 表示する日本語ラベル。
    pub label: String,
}

/// アンドゥ/リドゥスタックとメモリ会計、進行中ストロークの記録を持つ。
///
/// v6 §34〜35(ARCHITECTURE.md §18.2・§18.3)で「元に戻す履歴の保持数」
/// (`max_steps`)と、破棄の有無を示す `truncated` フラグを追加した:
/// - `max_steps`: 設定ダイアログ(v6-M2、未実装)から `set_max_steps` で
///   注入される上限件数(既定 `DEFAULT_MAX_STEPS` = 50)。
/// - `truncated`: 保持数キャップまたは SPEC §9 のバイト上限で最古のエントリ
///   を一度でも破棄したら `true` のまま(履歴パネル(v6-M3、未実装)の
///   「(これ以前の履歴は破棄されました)」注記の表示判定に使う)。
pub struct History {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    bytes_used: usize,
    /// SPEC §34: 「元に戻す履歴の保持数」(1–500、既定 50)。値そのものの
    /// 範囲クランプは設定側([1,500])が担うが、`History` 自身も 0 による
    /// 除算・無限ループ的な事故を避けるため `set_max_steps` で `1` 未満には
    /// ならないよう防御する(CLAUDE.md 鉄則: パニックしない)。
    max_steps: usize,
    /// 一度でも保持数/バイト上限で最古のエントリを破棄したら `true` の
    /// まま戻らない(ARCHITECTURE.md §18.3 最終項)。
    truncated: bool,
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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            bytes_used: 0,
            max_steps: DEFAULT_MAX_STEPS,
            truncated: false,
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

    /// ホットループ(1 スタンプ/1 行の画素ループ)向けの `original_pixel`
    /// アクセサを作る(`OriginalPixelCursor` のドキュメント参照、v4-M2、
    /// ARCHITECTURE.md §16.1)。意味論は素朴な「毎回 `(x, y)` を渡して
    /// `Option<[u8; 4]>` を得る」ままだが、同じタイル内の連続アクセスでは
    /// `HashMap` を再探索しない。
    pub fn original_pixel_cursor(&self) -> OriginalPixelCursor<'_> {
        OriginalPixelCursor {
            stroke: self.stroke.as_ref(),
            cached: None,
        }
    }

    /// SPEC §18: Esc キャンセル(`app.rs::cancel_floating`)専用。進行中の
    /// ストロークが対象とする `rect`(`ensure_tiles_saved`/
    /// `ensure_tiles_saved_buf` 済みの領域であること)を、ストローク開始前の
    /// 値へ復元して `doc` のアクティブレイヤーへ直接書き戻す。`commit_stroke`
    /// と違い `HistoryOp` は一切 push しない(履歴には何も積まない、
    /// SPEC §18)。ストローク自体もここでは消費しない(呼び出し側が続けて
    /// `cancel_stroke()` で破棄すること)。ストローク記録中でなければ
    /// 何もしない。
    ///
    /// v4 §16.1(SPEC §28: 「Esc キャンセル: 全レイヤー浮動化のキャンセル
    /// 復元をタイル一括コピーで 50ms 以下(4000×4000)」): 以前は `rect` 内の
    /// 各画素について `original_pixel(x, y)`(タイルの `HashMap` 探索 +
    /// 境界チェック)を呼んでから `doc.set_pixel` していた。移動ツールで
    /// アクティブレイヤー全体を浮動化してキャンセルすると `rect` が全面
    /// (4000×4000 なら 1600 万画素)になり得るため、退避済みのタイル
    /// (`self.stroke.tiles`)ごとに `rect` との交差を求め、交差した行を
    /// `copy_from_slice` の行スライスで一括コピーする形に書き直した。
    pub fn restore_stroke_region(&self, doc: &mut Document, rect: IRect) {
        let Some(stroke) = &self.stroke else {
            return;
        };
        let rect = rect.clamp_to(doc.width, doc.height);
        if rect.is_empty() {
            return;
        }
        let width = doc.width as usize;
        let Some(layer) = doc.layers.get_mut(stroke.layer) else {
            return;
        };
        for snap in stroke.tiles.values() {
            let tile_rect = snap.rect;
            let ix0 = tile_rect.x0.max(rect.x0);
            let iy0 = tile_rect.y0.max(rect.y0);
            let ix1 = tile_rect.x1.min(rect.x1);
            let iy1 = tile_rect.y1.min(rect.y1);
            if ix0 >= ix1 || iy0 >= iy1 {
                continue;
            }
            let tile_w = tile_rect.width() as usize;
            let row_len = (ix1 - ix0) as usize * 4;
            for y in iy0..iy1 {
                let tile_row_start =
                    ((y - tile_rect.y0) as usize * tile_w + (ix0 - tile_rect.x0) as usize) * 4;
                let doc_row_start = (y as usize * width + ix0 as usize) * 4;
                if let (Some(src), Some(dst)) = (
                    snap.pixels.get(tile_row_start..tile_row_start + row_len),
                    layer.pixels.get_mut(doc_row_start..doc_row_start + row_len),
                ) {
                    dst.copy_from_slice(src);
                }
            }
        }
        doc.mark_dirty(rect);
    }

    /// ストロークを確定し、触れた領域があれば 1 つの `Patch` として push する。
    ///
    /// `doc.modified` はここで実際に op が push されたときにだけ true にする
    /// (M4 で発見・修正したバグ: 以前は undo/redo の `apply_before`/
    /// `apply_after` でしか `modified` を立てておらず、通常の編集
    /// (ペン/消しゴム/図形/塗りつぶし/選択の確定など、すべてこの関数を
    /// 経由する)ではタイトルバーの `*` や未保存ガードが働かなかった)。
    ///
    /// v6 §33〜35(ARCHITECTURE.md §18.3): 呼び出し元由来の短い日本語ラベル
    /// (対応表参照)を 1 つ追加で受け取り、そのまま `push` へ渡す。
    pub fn commit_stroke(&mut self, doc: &mut Document, label: impl Into<String>) {
        if let Some(stroke) = self.stroke.take() {
            let layer_idx = stroke.layer;
            let Some(layer) = doc.layers.get(layer_idx) else {
                return;
            };
            let pixels = &layer.pixels;
            if let Some(op) = stroke.finish(doc.width, pixels) {
                doc.modified = true;
                self.push(op, label);
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
    /// (ARCHITECTURE.md §6)。メモリ上限(SPEC §9)・保持数上限(SPEC §34、
    /// `max_steps`)のいずれかを超えたら、`min(MIN_KEEP, max_steps)` 件を
    /// 残して最古から破棄する(ARCHITECTURE.md §18.3)。
    ///
    /// v6 §33〜35: `impl Into<String>` の日本語ラベルを 1 つ追加で受け取る
    /// (ARCHITECTURE.md §18.3 の対応表参照。すべての呼び出し元がラベルを
    /// 渡すようになった、機械的なシグネチャ変更)。
    pub fn push(&mut self, op: HistoryOp, label: impl Into<String>) {
        for removed in self.redo_stack.drain(..) {
            self.bytes_used = self.bytes_used.saturating_sub(removed.op.byte_size());
        }
        self.bytes_used += op.byte_size();
        self.undo_stack.push(HistoryEntry {
            op,
            label: label.into(),
        });
        self.trim_to_limits();
    }

    /// SPEC §34/ARCHITECTURE.md §18.3: 保持数キャップ(`max_steps`)と
    /// メモリ上限(SPEC §9、`MEMORY_LIMIT_BYTES`)のどちらかを超えている間、
    /// 最古のエントリから破棄する。下限は `min(MIN_KEEP, max_steps)` 件
    /// (v1 §9 の「直近10件は必ず保持」という floor を、`max_steps` 自体が
    /// それより小さく設定された場合でも矛盾させないための最小値クランプ、
    /// ARCHITECTURE.md §18.3)。破棄が実際に起きたら `truncated` を立てる
    /// (一度立ったら戻らない、履歴パネル(v6-M3)の注記表示用)。
    fn trim_to_limits(&mut self) {
        let min_keep = MIN_KEEP.min(self.max_steps);
        while self.undo_stack.len() > min_keep
            && (self.undo_stack.len() > self.max_steps || self.bytes_used > MEMORY_LIMIT_BYTES)
        {
            let removed = self.undo_stack.remove(0);
            self.bytes_used = self.bytes_used.saturating_sub(removed.op.byte_size());
            self.truncated = true;
        }
    }

    /// SPEC §34/ARCHITECTURE.md §18.2: 「元に戻す履歴の保持数」の変更を
    /// 即座に反映する。v6-M2 で配線済み: `app.rs::Tab::new`(新規タブに現在の
    /// 上限を適用)と `app.rs::apply_preferences`(設定ダイアログの OK 時に
    /// 開いている全タブへ適用)の 2 箇所から呼ばれる。`0` を渡されても
    /// 無限ループ/パニックしないよう最低 `1` にクランプする(CLAUDE.md 鉄則。
    /// 実際の UI 側の範囲クランプは `settings.rs` が `[1, 500]` で担う、
    /// 二重の防御)。
    pub fn set_max_steps(&mut self, max_steps: usize) {
        self.max_steps = max_steps.max(1);
        self.trim_to_limits();
    }

    /// 一度でも最古のエントリを破棄したことがあるか(ARCHITECTURE.md
    /// §18.3・§18.4: 履歴パネル(`ui/history_panel.rs`)の「(これ以前の履歴は
    /// 破棄されました)」注記の表示判定に使う)。
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// 履歴パネル(v6-M3、ARCHITECTURE.md §18.4)向け: 現在の `undo_stack` の
    /// 長さ。パネルの「現在位置」ハイライト判定と、`redo_stack` 側の行が
    /// クリックされたときの `jump_to` 目標長の計算基準に使う。
    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    /// 履歴パネル向け: `undo_stack` の各ラベルを先頭(最古)から順に返す
    /// イテレータ(ARCHITECTURE.md §18.4: 「`undo_stack` の各
    /// `HistoryEntry.label` を先頭(最古)から順に…表示」)。
    pub fn undo_labels(&self) -> impl ExactSizeIterator<Item = &str> {
        self.undo_stack.iter().map(|entry| entry.label.as_str())
    }

    /// 履歴パネル向け: `redo_stack` の各ラベルを「逆順」(直近の undo ほど
    /// 先)で返すイテレータ(ARCHITECTURE.md §18.4: 「続いて `redo_stack` の
    /// 各 `HistoryEntry.label` を逆順(直近の undo ほど上)に淡色で表示」)。
    /// `redo_stack` は undo するたびに末尾へ積むスタックなので、格納順を
    /// そのまま逆転させるとちょうど「時系列順に現在位置の直後から続く」
    /// 順序になる(`history.rs` のテスト参照)。
    pub fn redo_labels_reversed(&self) -> impl ExactSizeIterator<Item = &str> {
        self.redo_stack
            .iter()
            .rev()
            .map(|entry| entry.label.as_str())
    }

    /// 直近の op を取り消して `doc` に適用する。何も無ければ `false`。
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(entry) = self.undo_stack.pop() else {
            return false;
        };
        apply_before(doc, &entry.op);
        self.redo_stack.push(entry);
        true
    }

    /// 直近に取り消した op をやり直す。何も無ければ `false`。
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(entry) = self.redo_stack.pop() else {
            return false;
        };
        apply_after(doc, &entry.op);
        self.undo_stack.push(entry);
        true
    }

    /// メニューの「元に戻す」有効/無効表示(M4)や統合テストで使う。
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// メニューの「やり直し」有効/無効表示(M4)や統合テストで使う。
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// ARCHITECTURE.md §18.3・§18.4: 履歴パネル(`ui/history_panel.rs`)の
    /// クリックジャンプが使う薄いラッパー。「新しい仕組みを増やさない」
    /// 方針どおり、既存の単発 `undo`/`redo` を `target_len`(ジャンプ後に
    /// 望む `undo_stack.len()`)に達するまでループ呼び出しするだけ(最大
    /// `max_steps` 回程度、既定 50 なら軽量)。`target_len` が現在の
    /// undo+redo の合計件数を超えていても、`undo`/`redo` が `false` を
    /// 返した時点で安全に打ち切る(パニックしない、CLAUDE.md 鉄則)。
    /// 安全規則(進行中のストローク・浮動片は先に確定してから呼ぶこと)は
    /// 呼び出し元(`app.rs::jump_history_to`)が `commit_open_gesture()` を
    /// 経由して守る(ARCHITECTURE.md §18.6-1: ジャンプだけを特別扱いしない)。
    pub fn jump_to(&mut self, doc: &mut Document, target_len: usize) {
        while self.undo_stack.len() > target_len {
            if !self.undo(doc) {
                break;
            }
        }
        while self.undo_stack.len() < target_len {
            if !self.redo(doc) {
                break;
            }
        }
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
        HistoryOp::Patch { layer, regions } => {
            let (w, h) = (doc.width, doc.height);
            for region in regions {
                if let Some(l) = doc.layers.get_mut(*layer) {
                    paste_region(&mut l.pixels, w, h, region.rect, &region.before);
                }
                doc.mark_dirty(region.rect);
            }
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
        HistoryOp::Patch { layer, regions } => {
            let (w, h) = (doc.width, doc.height);
            for region in regions {
                if let Some(l) = doc.layers.get_mut(*layer) {
                    paste_region(&mut l.pixels, w, h, region.rect, &region.after);
                }
                doc.mark_dirty(region.rect);
            }
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

    /// テストから `ctx.doc.active_surface_mut(None)` 相当を作るヘルパー
    /// (raster 関数はいずれも `Surface` を要求するため)。
    fn surface(doc: &mut Document) -> Surface<'_> {
        doc.active_surface_mut(None)
    }

    /// テスト用: 単一領域だけの `Patch`(`HistoryOp::Patch` のリファクタ
    /// (history.rs:189 のレビューで発見・修正したバグ: 複数タイルを持てる
    /// `regions: Vec<PatchRegion>` になった)前と同じ形の `Patch` を、
    /// 既存テストの記述をあまり変えずに組み立てるためのヘルパー。
    fn single_region_patch(
        layer: usize,
        rect: IRect,
        before: Vec<u8>,
        after: Vec<u8>,
    ) -> HistoryOp {
        HistoryOp::Patch {
            layer,
            regions: vec![PatchRegion {
                rect,
                before,
                after,
            }],
        }
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
        history.commit_stroke(&mut doc, "テスト");

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
        history.commit_stroke(&mut doc, "テスト");
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
        history.commit_stroke(&mut doc, "テスト");

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
        history.commit_stroke(&mut doc, "テスト");
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

        history.push(
            single_region_patch(0, rect, vec![0, 0, 0, 0], vec![1, 1, 1, 1]),
            "テスト1",
        );
        history.undo(&mut doc);
        assert!(history.can_redo());

        history.push(
            single_region_patch(0, rect, vec![1, 1, 1, 1], vec![2, 2, 2, 2]),
            "テスト2",
        );
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
        for i in 0..15 {
            history.push(
                single_region_patch(0, rect, big.clone(), vec![1, 1, 1, 1]),
                format!("op{i}"),
            );
        }

        // 直近 10 件は必ず残る。
        assert_eq!(history.undo_stack.len(), MIN_KEEP);
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
        for i in 0..10 {
            history.push(
                single_region_patch(0, rect, huge.clone(), vec![1]),
                format!("op{i}"),
            );
        }
        assert_eq!(history.undo_stack.len(), MIN_KEEP);
    }

    #[test]
    fn original_pixel_returns_pre_stroke_value_after_write() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        // タイル退避後にドキュメントを書き換えても、original_pixel_cursor は
        // 退避時点(書き換え前)の値を返し続ける。
        raster::stamp_round(&mut surface(&mut doc), 10.0, 10.0, 4.0, [1, 2, 3, 4], false);
        assert_eq!(
            history.original_pixel_cursor().get(10, 10),
            Some([255, 255, 255, 255])
        );
        assert_eq!(doc.get_pixel(10, 10), Some([1, 2, 3, 4]));
    }

    #[test]
    fn original_pixel_is_none_before_tile_saved_or_outside_stroke() {
        let mut history = History::new();
        // ストローク記録中でなければ None。
        assert_eq!(history.original_pixel_cursor().get(5, 5), None);

        history.begin_stroke(0);
        // まだそのタイルを退避していなければ None。
        assert_eq!(history.original_pixel_cursor().get(5, 5), None);
    }

    // -- v4-M2: `OriginalPixelCursor`(タイル探索キャッシュ)の回帰テスト
    // (ARCHITECTURE.md §16.1)。1 スタンプの画素ループ全体で同じカーソルを
    // 使い回すため、タイル境界をまたいで行き来しても値を取り違えない
    // ことが特に重要(キャッシュした tile 座標の比較が壊れていた場合の
    // 回帰検知)。-------------------------------------------------------

    #[test]
    fn original_pixel_cursor_does_not_return_stale_tile_after_crossing_boundary() {
        // タイル(0,0)側の行を赤、タイル(1,0)側(x>=256)の行を青にしておき、
        // これを「ストローク開始前」の値として退避する。
        let mut doc = Document::new(300, 30, Background::Transparent);
        for x in 250..256 {
            doc.set_pixel(x, 15, [255, 0, 0, 255]);
        }
        for x in 256..262 {
            doc.set_pixel(x, 15, [0, 0, 255, 255]);
        }

        let mut history = History::new();
        history.begin_stroke(doc.active);
        let rect = IRect {
            x0: 250,
            y0: 10,
            x1: 262,
            y1: 20,
        };
        history.ensure_tiles_saved(&doc, rect);
        // 退避後に全画素を緑へ書き換える(退避値と現在値を区別できるように
        // しておく)。
        for x in 250..262 {
            doc.set_pixel(x, 15, [0, 255, 0, 255]);
        }

        let mut cursor = history.original_pixel_cursor();
        // タイル(0,0)側 → タイル(1,0)側 → タイル(0,0)側、と行き来する。
        assert_eq!(
            cursor.get(253, 15),
            Some([255, 0, 0, 255]),
            "tile(0,0) before crossing the boundary"
        );
        assert_eq!(
            cursor.get(258, 15),
            Some([0, 0, 255, 255]),
            "tile(1,0) after crossing the boundary"
        );
        assert_eq!(
            cursor.get(252, 15),
            Some([255, 0, 0, 255]),
            "back to tile(0,0) must not return the cached tile(1,0) snapshot"
        );
        assert_eq!(
            cursor.get(259, 15),
            Some([0, 0, 255, 255]),
            "back to tile(1,0) must not return the cached tile(0,0) snapshot"
        );
    }

    // -- v3 §18: Esc キャンセル(`restore_stroke_region`) ------------------

    #[test]
    fn restore_stroke_region_reverts_to_pre_stroke_bytes_without_pushing_history() {
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
        assert_ne!(doc.active_pixels(), original.as_slice());

        history.restore_stroke_region(&mut doc, bounds);
        history.cancel_stroke();

        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "restore must byte-exactly revert the touched region"
        );
        assert!(!history.can_undo(), "cancel must not push any undo entry");
        assert!(!history.has_open_stroke());
    }

    #[test]
    fn restore_stroke_region_is_a_no_op_without_an_open_stroke() {
        let mut doc = Document::new(10, 10, Background::White);
        let history = History::new();
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        };
        // ストロークが開いていない状態で呼んでもパニックせず、何も変えない。
        history.restore_stroke_region(&mut doc, rect);
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn has_open_stroke_tracks_begin_and_commit() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        assert!(!history.has_open_stroke());

        history.begin_stroke(doc.active);
        assert!(history.has_open_stroke());

        history.commit_stroke(&mut doc, "テスト");
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
        history.commit_stroke(&mut doc, "テスト");
        assert!(!doc.modified, "touching nothing must not set modified");

        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(5.0, 5.0, 2.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(&mut surface(&mut doc), 5.0, 5.0, 2.0, [1, 2, 3, 4], false);
        history.commit_stroke(&mut doc, "テスト");
        assert!(doc.modified, "a real edit must set modified");
    }

    // -- v3 レビューで発見・修正したバグ: ARCHITECTURE.md §15.2 の
    // 「確定時 before==after 抑制」が未実装だった(移動ツール/自由変形で
    // 全透明レイヤーを浮動化して無操作のまま確定すると、無意味な全面
    // Patch が積まれ既存の undo 履歴を大量に破棄しうる、
    // 消しゴム/ブラシでも実質変化なしのタッチで同じ問題が起きる)。
    // ---------------------------------------------------------------

    #[test]
    fn commit_stroke_suppresses_patch_when_before_equals_after() {
        // 全透明レイヤーに透明を書く(erase=true → [0,0,0,0])のは、
        // 移動ツール/自由変形が「触れたが実際には何も変えなかった」
        // ケース(空レイヤーの浮動片を確定する等)の代表例。
        let mut doc = Document::new(20, 20, Background::Transparent);
        let mut history = History::new();

        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(&mut surface(&mut doc), 10.0, 10.0, 4.0, [0, 0, 0, 0], true);
        history.commit_stroke(&mut doc, "テスト");

        assert!(
            !history.can_undo(),
            "a no-op stroke (before == after) must not push a history entry"
        );
        assert!(!doc.modified, "a no-op stroke must not set modified either");
    }

    #[test]
    fn commit_stroke_still_pushes_patch_when_pixels_actually_change() {
        // 抑制ロジックが実際の変更まで握りつぶさないことの対照テスト。
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
            [10, 20, 30, 255],
            false,
        );
        history.commit_stroke(&mut doc, "テスト");

        assert!(history.can_undo(), "a real edit must still push a patch");
        assert!(doc.modified);
    }

    #[test]
    fn drag_back_to_original_position_and_commit_is_suppressed() {
        // ARCHITECTURE.md §15.2 の具体シナリオ: 選択/浮動片をドラッグして
        // 元の位置へ戻して確定すると、切り出し元の透明化+貼り戻しで
        // バイト単位では完全に元通りになる。1 ストロークとして扱うと
        // before==after になり、抑制されるべき。
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();

        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        // 一旦別の色に変え、同じストローク内で元の白へ書き戻す
        // (浮動片を動かしてから同じ場所へ戻す操作の簡略版)。
        raster::stamp_round(&mut surface(&mut doc), 10.0, 10.0, 4.0, [1, 2, 3, 4], false);
        raster::stamp_round(
            &mut surface(&mut doc),
            10.0,
            10.0,
            4.0,
            [255, 255, 255, 255],
            false,
        );
        history.commit_stroke(&mut doc, "テスト");

        assert!(
            !history.can_undo(),
            "restoring the exact original bytes within one stroke must not push a patch"
        );
        assert!(!doc.modified);
    }

    // -- v3 レビューで発見・修正したバグ: `Patch` が触れたタイル群の外接
    // 矩形 1 個(union)だけを保持していたため、離れた 2 領域を触る操作
    // (選択範囲を遠く離れた位置へ移動、等)で実際の変更画素数に対しメモリが
    // 面積比で爆発していた。タイル単位で複数の `PatchRegion` に分けることで
    // 解消した(`HistoryOp::Patch` のドキュメントコメント参照)。--------------

    #[test]
    fn patch_memory_scales_with_touched_tiles_not_the_bounding_box_union() {
        // 4000×4000 ドキュメントで、左上と右下の離れた 10×10 領域だけを
        // 触る(選択範囲を対角線上の反対側へ移動する操作の簡略版)。
        let mut doc = Document::new(4000, 4000, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);

        let top_left = raster::stamp_bounds(5.0, 5.0, 4.0);
        let bottom_right = raster::stamp_bounds(3995.0, 3995.0, 4.0);
        history.ensure_tiles_saved(&doc, top_left);
        history.ensure_tiles_saved(&doc, bottom_right);
        raster::stamp_round(&mut surface(&mut doc), 5.0, 5.0, 4.0, [1, 2, 3, 4], false);
        raster::stamp_round(
            &mut surface(&mut doc),
            3995.0,
            3995.0,
            4.0,
            [5, 6, 7, 8],
            false,
        );
        history.commit_stroke(&mut doc, "テスト");

        assert!(history.can_undo(), "a real edit must still push a patch");

        // 修正前(外接矩形 1 個)なら before+after だけで
        // 4000*4000*4*2 = 128,000,000 バイト(約122MiB)。修正後は触れた
        // タイル(高々 2×2 個 = 4 タイル、256×256 以下)ぶんだけなので
        // 数百KB程度に収まるはず。1MiB を大きく下回ることを確認する
        // (退行検知の閾値、正確な値は実装の内部詳細に依存させない)。
        const OLD_BBOX_UNION_BYTES: usize = 4000 * 4000 * 4 * 2;
        assert!(
            history.bytes_used < 1024 * 1024,
            "expected tile-granular memory (~a few hundred KB), got {} bytes \
             (old bounding-box-union behavior would have used ~{} bytes)",
            history.bytes_used,
            OLD_BBOX_UNION_BYTES
        );

        // バイト正確性(復元)も両領域とも壊れていないことを確認する。
        assert_eq!(doc.get_pixel(5, 5), Some([1, 2, 3, 4]));
        assert_eq!(doc.get_pixel(3995, 3995), Some([5, 6, 7, 8]));
        // 2 領域の間(未退避のタイル)は無変化のはず。
        assert_eq!(doc.get_pixel(2000, 2000), Some([255, 255, 255, 255]));

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.get_pixel(5, 5),
            Some([255, 255, 255, 255]),
            "undo must byte-exactly restore the top-left region"
        );
        assert_eq!(
            doc.get_pixel(3995, 3995),
            Some([255, 255, 255, 255]),
            "undo must byte-exactly restore the bottom-right region"
        );

        assert!(history.redo(&mut doc));
        assert_eq!(doc.get_pixel(5, 5), Some([1, 2, 3, 4]));
        assert_eq!(doc.get_pixel(3995, 3995), Some([5, 6, 7, 8]));
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
        history.commit_stroke(&mut doc, "テスト");

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
        history.push(
            HistoryOp::ReplaceAll {
                before,
                after: after.clone(),
            },
            "テスト",
        );

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
        history.push(HistoryOp::ReplaceAll { before, after }, "テスト");

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
        history.push(
            HistoryOp::AddLayer {
                index,
                name: "レイヤー 1".to_owned(),
                before_active,
            },
            "レイヤーを追加",
        );

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
        history.push(
            HistoryOp::DuplicateLayer {
                index,
                layer,
                before_active,
            },
            "レイヤーを複製",
        );

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
        history.push(
            HistoryOp::RemoveLayer {
                index: 1,
                layer: removed,
                before_active,
            },
            "レイヤーを削除",
        );

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
        history.push(
            HistoryOp::MoveLayer { from: 1, to: 0 },
            "レイヤーの並び替え",
        );

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
        history.push(
            HistoryOp::MergeDown {
                index: 1,
                upper,
                lower_before: lower_before.clone(),
            },
            "レイヤーの結合",
        );

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

    // -- v4 §16.1/SPEC §28: Esc キャンセルのタイル一括コピー -------------------

    #[test]
    fn restore_stroke_region_whole_layer_4000x4000_is_correct_and_terminates_quickly() {
        // ARCHITECTURE.md §16.1・SPEC §28: 「Esc キャンセル: 全レイヤー浮動化
        // のキャンセル復元をタイル一括コピーで 50ms 以下(4000×4000)」。
        // 移動ツールでアクティブレイヤー全体を浮動化してから Esc でキャンセル
        // する経路(`app.rs::cancel_floating` → `restore_stroke_region`)を
        // 模した回帰テスト。`cargo test` はデバッグビルドのため実際の 50ms
        // 目標そのものではなく、緩い上限で O(n^2) 的な劣化がないことだけを
        // 保証する(実測のリリースビルド計測は別途行う、raster.rs の
        // flood_fill 4000x4000 テストと同じ方針)。
        let mut doc = Document::new(4000, 4000, Background::White);
        let original = doc.active_pixels().to_vec();

        let full_rect = IRect {
            x0: 0,
            y0: 0,
            x1: 4000,
            y1: 4000,
        };

        let mut history = History::new();
        history.begin_stroke(doc.active);
        // 移動ツールの浮動化は「全画素を退避してから透明化する」ので、
        // ここでも全面を退避してから全面を書き換える(タイル退避の範囲を
        // 現実のシナリオに揃えるため)。
        history.ensure_tiles_saved(&doc, full_rect);
        {
            let surface = doc.active_surface_mut(None);
            surface.pixels.fill(0);
        }
        assert_ne!(doc.active_pixels(), original.as_slice());

        let start = std::time::Instant::now();
        history.restore_stroke_region(&mut doc, full_rect);
        let elapsed = start.elapsed();
        history.cancel_stroke();

        assert!(
            elapsed.as_secs() < 10,
            "restore_stroke_region took suspiciously long (possible regression): {elapsed:?}"
        );
        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "restore must byte-exactly revert the whole layer"
        );
        assert!(!history.can_undo(), "cancel must not push any undo entry");
    }

    // -- v6 §33〜35(ARCHITECTURE.md §18.3): ラベル付け・保持数キャップ・
    // ジャンプ ------------------------------------------------------------

    #[test]
    fn push_records_the_given_label() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        history.push(
            single_region_patch(0, rect, vec![0, 0, 0, 0], vec![1, 1, 1, 1]),
            "レイヤーを追加",
        );
        assert_eq!(history.undo_stack.last().unwrap().label, "レイヤーを追加");
    }

    #[test]
    fn commit_stroke_records_the_given_label() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        history.begin_stroke(doc.active);
        let bounds = raster::stamp_bounds(10.0, 10.0, 4.0);
        history.ensure_tiles_saved(&doc, bounds);
        raster::stamp_round(&mut surface(&mut doc), 10.0, 10.0, 4.0, [1, 2, 3, 4], false);
        history.commit_stroke(&mut doc, "ブラシ");
        assert_eq!(history.undo_stack.last().unwrap().label, "ブラシ");
    }

    #[test]
    fn default_max_steps_does_not_evict_below_the_documented_default() {
        // SPEC §34: 「デフォルト 50」。既定のまま 50 件までは(バイト上限に
        // 触れない限り)何も破棄されないはず。
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        for i in 0..DEFAULT_MAX_STEPS {
            history.push(
                single_region_patch(0, rect, vec![0], vec![1]),
                format!("op{i}"),
            );
        }
        assert_eq!(history.undo_stack.len(), DEFAULT_MAX_STEPS);
        assert!(!history.is_truncated());
    }

    #[test]
    fn set_max_steps_evicts_down_to_the_new_cap_immediately_keeping_the_most_recent() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        for i in 0..8 {
            history.push(
                single_region_patch(0, rect, vec![0], vec![1]),
                format!("op{i}"),
            );
        }
        assert_eq!(history.undo_stack.len(), 8);

        history.set_max_steps(3);
        assert_eq!(
            history.undo_stack.len(),
            3,
            "lowering max_steps must immediately truncate to the new cap"
        );
        assert_eq!(history.undo_stack.first().unwrap().label, "op5");
        assert_eq!(history.undo_stack.last().unwrap().label, "op7");
        assert!(history.is_truncated());
    }

    #[test]
    fn push_after_lowering_max_steps_keeps_enforcing_the_new_cap() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        history.set_max_steps(2);
        for i in 0..5 {
            history.push(
                single_region_patch(0, rect, vec![0], vec![1]),
                format!("op{i}"),
            );
        }
        assert_eq!(history.undo_stack.len(), 2);
        assert_eq!(history.undo_stack.last().unwrap().label, "op4");
        assert_eq!(history.undo_stack.first().unwrap().label, "op3");
    }

    #[test]
    fn set_max_steps_clamps_zero_to_one_instead_of_panicking() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        history.push(single_region_patch(0, rect, vec![0], vec![1]), "a");
        history.push(single_region_patch(0, rect, vec![0], vec![1]), "b");
        // CLAUDE.md 鉄則: パニックしない。0 は 1 にクランプされる
        // (`settings.rs` の [1,500] クランプと二重の防御、ARCHITECTURE.md
        // §18.3)。
        history.set_max_steps(0);
        assert_eq!(history.undo_stack.len(), 1);
        assert_eq!(history.undo_stack.last().unwrap().label, "b");
    }

    #[test]
    fn min_keep_floor_is_the_smaller_of_ten_and_max_steps() {
        // ARCHITECTURE.md §18.3: 「下限は min(MIN_KEEP, max_steps) 件を必ず
        // 保持」。max_steps を MIN_KEEP(10)より大きく下げても(ここでは 20)、
        // 通常の保持数キャップがそのまま効く(floor は 10 のまま)ことを
        // 確認する対照テスト。
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        history.set_max_steps(20);
        for i in 0..25 {
            history.push(
                single_region_patch(0, rect, vec![0], vec![1]),
                format!("op{i}"),
            );
        }
        assert_eq!(
            history.undo_stack.len(),
            20,
            "capped at max_steps, not at MIN_KEEP"
        );
    }

    #[test]
    fn truncated_flag_starts_false_and_becomes_true_after_any_eviction_and_stays_true() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        assert!(!history.is_truncated());

        history.set_max_steps(2);
        history.push(single_region_patch(0, rect, vec![0], vec![1]), "a");
        assert!(
            !history.is_truncated(),
            "no eviction yet with only one entry"
        );
        history.push(single_region_patch(0, rect, vec![0], vec![1]), "b");
        assert!(
            !history.is_truncated(),
            "exactly at the cap, nothing evicted yet"
        );

        history.push(single_region_patch(0, rect, vec![0], vec![1]), "c");
        assert!(
            history.is_truncated(),
            "pushing past the cap must evict the oldest entry and set the flag"
        );

        // undo/redo は「破棄されたことがある」という事実を消さない。
        let mut doc = Document::new(4, 4, Background::White);
        history.undo(&mut doc);
        assert!(history.is_truncated());
        history.redo(&mut doc);
        assert!(history.is_truncated());
    }

    #[test]
    fn jump_to_moves_across_multiple_undo_redo_steps_reusing_undo_and_redo() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let original = doc.active_pixels().to_vec();

        // 3 個の独立したストローク(別々の位置)を積む。
        for (i, (cx, cy)) in [(3.0, 3.0), (10.0, 10.0), (16.0, 16.0)]
            .into_iter()
            .enumerate()
        {
            history.begin_stroke(doc.active);
            let bounds = raster::stamp_bounds(cx, cy, 2.0);
            history.ensure_tiles_saved(&doc, bounds);
            raster::stamp_round(
                &mut surface(&mut doc),
                cx,
                cy,
                2.0,
                [10, 20, 30, 255],
                false,
            );
            history.commit_stroke(&mut doc, format!("stroke{i}"));
        }
        let after_all = doc.active_pixels().to_vec();
        assert_eq!(history.undo_stack.len(), 3);

        // target_len=1: 1 個目のストロークだけが適用された状態まで戻る。
        history.jump_to(&mut doc, 1);
        assert_eq!(history.undo_stack.len(), 1);
        assert_eq!(history.redo_stack.len(), 2);
        assert_ne!(
            doc.get_pixel(3, 3),
            Some([255, 255, 255, 255]),
            "the first stroke must still be applied"
        );
        assert_eq!(
            doc.get_pixel(10, 10),
            Some([255, 255, 255, 255]),
            "the second stroke must not be applied yet at target_len=1"
        );

        // target_len=3: 最新状態まで一気に進む。
        history.jump_to(&mut doc, 3);
        assert_eq!(doc.active_pixels(), after_all.as_slice());
        assert_eq!(history.undo_stack.len(), 3);
        assert!(history.redo_stack.is_empty());

        // target_len=0: 完全に未編集の状態まで一気に戻る。
        history.jump_to(&mut doc, 0);
        assert_eq!(doc.active_pixels(), original.as_slice());
        assert_eq!(history.undo_stack.len(), 0);

        // 範囲外の target_len を渡してもパニックせず、可能な限りで打ち切る
        // (CLAUDE.md 鉄則)。
        history.jump_to(&mut doc, 999);
        assert_eq!(history.undo_stack.len(), 3);
        assert_eq!(doc.active_pixels(), after_all.as_slice());
    }

    #[test]
    fn jump_to_is_a_no_op_when_already_at_the_target_length() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut doc = Document::new(4, 4, Background::White);
        let mut history = History::new();
        history.push(
            single_region_patch(0, rect, vec![255, 255, 255, 255], vec![1, 1, 1, 1]),
            "a",
        );
        history.jump_to(&mut doc, 1);
        assert_eq!(history.undo_stack.len(), 1);
        assert!(history.redo_stack.is_empty());
    }

    // -- v6-M3(ARCHITECTURE.md §18.4): 履歴パネル向けアクセサ ---------------

    #[test]
    fn undo_labels_returns_oldest_to_newest() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut history = History::new();
        for label in ["ブラシ", "塗りつぶし", "矩形"] {
            history.push(single_region_patch(0, rect, vec![0], vec![1]), label);
        }
        assert_eq!(history.undo_len(), 3);
        let labels: Vec<&str> = history.undo_labels().collect();
        assert_eq!(labels, ["ブラシ", "塗りつぶし", "矩形"]);
    }

    #[test]
    fn redo_labels_reversed_continues_chronologically_after_the_current_position() {
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let mut doc = Document::new(4, 4, Background::White);
        let mut history = History::new();
        for label in ["op0", "op1", "op2"] {
            history.push(single_region_patch(0, rect, vec![0], vec![1]), label);
        }
        // 2 回 undo する: 最初の undo は "op2" を、2 回目は "op1" を
        // redo_stack へ積む(格納順は [op2, op1])。
        history.undo(&mut doc);
        history.undo(&mut doc);
        assert_eq!(history.undo_len(), 1);

        // 逆順にすると、時系列どおり「現在位置(op0)の直後」から続く
        // [op1, op2] になる(op1 が直近の undo、op2 がその前の undo)。
        let reversed: Vec<&str> = history.redo_labels_reversed().collect();
        assert_eq!(reversed, ["op1", "op2"]);
    }

    #[test]
    fn undo_labels_and_redo_labels_reversed_are_empty_on_fresh_history() {
        let history = History::new();
        assert_eq!(history.undo_len(), 0);
        assert_eq!(history.undo_labels().count(), 0);
        assert_eq!(history.redo_labels_reversed().count(), 0);
    }
}
