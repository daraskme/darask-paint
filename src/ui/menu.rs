//! メニューバー(SPEC §7、v6 §33)。
//!
//! v1〜v5 はドロップダウン式(`egui::menu::bar` + `ui.menu_button`)だった。
//! v6 M4(ARCHITECTURE.md §18.1)で「クリックでの開閉が無い、常時表示の
//! アイコンボタン行」に置き換えた。各項目の**実際の意味・確認ダイアログ・
//! ショートカットは一切変えていない**(見た目とレイアウトのみの変更、
//! ARCHITECTURE.md §18.6 落とし穴4)。各項目は `MenuAction` を返し、
//! `app.rs` が実行する(rfd のネイティブダイアログ呼び出しはフレーム処理の
//! 外側で行う必要があるため、ここではまだ実行せず「何を要求されたか」だけを
//! 返す、ARCHITECTURE.md §12-9)。有効/無効(元に戻す・やり直し・選択系・
//! トリミング)は `MenuState` で受け取る。
//!
//! 「最近使ったファイル」だけは可変長リストのため、SPEC §33 が認める唯一の
//! 例外としてクリックで軽量ポップアップ(`egui::Popup::menu` — `ui.menu_button`
//! を内部で支えているのと同じ機構、`egui::containers::menu::MenuButton::ui`
//! 参照)を開く。他は全てワンクリックで即座に `MenuAction` を返す。
//!
//! ## 折り返しの実装について(`ui.horizontal_wrapped` を使わない理由)
//!
//! `show` はまず `ui.available_width()` から「何個のボタン/区切りが 1 行に
//! 収まるか」を貪欲法(グリーディ)で `pack_rows` により事前に計算し、
//! その行数から `egui::Panel::top` の高さを `exact_size` で厳密に確定して
//! から、行ごとに独立した(折り返しを伴わない)`ui.horizontal` を並べる。
//!
//! `ui.horizontal_wrapped` + `Panel` の既定の自動高さ計算(前フレームの
//! パネル高さを引き継ぎ、実際のコンテンツ高さへ数フレームかけて収束する
//! 挙動)を使わない理由: ウィンドウ幅が変わった直後の 1 フレーム目は
//! 「まだ前の幅のときの行数」でパネル高さが決まってしまい、次の再描画
//! (マウス移動等の入力)が来るまで正しい行数で表示されない可能性がある。
//! このアプリは「入力がない限り再描画しない」(CLAUDE.md 鉄則、無条件の
//! `request_repaint()` 禁止)ため、リサイズ直後にちょうど再描画が起きない
//! フレームで実機確認・スクリーンショットを取ると、実際には数フレーム後に
//! 解消する一時的な行不足が「項目が消えている」ように見えることがある
//! (このモジュールの実装を固める過程で複数回遭遇した)。`pack_rows` は
//! 現在のフレームの `ui.available_width()` だけから行数を導出できるので、
//! `exact_size` と組み合わせれば**リサイズ直後の 1 フレーム目から**正しい
//! 高さで確定させられ、この手のフレーム依存の見え方の揺れを避けられる。

use eframe::egui;

use crate::keymap::{self, Action};
use crate::ui::icons;

/// クリックされたメニュー項目(まだ副作用は起こさない。`app.rs` が実行する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    New,
    Open,
    /// v4 §26: 「最近使ったファイル」サブメニューの `index` 番目
    /// (`MenuState::recent_files` と同じ添字)。`PathBuf` を持たせると
    /// `Copy` にできなくなるため、添字だけを運ぶ(`app.rs::open_recent_file`
    /// が実際のパスを引く)。
    OpenRecent(usize),
    Save,
    SaveAs,
    /// v5 §30/§32(V5-M3、ARCHITECTURE.md §17.6): 「タブを閉じる (Ctrl+W)」。
    CloseTab,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Deselect,
    /// v6 §33(ARCHITECTURE.md §18.1): 編集メニューに新規追加された
    /// 「自由変形」(Ctrl+T と同じ、`app.rs::free_transform` を呼ぶだけ。
    /// v3 §18 で実装済みの機能に、メニューからのアクセス経路を追加する)。
    FreeTransform,
    ImageResize,
    CanvasResize,
    Crop,
    /// v5 §31(ARCHITECTURE.md §17.5): 「選択範囲を新規タブに複製」。
    DuplicateSelectionToTab,
    FlipHorizontal,
    FlipVertical,
    RotateCw,
    RotateCcw,
    // -- v4 §24: 色調補正(ARCHITECTURE.md §16.5) --------------------------
    BrightnessContrast,
    HueSaturation,
    Invert,
    Grayscale,
    ZoomIn,
    ZoomOut,
    Zoom100,
    FitWindow,
    // -- v4 §25: ピクセルグリッド --------------------------------------------
    TogglePixelGrid,
    // -- v2 §13: レイヤーメニュー(ARCHITECTURE.md §14.8 V2-M2) -----------
    LayerAdd,
    LayerDuplicate,
    LayerDelete,
    LayerMoveUp,
    LayerMoveDown,
    LayerMergeDown,
    LayerFlatten,
    // -- v4 §26: ヘルプメニュー ------------------------------------------
    About,
    /// v6 §33/§34(ARCHITECTURE.md §18.1/§18.2): 「その他」グループの設定
    /// (歯車)ボタン。`ui/toolbar.rs::ToolbarAction::OpenPreferences` と
    /// 同じ `app.rs::open_preferences_modal` を呼ぶだけ(ツールバー側の歯車
    /// ボタンはそのまま残す — 2 箇所からアクセスできても害はない)。
    OpenPreferences,
}

