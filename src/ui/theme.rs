//! アプリ全体のビジュアルテーマ(v9 §44 → v13 §60「グラファイト+アンバー」)。
//!
//! v13 の UI 刷新で、Claude Design で作ったモック(SPEC §60)の配色に揃えた:
//!
//! - **無彩色のグラファイト 5 階調**(外枠 < パネル < 部品 < ホバー < 押下)。
//!   UI 自体に色味を持たせないことで、描いている絵の色を正しく判断できる
//!   (v9 の青みがかった灰色は、絵の色がわずかに黄寄りに見えた)。
//! - **アクセントはアンバー 1 色**。選択中のツール・アクティブなレイヤー行・
//!   スライダーの塗り・挿入位置インジケータ・未保存タブの点に一貫して使う。
//! - キャンバスの作業領域はパネルより暗くし、ドキュメントの縁を際立たせる。
//!
//! 色は各 UI モジュールが直接参照できるよう `pub const` で公開する(自前描画の
//! 行・ボタンが egui の `Visuals` を経由せずに同じ値を使えるように)。
//! 適用は起動時 1 回の `Style` 構築のみで、SPEC §0 の起動時間・アイドル
//! CPU 0% には影響しない。画像アセット・フォントは追加しない(CLAUDE.md)。

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// メニューバー・タブ帯・ステータスバー(最も外側の枠)。
pub const BG_APP: Color32 = Color32::from_rgb(0x16, 0x16, 0x18);
/// ツールバー・オプションバー・ドックのパネル面。
pub const BG_PANEL: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x21);
/// パネル内のまとまり(レイヤーのプロパティ欄など)。パネルより一段沈める。
pub const BG_CARD: Color32 = Color32::from_rgb(0x19, 0x19, 0x1c);
/// 入力欄・スライダーの値欄(掘り込み)。
pub const BG_SUNKEN: Color32 = Color32::from_rgb(0x15, 0x15, 0x17);
/// ボタン等の既定面。
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x26, 0x26, 0x2a);
pub const BG_HOVER: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x31);
pub const BG_ACTIVE: Color32 = Color32::from_rgb(0x34, 0x34, 0x3a);

/// キャンバスの作業領域(ドキュメント外)。SPEC §3 の「暗灰色」。
pub const CANVAS_WORKSPACE_FILL: Color32 = Color32::from_rgb(0x12, 0x12, 0x14);

/// 区切り線(パネル境界・セクション境界)。
pub const LINE: Color32 = Color32::from_rgb(0x26, 0x26, 0x2b);
/// 入力欄・トグルの枠。
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x34, 0x34, 0x3a);

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xe9);
/// ラベル・補足(パネル上で 6:1 前後)。
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xaa);
/// さらに控えめな補足(パネル上で 4.5:1 以上)。
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x93);
/// 無効・非表示レイヤー。
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x5e, 0x5e, 0x66);
/// アイコンの既定色。
pub const ICON: Color32 = Color32::from_rgb(0xb9, 0xb9, 0xc0);

/// アクセント(アンバー)。
pub const ACCENT: Color32 = Color32::from_rgb(0xf0, 0xa4, 0x3c);
/// アクセント上の文字色。
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1b, 0x14, 0x06);
/// アクセントの淡色(パネル上に 14% で重ねた色を不透明で持つ)。
/// アクティブ行・選択中ツールの背景。
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x3b, 0x31, 0x25);
/// アクセントの縁取り(パネル上に 40%)。アクティブ行・選択中ツールの枠。
pub const ACCENT_LINE: Color32 = Color32::from_rgb(0x72, 0x54, 0x2c);
/// 文字選択・選択中リスト項目の背景(アクセント文字が 4.5:1 で読める濃さ)。
const ACCENT_SELECTION: Color32 = Color32::from_rgb(0x5a, 0x41, 0x20);
/// 破壊的操作(削除)の文字色。
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x8a, 0x80);

/// パネル見出し・行などで使う文字サイズ。
pub const FONT_SMALL: f32 = 11.0;
pub const FONT_BODY: f32 = 13.0;

/// アプリ起動時に 1 回だけ呼ぶ(`DaraskApp::new`)。egui 0.35 はライト/
/// ダークのテーマ別 `Style` を持つため、ダークに固定した上でその `Style` を
/// 書き換える(OS がライトテーマでも本アプリの見た目は一定)。
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, configure_style);
}

