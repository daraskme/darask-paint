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
/// `Some((layer_index, editing_text))`。
pub type RenameState = Option<(usize, String)>;

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
            .on_hover_text("新規レイヤー (Ctrl+Shift+N)")
            .clicked()
        {
            action = Some(LayersPanelAction::Add);
        }
        if ui
            .add_enabled(layer_count < MAX_LAYERS, egui::Button::new("複製"))
            .on_hover_text("レイヤーを複製")
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
            .on_hover_text("下のレイヤーと結合 (Ctrl+E)")
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

        let is_editing = matches!(rename, Some((i, _)) if *i == idx);
        if is_editing {
            let (_, text) = rename.as_mut().expect("is_editing implies Some");
            let response = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(120.0)
                    .id(egui::Id::new(("darask_layer_rename", idx))),
            );
            response.request_focus();
            if response.lost_focus() {
                let trimmed = text.trim().to_owned();
                if !trimmed.is_empty() {
                    if let Some(layer) = doc.layers.get_mut(idx) {
                        layer.name = trimmed;
                    }
                }
                *rename = None;
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
                    *rename = Some((idx, layer.name.clone()));
                }
            }
        }
    });
}
