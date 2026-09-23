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
//! v12 §50.1 で行の構成を **[目アイコン][サムネイル][名前]** に刷新し、
//! ドラッグ&ドロップでの並べ替え(1 undo 単位)と、一覧直上の
//! 不透明度 / ブレンド / アルファロックを追加した。サムネイルの生成は
//! `Document::content_gen`(成功した変更でのみ増える世代)が変わった
//! **可視行**だけ・1 フレーム最大 `MAX_THUMBNAILS_PER_FRAME` 枚に制限する
//! (SPEC §50.1: 「毎フレーム禁止 — アイドル CPU 0%」)。

use eframe::egui;

use crate::document::{BlendMode, Document, Layer, MAX_LAYERS};
use crate::keymap::{self, Action};
use crate::raster;
use crate::ui::icons;

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
const THUMBNAIL_MAX_W: u32 = 40;
const THUMBNAIL_MAX_H: u32 = 30;

/// 1 フレームに新規生成するサムネイルの上限(ARCHITECTURE.md §22.1)。
/// 多層文書で undo するとすべての行の世代が一度に変わるため、上限が無いと
/// 1 フレームに 64 枚の縮小+テクスチャ確保が走る。
const MAX_THUMBNAILS_PER_FRAME: usize = 4;

/// 行に確保するサムネイル枠(実サムネイルは縦横比維持でこの中に収める)。
const THUMBNAIL_SLOT: egui::Vec2 = egui::vec2(THUMBNAIL_MAX_W as f32, THUMBNAIL_MAX_H as f32);
/// 目アイコンのサイズ(正方形)。
const EYE_SIZE: f32 = 16.0;
const ROW_HEIGHT: f32 = 40.0;
const ICON_BUTTON_SIZE: f32 = 26.0;

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
}

/// レイヤーパネルを描画する。要求された操作があれば返す(`Document` は
/// 一切変更しない — 読み取り専用)。
pub fn show(ui: &mut egui::Ui, ctx: LayersPanelCtx) -> Option<LayersPanelAction> {
    let LayersPanelCtx {
        doc,
        rename,
        thumbnails,
    } = ctx;
    let mut action = None;

    let active = doc.active_index();
    let layer_count = doc.layers.len();
    thumbnails.begin_frame(layer_count);

    // SPEC §50.1: 「不透明度スライダー・ブレンドモード・アルファロックは
    // 一覧の直上に置く」。
    show_active_layer_controls(ui, doc, active, &mut action);
    ui.add_space(4.0);

    // 操作は一覧の前にまとめる。多層文書で末尾までスクロールする必要がない。
    show_layer_toolbar(ui, doc, &mut action);
    ui.add_space(2.0);
    show_layer_list(ui, doc, active, rename, thumbnails, &mut action);
    // v12 §50.1(追いレビュー③): 1 フレームの生成上限で持ち越した可視行が
    // ある間だけ、**条件付きで** 1 回だけ再描画を要求して追いつかせる。
    // 全行が揃った(または生成不能として記録された)フレームでは要求しない
    // ので、無入力状態では有限回で止まる(アイドル CPU 0%)。
    if thumbnails.has_pending() {
        ui.ctx().request_repaint();
    }

    action
}

