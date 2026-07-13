//! レイヤーパネル(右パネルの一部、SPEC §13、ARCHITECTURE.md §14.4/§14.8)。
//!
//! 一覧(最上位レイヤーが一番上)・表示チェック・クリックでアクティブ化・
//! ダブルクリックで名前変更・新規/複製/削除/上へ/下へ/下と結合ボタン・
//! アクティブレイヤーの不透明度スライダーを表示する。
//!
//! 構造を変える操作(新規/複製/削除/上へ/下へ/下と結合、および
//! アクティブ化)は 1 undo 単位(新規/複製/削除/上下移動/結合)または
//! 「先に確定」フックが必要(アクティブ化、ARCHITECTURE.md §14.9-3)なため、
//! `LayersPanelAction` として返すだけに留め、実際の `Document` 操作は
//! `app.rs` が行う。表示切替・不透明度・名前変更は履歴に積まない
//! (SPEC §13 に明記)ので、ここで直接 `Document` を変更してよい。

use eframe::egui;

use crate::document::{Document, MAX_LAYERS};
use crate::keymap::{self, Action};

/// 構造を変える(または「先に確定」が必要な)操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayersPanelAction {
    Activate(usize),
    Add,
    Duplicate,
    Delete,
    MoveUp,
    MoveDown,
    MergeDown,
}

/// ダブルクリックで開始した名前編集の状態(`app.rs` が保持する)。
/// `Some((layer_index, editing_text, needs_focus))`。
///
/// `needs_focus` は編集開始直後の 1 フレームだけ `true`。egui 0.35 の
/// `Response::lost_focus()` は「直近フレームでフォーカスを持っていた &&
/// 現在フォーカスを持っていない」を照会する実装のため、`request_focus()` を
/// 毎フレーム無条件に呼ぶと `Memory::request_focus` が `focused_widget` を
/// 即座に同じ id へ再設定してしまい、`!has_focus(id)` が常に偽になって
/// `lost_focus()` が恒久的に発火しなくなる(Enter/Esc/欄外クリックで
/// リネームを確定・終了できず、フォーカスが居座り続けるため
/// `ctx.egui_wants_keyboard_input()` も真のままになり全ショートカットが
/// 効かなくなる)。`app.rs` の `TextEditState::needs_focus` と同じ
/// 「編集開始フレームのみ `request_focus()` する」パターンで回避する。
pub type RenameState = Option<(usize, String, bool)>;

/// レイヤーパネルを描画する。クリックされた構造操作があれば返す。
pub fn show(
    ui: &mut egui::Ui,
    doc: &mut Document,
    rename: &mut RenameState,
) -> Option<LayersPanelAction> {
    let mut action = None;
    ui.heading("レイヤー");
    ui.add_space(4.0);

    let active = doc.active_index();
    let layer_count = doc.layers.len();

    egui::ScrollArea::vertical()
        .max_height(180.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            // SPEC §13: 「一覧(最上位レイヤーが一番上に表示)」。
            for idx in (0..layer_count).rev() {
                show_layer_row(ui, doc, idx, active, rename, &mut action);
            }
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(layer_count < MAX_LAYERS, egui::Button::new("新規"))
            .on_hover_text(keymap::menu_label("新規レイヤー", Action::LayerAdd))
            .clicked()
        {
            action = Some(LayersPanelAction::Add);
        }
        if ui
            .add_enabled(layer_count < MAX_LAYERS, egui::Button::new("複製"))
            .on_hover_text(keymap::menu_label("レイヤーを複製", Action::LayerDuplicate))
            .clicked()
        {
            action = Some(LayersPanelAction::Duplicate);
        }
        // SPEC §13: 「レイヤーが 1 枚のときは削除・結合は無効」。
        if ui
            .add_enabled(layer_count > 1, egui::Button::new("削除"))
            .on_hover_text("レイヤーを削除")
            .clicked()
        {
            action = Some(LayersPanelAction::Delete);
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(active + 1 < layer_count, egui::Button::new("上へ"))
            .on_hover_text("上へ移動")
            .clicked()
        {
            action = Some(LayersPanelAction::MoveUp);
        }
        if ui
            .add_enabled(active > 0, egui::Button::new("下へ"))
            .on_hover_text("下へ移動")
            .clicked()
        {
            action = Some(LayersPanelAction::MoveDown);
        }
        if ui
            .add_enabled(layer_count > 1 && active > 0, egui::Button::new("下と結合"))
            .on_hover_text(keymap::menu_label(
                "下のレイヤーと結合",
                Action::LayerMergeDown,
            ))
            .clicked()
        {
            action = Some(LayersPanelAction::MergeDown);
        }
    });

    ui.add_space(6.0);
    show_opacity_slider(ui, doc, active);

    action
}

/// アクティブレイヤーの不透明度スライダー(SPEC §13)。ARCHITECTURE.md
/// §14.9-8: 値が実際に変わったフレームだけ `mark_all_dirty` する
/// (ドラッグ中に全面 recomposite が毎フレーム 1 回に抑えられる)。
fn show_opacity_slider(ui: &mut egui::Ui, doc: &mut Document, active: usize) {
    ui.label("不透明度:");
    let Some(layer) = doc.layers.get(active) else {
        return;
    };
    let mut opacity_pct = (layer.opacity as f32 / 255.0 * 100.0).round() as i32;
    if ui
        .add(egui::Slider::new(&mut opacity_pct, 0..=100).suffix("%"))
        .changed()
    {
        let new_opacity = ((opacity_pct.clamp(0, 100) as f32) / 100.0 * 255.0).round() as u8;
        if let Some(layer) = doc.layers.get_mut(active) {
            if layer.opacity != new_opacity {
                layer.opacity = new_opacity;
                doc.mark_all_dirty();
                doc.modified = true;
            }
        }
    }
}

