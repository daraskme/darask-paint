//! 左縦ツールバー(SPEC §4)。
//!
//! ボタンをクリックすると選択中のツールがハイライトされる。M4 で全ツールの
//! 挙動が出揃った。`show` はクリックされたツールを返すだけで `ToolKind` を
//! 直接書き換えない: 選択ツール(§7)から他のツールへ切り替える際に浮動片を
//! 確定させる必要があり、`app.rs` がそのフックを一箇所(`set_tool`)に
//! 集約するため。
//!
//! v2(SPEC §15、ARCHITECTURE.md §14.4)で漢字 1 文字のボタンを
//! `icons::paint_tool_icon` によるベクター描画へ置き換えた。egui の
//! `Button` はテキスト用の API を前提にしているため使わず、
//! `ui.allocate_exact_size` でボタン領域を確保し、hover/selected の背景を
//! 自前で塗ってからアイコンを重ねる(ARCHITECTURE.md §14.4)。

use eframe::egui;

use crate::keymap::{self, Action};
use crate::tools::{LassoMode, ToolKind};
use crate::ui::icons;

/// クリックされた操作(まだ副作用は起こさない。`app.rs` が実行する。
/// `ui/menu.rs::MenuAction`/`ui/tab_bar.rs::TabBarAction` と同じ流儀)。
///
/// v6 §33/§34(ARCHITECTURE.md §18.2): 設定(歯車)ボタンは `ToolKind` を
/// 持たない(ツールを切り替えるのではなくモーダルを開くだけ)ため、従来の
/// `Option<ToolKind>` では表現できず、この列挙体を新設した。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    SelectTool(ToolKind),
    /// SPEC §34: 設定(環境設定)ダイアログを開く(Ctrl+K と同じ)。
    OpenPreferences,
}

struct ToolButton {
    name: &'static str,
    label: &'static str,
    kind: ToolKind,
}

/// ショートカット表記はここに埋め込まず、`keymap::tool_shortcut_label`
/// (`keymap::KEYMAP` が唯一の情報源)から都度生成する(ARCHITECTURE.md
/// §15.4: 「メニュー表記・ツールチップは KEYMAP から文字列生成」)。SPEC §20
/// で直線/矩形/楕円が「U」1 本にまとめられた(旧 L/R/C は廃止)ことも、
/// `tool_shortcut_label` が吸収するのでここでは意識しなくてよい。
const TOOLS: &[ToolButton] = &[
    ToolButton {
        name: "ブラシ",
        label: "ブラシ",
        kind: ToolKind::Pen,
    },
    ToolButton {
        name: "消しゴム",
        label: "消しゴム",
        kind: ToolKind::Eraser,
    },
    ToolButton {
        name: "直線",
        label: "直線",
        kind: ToolKind::Line,
    },
    ToolButton {
        name: "矩形",
        label: "矩形",
        kind: ToolKind::Rect,
    },
    ToolButton {
        name: "楕円",
        label: "楕円",
        kind: ToolKind::Ellipse,
    },
    ToolButton {
        name: "塗りつぶし",
        label: "塗りつぶし",
        kind: ToolKind::Fill,
    },
    // v4 §23: グラデーション(Shift+G で塗りつぶしと巡回する仲間なので、
    // 塗りつぶしのすぐ後に置く)。
    ToolButton {
        name: "グラデーション",
        label: "グラデ",
        kind: ToolKind::Gradient,
    },
    ToolButton {
        name: "スポイト",
        label: "スポイト",
        kind: ToolKind::Picker,
    },
    // v3 §19: テキスト。SPEC §20 最終キーマップの並び(…I, T, U…)に合わせ、
    // スポイトの直後・選択の前に置く(ツールバー全体の PS 準拠キーマップへの
    // 総入れ替えは V3-M4 のスコープ、ARCHITECTURE.md §15.5)。
    ToolButton {
        name: "テキスト",
        label: "テキスト",
        kind: ToolKind::Text,
    },
    ToolButton {
        name: "矩形選択",
        label: "矩形選択",
        kind: ToolKind::Select,
    },
    // v4 §22: 楕円選択・なげなわ・自動選択。矩形選択(Shift+M で巡回する
    // 仲間)のすぐ後に置く。
    ToolButton {
        name: "楕円選択",
        label: "楕円選択",
        kind: ToolKind::EllipseSelect,
    },
    ToolButton {
        name: "なげなわ",
        label: "なげなわ",
        kind: ToolKind::Lasso,
    },
    ToolButton {
        name: "自動選択",
        label: "自動選択",
        kind: ToolKind::MagicWand,
    },
    // v3 §18: 移動・ズーム。選択(浮動化の仲間)の直後、手のひら(表示操作の
    // 仲間)の後にそれぞれ加える。
    ToolButton {
        name: "移動",
        label: "移動",
        kind: ToolKind::Move,
    },
    ToolButton {
        name: "手のひら",
        label: "手のひら",
        kind: ToolKind::Pan,
    },
    ToolButton {
        name: "ズーム",
        label: "ズーム",
        kind: ToolKind::Zoom,
    },
];