fn show_layer_toolbar(ui: &mut egui::Ui, doc: &Document, action: &mut Option<LayersPanelAction>) {
    let active = doc.active_index();
    let count = doc.layers.len();
    let buttons = [
        (
            "add",
            keymap::menu_label("新規レイヤー", Action::LayerAdd),
            count < MAX_LAYERS,
            icons::paint_layer_add_icon as fn(&egui::Painter, egui::Rect, egui::Color32),
            LayersPanelAction::Add,
        ),
        (
            "duplicate",
            keymap::menu_label("レイヤーを複製", Action::LayerDuplicate),
            count < MAX_LAYERS,
            icons::paint_layer_duplicate_icon,
            LayersPanelAction::Duplicate,
        ),
        (
            "up",
            "上へ移動".into(),
            active + 1 < count,
            icons::paint_layer_move_up_icon,
            LayersPanelAction::MoveUp,
        ),
        (
            "down",
            "下へ移動".into(),
            active > 0,
            icons::paint_layer_move_down_icon,
            LayersPanelAction::MoveDown,
        ),
        (
            "merge",
            if active > 0 && !doc.can_merge_active_down() {
                "「通常」以外のブレンドを含むレイヤーは結合できません".into()
            } else {
                keymap::menu_label("下と結合", Action::LayerMergeDown)
            },
            doc.can_merge_active_down(),
            icons::paint_layer_merge_down_icon,
            LayersPanelAction::MergeDown,
        ),
        (
            "delete",
            "レイヤーを削除".into(),
            count > 1,
            icons::paint_layer_delete_icon,
            LayersPanelAction::Delete,
        ),
    ];
    let columns = ((ui.available_width() + 4.0) / (ICON_BUTTON_SIZE + 4.0))
        .floor()
        .clamp(1.0, 6.0) as usize;
    for row in buttons.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (id, label, enabled, paint, requested) in row {
                let response = ui
                    .add_enabled_ui(*enabled, |ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::splat(ICON_BUTTON_SIZE),
                            egui::Sense::hover(),
                        );
                        let response = ui.interact(
                            rect,
                            ui.id().with(("layer_action", id)),
                            egui::Sense::click(),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                ui.is_enabled(),
                                label,
                            )
                        });
                        if ui.is_rect_visible(rect) {
                            let visuals = ui.style().interact(&response);
                            ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
                            paint(ui.painter(), rect.shrink(4.0), visuals.fg_stroke.color);
                        }
                        response
                    })
                    .inner
                    .on_hover_text(label)
                    .on_disabled_hover_text(label);
                if response.clicked() {
                    *action = Some(requested.clone());
                }
            }
        });
    }
}

