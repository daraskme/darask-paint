//! アンドゥ履歴パネル(右パネルの一部、SPEC §35、ARCHITECTURE.md §18.4)。
//!
//! 一覧は上から下へ時系列(先頭 = 初期状態、末尾 = 最新)。現在位置を
//! ハイライトし、それより後ろ(やり直し可能な範囲)は淡色表示にする。
//! クリックされた行があれば、そこへジャンプすべき `undo_stack` の目標長
//! (`History::jump_to` にそのまま渡す値)を返すだけに留める。実際の
//! ジャンプ実行(進行中のストローク・浮動片を先に確定してから
//! `History::jump_to` を呼ぶ「commit-first」安全規則)は呼び出し側
//! (`app.rs::jump_history_to`)の責務(ARCHITECTURE.md §18.4・§18.6-1:
//! 「ジャンプ専用の別ルートを作らない」— 既存の Undo/Redo ショートカットが
//! 使っているのと全く同じ `commit_open_gesture()` を再利用する)。

use eframe::egui;

use crate::history::History;

/// SPEC §35: 「先頭には常に仮想的な「(初期状態)」行がある」。
const INITIAL_STATE_LABEL: &str = "(初期状態)";

const OLDER_HISTORY_NOTE: &str = "(古い履歴もプロジェクトに保存されます)";

/// 履歴パネルを描画する(アクティブタブの `History` を渡す。タブ切替時は
/// 呼び出し側が渡す `history` が自然に切り替わるだけで追随する、
/// ARCHITECTURE.md §18.4)。クリックされた行があれば、`History::jump_to` に
/// そのまま渡せる目標 `undo_stack` 長を返す。
pub fn show(ui: &mut egui::Ui, history: &History) -> Option<usize> {
    let mut jump_target = None;
    ui.heading("履歴");
    ui.add_space(4.0);

    let current_len = history.undo_len();
    let redo_len = history.redo_labels_reversed().len();
    let display_limit = history.display_step_limit();
    let mut undo_take = current_len.min(display_limit.div_ceil(2));
    let mut redo_take = redo_len.min(display_limit.saturating_sub(undo_take));
    let remaining = display_limit.saturating_sub(undo_take + redo_take);
    undo_take += current_len.saturating_sub(undo_take).min(remaining);
    let remaining = display_limit.saturating_sub(undo_take + redo_take);
    redo_take += redo_len.saturating_sub(redo_take).min(remaining);
    let undo_start = current_len.saturating_sub(undo_take);

    egui::ScrollArea::vertical()
        .max_height(140.0)
        .auto_shrink([false, true])
        .id_salt("darask_history_panel_scroll")
        .show(ui, |ui| {
            if undo_start > 0 {
                ui.weak(OLDER_HISTORY_NOTE);
            }

            // 仮想「(初期状態)」行。target_len = 0(未編集の状態まで戻る)。
            if undo_start == 0 && row(ui, INITIAL_STATE_LABEL, current_len == 0, false).clicked() {
                jump_target = Some(0);
            }

            // undo_stack: 先頭(最古)から順に。行 i(0-indexed)をクリック
            // すると、そのラベルの操作を適用した直後の状態(target_len =
            // i + 1)まで戻る/進む。末尾(target_len == current_len)が
            // 「現在位置」。
            for (i, label) in history.undo_labels().enumerate().skip(undo_start) {
                let target = i + 1;
                if row(ui, label, target == current_len, false).clicked() {
                    jump_target = Some(target);
                }
            }

            // redo_stack: 「逆順(直近の undo ほど上)」で、現在位置の直後
            // から時系列順に続く(`History::redo_labels_reversed` のドキュ
            // メント参照)。淡色表示、target_len は現在位置からの通し番号。
            for (j, label) in history.redo_labels_reversed().take(redo_take).enumerate() {
                let target = current_len + j + 1;
                if row(ui, label, false, true).clicked() {
                    jump_target = Some(target);
                }
            }
        });

    jump_target
}

