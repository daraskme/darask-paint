//! メニューバー(SPEC §7、v6 §33、v12 §59)。
//!
//! 頻繁に使う 9 操作だけを常時表示し、関連操作を 9 個のグループタイルへ
//! まとめる。各項目が返す `MenuAction`、有効条件、ショートカット、確認
//! ダイアログや履歴の扱いは従来から変更しない。

use eframe::egui;

use crate::keymap::{self, Action};
use crate::ui::icons;

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

const BUTTON_W: f32 = 44.0;
const BUTTON_H: f32 = 40.0;
const ICON_SIZE: f32 = 17.0;
const LABEL_SIZE: f32 = 8.0;
const SEP_W: f32 = 6.0;
const ROW_ITEM_SPACING_X: f32 = 6.0;
const PANEL_MARGIN_H: f32 = 8.0;
const PANEL_MARGIN_V: f32 = 2.0;
const POPUP_MIN_WIDTH: f32 = 286.0;
const POPUP_ROW_H: f32 = 27.0;
const POPUP_ICON_SIZE: f32 = 17.0;
const FILE_RECENT_INSERT_INDEX: usize = 3;

type PaintFn = fn(&egui::Painter, egui::Rect, egui::Color32);

#[derive(Clone, Copy)]
struct MenuGroup {
    label: &'static str,
    icon: PaintFn,
    items: &'static [MenuAction],
}

#[derive(Clone, Copy)]
enum MenuSlot {
    Item(MenuAction),
    Group(MenuGroup),
    Separator,
}

const FILE_ITEMS: &[MenuAction] = &[
    MenuAction::SaveAs,
    MenuAction::OpenFolderAsPages,
    MenuAction::PasteFromFile,
    MenuAction::CloseTab,
    MenuAction::Exit,
];
const SELECTION_ITEMS: &[MenuAction] = &[
    MenuAction::SelectAll,
    MenuAction::Deselect,
    MenuAction::SelectInverse,
    MenuAction::Delete,
    MenuAction::CopyMerged,
    MenuAction::DuplicateSelectionToTab,
    MenuAction::CutSelectionToTab,
];
const SIZE_ITEMS: &[MenuAction] = &[
    MenuAction::ImageResize,
    MenuAction::CanvasResize,
    MenuAction::Crop,
];
const TRANSFORM_ITEMS: &[MenuAction] = &[
    MenuAction::FlipHorizontal,
    MenuAction::FlipVertical,
    MenuAction::RotateCw,
    MenuAction::RotateCcw,
];
const COLOR_ITEMS: &[MenuAction] = &[
    MenuAction::BrightnessContrast,
    MenuAction::HueSaturation,
    MenuAction::Invert,
    MenuAction::Grayscale,
    MenuAction::Mosaic,
];
const AI_ITEMS: &[MenuAction] = &[
    MenuAction::InpaintSelection,
    MenuAction::IopaintInpaint,
    MenuAction::DiffusionGenerate,
    MenuAction::DiffusionInpaint,
];
const LAYER_ITEMS: &[MenuAction] = &[
    MenuAction::LayerAdd,
    MenuAction::LayerDuplicate,
    MenuAction::LayerDelete,
    MenuAction::LayerMoveUp,
    MenuAction::LayerMoveDown,
    MenuAction::LayerMergeDown,
    MenuAction::LayerFlatten,
];
const VIEW_ITEMS: &[MenuAction] = &[
    MenuAction::ZoomIn,
    MenuAction::ZoomOut,
    MenuAction::Zoom100,
    MenuAction::FitWindow,
    MenuAction::TogglePixelGrid,
];
const OTHER_ITEMS: &[MenuAction] = &[
    MenuAction::OpenPreferences,
    MenuAction::ResetPanelLayout,
    MenuAction::About,
];