/// SPEC §50.1: 一覧の直上に置くアクティブレイヤーの設定
/// (不透明度 §13 / ブレンド §50.2 / アルファロック §50.3)。
fn show_active_layer_controls(
    ui: &mut egui::Ui,
    doc: &Document,
    active: usize,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(layer) = doc.layers.get(active) else {
        return;
    };

    // ARCHITECTURE.md §14.9-8: 値が実際に変わったフレームだけ要求を出す
    // (ドラッグ中の全面 recomposite を毎フレーム 1 回に抑える)。
    let mut opacity_pct = (layer.opacity as f32 / 255.0 * 100.0).round() as i32;
    ui.horizontal(|ui| {
        ui.label("不透明度");
        let value_width = ui
            .painter()
            .layout_no_wrap(
                "100%".into(),
                ui.style().drag_value_text_style.resolve(ui.style()),
                egui::Color32::WHITE,
            )
            .size()
            .x
            + 2.0 * ui.spacing().button_padding.x;
        ui.spacing_mut().slider_width = (ui.available_width()
            - value_width.max(ui.spacing().interact_size.x)
            - ui.spacing().item_spacing.x
            - 2.0)
            .max(16.0);
        if ui
            .add(egui::Slider::new(&mut opacity_pct, 0..=100).suffix("%"))
            .changed()
        {
            let new_opacity = ((opacity_pct.clamp(0, 100) as f32) / 100.0 * 255.0).round() as u8;
            if layer.opacity != new_opacity {
                *action = Some(LayersPanelAction::SetOpacity(new_opacity));
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label("合成:");
        egui::ComboBox::from_id_salt("darask_layer_blend")
            .selected_text(layer.blend.label())
            .width(
                (ui.available_width() - ICON_BUTTON_SIZE - ui.spacing().item_spacing.x).max(60.0),
            )
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
            });

        // アルファロックのトグル(市松+錠のアイコン、SPEC §50.3)。
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(ICON_BUTTON_SIZE), egui::Sense::click());
        let response = response.on_hover_text(if layer.alpha_lock {
            "透明保護: ON(透明部分を保護。クリックで解除)"
        } else {
            "透明保護: OFF(クリックで透明部分を保護)"
        });
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                ui.is_enabled(),
                layer.alpha_lock,
                "透明保護",
            )
        });
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact_selectable(&response, layer.alpha_lock);
            ui.painter().rect_filled(rect, 3.0, visuals.weak_bg_fill);
            icons::paint_alpha_lock_icon(
                ui.painter(),
                rect.shrink(5.0),
                visuals.fg_stroke.color,
                layer.alpha_lock,
            );
        }
        if response.clicked() {
            *action = Some(LayersPanelAction::SetAlphaLock(!layer.alpha_lock));
        }
    });
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

    let list_response = ui
        .scope(|ui| {
            for idx in (0..layer_count).rev() {
                let row = show_layer_row(ui, doc, idx, active, rename, thumbnails, action);
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

    let Some(dragged) = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p) else {
        return;
    };
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
    } else {
        // ドラッグ中(まだ離していない)。`dragged` は指標の描画にだけ使う。
        let _ = dragged;
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

/// 挿入位置のインジケータ(SPEC §50.1: 「挿入位置インジケータ」)。
fn draw_insertion_indicator(ui: &egui::Ui, rows: &[(usize, egui::Rect)], slot: usize) {
    let y = match rows.get(slot) {
        Some((_, rect)) => rect.top(),
        None => match rows.last() {
            Some((_, rect)) => rect.bottom(),
            None => return,
        },
    };
    let Some((_, first)) = rows.first() else {
        return;
    };
    let color = ui.visuals().selection.bg_fill;
    ui.painter().hline(
        first.left()..=first.right(),
        y,
        egui::Stroke::new(2.0, color),
    );
}

/// 1 行 = [目アイコン][サムネイル][名前]。戻り値は行全体の矩形
/// (ドラッグ&ドロップの挿入位置計算に使う)。
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
fn show_layer_row(
    ui: &mut egui::Ui,
    doc: &Document,
    idx: usize,
    active: usize,
    rename: &mut RenameState,
    thumbnails: &mut ThumbnailCache,
    action: &mut Option<LayersPanelAction>,
) -> egui::Rect {
    let is_editing = matches!(rename, Some((i, _, _)) if *i == idx);
    let is_active = idx == active;
    let row_width = ui.available_width();
    let fill = if is_active {
        ui.visuals().selection.bg_fill
    } else {
        ui.visuals().faint_bg_color
    };
    let frame = egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(4, 4))
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width((row_width - 8.0).max(0.0));
            ui.horizontal(|ui| {
                ui.set_min_height(ROW_HEIGHT);
                ui.spacing_mut().item_spacing.x = 5.0;
                show_eye_toggle(ui, doc, idx, action);
                if is_editing {
                    show_thumbnail(ui, doc, idx, thumbnails);
                    show_rename_editor(ui, idx, rename, action);
                    return;
                }
                let body_width = ui.available_width();
                let dragged =
                    ui.dnd_drag_source(egui::Id::new(("darask_layer_row", idx)), idx, |ui| {
                        ui.set_min_size(egui::vec2(body_width, ROW_HEIGHT));
                        ui.horizontal(|ui| {
                            show_thumbnail(ui, doc, idx, thumbnails);
                            show_name_label(ui, doc, idx, is_active);
                        });
                    });
                let row = dragged
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::Grab);
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
                        "{}\n{}\nダブルクリックで名前変更 / ドラッグで並べ替え / 右クリックで操作",
                        layer.name,
                        layer_status(layer)
                    ));
                }
            });
        });
    let rect = frame.response.rect;
    if is_active && ui.is_rect_visible(rect) {
        let stripe = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, 4.0),
            egui::vec2(3.0, rect.height() - 8.0),
        );
        ui.painter()
            .rect_filled(stripe, 2.0, ui.visuals().selection.stroke.color);
    }
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
    if ui.button("名前を変更").clicked() {
        *rename = Some((idx, layer.name.clone(), true));
        ui.close();
    }
    if ui
        .button(if layer.visible {
            "非表示にする"
        } else {
            "表示する"
        })
        .clicked()
    {
        *action = Some(LayersPanelAction::SetVisible(idx, !layer.visible));
        ui.close();
    }
    if ui
        .selectable_label(layer.alpha_lock, "透明部分を保護")
        .clicked()
    {
        *action = Some(LayersPanelAction::ForLayer {
            uid: layer.uid,
            command: LayerRowCommand::ToggleAlphaLock,
        });
        ui.close();
    }
    ui.separator();
    let can_merge = idx
        .checked_sub(1)
        .and_then(|i| doc.layers.get(i))
        .is_some_and(|below| below.blend == BlendMode::Normal && layer.blend == BlendMode::Normal);
    for (label, enabled, command) in [
        (
            "複製",
            doc.layers.len() < MAX_LAYERS,
            LayerRowCommand::Duplicate,
        ),
        (
            "上へ移動",
            idx + 1 < doc.layers.len(),
            LayerRowCommand::MoveUp,
        ),
        ("下へ移動", idx > 0, LayerRowCommand::MoveDown),
        ("下と結合", can_merge, LayerRowCommand::MergeDown),
        ("削除", doc.layers.len() > 1, LayerRowCommand::Delete),
    ] {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            *action = Some(LayersPanelAction::ForLayer {
                uid: layer.uid,
                command,
            });
            ui.close();
        }
    }
}

