//! メニューバー(SPEC §7、v6 §33、v12 §59 → v13 §60)。
//!
//! v13 §60 で「アイコンタイル+グループタイル」の帯(高さ 50px)を、
//! **テキストのドロップダウンメニュー(左)+よく使う 9 操作のアイコン(右)**
//! の 1 段(40px)に置き換えた。各項目が返す `MenuAction`、有効条件、
//! ショートカット、確認ダイアログや履歴の扱いは従来から変更しない
//! (§59 の 9 操作は右側のアイコンとして常時表示のまま)。

use eframe::egui;

use crate::keymap::{self, Action};
use crate::ui::widgets::{self, IconPaint};
use crate::ui::{icons, theme};

/// クリックされたメニュー項目(まだ副作用は起こさない。`app.rs` が実行する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuAction {
    New,
    Open,
    OpenFolderAsPages,
    OpenRecent(usize),
    Save,
    SaveAs,
    CloseTab,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    CopyMerged,
    Paste,
    /// v13: クリップボード画像を新規レイヤーとして追加(Ctrl+Shift+V)。
    PasteAsLayer,
    /// v13: クリップボード画像から新規タブ(Ctrl+Alt+V)。
    NewFromClipboard,
    PasteFromFile,
    Delete,
    SelectAll,
    Deselect,
    SelectInverse,
    InpaintSelection,
    IopaintInpaint,
    DiffusionGenerate,
    DiffusionInpaint,
    FreeTransform,
    ImageResize,
    CanvasResize,
    Crop,
    DuplicateSelectionToTab,
    CutSelectionToTab,
    FlipHorizontal,
    FlipVertical,
    RotateCw,
    RotateCcw,
    BrightnessContrast,
    HueSaturation,
    Invert,
    Grayscale,
    Mosaic,
    ZoomIn,
    ZoomOut,
    Zoom100,
    FitWindow,
    TogglePixelGrid,
    ResetPanelLayout,
    LayerAdd,
    LayerDuplicate,
    LayerDelete,
    LayerMoveUp,
    LayerMoveDown,
    LayerMergeDown,
    LayerFlatten,
    About,
    OpenPreferences,
}

/// メニュー項目の有効/無効判定に使う状態。
pub struct MenuState<'a> {
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_selection: bool,
    pub background_job_running: bool,
    pub can_duplicate_selection_to_tab: bool,
    pub can_add_layer: bool,
    pub can_delete_layer: bool,
    pub can_move_layer_up: bool,
    pub can_move_layer_down: bool,
    pub can_merge_layer_down: bool,
    pub can_flatten_layers: bool,
    pub pixel_grid_visible: bool,
    pub recent_files: &'a std::collections::VecDeque<std::path::PathBuf>,
}

/// 1 段の高さ。幅が足りないときはアイコン列を 2 段目へ送る。
const ROW_H: f32 = 40.0;
const SECOND_ROW_H: f32 = 36.0;
const QUICK_BUTTON: f32 = 30.0;
const QUICK_ICON: f32 = 18.0;
const QUICK_SEP_W: f32 = 13.0;
const MENU_PAD_X: f32 = 10.0;
const BRAND_MARK: f32 = 22.0;
const BAR_MARGIN_X: f32 = 10.0;
/// メニュー群とアイコン列の間に最低限空けるすき間。
const MIN_GAP: f32 = 24.0;
/// ドロップダウンの幅(行はこの幅で固定。ポップアップの利用可能幅いっぱいに
/// 広げると画面幅近くまで伸びてしまう)。
const POPUP_MIN_WIDTH: f32 = 300.0;
const POPUP_ROW_H: f32 = 30.0;
const POPUP_ICON_SIZE: f32 = 16.0;

/// ドロップダウンの 1 行。
#[derive(Clone, Copy)]
enum MenuEntry {
    Action(MenuAction),
    Separator,
    /// 「最近使ったファイル」の一覧(§26 の挙動は不変)。
    RecentFiles,
}

#[derive(Clone, Copy)]
struct TextMenu {
    label: &'static str,
    entries: &'static [MenuEntry],
}

use MenuEntry::{Action as A, RecentFiles, Separator as Sep};