impl MenuAction {
    /// タイル下段へ常時表示する短い日本語ラベル。完全な操作名と
    /// ショートカットは従来どおりツールチップへ残す。
    fn short_label(self) -> &'static str {
        match self {
            Self::New => "新規",
            Self::Open => "開く",
            Self::OpenRecent(_) => "最近開く",
            Self::Save => "保存",
            Self::SaveAs => "別名保存",
            Self::CloseTab => "タブ閉じ",
            Self::Exit => "終了",
            Self::Undo => "元に戻す",
            Self::Redo => "やり直し",
            Self::Cut => "切り取り",
            Self::Copy => "コピー",
            Self::Paste => "貼り付け",
            Self::Delete => "削除",
            Self::SelectAll => "全選択",
            Self::Deselect => "選択解除",
            Self::FreeTransform => "自由変形",
            Self::ImageResize => "画像サイズ",
            Self::CanvasResize => "キャンバス",
            Self::Crop => "トリミング",
            Self::DuplicateSelectionToTab => "選択複製",
            Self::FlipHorizontal => "左右反転",
            Self::FlipVertical => "上下反転",
            Self::RotateCw => "右回転",
            Self::RotateCcw => "左回転",
            Self::BrightnessContrast => "明暗調整",
            Self::HueSaturation => "色相彩度",
            Self::Invert => "階調反転",
            Self::Grayscale => "グレー化",
            Self::ZoomIn => "拡大",
            Self::ZoomOut => "縮小",
            Self::Zoom100 => "100%",
            Self::FitWindow => "全体表示",
            Self::TogglePixelGrid => "グリッド",
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
}

/// メニュー項目の有効/無効判定に使う状態。`recent_files` を借用するため
/// ライフタイム付き(v4 §26)。
pub struct MenuState<'a> {
    pub can_undo: bool,
    pub can_redo: bool,
    /// 選択または浮動片がある(切り取り/コピー/削除/トリミングを有効にする)。
    pub has_selection: bool,
    /// v5 §31(ARCHITECTURE.md §17.6): 「選択範囲を新規タブに複製」の有効/
    /// 無効(`has_selection` と同じ値でよい)。
    pub can_duplicate_selection_to_tab: bool,
    // -- v2 §13: レイヤーメニューの有効/無効(document.rs の各操作の成否
    // 条件と 1:1 に対応させる、ARCHITECTURE.md §14.8 V2-M2) -------------
    pub can_add_layer: bool,
    pub can_delete_layer: bool,
    pub can_move_layer_up: bool,
    pub can_move_layer_down: bool,
    pub can_merge_layer_down: bool,
    pub can_flatten_layers: bool,
    /// v4 §25: 表示メニューの「ピクセルグリッド」チェック表示用(実際の
    /// トグルは `MenuAction::TogglePixelGrid` を経由して app.rs が行う)。
    pub pixel_grid_visible: bool,
    /// v4 §26: 「ファイル > 最近使ったファイル」サブメニューの中身
    /// (先頭が最新、`app.rs::recent_files` と同じ順序)。
    pub recent_files: &'a std::collections::VecDeque<std::path::PathBuf>,
}

