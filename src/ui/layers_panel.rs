//! レイヤーパネル(右パネルの一部、SPEC §13/§50.1、ARCHITECTURE.md
//! §14.4/§14.8/§22.1)。
//!
//! 一覧(最上位レイヤーが一番上)・クリックでアクティブ化・ダブルクリックで
//! 名前変更・新規/複製/削除/上へ/下へ/下と結合ボタンを表示する。
//!
//! v8 レビュー修正で全操作を `LayersPanelAction` 経由に統一した: 以前は
//! 「表示切替・不透明度・名前変更は履歴に積まない(SPEC §13)ので、ここで
//! 直接 `Document` を変更してよい」としていたが、SPEC §13 最終項の
//! 「レイヤー操作は浮動片やストローク進行中にはツール切替と同じ扱い
//! (先に確定してから実行)」は履歴に積まない操作にも及ぶ(ARCHITECTURE.md
//! §14.9-3)。パネルは常に「何を要求されたか」だけを返し、`app.rs` が
//! `commit_open_gesture()` を通してから適用する(メニュー・ツールバーと
//! 同じ流儀に揃った)。
//!
//! v12 §50.1 で行の構成を **[サムネイル][名前][目アイコン]** に刷新し、
//! ドラッグ&ドロップでの並べ替え(1 undo 単位)と、一覧直上の
//! 不透明度 / ブレンド / アルファロックを追加した。サムネイルの生成は
//! `Document::content_gen`(成功した変更でのみ増える世代)が変わった
//! **可視行**だけ・1 フレーム最大 `MAX_THUMBNAILS_PER_FRAME` 枚に制限する
//! (SPEC §50.1: 「毎フレーム禁止 — アイドル CPU 0%」)。

use eframe::egui;

use crate::document::{BlendMode, Document, Layer, MAX_LAYERS};
use crate::keymap::{self, Action};
use crate::raster;
use crate::ui::{icons, theme, widgets};