const FILE_GROUP: MenuGroup = MenuGroup {
    label: "ファイル…",
    icon: icons::paint_menu_file_group_icon,
    items: FILE_ITEMS,
};
const SELECTION_GROUP: MenuGroup = MenuGroup {
    label: "選択…",
    icon: icons::paint_menu_selection_group_icon,
    items: SELECTION_ITEMS,
};
const SIZE_GROUP: MenuGroup = MenuGroup {
    label: "サイズ…",
    icon: icons::paint_menu_size_group_icon,
    items: SIZE_ITEMS,
};
const TRANSFORM_GROUP: MenuGroup = MenuGroup {
    label: "変形…",
    icon: icons::paint_menu_transform_group_icon,
    items: TRANSFORM_ITEMS,
};
const COLOR_GROUP: MenuGroup = MenuGroup {
    label: "色調補正…",
    icon: icons::paint_menu_color_group_icon,
    items: COLOR_ITEMS,
};
const AI_GROUP: MenuGroup = MenuGroup {
    label: "AI・修復…",
    icon: icons::paint_menu_ai_group_icon,
    items: AI_ITEMS,
};
const LAYER_GROUP: MenuGroup = MenuGroup {
    label: "レイヤー…",
    icon: icons::paint_menu_layer_group_icon,
    items: LAYER_ITEMS,
};
const VIEW_GROUP: MenuGroup = MenuGroup {
    label: "表示…",
    icon: icons::paint_menu_view_group_icon,
    items: VIEW_ITEMS,
};
const OTHER_GROUP: MenuGroup = MenuGroup {
    label: "その他…",
    icon: icons::paint_menu_other_group_icon,
    items: OTHER_ITEMS,
};

/// SPEC §59 のトップレベル順序を保持する唯一のテーブル。
const TOP_LEVEL_SLOTS: &[MenuSlot] = &[
    MenuSlot::Item(MenuAction::New),
    MenuSlot::Item(MenuAction::Open),
    MenuSlot::Item(MenuAction::Save),
    MenuSlot::Group(FILE_GROUP),
    MenuSlot::Separator,
    MenuSlot::Item(MenuAction::Undo),
    MenuSlot::Item(MenuAction::Redo),
    MenuSlot::Item(MenuAction::Cut),
    MenuSlot::Item(MenuAction::Copy),
    MenuSlot::Item(MenuAction::Paste),
    MenuSlot::Item(MenuAction::FreeTransform),
    MenuSlot::Group(SELECTION_GROUP),
    MenuSlot::Separator,
    MenuSlot::Group(SIZE_GROUP),
    MenuSlot::Group(TRANSFORM_GROUP),
    MenuSlot::Group(COLOR_GROUP),
    MenuSlot::Group(AI_GROUP),
    MenuSlot::Separator,
    MenuSlot::Group(LAYER_GROUP),
    MenuSlot::Separator,
    MenuSlot::Group(VIEW_GROUP),
    MenuSlot::Group(OTHER_GROUP),
];

impl MenuAction {
    fn short_label(self) -> &'static str {
        match self {
            Self::New => "新規",
            Self::Open => "開く",
            Self::OpenFolderAsPages => "ページフォルダ",
            Self::OpenRecent(_) => "最近開く",
            Self::Save => "保存",
            Self::SaveAs => "別名保存",
            Self::CloseTab => "タブ閉じ",
            Self::Exit => "終了",
            Self::Undo => "元に戻す",
            Self::Redo => "やり直し",
            Self::Cut => "切り取り",
            Self::Copy => "コピー",
            Self::CopyMerged => "結合コピー",
            Self::Paste => "貼り付け",
            Self::PasteFromFile => "画像貼付",
            Self::Delete => "削除",
            Self::SelectAll => "全選択",
            Self::Deselect => "選択解除",
            Self::SelectInverse => "選択反転",
            Self::InpaintSelection => "修復",
            Self::IopaintInpaint => "AI修復",
            Self::DiffusionGenerate => "AI生成",
            Self::DiffusionInpaint => "AI置換",
            Self::FreeTransform => "自由変形",
            Self::ImageResize => "画像サイズ",
            Self::CanvasResize => "キャンバス",
            Self::Crop => "トリミング",
            Self::DuplicateSelectionToTab => "選択複製",
            Self::CutSelectionToTab => "切り出し",
            Self::FlipHorizontal => "左右反転",
            Self::FlipVertical => "上下反転",
            Self::RotateCw => "右回転",
            Self::RotateCcw => "左回転",
            Self::BrightnessContrast => "明暗調整",
            Self::HueSaturation => "色相彩度",
            Self::Invert => "階調反転",
            Self::Grayscale => "グレー化",
            Self::Mosaic => "モザイク",
            Self::ZoomIn => "拡大",
            Self::ZoomOut => "縮小",
            Self::Zoom100 => "100%",
            Self::FitWindow => "全体表示",
            Self::TogglePixelGrid => "グリッド",
            Self::ResetPanelLayout => "パネル配置",
            Self::LayerAdd => "追加",
            Self::LayerDuplicate => "複製",
            Self::LayerDelete => "削除",
            Self::LayerMoveUp => "上へ",
            Self::LayerMoveDown => "下へ",
            Self::LayerMergeDown => "下と結合",
            Self::LayerFlatten => "画像統合",
            Self::About => "情報",
            Self::OpenPreferences => "設定",
        }
    }

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
            Self::OpenPreferences => "設定",
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
            Self::LayerAdd | Self::LayerDuplicate => state.can_add_layer,
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

    fn icon(self) -> PaintFn {
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

impl MenuGroup {
    fn enabled(self, state: &MenuState) -> bool {
        self.items.iter().any(|action| action.enabled(state))
    }

    fn has_recent_files(self) -> bool {
        self.label == FILE_GROUP.label
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    enabled: bool,
    selected: bool,
    paint: PaintFn,
    label: &'static str,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(BUTTON_W, BUTTON_H), sense);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        let mut painter = ui.painter().clone();
        if !enabled {
            painter.multiply_opacity(ui.visuals().disabled_alpha());
        }
        painter.rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 12.0),
            egui::vec2(ICON_SIZE, ICON_SIZE),
        );
        paint(&painter, icon_rect, visuals.fg_stroke.color);
        painter.with_clip_rect(rect.shrink(1.0)).text(
            egui::pos2(rect.center().x, rect.bottom() - 3.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(LABEL_SIZE),
            visuals.fg_stroke.color,
        );
    }
    response
}