fn show_layer_row(
    ui: &mut egui::Ui,
    doc: &mut Document,
    idx: usize,
    active: usize,
    rename: &mut RenameState,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(layer_visible) = doc.layers.get(idx).map(|l| l.visible) else {
        return;
    };
    let is_active = idx == active;

    ui.horizontal(|ui| {
        let mut visible = layer_visible;
        if ui.checkbox(&mut visible, "").changed() {
            if let Some(layer) = doc.layers.get_mut(idx) {
                layer.visible = visible;
            }
            doc.mark_all_dirty();
            doc.modified = true;
        }

        let is_editing = matches!(rename, Some((i, _, _)) if *i == idx);
        if is_editing {
            let (_, text, needs_focus) = rename.as_mut().expect("is_editing implies Some");
            let response = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(120.0)
                    .id(egui::Id::new(("darask_layer_rename", idx))),
            );
            // 編集開始フレームのみフォーカスを要求する(`RenameState` の
            // ドキュメントコメント参照)。
            if *needs_focus {
                response.request_focus();
            }
            let lost_focus = response.lost_focus();
            if let Some((_, _, needs_focus)) = rename.as_mut() {
                *needs_focus = false;
            }
            if lost_focus {
                if let Some((_, text, _)) = rename.take() {
                    let trimmed = text.trim().to_owned();
                    if !trimmed.is_empty() {
                        if let Some(layer) = doc.layers.get_mut(idx) {
                            layer.name = trimmed;
                            // バグ修正: 上の表示切替・下の不透明度ハンドラは
                            // どちらも変更時に `doc.modified` を立てるが、
                            // リネームだけこれを欠いていた。立てないと
                            // `doc_is_pristine()`(`path.is_none() &&
                            // !modified` のみ判定)がリネーム済みの文書を
                            // 「白紙」のまま誤判定し、Ctrl+V の白紙置換
                            // パス(`replace_document_with_pasted_image`)に
                            // 載ってドキュメントごと差し替わってしまう。
                            doc.modified = true;
                        }
                    }
                }
            }
        } else {
            let name = doc
                .layers
                .get(idx)
                .map(|l| l.name.clone())
                .unwrap_or_default();
            let response = ui
                .selectable_label(is_active, name)
                .on_hover_text("クリックでアクティブ化、ダブルクリックで名前変更");
            if response.clicked() {
                *action = Some(LayersPanelAction::Activate(idx));
            }
            if response.double_clicked() {
                if let Some(layer) = doc.layers.get(idx) {
                    *rename = Some((idx, layer.name.clone(), true));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;

    /// 回帰テスト: src/ui/layers_panel.rs:171(元)のバグ。編集中の
    /// `TextEdit` に毎フレーム無条件で `request_focus()` を呼ぶと、
    /// singleline `TextEdit` が Enter で内部的に `surrender_focus()` しても
    /// 直後の `request_focus()` がフォーカスを奪い返してしまい
    /// `Response::lost_focus()` が恒久的に発火しなくなる(egui 0.35
    /// `Memory::lost_focus` は `!has_focus(id)` を必須条件に持つため)。
    /// `egui::Context` はバックエンド不要で複数フレームを直接駆動できる
    /// (`app.rs::ctx_with_key_event` と同じ手法)。
    #[test]
    fn layer_rename_textedit_loses_focus_on_enter_and_commits_name() {
        let mut doc = Document::new(4, 4, Background::White);
        let mut rename: RenameState = Some((0, "old".to_owned(), true));

        let ctx = egui::Context::default();

        // フレーム1: 編集開始フレーム。needs_focus=true → request_focus() が
        // 呼ばれ TextEdit がフォーカスを得る。
        ctx.begin_pass(egui::RawInput::default());
        egui::Area::new(egui::Id::new("test_area")).show(&ctx, |ui| {
            show(ui, &mut doc, &mut rename);
        });
        let _ = ctx.end_pass();
        assert!(
            matches!(&rename, Some((_, _, needs_focus)) if !*needs_focus),
            "needs_focus must be consumed after the first frame"
        );
        assert_eq!(doc.layers[0].name, "背景", "still editing, name unchanged");

        // 編集中にテキストを変更してから Enter。singleline TextEdit は
        // Enter で内部的に surrender_focus する(egui 0.35
        // text_edit/builder.rs:1115)。
        if let Some((_, text, _)) = rename.as_mut() {
            *text = "new name".to_owned();
        }
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        });
        egui::Area::new(egui::Id::new("test_area")).show(&ctx, |ui| {
            show(ui, &mut doc, &mut rename);
        });
        let _ = ctx.end_pass();

        assert!(
            rename.is_none(),
            "Enter surrenders focus mid-frame; without the needs_focus fix the immediately \
             following request_focus() would reclaim it every frame and lost_focus() would \
             never fire, leaving the rename box (and keyboard shortcuts) stuck open forever"
        );
        assert_eq!(
            doc.layers[0].name, "new name",
            "lost_focus() firing must commit the edited name"
        );
    }
}