/// SPEC §50.1: 目アイコン(チェックボックス廃止)。挙動は従来と同一
/// (履歴に積まない・commit-first は `app.rs` 側)。
fn show_eye_toggle(
    ui: &mut egui::Ui,
    doc: &Document,
    idx: usize,
    action: &mut Option<LayersPanelAction>,
) {
    let Some(visible) = doc.layers.get(idx).map(|l| l.visible) else {
        return;
    };
    let (rect, _) =
        ui.allocate_exact_size(egui::Vec2::splat(ICON_BUTTON_SIZE), egui::Sense::hover());
    // 安定した id を与える(テストから `Context::read_response` で位置を
    // 引けるようにするため。自動採番の id はレイアウト依存で参照しづらい)。
    let response = ui.interact(
        rect,
        egui::Id::new(("darask_layer_eye", idx)),
        egui::Sense::click(),
    );
    let response = response.on_hover_text(if visible {
        "表示中(クリックで非表示)"
    } else {
        "非表示(クリックで表示)"
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
        let color = if visible {
            ui.visuals().widgets.active.fg_stroke.color
        } else {
            ui.visuals().widgets.noninteractive.fg_stroke.color
        };
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
        }
        icons::paint_eye_icon(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(EYE_SIZE)),
            color,
            visible,
        );
    }
    if response.clicked() {
        action.get_or_insert(LayersPanelAction::SetVisible(idx, !visible));
    }
}

/// SPEC §50.1: サムネイル(最大 40×30・縦横比維持・市松下地・そのレイヤーの
/// 画素のみ)。可視行かつ世代が変わったときだけ生成し、まだ無い行は
/// プレースホルダ(単色矩形)を描く。
fn show_thumbnail(ui: &mut egui::Ui, doc: &Document, idx: usize, thumbnails: &mut ThumbnailCache) {
    let (rect, _) = ui.allocate_exact_size(THUMBNAIL_SLOT, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let Some(layer) = doc.layers.get(idx) else {
        return;
    };
    let texture = thumbnails.texture(ui.ctx(), idx, layer, doc.width, doc.height, doc.content_gen);
    ui.painter()
        .rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    match texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let draw = egui::Rect::from_center_size(rect.center(), size).intersect(rect);
            ui.painter().image(
                texture.id(),
                draw,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            ui.painter()
                .rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        }
    }
    ui.painter().rect_stroke(
        rect,
        2.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

/// 名前(表示のみ。クリック・ダブルクリックは行全体の response が受ける —
/// `show_layer_row` のコメント参照)。
fn show_name_label(ui: &mut egui::Ui, doc: &Document, idx: usize, is_active: bool) {
    let Some(layer) = doc.layers.get(idx) else {
        return;
    };
    let width = (ui.available_width()
        - if layer.alpha_lock {
            EYE_SIZE + ui.spacing().item_spacing.x
        } else {
            0.0
        })
    .max(0.0);
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 2.0;
        let color = if layer.visible {
            ui.visuals().widgets.inactive.fg_stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        let mut name = egui::RichText::new(&layer.name).color(color);
        if is_active {
            name = name.strong();
        }
        ui.add(egui::Label::new(name).truncate());
        ui.add(
            egui::Label::new(
                egui::RichText::new(layer_status(layer))
                    .size(10.0)
                    .color(ui.visuals().widgets.noninteractive.fg_stroke.color),
            )
            .truncate(),
        );
    });
    if layer.alpha_lock {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(EYE_SIZE), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            icons::paint_alpha_lock_icon(
                ui.painter(),
                rect,
                ui.visuals().widgets.inactive.fg_stroke.color,
                true,
            );
        }
    }
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