fn group_separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SEP_W, BUTTON_H), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().vline(rect.center().x, rect.y_range(), stroke);
    }
}

fn slot_width(slot: &MenuSlot) -> f32 {
    match slot {
        MenuSlot::Separator => SEP_W,
        MenuSlot::Item(_) | MenuSlot::Group(_) => BUTTON_W,
    }
}

fn pack_rows(slots: &[MenuSlot], avail_width: f32, spacing: f32) -> Vec<Vec<usize>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut used = 0.0;
    for (index, slot) in slots.iter().enumerate() {
        let additional = slot_width(slot) + if row.is_empty() { 0.0 } else { spacing };
        if !row.is_empty() && used + additional > avail_width {
            while matches!(row.last(), Some(last) if matches!(slots[*last], MenuSlot::Separator)) {
                row.pop();
            }
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
            }
            used = 0.0;
        }
        if row.is_empty() && matches!(slot, MenuSlot::Separator) {
            continue;
        }
        used += slot_width(slot) + if row.is_empty() { 0.0 } else { spacing };
        row.push(index);
    }
    while matches!(row.last(), Some(last) if matches!(slots[*last], MenuSlot::Separator)) {
        row.pop();
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn popup_action_row(
    ui: &mut egui::Ui,
    state: &MenuState,
    action: MenuAction,
) -> (egui::Response, Option<MenuAction>) {
    let enabled = action.enabled(state);
    let selected = action.selected(state);
    let width = ui.available_width().max(POPUP_MIN_WIDTH);
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
        let visuals = ui.style().interact_selectable(&response, selected);
        let mut painter = ui.painter().clone();
        if !enabled {
            painter.multiply_opacity(ui.visuals().disabled_alpha());
        }
        painter.rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::vec2(POPUP_ICON_SIZE, POPUP_ICON_SIZE),
        );
        (action.icon())(&painter, icon_rect, visuals.fg_stroke.color);
        painter.text(
            egui::pos2(rect.left() + 28.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            action.full_label(),
            egui::TextStyle::Button.resolve(ui.style()),
            visuals.fg_stroke.color,
        );
        let shortcut = action.shortcut_label();
        if !shortcut.is_empty() {
            painter.text(
                egui::pos2(rect.right() - 6.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                shortcut,
                egui::TextStyle::Button.resolve(ui.style()),
                visuals.fg_stroke.color,
            );
        }
    }
    let clicked = response.clicked().then_some(action);
    (response, clicked)
}

fn show_action_rows(
    ui: &mut egui::Ui,
    state: &MenuState,
    actions: &[MenuAction],
) -> Option<MenuAction> {
    for action in actions {
        let (_, clicked) = popup_action_row(ui, state, *action);
        if clicked.is_some() {
            return clicked;
        }
    }
    None
}

fn recent_file_row(ui: &mut egui::Ui, index: usize, path: &std::path::Path) -> Option<MenuAction> {
    let full_path = path.to_string_lossy().into_owned();
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| full_path.clone());
    let width = ui.available_width().max(POPUP_MIN_WIDTH);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, POPUP_ROW_H), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::vec2(POPUP_ICON_SIZE, POPUP_ICON_SIZE),
        );
        icons::paint_recent_files_icon(ui.painter(), icon_rect, visuals.fg_stroke.color);
        ui.painter().text(
            egui::pos2(rect.left() + 28.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::TextStyle::Button.resolve(ui.style()),
            visuals.fg_stroke.color,
        );
    }
    response
        .on_hover_text(full_path)
        .clicked()
        .then_some(MenuAction::OpenRecent(index))
}

