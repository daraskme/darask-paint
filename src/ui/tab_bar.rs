//! タブバー(SPEC §30、ARCHITECTURE.md §17.2/§17.3/§17.7 V5-M2)。
//!
//! メニューバーの直下・ツールバー/オプションバーの上に置く横 1 列のタブ帯
//! (横スクロール、多数タブでも折り返さない)。各タブはファイル名(無題なら
//! 「無題」「無題2」…と連番、`app.rs::Tab::label` が算出する)+ 未保存
//! インジケータ(`*`)+ 閉じるボタン(×)を表示する。クリックでアクティブ化、
//! 中クリックでも閉じる(SPEC §30)。
//!
//! 他の `ui/*` パネルと同じく、実際のタブ切替・オープン・クローズは
//! `app.rs` 側の唯一の入口(`switch_tab`/`close_tab`、ARCHITECTURE.md §17.3:
//! 「タブ切替前に必ず commit_open_gesture() を呼ぶ」)を経由する必要がある
//! ため、ここでは [`TabBarAction`] を返すだけに留める。

use eframe::egui::{self, vec2, Color32, CornerRadius, Rect, Sense};

use crate::ui::{icons, theme, widgets};

/// クリックされた操作(まだ副作用は起こさない。`app.rs` が実行する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarAction {
    Activate(usize),
    Close(usize),
    /// v13 §60: タブ列の右の「+」(Ctrl+N と同じ「新規」)。
    New,
}

/// 1 タブぶんの表示情報(所有権は `app.rs` 側、ここでは借用のみ)。
pub struct TabInfo {
    pub label: String,
    /// SPEC §30: 「未保存インジケータ(`*`)」(`doc.modified` と同じ)。
    pub modified: bool,
}

const TAB_BAR_HEIGHT: f32 = 34.0;
const TAB_H: f32 = 30.0;
const CLOSE: f32 = 18.0;

/// 直近フレームでどのタブがアクティブだったかを覚えておくための egui
/// メモリキー(`IdTypeMap`、`ui/*` の他パネルと同じく `app.rs` に新しい
/// フィールドを増やさず自己完結させる)。バグ修正: 以前はここで一切
/// スクロール位置を追従させておらず、`ScrollArea::horizontal()` に包む
/// だけだったため、タブを大量に開いてタブバーが横スクロール状態のとき
/// Ctrl+Tab・Ctrl+Shift+Tab・タブバークリック・新規タブ追加でアクティブ
/// タブが変わっても、スクロール位置が追従せずアクティブタブが画面外に
/// 隠れたままになっていた。
const LAST_ACTIVE_KEY: &str = "darask_tab_bar_last_active";

/// タブバーを描画する。クリックされた操作があれば返す(複数同時クリックは
/// 起こらないので `Option` でよい、`ui/menu.rs::show` と同じ流儀)。
pub fn show(ui: &mut egui::Ui, tabs: &[TabInfo], active: usize) -> Option<TabBarAction> {
    let mut action = None;
    let id = egui::Id::new(LAST_ACTIVE_KEY);
    // アクティブタブが前フレームから変わった(=切替操作・新規タブ追加が
    // 起きた)フレームでだけスクロールを追従させる。毎フレーム無条件に
    // 追従させると、ユーザーが他のタブを見ようとタブバーを手動スクロール
    // した直後にアクティブタブの位置へ強制的に引き戻されてしまう。
    let last_active: Option<usize> = ui.ctx().data(|d| d.get_temp(id));
    let just_switched = should_scroll_active_into_view(last_active, active);
    ui.ctx().data_mut(|d| d.insert_temp(id, active));
    egui::Panel::top("tab_bar")
        .exact_size(TAB_BAR_HEIGHT)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG_APP)
                .inner_margin(egui::Margin {
                    left: 8,
                    right: 8,
                    top: 4,
                    bottom: 0,
                }),
        )
        .show(ui, |ui| {
            // SPEC §30: 「横1列、水平スクロール、多数タブでも折り返さない」。
            egui::ScrollArea::horizontal()
                .auto_shrink([false, true])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (index, tab) in tabs.iter().enumerate() {
                            show_tab(ui, index, tab, index == active, just_switched, &mut action);
                        }
                        ui.add_space(2.0);
                        let response = widgets::icon_button(
                            ui,
                            vec2(26.0, 26.0),
                            13.0,
                            true,
                            false,
                            icons::paint_plus_icon,
                            "新規",
                        )
                        .on_hover_text(crate::keymap::menu_label(
                            "新規",
                            crate::keymap::Action::New,
                        ));
                        if response.clicked() {
                            action = Some(TabBarAction::New);
                        }
                    });
                });
        });
    action
}