/// 履歴パネルの 1 行。`is_current` なら選択ハイライト、`dimmed` なら
/// SPEC §35 の「やり直し可能な範囲は淡色表示」に従い弱色のテキストで描く。
fn row(ui: &mut egui::Ui, label: &str, is_current: bool, dimmed: bool) -> egui::Response {
    if dimmed {
        // `ui.selectable_label` は `egui::Button::selectable` の薄いラッパー
        // (egui 0.35 `ui.rs` 参照)。`SelectableLabel` という公開ウィジェット
        // 型は無い(`WidgetType` の内部バリアント名でしかない)ため、同じ
        // `Button::selectable` を直接使い、テキストだけ `weak()` で弱色にする。
        ui.add(egui::Button::selectable(
            false,
            egui::RichText::new(label).weak(),
        ))
    } else {
        ui.selectable_label(is_current, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Document};
    use crate::raster::{self, Surface};

    fn surface(doc: &mut Document) -> Surface<'_> {
        doc.active_surface_mut(None)
    }

    /// `show` を 1 フレーム描画し、クリックなしで `None` を返すことを確認する
    /// (egui の `Context` はバックエンド不要で駆動できる、
    /// `ui/layers_panel.rs` のテストと同じ手法)。実クリックのピクセル座標を
    /// 特定して押下イベントを注入する統合テストは、egui のレイアウト内部
    /// (フォントメトリクス依存の行位置)に脆く結び付いてしまうため行わず、
    /// 「パネルが例外を出さずに描画でき、無操作時は `None` を返す」ことと、
    /// `show` が使う target 計算式の正しさ(下の `*_target_formula_matches_*`)
    /// を分けて検証する。
    fn render_without_clicking(history: &History) -> Option<usize> {
        let ctx = egui::Context::default();
        let mut result = None;
        ctx.begin_pass(egui::RawInput::default());
        egui::Area::new(egui::Id::new("darask_history_panel_test_area")).show(&ctx, |ui| {
            result = show(ui, history);
        });
        let _ = ctx.end_pass();
        result
    }

    #[test]
    fn fresh_history_renders_without_a_click_and_returns_none() {
        let history = History::new();
        assert_eq!(render_without_clicking(&history), None);
    }

    #[test]
    fn history_with_undo_and_redo_entries_renders_without_a_click_and_returns_none() {
        // undo_stack・redo_stack の両方に行がある状態(履歴パネルが描く
        // 3 種類の行 — 現在位置行・undo 行・淡色の redo 行 — を全部通す
        // 描画経路の健全性チェック。パニックしないこと、クリックしていない
        // ので `None` を返すことを確認する。
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        for (i, (cx, cy)) in [(2.0, 2.0), (5.0, 5.0), (8.0, 8.0)].into_iter().enumerate() {
            history.begin_stroke(doc.active);
            let bounds = raster::stamp_bounds(cx, cy, 1.0);
            history.ensure_tiles_saved(&doc, bounds);
            raster::stamp_round(&mut surface(&mut doc), cx, cy, 1.0, [1, 2, 3, 4], false);
            history.commit_stroke(&mut doc, format!("op{i}"));
        }
        history.undo(&mut doc);
        assert!(history.can_undo() && history.can_redo());

        assert_eq!(render_without_clicking(&history), None);
    }

    #[test]
    fn cache_hint_does_not_truncate_or_break_history_rendering() {
        let mut history = History::new();
        history.set_max_steps(2);
        for label in ["a", "b", "c"] {
            history.push(
                crate::history::HistoryOp::Patch {
                    layer: 0,
                    regions: vec![],
                },
                label,
            );
        }
        assert_eq!(render_without_clicking(&history), None);
    }

    // -- `show` 内の target 計算式のピン止めテスト --------------------------
    //
    // `show` は行のクリックに応じて
    //   ・「(初期状態)」行 → target = 0
    //   ・undo 行 i(0-indexed、最古から) → target = i + 1
    //   ・redo 行 j(0-indexed、`redo_labels_reversed` の順) →
    //     target = current_len + j + 1
    // という式で `History::jump_to` の目標長を計算する(関数本体のコメント
    // 参照)。実際のポインタクリックは注入せず、同じ式を `History::jump_to`
    // に通して想定どおりの状態に一致することを確認することで、この式が
    // `History` の意味論とずれていないことをピン止めする。

    #[test]
    fn undo_row_target_formula_matches_jump_to_semantics() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        for (i, (cx, cy)) in [(2.0, 2.0), (5.0, 5.0), (8.0, 8.0)].into_iter().enumerate() {
            history.begin_stroke(doc.active);
            let bounds = raster::stamp_bounds(cx, cy, 1.0);
            history.ensure_tiles_saved(&doc, bounds);
            raster::stamp_round(&mut surface(&mut doc), cx, cy, 1.0, [1, 2, 3, 4], false);
            history.commit_stroke(&mut doc, format!("op{i}"));
        }
        history.undo(&mut doc);
        history.undo(&mut doc);
        history.undo(&mut doc);
        assert_eq!(history.undo_len(), 0);

        // undo 行 1("op1"、0-indexed i=1)→ target = i + 1 = 2。
        history.jump_to(&mut doc, 1 + 1);
        assert_eq!(history.undo_len(), 2);
        assert_eq!(history.undo_labels().last(), Some("op1"));
    }

    #[test]
    fn redo_row_target_formula_matches_jump_to_semantics() {
        let mut doc = Document::new(10, 10, Background::White);
        let mut history = History::new();
        for (i, (cx, cy)) in [(2.0, 2.0), (5.0, 5.0), (8.0, 8.0)].into_iter().enumerate() {
            history.begin_stroke(doc.active);
            let bounds = raster::stamp_bounds(cx, cy, 1.0);
            history.ensure_tiles_saved(&doc, bounds);
            raster::stamp_round(&mut surface(&mut doc), cx, cy, 1.0, [1, 2, 3, 4], false);
            history.commit_stroke(&mut doc, format!("op{i}"));
        }
        history.undo(&mut doc);
        history.undo(&mut doc);
        let current_len = history.undo_len();
        assert_eq!(current_len, 1);
        assert_eq!(
            history.redo_labels_reversed().collect::<Vec<_>>(),
            ["op1", "op2"]
        );

        // redo 行 1("op2"、`redo_labels_reversed` の j=1)→
        // target = current_len + j + 1 = 1 + 1 + 1 = 3。
        history.jump_to(&mut doc, current_len + 1 + 1);
        assert_eq!(history.undo_len(), 3);
        assert_eq!(history.undo_labels().last(), Some("op2"));
    }
}