/// パネルからの操作要求。構造を変える操作(1 undo 単位)も、履歴に積まない
/// 操作(表示/不透明度/名前/ブレンド/アルファロック — SPEC §13/§50)も、
/// すべて `app.rs` が commit-first ガードを通してから適用する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayersPanelAction {
    Activate(usize),
    Add,
    Duplicate,
    Delete,
    MoveUp,
    MoveDown,
    MergeDown,
    /// v12 §50.1: ドラッグ&ドロップでの並べ替え(`from` を取り除いて `to` へ
    /// 挿入。1 undo 単位)。同位置へのドロップは要求自体を出さない。
    Move {
        from: usize,
        to: usize,
    },
    /// 表示切替(SPEC §13: 履歴には積まない)。
    SetVisible(usize, bool),
    /// v13 §60: 目アイコンの Alt+クリック = そのレイヤーだけ表示(もう一度で
    /// 全表示に戻す。`solo_visibility`)。表示切替と同じく履歴には積まない。
    Solo(usize),
    /// アクティブレイヤーの不透明度(0-255。SPEC §13: 履歴には積まない)。
    SetOpacity(u8),
    /// v12 §50.2: アクティブレイヤーのブレンドモード(履歴には積まない)。
    SetBlend(BlendMode),
    /// v12 §50.3: アクティブレイヤーのアルファロック(履歴には積まない)。
    SetAlphaLock(bool),
    /// 名前変更の確定(ダブルクリック編集の Enter/フォーカス外し)。
    CommitRename(usize, String),
    /// 行メニューはアクティブ行ではなく、開いたレイヤーを操作する。
    ForLayer {
        uid: u64,
        command: LayerRowCommand,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRowCommand {
    Duplicate,
    Delete,
    MoveUp,
    MoveDown,
    MergeDown,
    ToggleAlphaLock,
}

/// ダブルクリックで開始した名前編集の状態(`app.rs` が保持する)。
/// `Some((layer_index, editing_text, needs_focus))`。
///
/// `needs_focus` は編集開始直後の 1 フレームだけ `true`。egui 0.35 の
/// `Response::lost_focus()` は「直近フレームでフォーカスを持っていた &&
/// 現在フォーカスを持っていない」を照会する実装のため、`request_focus()` を
/// 毎フレーム無条件に呼ぶと `Memory::request_focus` が `focused_widget` を
/// 即座に同じ id へ再設定してしまい、`!has_focus(id)` が常に偽になって
/// `lost_focus()` が恒久的に発火しなくなる(Enter/Esc/欄外クリックで
/// リネームを確定・終了できず、フォーカスが居座り続けるため
/// `ctx.egui_wants_keyboard_input()` も真のままになり全ショートカットが
/// 効かなくなる)。`app.rs` の `TextEditState::needs_focus` と同じ
/// 「編集開始フレームのみ `request_focus()` する」パターンで回避する。
pub type RenameState = Option<(usize, String, bool)>;

/// SPEC §50.1: サムネイルの最大寸法(縦横比は維持するのでこれ以下になる)。
const THUMBNAIL_MAX_W: u32 = 48;
const THUMBNAIL_MAX_H: u32 = 36;

/// 1 フレームに新規生成するサムネイルの上限(ARCHITECTURE.md §22.1)。
/// 多層文書で undo するとすべての行の世代が一度に変わるため、上限が無いと
/// 1 フレームに 64 枚の縮小+テクスチャ確保が走る。
const MAX_THUMBNAILS_PER_FRAME: usize = 4;

/// 行に確保するサムネイル枠(実サムネイルは縦横比維持でこの中に収める)。
const THUMBNAIL_SLOT: egui::Vec2 = egui::vec2(THUMBNAIL_MAX_W as f32, THUMBNAIL_MAX_H as f32);
/// 目アイコンの当たり判定(正方形)と絵の大きさ。v13 §60 で 16px から広げた。
const EYE_HIT: f32 = 28.0;
const EYE_ICON: f32 = 17.0;
/// 一覧の 1 行の最小の高さと左右の余白。
const ROW_HEIGHT: f32 = 48.0;
const ROW_PAD: f32 = 6.0;
/// 名前+詳細の 2 行ぶんの高さ。
const NAME_BLOCK_H: f32 = 32.0;
/// 一覧下の操作ボタン。
const TOOL_BUTTON_W: f32 = 32.0;
const TOOL_BUTTON_H: f32 = 30.0;
/// プロパティ欄の項目名の幅と、透明保護トグルの幅。
const PROP_LABEL_W: f32 = 44.0;
const LOCK_TOGGLE_W: f32 = 54.0;
/// 行の詳細欄に出す透明保護の錠(正方形)。
const LOCK_BADGE: f32 = 11.0;
/// 行の右クリックメニューの幅。
const ROW_MENU_WIDTH: f32 = 230.0;

/// v12 §50.1: レイヤーサムネイルのテクスチャキャッシュ(**タブごと**に
/// `Tab` が 1 個保持する)。
///
/// - 行(レイヤー添字)あたりテクスチャは 1 枚だけを保持し、再生成時は
///   同じテクスチャを `TextureHandle::set` で**置換**する(テクスチャ id を
///   増やし続けない)。
/// - レイヤー枚数が変わった(= 構造変更)、または文書・レイヤー構成が
///   入れ替わったときは `invalidate_all` で全消去する(並べ替えのように
///   枚数が変わらない構造変更でも、行とレイヤーの対応が変わるため必須)。
/// - 生成は「可視行」かつ「世代が変わった行」だけ、1 フレーム上限つき。
///   上限で持ち越した行が残っているフレームだけ `request_repaint` を 1 回
///   出して追いつかせる(揃えば要求は止まる = 有限。生成に失敗する入力
///   (0×0 等)も「その世代は完了」として記録するのでループにならない)。
#[derive(Default)]
pub struct ThumbnailCache {
    entries: Vec<Option<ThumbnailEntry>>,
    /// このフレームで生成した枚数(`begin_frame` でリセット)。
    generated_this_frame: usize,
    /// このフレームで生成しきれなかった可視行があるか(`begin_frame` で
    /// リセット、`show` の最後に `request_repaint` の判断へ使う)。
    pending: bool,
}

struct ThumbnailEntry {
    /// このテクスチャを作った時点の `Document::content_gen`。
    gen: u64,
    size: [usize; 2],
    /// 縮小に失敗する入力(0×0 レイヤーなど)では `None`。この場合も
    /// 「その世代は処理済み」として残すことで、再試行の無限ループを防ぐ。
    texture: Option<egui::TextureHandle>,
}

impl ThumbnailCache {
    /// フレーム開始時の同期。レイヤー枚数が変わっていれば全消去する
    /// (行とレイヤーの対応が総入れ替えになるため)。
    fn begin_frame(&mut self, layer_count: usize) {
        if self.entries.len() != layer_count {
            self.entries.clear();
            self.entries.resize_with(layer_count, || None);
        }
        self.generated_this_frame = 0;
        self.pending = false;
    }

    /// 文書の差し替え(新規/開く/プロジェクト読込/タブ内容の置換)・
    /// レイヤー構造の変更(追加/複製/削除/並べ替え/結合)・undo/redo・
    /// 履歴ジャンプ・スナップショット復元でキャッシュを全消去する
    /// (`app.rs` から呼ぶ)。
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// このフレームで生成しきれなかった可視行が残っているか。
    fn has_pending(&self) -> bool {
        self.pending
    }

    /// テスト用: キャッシュに載っている行数(0 = 全消去済み)。
    #[cfg(test)]
    pub(crate) fn cached_rows(&self) -> usize {
        self.entries.len()
    }

    /// テスト用: 実テクスチャを持たないダミー行でキャッシュを埋める
    /// (`app.rs` 側の「構造変更・undo/redo で全消去する」配線を、egui の
    /// フレームを回さずに検証するため)。
    #[cfg(test)]
    pub(crate) fn seed_rows_for_test(&mut self, rows: usize) {
        self.entries.clear();
        self.entries.resize_with(rows, || {
            Some(ThumbnailEntry {
                gen: 0,
                size: [0, 0],
                texture: None,
            })
        });
    }

    /// `idx` 行のサムネイルテクスチャ。まだ無い/古い場合はこのフレームの
    /// 生成上限に余裕があるときだけ作る(無ければ `None` = プレースホルダ)。
    fn texture(
        &mut self,
        ctx: &egui::Context,
        idx: usize,
        layer: &Layer,
        width: u32,
        height: u32,
        gen: u64,
    ) -> Option<egui::TextureHandle> {
        let fresh = self
            .entries
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .is_some_and(|entry| entry.gen == gen);
        if fresh {
            return self.entries[idx]
                .as_ref()
                .and_then(|e| e.texture.as_ref())
                .cloned();
        }
        if self.generated_this_frame >= MAX_THUMBNAILS_PER_FRAME {
            // 上限に達したフレームでは、古いテクスチャがあればそれを出しつつ
            // 「持ち越しあり」を記録する(`show` が 1 回だけ repaint を要求し、
            // 次のフレームで続きを生成する)。
            self.pending = true;
            return self
                .entries
                .get(idx)
                .and_then(|slot| slot.as_ref())
                .and_then(|e| e.texture.as_ref())
                .cloned();
        }
        let (tw, th, pixels) = raster::thumbnail_rgba(
            &layer.pixels,
            width,
            height,
            THUMBNAIL_MAX_W,
            THUMBNAIL_MAX_H,
        );
        self.generated_this_frame += 1;
        let slot = self.entries.get_mut(idx)?;
        if tw == 0 || th == 0 {
            // 縮小できない入力。再試行しても同じなので「完了」として記録する
            // (`request_repaint` のループを作らないための有界性の要)。
            *slot = Some(ThumbnailEntry {
                gen,
                size: [0, 0],
                texture: None,
            });
            return None;
        }
        let size = [tw as usize, th as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
        match slot {
            // 同じ寸法なら既存テクスチャを差し替える(id を増やさない)。
            Some(ThumbnailEntry {
                gen: entry_gen,
                size: entry_size,
                texture: Some(texture),
            }) if *entry_size == size => {
                texture.set(image, egui::TextureOptions::NEAREST);
                *entry_gen = gen;
            }
            _ => {
                let texture = ctx.load_texture(
                    format!("darask_layer_thumb_{idx}"),
                    image,
                    egui::TextureOptions::NEAREST,
                );
                *slot = Some(ThumbnailEntry {
                    gen,
                    size,
                    texture: Some(texture),
                });
            }
        }
        slot.as_ref().and_then(|e| e.texture.as_ref()).cloned()
    }
}

/// `show` に渡す一式(`Document` は読み取り専用。適用は `app.rs` の
/// `handle_layers_panel_action`)。
pub struct LayersPanelCtx<'a> {
    pub doc: &'a Document,
    pub rename: &'a mut RenameState,
    pub thumbnails: &'a mut ThumbnailCache,
    /// v13 §60: ドックがこのパネルに割り当てた本体の高さ。`Some` のときは
    /// 一覧だけを内部でスクロールさせ、操作ボタンを常にパネル下端に出す。
    /// `None`(フローティング・テスト)は従来どおり一覧をそのまま流す。
    pub fill_height: Option<f32>,
}

/// レイヤーパネルを描画する。要求された操作があれば返す(`Document` は
/// 一切変更しない — 読み取り専用)。
///
/// v13 §60: 構成は「プロパティ欄(合成・保護・不透明度)/一覧/操作ボタン」。
/// 「新規レイヤー」はパネル見出しの「+ 追加」(`side_panel.rs`)に移した。
pub fn show(ui: &mut egui::Ui, ctx: LayersPanelCtx) -> Option<LayersPanelAction> {
    let LayersPanelCtx {
        doc,
        rename,
        thumbnails,
        fill_height,
    } = ctx;
    let mut action = None;

    let active = doc.active_index();
    let layer_count = doc.layers.len();
    thumbnails.begin_frame(layer_count);
    let top = ui.cursor().top();

    // SPEC §50.1: 「不透明度スライダー・ブレンドモード・アルファロックは
    // 一覧の直上に置く」。
    show_active_layer_controls(ui, doc, active, &mut action);
    ui.add_space(4.0);

    match fill_height {
        Some(fill) => {
            let used = ui.cursor().top() - top;
            let toolbar_h = 2.0 + 1.0 + TOOL_BUTTON_H + ui.spacing().item_spacing.y * 3.0;
            let list_h = list_height(fill, used, toolbar_h);
            egui::ScrollArea::vertical()
                .id_salt("darask_layer_list")
                .min_scrolled_height(list_h)
                .max_height(list_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    show_layer_list(ui, doc, active, rename, thumbnails, &mut action);
                });
        }
        None => show_layer_list(ui, doc, active, rename, thumbnails, &mut action),
    }
    // v12 §50.1(追いレビュー③): 1 フレームの生成上限で持ち越した可視行が
    // ある間だけ、**条件付きで** 1 回だけ再描画を要求して追いつかせる。
    // 全行が揃った(または生成不能として記録された)フレームでは要求しない
    // ので、無入力状態では有限回で止まる(アイドル CPU 0%)。
    if thumbnails.has_pending() {
        ui.ctx().request_repaint();
    }

    ui.add_space(2.0);
    show_layer_toolbar(ui, doc, active, &mut action);

    action
}

/// 内部スクロールする一覧の高さ(純関数)。本体の割り当て `fill` から
/// プロパティ欄(`used`)と操作ボタン列を引き、最低 2 行ぶんは残す。
fn list_height(fill: f32, used: f32, toolbar_h: f32) -> f32 {
    (fill - used - toolbar_h).max(ROW_HEIGHT * 2.0 + 2.0)
}