/// 1 タブ = [未保存の点][ファイル名][×]。アクティブなタブはパネルと同じ面で
/// 塗り、下のオプションバーとつながって見えるようにする。
fn show_tab(
    ui: &mut egui::Ui,
    index: usize,
    tab: &TabInfo,
    is_active: bool,
    just_switched: bool,
    action: &mut Option<TabBarAction>,
) {
    let galley = ui.painter().layout_no_wrap(
        tab.label.clone(),
        egui::FontId::proportional(12.5),
        theme::TEXT,
    );
    let dot_w = if tab.modified { 13.0 } else { 0.0 };
    let width = 12.0 + dot_w + galley.size().x + 8.0 + CLOSE + 6.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, TAB_H), Sense::click());
    let close_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 6.0 - CLOSE / 2.0, rect.center().y),
        vec2(CLOSE, CLOSE),
    );
    let close = ui.interact(
        close_rect,
        ui.id().with(("darask_tab_close", index)),
        Sense::click(),
    );
    // SPEC §30: 「ファイル名 + 未保存インジケータ」。
    let star = if tab.modified { "(未保存)" } else { "" };
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            is_active,
            format!("{}{star}", tab.label),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = CornerRadius {
            nw: 8,
            ne: 8,
            sw: 0,
            se: 0,
        };
        let text_color = if is_active {
            Color32::from_rgb(0xf2, 0xf2, 0xf4)
        } else if response.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_FAINT
        };
        if is_active {
            painter.rect_filled(rect, radius, theme::BG_PANEL);
        } else if response.hovered() {
            painter.rect_filled(rect, radius, Color32::from_rgb(0x1c, 0x1c, 0x1f));
        }
        let mut x = rect.left() + 12.0;
        if tab.modified {
            painter.circle_filled(egui::pos2(x + 3.5, rect.center().y), 3.5, theme::ACCENT);
            x += dot_w;
        }
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
        if is_active || response.hovered() || close.hovered() {
            let color = if close.hovered() {
                painter.rect_filled(close_rect, 4.0, theme::BG_HOVER);
                Color32::WHITE
            } else {
                theme::TEXT_FAINT
            };
            icons::paint_close_icon(painter, close_rect.shrink(3.0), color);
        }
    }
    let response = response.on_hover_text(format!(
        "{}{star}\nクリックでアクティブ化、中クリックで閉じる",
        tab.label
    ));
    // アクティブタブが画面外に隠れたままにならないよう、切替直後の
    // フレームだけタブバーのスクロール位置を追従させる。
    if is_active && just_switched {
        response.scroll_to_me(Some(egui::Align::Center));
    }
    // SPEC §30: 「閉じるボタン(×)」「中クリックでも閉じる」。
    if close.on_hover_text("タブを閉じる").clicked() || response.middle_clicked() {
        *action = Some(TabBarAction::Close(index));
    } else if response.clicked() {
        *action = Some(TabBarAction::Activate(index));
    }
}

/// タブバーのスクロール追従が必要か(純関数、テスト可能)。「前フレームの
/// アクティブタブ」と「今のアクティブタブ」が違うフレームでだけ真になる。
/// 常に真を返すと、ユーザーが他のタブを見ようとタブバーを手動スクロール
/// した直後に強制的にアクティブタブへ引き戻されてしまう。
fn should_scroll_active_into_view(last_active: Option<usize>, active: usize) -> bool {
    last_active != Some(active)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回帰テスト: バグ修正前はタブバーがアクティブタブの位置へ一切
    /// スクロール追従しなかった(`grep -rn "scroll_to" src/` がヒット0件
    /// だった)。この判定関数がタブ切替を正しく検出することを担保する。
    #[test]
    fn detects_a_switch_but_not_a_repeat_of_the_same_active_tab() {
        // 初回フレーム(前フレーム情報がまだ無い)は追従させる
        // (起動直後・タブバー初描画で問題は起きないが、一貫性のため)。
        assert!(should_scroll_active_into_view(None, 0));
        // 同じアクティブタブのままの後続フレームは追従させない
        // (手動スクロールを毎フレーム引き戻してしまうバグを防ぐ)。
        assert!(!should_scroll_active_into_view(Some(0), 0));
        // Ctrl+Tab/Ctrl+Shift+Tab・タブバークリック・新規タブ追加で
        // アクティブタブが変わったフレームは追従させる。
        assert!(should_scroll_active_into_view(Some(0), 1));
        assert!(should_scroll_active_into_view(Some(5), 0));
    }
}
