//! 右パネルの枠(SPEC §3: 画面構成、固定幅 約 210px)。
//!
//! ARCHITECTURE.md §14.7: 「色(§14)+レイヤー(§13)を縦に配置」。v2 M3 で
//! 色パネル(`color_panel`)を追加し、レイヤーパネルの上に並べた。
//! ARCHITECTURE.md §14.9-7: 「パネル追加で `CentralPanel` より先に右パネル
//! を show する」ため、`app.rs` は `CentralPanel::show` より前にこれを
//! 呼ぶこと。
//!
//! v6-M3(SPEC §35、ARCHITECTURE.md §18.4): 「色」「レイヤー」に続く 3 番目の
//! セクションとして履歴パネル(`history_panel`)を追加した。アクティブタブの
//! `History` は呼び出し側(`app.rs`)がタブ切替のたびに渡し直すだけなので、
//! ここで特別な追随処理は不要(`history_panel` のドキュメントコメント参照)。
//!
//! v12 §50.1 で 3 セクションを **`CollapsingHeader`** にし(レイヤーは既定で
//! 展開)、スクロールをパネル全体の 1 本に集約した: 以前は「パネル全体の
//! `ScrollArea`」の中に「レイヤー一覧(max_height 180px)」と「履歴
//! (max_height 140px)」が独自のスクロール領域を持っており、狭い画面では
//! スクロールバーが 3 本入れ子になって目的の行へ辿り着けなかった。
//! 内側の固定高さを廃止したことで、折りたたんだセクションの高さが
//! そのまま残りのセクション(=展開中のレイヤー一覧)に回る。

use eframe::egui;

use crate::document::Document;
use crate::history::History;
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::history_panel;
use crate::ui::layers_panel::{
    self, LayersPanelAction, LayersPanelCtx, RenameState, ThumbnailCache,
};

/// SPEC §3: 「右パネルは固定幅 約210px」。
const SIDE_PANEL_WIDTH: f32 = 210.0;

/// `show` に渡すアクティブタブ由来の状態(`Tab` の disjoint なフィールドを
/// そのまま束ねたもの — `app.rs` 側で `&mut Tab` を 1 回だけ借りて渡す)。
pub struct SidePanelCtx<'a> {
    pub doc: &'a Document,
    pub rename: &'a mut RenameState,
    pub thumbnails: &'a mut ThumbnailCache,
    pub history: &'a History,
}

/// 右パネル全体を描画する。レイヤー操作(構造を変える、または「先に確定」
/// が必要なもの)があれば `LayersPanelAction` を、履歴パネルの行クリックが
/// あれば `History::jump_to` にそのまま渡せる目標 `undo_stack` 長を返す
/// (どちらも実際の `Document`/`History` 操作は呼び出し側 `app.rs` が行う、
/// `layers_panel`/`history_panel` のドキュメントコメント参照)。
pub fn show(
    ui: &mut egui::Ui,
    tab: SidePanelCtx<'_>,
    color_ctx: ColorPanelCtx,
) -> (Option<LayersPanelAction>, Option<usize>) {
    let SidePanelCtx {
        doc,
        rename,
        thumbnails,
        history,
    } = tab;
    let mut layer_action = None;
    let mut history_jump = None;
    egui::Panel::right("side_panel")
        .resizable(false)
        .exact_size(SIDE_PANEL_WIDTH)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::CollapsingHeader::new("色")
                        .default_open(true)
                        .show(ui, |ui| color_panel::show(ui, color_ctx));
                    // SPEC §50.1: レイヤーは既定で展開。
                    egui::CollapsingHeader::new("レイヤー")
                        .default_open(true)
                        .show(ui, |ui| {
                            layer_action = layers_panel::show(
                                ui,
                                LayersPanelCtx {
                                    doc,
                                    rename,
                                    thumbnails,
                                },
                            );
                        });
                    egui::CollapsingHeader::new("履歴")
                        .default_open(true)
                        .show(ui, |ui| {
                            history_jump = history_panel::show(ui, history);
                        });
                });
        });
    (layer_action, history_jump)
}
