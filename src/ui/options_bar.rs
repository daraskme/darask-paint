//! オプションバー(SPEC §3 の画面構成、§4 の内容)。
//!
//! M3 で実配線した:
//! - ブラシサイズスライダー(1–64px、SPEC §4)。
//! - ツール固有オプション: 矩形/楕円の枠線のみ/塗りつぶし/両方モード、
//!   塗りつぶしの許容値。
//!
//! v2 M3(ARCHITECTURE.md §14.7/§14.8)で色関連(スウォッチ・最近使った色)
//! を右パネルの `color_panel` へ移設した。サイズ・ツール固有オプションだけ
//! がここに残る。
//!
//! v3(SPEC §17、ARCHITECTURE.md §15.5 V3-M1)でペンの「アンチエイリアス」
//! チェックボックスを廃止し、ブラシ(旧ペン)・消しゴム共通の硬さ・不透明度
//! スライダーと鉛筆モードチェックボックスに置き換えた。

use eframe::egui;

use crate::keymap::{self, Action};
use crate::raster::GradientKind;
use crate::tools::gradient::GradientColors;
use crate::tools::shapes::ShapeMode;
use crate::tools::{LassoMode, ToolKind};

/// `show` に渡すオプションバーの状態一式。app.rs が各フィールドの持ち主
/// (`DaraskApp`/各 `Tool`)から可変参照を集めて構築する。
pub struct OptionsBarCtx<'a> {
    pub tool: ToolKind,
    pub brush_size: &'a mut f32,
    /// SPEC §17: 硬さ 0–100%(ブラシ/消しゴム共通)。
    pub brush_hardness: &'a mut u8,
    /// SPEC §17: 不透明度 1–100%(ブラシ/消しゴム共通。消しゴムでは
    /// 「強さ」というラベルで表示する)。
    pub brush_opacity: &'a mut u8,
    /// SPEC §17: 鉛筆モード(ブラシ/消しゴム共通)。
    pub pencil_mode: &'a mut bool,
    /// SPEC §25: ブラシ/消しゴム/鉛筆共通のスムージング(0–100%)。
    pub brush_smoothing: &'a mut u8,
    /// 現在のツールが矩形/楕円のときだけ `Some`(そのツール自身の `mode` の
    /// 可変参照)。
    pub shape_mode: Option<&'a mut ShapeMode>,
    pub fill_tolerance: &'a mut u8,
    /// v4 §23: グラデーションの種類(線形/円形)。
    pub gradient_kind: &'a mut GradientKind,
    /// v4 §23: グラデーションの色(プライマリ→セカンダリ/プライマリ→透明)。
    pub gradient_colors: &'a mut GradientColors,
    /// SPEC §19: テキストツールのフォントサイズ(8–144px、デフォルト 24)。
    pub text_font_size: &'a mut f32,
    /// v4 §22: なげなわの自由/多角形モード(Shift+L で切替)。表示専用
    /// (直接編集はできない。ここに出すのは ARCHITECTURE.md §16.10-10:
    /// 「巡回系はオプションバーの整合を忘れない」ため)。
    pub lasso_mode: LassoMode,
    /// v4 §22: 自動選択の許容値(0–255、塗りつぶしと同じ意味)。
    pub magic_wand_tolerance: &'a mut u8,
}

pub fn show(ui: &mut egui::Ui, state: OptionsBarCtx) {
    let OptionsBarCtx {
        tool,
        brush_size,
        brush_hardness,
        brush_opacity,
        pencil_mode,
        brush_smoothing,
        shape_mode,
        fill_tolerance,
        gradient_kind,
        gradient_colors,
        text_font_size,
        lasso_mode,
        magic_wand_tolerance,
    } = state;

    egui::Panel::top("options_bar")
        .exact_size(44.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("サイズ:");
                ui.add(egui::Slider::new(brush_size, 1.0..=64.0).suffix("px"));

                ui.separator();
                show_tool_specific(
                    ui,
                    tool,
                    brush_hardness,
                    brush_opacity,
                    pencil_mode,
                    brush_smoothing,
                    shape_mode,
                    fill_tolerance,
                    gradient_kind,
                    gradient_colors,
                    text_font_size,
                    lasso_mode,
                    magic_wand_tolerance,
                );
            });
        });
}