/// SPEC §60 のメニュー構成を保持する唯一のテーブル。
const MENUS: &[TextMenu] = &[
    TextMenu {
        label: "ファイル",
        entries: &[
            A(MenuAction::New),
            A(MenuAction::Open),
            A(MenuAction::OpenFolderAsPages),
            Sep,
            A(MenuAction::Save),
            A(MenuAction::SaveAs),
            Sep,
            RecentFiles,
            Sep,
            A(MenuAction::CloseTab),
            A(MenuAction::Exit),
        ],
    },
    TextMenu {
        label: "編集",
        entries: &[
            A(MenuAction::Undo),
            A(MenuAction::Redo),
            Sep,
            A(MenuAction::Cut),
            A(MenuAction::Copy),
            A(MenuAction::CopyMerged),
            Sep,
            A(MenuAction::Paste),
            A(MenuAction::PasteAsLayer),
            A(MenuAction::NewFromClipboard),
            A(MenuAction::PasteFromFile),
            Sep,
            A(MenuAction::FreeTransform),
            A(MenuAction::OpenPreferences),
        ],
    },
    TextMenu {
        label: "選択",
        entries: &[
            A(MenuAction::SelectAll),
            A(MenuAction::Deselect),
            A(MenuAction::SelectInverse),
            A(MenuAction::Delete),
            Sep,
            A(MenuAction::DuplicateSelectionToTab),
            A(MenuAction::CutSelectionToTab),
        ],
    },
    TextMenu {
        label: "画像",
        entries: &[
            A(MenuAction::ImageResize),
            A(MenuAction::CanvasResize),
            A(MenuAction::Crop),
            Sep,
            A(MenuAction::FlipHorizontal),
            A(MenuAction::FlipVertical),
            A(MenuAction::RotateCw),
            A(MenuAction::RotateCcw),
        ],
    },
    TextMenu {
        label: "色調補正",
        entries: &[
            A(MenuAction::BrightnessContrast),
            A(MenuAction::HueSaturation),
            A(MenuAction::Invert),
            A(MenuAction::Grayscale),
            Sep,
            A(MenuAction::Mosaic),
        ],
    },
    TextMenu {
        label: "AI・修復",
        entries: &[
            A(MenuAction::InpaintSelection),
            Sep,
            A(MenuAction::IopaintInpaint),
            A(MenuAction::DiffusionGenerate),
            A(MenuAction::DiffusionInpaint),
        ],
    },
    TextMenu {
        label: "レイヤー",
        entries: &[
            A(MenuAction::LayerAdd),
            A(MenuAction::LayerDuplicate),
            A(MenuAction::LayerDelete),
            Sep,
            A(MenuAction::LayerMoveUp),
            A(MenuAction::LayerMoveDown),
            Sep,
            A(MenuAction::LayerMergeDown),
            A(MenuAction::LayerFlatten),
        ],
    },
    TextMenu {
        label: "表示",
        entries: &[
            A(MenuAction::ZoomIn),
            A(MenuAction::ZoomOut),
            A(MenuAction::Zoom100),
            A(MenuAction::FitWindow),
            Sep,
            A(MenuAction::TogglePixelGrid),
            Sep,
            A(MenuAction::ResetPanelLayout),
        ],
    },
    TextMenu {
        label: "ヘルプ",
        entries: &[A(MenuAction::About)],
    },
];

/// 右側に常時表示する操作(SPEC §59 の 9 操作。`None` は区切り線)。
const QUICK_ACTIONS: &[Option<MenuAction>] = &[
    Some(MenuAction::New),
    Some(MenuAction::Open),
    Some(MenuAction::Save),
    None,
    Some(MenuAction::Undo),
    Some(MenuAction::Redo),
    None,
    Some(MenuAction::Cut),
    Some(MenuAction::Copy),
    Some(MenuAction::Paste),
    None,
    Some(MenuAction::FreeTransform),
];