fn show_recent_files(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    ui.separator();
    ui.weak("最近使ったファイル");
    if state.recent_files.is_empty() {
        ui.weak("(なし)");
        return None;
    }
    for (index, path) in state.recent_files.iter().enumerate() {
        if let Some(action) = recent_file_row(ui, index, path) {
            return Some(action);
        }
    }
    None
}

fn show_group_contents(
    ui: &mut egui::Ui,
    group: MenuGroup,
    state: &MenuState,
) -> Option<MenuAction> {
    ui.set_min_width(POPUP_MIN_WIDTH);
    if !group.has_recent_files() {
        return show_action_rows(ui, state, group.items);
    }
    if let Some(action) = show_action_rows(ui, state, &group.items[..FILE_RECENT_INSERT_INDEX]) {
        return Some(action);
    }
    if let Some(action) = show_recent_files(ui, state) {
        return Some(action);
    }
    ui.separator();
    show_action_rows(ui, state, &group.items[FILE_RECENT_INSERT_INDEX..])
}

fn group_button(ui: &mut egui::Ui, group: MenuGroup, state: &MenuState) -> Option<MenuAction> {
    let response = icon_button(ui, group.enabled(state), false, group.icon, group.label)
        .on_hover_text(group.label);
    let mut action = None;
    egui::Popup::menu(&response).show(|ui| {
        if let Some(selected) = show_group_contents(ui, group, state) {
            action = Some(selected);
            ui.close();
        }
    });
    action
}

