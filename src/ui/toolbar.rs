//! 左縦ツールバー(SPEC §4 → v13 §60 のアイコンレール)。
//!
//! ボタンをクリックすると選択中のツールがハイライトされる。`show` は
//! クリックされた操作を返すだけで `ToolKind` を直接書き換えない: 選択ツール
//! (§7)から他のツールへ切り替える際に浮動片を確定させる必要があり、
//! `app.rs` がそのフックを一箇所(`set_tool`)に集約するため。
//!
//! v13 §60 で、アイコン+小さな文字ラベルの 84px 幅の列を、**アイコンだけの
//! 52px 幅のレール**に改めた(名前とショートカットはツールチップ)。ツールは
//! 用途ごとの 4 グループ(選択・移動 / 描画 / 図形・文字 / 補助)に区切り、
//! 下端に描画色・背景色のチップ(入れ替えボタン付き)と設定ボタンを固定する。
//! アイコン自体は従来どおり `icons::paint_tool_icon` のベクター描画。

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};

use crate::keymap::{self, Action};
use crate::tools::{LassoMode, ToolKind};
use crate::ui::{icons, theme, widgets};

/// クリックされた操作(まだ副作用は起こさない。`app.rs` が実行する。
/// `ui/menu.rs::MenuAction`/`ui/tab_bar.rs::TabBarAction` と同じ流儀)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    SelectTool(ToolKind),
    /// SPEC §34: 設定(環境設定)ダイアログを開く(Ctrl+K と同じ)。
    OpenPreferences,
    /// 描画色と背景色を入れ替える(X と同じ)。
    SwapColors,
}

struct ToolButton {
    name: &'static str,
    kind: ToolKind,
}

const fn t(name: &'static str, kind: ToolKind) -> ToolButton {
    ToolButton { name, kind }
}

/// SPEC §60: 用途ごとのグループ(グループ間に区切り線)。ショートカット表記は
/// ここに埋め込まず `keymap::tool_shortcut_label` から都度生成する
/// (ARCHITECTURE.md §15.4: 「ツールチップは KEYMAP から文字列生成」)。
const TOOL_GROUPS: &[&[ToolButton]] = &[
    &[
        t("移動", ToolKind::Move),
        t("矩形選択", ToolKind::Select),
        t("楕円選択", ToolKind::EllipseSelect),
        t("なげなわ", ToolKind::Lasso),
        t("自動選択", ToolKind::MagicWand),
        t("選択ブラシ", ToolKind::SelectBrush),
    ],
    &[
        t("ブラシ", ToolKind::Pen),
        t("消しゴム", ToolKind::Eraser),
        t("塗りつぶし", ToolKind::Fill),
        t("グラデーション", ToolKind::Gradient),
    ],
    &[
        t("直線", ToolKind::Line),
        t("矩形", ToolKind::Rect),
        t("楕円", ToolKind::Ellipse),
        t("テキスト", ToolKind::Text),
    ],
    &[
        t("スポイト", ToolKind::Picker),
        t("手のひら", ToolKind::Pan),
        t("ズーム", ToolKind::Zoom),
    ],
];

/// レールの幅(`app.rs` の左ドック配置にも効く)。
pub const RAIL_WIDTH: f32 = 52.0;
const BUTTON_W: f32 = 38.0;
const BUTTON_H: f32 = 34.0;
const ICON_SIZE: f32 = 19.0;
/// 下端の固定部(色チップ 38 + 設定 34 + 余白)。
const FOOTER_H: f32 = 38.0 + 6.0 + BUTTON_H + 8.0;
const CHIP: f32 = 22.0;

/// ツール名(オプションバーの見出し等で使う)。
pub fn tool_name(kind: ToolKind) -> &'static str {
    TOOL_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .find(|tool| tool.kind == kind)
        .map(|tool| tool.name)
        .unwrap_or("")
}

/// 現在のツール `current` を表示する。クリックされた操作があれば返す。
/// `lasso_mode` はなげなわのツールチップに現在のモード(自由/多角形)を
/// 出すためだけに使う(ARCHITECTURE.md §16.10-10)。
pub fn show(
    ui: &mut egui::Ui,
    current: ToolKind,
    lasso_mode: LassoMode,
    primary: Color32,
    secondary: Color32,
) -> Option<ToolbarAction> {
    let mut clicked = None;
    egui::Panel::left("tool_bar")
        .resizable(false)
        .exact_size(RAIL_WIDTH)
        .frame(egui::Frame::NONE.fill(theme::BG_PANEL))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.painter().vline(
                full.right() - 0.5,
                full.y_range(),
                Stroke::new(1.0, theme::LINE),
            );
            let footer_top = (full.bottom() - FOOTER_H).max(full.top());
            let tools_rect = Rect::from_min_max(full.min, pos2(full.right(), footer_top));
            let footer_rect = Rect::from_min_max(pos2(full.left(), footer_top), full.max);

            ui.scope_builder(egui::UiBuilder::new().max_rect(tools_rect), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("darask_tool_rail")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (index, group) in TOOL_GROUPS.iter().enumerate() {
                                if index > 0 {
                                    group_divider(ui);
                                }
                                for tool in *group {
                                    if let Some(action) = tool_button(ui, tool, current, lasso_mode)
                                    {
                                        clicked = Some(action);
                                    }
                                }
                            }
                        });
                    });
            });

            ui.scope_builder(egui::UiBuilder::new().max_rect(footer_rect), |ui| {
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if color_chips(ui, primary, secondary) {
                        clicked = Some(ToolbarAction::SwapColors);
                    }
                    let shortcut = keymap::label_for(Action::OpenPreferences);
                    let response = widgets::icon_button(
                        ui,
                        vec2(BUTTON_W, BUTTON_H),
                        18.0,
                        true,
                        false,
                        icons::paint_settings_icon,
                        "設定",
                    )
                    .on_hover_text(format!("設定 ({shortcut})"));
                    if response.clicked() {
                        clicked = Some(ToolbarAction::OpenPreferences);
                    }
                });
            });
        });
    clicked
}