impl MenuAction {
    fn full_label(self) -> &'static str {
        match self {
            Self::New => "新規",
            Self::Open => "開く",
            Self::OpenFolderAsPages => "フォルダをページとして開く…",
            Self::OpenRecent(_) => "最近使ったファイル",
            Self::Save => "上書き保存",
            Self::SaveAs => "名前を付けて保存",
            Self::CloseTab => "タブを閉じる",
            Self::Exit => "終了",
            Self::Undo => "元に戻す",
            Self::Redo => "やり直し",
            Self::Cut => "切り取り",
            Self::Copy => "コピー",
            Self::CopyMerged => "結合部分をコピー",
            Self::Paste => "貼り付け",
            Self::PasteAsLayer => "レイヤーとして貼り付け",
            Self::NewFromClipboard => "クリップボードから新規画像",
            Self::PasteFromFile => "ファイルから貼り付け",
            Self::Delete => "削除",
            Self::SelectAll => "すべて選択",
            Self::Deselect => "選択解除",
            Self::SelectInverse => "選択範囲を反転",
            Self::InpaintSelection => "選択範囲を修復",
            Self::IopaintInpaint => "AI 修復(IOpaint)…",
            Self::DiffusionGenerate => "AI 生成(Diffusion)…",
            Self::DiffusionInpaint => "AI 置換(Diffusion)…",
            Self::FreeTransform => "自由変形",
            Self::ImageResize => "画像サイズ変更",
            Self::CanvasResize => "キャンバスサイズ変更",
            Self::Crop => "選択範囲でトリミング",
            Self::DuplicateSelectionToTab => "選択範囲を新規タブに複製",
            Self::CutSelectionToTab => "選択範囲を切り取って新規タブへ",
            _ => self.full_label_image_and_later(),
        }
    }

    fn full_label_image_and_later(self) -> &'static str {
        match self {
            Self::FlipHorizontal => "左右反転",
            Self::FlipVertical => "上下反転",
            Self::RotateCw => "右に 90°回転",
            Self::RotateCcw => "左に 90°回転",
            Self::BrightnessContrast => "明るさ・コントラスト",
            Self::HueSaturation => "色相・彩度・明度",
            Self::Invert => "階調の反転",
            Self::Grayscale => "グレースケール化",
            Self::Mosaic => "モザイク",
            _ => self.full_label_view_and_later(),
        }
    }

    fn full_label_view_and_later(self) -> &'static str {
        match self {
            Self::ZoomIn => "拡大",
            Self::ZoomOut => "縮小",
            Self::Zoom100 => "100%",
            Self::FitWindow => "ウィンドウに合わせる",
            Self::TogglePixelGrid => "ピクセルグリッド表示",
            Self::ResetPanelLayout => "パネル配置をリセット",
            Self::LayerAdd => "新規レイヤー",
            Self::LayerDuplicate => "レイヤーを複製",
            Self::LayerDelete => "レイヤーを削除",
            Self::LayerMoveUp => "上へ",
            Self::LayerMoveDown => "下へ",
            Self::LayerMergeDown => "下と結合",
            Self::LayerFlatten => "画像の統合",
            Self::About => "バージョン情報",
            Self::OpenPreferences => "設定…",
            _ => "",
        }
    }

    fn shortcut(self) -> Option<Action> {
        match self {
            Self::New => Some(Action::New),
            Self::Open => Some(Action::Open),
            Self::Save => Some(Action::Save),
            Self::SaveAs => Some(Action::SaveAs),
            Self::CloseTab => Some(Action::CloseTab),
            Self::Undo => Some(Action::Undo),
            Self::Redo => Some(Action::Redo),
            Self::Cut => Some(Action::Cut),
            Self::Copy => Some(Action::Copy),
            Self::CopyMerged => Some(Action::CopyMerged),
            Self::Paste => Some(Action::Paste),
            Self::PasteAsLayer => Some(Action::PasteAsLayer),
            Self::NewFromClipboard => Some(Action::NewFromClipboard),
            Self::Delete => Some(Action::Delete),
            Self::SelectAll => Some(Action::SelectAll),
            Self::Deselect => Some(Action::Deselect),
            Self::SelectInverse => Some(Action::SelectInverse),
            Self::FreeTransform => Some(Action::FreeTransform),
            Self::HueSaturation => Some(Action::HueSaturation),
            Self::Invert => Some(Action::Invert),
            Self::Grayscale => Some(Action::Grayscale),
            Self::ZoomIn => Some(Action::ZoomIn),
            Self::ZoomOut => Some(Action::ZoomOut),
            Self::Zoom100 => Some(Action::Zoom100),
            Self::FitWindow => Some(Action::FitWindow),
            Self::LayerAdd => Some(Action::LayerAdd),
            Self::LayerDuplicate => Some(Action::LayerDuplicate),
            Self::LayerMergeDown => Some(Action::LayerMergeDown),
            Self::LayerFlatten => Some(Action::LayerFlatten),
            Self::OpenPreferences => Some(Action::OpenPreferences),
            _ => None,
        }
    }

    fn shortcut_label(self) -> String {
        if self == Self::Exit {
            "Alt+F4".to_owned()
        } else {
            self.shortcut().map(keymap::label_for).unwrap_or_default()
        }
    }

    fn enabled(self, state: &MenuState) -> bool {
        match self {
            Self::Undo => state.can_undo,
            Self::Redo => state.can_redo,
            Self::Cut
            | Self::Copy
            | Self::CopyMerged
            | Self::Delete
            | Self::Deselect
            | Self::SelectInverse
            | Self::Crop
            | Self::CutSelectionToTab => state.has_selection,
            Self::InpaintSelection | Self::IopaintInpaint | Self::DiffusionInpaint => {
                state.has_selection && !state.background_job_running
            }
            Self::DiffusionGenerate => !state.background_job_running,
            Self::DuplicateSelectionToTab => state.can_duplicate_selection_to_tab,
            Self::LayerAdd | Self::LayerDuplicate | Self::PasteAsLayer => state.can_add_layer,
            Self::LayerDelete => state.can_delete_layer,
            Self::LayerMoveUp => state.can_move_layer_up,
            Self::LayerMoveDown => state.can_move_layer_down,
            Self::LayerMergeDown => state.can_merge_layer_down,
            Self::LayerFlatten => state.can_flatten_layers,
            _ => true,
        }
    }

    fn selected(self, state: &MenuState) -> bool {
        self == Self::TogglePixelGrid && state.pixel_grid_visible
    }

    fn icon(self) -> IconPaint {
        match self {
            Self::New => icons::paint_new_document_icon,
            Self::Open => icons::paint_open_icon,
            Self::OpenFolderAsPages => icons::paint_open_icon,
            Self::OpenRecent(_) => icons::paint_recent_files_icon,
            Self::Save => icons::paint_save_icon,
            Self::SaveAs => icons::paint_save_as_icon,
            Self::CloseTab => icons::paint_close_tab_icon,
            Self::Exit => icons::paint_exit_icon,
            Self::Undo => icons::paint_undo_icon,
            Self::Redo => icons::paint_redo_icon,
            Self::Cut => icons::paint_cut_icon,
            Self::Copy => icons::paint_copy_icon,
            Self::CopyMerged => icons::paint_copy_merged_icon,
            Self::Paste => icons::paint_paste_icon,
            Self::PasteAsLayer => icons::paint_paste_as_layer_icon,
            Self::NewFromClipboard => icons::paint_new_from_clipboard_icon,
            Self::PasteFromFile => icons::paint_paste_file_icon,
            Self::Delete => icons::paint_delete_icon,
            Self::SelectAll => icons::paint_select_all_icon,
            Self::Deselect => icons::paint_deselect_icon,
            Self::SelectInverse => icons::paint_select_inverse_icon,
            Self::InpaintSelection
            | Self::IopaintInpaint
            | Self::DiffusionGenerate
            | Self::DiffusionInpaint => icons::paint_inpaint_icon,
            Self::FreeTransform => icons::paint_free_transform_icon,
            Self::ImageResize => icons::paint_image_resize_icon,
            Self::CanvasResize => icons::paint_canvas_resize_icon,
            Self::Crop => icons::paint_crop_icon,
            Self::DuplicateSelectionToTab => icons::paint_duplicate_to_tab_icon,
            Self::CutSelectionToTab => icons::paint_cut_to_tab_icon,
            Self::FlipHorizontal => icons::paint_flip_horizontal_icon,
            Self::FlipVertical => icons::paint_flip_vertical_icon,
            Self::RotateCw => icons::paint_rotate_cw_icon,
            Self::RotateCcw => icons::paint_rotate_ccw_icon,
            Self::BrightnessContrast => icons::paint_brightness_contrast_icon,
            Self::HueSaturation => icons::paint_hue_saturation_icon,
            Self::Invert => icons::paint_invert_icon,
            Self::Grayscale => icons::paint_grayscale_icon,
            Self::Mosaic => icons::paint_mosaic_icon,
            Self::ZoomIn => icons::paint_zoom_in_icon,
            Self::ZoomOut => icons::paint_zoom_out_icon,
            Self::Zoom100 => icons::paint_zoom_100_icon,
            Self::FitWindow => icons::paint_fit_window_icon,
            Self::TogglePixelGrid => icons::paint_pixel_grid_icon,
            Self::ResetPanelLayout => icons::paint_panel_reset_icon,
            Self::LayerAdd => icons::paint_layer_add_icon,
            Self::LayerDuplicate => icons::paint_layer_duplicate_icon,
            Self::LayerDelete => icons::paint_layer_delete_icon,
            Self::LayerMoveUp => icons::paint_layer_move_up_icon,
            Self::LayerMoveDown => icons::paint_layer_move_down_icon,
            Self::LayerMergeDown => icons::paint_layer_merge_down_icon,
            Self::LayerFlatten => icons::paint_layer_flatten_icon,
            Self::About => icons::paint_about_icon,
            Self::OpenPreferences => icons::paint_settings_icon,
        }
    }

    fn tooltip(self) -> String {
        match self.shortcut() {
            Some(shortcut) => keymap::menu_label(self.full_label(), shortcut),
            None if self == Self::Exit => format!("{} (Alt+F4)", self.full_label()),
            None => self.full_label().to_owned(),
        }
    }
}