/// メニューバーを描画し、クリックされた操作を返す。
pub fn show(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    let mut action = None;
    let spacing = ui.spacing().item_spacing;
    let avail_width = (ui.available_width() - PANEL_MARGIN_H * 2.0).max(0.0);
    let rows = pack_rows(TOP_LEVEL_SLOTS, avail_width, ROW_ITEM_SPACING_X);
    let row_count = (rows.len().max(1) - 1) as f32;
    let content_height = rows.len().max(1) as f32 * BUTTON_H + row_count * spacing.y;
    let panel_height = content_height + PANEL_MARGIN_V * 2.0;

    egui::Panel::top("menu_bar")
        .exact_size(panel_height)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = ROW_ITEM_SPACING_X;
            for row in &rows {
                ui.horizontal(|ui| {
                    for index in row {
                        match TOP_LEVEL_SLOTS[*index] {
                            MenuSlot::Item(item) => {
                                let response = icon_button(
                                    ui,
                                    item.enabled(state),
                                    item.selected(state),
                                    item.icon(),
                                    item.short_label(),
                                )
                                .on_hover_text(item.tooltip());
                                if response.clicked() {
                                    action = Some(item);
                                }
                            }
                            MenuSlot::Group(group) => {
                                if let Some(selected) = group_button(ui, group, state) {
                                    action = Some(selected);
                                }
                            }
                            MenuSlot::Separator => group_separator(ui),
                        }
                    }
                });
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

    fn dummy_item() -> MenuSlot {
        MenuSlot::Item(MenuAction::About)
    }

    #[test]
    fn slot_width_separator_is_narrower_than_an_item() {
        assert_eq!(slot_width(&MenuSlot::Separator), SEP_W);
        assert_eq!(slot_width(&dummy_item()), BUTTON_W);
    }

    #[test]
    fn pack_rows_obeys_width_and_spacing() {
        let slots: Vec<MenuSlot> = (0..5).map(|_| dummy_item()).collect();
        assert_eq!(
            pack_rows(&slots, BUTTON_W * 3.0, 0.0),
            vec![vec![0, 1, 2], vec![3, 4]]
        );
        let three: Vec<MenuSlot> = (0..3).map(|_| dummy_item()).collect();
        assert_eq!(
            pack_rows(&three, BUTTON_W * 2.0 + 4.0, 4.0),
            vec![vec![0, 1], vec![2]]
        );
    }

    #[test]
    fn pack_rows_places_every_index_once() {
        let slots: Vec<MenuSlot> = (0..10).map(|_| dummy_item()).collect();
        let rows = pack_rows(&slots, BUTTON_W * 4.0, 2.0);
        assert!(rows.iter().all(|row| !row.is_empty()));
        let mut indices: Vec<usize> = rows.iter().flatten().copied().collect();
        indices.sort_unstable();
        assert_eq!(indices, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn pack_rows_handles_too_narrow_width() {
        let slots = vec![dummy_item(), dummy_item()];
        assert_eq!(pack_rows(&slots, 0.0, 0.0), vec![vec![0], vec![1]]);
    }

    #[test]
    fn standard_widths_use_one_and_two_rows() {
        let rows_1280 = pack_rows(
            TOP_LEVEL_SLOTS,
            1280.0 - PANEL_MARGIN_H * 2.0,
            ROW_ITEM_SPACING_X,
        );
        let rows_640 = pack_rows(
            TOP_LEVEL_SLOTS,
            640.0 - PANEL_MARGIN_H * 2.0,
            ROW_ITEM_SPACING_X,
        );
        assert_eq!(rows_1280.len(), 1, "1280px の実測行数: {}", rows_1280.len());
        assert_eq!(rows_640.len(), 2, "640px の実測行数: {}", rows_640.len());
        for row in rows_640 {
            assert!(!matches!(TOP_LEVEL_SLOTS[row[0]], MenuSlot::Separator));
            if let Some(last) = row.last() {
                assert!(!matches!(TOP_LEVEL_SLOTS[*last], MenuSlot::Separator));
            }
        }
    }

    #[test]
    fn top_level_order_matches_spec_59() {
        let labels: Vec<&str> = TOP_LEVEL_SLOTS
            .iter()
            .map(|slot| match slot {
                MenuSlot::Item(action) => action.full_label(),
                MenuSlot::Group(group) => group.label,
                MenuSlot::Separator => "|",
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "新規",
                "開く",
                "上書き保存",
                "ファイル…",
                "|",
                "元に戻す",
                "やり直し",
                "切り取り",
                "コピー",
                "貼り付け",
                "自由変形",
                "選択…",
                "|",
                "サイズ…",
                "変形…",
                "色調補正…",
                "AI・修復…",
                "|",
                "レイヤー…",
                "|",
                "表示…",
                "その他…",
            ]
        );
    }

    #[test]
    fn all_53_menu_entries_appear_exactly_once() {
        let mut placed = Vec::new();
        for slot in TOP_LEVEL_SLOTS {
            match slot {
                MenuSlot::Item(action) => placed.push(*action),
                MenuSlot::Group(group) if group.has_recent_files() => {
                    placed.extend_from_slice(&group.items[..FILE_RECENT_INSERT_INDEX]);
                    placed.push(MenuAction::OpenRecent(0));
                    placed.extend_from_slice(&group.items[FILE_RECENT_INSERT_INDEX..]);
                }
                MenuSlot::Group(group) => placed.extend_from_slice(group.items),
                MenuSlot::Separator => {}
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
        assert_eq!(placed.len(), 53);
    }

    #[test]
    fn group_is_disabled_only_when_every_item_is_disabled() {
        let recent_files = std::collections::VecDeque::new();
        let mut disabled = state_with(&recent_files, false);
        disabled.background_job_running = true;
        assert!(!AI_GROUP.enabled(&disabled));
        assert!(!LAYER_GROUP.enabled(&disabled));

        let mut one_enabled = state_with(&recent_files, false);
        one_enabled.has_selection = true;
        assert!(AI_GROUP.enabled(&one_enabled));
        one_enabled.has_selection = false;
        one_enabled.can_add_layer = true;
        assert!(LAYER_GROUP.enabled(&one_enabled));
    }

    #[test]
    fn file_group_keeps_recent_files_between_paste_and_close() {
        assert_eq!(FILE_RECENT_INSERT_INDEX, 3);
        assert_eq!(
            FILE_GROUP.items,
            [
                MenuAction::SaveAs,
                MenuAction::OpenFolderAsPages,
                MenuAction::PasteFromFile,
                MenuAction::CloseTab,
                MenuAction::Exit,
            ]
        );
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
            let _ = ctx.run_ui(input(vec![egui::Event::PointerMoved(center)]), |ui| {
                let (_, action) = popup_action_row(ui, &state, expected);
                returned = action;
            });
            let pointer_button = |pressed| egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let _ = ctx.run_ui(
                input(vec![
                    egui::Event::PointerMoved(center),
                    pointer_button(true),
                ]),
                |ui| {
                    let (_, action) = popup_action_row(ui, &state, expected);
                    returned = action;
                },
            );
            assert_eq!(returned, None);

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

    #[test]
    fn every_top_level_label_is_short_and_nonempty() {
        for slot in TOP_LEVEL_SLOTS {
            let label = match slot {
                MenuSlot::Item(action) => action.short_label(),
                MenuSlot::Group(group) => group.label,
                MenuSlot::Separator => continue,
            };
            assert!(!label.is_empty());
            assert!(label.chars().count() <= 6, "label too long: {label}");
        }
    }
}