/// 一覧下の操作ボタン(複製/下と結合 | 上へ/下へ … 右端に削除)。
fn show_layer_toolbar(
    ui: &mut egui::Ui,
    doc: &Document,
    active: usize,
    action: &mut Option<LayersPanelAction>,
) {
    let layer_count = doc.layers.len();
    let (sep, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        sep.x_range(),
        sep.center().y,
        egui::Stroke::new(1.0, theme::LINE),
    );

    let can_add = layer_count < MAX_LAYERS;
    // v12 §50.2: 非通常ブレンドを含む 2 枚は結合できない(グレーアウト)。
    let can_merge = doc.can_merge_active_down();
    let merge_hint = if !can_merge && layer_count > 1 && active > 0 {
        "「通常」以外のブレンドを含むレイヤーは結合できません".to_owned()
    } else {
        keymap::menu_label("下のレイヤーと結合", Action::LayerMergeDown)
    };
    // 狭いドック(フローティングで縮めた場合)でも 1 行に収める。
    let button_w = ((ui.available_width() - 20.0) / 5.0).clamp(22.0, TOOL_BUTTON_W);
    let size = egui::vec2(button_w, TOOL_BUTTON_H);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let button = |ui: &mut egui::Ui, enabled: bool, paint: IconPaint, hint: String| {
            widgets::icon_button(ui, size, 17.0, enabled, false, paint, &hint)
                .on_hover_text(hint)
                .clicked()
        };
        if button(
            ui,
            can_add,
            icons::paint_layer_duplicate_icon,
            keymap::menu_label("レイヤーを複製", Action::LayerDuplicate),
        ) {
            *action = Some(LayersPanelAction::Duplicate);
        }
        if button(
            ui,
            can_merge,
            icons::paint_layer_merge_down_icon,
            merge_hint,
        ) {
            *action = Some(LayersPanelAction::MergeDown);
        }
        widgets::vertical_divider(ui, 16.0);
        if button(
            ui,
            active + 1 < layer_count,
            icons::paint_layer_move_up_icon,
            "上へ移動".to_owned(),
        ) {
            *action = Some(LayersPanelAction::MoveUp);
        }
        if button(
            ui,
            active > 0,
            icons::paint_layer_move_down_icon,
            "下へ移動".to_owned(),
        ) {
            *action = Some(LayersPanelAction::MoveDown);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // SPEC §13: 「レイヤーが 1 枚のときは削除・結合は無効」。
            if button(
                ui,
                layer_count > 1,
                icons::paint_delete_icon,
                "レイヤーを削除".to_owned(),
            ) {
                *action = Some(LayersPanelAction::Delete);
            }
        });
    });
}

type IconPaint = widgets::IconPaint;

/// SPEC §50.1: 一覧の直上に置くアクティブレイヤーの設定
/// (ブレンド §50.2 / アルファロック §50.3 / 不透明度 §13)。
/// v13 §60: 一段沈めた欄にまとめ、項目名を付ける(狭いときは項目名を省く)。
fn show_active_layer_controls(
    ui: &mut egui::Ui,
    doc: &Document,
    active: usize,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(layer) = doc.layers.get(active) else {
        return;
    };
    egui::Frame::NONE
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let labels = ui.available_width() >= 200.0;
            let label = |ui: &mut egui::Ui, text: &str| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(PROP_LABEL_W, 22.0), egui::Sense::hover());
                ui.painter().text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    text,
                    egui::FontId::proportional(theme::FONT_SMALL),
                    theme::TEXT_FAINT,
                );
            };

            ui.horizontal(|ui| {
                if labels {
                    label(ui, "合成");
                }
                let lock_w = LOCK_TOGGLE_W + ui.spacing().item_spacing.x;
                egui::ComboBox::from_id_salt("darask_layer_blend")
                    .selected_text(layer.blend.label())
                    .width((ui.available_width() - lock_w).max(40.0))
                    .show_ui(ui, |ui| {
                        for mode in BlendMode::ALL {
                            if ui
                                .selectable_label(layer.blend == mode, mode.label())
                                .clicked()
                                && layer.blend != mode
                            {
                                *action = Some(LayersPanelAction::SetBlend(mode));
                            }
                        }
                    })
                    .response
                    .on_hover_text("合成モード");
                if alpha_lock_toggle(ui, layer.alpha_lock) {
                    *action = Some(LayersPanelAction::SetAlphaLock(!layer.alpha_lock));
                }
            });

            // ARCHITECTURE.md §14.9-8: 値が実際に変わったフレームだけ要求を出す
            // (ドラッグ中の全面 recomposite を毎フレーム 1 回に抑える)。
            ui.horizontal(|ui| {
                if labels {
                    label(ui, "不透明度");
                }
                // 数値欄を右端に先に置き、スライダーは残りの幅を使う(数値欄の
                // 実寸がフォントで変わってもはみ出さない)。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut opacity_pct = (layer.opacity as f32 / 255.0 * 100.0).round() as i32;
                    let before = opacity_pct;
                    widgets::number_field(ui, &mut opacity_pct, 0..=100, "%", 48.0);
                    let mut changed = opacity_pct != before;
                    let mut t = opacity_pct as f32 / 100.0;
                    let track_w = ui.available_width().max(24.0);
                    if widgets::track(ui, &mut t, track_w, true).changed() {
                        opacity_pct = (t * 100.0).round() as i32;
                        changed = true;
                    }
                    if changed {
                        let new_opacity =
                            ((opacity_pct.clamp(0, 100) as f32) / 100.0 * 255.0).round() as u8;
                        if layer.opacity != new_opacity {
                            *action = Some(LayersPanelAction::SetOpacity(new_opacity));
                        }
                    }
                });
            });
        });
}

/// 透明保護のトグル(錠アイコン+「保護」)。押されたら `true`。
fn alpha_lock_toggle(ui: &mut egui::Ui, locked: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(LOCK_TOGGLE_W, 22.0), egui::Sense::click());
    let response = response.on_hover_text(if locked {
        "透明保護: ON(透明部分を保護。クリックで解除)"
    } else {
        "透明保護: OFF(クリックで透明部分を保護)"
    });
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, locked, "透明部分を保護")
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, stroke, fg) = if locked {
            (theme::ACCENT_SOFT, theme::ACCENT_LINE, theme::ACCENT)
        } else if response.hovered() {
            (theme::BG_HOVER, theme::LINE_STRONG, egui::Color32::WHITE)
        } else {
            (
                egui::Color32::TRANSPARENT,
                theme::LINE_STRONG,
                theme::TEXT_DIM,
            )
        };
        painter.rect_filled(rect, 6.0, fill);
        painter.rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
        let icon = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 13.0, rect.center().y),
            egui::vec2(13.0, 13.0),
        );
        icons::paint_alpha_lock_icon(painter, icon, fg, locked);
        painter.text(
            egui::pos2(rect.left() + 23.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "保護",
            egui::FontId::proportional(11.5),
            fg,
        );
    }
    response.clicked()
}

/// レイヤー一覧(最上位が一番上)+ ドラッグ&ドロップの並べ替え。
///
/// SPEC §50.1: 固定 `max_height` のスクロール領域は廃止した(右パネル全体の
/// スクロール 1 本に集約し、二重スクロールを作らない — `side_panel.rs`)。
/// 「可視行のみ生成」は `Ui::is_rect_visible`(親のクリップ矩形との交差)で
/// 判定するため、スクロール領域の外に流れた行はサムネイルを作らない。
fn show_layer_list(
    ui: &mut egui::Ui,
    doc: &Document,
    active: usize,
    rename: &mut RenameState,
    thumbnails: &mut ThumbnailCache,
    action: &mut Option<LayersPanelAction>,
) {
    let layer_count = doc.layers.len();
    // 表示順(上=最上位レイヤー)の行ごとに、レイヤー添字と矩形を控える。
    let mut rows: Vec<(usize, egui::Rect)> = Vec::with_capacity(layer_count);
    let dragged = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p);

    let list_response = ui
        .scope(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for idx in (0..layer_count).rev() {
                let row = show_layer_row(ui, doc, idx, active, dragged, rename, thumbnails, action);
                rows.push((idx, row));
            }
        })
        .response;

    if layer_count < 2 {
        return;
    }
    // ドロップ先の判定用に一覧全体を覆う当たり判定(hover のみ。クリックも
    // ドラッグも sense しないので、行のボタン・ドラッグ源とは競合しない)。
    let zone = ui.interact(
        list_response.rect,
        ui.id().with("darask_layer_dnd_zone"),
        egui::Sense::hover(),
    );

    if dragged.is_none() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if !zone.contains_pointer() {
        return;
    }
    let slot = insertion_slot(&rows, pointer.y);
    draw_insertion_indicator(ui, &rows, slot);

    if let Some(dropped) = zone.dnd_release_payload::<usize>() {
        let from = *dropped;
        if let Some(to) = drop_target_index(from, slot, layer_count) {
            *action = Some(LayersPanelAction::Move { from, to });
        }
    }
}

