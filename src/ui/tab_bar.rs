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

use eframe::egui;

/// クリックされた操作(まだ副作用は起こさない。`app.rs` が実行する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarAction {
    Activate(usize),
    Close(usize),
}

/// 1 タブぶんの表示情報(所有権は `app.rs` 側、ここでは借用のみ)。
pub struct TabInfo {
    pub label: String,
    /// SPEC §30: 「未保存インジケータ(`*`)」(`doc.modified` と同じ)。
    pub modified: bool,
}

const TAB_BAR_HEIGHT: f32 = 26.0;

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
        .show(ui, |ui| {
            // SPEC §30: 「横1列、水平スクロール、多数タブでも折り返さない」。
            egui::ScrollArea::horizontal()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, tab) in tabs.iter().enumerate() {
                            show_tab(ui, index, tab, index == active, just_switched, &mut action);
                        }
                    });
                });
        });
    action
}

fn show_tab(
    ui: &mut egui::Ui,
    index: usize,
    tab: &TabInfo,
    is_active: bool,
    just_switched: bool,
    action: &mut Option<TabBarAction>,
) {
    ui.horizontal(|ui| {
        // SPEC §30: 「ファイル名 + 未保存インジケータ(*)」。
        let star = if tab.modified { "*" } else { "" };
        let response = ui
            .selectable_label(is_active, format!("{}{star}", tab.label))
            .on_hover_text("クリックでアクティブ化、中クリックで閉じる");
        // アクティブタブが画面外に隠れたままにならないよう、切替直後の
        // フレームだけタブバーのスクロール位置を追従させる。
        if is_active && just_switched {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if response.clicked() {
            *action = Some(TabBarAction::Activate(index));
        }
        // SPEC §30: 「中クリックでも閉じる」。
        if response.middle_clicked() {
            *action = Some(TabBarAction::Close(index));
        }
        // SPEC §30: 「閉じるボタン(×)」。
        if ui.small_button("×").on_hover_text("タブを閉じる").clicked() {
            *action = Some(TabBarAction::Close(index));
        }
    });
    ui.separator();
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