/// ボタン全体のサイズ(論理ポイント)。アイコン本体は `ICON_MARGIN` だけ
/// 内側に収める(SPEC §15: 「約 20×20」のアイコンを 30×30 のボタンに乗せる)。
const BUTTON_W: f32 = 50.0;
const BUTTON_H: f32 = 42.0;
const ICON_SIZE: f32 = 18.0;
const LABEL_SIZE: f32 = 9.0;

/// 現在のツール `current` を表示する。クリックされた操作があれば返す
/// (`ToolKind` はここでは書き換えない、上記コメント参照)。`lasso_mode` は
/// なげなわのツールチップに現在のモード(自由/多角形)を出すためだけに使う
/// (ARCHITECTURE.md §16.10-10: 「巡回系はツールバー…の整合を忘れない」)。
///
/// v6 §33/§34(ARCHITECTURE.md §18.2): ツール一覧の下に区切り線を挟んで
/// 設定(歯車)ボタンを 1 つ追加した(「ツールバーにも歯車アイコンのボタンを
/// 1 つ」)。M4 のメニュー全展開アイコン化(SPEC §33)より先行するため、
/// ここではこのボタン単体だけを追加する(メニュー全体の再設計はスコープ外)。
pub fn show(ui: &mut egui::Ui, current: ToolKind, lasso_mode: LassoMode) -> Option<ToolbarAction> {
    let mut clicked = None;
    egui::Panel::left("tool_bar")
        .resizable(false)
        .exact_size(84.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        for tool in TOOLS {
                            let selected = current == tool.kind;
                            let response = tool_button(ui, tool, selected);
                            let was_clicked = response.clicked();
                            if response.hovered() {
                                let shortcut = keymap::tool_shortcut_label(tool.kind);
                                let tooltip = if tool.kind == ToolKind::Lasso {
                                    format!("{}({}) ({shortcut})", tool.name, lasso_mode.label())
                                } else {
                                    format!("{} ({shortcut})", tool.name)
                                };
                                response.on_hover_text(tooltip);
                            }
                            if was_clicked {
                                clicked = Some(ToolbarAction::SelectTool(tool.kind));
                            }
                        }
                        ui.separator();
                        let response = settings_button(ui);
                        let was_clicked = response.clicked();
                        if response.hovered() {
                            let shortcut = keymap::label_for(Action::OpenPreferences);
                            response.on_hover_text(format!("設定 ({shortcut})"));
                        }
                        if was_clicked {
                            clicked = Some(ToolbarAction::OpenPreferences);
                        }
                    });
                });
        });
    clicked
}

/// SPEC §15 / ARCHITECTURE.md §14.4: hover/selected の背景を自前で塗って
/// から `icons::paint_tool_icon` を重ねる(egui `Button` の text 描画 API に
/// 依存しない)。色は egui のテキスト色に追従し、選択中はアクセント色になる
/// (`Style::interact_selectable` が `Button` の内部実装と同じ規則で解決する)。
fn tool_button(ui: &mut egui::Ui, tool: &ToolButton, selected: bool) -> egui::Response {
    let size = egui::vec2(BUTTON_W, BUTTON_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, tool.name)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_center = egui::pos2(rect.center().x, rect.top() + 12.0);
        let icon_rect = egui::Rect::from_center_size(icon_center, egui::vec2(ICON_SIZE, ICON_SIZE));
        icons::paint_tool_icon(tool.kind, ui.painter(), icon_rect, visuals.fg_stroke.color);
        ui.painter().with_clip_rect(rect.shrink(1.0)).text(
            egui::pos2(rect.center().x, rect.bottom() - 3.0),
            egui::Align2::CENTER_BOTTOM,
            tool.label,
            egui::FontId::proportional(LABEL_SIZE),
            visuals.fg_stroke.color,
        );
    }
    response
}

/// SPEC §34(ARCHITECTURE.md §18.2): 設定ボタン。`tool_button` と同じ
/// hover 背景の描き方だが、押しっぱなしのトグル状態(`selected`)を持たない
/// (モーダルを開くだけの単発アクション、`ui/menu.rs` の各項目と同じ扱い)。
fn settings_button(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(BUTTON_W, BUTTON_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "設定"));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_center = egui::pos2(rect.center().x, rect.top() + 12.0);
        let icon_rect = egui::Rect::from_center_size(icon_center, egui::vec2(ICON_SIZE, ICON_SIZE));
        icons::paint_settings_icon(ui.painter(), icon_rect, visuals.fg_stroke.color);
        ui.painter().with_clip_rect(rect.shrink(1.0)).text(
            egui::pos2(rect.center().x, rect.bottom() - 3.0),
            egui::Align2::CENTER_BOTTOM,
            "設定",
            egui::FontId::proportional(LABEL_SIZE),
            visuals.fg_stroke.color,
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_short_visible_label_and_no_kind_is_duplicated() {
        assert_eq!(TOOLS.len(), 16);
        for (index, tool) in TOOLS.iter().enumerate() {
            assert!(!tool.label.is_empty());
            assert!(tool.label.chars().count() <= 5, "{}", tool.label);
            assert!(TOOLS[..index].iter().all(|other| other.kind != tool.kind));
        }
    }
}
