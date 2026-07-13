//! ブラシ/消しゴム共通のストロークエンジン(SPEC §17, ARCHITECTURE.md §15.1)。
//!
//! v2 までの AA ペン(カバレッジマスク + CoW タイル元画像)方式を、硬さ・
//! 不透明度・鉛筆モード・消しゴム強さ・Shift+クリック連結を持つ全ストローク
//! へ一般化したもの。`tools/pen.rs`(ブラシ)と `tools/eraser.rs`(消しゴム)
//! はこの `BrushEngine` を薄くラップするだけで、実際のスタンプ/合成ロジック
//! はここに集約する。
//!
//! 毎スタンプ、カバレッジマスク(ドキュメント全体、遅延確保・ストローク間で
//! 再利用)に `max` で書き込み、影響を受けた画素を「ストローク開始前の
//! ピクセル」(history.rs の CoW タイル)から都度合成し直す
//! (ARCHITECTURE.md §15.1)。これにより「1 ストローク内で何度重ねてもその
//! ストロークの被覆は不透明度上限を超えない」(PS opacity 意味論)が
//! mask の max 合成から自然に出る(ARCHITECTURE.md §15.6 落とし穴7:
//! 「直接レイヤーに blend を重ねると 1 ストローク内で濃度が累積してしまう」
//! を避けるため、必ず base からの再合成にする)。

use eframe::egui::PointerButton;

use super::{color_bytes_for, ToolCtx, ToolEvent};
use crate::document::IRect;
use crate::raster;

/// 1 スタンプの見た目を決めるパラメータ(SPEC §17)。
#[derive(Clone, Copy)]
pub struct BrushParams {
    pub radius: f32,
    /// 0.0–1.0(SPEC §17: 硬さ 0–100%)。鉛筆モードでは無視される。
    pub hardness: f32,
    /// 0.0–1.0(SPEC §17: 不透明度 1–100%。消しゴムは「強さ」として使う)。
    pub opacity: f32,
    /// SPEC §17: 鉛筆モード(2値スタンプ、硬さ無視)。
    pub pencil: bool,
    /// `true` なら消しゴム(カバレッジ×強さぶんアルファを減らす)、
    /// `false` ならブラシ(カバレッジ×不透明度で色を合成)。
    pub erase: bool,
    /// SPEC §25: スムージング(手ブレ補正)。0.0–1.0(UI は 0–100%、
    /// 0=補正なし)。Shift+クリック直線連結には適用しない
    /// (`handle` の `Down` 分岐参照)。
    pub smoothing: f32,
}

/// SPEC §25: スムージングの指数移動平均に使う重み `α`
/// (ARCHITECTURE.md §16.6: 「α = 1.0 - strength*0.9」)。`strength` は
/// 0.0–1.0(0=補正なし→α=1.0 で `raw` に即座に一致、1=最も強い補正→
/// α=0.1)。純関数なのでテスト可能。
pub fn smoothing_alpha(strength: f32) -> f32 {
    1.0 - strength.clamp(0.0, 1.0) * 0.9
}

/// 1 フレームぶんのスムージング(指数移動平均、ARCHITECTURE.md §16.6)。
/// `prev` は直前の平滑化済み位置、`raw` は今回受け取った生の入力位置。
pub fn smooth_towards(prev: (f32, f32), raw: (f32, f32), strength: f32) -> (f32, f32) {
    let alpha = smoothing_alpha(strength);
    (
        prev.0 + (raw.0 - prev.0) * alpha,
        prev.1 + (raw.1 - prev.1) * alpha,
    )
}

/// 進行中のストロークの状態。
struct ActiveStroke {
    button: PointerButton,
    /// 直近に受け取った生の入力位置(SPEC §25: Up 時の「最終点まで追いつく」
    /// 処理や、確定後の Shift+クリック連結の起点に使う)。
    last: (f32, f32),
    /// SPEC §25: スムージング後、実際にスタンプした最新位置。`smoothing`
    /// が 0 なら常に `last` と一致する。
    smoothed: (f32, f32),
    color: [u8; 4],
    /// これまでにこのストロークで触れた領域の外接矩形(ストローク終了時に
    /// マスクをクリアする範囲として使う)。
    touched: Option<IRect>,
}

/// ブラシ/消しゴム 1 ツールぶんの進行中ストローク状態(`tools/pen.rs`/
/// `tools/eraser.rs` がそれぞれ 1 つずつ所有する)。
pub struct BrushEngine {
    /// カバレッジマスク(ドキュメント全体、遅延確保・再利用)。ストロークが
    /// 進行していない間は全画素 0 に保たれる不変条件を守る(`finish` が
    /// ストロークの触れた範囲だけをクリアする)。
    mask: Vec<u8>,
    mask_size: (u32, u32),
    stroke: Option<ActiveStroke>,
    /// SPEC §17「Shift+クリック」連結用: 直近ストロークの終点(画像座標)。
    /// ツールごとに独立して保持する(ARCHITECTURE.md §15.1)。
    last_end: Option<(f32, f32)>,
}