/// 表示順の行矩形群とポインタの y から「何行目の直前に挿入するか」
/// (0..=行数)を求める。行の中心より上なら手前、下なら次の行の手前。
fn insertion_slot(rows: &[(usize, egui::Rect)], pointer_y: f32) -> usize {
    rows.iter()
        .position(|(_, rect)| pointer_y < rect.center().y)
        .unwrap_or(rows.len())
}

/// 表示順の挿入位置(`slot`)を `Document::move_layer` の `to`(取り除いた
/// 後の添字)へ変換する。同じ位置に落とした場合は `None`(no-op — SPEC §50.1
/// 「ドロップ位置が元位置と同じなら履歴に積まない」)。
///
/// 一覧は最上位レイヤーが先頭なので、表示順スロット `slot` は取り除く前の
/// レイヤー添字 `count - slot` に対応する。取り除いた後の添字は、元位置より
/// 後ろへ挿入する場合だけ 1 つ手前へずれる。
fn drop_target_index(from: usize, slot: usize, count: usize) -> Option<usize> {
    let insert_before_removal = count.checked_sub(slot)?;
    let to = if insert_before_removal > from {
        insert_before_removal.checked_sub(1)?
    } else {
        insert_before_removal
    };
    if to == from || to >= count {
        return None;
    }
    Some(to)
}

/// 挿入位置のインジケータ(SPEC §50.1 / §60: アクセント色の線+左端の点)。
fn draw_insertion_indicator(ui: &egui::Ui, rows: &[(usize, egui::Rect)], slot: usize) {
    let y = match rows.get(slot) {
        Some((_, rect)) => rect.top() - 1.0,
        None => match rows.last() {
            Some((_, rect)) => rect.bottom() + 1.0,
            None => return,
        },
    };
    let Some((_, first)) = rows.first() else {
        return;
    };
    let painter = ui.painter();
    painter.hline(
        first.left() + 3.0..=first.right(),
        y,
        egui::Stroke::new(3.0, theme::ACCENT),
    );
    painter.circle_filled(egui::pos2(first.left() + 3.0, y), 4.5, theme::ACCENT);
}

/// 1 行 = [サムネイル][名前 / 合成・不透明度][目アイコン]。戻り値は行全体の
/// 矩形(ドラッグ&ドロップの挿入位置計算に使う)。背景(アクティブ/ホバー)は
/// 行を並べた後に先頭へ差し込んだ `Shape` を差し替えて描く。
///
/// **目アイコンはドラッグ源の外に置く**(egui 0.35 の当たり判定は「後から
/// 登録された widget が上」で、`dnd_drag_source` は中身より**後**に
/// `Sense::drag()` の widget を登録する。そのため中に click widget を入れると
/// hit_test の `(click, drag)` 分岐が「上にあるのはドラッグのみを sense する
/// widget」と判断してクリックを捨ててしまい、目アイコンも名前クリックも
/// 一切反応しなくなる)。サムネイルと名前はドラッグ源の中に置き、行の
/// クリック/ダブルクリックは `Response::interact(Sense::click())` で
/// **ドラッグ源自身の response** に足して受ける(同じ id なので
/// hit_test の「クリックもドラッグも sense する widget」分岐に入る)。
/// リネーム編集中の行はドラッグ源にせず `TextEdit` をそのまま置く
/// (編集中にその行をドラッグする必要はない)。
#[allow(clippy::too_many_arguments)]
fn show_layer_row(
    ui: &mut egui::Ui,
    doc: &Document,
    idx: usize,
    active: usize,
    dragged: Option<usize>,
    rename: &mut RenameState,
    thumbnails: &mut ThumbnailCache,
    action: &mut Option<LayersPanelAction>,
) -> egui::Rect {
    let is_editing = matches!(rename, Some((i, _, _)) if *i == idx);
    let is_active = idx == active;
    let is_dragged = dragged == Some(idx);
    let background = ui.painter().add(egui::Shape::Noop);
    let rect = ui
        .horizontal(|ui| {
            ui.set_min_height(ROW_HEIGHT);
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.add_space(ROW_PAD);
            let content_w = (ui.available_width()
                - EYE_HIT
                - ROW_PAD
                - ui.spacing().item_spacing.x * 2.0)
                .max(0.0);
            if is_editing {
                ui.allocate_ui_with_layout(
                    egui::vec2(content_w, ROW_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(content_w);
                        show_thumbnail(ui, doc, idx, thumbnails);
                        show_rename_editor(ui, idx, rename, action);
                    },
                );
            } else {
                let dragged =
                    ui.dnd_drag_source(egui::Id::new(("darask_layer_row", idx)), idx, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(content_w, ROW_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_width(content_w);
                                ui.set_min_height(ROW_HEIGHT);
                                ui.spacing_mut().item_spacing.x = 10.0;
                                show_thumbnail(ui, doc, idx, thumbnails);
                                show_name_label(ui, doc, idx, is_active);
                            },
                        );
                    });
                let row = dragged.response.interact(egui::Sense::click());
                if row.clicked() || row.secondary_clicked() {
                    action.get_or_insert(LayersPanelAction::Activate(idx));
                }
                if row.double_clicked() {
                    if let Some(layer) = doc.layers.get(idx) {
                        *rename = Some((idx, layer.name.clone(), true));
                    }
                }
                row.context_menu(|ui| show_row_menu(ui, doc, idx, rename, action));
                if let Some(layer) = doc.layers.get(idx) {
                    row.on_hover_text(format!(
                        "{}({})\nダブルクリックで名前変更 · ドラッグで並べ替え · 右クリックでメニュー",
                        layer.name,
                        layer_status(layer)
                    ));
                }
            }
            show_eye_toggle(ui, doc, idx, action);
        })
        .response
        .rect;

    let hovered = ui.rect_contains_pointer(rect);
    let shape = if is_dragged {
        // ドラッグ中の元の位置は斜線ではなく、薄い枠だけを残す(SPEC §60)。
        egui::Shape::rect_stroke(
            rect,
            7.0,
            egui::Stroke::new(1.0, theme::LINE_STRONG),
            egui::StrokeKind::Inside,
        )
    } else if is_active {
        egui::Shape::Rect(egui::epaint::RectShape::new(
            rect,
            7.0,
            theme::ACCENT_SOFT,
            egui::Stroke::new(1.0, theme::ACCENT_LINE),
            egui::StrokeKind::Inside,
        ))
    } else if hovered {
        egui::Shape::rect_filled(rect, 7.0, theme::BG_WIDGET)
    } else {
        egui::Shape::Noop
    };
    ui.painter().set(background, shape);
    rect
}

