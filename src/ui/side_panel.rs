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
//!
//! v6-M3(SPEC §35、ARCHITECTURE.md §18.4): 「色」「レイヤー」に続く 3 番目の
//! セクションとして履歴パネル(`history_panel`)を追加した。アクティブタブの
//! `History` は呼び出し側(`app.rs`)がタブ切替のたびに渡し直すだけなので、
//! ここで特別な追随処理は不要(`history_panel` のドキュメントコメント参照)。

use eframe::egui;

use crate::document::Document;
use crate::history::History;
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::history_panel;
use crate::ui::layers_panel::{self, LayersPanelAction, RenameState};

/// SPEC §3: 「右パネルは固定幅 約210px」。
const SIDE_PANEL_WIDTH: f32 = 210.0;

/// 右パネル全体を描画する。レイヤー操作(構造を変える、または「先に確定」
/// が必要なもの)があれば `LayersPanelAction` を、履歴パネルの行クリックが
/// あれば `History::jump_to` にそのまま渡せる目標 `undo_stack` 長を返す
/// (どちらも実際の `Document`/`History` 操作は呼び出し側 `app.rs` が行う、
/// `layers_panel`/`history_panel` のドキュメントコメント参照)。
pub fn show(
    ui: &mut egui::Ui,
    doc: &mut Document,
    rename: &mut RenameState,
    history: &History,
    color_ctx: ColorPanelCtx,
) -> (Option<LayersPanelAction>, Option<usize>) {
    let mut layer_action = None;
    let mut history_jump = None;
    egui::Panel::right("side_panel")
        .resizable(false)
        .exact_size(SIDE_PANEL_WIDTH)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    color_panel::show(ui, color_ctx);
                    ui.separator();
                    layer_action = layers_panel::show(ui, doc, rename);
                    ui.separator();
                    history_jump = history_panel::show(ui, history);
                });
        });
    (layer_action, history_jump)
}