impl TextMenu {
    fn enabled(self, state: &MenuState) -> bool {
        self.entries.iter().any(|entry| match entry {
            MenuEntry::Action(action) => action.enabled(state),
            MenuEntry::RecentFiles => !state.recent_files.is_empty(),
            MenuEntry::Separator => false,
        })
    }
}

/// ドロップダウンの 1 行(アイコン + 名前 + 右寄せのショートカット)。
fn popup_action_row(
    ui: &mut egui::Ui,
    state: &MenuState,
    action: MenuAction,
) -> (egui::Response, Option<MenuAction>) {
    let enabled = action.enabled(state);
    let selected = action.selected(state);
    let width = POPUP_MIN_WIDTH;
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, POPUP_ROW_H), sense);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            enabled,
            selected,
            action.full_label(),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (text, icon, shortcut_color) = if !enabled {
            (
                theme::TEXT_DISABLED,
                theme::TEXT_DISABLED,
                theme::TEXT_DISABLED,
            )
        } else if response.hovered() {
            painter.rect_filled(rect, 6.0, theme::BG_HOVER);
            (egui::Color32::WHITE, egui::Color32::WHITE, theme::TEXT_DIM)
        } else {
            (theme::TEXT, theme::TEXT_DIM, theme::TEXT_FAINT)
        };
        let icon = if selected && enabled {
            theme::ACCENT
        } else {
            icon
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::vec2(POPUP_ICON_SIZE, POPUP_ICON_SIZE),
        );
        (action.icon())(painter, icon_rect, icon);
        painter.text(
            egui::pos2(rect.left() + 36.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            action.full_label(),
            egui::FontId::proportional(12.5),
            text,
        );
        let mut right = rect.right() - 10.0;
        if selected {
            let check = egui::Rect::from_center_size(
                egui::pos2(right - 6.0, rect.center().y),
                egui::vec2(12.0, 12.0),
            );
            let st = egui::Stroke::new(1.8, theme::ACCENT);
            painter.line_segment(
                [
                    check.lerp_inside(egui::vec2(0.1, 0.55)),
                    check.lerp_inside(egui::vec2(0.4, 0.85)),
                ],
                st,
            );
            painter.line_segment(
                [
                    check.lerp_inside(egui::vec2(0.4, 0.85)),
                    check.lerp_inside(egui::vec2(0.95, 0.2)),
                ],
                st,
            );
            right -= 20.0;
        }
        let shortcut = action.shortcut_label();
        if !shortcut.is_empty() {
            painter.text(
                egui::pos2(right, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                shortcut,
                egui::FontId::proportional(12.0),
                shortcut_color,
            );
        }
    }
    let clicked = response.clicked().then_some(action);
    (response, clicked)
}