fn show_row_menu(
    ui: &mut egui::Ui,
    doc: &Document,
    idx: usize,
    rename: &mut RenameState,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(layer) = doc.layers.get(idx) else {
        return;
    };
    ui.set_min_width(ROW_MENU_WIDTH);
    ui.spacing_mut().item_spacing.y = 1.0;
    // 操作対象を取り違えないよう、右クリックした行の名前を見出しにする。
    ui.label(
        egui::RichText::new(format!("「{}」", layer.name))
            .size(theme::FONT_SMALL)
            .color(theme::TEXT_FAINT),
    );
    if menu_row(
        ui,
        true,
        icons::paint_rename_icon,
        "名前を変更",
        "ダブルクリック",
        false,
    ) {
        *rename = Some((idx, layer.name.clone(), true));
        ui.close();
    }
    let eye: IconPaint = if layer.visible {
        |p, r, c| icons::paint_eye_icon(p, r, c, false)
    } else {
        |p, r, c| icons::paint_eye_icon(p, r, c, true)
    };
    let eye_label = if layer.visible {
        "非表示にする"
    } else {
        "表示する"
    };
    if menu_row(ui, true, eye, eye_label, "", false) {
        *action = Some(LayersPanelAction::SetVisible(idx, !layer.visible));
        ui.close();
    }
    if menu_row(
        ui,
        true,
        |p, r, c| icons::paint_alpha_lock_icon(p, r, c, true),
        "透明部分を保護",
        if layer.alpha_lock { "ON" } else { "" },
        false,
    ) {
        *action = Some(LayersPanelAction::ForLayer {
            uid: layer.uid,
            command: LayerRowCommand::ToggleAlphaLock,
        });
        ui.close();
    }
    menu_separator(ui);
    let can_merge = idx
        .checked_sub(1)
        .and_then(|i| doc.layers.get(i))
        .is_some_and(|below| below.blend == BlendMode::Normal && layer.blend == BlendMode::Normal);
    let duplicate_key = keymap::label_for(Action::LayerDuplicate);
    let merge_key = keymap::label_for(Action::LayerMergeDown);
    let rows: [(IconPaint, &str, &str, bool, LayerRowCommand); 4] = [
        (
            icons::paint_layer_duplicate_icon,
            "複製",
            duplicate_key.as_str(),
            doc.layers.len() < MAX_LAYERS,
            LayerRowCommand::Duplicate,
        ),
        (
            icons::paint_layer_move_up_icon,
            "上へ移動",
            "",
            idx + 1 < doc.layers.len(),
            LayerRowCommand::MoveUp,
        ),
        (
            icons::paint_layer_move_down_icon,
            "下へ移動",
            "",
            idx > 0,
            LayerRowCommand::MoveDown,
        ),
        (
            icons::paint_layer_merge_down_icon,
            "下と結合",
            merge_key.as_str(),
            can_merge,
            LayerRowCommand::MergeDown,
        ),
    ];
    for (paint, label, shortcut, enabled, command) in rows {
        if menu_row(ui, enabled, paint, label, shortcut, false) {
            *action = Some(LayersPanelAction::ForLayer {
                uid: layer.uid,
                command,
            });
            ui.close();
        }
    }
    menu_separator(ui);
    if menu_row(
        ui,
        doc.layers.len() > 1,
        icons::paint_delete_icon,
        "削除",
        "",
        true,
    ) {
        *action = Some(LayersPanelAction::ForLayer {
            uid: layer.uid,
            command: LayerRowCommand::Delete,
        });
        ui.close();
    }
}

/// 右クリックメニューの 1 行(アイコン+名前+右寄せの補足)。押されたら `true`。
fn menu_row(
    ui: &mut egui::Ui,
    enabled: bool,
    paint: IconPaint,
    label: &str,
    trailing: &str,
    danger: bool,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let width = ROW_MENU_WIDTH;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let base = if danger { theme::DANGER } else { theme::TEXT };
        let (text, icon) = if !enabled {
            (theme::TEXT_DISABLED, theme::TEXT_DISABLED)
        } else if response.hovered() {
            painter.rect_filled(rect, 6.0, theme::BG_HOVER);
            (
                if danger {
                    theme::DANGER
                } else {
                    egui::Color32::WHITE
                },
                if danger {
                    theme::DANGER
                } else {
                    egui::Color32::WHITE
                },
            )
        } else {
            (
                base,
                if danger {
                    theme::DANGER
                } else {
                    theme::TEXT_DIM
                },
            )
        };
        paint(
            painter,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 16.0, rect.center().y),
                egui::vec2(15.0, 15.0),
            ),
            icon,
        );
        painter.text(
            egui::pos2(rect.left() + 32.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.5),
            text,
        );
        if !trailing.is_empty() {
            painter.text(
                egui::pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                trailing,
                egui::FontId::proportional(12.0),
                if enabled {
                    theme::TEXT_FAINT
                } else {
                    theme::TEXT_DISABLED
                },
            );
        }
    }
    response.clicked()
}

fn menu_separator(ui: &mut egui::Ui) {
    let width = ROW_MENU_WIDTH;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 9.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(2.0),
        rect.center().y,
        egui::Stroke::new(1.0, theme::LINE_STRONG),
    );
}

fn layer_status(layer: &Layer) -> String {
    let opacity = (layer.opacity as f32 / 255.0 * 100.0).round() as u32;
    let mut status = format!("{} · {}%", layer.blend.label(), opacity);
    if !layer.visible {
        status.push_str(" · 非表示");
    }
    if layer.alpha_lock {
        status.push_str(" · 透明保護");
    }
    status
}

/// SPEC §50.1: 目アイコン(チェックボックス廃止)。挙動は従来と同一
/// (履歴に積まない・commit-first は `app.rs` 側)。v13 §60: 当たり判定を
/// 28px 四方に広げ、Alt+クリックで「このレイヤーだけ表示」(単独表示)。
fn show_eye_toggle(
    ui: &mut egui::Ui,
    doc: &Document,
    idx: usize,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(visible) = doc.layers.get(idx).map(|l| l.visible) else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(EYE_HIT, EYE_HIT), egui::Sense::hover());
    // 安定した id を与える(テストから `Context::read_response` で位置を
    // 引けるようにするため。自動採番の id はレイアウト依存で参照しづらい)。
    let response = ui.interact(
        rect,
        egui::Id::new(("darask_layer_eye", idx)),
        egui::Sense::click(),
    );
    let response = response.on_hover_text(if visible {
        "表示中(クリックで非表示 · Alt+クリックでこのレイヤーだけ表示)"
    } else {
        "非表示(クリックで表示 · Alt+クリックでこのレイヤーだけ表示)"
    });
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            visible,
            "レイヤーの表示",
        )
    });
    if ui.is_rect_visible(rect) {
        let color = if response.hovered() {
            ui.painter().rect_filled(rect, 6.0, theme::BG_HOVER);
            egui::Color32::WHITE
        } else if visible {
            theme::ICON
        } else {
            theme::TEXT_DISABLED
        };
        icons::paint_eye_icon(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), egui::vec2(EYE_ICON, EYE_ICON)),
            color,
            visible,
        );
    }
    if response.clicked() {
        if ui.input(|i| i.modifiers.alt) {
            action.get_or_insert(LayersPanelAction::Solo(idx));
        } else {
            action.get_or_insert(LayersPanelAction::SetVisible(idx, !visible));
        }
    }
}

/// SPEC §50.1: サムネイル(最大 48×36・縦横比維持・市松下地・そのレイヤーの
/// 画素のみ)。可視行かつ世代が変わったときだけ生成し、まだ無い行は
/// プレースホルダ(単色矩形)を描く。非表示レイヤーは薄く描く。
fn show_thumbnail(ui: &mut egui::Ui, doc: &Document, idx: usize, thumbnails: &mut ThumbnailCache) {
    let (rect, _) = ui.allocate_exact_size(THUMBNAIL_SLOT, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let Some(layer) = doc.layers.get(idx) else {
        return;
    };
    let texture = thumbnails.texture(ui.ctx(), idx, layer, doc.width, doc.height, doc.content_gen);
    let draw = match texture {
        Some(texture) => {
            let draw =
                egui::Rect::from_center_size(rect.center(), texture.size_vec2()).intersect(rect);
            let tint = if layer.visible {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_white_alpha(90)
            };
            ui.painter().image(
                texture.id(),
                draw,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint,
            );
            draw
        }
        None => {
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
            rect
        }
    };
    ui.painter().rect_stroke(
        draw,
        2.0,
        egui::Stroke::new(1.0, theme::LINE_STRONG),
        egui::StrokeKind::Outside,
    );
}

/// 名前と、その下に小さく「合成モード · 不透明度%」(+透明保護の錠)。表示のみ
/// (クリック・ダブルクリックは行全体の response が受ける —
/// `show_layer_row` のコメント参照)。
fn show_name_label(ui: &mut egui::Ui, doc: &Document, idx: usize, is_active: bool) {
    let Some(layer) = doc.layers.get(idx) else {
        return;
    };
    let (name_color, detail_color) = if is_active {
        (
            egui::Color32::WHITE,
            egui::Color32::from_rgb(0xb5, 0xb5, 0xbc),
        )
    } else if layer.visible {
        (theme::TEXT, theme::TEXT_FAINT)
    } else {
        (theme::TEXT_DISABLED, theme::TEXT_DISABLED)
    };
    let opacity_pct = (layer.opacity as f32 / 255.0 * 100.0).round() as i32;
    let mut detail = format!("{} · {}%", layer.blend.label(), opacity_pct);
    if !layer.visible {
        detail.push_str(" · 非表示");
    }
    // 2 行ぶんの高さを先に決めて確保する(行の中で縦中央に置くため)。
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(width, NAME_BLOCK_H),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_height(NAME_BLOCK_H);
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut name = egui::RichText::new(&layer.name)
                .size(13.0)
                .color(name_color);
            if is_active {
                name = name.strong();
            }
            ui.add(egui::Label::new(name).truncate().selectable(false));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // 錠は先に置く(省略表示される詳細文の後ろだと狭い幅ではみ出す)。
                if layer.alpha_lock {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(LOCK_BADGE, LOCK_BADGE),
                        egui::Sense::hover(),
                    );
                    icons::paint_alpha_lock_icon(ui.painter(), rect, theme::ACCENT, true);
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(detail)
                            .size(theme::FONT_SMALL)
                            .color(detail_color),
                    )
                    .truncate()
                    .selectable(false),
                );
            });
        },
    );
}