/// SPEC §3 の「ツール固有」領域: 現在のツールに応じたオプションを出す。
#[allow(clippy::too_many_arguments)]
fn show_tool_specific(
    ui: &mut egui::Ui,
    tool: ToolKind,
    brush_hardness: &mut u8,
    brush_opacity: &mut u8,
    pencil_mode: &mut bool,
    brush_smoothing: &mut u8,
    shape_mode: Option<&mut ShapeMode>,
    fill_tolerance: &mut u8,
    gradient_kind: &mut GradientKind,
    gradient_colors: &mut GradientColors,
    text_font_size: &mut f32,
    lasso_mode: LassoMode,
    magic_wand_tolerance: &mut u8,
) {
    match tool {
        ToolKind::Pen | ToolKind::Eraser => {
            show_brush_engine_options(
                ui,
                tool,
                brush_hardness,
                brush_opacity,
                pencil_mode,
                brush_smoothing,
            );
        }
        ToolKind::Rect | ToolKind::Ellipse => {
            if let Some(mode) = shape_mode {
                ui.label("モード:");
                ui.radio_value(mode, ShapeMode::Outline, ShapeMode::Outline.label());
                ui.radio_value(mode, ShapeMode::Fill, ShapeMode::Fill.label());
                ui.radio_value(mode, ShapeMode::Both, ShapeMode::Both.label());
            }
        }
        ToolKind::Fill => {
            ui.label("許容値:");
            let mut tol = *fill_tolerance as i32;
            if ui.add(egui::Slider::new(&mut tol, 0..=255)).changed() {
                *fill_tolerance = tol.clamp(0, 255) as u8;
            }
        }
        // v4 §23: 「種類: 線形 / 円形」「色: プライマリ→セカンダリ /
        // プライマリ→透明」(SPEC §23)。
        ToolKind::Gradient => {
            ui.label("種類:");
            ui.radio_value(gradient_kind, GradientKind::Linear, "線形");
            ui.radio_value(gradient_kind, GradientKind::Radial, "円形");
            ui.separator();
            ui.label("色:");
            ui.radio_value(
                gradient_colors,
                GradientColors::PrimaryToSecondary,
                GradientColors::PrimaryToSecondary.label(),
            );
            ui.radio_value(
                gradient_colors,
                GradientColors::PrimaryToTransparent,
                GradientColors::PrimaryToTransparent.label(),
            );
        }
        // SPEC §19: 「オプションバー: フォントサイズ 8–144px(デフォルト 24)」。
        ToolKind::Text => {
            ui.label("フォントサイズ:");
            ui.add(egui::Slider::new(text_font_size, 8.0..=144.0).suffix("px"));
        }
        // v4 §22: なげなわのモードは Shift+L でだけ切り替える(ここは表示
        // 専用。ARCHITECTURE.md §16.10-10 の「オプションバーの整合」)。
        //
        // v4 レビューで発見・修正したバグ: 以前はここに「Shift+L」を固定
        // 文字列で埋め込んでいた。keymap.rs のモジュールコメントおよび
        // ARCHITECTURE.md §15.4 は「メニュー表記・ツールチップは KEYMAP から
        // 文字列生成する(表記と実挙動を構造的に乖離させない)」と定めて
        // おり、toolbar.rs(`keymap::tool_shortcut_label`)・menu.rs
        // (`keymap::menu_label`)は準拠しているのに、ここだけ
        // `keymap::label_for(Action::CycleLassoMode)` を使っていなかった
        // (将来 KEYMAP のバインドを変更しても、この文字列だけコンパイル
        // エラーにもテスト失敗にもならず取り残される)。
        ToolKind::Lasso => {
            ui.label(lasso_mode_label(lasso_mode));
        }
        // v4 §22: 自動選択は塗りつぶしと同じ許容値スライダー。
        ToolKind::MagicWand => {
            ui.label("許容値:");
            let mut tol = *magic_wand_tolerance as i32;
            if ui.add(egui::Slider::new(&mut tol, 0..=255)).changed() {
                *magic_wand_tolerance = tol.clamp(0, 255) as u8;
            }
        }
        ToolKind::Line
        | ToolKind::Picker
        | ToolKind::Select
        | ToolKind::EllipseSelect
        | ToolKind::Pan
        | ToolKind::Move
        | ToolKind::Zoom => {
            ui.weak("(ツール固有オプションなし)");
        }
    }
}

