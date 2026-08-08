//! v9 §44: アプリ全体のビジュアルテーマ。
//!
//! egui 既定のダークテーマは汎用のグレー基調で、ペイントソフトとしては
//! パネル・キャンバス・ウィジェットの階層が平坦に見える。ここで一度だけ
//! (`DaraskApp::new`)適用するカスタムテーマは:
//!
//! - **青みがかったダークグレーの 3 階調**(背景 < パネル < ウィジェット)で
//!   奥行きを作り、キャンバスが最も引き立つようにする。
//! - **単一のアクセント色**(落ち着いた青)を選択・強調・スライダーへ一貫して
//!   使う(ツールバーの選択中ツール・メニューのトグル・テキスト選択が同じ
//!   色になる)。
//! - 角丸・余白をわずかに広げ、44×40px のアイコンタイル群が窮屈に
//!   見えないようにする。
//!
//! 適用は起動時 1 回の `Style` 構築のみ(数マイクロ秒)で、SPEC §0 の起動
//! 300ms・アイドル CPU 0% には影響しない。色は `Color32::from_rgb` の
//! 定数だけで作り、画像アセット・フォントは一切追加しない(CLAUDE.md)。

use eframe::egui::{self, Color32, CornerRadius, Stroke};

/// キャンバスの作業領域(ドキュメント外)の背景色。SPEC §3 の「暗灰色」を
/// テーマに合わせて青みがかった暗色にする(`app.rs` の `CentralPanel` が
/// 使う)。
pub const CANVAS_WORKSPACE_FILL: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);

/// アクセント(選択・強調)。彩度を抑えた青 — 描いている絵の色と喧嘩しない。
const ACCENT: Color32 = Color32::from_rgb(0x4e, 0x8c, 0xd8);
/// アクセントの淡色(選択背景など、下の内容が透ける場面用)。
const ACCENT_SOFT: Color32 = Color32::from_rgba_premultiplied(0x2a, 0x4a, 0x72, 0xc0);

/// 背景 3 階調(暗い順): 入力欄など「掘り込み」 < パネル < ウィジェット。
const BG_SUNKEN: Color32 = Color32::from_rgb(0x15, 0x17, 0x1b);
const BG_PANEL: Color32 = Color32::from_rgb(0x1e, 0x20, 0x25);
const BG_WIDGET: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x35);
const BG_HOVER: Color32 = Color32::from_rgb(0x35, 0x39, 0x44);
const BG_ACTIVE: Color32 = Color32::from_rgb(0x3d, 0x42, 0x50);

/// 前景(文字・アイコン)。
const FG_TEXT: Color32 = Color32::from_rgb(0xd9, 0xdc, 0xe2);
const FG_WEAK: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
/// 枠線(控えめ。ホバーで少し明るく)。
const STROKE_QUIET: Color32 = Color32::from_rgb(0x3a, 0x3e, 0x49);
const STROKE_HOVER: Color32 = Color32::from_rgb(0x4d, 0x53, 0x61);

/// アプリ起動時に 1 回だけ呼ぶ(`DaraskApp::new`)。egui 0.35 はライト/
/// ダークのテーマ別 `Style` を持つため、ダークに固定した上でその `Style` を
/// 書き換える(OS がライトテーマでも本アプリの見た目は一定)。
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, configure_style);
}

fn configure_style(style: &mut egui::Style) {
    let visuals = &mut style.visuals;
    *visuals = egui::Visuals::dark();

    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG_SUNKEN;
    visuals.faint_bg_color = BG_WIDGET;
    visuals.code_bg_color = BG_SUNKEN;

    let radius = CornerRadius::same(4);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(6);

    // 5 状態のウィジェット表色(egui の interact/interact_selectable が参照
    // する。ツールバー・メニューバーの自前タイルも同じ値を読むため、ここを
    // 変えるだけで全 UI が追随する)。
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.weak_bg_fill = BG_PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, STROKE_QUIET);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG_WEAK);
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = BG_WIDGET;
    widgets.inactive.weak_bg_fill = BG_WIDGET;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = Stroke::new(1.0, FG_TEXT);
    widgets.inactive.corner_radius = radius;

    widgets.hovered.bg_fill = BG_HOVER;
    widgets.hovered.weak_bg_fill = BG_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, STROKE_HOVER);
    widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.hovered.corner_radius = radius;

    widgets.active.bg_fill = BG_ACTIVE;
    widgets.active.weak_bg_fill = BG_ACTIVE;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.active.corner_radius = radius;

    widgets.open.bg_fill = BG_ACTIVE;
    widgets.open.weak_bg_fill = BG_ACTIVE;
    widgets.open.bg_stroke = Stroke::new(1.0, STROKE_HOVER);
    widgets.open.fg_stroke = Stroke::new(1.0, FG_TEXT);
    widgets.open.corner_radius = radius;

    // 選択(ツールバーの選択中ツール・トグル ON・テキスト選択・スライダー)。
    visuals.selection.bg_fill = ACCENT_SOFT;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    // タイル群がわずかに呼吸できる余白(既定 4.0/2.0 前後からの微調整)。
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(6.0, 3.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テーマの不変条件: 3 階調の背景が暗い順に並び、本文文字が背景に対して
    /// 十分明るい(コントラストが逆転・平坦化するリグレッションの検知)。
    #[test]
    fn background_layers_are_ordered_dark_to_light() {
        let luma =
            |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        assert!(luma(BG_SUNKEN) < luma(BG_PANEL));
        assert!(luma(BG_PANEL) < luma(BG_WIDGET));
        assert!(luma(BG_WIDGET) < luma(BG_HOVER));
        assert!(luma(BG_HOVER) < luma(BG_ACTIVE));
        assert!(
            luma(FG_TEXT) - luma(BG_PANEL) > 100.0,
            "本文とパネル背景のコントラストを維持する"
        );
        assert!(luma(CANVAS_WORKSPACE_FILL) > luma(BG_PANEL));
    }
}