impl Default for BrushEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BrushEngine {
    pub fn new() -> Self {
        Self {
            mask: Vec::new(),
            mask_size: (0, 0),
            stroke: None,
            last_end: None,
        }
    }

    /// SPEC §17「Shift+クリック」連結の終点をリセットする。ドキュメントの
    /// 差し替え(新規作成/開く/白紙貼り付け置換)で呼ぶこと — さもないと
    /// 前のドキュメントの画像座標が `last_end` に残り続け、まだ一度も
    /// 描いていない新ドキュメントで Shift+クリックすると、存在しないはずの
    /// 「直前のストローク」の終点(旧ドキュメント上の座標)から新キャンバス
    /// を横切る直線が引かれてしまう(v3 レビューで発見・修正したバグ)。
    /// 呼び出し側はドキュメント差し替え前に進行中のジェスチャを確定済み
    /// のはずだが、防御的に進行中ストローク・マスクも合わせて破棄する。
    pub fn reset_for_new_document(&mut self) {
        self.last_end = None;
        self.stroke = None;
        self.mask = Vec::new();
        self.mask_size = (0, 0);
    }

    fn ensure_mask_capacity(&mut self, doc_w: u32, doc_h: u32) {
        let size = (doc_w, doc_h);
        if self.mask_size != size {
            self.mask = vec![0u8; doc_w as usize * doc_h as usize];
            self.mask_size = size;
        }
    }

    /// ストロークが触れた矩形ぶんだけマスクを 0 に戻す(次のストロークに
    /// 備えて「アイドル時は全画素 0」の不変条件を保つ、ARCHITECTURE.md
    /// §15.6 落とし穴2: サイズ変更時は `mask_size` の不一致で丸ごと再確保
    /// されるので、ここでは通常のクリアだけを行えばよい)。
    fn clear_mask_region(&mut self, rect: IRect) {
        let rect = rect.clamp_to(self.mask_size.0, self.mask_size.1);
        if rect.is_empty() {
            return;
        }
        let w = self.mask_size.0 as usize;
        for y in rect.y0..rect.y1 {
            let start = y as usize * w + rect.x0 as usize;
            let end = start + rect.width() as usize;
            if let Some(slice) = self.mask.get_mut(start..end) {
                slice.fill(0);
            }
        }
    }