/// なげなわのオプションバー表示文字列(SPEC §22)。ショートカット表記を
/// `keymap::label_for` から生成する(ARCHITECTURE.md §15.4: 「メニュー
/// 表記・ツールチップは KEYMAP から文字列生成する」)。egui に依存しない
/// 純関数として切り出してあるのでテストできる(`show` 内に埋め込んだままだと
/// ハードコード化への逆行を検知できない、v4 レビューで発見・修正したバグの
/// 再発防止)。
fn lasso_mode_label(mode: LassoMode) -> String {
    let shortcut = keymap::label_for(Action::CycleLassoMode);
    format!("モード: {}({shortcut} で切替)", mode.label())
}

/// SPEC §17: ブラシ(旧ペン)・消しゴム共通のストロークエンジンオプション
/// (硬さ・不透明度/強さ・鉛筆モード・SPEC §25 のスムージング)。
fn show_brush_engine_options(
    ui: &mut egui::Ui,
    tool: ToolKind,
    brush_hardness: &mut u8,
    brush_opacity: &mut u8,
    pencil_mode: &mut bool,
    brush_smoothing: &mut u8,
) {
    // SPEC §17: 「鉛筆モード…硬さ無視」なので、鉛筆モード中は硬さスライダー
    // を無効化(グレーアウト)する。
    ui.add_enabled_ui(!*pencil_mode, |ui| {
        ui.label("硬さ:");
        let mut hardness = *brush_hardness as i32;
        if ui
            .add(egui::Slider::new(&mut hardness, 0..=100).suffix("%"))
            .changed()
        {
            *brush_hardness = hardness.clamp(0, 100) as u8;
        }
    });

    // SPEC §17: 消しゴムでは同じ値を「強さ」というラベルで表示する。
    let opacity_label = if tool == ToolKind::Eraser {
        "強さ:"
    } else {
        "不透明度:"
    };
    ui.label(opacity_label);
    let mut opacity = *brush_opacity as i32;
    if ui
        .add(egui::Slider::new(&mut opacity, 1..=100).suffix("%"))
        .changed()
    {
        *brush_opacity = opacity.clamp(1, 100) as u8;
    }

    ui.checkbox(pencil_mode, "鉛筆モード");

    // SPEC §25: 「スムージング(手ブレ補正)…ブラシ/消しゴム/鉛筆のオプション
    // に 0–100%」。鉛筆モードでも有効(硬さと違い、無視されない)。
    ui.label("スムージング:");
    let mut smoothing = *brush_smoothing as i32;
    if ui
        .add(egui::Slider::new(&mut smoothing, 0..=100).suffix("%"))
        .changed()
    {
        *brush_smoothing = smoothing.clamp(0, 100) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v4 レビューで発見・修正したバグの回帰テスト: なげなわのオプション
    /// バー表示は `keymap::label_for(Action::CycleLassoMode)` から生成
    /// されなければならず、"Shift+L" を固定文字列で埋め込んではいけない
    /// (KEYMAP のバインドが将来変わった場合に自動で追随するように)。
    #[test]
    fn lasso_mode_label_is_derived_from_keymap_not_hardcoded() {
        let shortcut = keymap::label_for(Action::CycleLassoMode);
        assert_eq!(
            shortcut, "Shift+L",
            "sanity check on today's keymap.rs binding"
        );

        let label = lasso_mode_label(LassoMode::Freehand);
        assert!(
            label.contains(&shortcut),
            "the label must embed keymap::label_for's current value, not a literal copy: {label}"
        );
        assert_eq!(label, "モード: 自由(Shift+L で切替)");

        let label = lasso_mode_label(LassoMode::Polygon);
        assert_eq!(label, "モード: 多角形(Shift+L で切替)");
    }
}
