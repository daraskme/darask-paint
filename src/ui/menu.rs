//! メニューバー(SPEC §7)。
//!
//! M1 では日本語ラベルでのレイアウトのみ実装していた。M4 で実際のクリック
//! 処理を配線する: 各項目は `MenuAction` を返し、`app.rs` が実行する
//! (rfd のネイティブダイアログ呼び出しはフレーム処理の外側で行う必要が
//! あるため、ここではまだ実行せず「何を要求されたか」だけを返す、
//! ARCHITECTURE.md §12-9)。有効/無効(元に戻す・やり直し・選択系・
//! トリミング)は `MenuState` で受け取る。

use eframe::egui;

use crate::keymap::{self, Action};

/// クリックされたメニュー項目(まだ副作用は起こさない。`app.rs` が実行する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    New,
    Open,
    Save,
    SaveAs,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Deselect,
    ImageResize,
    CanvasResize,
    Crop,
    FlipHorizontal,
    FlipVertical,
    RotateCw,
    RotateCcw,
    ZoomIn,
    ZoomOut,
    Zoom100,
    FitWindow,
    // -- v2 §13: レイヤーメニュー(ARCHITECTURE.md §14.8 V2-M2) -----------
    LayerAdd,
    LayerDuplicate,
    LayerDelete,
    LayerMoveUp,
    LayerMoveDown,
    LayerMergeDown,
    LayerFlatten,
}

/// メニュー項目の有効/無効判定に使う状態。
pub struct MenuState {
    pub can_undo: bool,
    pub can_redo: bool,
    /// 選択または浮動片がある(切り取り/コピー/削除/トリミングを有効にする)。
    pub has_selection: bool,
    // -- v2 §13: レイヤーメニューの有効/無効(document.rs の各操作の成否
    // 条件と 1:1 に対応させる、ARCHITECTURE.md §14.8 V2-M2) -------------
    pub can_add_layer: bool,
    pub can_delete_layer: bool,
    pub can_move_layer_up: bool,
    pub can_move_layer_down: bool,
    pub can_merge_layer_down: bool,
    pub can_flatten_layers: bool,
}

/// クリックされた項目があれば返す(複数同時クリックは起こらないので
/// `Option` でよい)。
pub fn show(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    let mut action = None;
    egui::Panel::top("menu_bar").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.menu_button("ファイル", |ui| {
                if ui.button(keymap::menu_label("新規", Action::New)).clicked() {
                    action = Some(MenuAction::New);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("開く", Action::Open))
                    .clicked()
                {
                    action = Some(MenuAction::Open);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("上書き保存", Action::Save))
                    .clicked()
                {
                    action = Some(MenuAction::Save);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("名前を付けて保存", Action::SaveAs))
                    .clicked()
                {
                    action = Some(MenuAction::SaveAs);
                    ui.close();
                }
                ui.separator();
                // Alt+F4 は OS/ウィンドウマネージャが `close_requested` として
                // 通知するものであり egui が消費するショートカットではない
                // ため、`keymap::KEYMAP` の対象外(`keymap` モジュール
                // ドキュメントコメント参照)。表記は固定文字列のままでよい。
                if ui.button("終了 (Alt+F4)").clicked() {
                    action = Some(MenuAction::Exit);
                    ui.close();
                }
            });
            ui.menu_button("編集", |ui| {
                if ui
                    .add_enabled(
                        state.can_undo,
                        egui::Button::new(keymap::menu_label("元に戻す", Action::Undo)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Undo);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.can_redo,
                        egui::Button::new(keymap::menu_label("やり直し", Action::Redo)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Redo);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        state.has_selection,
                        egui::Button::new(keymap::menu_label("切り取り", Action::Cut)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Cut);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.has_selection,
                        egui::Button::new(keymap::menu_label("コピー", Action::Copy)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Copy);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("貼り付け", Action::Paste))
                    .clicked()
                {
                    action = Some(MenuAction::Paste);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.has_selection,
                        egui::Button::new(keymap::menu_label("削除", Action::Delete)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Delete);
                    ui.close();
                }
                ui.separator();
                if ui
                    .button(keymap::menu_label("すべて選択", Action::SelectAll))
                    .clicked()
                {
                    action = Some(MenuAction::SelectAll);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.has_selection,
                        egui::Button::new(keymap::menu_label("選択解除", Action::Deselect)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Deselect);
                    ui.close();
                }
            });
            ui.menu_button("画像", |ui| {
                if ui.button("画像サイズ変更…").clicked() {
                    action = Some(MenuAction::ImageResize);
                    ui.close();
                }
                if ui.button("キャンバスサイズ変更…").clicked() {
                    action = Some(MenuAction::CanvasResize);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.has_selection,
                        egui::Button::new("選択範囲でトリミング"),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Crop);
                    ui.close();
                }
                ui.separator();
                if ui.button("左右反転").clicked() {
                    action = Some(MenuAction::FlipHorizontal);
                    ui.close();
                }
                if ui.button("上下反転").clicked() {
                    action = Some(MenuAction::FlipVertical);
                    ui.close();
                }
                if ui.button("右に90°回転").clicked() {
                    action = Some(MenuAction::RotateCw);
                    ui.close();
                }
                if ui.button("左に90°回転").clicked() {
                    action = Some(MenuAction::RotateCcw);
                    ui.close();
                }
            });
            ui.menu_button("レイヤー", |ui| {
                if ui
                    .add_enabled(
                        state.can_add_layer,
                        egui::Button::new(keymap::menu_label("新規レイヤー", Action::LayerAdd)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerAdd);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.can_add_layer,
                        egui::Button::new(keymap::menu_label(
                            "レイヤーを複製",
                            Action::LayerDuplicate,
                        )),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerDuplicate);
                    ui.close();
                }
                if ui
                    .add_enabled(state.can_delete_layer, egui::Button::new("レイヤーを削除"))
                    .clicked()
                {
                    action = Some(MenuAction::LayerDelete);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(state.can_move_layer_up, egui::Button::new("上へ移動"))
                    .clicked()
                {
                    action = Some(MenuAction::LayerMoveUp);
                    ui.close();
                }
                if ui
                    .add_enabled(state.can_move_layer_down, egui::Button::new("下へ移動"))
                    .clicked()
                {
                    action = Some(MenuAction::LayerMoveDown);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        state.can_merge_layer_down,
                        egui::Button::new(keymap::menu_label(
                            "下のレイヤーと結合",
                            Action::LayerMergeDown,
                        )),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerMergeDown);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.can_flatten_layers,
                        egui::Button::new(keymap::menu_label("画像の統合", Action::LayerFlatten)),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerFlatten);
                    ui.close();
                }
            });
            ui.menu_button("表示", |ui| {
                if ui
                    .button(keymap::menu_label("拡大", Action::ZoomIn))
                    .clicked()
                {
                    action = Some(MenuAction::ZoomIn);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("縮小", Action::ZoomOut))
                    .clicked()
                {
                    action = Some(MenuAction::ZoomOut);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label("100%", Action::Zoom100))
                    .clicked()
                {
                    action = Some(MenuAction::Zoom100);
                    ui.close();
                }
                if ui
                    .button(keymap::menu_label(
                        "ウィンドウに合わせる",
                        Action::FitWindow,
                    ))
                    .clicked()
                {
                    action = Some(MenuAction::FitWindow);
                    ui.close();
                }
            });
        });
    });
    action
}
