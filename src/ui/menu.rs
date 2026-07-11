//! メニューバー(SPEC §7)。
//!
//! M1 では日本語ラベルでのレイアウトのみ実装していた。M4 で実際のクリック
//! 処理を配線する: 各項目は `MenuAction` を返し、`app.rs` が実行する
//! (rfd のネイティブダイアログ呼び出しはフレーム処理の外側で行う必要が
//! あるため、ここではまだ実行せず「何を要求されたか」だけを返す、
//! ARCHITECTURE.md §12-9)。有効/無効(元に戻す・やり直し・選択系・
//! トリミング)は `MenuState` で受け取る。

use eframe::egui;

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
                if ui.button("新規 (Ctrl+N)").clicked() {
                    action = Some(MenuAction::New);
                    ui.close();
                }
                if ui.button("開く (Ctrl+O)").clicked() {
                    action = Some(MenuAction::Open);
                    ui.close();
                }
                if ui.button("上書き保存 (Ctrl+S)").clicked() {
                    action = Some(MenuAction::Save);
                    ui.close();
                }
                if ui.button("名前を付けて保存 (Ctrl+Shift+S)").clicked() {
                    action = Some(MenuAction::SaveAs);
                    ui.close();
                }
                ui.separator();
                if ui.button("終了 (Alt+F4)").clicked() {
                    action = Some(MenuAction::Exit);
                    ui.close();
                }
            });
            ui.menu_button("編集", |ui| {
                if ui
                    .add_enabled(state.can_undo, egui::Button::new("元に戻す (Ctrl+Z)"))
                    .clicked()
                {
                    action = Some(MenuAction::Undo);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.can_redo,
                        egui::Button::new("やり直し (Ctrl+Y, Ctrl+Shift+Z)"),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::Redo);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(state.has_selection, egui::Button::new("切り取り (Ctrl+X)"))
                    .clicked()
                {
                    action = Some(MenuAction::Cut);
                    ui.close();
                }
                if ui
                    .add_enabled(state.has_selection, egui::Button::new("コピー (Ctrl+C)"))
                    .clicked()
                {
                    action = Some(MenuAction::Copy);
                    ui.close();
                }
                if ui.button("貼り付け (Ctrl+V)").clicked() {
                    action = Some(MenuAction::Paste);
                    ui.close();
                }
                if ui
                    .add_enabled(state.has_selection, egui::Button::new("削除 (Delete)"))
                    .clicked()
                {
                    action = Some(MenuAction::Delete);
                    ui.close();
                }
                ui.separator();
                if ui.button("すべて選択 (Ctrl+A)").clicked() {
                    action = Some(MenuAction::SelectAll);
                    ui.close();
                }
                if ui
                    .add_enabled(state.has_selection, egui::Button::new("選択解除 (Ctrl+D)"))
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
                        egui::Button::new("新規レイヤー (Ctrl+Shift+N)"),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerAdd);
                    ui.close();
                }
                if ui
                    .add_enabled(state.can_add_layer, egui::Button::new("レイヤーを複製"))
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
                        egui::Button::new("下のレイヤーと結合 (Ctrl+E)"),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerMergeDown);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        state.can_flatten_layers,
                        egui::Button::new("画像の統合 (Ctrl+Shift+E)"),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::LayerFlatten);
                    ui.close();
                }
            });
            ui.menu_button("表示", |ui| {
                if ui.button("拡大 (Ctrl++)").clicked() {
                    action = Some(MenuAction::ZoomIn);
                    ui.close();
                }
                if ui.button("縮小 (Ctrl+-)").clicked() {
                    action = Some(MenuAction::ZoomOut);
                    ui.close();
                }
                if ui.button("100% (Ctrl+1)").clicked() {
                    action = Some(MenuAction::Zoom100);
                    ui.close();
                }
                if ui.button("ウィンドウに合わせる (Ctrl+0)").clicked() {
                    action = Some(MenuAction::FitWindow);
                    ui.close();
                }
            });
        });
    });
    action
}