/// メニューアイコンボタンの外形サイズ(SPEC §33: 「約 20×24px の正方形
/// ボタン」)。アイコン本体は `ui/icons.rs` の流儀どおり正方形の相対座標
/// (0..1)から組み立てる前提なので、ボタンの縦横比が 1:1 でなくても
/// 描画前に正方形の `icon_rect` を切り出す(下記 `icon_button`)。
const BUTTON_W: f32 = 44.0;
const BUTTON_H: f32 = 40.0;
const ICON_SIZE: f32 = 17.0;
const LABEL_SIZE: f32 = 8.0;
/// グループ間の区切り線の幅(行分割の計算にも使う、`slot_width` 参照)。
const SEP_W: f32 = 6.0;

/// アイコン描画関数の共通シグネチャ(`ui/icons.rs` の全関数がこれに従う)。
/// キャプチャ変数を持たない関数ポインタなので `Copy` にでき、`Slot` の
/// テーブルに素直に収められる。
type PaintFn = fn(&egui::Painter, egui::Rect, egui::Color32);

/// 1 個のアイコンボタン(`ui/toolbar.rs::tool_button` と同じ「hover 背景を
/// 自前で塗ってからアイコンを重ねる」流儀、egui `Button` の text 用 API に
/// 依存しない)。`selected` はピクセルグリッドのようなトグル状態表示にのみ
/// true にする(SPEC §33: 「他のツールボタンと同じ .selected(bool)
/// ハイライト」)。
///
/// `enabled == false` のときは `ui.add_enabled_ui`(内部で `ui.scope` =
/// 子 `Ui` を都度生成する)を使わず、`ui.painter()` を複製して
/// `Painter::multiply_opacity` で薄くしたものに描くだけにする(クリックも
/// `Sense::hover()` にして無効化する)。42 個ものボタンを並べる行で毎回
/// 子 `Ui` を生成するのは無駄なので、単色ペインターの複製だけで済ませる
/// 軽量な実装にした。
fn icon_button(
    ui: &mut egui::Ui,
    enabled: bool,
    selected: bool,
    paint: PaintFn,
    label: &'static str,
) -> egui::Response {
    let size = egui::vec2(BUTTON_W, BUTTON_H);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
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
        let icon_center = egui::pos2(rect.center().x, rect.top() + 12.0);
        let icon_rect = egui::Rect::from_center_size(icon_center, egui::vec2(ICON_SIZE, ICON_SIZE));
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

/// SPEC §33: グループ間の薄い区切り線(「カテゴリごとに薄い区切り線を入れて
/// 視覚的にグループ化する」)。`SEP_W` 固定の縦線を自前で描く(モジュール
/// ドキュメントコメント参照: `ui.separator()` は使わない)。
fn group_separator(ui: &mut egui::Ui) {
    let size = egui::vec2(SEP_W, BUTTON_H);
    let (rect, _response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().vline(rect.center().x, rect.y_range(), stroke);
    }
}

/// SPEC §33: 「最近使ったファイル」だけの例外(クリックで小さなポップアップ
/// リストを開く、モジュールのドキュメントコメント参照)。
fn recent_files_button(
    ui: &mut egui::Ui,
    recent_files: &std::collections::VecDeque<std::path::PathBuf>,
) -> Option<MenuAction> {
    let mut action = None;
    let response = icon_button(ui, true, false, icons::paint_recent_files_icon, "最近開く")
        .on_hover_text("最近使ったファイル");
    egui::Popup::menu(&response).show(|ui| {
        if recent_files.is_empty() {
            ui.weak("(なし)");
        } else {
            for (i, path) in recent_files.iter().enumerate() {
                let full_path = path.to_string_lossy().into_owned();
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| full_path.clone());
                if ui.button(label).on_hover_text(&full_path).clicked() {
                    action = Some(MenuAction::OpenRecent(i));
                    ui.close();
                }
            }
        }
    });
    action
}

/// 単発アクションのアイコンボタン 1 個ぶんのデータ(モジュールドキュメント
/// コメントの「行分割を自前で行う」ためのテーブル行)。
struct MenuItem {
    enabled: bool,
    tooltip: Tooltip,
    paint: PaintFn,
    action: MenuAction,
}