fn group_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, theme::LINE_STRONG),
    );
}

fn tool_button(
    ui: &mut egui::Ui,
    tool: &ToolButton,
    current: ToolKind,
    lasso_mode: LassoMode,
) -> Option<ToolbarAction> {
    let selected = current == tool.kind;
    let (rect, response) = ui.allocate_exact_size(vec2(BUTTON_W, BUTTON_H), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, tool.name)
    });
    if ui.is_rect_visible(rect) {
        let (fill, stroke, icon) = widgets::button_colors(&response, true, selected);
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, 8.0, fill);
        }
        if let Some(stroke) = stroke {
            ui.painter()
                .rect_stroke(rect, 8.0, stroke, StrokeKind::Inside);
        }
        let icon_rect = Rect::from_center_size(rect.center(), vec2(ICON_SIZE, ICON_SIZE));
        icons::paint_tool_icon(tool.kind, ui.painter(), icon_rect, icon);
    }
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
    was_clicked.then_some(ToolbarAction::SelectTool(tool.kind))
}

/// 描画色(左上)と背景色(右下)のチップ+右上の入れ替えボタン。
/// 入れ替えが押されたら `true`。
fn color_chips(ui: &mut egui::Ui, primary: Color32, secondary: Color32) -> bool {
    let (outer, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
    let secondary_rect = Rect::from_min_size(outer.max - vec2(CHIP, CHIP), vec2(CHIP, CHIP));
    let primary_rect = Rect::from_min_size(outer.min, vec2(CHIP, CHIP));
    let swap_rect = Rect::from_min_size(pos2(outer.right() - 13.0, outer.top()), vec2(13.0, 13.0));
    if ui.is_rect_visible(outer) {
        let painter = ui.painter();
        paint_chip(painter, secondary_rect, secondary, false);
        paint_chip(painter, primary_rect, primary, true);
    }
    let primary_hover = ui.interact(
        primary_rect,
        ui.id().with("darask_chip_primary"),
        Sense::hover(),
    );
    primary_hover.on_hover_text(format!(
        "描画色 {}",
        crate::ui::color_panel::format_hex(primary)
    ));
    let secondary_hover = ui.interact(
        secondary_rect,
        ui.id().with("darask_chip_secondary"),
        Sense::hover(),
    );
    secondary_hover.on_hover_text(format!(
        "背景色 {}",
        crate::ui::color_panel::format_hex(secondary)
    ));
    let swap = ui.interact(swap_rect, ui.id().with("darask_chip_swap"), Sense::click());
    if ui.is_rect_visible(swap_rect) {
        let color = if swap.hovered() {
            Color32::WHITE
        } else {
            theme::TEXT_FAINT
        };
        icons::paint_swap_icon(ui.painter(), swap_rect, color);
    }
    let shortcut = keymap::label_for(Action::SwapColors);
    swap.on_hover_text(format!("描画色と背景色を入れ替え ({shortcut})"))
        .clicked()
}

fn paint_chip(painter: &egui::Painter, rect: Rect, color: Color32, front: bool) {
    if color.a() < 255 {
        let half = rect.size() / 2.0;
        painter.rect_filled(rect, 5.0, Color32::from_gray(205));
        for (ix, iy) in [(0.0, 0.0), (1.0, 1.0)] {
            let min = rect.min + vec2(ix * half.x, iy * half.y);
            painter.rect_filled(Rect::from_min_size(min, half), 0.0, Color32::from_gray(160));
        }
    }
    painter.rect_filled(rect, 5.0, color);
    if front {
        painter.rect_stroke(
            rect,
            5.0,
            Stroke::new(2.0, theme::BG_PANEL),
            StrokeKind::Outside,
        );
        painter.rect_stroke(
            rect.expand(2.0),
            6.0,
            Stroke::new(1.0, Color32::from_rgb(0x4a, 0x4a, 0x52)),
            StrokeKind::Outside,
        );
    } else {
        painter.rect_stroke(
            rect,
            5.0,
            Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3a, 0x40)),
            StrokeKind::Outside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_appears_once_across_the_groups() {
        let all: Vec<&ToolButton> = TOOL_GROUPS.iter().flat_map(|g| g.iter()).collect();
        // v12 §51.2 で選択ブラシが加わり 17 個。
        assert_eq!(all.len(), 17);
        for (index, tool) in all.iter().enumerate() {
            assert!(!tool.name.is_empty());
            assert!(all[..index].iter().all(|other| other.kind != tool.kind));
        }
        assert!(TOOL_GROUPS.iter().all(|group| !group.is_empty()));
    }

    #[test]
    fn tool_name_looks_up_the_rail_entry() {
        assert_eq!(tool_name(ToolKind::Pen), "ブラシ");
        assert_eq!(tool_name(ToolKind::SelectBrush), "選択ブラシ");
    }
}