fn popup_separator(ui: &mut egui::Ui) {
    let width = POPUP_MIN_WIDTH;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 9.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(2.0),
        rect.center().y,
        egui::Stroke::new(1.0, theme::LINE_STRONG),
    );
}

fn recent_file_row(ui: &mut egui::Ui, index: usize, path: &std::path::Path) -> Option<MenuAction> {
    let full_path = path.to_string_lossy().into_owned();
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| full_path.clone());
    let width = POPUP_MIN_WIDTH;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, POPUP_ROW_H), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    if ui.is_rect_visible(rect) {
        let (text, icon) = if response.hovered() {
            ui.painter().rect_filled(rect, 6.0, theme::BG_HOVER);
            (egui::Color32::WHITE, egui::Color32::WHITE)
        } else {
            (theme::TEXT, theme::TEXT_DIM)
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::vec2(POPUP_ICON_SIZE, POPUP_ICON_SIZE),
        );
        icons::paint_recent_files_icon(ui.painter(), icon_rect, icon);
        ui.painter().text(
            egui::pos2(rect.left() + 36.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.5),
            text,
        );
    }
    response
        .on_hover_text(full_path)
        .clicked()
        .then_some(MenuAction::OpenRecent(index))
}

fn show_recent_files(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("  最近使ったファイル")
            .size(theme::FONT_SMALL)
            .color(theme::TEXT_FAINT),
    );
    if state.recent_files.is_empty() {
        ui.label(
            egui::RichText::new("  (なし)")
                .size(theme::FONT_SMALL)
                .color(theme::TEXT_DISABLED),
        );
        return None;
    }
    for (index, path) in state.recent_files.iter().enumerate() {
        if let Some(action) = recent_file_row(ui, index, path) {
            return Some(action);
        }
    }
    None
}

