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

use crate::tools::ToolKind;
use crate::ui::icons;

struct ToolButton {
    name: &'static str,
    shortcut: &'static str,
    kind: ToolKind,
}

const TOOLS: &[ToolButton] = &[
    ToolButton {
        name: "ペン",
        shortcut: "B",
        kind: ToolKind::Pen,
    },
    ToolButton {
        name: "消しゴム",
        shortcut: "E",
        kind: ToolKind::Eraser,
    },
    ToolButton {
        name: "直線",
        shortcut: "L",
        kind: ToolKind::Line,
    },
    ToolButton {
        name: "矩形",
        shortcut: "R",
        kind: ToolKind::Rect,
    },
    ToolButton {
        name: "楕円",
        shortcut: "C",
        kind: ToolKind::Ellipse,
    },
    ToolButton {
        name: "塗りつぶし",
        shortcut: "F",
        kind: ToolKind::Fill,
    },
    ToolButton {
        name: "スポイト",
        shortcut: "I",
        kind: ToolKind::Picker,
    },
    ToolButton {
        name: "選択",
        shortcut: "M",
        kind: ToolKind::Select,
    },
    ToolButton {
        name: "手のひら",
        shortcut: "H",
        kind: ToolKind::Pan,
    },
];

/// ボタン全体のサイズ(論理ポイント)。アイコン本体は `ICON_MARGIN` だけ
/// 内側に収める(SPEC §15: 「約 20×20」のアイコンを 30×30 のボタンに乗せる)。
const BUTTON_SIZE: f32 = 30.0;
const ICON_MARGIN: f32 = 5.0;

/// 現在のツール `current` を表示する。クリックされたツールがあれば返す
/// (`ToolKind` はここでは書き換えない、上記コメント参照)。
pub fn show(ui: &mut egui::Ui, current: ToolKind) -> Option<ToolKind> {
    let mut clicked = None;
    egui::Panel::left("tool_bar")
        .resizable(false)
        .exact_size(40.0)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                for tool in TOOLS {
                    let selected = current == tool.kind;
                    let response = tool_button(ui, tool.kind, selected)
                        .on_hover_text(format!("{} ({})", tool.name, tool.shortcut));
                    if response.clicked() {
                        clicked = Some(tool.kind);
                    }
                }
            });
        });
    clicked
}

/// SPEC §15 / ARCHITECTURE.md §14.4: hover/selected の背景を自前で塗って
/// から `icons::paint_tool_icon` を重ねる(egui `Button` の text 描画 API に
/// 依存しない)。色は egui のテキスト色に追従し、選択中はアクセント色になる
/// (`Style::interact_selectable` が `Button` の内部実装と同じ規則で解決する)。
fn tool_button(ui: &mut egui::Ui, kind: ToolKind, selected: bool) -> egui::Response {
    let size = egui::vec2(BUTTON_SIZE, BUTTON_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_rect = rect.shrink(ICON_MARGIN);
        icons::paint_tool_icon(kind, ui.painter(), icon_rect, visuals.fg_stroke.color);
    }
    response
}
