//! 右パネルの枠(SPEC §3: 画面構成、固定幅 約 210px)。
//!
//! ARCHITECTURE.md §14.7: 「色(§14)+レイヤー(§13)を縦に配置」。v2 M3 で
//! 色パネル(`color_panel`)を追加し、レイヤーパネルの上に並べた。
//! ARCHITECTURE.md §14.9-7: 「パネル追加で `CentralPanel` より先に右パネル
//! を show する」ため、`app.rs` は `CentralPanel::show` より前にこれを
//! 呼ぶこと。
//!
//! 色パネル(ホイール・パレット等)とレイヤーパネルを縦に並べると
//! 既定のウィンドウ高さ(800px、最小 480px)を超えうるため、全体を
//! 縦スクロール領域に収める(レイヤー一覧自体も内部に独自のスクロール
//! 領域を持つが、egui はネストしたスクロール領域を問題なく扱える)。

use eframe::egui;

use crate::document::Document;
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::layers_panel::{self, LayersPanelAction, RenameState};

/// SPEC §3: 「右パネルは固定幅 約210px」。
const SIDE_PANEL_WIDTH: f32 = 210.0;

pub fn show(
    ui: &mut egui::Ui,
    doc: &mut Document,
    rename: &mut RenameState,
    color_ctx: ColorPanelCtx,
) -> Option<LayersPanelAction> {
    let mut action = None;
    egui::Panel::right("side_panel")
        .resizable(false)
        .exact_size(SIDE_PANEL_WIDTH)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    color_panel::show(ui, color_ctx);
                    ui.separator();
                    action = layers_panel::show(ui, doc, rename);
                });
        });
    action
}