fn show_menu_contents(ui: &mut egui::Ui, menu: TextMenu, state: &MenuState) -> Option<MenuAction> {
    ui.set_min_width(POPUP_MIN_WIDTH);
    ui.spacing_mut().item_spacing.y = 1.0;
    for entry in menu.entries {
        let clicked = match entry {
            MenuEntry::Action(action) => popup_action_row(ui, state, *action).1,
            MenuEntry::Separator => {
                popup_separator(ui);
                None
            }
            MenuEntry::RecentFiles => show_recent_files(ui, state),
        };
        if clicked.is_some() {
            return clicked;
        }
    }
    None
}

/// ロゴマーク(アンバーの角丸+ブラシ)とアプリ名。
fn brand(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(BRAND_MARK, BRAND_MARK), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 6.0, theme::ACCENT);
        icons::paint_tool_icon(
            crate::tools::ToolKind::Pen,
            ui.painter(),
            rect.shrink(4.0),
            theme::ON_ACCENT,
        );
    }
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("Darask Paint")
            .strong()
            .size(13.0)
            .color(theme::TEXT),
    );
    ui.add_space(8.0);
}

fn brand_width(ui: &egui::Ui) -> f32 {
    let text = ui.painter().layout_no_wrap(
        "Darask Paint".to_owned(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
    );
    BRAND_MARK + 2.0 + text.size().x + 8.0 + ui.spacing().item_spacing.x * 2.0
}

fn menus_width(ui: &egui::Ui) -> f32 {
    MENUS
        .iter()
        .map(|menu| {
            ui.painter()
                .layout_no_wrap(
                    menu.label.to_owned(),
                    egui::FontId::proportional(theme::FONT_BODY),
                    theme::TEXT,
                )
                .size()
                .x
                + MENU_PAD_X * 2.0
        })
        .sum()
}

fn quick_width() -> f32 {
    QUICK_ACTIONS
        .iter()
        .map(|slot| match slot {
            Some(_) => QUICK_BUTTON + 2.0,
            None => QUICK_SEP_W,
        })
        .sum()
}

/// 1 段に収まるか(純関数)。`left` はロゴ+メニュー、`right` はアイコン列。
fn fits_one_row(avail: f32, left: f32, right: f32) -> bool {
    left + MIN_GAP + right + BAR_MARGIN_X * 2.0 <= avail
}

/// `reversed` は右詰めレイアウト(右から順に置く)用。
fn quick_buttons(
    ui: &mut egui::Ui,
    state: &MenuState,
    action: &mut Option<MenuAction>,
    reversed: bool,
) {
    ui.spacing_mut().item_spacing.x = 2.0;
    let slots: Vec<&Option<MenuAction>> = if reversed {
        QUICK_ACTIONS.iter().rev().collect()
    } else {
        QUICK_ACTIONS.iter().collect()
    };
    for slot in slots {
        match slot {
            Some(item) => {
                let response = widgets::icon_button(
                    ui,
                    egui::vec2(QUICK_BUTTON, QUICK_BUTTON),
                    QUICK_ICON,
                    item.enabled(state),
                    false,
                    item.icon(),
                    item.full_label(),
                )
                .on_hover_text(item.tooltip());
                if response.clicked() {
                    *action = Some(*item);
                }
            }
            None => widgets::vertical_divider(ui, 18.0),
        }
    }
}

fn menu_bar_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = egui::vec2(MENU_PAD_X, 5.0);
    style.spacing.item_spacing.x = 0.0;
    let w = &mut style.visuals.widgets;
    w.inactive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0xc8, 0xc8, 0xce));
    w.hovered.weak_bg_fill = theme::BG_WIDGET;
    w.open.weak_bg_fill = theme::BG_HOVER;
}