/// 名前編集(Enter/フォーカス外しで確定、Esc でキャンセル)。
fn show_rename_editor(
    ui: &mut egui::Ui,
    idx: usize,
    rename: &mut RenameState,
    action: &mut Option<LayersPanelAction>,
) {
    let Some((_, text, needs_focus)) = rename.as_mut() else {
        return;
    };
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(ui.available_width())
            .id(egui::Id::new(("darask_layer_rename", idx))),
    );
    // 編集開始フレームのみフォーカスを要求する(`RenameState` の
    // ドキュメントコメント参照)。
    if *needs_focus {
        response.request_focus();
    }
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        response.surrender_focus();
        *rename = None;
        return;
    }
    let lost_focus = response.lost_focus();
    if let Some((_, _, needs_focus)) = rename.as_mut() {
        *needs_focus = false;
    }
    if lost_focus {
        if let Some((_, text, _)) = rename.take() {
            let trimmed = text.trim().to_owned();
            if !trimmed.is_empty() {
                // 適用は app.rs(`modified` を立てる理由も含めて
                // `commit_rename_action` のコメント参照)。
                action.get_or_insert(LayersPanelAction::CommitRename(idx, trimmed));
            }
        }
    }
}

/// v13 §60: 目アイコンの Alt+クリック(単独表示)後の各レイヤーの表示状態。
/// すでに「そのレイヤーだけ表示」なら全レイヤーを表示に戻す(Photoshop と
/// 同じトグル)。それ以外はそのレイヤーだけを表示する。`idx` が範囲外なら
/// `None`。
pub fn solo_visibility(current: &[bool], idx: usize) -> Option<Vec<bool>> {
    if idx >= current.len() {
        return None;
    }
    let already_solo = current
        .iter()
        .enumerate()
        .all(|(i, visible)| *visible == (i == idx));
    Some(if already_solo {
        vec![true; current.len()]
    } else {
        (0..current.len()).map(|i| i == idx).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;

    #[test]
    fn escape_cancels_rename_without_committing() {
        let ctx = egui::Context::default();
        let doc = Document::new(4, 4, Background::White);
        let mut rename = Some((0, "changed name".into(), true));
        let mut thumbnails = ThumbnailCache::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, ctx_for(&doc, &mut rename, &mut thumbnails));
        });
        let mut action = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                action = show(ui, ctx_for(&doc, &mut rename, &mut thumbnails));
            },
        );
        assert!(action.is_none());
        assert!(rename.is_none());
        assert!(!ctx.egui_wants_keyboard_input());
    }

    #[test]
    fn long_names_and_locked_layers_fit_a_narrow_panel() {
        for width in [150.0, 210.0, 320.0] {
            let ctx = egui::Context::default();
            crate::ui::theme::apply(&ctx);
            let mut doc = Document::new(4, 4, Background::White);
            doc.layers[0].alpha_lock = true;
            let mut rename = None;
            let mut thumbnails = ThumbnailCache::default();
            let mut short_height = 0.0;
            for name in [
                "Short",
                "A very long layer name that must never widen the panel",
            ] {
                doc.layers[0].name = name.into();
                let mut bounds = egui::Rect::NOTHING;
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    ui.set_width(width);
                    show(ui, ctx_for(&doc, &mut rename, &mut thumbnails));
                    bounds = ui.min_rect();
                });
                assert!(bounds.width() <= width + 0.5, "width {width}: {bounds:?}");
                let row = ctx
                    .read_response(egui::Id::new(("darask_layer_row", 0)))
                    .expect("row");
                if name == "Short" {
                    short_height = row.rect.height();
                } else {
                    assert!(
                        (row.rect.height() - short_height).abs() <= 0.5,
                        "width {width}: {:?} vs {short_height}",
                        row.rect
                    );
                }
            }
        }
    }

    fn ctx_for<'a>(
        doc: &'a Document,
        rename: &'a mut RenameState,
        thumbnails: &'a mut ThumbnailCache,
    ) -> LayersPanelCtx<'a> {
        LayersPanelCtx {
            doc,
            rename,
            thumbnails,
            fill_height: None,
        }
    }

    /// 回帰テスト: src/ui/layers_panel.rs:171(元)のバグ。編集中の
    /// `TextEdit` に毎フレーム無条件で `request_focus()` を呼ぶと、
    /// singleline `TextEdit` が Enter で内部的に `surrender_focus()` しても
    /// 直後の `request_focus()` がフォーカスを奪い返してしまい
    /// `Response::lost_focus()` が恒久的に発火しなくなる(egui 0.35
    /// `Memory::lost_focus` は `!has_focus(id)` を必須条件に持つため)。
    /// `egui::Context` はバックエンド不要で複数フレームを直接駆動できる
    /// (`app.rs::ctx_with_key_event` と同じ手法)。
    #[test]
    fn layer_rename_textedit_loses_focus_on_enter_and_commits_name() {
        let doc = Document::new(4, 4, Background::White);
        let mut rename: RenameState = Some((0, "old".to_owned(), true));
        let mut thumbnails = ThumbnailCache::default();

        let ctx = egui::Context::default();

        // フレーム1: 編集開始フレーム。needs_focus=true → request_focus() が
        // 呼ばれ TextEdit がフォーカスを得る。
        ctx.begin_pass(egui::RawInput::default());
        egui::Area::new(egui::Id::new("test_area")).show(&ctx, |ui| {
            show(ui, ctx_for(&doc, &mut rename, &mut thumbnails));
        });
        let _ = ctx.end_pass();
        assert!(
            matches!(&rename, Some((_, _, needs_focus)) if !*needs_focus),
            "needs_focus must be consumed after the first frame"
        );
        assert_eq!(doc.layers[0].name, "背景", "still editing, name unchanged");

        // 編集中にテキストを変更してから Enter。singleline TextEdit は
        // Enter で内部的に surrender_focus する(egui 0.35
        // text_edit/builder.rs:1115)。
        if let Some((_, text, _)) = rename.as_mut() {
            *text = "new name".to_owned();
        }
        let mut committed = None;
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        });
        egui::Area::new(egui::Id::new("test_area")).show(&ctx, |ui| {
            committed = show(ui, ctx_for(&doc, &mut rename, &mut thumbnails));
        });
        let _ = ctx.end_pass();

        assert!(
            rename.is_none(),
            "Enter surrenders focus mid-frame; without the needs_focus fix the immediately \
             following request_focus() would reclaim it every frame and lost_focus() would \
             never fire, leaving the rename box (and keyboard shortcuts) stuck open forever"
        );
        // v8 レビュー修正②: パネルは `Document` を直接変更せず、確定内容を
        // `CommitRename` として返す(適用は app.rs の commit-first 経由)。
        assert_eq!(
            committed,
            Some(LayersPanelAction::CommitRename(0, "new name".to_owned())),
            "lost_focus() firing must request the rename commit"
        );
        assert_eq!(doc.layers[0].name, "背景", "パネル自身は文書を変更しない");
    }

    /// v12 §50.1: サムネイルは「可視行 × 世代が変わった行」だけを、
    /// 1 フレーム上限つきで生成する(毎フレーム再生成しない)。
    #[test]
    fn thumbnails_are_generated_once_per_generation_and_capped_per_frame() {
        let mut doc = Document::new(8, 8, Background::White);
        for i in 0..7 {
            assert!(doc.add_layer(format!("レイヤー {i}")));
        }
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        // `Area` は初回フレームが採寸パス(不可視)になり「可視行のみ生成」の
        // 条件を満たさないため、`Context::run_ui` のルート `Ui`(常に可視)に
        // 直接描く。
        let mut render = |thumbnails: &mut ThumbnailCache, doc: &Document| {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(ui, ctx_for(doc, &mut rename, thumbnails));
            });
        };

        render(&mut thumbnails, &doc);
        assert_eq!(
            thumbnails.generated_this_frame, MAX_THUMBNAILS_PER_FRAME,
            "1 フレームの生成枚数は上限で頭打ちになる"
        );
        render(&mut thumbnails, &doc);
        assert_eq!(
            thumbnails.generated_this_frame, MAX_THUMBNAILS_PER_FRAME,
            "残りの行は次のフレームで生成される"
        );
        render(&mut thumbnails, &doc);
        assert!(
            thumbnails.generated_this_frame < MAX_THUMBNAILS_PER_FRAME,
            "全行が揃ったら生成は止まる"
        );
        render(&mut thumbnails, &doc);
        assert_eq!(
            thumbnails.generated_this_frame, 0,
            "世代が変わらない限り 1 枚も再生成しない(アイドル CPU 0%)"
        );

        // 画素が変わる操作(= content_gen が増える)でだけ作り直す。
        doc.bump_content_gen();
        render(&mut thumbnails, &doc);
        assert!(
            thumbnails.generated_this_frame > 0,
            "世代が変われば再生成する"
        );
    }

    /// v12 §50.1(追いレビュー③): 1 フレーム上限で持ち越した行は、
    /// **追加の入力なしで**後続フレームに追いつき、全部揃ったら
    /// 再描画要求(`ThumbnailCache::has_pending` → `request_repaint`)が
    /// 止まる(有界であること)。
    #[test]
    fn thumbnails_catch_up_without_further_input_and_then_stop_requesting_repaint() {
        let mut doc = Document::new(8, 8, Background::White);
        for i in 0..8 {
            assert!(doc.add_layer(format!("レイヤー {i}")));
        }
        assert_eq!(doc.layers.len(), 9);
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        let render = |thumbnails: &mut ThumbnailCache, doc: &Document, rename: &mut RenameState| {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(
                    ui,
                    LayersPanelCtx {
                        doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
        };

        // 9 行 ÷ 4 枚/フレーム → 3 フレームで揃う。持ち越しがある間だけ
        // `has_pending()`(= `request_repaint` を出すフレーム)が真になる。
        let mut pending_frames = 0;
        let mut settled = None;
        for frame in 0..12 {
            render(&mut thumbnails, &doc, &mut rename);
            if thumbnails.has_pending() {
                pending_frames += 1;
            } else {
                settled = Some(frame);
                break;
            }
        }
        assert_eq!(settled, Some(2), "3 フレーム目(添字 2)で全行が揃う");
        assert_eq!(pending_frames, 2, "持ち越しがある 2 フレームだけ要求する");
        assert!(
            thumbnails.entries.iter().all(|e| e.is_some()),
            "全行のサムネイルが揃っている"
        );

        // 追加入力なしの後続フレームでは 1 枚も再生成せず、要求も出さない
        // (アイドル CPU 0%)。
        render(&mut thumbnails, &doc, &mut rename);
        assert_eq!(thumbnails.generated_this_frame, 0);
        assert!(!thumbnails.has_pending());

        // 並べ替え(枚数は変わらない)+ `invalidate_all`(app.rs の
        // `push_layer_history` が呼ぶ経路と同じ状態)からも同様に追いつく。
        assert!(doc.move_layer(0, 8));
        thumbnails.invalidate_all();
        let mut settled_after_reorder = None;
        for frame in 0..12 {
            render(&mut thumbnails, &doc, &mut rename);
            if !thumbnails.has_pending() {
                settled_after_reorder = Some(frame);
                break;
            }
        }
        assert_eq!(settled_after_reorder, Some(2));
        assert!(thumbnails.entries.iter().all(|e| e.is_some()));
    }

    /// 縮小できない入力(0×0 レイヤー)は「その世代は完了」として記録され、
    /// 再描画要求のループにならない。
    #[test]
    fn undrawable_thumbnails_are_cached_as_done_and_do_not_loop() {
        let doc = Document::new(0, 0, Background::White);
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();
        let render = |thumbnails: &mut ThumbnailCache, rename: &mut RenameState| {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(
                    ui,
                    LayersPanelCtx {
                        doc: &doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
        };
        render(&mut thumbnails, &mut rename);
        assert!(!thumbnails.has_pending());
        render(&mut thumbnails, &mut rename);
        assert_eq!(thumbnails.generated_this_frame, 0, "再試行しない");
        assert!(!thumbnails.has_pending());
    }

    /// v12 §50.1: レイヤー枚数が変わったらキャッシュを全消去する
    /// (行とレイヤーの対応が総入れ替えになるため)。
    #[test]
    fn thumbnail_cache_is_dropped_when_the_layer_count_changes() {
        let mut cache = ThumbnailCache::default();
        cache.begin_frame(3);
        assert_eq!(cache.entries.len(), 3);
        cache.begin_frame(5);
        assert_eq!(cache.entries.len(), 5);
        assert!(cache.entries.iter().all(|e| e.is_none()));
        cache.invalidate_all();
        assert!(cache.entries.is_empty());
    }

    /// v12 §50.1: 実際のポインタ操作(押下 → 移動 → 離す)でドラッグ&ドロップ
    /// 並べ替えが `Move` 要求になることの結線テスト。行の矩形は egui の
    /// `Context::read_response` から取るのでレイアウト定数に依存しない。
    #[test]
    fn dragging_the_bottom_row_to_the_top_requests_a_move_to_the_topmost_index() {
        let mut doc = Document::new(4, 4, Background::White);
        for name in ["1", "2"] {
            assert!(doc.add_layer(name.to_owned()));
        }
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        let frame = |events: Vec<egui::Event>,
                     pointer: Option<egui::Pos2>,
                     rename: &mut RenameState,
                     thumbnails: &mut ThumbnailCache|
         -> Option<LayersPanelAction> {
            let mut action = None;
            let mut input = egui::RawInput {
                events,
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.insert(0, egui::Event::PointerMoved(pos));
            }
            let _ = ctx.run_ui(input, |ui| {
                action = show(
                    ui,
                    LayersPanelCtx {
                        doc: &doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
            action
        };

        // レイアウトを確定させ、行の矩形を読む(行 id は表示順ではなく
        // レイヤー添字で作られる)。
        frame(Vec::new(), None, &mut rename, &mut thumbnails);
        let row_rect = |idx: usize| {
            ctx.read_response(egui::Id::new(("darask_layer_row", idx)))
                .map(|r| r.rect)
                .unwrap_or_else(|| panic!("row {idx} must have been laid out"))
        };
        let bottom = row_rect(0).center();
        // 一番上の行(= 最上位レイヤー 2)の上端より少し上へ落とす = スロット 0。
        let top_edge = row_rect(2).top() + 1.0;
        let drop_pos = egui::pos2(bottom.x, top_edge);

        // 押下 → ドラッグ開始(移動)→ 離す。
        let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            vec![press(bottom, true)],
            Some(bottom),
            &mut rename,
            &mut thumbnails,
        );
        frame(Vec::new(), Some(drop_pos), &mut rename, &mut thumbnails);
        let action = frame(
            vec![press(drop_pos, false)],
            Some(drop_pos),
            &mut rename,
            &mut thumbnails,
        );

        assert_eq!(
            action,
            Some(LayersPanelAction::Move { from: 0, to: 2 }),
            "最下層の行を一覧の一番上へドロップしたら最上位への移動要求になる"
        );
    }

    /// 行をクリックしたらアクティブ化要求になる(ドラッグ源で包んでも
    /// 内側のクリックが飲まれないこと。egui の hit_test は「大きなドラッグ
    /// 背景の上の小さなクリック widget」を両立させる)。
    #[test]
    fn clicking_a_row_still_requests_activation() {
        let mut doc = Document::new(4, 4, Background::White);
        assert!(doc.add_layer("1".to_owned()));
        assert_eq!(doc.active, 1);
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        let frame = |events: Vec<egui::Event>,
                     pointer: Option<egui::Pos2>,
                     rename: &mut RenameState,
                     thumbnails: &mut ThumbnailCache|
         -> Option<LayersPanelAction> {
            let mut action = None;
            let mut input = egui::RawInput {
                events,
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.insert(0, egui::Event::PointerMoved(pos));
            }
            let _ = ctx.run_ui(input, |ui| {
                action = show(
                    ui,
                    LayersPanelCtx {
                        doc: &doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
            action
        };

        frame(Vec::new(), None, &mut rename, &mut thumbnails);
        // 行 0(最下層)の名前ラベルの位置をクリックする。
        let rect = ctx
            .read_response(egui::Id::new(("darask_layer_row", 0)))
            .map(|r| r.rect)
            .expect("row laid out");
        let pos = egui::pos2(rect.right() - 12.0, rect.center().y);
        let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        // 押下の前に「ポインタがその widget の上にあるフレーム」が要る
        // (egui の当たり判定は前フレームの widget 矩形を使うため)。
        frame(Vec::new(), Some(pos), &mut rename, &mut thumbnails);
        frame(
            vec![press(pos, true)],
            Some(pos),
            &mut rename,
            &mut thumbnails,
        );
        let action = frame(
            vec![press(pos, false)],
            Some(pos),
            &mut rename,
            &mut thumbnails,
        );
        assert_eq!(action, Some(LayersPanelAction::Activate(0)));
    }

    /// 目アイコンのクリックは表示切替要求になる(チェックボックス廃止後も
    /// 挙動は同一 — SPEC §50.1)。
    #[test]
    fn clicking_the_eye_icon_requests_a_visibility_toggle() {
        let doc = Document::new(4, 4, Background::White);
        assert!(doc.layers[0].visible);
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        let frame = |events: Vec<egui::Event>,
                     pointer: Option<egui::Pos2>,
                     rename: &mut RenameState,
                     thumbnails: &mut ThumbnailCache|
         -> Option<LayersPanelAction> {
            let mut action = None;
            let mut input = egui::RawInput {
                events,
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.insert(0, egui::Event::PointerMoved(pos));
            }
            let _ = ctx.run_ui(input, |ui| {
                action = show(
                    ui,
                    LayersPanelCtx {
                        doc: &doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
            action
        };

        frame(Vec::new(), None, &mut rename, &mut thumbnails);
        let pos = ctx
            .read_response(egui::Id::new(("darask_layer_eye", 0)))
            .map(|r| r.rect.center())
            .expect("eye icon laid out");
        let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        // 押下の前に「ポインタがその widget の上にあるフレーム」が要る
        // (egui の当たり判定は前フレームの widget 矩形を使うため)。
        frame(Vec::new(), Some(pos), &mut rename, &mut thumbnails);
        frame(
            vec![press(pos, true)],
            Some(pos),
            &mut rename,
            &mut thumbnails,
        );
        let action = frame(
            vec![press(pos, false)],
            Some(pos),
            &mut rename,
            &mut thumbnails,
        );
        assert_eq!(action, Some(LayersPanelAction::SetVisible(0, false)));
    }

    /// 同じ位置へのドロップは no-op(履歴に積まない — SPEC §50.1)。
    #[test]
    fn dropping_a_row_onto_itself_requests_nothing() {
        let mut doc = Document::new(4, 4, Background::White);
        assert!(doc.add_layer("1".to_owned()));
        let mut rename: RenameState = None;
        let mut thumbnails = ThumbnailCache::default();
        let ctx = egui::Context::default();

        let frame = |events: Vec<egui::Event>,
                     pointer: Option<egui::Pos2>,
                     rename: &mut RenameState,
                     thumbnails: &mut ThumbnailCache|
         -> Option<LayersPanelAction> {
            let mut action = None;
            let mut input = egui::RawInput {
                events,
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.insert(0, egui::Event::PointerMoved(pos));
            }
            let _ = ctx.run_ui(input, |ui| {
                action = show(
                    ui,
                    LayersPanelCtx {
                        doc: &doc,
                        rename,
                        thumbnails,
                        fill_height: None,
                    },
                );
            });
            action
        };

        frame(Vec::new(), None, &mut rename, &mut thumbnails);
        let rect = ctx
            .read_response(egui::Id::new(("darask_layer_row", 1)))
            .map(|r| r.rect)
            .expect("row laid out");
        let start = rect.center();
        // 自分の行の中(中心より少し上)へ落とす = 元の位置。
        let end = egui::pos2(start.x, rect.top() + 1.0);
        let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            vec![press(start, true)],
            Some(start),
            &mut rename,
            &mut thumbnails,
        );
        frame(Vec::new(), Some(end), &mut rename, &mut thumbnails);
        let action = frame(
            vec![press(end, false)],
            Some(end),
            &mut rename,
            &mut thumbnails,
        );
        assert_eq!(action, None, "同位置へのドロップは要求を出さない");
    }

    /// v12 §50.1: 表示順スロット → `Document::move_layer` の `to` への変換。
    /// 一覧は最上位が先頭(表示順は文書添字の逆順)。
    #[test]
    fn list_height_takes_the_rest_but_keeps_two_rows() {
        assert_eq!(list_height(500.0, 90.0, 40.0), 370.0);
        assert_eq!(list_height(100.0, 90.0, 40.0), ROW_HEIGHT * 2.0 + 2.0);
    }

    #[test]
    fn solo_visibility_isolates_one_layer_and_toggles_back_to_all() {
        assert_eq!(
            solo_visibility(&[true, true, false], 0),
            Some(vec![true, false, false])
        );
        // 非表示のレイヤーでも単独表示にできる。
        assert_eq!(
            solo_visibility(&[true, true, false], 2),
            Some(vec![false, false, true])
        );
        // すでに単独表示なら全表示へ戻す。
        assert_eq!(
            solo_visibility(&[false, true, false], 1),
            Some(vec![true, true, true])
        );
        assert_eq!(solo_visibility(&[true], 0), Some(vec![true]));
        assert_eq!(solo_visibility(&[true, true], 2), None);
    }

    #[test]
    fn drop_target_index_maps_display_slots_to_document_indices() {
        // 3 枚(文書添字 0=最下層 .. 2=最上位)、表示は [2, 1, 0]。
        // 最下層(0)を一番上(slot 0)へ → to = 2。
        assert_eq!(drop_target_index(0, 0, 3), Some(2));
        // 最上位(2)を一番下(slot 3)へ → to = 0。
        assert_eq!(drop_target_index(2, 3, 3), Some(0));
        // 中央(1)を一番上へ → to = 2。
        assert_eq!(drop_target_index(1, 0, 3), Some(2));
        // 自分自身の直前・直後は no-op。
        assert_eq!(drop_target_index(2, 0, 3), None);
        assert_eq!(drop_target_index(2, 1, 3), None);
        assert_eq!(drop_target_index(0, 2, 3), None);
        assert_eq!(drop_target_index(0, 3, 3), None);
    }

    /// 行の中心を境に「その行の手前」へ挿入する。
    #[test]
    fn insertion_slot_uses_row_midpoints() {
        let row =
            |top: f32| egui::Rect::from_min_size(egui::pos2(0.0, top), egui::vec2(100.0, 30.0));
        let rows = vec![(2usize, row(0.0)), (1, row(30.0)), (0, row(60.0))];
        assert_eq!(insertion_slot(&rows, 5.0), 0);
        assert_eq!(insertion_slot(&rows, 20.0), 1);
        assert_eq!(insertion_slot(&rows, 50.0), 2);
        assert_eq!(insertion_slot(&rows, 89.0), 3);
    }
}