fn configure_style(style: &mut egui::Style) {
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(FONT_SMALL, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(FONT_BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(FONT_BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(16.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();

    let visuals = &mut style.visuals;
    *visuals = egui::Visuals::dark();

    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = Color32::from_rgb(0x25, 0x25, 0x29);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3a, 0x40));
    visuals.extreme_bg_color = BG_SUNKEN;
    visuals.text_edit_bg_color = Some(BG_SUNKEN);
    visuals.faint_bg_color = BG_WIDGET;
    visuals.code_bg_color = BG_SUNKEN;
    visuals.override_text_color = None;
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;

    let radius = CornerRadius::same(6);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(10);

    // 5 状態のウィジェット表色(egui の interact/interact_selectable が参照
    // する。自前描画のボタン・タイルも同じ値を読むため、ここを変えるだけで
    // 全 UI が追随する)。
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.weak_bg_fill = BG_PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = BG_ACTIVE;
    widgets.inactive.weak_bg_fill = BG_WIDGET;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = radius;

    widgets.hovered.bg_fill = BG_HOVER;
    widgets.hovered.weak_bg_fill = BG_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x44, 0x44, 0x4b));
    widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.hovered.corner_radius = radius;

    widgets.active.bg_fill = BG_ACTIVE;
    widgets.active.weak_bg_fill = BG_ACTIVE;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    widgets.active.corner_radius = radius;

    widgets.open.bg_fill = BG_HOVER;
    widgets.open.weak_bg_fill = BG_HOVER;
    widgets.open.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x44, 0x44, 0x4b));
    widgets.open.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    widgets.open.corner_radius = radius;

    // 選択(トグル ON・テキスト選択・リストの選択項目)。スライダーの塗りは
    // `widgets::accent_slider` がスコープ内でだけ ACCENT に差し替える。
    visuals.selection.bg_fill = ACCENT_SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.slider_rail_height = 4.0;
    style.spacing.menu_margin = egui::Margin::same(6);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: Color32) -> f32 {
        0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32
    }

    /// WCAG のコントラスト比(sRGB を線形化した相対輝度から)。
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lin = |v: u8| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let rel = |c: Color32| 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
        let (hi, lo) = {
            let (x, y) = (rel(a), rel(b));
            if x > y {
                (x, y)
            } else {
                (y, x)
            }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    /// テーマの不変条件: 背景が暗い順に並び、作業領域がパネルより暗い
    /// (ドキュメントの縁が沈まない)。
    #[test]
    fn background_layers_are_ordered_dark_to_light() {
        assert!(luma(CANVAS_WORKSPACE_FILL) < luma(BG_APP));
        assert!(luma(BG_SUNKEN) < luma(BG_CARD));
        assert!(luma(BG_CARD) < luma(BG_PANEL));
        assert!(luma(BG_PANEL) < luma(BG_WIDGET));
        assert!(luma(BG_WIDGET) < luma(BG_HOVER));
        assert!(luma(BG_HOVER) < luma(BG_ACTIVE));
    }

    /// 文字色は WCAG AA(本文 4.5:1)をパネル面で満たす。
    #[test]
    fn text_colors_meet_contrast_on_panels() {
        for fg in [TEXT, TEXT_DIM, TEXT_FAINT] {
            assert!(contrast(fg, BG_PANEL) >= 4.5, "{fg:?}");
            assert!(contrast(fg, BG_APP) >= 4.5, "{fg:?}");
        }
        assert!(contrast(ON_ACCENT, ACCENT) >= 4.5);
        assert!(contrast(ACCENT, ACCENT_SELECTION) >= 3.0);
        assert!(contrast(TEXT, ACCENT_SOFT) >= 4.5);
    }

    /// UI 面は無彩色(R=G≒B)。絵の色の判断を邪魔しない。
    #[test]
    fn surfaces_are_neutral_gray() {
        for c in [BG_APP, BG_PANEL, BG_CARD, BG_WIDGET, BG_HOVER, BG_ACTIVE] {
            assert_eq!(c.r(), c.g(), "{c:?}");
            assert!(c.b().abs_diff(c.r()) <= 6, "{c:?}");
        }
    }
}