#[derive(Clone, Copy)]
enum Tooltip {
    Plain(&'static str),
    Shortcut(&'static str, Action),
}

/// アイコン行の 1 スロット。`ui.horizontal_wrapped` を使わず、
/// `ui.available_width()` から自前で行分割する(モジュールドキュメント
/// コメント参照)。
enum Slot {
    Item(MenuItem),
    /// SPEC §33: ピクセルグリッド表示のようなトグル状態ボタン。
    Toggle {
        selected: bool,
        tooltip: &'static str,
        paint: PaintFn,
        action: MenuAction,
    },
    /// SPEC §33 唯一の例外(ポップアップ、`recent_files_button` 参照)。
    RecentFiles,
    /// SPEC §33: グループ間の区切り線。
    Sep,
}

fn mi(enabled: bool, tooltip: &'static str, paint: PaintFn, action: MenuAction) -> Slot {
    Slot::Item(MenuItem {
        enabled,
        tooltip: Tooltip::Plain(tooltip),
        paint,
        action,
    })
}

fn mi_shortcut(
    enabled: bool,
    label: &'static str,
    shortcut: Action,
    paint: PaintFn,
    action: MenuAction,
) -> Slot {
    Slot::Item(MenuItem {
        enabled,
        tooltip: Tooltip::Shortcut(label, shortcut),
        paint,
        action,
    })
}

/// レイアウトの行分割に使う「このスロットの幅」。
fn slot_width(slot: &Slot) -> f32 {
    match slot {
        Slot::Sep => SEP_W,
        _ => BUTTON_W,
    }
}

/// ARCHITECTURE.md §18.1: グループ順序と区切りは
/// ファイル → 編集 → 画像 → レイヤー → 表示 → その他(ヘルプ/設定)。
/// SPEC §33 の全項目を宣言順どおりに並べたテーブルを組み立てる(実際の
/// 副作用は起こさない、`show` が行分割・描画・クリック判定を行う)。
fn build_slots(state: &MenuState) -> Vec<Slot> {
    vec![
        // -- ファイル ---------------------------------------------------
        mi_shortcut(
            true,
            "新規",
            Action::New,
            icons::paint_new_document_icon,
            MenuAction::New,
        ),
        mi_shortcut(
            true,
            "開く",
            Action::Open,
            icons::paint_open_icon,
            MenuAction::Open,
        ),
        mi_shortcut(
            true,
            "上書き保存",
            Action::Save,
            icons::paint_save_icon,
            MenuAction::Save,
        ),
        mi_shortcut(
            true,
            "名前を付けて保存",
            Action::SaveAs,
            icons::paint_save_as_icon,
            MenuAction::SaveAs,
        ),
        // v4 §26: 「最近使ったファイル」ポップアップ(SPEC §33 の唯一の例外)。
        // SPEC §33 のファイルグループ明記順(新規/開く/上書き保存/名前を付けて
        // 保存/最近使ったファイル/タブを閉じる/終了)どおり、「名前を付けて
        // 保存」の直後・「タブを閉じる」の直前に置く。
        Slot::RecentFiles,
        // v5 §17.6: 「ファイルメニューに「タブを閉じる (Ctrl+W)」を追加
        // (名前を付けて保存の後、終了の前)」。
        mi_shortcut(
            true,
            "タブを閉じる",
            Action::CloseTab,
            icons::paint_close_tab_icon,
            MenuAction::CloseTab,
        ),
        // Alt+F4 は OS/ウィンドウマネージャが `close_requested` として通知
        // するものであり egui が消費するショートカットではないため、
        // `keymap::KEYMAP` の対象外(`keymap` モジュールドキュメントコメント
        // 参照)。表記は固定文字列のままでよい。
        mi(
            true,
            "終了 (Alt+F4)",
            icons::paint_exit_icon,
            MenuAction::Exit,
        ),
        Slot::Sep,
        // -- 編集 ---------------------------------------------------------
        mi_shortcut(
            state.can_undo,
            "元に戻す",
            Action::Undo,
            icons::paint_undo_icon,
            MenuAction::Undo,
        ),
        mi_shortcut(
            state.can_redo,
            "やり直し",
            Action::Redo,
            icons::paint_redo_icon,
            MenuAction::Redo,
        ),
        mi_shortcut(
            state.has_selection,
            "切り取り",
            Action::Cut,
            icons::paint_cut_icon,
            MenuAction::Cut,
        ),
        mi_shortcut(
            state.has_selection,
            "コピー",
            Action::Copy,
            icons::paint_copy_icon,
            MenuAction::Copy,
        ),
        mi_shortcut(
            true,
            "貼り付け",
            Action::Paste,
            icons::paint_paste_icon,
            MenuAction::Paste,
        ),
        mi_shortcut(
            state.has_selection,
            "削除",
            Action::Delete,
            icons::paint_delete_icon,
            MenuAction::Delete,
        ),
        mi_shortcut(
            true,
            "すべて選択",
            Action::SelectAll,
            icons::paint_select_all_icon,
            MenuAction::SelectAll,
        ),
        mi_shortcut(
            state.has_selection,
            "選択解除",
            Action::Deselect,
            icons::paint_deselect_icon,
            MenuAction::Deselect,
        ),
        // v6 §33: 編集メニューに新規追加(`MenuAction::FreeTransform`
        // ドキュメントコメント参照)。
        mi_shortcut(
            true,
            "自由変形",
            Action::FreeTransform,
            icons::paint_free_transform_icon,
            MenuAction::FreeTransform,
        ),
        Slot::Sep,
        // -- 画像 ---------------------------------------------------------
        mi(
            true,
            "画像サイズ変更…",
            icons::paint_image_resize_icon,
            MenuAction::ImageResize,
        ),
        mi(
            true,
            "キャンバスサイズ変更…",
            icons::paint_canvas_resize_icon,
            MenuAction::CanvasResize,
        ),
        mi(
            state.has_selection,
            "選択範囲でトリミング",
            icons::paint_crop_icon,
            MenuAction::Crop,
        ),
        // v5 §31: 「選択範囲でトリミング」の直後に追加。
        mi(
            state.can_duplicate_selection_to_tab,
            "選択範囲を新規タブに複製",
            icons::paint_duplicate_to_tab_icon,
            MenuAction::DuplicateSelectionToTab,
        ),
        mi(
            true,
            "左右反転",
            icons::paint_flip_horizontal_icon,
            MenuAction::FlipHorizontal,
        ),
        mi(
            true,
            "上下反転",
            icons::paint_flip_vertical_icon,
            MenuAction::FlipVertical,
        ),
        mi(
            true,
            "右に90°回転",
            icons::paint_rotate_cw_icon,
            MenuAction::RotateCw,
        ),
        mi(
            true,
            "左に90°回転",
            icons::paint_rotate_ccw_icon,
            MenuAction::RotateCcw,
        ),
        // v4 §24: 色調補正(以前はサブメニューだったが、v6 で他の項目と
        // 同列のアイコンボタンに展開する — 挙動は一切変えない、
        // ARCHITECTURE.md §18.6 落とし穴4)。
        mi(
            true,
            "明るさ・コントラスト…",
            icons::paint_brightness_contrast_icon,
            MenuAction::BrightnessContrast,
        ),
        mi_shortcut(
            true,
            "色相・彩度・明度…",
            Action::HueSaturation,
            icons::paint_hue_saturation_icon,
            MenuAction::HueSaturation,
        ),
        mi_shortcut(
            true,
            "階調の反転",
            Action::Invert,
            icons::paint_invert_icon,
            MenuAction::Invert,
        ),
        mi_shortcut(
            true,
            "グレースケール化",
            Action::Grayscale,
            icons::paint_grayscale_icon,
            MenuAction::Grayscale,
        ),
        Slot::Sep,
        // -- レイヤー -------------------------------------------------------
        mi_shortcut(
            state.can_add_layer,
            "新規レイヤー",
            Action::LayerAdd,
            icons::paint_layer_add_icon,
            MenuAction::LayerAdd,
        ),
        mi_shortcut(
            state.can_add_layer,
            "レイヤーを複製",
            Action::LayerDuplicate,
            icons::paint_layer_duplicate_icon,
            MenuAction::LayerDuplicate,
        ),
        mi(
            state.can_delete_layer,
            "レイヤーを削除",
            icons::paint_layer_delete_icon,
            MenuAction::LayerDelete,
        ),
        mi(
            state.can_move_layer_up,
            "上へ移動",
            icons::paint_layer_move_up_icon,
            MenuAction::LayerMoveUp,
        ),
        mi(
            state.can_move_layer_down,
            "下へ移動",
            icons::paint_layer_move_down_icon,
            MenuAction::LayerMoveDown,
        ),
        mi_shortcut(
            state.can_merge_layer_down,
            "下のレイヤーと結合",
            Action::LayerMergeDown,
            icons::paint_layer_merge_down_icon,
            MenuAction::LayerMergeDown,
        ),
        mi_shortcut(
            state.can_flatten_layers,
            "画像の統合",
            Action::LayerFlatten,
            icons::paint_layer_flatten_icon,
            MenuAction::LayerFlatten,
        ),
        Slot::Sep,
        // -- 表示 -----------------------------------------------------------
        mi_shortcut(
            true,
            "拡大",
            Action::ZoomIn,
            icons::paint_zoom_in_icon,
            MenuAction::ZoomIn,
        ),
        mi_shortcut(
            true,
            "縮小",
            Action::ZoomOut,
            icons::paint_zoom_out_icon,
            MenuAction::ZoomOut,
        ),
        mi_shortcut(
            true,
            "100%",
            Action::Zoom100,
            icons::paint_zoom_100_icon,
            MenuAction::Zoom100,
        ),
        mi_shortcut(
            true,
            "ウィンドウに合わせる",
            Action::FitWindow,
            icons::paint_fit_window_icon,
            MenuAction::FitWindow,
        ),
        // v4 §25: 「ピクセルグリッド…表示メニューにトグル」。v6 §33 で
        // チェックボックスから「他のツールボタンと同じ .selected(bool)
        // ハイライト」に変わった(実際の状態更新は従来どおり
        // `MenuAction::TogglePixelGrid` を経由して app.rs が行う)。
        Slot::Toggle {
            selected: state.pixel_grid_visible,
            tooltip: "ピクセルグリッド",
            paint: icons::paint_pixel_grid_icon,
            action: MenuAction::TogglePixelGrid,
        },
        Slot::Sep,
        // -- その他(バージョン情報・設定、SPEC §33) --------------------------
        mi(
            true,
            "バージョン情報",
            icons::paint_about_icon,
            MenuAction::About,
        ),
        // v6 §34: 設定(環境設定)ダイアログ。ツールバーの歯車ボタン
        // (`ui/toolbar.rs::ToolbarAction::OpenPreferences`)と同じアイコンを
        // 使う(`icons::paint_settings_icon` を共有)。
        mi_shortcut(
            true,
            "設定",
            Action::OpenPreferences,
            icons::paint_settings_icon,
            MenuAction::OpenPreferences,
        ),
    ]
}

/// `slots` を `avail_width` に収まるよう貪欲に行分割する(各行のスロット
/// index の `Vec`)。モジュールドキュメントコメント参照。
fn pack_rows(slots: &[Slot], avail_width: f32, spacing: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_width = 0.0_f32;
    for (i, slot) in slots.iter().enumerate() {
        let w = slot_width(slot);
        let candidate = if current.is_empty() {
            w
        } else {
            current_width + spacing + w
        };
        if !current.is_empty() && candidate > avail_width {
            rows.push(std::mem::take(&mut current));
            current_width = w;
            current.push(i);
        } else {
            current_width = candidate;
            current.push(i);
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    // 折り返し自体がカテゴリ境界になるため、行頭・行末に孤立した区切り線は
    // 描かない。狭い幅で「線だけの行」や行末の不自然な線を作らない。
    for row in &mut rows {
        while row
            .first()
            .is_some_and(|&index| matches!(slots[index], Slot::Sep))
        {
            row.remove(0);
        }
        while row
            .last()
            .is_some_and(|&index| matches!(slots[index], Slot::Sep))
        {
            row.pop();
        }
    }
    rows.retain(|row| !row.is_empty());
    rows
}

/// `Frame::side_top_panel`(egui 既定、`ui/menu.rs` はカスタム `Frame` を
/// 設定していないのでこれがそのまま使われる)の `inner_margin`。水平
/// (左右それぞれ)・垂直(上下それぞれ)の順。パネルの厳密な高さを自前で
/// 計算する(下記 `show` のドキュメントコメント参照)ために必要な値。
const PANEL_MARGIN_H: f32 = 8.0;
const PANEL_MARGIN_V: f32 = 2.0;

/// クリックされた項目があれば返す(複数同時クリックは起こらないので
/// `Option` でよい)。
///
/// **パネルの高さは自動計算(`Panel` の既定の「前フレームの高さを引き継ぐ」
/// 挙動)に任せず、`exact_size` で現在のフレームの `avail_width` から毎回
/// 厳密に計算し直す。**(モジュールドキュメントコメントの「折り返しの実装
/// について」参照: リサイズ直後の 1 フレーム目から正しい行数で確定させる
/// ため)。
pub fn show(ui: &mut egui::Ui, state: &MenuState) -> Option<MenuAction> {
    let mut action = None;
    let slots = build_slots(state);
    let spacing = ui.spacing().item_spacing;
    // `Panel` の内側 `Ui`(= 実際にボタンを並べる `ui`)の幅は、ここで見えて
    // いる `ui.available_width()` から `Frame::side_top_panel` の水平
    // `inner_margin`(左右それぞれ `PANEL_MARGIN_H`)を引いたもの。ここを
    // 過大評価すると行分割が 1 個多くボタンを詰め込んでしまい、ウィンドウ
    // 右端でボタンが見切れる。
    let avail_width = (ui.available_width() - PANEL_MARGIN_H * 2.0).max(0.0);
    let rows = pack_rows(&slots, avail_width, spacing.x);
    let row_count = (rows.len().max(1) - 1) as f32;
    let content_height = rows.len().max(1) as f32 * BUTTON_H + row_count * spacing.y;
    let panel_height = content_height + PANEL_MARGIN_V * 2.0;

    egui::Panel::top("menu_bar")
        .exact_size(panel_height)
        .show(ui, |ui| {
            for row in &rows {
                ui.horizontal(|ui| {
                    for &i in row {
                        match &slots[i] {
                            Slot::Item(item) => {
                                let response = icon_button(
                                    ui,
                                    item.enabled,
                                    false,
                                    item.paint,
                                    item.action.short_label(),
                                );
                                let clicked = response.clicked();
                                if response.hovered() {
                                    match item.tooltip {
                                        Tooltip::Plain(text) => {
                                            response.on_hover_text(text);
                                        }
                                        Tooltip::Shortcut(label, shortcut) => {
                                            response
                                                .on_hover_text(keymap::menu_label(label, shortcut));
                                        }
                                    }
                                }
                                if clicked {
                                    action = Some(item.action);
                                }
                            }
                            Slot::Toggle {
                                selected,
                                tooltip,
                                paint,
                                action: toggle_action,
                            } => {
                                let response = icon_button(
                                    ui,
                                    true,
                                    *selected,
                                    *paint,
                                    toggle_action.short_label(),
                                );
                                let clicked = response.clicked();
                                if response.hovered() {
                                    response.on_hover_text(*tooltip);
                                }
                                if clicked {
                                    action = Some(*toggle_action);
                                }
                            }
                            Slot::RecentFiles => {
                                if let Some(a) = recent_files_button(ui, state.recent_files) {
                                    action = Some(a);
                                }
                            }
                            Slot::Sep => group_separator(ui),
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

    fn dummy_item() -> Slot {
        mi(true, "x", icons::paint_about_icon, MenuAction::About)
    }

    fn all_enabled_state(
        recent_files: &std::collections::VecDeque<std::path::PathBuf>,
    ) -> MenuState<'_> {
        MenuState {
            can_undo: true,
            can_redo: true,
            has_selection: true,
            can_duplicate_selection_to_tab: true,
            can_add_layer: true,
            can_delete_layer: true,
            can_move_layer_up: true,
            can_move_layer_down: true,
            can_merge_layer_down: true,
            can_flatten_layers: true,
            pixel_grid_visible: true,
            recent_files,
        }
    }

    #[test]
    fn slot_width_separator_is_narrower_than_an_item() {
        assert_eq!(slot_width(&Slot::Sep), SEP_W);
        assert_eq!(slot_width(&dummy_item()), BUTTON_W);
    }

    #[test]
    fn pack_rows_fits_as_many_items_as_the_width_allows() {
        let slots: Vec<Slot> = (0..5).map(|_| dummy_item()).collect();
        let rows = pack_rows(&slots, BUTTON_W * 3.0, 0.0);
        assert_eq!(rows, vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[test]
    fn pack_rows_accounts_for_item_spacing_between_slots() {
        let slots: Vec<Slot> = (0..3).map(|_| dummy_item()).collect();
        // 2 個ぶんの幅+間隔ちょうどなら 2 個目までは入るが 3 個目は入らない。
        let avail = BUTTON_W * 2.0 + 4.0;
        let rows = pack_rows(&slots, avail, 4.0);
        assert_eq!(rows, vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn pack_rows_places_every_index_exactly_once_with_no_empty_rows() {
        let slots: Vec<Slot> = (0..10).map(|_| dummy_item()).collect();
        let rows = pack_rows(&slots, BUTTON_W * 4.0, 2.0);
        assert!(rows.iter().all(|r| !r.is_empty()));
        let mut all_indices: Vec<usize> = rows.iter().flatten().copied().collect();
        all_indices.sort_unstable();
        assert_eq!(all_indices, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn pack_rows_handles_a_slot_wider_than_avail_width_without_panicking() {
        // 1 個も収まらない幅でも無限ループやパニックにならず、最低 1 個は
        // 強制的に置く(ARCHITECTURE.md §18.6-6: 640px 最小幅でも操作不能に
        // ならないことの土台となる性質)。
        let slots = vec![dummy_item(), dummy_item()];
        let rows = pack_rows(&slots, 0.0, 0.0);
        assert_eq!(rows, vec![vec![0], vec![1]]);
    }

    #[test]
    fn pack_rows_of_empty_slots_produces_no_rows() {
        let slots: Vec<Slot> = Vec::new();
        assert!(pack_rows(&slots, 100.0, 4.0).is_empty());
    }

    #[test]
    fn compact_minimum_width_layout_has_no_orphan_separator_and_at_most_four_rows() {
        let recent_files = std::collections::VecDeque::new();
        let state = all_enabled_state(&recent_files);
        let slots = build_slots(&state);
        let rows = pack_rows(&slots, 640.0 - PANEL_MARGIN_H * 2.0, 8.0);
        assert!(rows.len() <= 4);
        for row in rows {
            assert!(!matches!(slots[row[0]], Slot::Sep));
            assert!(!matches!(slots[*row.last().unwrap()], Slot::Sep));
        }
    }

    #[test]
    fn every_visible_action_label_is_short_and_nonempty() {
        let recent_files = std::collections::VecDeque::new();
        let state = all_enabled_state(&recent_files);
        for slot in build_slots(&state) {
            let label = match slot {
                Slot::Item(item) => item.action.short_label(),
                Slot::Toggle { action, .. } => action.short_label(),
                Slot::RecentFiles => "最近開く",
                Slot::Sep => continue,
            };
            assert!(!label.is_empty());
            assert!(label.chars().count() <= 6, "label too long: {label}");
        }
    }

    /// SPEC §33 の全項目(ファイル7・編集9・画像12・レイヤー7・表示5・その他2
    /// の合計42)と区切り5、合わせて47スロットであることの静的テスト
    /// (ARCHITECTURE.md §18.6-3 と同種の「項目の過不足に気づく」ための数合わせ)。
    #[test]
    fn build_slots_has_the_expected_total_count() {
        let recent_files = std::collections::VecDeque::new();
        let state = MenuState {
            can_undo: false,
            can_redo: false,
            has_selection: false,
            can_duplicate_selection_to_tab: false,
            can_add_layer: true,
            can_delete_layer: false,
            can_move_layer_up: false,
            can_move_layer_down: false,
            can_merge_layer_down: false,
            can_flatten_layers: false,
            pixel_grid_visible: false,
            recent_files: &recent_files,
        };
        let slots = build_slots(&state);
        let item_count = slots.iter().filter(|s| !matches!(s, Slot::Sep)).count();
        let sep_count = slots.iter().filter(|s| matches!(s, Slot::Sep)).count();
        assert_eq!(item_count, 42);
        assert_eq!(sep_count, 5);
        assert_eq!(slots.len(), 47);
    }

    /// 回帰テスト: SPEC.md §33(415-420行目)は「ファイル」グループの並び順を
    /// 「新規 / 開く / 上書き保存 / 名前を付けて保存 / 最近使ったファイル /
    /// タブを閉じる / 終了」と明記している。以前は `Slot::RecentFiles` が
    /// `Open` の直後(3番目)に来ており SPEC が指定する `SaveAs` の直後
    /// (5番目)ではなかった(項目の存在・個数だけを見る
    /// `build_slots_has_the_expected_total_count` では検知できない並び順の
    /// 不一致)。この回帰を防ぐため、先頭 7 スロット(ファイルグループ)の
    /// `MenuAction` 列を SPEC の明記順と 1:1 で突き合わせる。
    #[test]
    fn build_slots_file_group_order_matches_spec_33() {
        let recent_files = std::collections::VecDeque::new();
        let state = MenuState {
            can_undo: false,
            can_redo: false,
            has_selection: false,
            can_duplicate_selection_to_tab: false,
            can_add_layer: true,
            can_delete_layer: false,
            can_move_layer_up: false,
            can_move_layer_down: false,
            can_merge_layer_down: false,
            can_flatten_layers: false,
            pixel_grid_visible: false,
            recent_files: &recent_files,
        };
        let slots = build_slots(&state);
        // SPEC §33: 新規 / 開く / 上書き保存 / 名前を付けて保存 /
        // 最近使ったファイル(RecentFiles ポップアップ、MenuAction 無し) /
        // タブを閉じる / 終了、に続いて区切り線。
        let expected_actions: [Option<MenuAction>; 7] = [
            Some(MenuAction::New),
            Some(MenuAction::Open),
            Some(MenuAction::Save),
            Some(MenuAction::SaveAs),
            None,
            Some(MenuAction::CloseTab),
            Some(MenuAction::Exit),
        ];
        for (i, expected) in expected_actions.iter().enumerate() {
            match (&slots[i], expected) {
                (Slot::Item(item), Some(action)) => {
                    assert_eq!(item.action, *action, "file group slot {i} action mismatch")
                }
                (Slot::RecentFiles, None) => {}
                _ => panic!("file group slot {i} order mismatch"),
            }
        }
        assert!(
            matches!(slots[7], Slot::Sep),
            "file group must be followed by a separator before 編集"
        );
    }
}