/// メニューバーを描画し、クリックされた操作を返す。
pub fn show(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    let mut action = None;
    let left = brand_width(ui) + menus_width(ui);
    let one_row = fits_one_row(ui.available_width(), left, quick_width());
    let height = if one_row { ROW_H } else { ROW_H + SECOND_ROW_H };

    egui::Panel::top("menu_bar")
        .exact_size(height)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG_APP)
                .inner_margin(egui::Margin::symmetric(BAR_MARGIN_X as i8, 0)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.painter().hline(
                full.x_range().expand(BAR_MARGIN_X),
                full.bottom() - 0.5,
                egui::Stroke::new(1.0, theme::LINE),
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ROW_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    brand(ui);
                    egui::MenuBar::new().style(menu_bar_style).ui(ui, |ui| {
                        for menu in MENUS {
                            let enabled = menu.enabled(state);
                            ui.add_enabled_ui(enabled, |ui| {
                                ui.menu_button(menu.label, |ui| {
                                    if let Some(selected) = show_menu_contents(ui, *menu, state) {
                                        action = Some(selected);
                                        ui.close();
                                    }
                                });
                            });
                        }
                        if one_row {
                            // 右から詰めるので逆順に並べる。
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| quick_buttons(ui, state, &mut action, true),
                            );
                        }
                    });
                },
            );
            if !one_row {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), SECOND_ROW_H),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| quick_buttons(ui, state, &mut action, false),
                );
            }
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_MENU_ACTIONS: &[MenuAction] = &[
        MenuAction::New,
        MenuAction::Open,
        MenuAction::OpenFolderAsPages,
        MenuAction::Save,
        MenuAction::SaveAs,
        MenuAction::CloseTab,
        MenuAction::Exit,
        MenuAction::Undo,
        MenuAction::Redo,
        MenuAction::Cut,
        MenuAction::Copy,
        MenuAction::CopyMerged,
        MenuAction::Paste,
        MenuAction::PasteAsLayer,
        MenuAction::NewFromClipboard,
        MenuAction::PasteFromFile,
        MenuAction::OpenRecent(0),
        MenuAction::Delete,
        MenuAction::SelectAll,
        MenuAction::Deselect,
        MenuAction::SelectInverse,
        MenuAction::InpaintSelection,
        MenuAction::IopaintInpaint,
        MenuAction::DiffusionGenerate,
        MenuAction::DiffusionInpaint,
        MenuAction::FreeTransform,
        MenuAction::ImageResize,
        MenuAction::CanvasResize,
        MenuAction::Crop,
        MenuAction::DuplicateSelectionToTab,
        MenuAction::CutSelectionToTab,
        MenuAction::FlipHorizontal,
        MenuAction::FlipVertical,
        MenuAction::RotateCw,
        MenuAction::RotateCcw,
        MenuAction::BrightnessContrast,
        MenuAction::HueSaturation,
        MenuAction::Invert,
        MenuAction::Grayscale,
        MenuAction::Mosaic,
        MenuAction::ZoomIn,
        MenuAction::ZoomOut,
        MenuAction::Zoom100,
        MenuAction::FitWindow,
        MenuAction::TogglePixelGrid,
        MenuAction::ResetPanelLayout,
        MenuAction::LayerAdd,
        MenuAction::LayerDuplicate,
        MenuAction::LayerDelete,
        MenuAction::LayerMoveUp,
        MenuAction::LayerMoveDown,
        MenuAction::LayerMergeDown,
        MenuAction::LayerFlatten,
        MenuAction::About,
        MenuAction::OpenPreferences,
    ];

    fn state_with<'a>(
        recent_files: &'a std::collections::VecDeque<std::path::PathBuf>,
        enabled: bool,
    ) -> MenuState<'a> {
        MenuState {
            can_undo: enabled,
            can_redo: enabled,
            has_selection: enabled,
            background_job_running: false,
            can_duplicate_selection_to_tab: enabled,
            can_add_layer: enabled,
            can_delete_layer: enabled,
            can_move_layer_up: enabled,
            can_move_layer_down: enabled,
            can_merge_layer_down: enabled,
            can_flatten_layers: enabled,
            pixel_grid_visible: enabled,
            recent_files,
        }
    }

    #[test]
    fn menu_labels_match_spec_60() {
        let labels: Vec<&str> = MENUS.iter().map(|menu| menu.label).collect();
        assert_eq!(
            labels,
            vec![
                "ファイル",
                "編集",
                "選択",
                "画像",
                "色調補正",
                "AI・修復",
                "レイヤー",
                "表示",
                "ヘルプ"
            ]
        );
    }

    #[test]
    fn every_menu_entry_appears_exactly_once() {
        let mut placed = Vec::new();
        for menu in MENUS {
            for entry in menu.entries {
                match entry {
                    MenuEntry::Action(action) => placed.push(*action),
                    MenuEntry::RecentFiles => placed.push(MenuAction::OpenRecent(0)),
                    MenuEntry::Separator => {}
                }
            }
        }
        assert_eq!(placed.len(), ALL_MENU_ACTIONS.len());
        for action in ALL_MENU_ACTIONS {
            assert_eq!(
                placed.iter().filter(|placed| *placed == action).count(),
                1,
                "{action:?} の配置数"
            );
        }
        assert_eq!(placed.len(), 55);
    }

    #[test]
    fn quick_actions_keep_the_nine_always_visible_operations_of_spec_59() {
        let quick: Vec<&str> = QUICK_ACTIONS
            .iter()
            .flatten()
            .map(|action| action.full_label())
            .collect();
        assert_eq!(
            quick,
            vec![
                "新規",
                "開く",
                "上書き保存",
                "元に戻す",
                "やり直し",
                "切り取り",
                "コピー",
                "貼り付け",
                "自由変形"
            ]
        );
        assert!(!matches!(QUICK_ACTIONS.first(), Some(None)));
        assert!(!matches!(QUICK_ACTIONS.last(), Some(None)));
    }

    #[test]
    fn menus_never_start_or_end_with_a_separator() {
        for menu in MENUS {
            assert!(
                !matches!(menu.entries.first(), Some(MenuEntry::Separator)),
                "{}",
                menu.label
            );
            assert!(
                !matches!(menu.entries.last(), Some(MenuEntry::Separator)),
                "{}",
                menu.label
            );
        }
    }

    #[test]
    fn the_bar_uses_one_row_at_1280_and_two_rows_when_narrow() {
        // 実測に近い見積もり(13px の日本語 28 文字 + 余白)。
        let left = 130.0 + 28.0 * 13.0 + MENUS.len() as f32 * MENU_PAD_X * 2.0;
        assert!(fits_one_row(1280.0, left, quick_width()));
        assert!(!fits_one_row(640.0, left, quick_width()));
    }

    #[test]
    fn a_menu_is_disabled_only_when_every_entry_is_disabled() {
        let recent_files = std::collections::VecDeque::new();
        let mut disabled = state_with(&recent_files, false);
        disabled.background_job_running = true;
        let ai = MENUS
            .iter()
            .find(|m| m.label == "AI・修復")
            .expect("AI menu");
        assert!(!ai.enabled(&disabled));
        let mut one_enabled = state_with(&recent_files, false);
        one_enabled.has_selection = true;
        assert!(ai.enabled(&one_enabled));
    }

    #[test]
    fn clipboard_items_are_in_the_edit_menu_with_shortcuts() {
        let edit = MENUS.iter().find(|m| m.label == "編集").expect("edit menu");
        let actions: Vec<MenuAction> = edit
            .entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Action(a) => Some(*a),
                _ => None,
            })
            .collect();
        let paste = actions
            .iter()
            .position(|a| *a == MenuAction::Paste)
            .expect("paste");
        assert_eq!(actions[paste + 1], MenuAction::PasteAsLayer);
        assert_eq!(actions[paste + 2], MenuAction::NewFromClipboard);
        assert_eq!(MenuAction::PasteAsLayer.shortcut_label(), "Ctrl+Shift+V");
        assert_eq!(MenuAction::NewFromClipboard.shortcut_label(), "Ctrl+Alt+V");
    }

    #[test]
    fn every_static_action_has_complete_display_metadata() {
        for action in ALL_MENU_ACTIONS {
            assert!(!action.full_label().is_empty(), "{action:?}");
            let icon = action.icon();
            assert_ne!(icon as usize, 0, "{action:?}");
        }
    }

    #[test]
    fn popup_rows_return_the_same_menu_actions() {
        let recent_files = std::collections::VecDeque::new();
        let state = state_with(&recent_files, true);
        for expected in [
            MenuAction::SaveAs,
            MenuAction::SelectAll,
            MenuAction::Mosaic,
            MenuAction::NewFromClipboard,
        ] {
            let ctx = egui::Context::default();
            let mut row_rect = None;
            let mut returned = None;
            let input = |events| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input(Vec::new()), |ui| {
                let (response, action) = popup_action_row(ui, &state, expected);
                row_rect = Some(response.rect);
                returned = action;
            });
            assert_eq!(returned, None);

            let center = match row_rect {
                Some(rect) => rect.center(),
                None => panic!("popup row rect was not allocated"),
            };
            let pointer_button = |pressed| egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for events in [
                vec![egui::Event::PointerMoved(center)],
                vec![egui::Event::PointerMoved(center), pointer_button(true)],
            ] {
                let _ = ctx.run_ui(input(events), |ui| {
                    let (_, action) = popup_action_row(ui, &state, expected);
                    returned = action;
                });
                assert_eq!(returned, None);
            }
            let _ = ctx.run_ui(
                input(vec![
                    egui::Event::PointerMoved(center),
                    pointer_button(false),
                ]),
                |ui| {
                    let (_, action) = popup_action_row(ui, &state, expected);
                    returned = action;
                },
            );
            assert_eq!(returned, Some(expected));
        }
    }
}