    /// 1 スタンプぶんのカバレッジをマスクに `max` で書き込み、影響を受けた
    /// 画素を「ストローク開始前のピクセル」から都度合成し直す
    /// (ARCHITECTURE.md §15.1 の擬似コード)。
    fn apply_stamp(
        &mut self,
        ctx: &mut ToolCtx,
        cx: f32,
        cy: f32,
        params: &BrushParams,
        color: [u8; 4],
        touched: &mut Option<IRect>,
    ) {
        self.ensure_mask_capacity(ctx.doc.width, ctx.doc.height);
        let bounds =
            raster::stamp_bounds(cx, cy, params.radius).clamp_to(ctx.doc.width, ctx.doc.height);
        if bounds.is_empty() {
            return;
        }
        ctx.history.ensure_tiles_saved(ctx.doc, bounds);

        let mask_w = ctx.doc.width as usize;
        // `ctx.doc`(Surface 経由でアクティブレイヤーのバッファを借用)と
        // `ctx.history`(タイル退避の読み取り)は ToolCtx の別フィールドなので
        // 同時に借用してよい(v2 の AA ペン実装と同じ分割借用のイディオム)。
        //
        // v4-M2 性能改善(ARCHITECTURE.md §16.1): 1 スタンプの画素ループ
        // 全体で `OriginalPixelCursor` を 1 個だけ作って使い回す(画素ごとに
        // `original_pixel` を呼ぶと `stroke.tiles` の `HashMap` を毎回
        // 引き直すことになり、64px ブラシの高速ドラッグでホットパスの
        // 支配的コストになっていた。`OriginalPixelCursor` のドキュメント
        // 参照)。
        let mut original_cursor = ctx.history.original_pixel_cursor();
        let mut surface = ctx.doc.active_surface_mut(ctx.clip);
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                let coverage_here = if params.pencil {
                    raster::stamp_pencil_coverage(cx, cy, params.radius, x, y)
                } else {
                    raster::stamp_soft_coverage(cx, cy, params.radius, params.hardness, x, y)
                };
                let idx = y as usize * mask_w + x as usize;
                let Some(slot) = self.mask.get_mut(idx) else {
                    continue;
                };
                if coverage_here > *slot {
                    *slot = coverage_here;
                }
                let coverage = *slot;
                let original = original_cursor
                    .get(x, y)
                    .or_else(|| surface.get_pixel(x, y))
                    .unwrap_or([0, 0, 0, 0]);
                if params.erase {
                    // SPEC §17: 「カバレッジ×強さぶんアルファを減らす」。
                    // RGB は保持し、アルファだけをストローク開始前の値から
                    // 減衰させる(元に戻せる CoW の前提を崩さない)。
                    let reduce = (coverage as f32 / 255.0) * params.opacity;
                    let new_alpha = (original[3] as f32 * (1.0 - reduce))
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    surface.set_pixel(x, y, [original[0], original[1], original[2], new_alpha]);
                } else {
                    let alpha = (color[3] as f32 * (coverage as f32 / 255.0) * params.opacity)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let blended =
                        raster::blend_over(original, [color[0], color[1], color[2], alpha]);
                    surface.set_pixel(x, y, blended);
                }
            }
        }
        ctx.doc.mark_dirty(bounds);
        *touched = Some(match touched {
            Some(t) => t.union(&bounds),
            None => bounds,
        });
    }

    /// スタンプ間隔ポリシー(ARCHITECTURE.md §15.1: 「ソフト時 ≤ max(1px,
    /// r/4)」。鉛筆(2値)は従来どおり r/2 で隙間が出ないことを確認済み
    /// (raster.rs の `stroke_segment` と同じ間隔)。
    fn apply_segment(
        &mut self,
        ctx: &mut ToolCtx,
        from: (f32, f32),
        to: (f32, f32),
        params: &BrushParams,
        color: [u8; 4],
        touched: &mut Option<IRect>,
    ) {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let dist = (dx * dx + dy * dy).sqrt();
        let divisor = if params.pencil { 2.0 } else { 4.0 };
        let step = (params.radius / divisor).max(1.0);
        let steps = (dist / step).ceil().max(1.0) as u32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = from.0 + dx * t;
            let y = from.1 + dy * t;
            self.apply_stamp(ctx, x, y, params, color, touched);
        }
    }

    /// `Tool::event` の共通実装。ストロークが確定したら、その色を選んだ
    /// ボタンを返す(呼び出し側が「最近使った色」に記録するため。消しゴムは
    /// 色の概念がないため無視してよい)。
    pub fn handle(
        &mut self,
        ev: ToolEvent,
        ctx: &mut ToolCtx,
        params: BrushParams,
    ) -> Option<PointerButton> {
        match ev {
            ToolEvent::Down { img, button, mods } => {
                if !matches!(button, PointerButton::Primary | PointerButton::Secondary) {
                    return None;
                }
                let color = color_bytes_for(ctx, button, params.erase);
                ctx.history.begin_stroke(ctx.doc.active);
                let mut touched = None;
                // SPEC §17: 「直前のストローク終点から Shift+クリック地点
                // まで直線をブラシで引く」。直前の終点が無ければ(この
                // ツールで初回のストローク)通常のスタンプにフォールバック
                // する。SPEC §25: 「Shift+クリック直線連結には適用しない」
                // ため、この経路はスムージングを一切経由しない(生の座標を
                // そのまま使う)。
                if mods.shift {
                    if let Some(prev) = self.last_end {
                        self.apply_segment(ctx, prev, (img.x, img.y), &params, color, &mut touched);
                    } else {
                        self.apply_stamp(ctx, img.x, img.y, &params, color, &mut touched);
                    }
                } else {
                    self.apply_stamp(ctx, img.x, img.y, &params, color, &mut touched);
                }
                self.stroke = Some(ActiveStroke {
                    button,
                    last: (img.x, img.y),
                    // SPEC §25: ストローク開始点は補正なし(即座にその位置
                    // から描き始める。遅延はストローク進行中にのみ生じる)。
                    smoothed: (img.x, img.y),
                    color,
                    touched,
                });
                None
            }
            ToolEvent::Drag { img, button, .. } => {
                let state = self.stroke.as_ref()?;
                if state.button != button {
                    return None;
                }
                let prev_smoothed = state.smoothed;
                let raw = (img.x, img.y);
                let new_smoothed = smooth_towards(prev_smoothed, raw, params.smoothing);
                let color = state.color;
                let mut touched = state.touched;
                self.apply_segment(
                    ctx,
                    prev_smoothed,
                    new_smoothed,
                    &params,
                    color,
                    &mut touched,
                );
                if let Some(state) = self.stroke.as_mut() {
                    state.last = raw;
                    state.smoothed = new_smoothed;
                    state.touched = touched;
                }
                None
            }
            ToolEvent::Up { img, button } => {
                let state = self.stroke.take()?;
                if state.button != button {
                    // 別ボタンの Up(通常は起きないが安全側に倒す): 状態を戻す。
                    self.stroke = Some(state);
                    return None;
                }
                // SPEC §25: 「ストローク終端で最終点まで追いつかせる」。
                // スムージングで遅れていた平滑化済み位置から、実際の最終
                // ポインタ位置まで最後のセグメントを描き切る(スムージング
                // 無し(`smoothed == final_point`)なら無害な 0 長セグメント
                // になるだけで、既存の挙動と完全に一致する)。
                let final_point = (img.x, img.y);
                let mut touched = state.touched;
                if final_point != state.smoothed {
                    self.apply_segment(
                        ctx,
                        state.smoothed,
                        final_point,
                        &params,
                        state.color,
                        &mut touched,
                    );
                }
                // ARCHITECTURE.md §18.3 の対応表: ブラシ/消しゴムは同じ
                // エンジンを共有するため `params.erase` でラベルを分ける。
                ctx.history.commit_stroke(
                    ctx.doc,
                    if params.erase {
                        "消しゴム"
                    } else {
                        "ブラシ"
                    },
                );
                if let Some(bbox) = touched {
                    self.clear_mask_region(bbox);
                }
                self.last_end = Some(final_point);
                Some(button)
            }
            ToolEvent::Hover { .. } => None,
        }
    }

    /// `Tool::cancel` の共通実装。進行中のストロークがあれば `Up` と同様に
    /// 確定する(ARCHITECTURE.md §6「1 ストローク = 1 undo 単位」を、
    /// ツール切替という中断経路でも守るため)。
    ///
    /// `label` は History に積む undo ラベル(ARCHITECTURE.md §18.3:
    /// 「ブラシ」/「消しゴム」)。呼び出し元(`PenTool`/`EraserTool`)は
    /// それぞれ固定のツール種別しか持たないため、`BrushParams` 経由ではなく
    /// 呼び出し側から直接渡す(`handle` の `Up` 分岐が `params.erase` から
    /// 選ぶのと同じ結果になる)。
    pub fn cancel(&mut self, ctx: &mut ToolCtx, label: &'static str) -> Option<PointerButton> {
        let state = self.stroke.take()?;
        ctx.history.commit_stroke(ctx.doc, label);
        if let Some(bbox) = state.touched {
            self.clear_mask_region(bbox);
        }
        self.last_end = Some(state.last);
        Some(state.button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Document};
    use crate::history::History;
    use eframe::egui::{Color32, Modifiers, Pos2};

    fn ctx<'a>(
        doc: &'a mut Document,
        history: &'a mut History,
        used: &'a mut Vec<Color32>,
    ) -> ToolCtx<'a> {
        ToolCtx {
            doc,
            history,
            primary: Color32::from_rgba_unmultiplied(255, 0, 0, 255),
            secondary: Color32::from_rgba_unmultiplied(0, 0, 255, 255),
            brush_size: 16.0,
            hardness: 1.0,
            opacity: 1.0,
            pencil: false,
            smoothing: 0.0,
            used_colors: used,
            clip: None,
        }
    }

    fn brush_params(radius: f32, hardness: f32, opacity: f32, pencil: bool) -> BrushParams {
        BrushParams {
            radius,
            hardness,
            opacity,
            pencil,
            erase: false,
            smoothing: 0.0,
        }
    }

    fn brush_params_with_smoothing(radius: f32, smoothing: f32) -> BrushParams {
        BrushParams {
            radius,
            hardness: 1.0,
            opacity: 1.0,
            pencil: false,
            erase: false,
            smoothing,
        }
    }

    // -- v4 §16.1/SPEC §28: ブラシ追従(4000×4000・64px ブラシの高速ドラッグ) --

    #[test]
    fn fast_drag_segment_on_a_large_document_terminates_quickly() {
        // SPEC §28: 「ブラシ追従: 4000×4000・ブラシ64px の高速ドラッグで
        // フレーム 16ms 以下(ドラッグを横断する巨大 dirty 矩形を作らない —
        // セグメント単位の dirty 処理)」。V4-M1 で実装したのは括弧内の
        // 「セグメント単位の dirty 処理」(`DirtyRegion`、このファイルでは
        // `BrushEngine` が呼ぶ `Document::mark_dirty` がスタンプごとに
        // セグメントを積むだけになった)であり、これにより 1 フレームの
        // ドラッグ距離に関係なく recomposite/テクスチャ部分更新のコストは
        // 実際に触れた面積に比例するようになった(以前は触れた領域の外接
        // 矩形 1 個ぶんの再合成が走っていた)。
        //
        // v4-M1 の時点では、このテストが計測する `apply_stamp` 自体の画素
        // ループ(カバレッジ計算 + `History::original_pixel` の CoW タイル
        // 参照)が画素ごとに `stroke.tiles` の `HashMap` を引き直しており、
        // リリースビルド実測で 1 フレームに画像座標で約 1400px 移動する
        // 極端に速いドラッグ(64px ブラシ)が約 34ms、約 5400px(キャンバス
        // 対角線相当)では約 123ms かかっていた(16ms 目標を超過)。
        // v4-M2(ARCHITECTURE.md §16.1)で `History::original_pixel_cursor`
        // (`OriginalPixelCursor`、1 スタンプの画素ループ全体で使い回すタイル
        // 探索キャッシュ)を導入し、同じタイル内では `HashMap` を再探索
        // しないようにした結果、リリースビルド実測で 1400px ドラッグは
        // 約 24ms(-29%)、5400px 対角線ドラッグは約 95ms(-23%)まで改善
        // した。ただし `HashMap` 探索自体を完全に取り除いても(検証用に
        // 一時的に History 参照そのものを外して計測)1400px ドラッグは
        // 約 23ms までしか縮まらなかったため、残るコストの大半は
        // `stamp_soft_coverage` の距離計算や `blend_over` の除算といった
        // 画素ごとの浮動小数点演算であり、`HashMap` 探索は当初の見立てほど
        // 支配的ではなかった(この画素演算自体の最適化は本タスクの
        // 診断・スコープ外のため今回は行わない、詳細は
        // `history.rs::OriginalPixelCursor` のドキュメント参照)。
        //
        // `cargo test` はデバッグビルドのため、実際の 16ms 目標そのものでは
        // なく緩い上限で回帰検知(無限ループ/O(n^2) 的な劣化がないこと)だけを
        // 行う(raster.rs の flood_fill 4000x4000 テストと同じ方針)。
        let mut doc = Document::new(4000, 4000, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(32.0, 0.8, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(100.0, 100.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        let start = std::time::Instant::now();
        engine.handle(
            ToolEvent::Drag {
                img: Pos2::new(1100.0, 1100.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        let elapsed = start.elapsed();
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(1100.0, 1100.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        assert!(
            elapsed.as_secs() < 10,
            "a single fast-drag segment took suspiciously long (possible regression): {elapsed:?}"
        );
    }

    #[test]
    fn fast_drag_segment_across_the_canvas_diagonal_terminates_quickly() {
        // 上のテストの「1400px」に加え、SPEC §28/deviations が言及する
        // もう一つの実測シナリオ(4000×4000 キャンバス対角線相当、約5657px)
        // の回帰テスト。release ビルド実測ではこちらが約 95ms
        // (v4-M2 の `OriginalPixelCursor` 導入前は約 123ms)で、上の
        // 1400px ケースより負荷が大きいぶん先に regression が見えやすい。
        // `cargo test` はデバッグビルドのため、ここも緩い上限で回帰検知
        // (無限ループ/O(n^2) 的な劣化がないこと)だけを行う。
        let mut doc = Document::new(4000, 4000, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(32.0, 0.8, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(0.0, 0.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        let start = std::time::Instant::now();
        engine.handle(
            ToolEvent::Drag {
                img: Pos2::new(3999.0, 3999.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        let elapsed = start.elapsed();
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(3999.0, 3999.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        assert!(
            elapsed.as_secs() < 10,
            "a single fast-drag segment across the canvas diagonal took suspiciously long \
             (possible regression): {elapsed:?}"
        );
    }

    // -- v4-M2: `apply_stamp` のタイル境界回帰テスト(ARCHITECTURE.md §16.1) --

    #[test]
    fn stamp_spanning_a_tile_boundary_blends_each_side_from_its_own_original_pixel() {
        // `History::OriginalPixelCursor`(タイル探索キャッシュ)の回帰
        // テスト: タイル境界(x=256)の両側で下地色が異なるドキュメントに、
        // 境界をまたぐ 1 スタンプを置く。キャッシュがタイル座標を取り違えると
        // 片側がもう片方のタイルの退避値から誤って合成されてしまう。
        let mut doc = Document::new(300, 60, Background::Transparent);
        for x in 0..300i32 {
            let base = if x < 256 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            for y in 0..60i32 {
                doc.set_pixel(x, y, base);
            }
        }
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        // 半径 40・硬さ100%: 中心から26px離れた場所でもカバレッジは
        // まだ内側(255)なので、期待値を厳密に計算できる。
        let params = brush_params(40.0, 1.0, 0.5, false);
        let green = [0, 255, 0, 255];
        let mut touched = None;

        history.begin_stroke(doc.active);
        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            engine.apply_stamp(&mut c, 256.0, 30.0, &params, green, &mut touched);
        }

        let left = doc.get_pixel(230, 30).unwrap();
        let right = doc.get_pixel(282, 30).unwrap();
        let expected_left = raster::blend_over([255, 0, 0, 255], [0, 255, 0, 128]);
        let expected_right = raster::blend_over([0, 0, 255, 255], [0, 255, 0, 128]);
        assert_eq!(
            left, expected_left,
            "left of the tile boundary must blend from the red base, \
             not the other tile's cached snapshot"
        );
        assert_eq!(
            right, expected_right,
            "right of the tile boundary must blend from the blue base, \
             not the other tile's cached snapshot"
        );
    }

    #[test]
    fn soft_stroke_paints_and_restores_on_undo() {
        let mut doc = Document::new(30, 30, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let original = doc.active_pixels().to_vec();

        let mut engine = BrushEngine::new();
        let params = brush_params(6.0, 1.0, 1.0, false);
        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            engine.handle(
                ToolEvent::Down {
                    img: Pos2::new(15.0, 15.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
            engine.handle(
                ToolEvent::Up {
                    img: Pos2::new(15.0, 15.0),
                    button: PointerButton::Primary,
                },
                &mut c,
                params,
            );
        }
        assert_ne!(doc.active_pixels(), original.as_slice());
        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.active_pixels(),
            original.as_slice(),
            "undo should byte-exactly restore"
        );
    }

    #[test]
    fn mask_is_cleared_after_stroke_so_next_stroke_starts_fresh() {
        let mut doc = Document::new(30, 30, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(6.0, 1.0, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );
        assert!(
            engine.mask.iter().all(|&v| v == 0),
            "mask should be all-zero at rest"
        );
    }

    // -- SPEC §17: PS opacity 意味論(1 ストローク内は不透明度上限を超えない) --

    #[test]
    fn overlapping_stamps_within_one_stroke_do_not_exceed_opacity_cap() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        // 不透明度 40%、完全不透明な赤を白地に重ね塗りする。
        let params = brush_params(6.0, 1.0, 0.4, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        // 同じ場所に何度もスタンプを重ねる(1 ストロークのまま)。
        for _ in 0..5 {
            engine.handle(
                ToolEvent::Drag {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
        }
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        let px = doc.get_pixel(10, 10).expect("in bounds");
        // 白(255)の上に不透明度 40% の赤を 1 回だけ合成した結果に一致する
        // はず(何度も重ねても加算されない)。
        let expected = raster::blend_over([255, 255, 255, 255], [255, 0, 0, 102]);
        assert_eq!(
            px, expected,
            "opacity must not exceed the 40% cap within a single stroke"
        );
    }

    #[test]
    fn separate_strokes_do_accumulate_opacity() {
        // ARCHITECTURE.md §15.1: 「ストロークをまたげば重なる」。
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(6.0, 1.0, 0.5, false);

        for _ in 0..2 {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            engine.handle(
                ToolEvent::Down {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
            engine.handle(
                ToolEvent::Up {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                },
                &mut c,
                params,
            );
        }

        let px = doc.get_pixel(10, 10).expect("in bounds");
        // 2 回の別ストロークで重ねた結果は、1 ストローク分(50%)よりも
        // 赤みが強くなる(2 回目は 1 回目の結果の上にさらに 50% 重なる)。
        // 赤(255,0,0)を白に重ねても R チャンネルは常に 255 のままなので、
        // G/B チャンネル(重なるほど 0 に近づく)で判定する。
        let one_stroke = raster::blend_over([255, 255, 255, 255], [255, 0, 0, 128]);
        assert!(
            px[1] < one_stroke[1],
            "two strokes should be redder (lower G) than a single 50% stroke: got {px:?} vs one_stroke {one_stroke:?}"
        );
    }

    // -- SPEC §17: 鉛筆モード(硬さ無視、2値) --------------------------------

    #[test]
    fn pencil_mode_produces_binary_alpha_no_partial_edge_pixels() {
        let mut doc = Document::new(30, 30, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        // 硬さは無視されるはず(0.0 を渡しても鉛筆モードなら効かない)。
        let params = brush_params(6.0, 0.0, 1.0, true);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(15.0, 15.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(15.0, 15.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        for y in 5..25 {
            for x in 5..25 {
                let a = doc.get_pixel(x, y).unwrap()[3];
                assert!(
                    a == 0 || a == 255,
                    "pencil mode must not produce partial alpha, got {a} at ({x},{y})"
                );
            }
        }
        assert_eq!(doc.get_pixel(15, 15).unwrap()[3], 255);
    }

    // -- SPEC §17: 消しゴム強さ(カバレッジ×強さぶんアルファを減らす) --------

    #[test]
    fn eraser_strength_partially_reduces_alpha_and_caps_within_stroke() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = BrushParams {
            radius: 6.0,
            hardness: 1.0,
            opacity: 0.5, // 「強さ」50%。
            pencil: false,
            erase: true,
            smoothing: 0.0,
        };

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        // 同じ場所へ何度も重ねても、このストローク内では 50% 減衰のまま
        // (max 合成のため、繰り返しても際限なく消えたりしない)。
        for _ in 0..4 {
            engine.handle(
                ToolEvent::Drag {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
        }
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        let alpha = doc.get_pixel(10, 10).unwrap()[3];
        assert!(
            (120..=135).contains(&alpha),
            "expected ~50% alpha remaining, got {alpha}"
        );
        // RGB は保持される(消しゴムは色を変えない)。
        let rgb = &doc.get_pixel(10, 10).unwrap()[0..3];
        assert_eq!(rgb, [255, 255, 255]);
    }

    #[test]
    fn eraser_full_strength_makes_pixel_fully_transparent() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = BrushParams {
            radius: 6.0,
            hardness: 1.0,
            opacity: 1.0,
            pencil: false,
            erase: true,
            smoothing: 0.0,
        };

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        assert_eq!(doc.get_pixel(10, 10).unwrap()[3], 0);
    }

    // -- SPEC §17: Shift+クリック連結 -----------------------------------------

    #[test]
    fn shift_click_connects_a_straight_line_from_last_stroke_end() {
        let mut doc = Document::new(40, 10, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(3.0, 1.0, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        // 1 個目: 単クリック(ドット)を (5,5) に打つ。
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );
        // 中間点はまだ塗られていないはず。
        assert_eq!(c.doc.get_pixel(20, 5).unwrap()[3], 0);

        // 2 個目: Shift+クリックで (35,5) まで直線連結。
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(35.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::SHIFT,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(35.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        // 経路の中間点が塗られているはず(単なる 2 つのドットではなく線)。
        assert_ne!(
            c.doc.get_pixel(20, 5).unwrap()[3],
            0,
            "shift+click must connect a line through the midpoint"
        );
    }

    // -- v3 レビューで発見・修正したバグ: ドキュメント差し替え(新規作成/
    // 開く/白紙貼り付け置換)後も `last_end` が残留し、まだ一度も描いて
    // いない新ドキュメントで最初の Shift+クリックが旧ドキュメントの座標
    // から直線を引いてしまっていた。`reset_for_new_document` で解消する
    // (呼び出し配線は app.rs::open_path/confirm_new/
    // replace_document_with_pasted_image、`reset_tool_state_for_new_
    // document` 参照)。---------------------------------------------------

    #[test]
    fn reset_for_new_document_clears_stale_last_end_so_shift_click_paints_a_dot() {
        let mut doc = Document::new(40, 10, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(3.0, 1.0, 1.0, false);

        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            // 「旧ドキュメント」で (5,5) に単クリック(ドット)を打ち、
            // last_end を (5,5) にする。
            engine.handle(
                ToolEvent::Down {
                    img: Pos2::new(5.0, 5.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
            engine.handle(
                ToolEvent::Up {
                    img: Pos2::new(5.0, 5.0),
                    button: PointerButton::Primary,
                },
                &mut c,
                params,
            );
        }

        // ドキュメント差し替え(新規作成/開く相当)。
        engine.reset_for_new_document();
        let mut doc = Document::new(40, 10, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();

        // 「新ドキュメント」で最初の Shift+クリックを (35,5) に打つ。
        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            engine.handle(
                ToolEvent::Down {
                    img: Pos2::new(35.0, 5.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::SHIFT,
                },
                &mut c,
                params,
            );
            engine.handle(
                ToolEvent::Up {
                    img: Pos2::new(35.0, 5.0),
                    button: PointerButton::Primary,
                },
                &mut c,
                params,
            );
        }

        // last_end がリセットされていれば単なるドットになり、旧ドキュメント
        // の (5,5) から (35,5) までの経路の中間点は塗られない。
        assert_eq!(
            doc.get_pixel(20, 5).unwrap()[3],
            0,
            "after reset_for_new_document, shift+click must not connect to the stale \
             pre-reset endpoint from the previous document"
        );
        assert_ne!(
            doc.get_pixel(35, 5).unwrap()[3],
            0,
            "the shift+click point itself must still be painted as a dot"
        );
    }

    #[test]
    fn shift_click_without_a_prior_stroke_just_paints_a_dot() {
        let mut doc = Document::new(20, 20, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(3.0, 1.0, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::SHIFT,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(10.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );
        assert_ne!(doc.get_pixel(10, 10).unwrap()[3], 0);
    }

    // -- v4 §16.3/§21: 描画クリップ(選択範囲内にのみ効く) ---------------------

    #[test]
    fn stroke_is_clipped_to_the_selection_mask() {
        use crate::document::SelMask;

        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(6.0, 1.0, 1.0, false);

        // 左半分だけ選択されたクリップマスク。
        let clip = SelMask {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 20,
            },
            mask: vec![255u8; 10 * 20],
        };

        {
            let mut c = ctx(&mut doc, &mut history, &mut used);
            c.clip = Some(&clip);
            // ストローク中心をクリップ境界(x=10)にまたがせる。
            engine.handle(
                ToolEvent::Down {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                    mods: Modifiers::NONE,
                },
                &mut c,
                params,
            );
            engine.handle(
                ToolEvent::Up {
                    img: Pos2::new(10.0, 10.0),
                    button: PointerButton::Primary,
                },
                &mut c,
                params,
            );
        }

        assert_ne!(
            doc.get_pixel(6, 10),
            Some([255, 255, 255, 255]),
            "inside the clip must be painted"
        );
        assert_eq!(
            doc.get_pixel(14, 10),
            Some([255, 255, 255, 255]),
            "outside the clip must remain untouched even though it's within the brush radius"
        );
    }

    #[test]
    fn cancel_commits_open_stroke_and_records_last_end() {
        let mut doc = Document::new(20, 20, Background::White);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(4.0, 1.0, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        assert!(c.history.has_open_stroke());
        let button = engine.cancel(&mut c, "ブラシ");
        assert_eq!(button, Some(PointerButton::Primary));
        assert!(!c.history.has_open_stroke());
        assert!(c.history.can_undo());
        assert_eq!(engine.last_end, Some((5.0, 5.0)));
    }

    // -- SPEC §25: スムージング(手ブレ補正) -----------------------------------

    #[test]
    fn smoothing_alpha_is_one_at_zero_strength_and_point_one_at_full_strength() {
        assert_eq!(smoothing_alpha(0.0), 1.0);
        assert!((smoothing_alpha(1.0) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn smoothing_alpha_clamps_out_of_range_strength() {
        assert_eq!(smoothing_alpha(-5.0), smoothing_alpha(0.0));
        assert_eq!(smoothing_alpha(5.0), smoothing_alpha(1.0));
    }

    #[test]
    fn smooth_towards_zero_strength_jumps_directly_to_raw() {
        let out = smooth_towards((0.0, 0.0), (10.0, 20.0), 0.0);
        assert_eq!(out, (10.0, 20.0));
    }

    #[test]
    fn smooth_towards_full_strength_moves_only_a_tenth_of_the_way() {
        let out = smooth_towards((0.0, 0.0), (10.0, 0.0), 1.0);
        assert!((out.0 - 1.0).abs() < 1e-5, "expected ~1.0, got {}", out.0);
    }

    #[test]
    fn dragging_with_smoothing_lags_behind_the_raw_pointer_position() {
        // スムージング有りだと、平滑化済み位置は生の入力より手前で止まる
        // (ARCHITECTURE.md §16.6 の EMA の性質どおり)。
        let mut doc = Document::new(60, 20, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params_with_smoothing(3.0, 0.9);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(0.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Drag {
                img: Pos2::new(50.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );

        // 大きな強さ(0.9)の EMA は 1 フレームで生の移動量のごく一部にしか
        // 追いつかないため、遠く離れた終点(x=50 付近)はまだ塗られていない
        // はず(手前で止まっている)。
        assert_eq!(doc.get_pixel(48, 10).unwrap()[3], 0);
    }

    #[test]
    fn releasing_catches_up_to_the_final_pointer_position_even_with_smoothing() {
        // SPEC §25: 「ストローク終端で最終点まで追いつかせる」。
        let mut doc = Document::new(60, 20, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params_with_smoothing(3.0, 0.9);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(0.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Drag {
                img: Pos2::new(50.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(50.0, 10.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        assert_ne!(
            doc.get_pixel(49, 10).unwrap()[3],
            0,
            "Up must paint through to the final raw pointer position despite smoothing lag"
        );
    }

    #[test]
    fn shift_click_line_connect_ignores_smoothing() {
        // SPEC §25: 「Shift+クリック直線連結には適用しない」。強いスムージング
        // 設定下でも、Shift+クリックは即座に直線で終点まで届くはず。
        let mut doc = Document::new(60, 10, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params_with_smoothing(3.0, 1.0);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(5.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(55.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::SHIFT,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Up {
                img: Pos2::new(55.0, 5.0),
                button: PointerButton::Primary,
            },
            &mut c,
            params,
        );

        assert_ne!(
            doc.get_pixel(30, 5).unwrap()[3],
            0,
            "shift+click must connect a full straight line even with strong smoothing"
        );
    }

    #[test]
    fn zero_smoothing_behaves_exactly_like_before_the_feature_existed() {
        // strength=0 は α=1.0 なので、平滑化済み位置は常に生の入力に一致する
        // (回帰なし)。
        let mut doc = Document::new(60, 20, Background::Transparent);
        let mut history = History::new();
        let mut used = Vec::new();
        let mut engine = BrushEngine::new();
        let params = brush_params(3.0, 1.0, 1.0, false);

        let mut c = ctx(&mut doc, &mut history, &mut used);
        engine.handle(
            ToolEvent::Down {
                img: Pos2::new(0.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );
        engine.handle(
            ToolEvent::Drag {
                img: Pos2::new(50.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            &mut c,
            params,
        );

        assert_ne!(doc.get_pixel(49, 10).unwrap()[3], 0);
    }
}
