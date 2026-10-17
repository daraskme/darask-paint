//! ドッキングパネルの描画(SPEC §58、ARCHITECTURE.md §22.6b)。
//!
//! v12 P1.5 以前は「右パネル固定・幅 210px」の枠だった(SPEC §3)。
//! §58 で各セクション(色 / レイヤー / 履歴、将来ページ §54)は
//! **右ドック / 左ドック / フローティング**の 3 状態を取れるパネルになり、
//! このモジュールはその**ドックレンダラ**になった:
//!
//! - `panels::PanelLayout` が「どのパネルがどこに、どの順で、折りたたまれて
//!   いるか」を持つ(永続化・並べ替え規則はすべてあちら側の純粋ロジック)。
//! - ここは配置に従って `egui::Panel::left/right` と `egui::Window` を出し、
//!   ヘッダのドラッグ&ドロップ・「▾」メニューの結果を `PanelLayout` へ
//!   書き戻すだけ。パネル本体の中身は従来どおり
//!   `color_panel`/`layers_panel`/`history_panel` に委譲する。
//!
//! # egui 0.35 での注意点(ARCHITECTURE.md §22.6b の落とし穴)
//!
//! 1. **パネル宣言順**(左→右→中央)。`app.rs` はツールバー(left)の直後に
//!    これを呼び、`options_bar`(top)・`CentralPanel` はその後に来る。
//! 2. **`dnd_drag_source` は中身より後に `Sense::drag()` の widget を登録する**
//!    ため、ヘッダ全体を素直に包むと内部のボタン(折りたたみ矢印・▾)が
//!    クリック不能になる(`layers_panel.rs::show_layer_row` のコメント参照)。
//!    ここでも同じ流儀で「ボタンはドラッグ源の外、クリックは
//!    `Response::interact(Sense::click())` で足す」構成にしている。
//! 3. **フローティングの移動は自前**。`egui::Window` は `movable(false)` +
//!    `current_pos` で位置を `PanelLayout` から毎フレーム与え、ヘッダの
//!    ドラッグ量だけ動かす。こうするとヘッダ 1 か所で「移動」と「ドックへ
//!    戻す」の両方を扱え、egui の内部 id(タイトルバーのドラッグ判定)に
//!    依存しないで済む。リサイズは egui に任せ、確定値を書き戻す。
//! 4. **`request_repaint` は「配置が変わったフレーム」だけ**。ドラッグ中の
//!    再描画は入力イベントが駆動するので不要だが、ドロップやメニューによる
//!    配置変更はパネルを描き終えたあとに確定するため、その 1 回だけ次の
//!    フレームを予約する(CLAUDE.md が禁じているのは*無条件*の
//!    `request_repaint()`。アイドル時は呼ばれない — `show` 末尾)。
//! 5. **モーダル表示中は配置操作を止める**(`show` の `interactive`)。
//!    ドロップ判定をポインタ座標の幾何比較で行う都合上、egui のモーダル
//!    入力ブロックだけでは「裏で再ドックされる」のを防げない。

use eframe::egui;

use crate::document::Document;
use crate::history::History;
use crate::pages::PageSet;
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::history_panel;
use crate::ui::layers_panel::{
    self, LayersPanelAction, LayersPanelCtx, RenameState, ThumbnailCache,
};
use crate::ui::pages_panel::{self, PageThumbnailCache};
use crate::ui::panels::{self, DockSide, PanelKind, PanelLayout, PanelMove, PanelPlacement};
use crate::ui::{icons, theme, widgets};

/// ヘッダの折りたたみ矢印の一辺。
const HEADER_ARROW_SIZE: f32 = 16.0;
/// ヘッダ右端の「…」ボタンの一辺。
const HEADER_MENU_SIZE: f32 = 24.0;
/// v13 §60: ヘッダの高さとパネル本体の左右余白。
const HEADER_H: f32 = 34.0;
const BODY_MARGIN_X: i8 = 10;
const BODY_MARGIN_BOTTOM: f32 = 10.0;
/// レイヤーパネル本体(プロパティ欄+一覧+操作ボタン)に最低限確保する高さ。
/// ドックが低いときはこれ以上は縮めず、ドック全体のスクロールに任せる。
const MIN_LAYERS_BODY_H: f32 = 230.0;
/// ドラッグでフローティング化したとき、ポインタをヘッダのどのあたりに
/// 置いたことにするか(見た目の飛びを減らすためだけの値)。
const FLOAT_DROP_GRAB_Y: f32 = 12.0;

/// `show` に渡すアクティブタブ+色の状態一式(`app.rs` 側で `&mut Tab` と
/// アプリ側フィールドを 1 回ずつ借りて束ねる)。各パネル本体はここから
/// **互いに素な**フィールドだけを使うので、同じ 1 フレーム中にドック・
/// フローティングのどこから呼ばれても借用が衝突しない。
pub struct PanelsCtx<'a> {
    pub doc: &'a Document,
    pub rename: &'a mut RenameState,
    pub thumbnails: &'a mut ThumbnailCache,
    pub history: &'a History,
    pub pages: Option<&'a mut PageSet>,
    pub page_thumbnails: &'a mut PageThumbnailCache,
    pub color: ColorPanelCtx<'a>,
}

/// パネル本体で起きた「呼び出し側(`app.rs`)が実行すべき操作」。
/// 従来の `(Option<LayersPanelAction>, Option<usize>)` を構造体にしたもの。
#[derive(Default)]
pub struct PanelsOutput {
    pub layer_action: Option<LayersPanelAction>,
    /// 履歴パネルの行クリック(`History::jump_to` に渡す undo スタック長)。
    pub history_jump: Option<usize>,
    pub page_switch: Option<usize>,
    pub page_errors: Vec<String>,
}

/// 1 フレームの描画中に溜まる副作用(引数の数を抑えるための束ね。
/// `show` が最後にまとめて処理する)。
#[derive(Default)]
struct FrameSink {
    out: PanelsOutput,
    /// 「▾」メニューによる配置変更。ドック一覧を反復し終えてから適用する
    /// (反復中に配置を書き換えない)。
    pending_move: Option<(PanelKind, PanelMove)>,
    /// 「描き終わってから配置が変わった」= もう 1 フレーム必要
    /// (`show` 末尾の `request_repaint` 参照)。フローティングの位置・寸法の
    /// 書き戻しは**含めない** — 毎フレーム起こりうるので常時再描画になる。
    placement_changed: bool,
}

/// 全パネル(左ドック → 右ドック → フローティング)を描画する。
///
/// `interactive` が `false`(= モーダル表示中。`app.rs` が
/// `self.modal.is_none()` を渡す)のときは、**パネルの配置に関わる操作を
/// すべて止める**: 進行中のドラッグを破棄し、ドロップ判定・ヘッダの
/// ドラッグ量・「▾」メニュー・右クリックメニュー・折りたたみを受け付けない。
/// ドロップ判定はポインタ座標の幾何比較と `pointer.any_released()` の直読み
/// で行うため、egui がモーダルのために入力を遮っていても素通りしてしまう
/// (= モーダルの裏でパネルが移動してしまう)、というレビュー指摘への対応。
/// 中身(色/レイヤー/履歴)は従来どおり描くが、モーダル自身が egui の入力
/// ブロックを張るので操作はできない。
pub fn show(
    ui: &mut egui::Ui,
    layout: &mut PanelLayout,
    ctx: &mut PanelsCtx<'_>,
    interactive: bool,
) -> PanelsOutput {
    let mut sink = FrameSink::default();
    // ヘッダをドラッグ中か(ドロップ先候補の表示・空ドックの一時表示に使う)。
    let dragging = if interactive {
        egui::DragAndDrop::has_payload_of_type::<PanelKind>(ui.ctx())
    } else {
        // 掴んだままモーダルが開いた場合、そのドラッグは無かったことにする
        // (離した瞬間に裏で確定しないように、ペイロード自体を捨てる)。
        egui::DragAndDrop::clear_payload(ui.ctx());
        false
    };

    for side in DockSide::DECLARATION_ORDER {
        // SPEC §58: 空のドックは出さない(キャンバス最大化)。ただしドラッグ
        // 中だけは、戻す先が無くならないように空のドロップ領域を出す。
        if layout.is_dock_empty(side) && !dragging {
            continue;
        }
        show_dock(ui, side, layout, ctx, &mut sink, dragging, interactive);
    }

    for kind in layout.floating() {
        show_floating(ui, kind, layout, ctx, &mut sink, interactive);
    }

    if dragging && float_on_drop_outside_docks(ui, layout) {
        sink.placement_changed = true;
    }
    if let Some((kind, mv)) = sink.pending_move {
        layout.apply_move(kind, mv);
        sink.placement_changed = true;
    }
    // ドロップ確定・メニューでの移動は「そのフレームのパネルを描き終えた後」に
    // 反映されるため、もう 1 フレーム描かないと画面に出ない。マウスを離した
    // 直後は入力イベントが尽きて再描画が起きないことがあるので、**配置が実際に
    // 変わったフレームだけ** 1 回予約する(CLAUDE.md が禁じているのは
    // 「無条件の `request_repaint()`」= 毎フレーム予約。アイドル時にここへ
    // 到達することはないので、CPU 0% 要件は変わらない)。
    if sink.placement_changed {
        ui.ctx().request_repaint();
    }
    sink.out
}

/// 片側のドック(`egui::Panel`)を描画する。
fn show_dock(
    ui: &mut egui::Ui,
    side: DockSide,
    layout: &mut PanelLayout,
    ctx: &mut PanelsCtx<'_>,
    sink: &mut FrameSink,
    dragging: bool,
    interactive: bool,
) {
    let panel = match side {
        DockSide::Left => egui::Panel::left("darask_dock_left"),
        DockSide::Right => egui::Panel::right("darask_dock_right"),
    };
    panel
        .resizable(false)
        .exact_size(panels::DOCK_WIDTH)
        .frame(egui::Frame::NONE.fill(theme::BG_PANEL))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let edge = match side {
                DockSide::Left => full.right() - 0.5,
                DockSide::Right => full.left() + 0.5,
            };
            ui.painter()
                .vline(edge, full.y_range(), egui::Stroke::new(1.0, theme::LINE));
            // SPEC §50.1 / §60: スクロールは基本的にドック全体の 1 本。ただし
            // 展開中のレイヤーパネルには「ドックの残りの高さ」を割り当て、一覧
            // だけをその中でスクロールさせる(操作ボタンが常に見える)。他の
            // パネルの高さは前フレームの実測を使う(変わったフレームだけ再描画を
            // 1 回予約するので、数フレームで収束する)。
            let dock_h = ui.available_height();
            let heights_id = egui::Id::new(("darask_dock_block_heights", dock_tag(side)));
            let last_heights: Vec<(PanelKind, f32)> = ui
                .ctx()
                .data(|d| d.get_temp(heights_id))
                .unwrap_or_default();
            let mut heights: Vec<(PanelKind, f32)> = Vec::new();
            let docked = layout.docked(side);
            let mut blocks: Vec<egui::Rect> = Vec::new();
            egui::ScrollArea::vertical()
                .id_salt(dock_tag(side))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for &kind in &docked {
                        let extra = header_extra(kind, ctx);
                        let fill = (kind == PanelKind::Layers && !layout.collapsed(kind))
                            .then(|| layers_fill_height(dock_h, kind, &docked, &last_heights));
                        let block = ui
                            .scope(|ui| {
                                let header =
                                    panel_header(ui, kind, layout, false, interactive, &extra);
                                if header.toggle_collapse {
                                    layout.toggle_collapsed(kind);
                                }
                                if let Some(mv) = header.mv {
                                    sink.pending_move = Some((kind, mv));
                                }
                                if header.add_layer {
                                    sink.out.layer_action = Some(LayersPanelAction::Add);
                                }
                                if !layout.collapsed(kind) {
                                    panel_body_framed(ui, kind, ctx, &mut sink.out, fill);
                                }
                            })
                            .response
                            .rect;
                        heights.push((kind, block.height()));
                        ui.painter().hline(
                            block.x_range(),
                            block.bottom() - 0.5,
                            egui::Stroke::new(1.0, theme::LINE),
                        );
                        blocks.push(block);
                    }
                    if blocks.is_empty() {
                        ui.add_space(8.0);
                        ui.weak("ここへドロップ");
                    }
                });
            if dragging && handle_dock_drop(ui, side, layout, &blocks) {
                sink.placement_changed = true;
            }
            if !same_heights(&heights, &last_heights) {
                ui.ctx().data_mut(|d| d.insert_temp(heights_id, heights));
                ui.ctx().request_repaint();
            }
        });
}

/// 展開中のレイヤーパネル本体に割り当てる高さ(純関数)。ドックの高さから、
/// 他のパネルの前フレームの高さ(未計測なら見出しだけとみなす)と自分の見出し・
/// 下余白を引く。
fn layers_fill_height(
    dock_h: f32,
    layers: PanelKind,
    docked: &[PanelKind],
    last_heights: &[(PanelKind, f32)],
) -> f32 {
    let others: f32 = docked
        .iter()
        .filter(|kind| **kind != layers)
        .map(|kind| {
            last_heights
                .iter()
                .find(|(k, _)| k == kind)
                .map_or(HEADER_H, |(_, h)| *h)
        })
        .sum();
    (dock_h - others - HEADER_H - BODY_MARGIN_BOTTOM).max(MIN_LAYERS_BODY_H)
}

#[cfg(test)]
mod fill_tests {
    use super::*;

    #[test]
    fn layers_get_the_rest_of_the_dock_but_never_less_than_the_minimum() {
        let docked = [
            PanelKind::Color,
            PanelKind::Layers,
            PanelKind::History,
            PanelKind::Pages,
        ];
        let last = [
            (PanelKind::Color, 340.0),
            (PanelKind::Layers, 300.0),
            (PanelKind::History, HEADER_H),
            (PanelKind::Pages, HEADER_H),
        ];
        let fill = layers_fill_height(1000.0, PanelKind::Layers, &docked, &last);
        assert_eq!(
            fill,
            1000.0 - 340.0 - HEADER_H * 2.0 - HEADER_H - BODY_MARGIN_BOTTOM
        );
        assert_eq!(
            layers_fill_height(500.0, PanelKind::Layers, &docked, &last),
            MIN_LAYERS_BODY_H
        );
        // 未計測のパネルは見出しだけとみなす。
        let fill = layers_fill_height(1000.0, PanelKind::Layers, &docked, &[]);
        assert_eq!(fill, 1000.0 - HEADER_H * 4.0 - BODY_MARGIN_BOTTOM);
    }

    #[test]
    fn heights_compare_with_a_subpixel_tolerance() {
        let a = [(PanelKind::Color, 100.0), (PanelKind::Layers, 50.0)];
        let b = [(PanelKind::Color, 100.3), (PanelKind::Layers, 50.0)];
        let c = [(PanelKind::Color, 101.0), (PanelKind::Layers, 50.0)];
        assert!(same_heights(&a, &b));
        assert!(!same_heights(&a, &c));
        assert!(!same_heights(&a, &a[..1]));
    }
}

/// 前フレームと同じ並び・高さか(サブピクセルの揺れは無視する)。
fn same_heights(a: &[(PanelKind, f32)], b: &[(PanelKind, f32)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((ka, ha), (kb, hb))| ka == kb && (ha - hb).abs() < 0.5)
}

/// ドックの上でのドロップ(並べ替え/他ドックからの移動)。挿入位置の
/// インジケータもここで描く(SPEC §58)。配置が変わったら `true`。
fn handle_dock_drop(
    ui: &mut egui::Ui,
    side: DockSide,
    layout: &mut PanelLayout,
    blocks: &[egui::Rect],
) -> bool {
    let area = ui.max_rect();
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return false;
    };
    // ドロップ先の判定は **幾何的な内包** で行う(`Response::contains_pointer`
    // ではない)。フローティングパネルをドックへ戻すとき、そのウィンドウ自身が
    // ポインタの下にありドックより上のレイヤーに居るため、当たり判定ベースだと
    // ドックが永久に「ポインタを含まない」ことになり再ドックできない。
    // 「ドックの矩形の上で離したか」だけを見ればこの問題は起きない。
    if !area.contains(pointer) {
        return false;
    }
    if egui::DragAndDrop::payload::<PanelKind>(ui.ctx()).is_none() {
        return false;
    }
    let slot = panels::insertion_slot(blocks, pointer.y);
    draw_insertion_indicator(ui, area, blocks, slot);
    if !ui.ctx().input(|i| i.pointer.any_released()) {
        return false;
    }
    match egui::DragAndDrop::take_payload::<PanelKind>(ui.ctx()) {
        Some(kind) => layout.dock_at_slot(*kind, side, slot),
        None => false,
    }
}

/// 挿入位置のインジケータ(SPEC §50.1 のレイヤー行 DnD と同じ意匠)。
fn draw_insertion_indicator(ui: &egui::Ui, area: egui::Rect, blocks: &[egui::Rect], slot: usize) {
    let y = match blocks.get(slot) {
        Some(rect) => rect.top(),
        None => match blocks.last() {
            Some(rect) => rect.bottom(),
            None => area.top() + 4.0,
        },
    };
    let color = ui.visuals().selection.bg_fill;
    ui.painter()
        .hline(area.left()..=area.right(), y, egui::Stroke::new(2.0, color));
}

/// フローティングパネル(`egui::Window`)を描画する。
///
/// 位置は `PanelLayout` が正(`current_pos` で毎フレーム与える)で、移動は
/// ヘッダのドラッグ量から自分で積む。リサイズは egui に任せ、確定した外形を
/// 書き戻す(折りたたみ中は本体が無いぶん縮むので寸法は書き戻さない)。
fn show_floating(
    ui: &mut egui::Ui,
    kind: PanelKind,
    layout: &mut PanelLayout,
    ctx: &mut PanelsCtx<'_>,
    sink: &mut FrameSink,
    interactive: bool,
) {
    let PanelPlacement::Floating { pos, size } = layout.placement(kind) else {
        return;
    };
    let collapsed = layout.collapsed(kind);
    // 折りたたみ状態を id に含めることで、egui が覚えている「ユーザーが
    // 決めた寸法」(`Resize` の状態)を展開時と折りたたみ時で分ける。
    // こうすると折りたたみ→展開で元の寸法に戻る(同じ id だと、折りたたみ
    // 中に縮んだ寸法が展開後も残るか、逆に空白の大箱になる)。
    let window_id = egui::Id::new(("darask_panel_window", kind.tag(), collapsed));
    let mut window = egui::Window::new(kind.title())
        .id(window_id)
        .title_bar(false)
        .movable(false)
        .constrain(true)
        .current_pos(pos);
    window = if collapsed {
        // 幅は保ったまま、高さはヘッダぶんだけに自動で縮める。
        window
            .resizable(false)
            .min_width(size.x)
            .max_width(size.x)
            .default_size(egui::vec2(size.x, 1.0))
    } else {
        window
            .resizable(true)
            .min_width(panels::MIN_FLOAT_SIZE.x)
            .min_height(panels::MIN_FLOAT_SIZE.y)
            .default_size(size)
            .vscroll(true)
    };

    let mut toggle_collapse = false;
    let mut mv = None;
    let mut drag_delta = egui::Vec2::ZERO;
    let extra = header_extra(kind, ctx);
    let mut add_layer = false;
    let response = window.show(ui.ctx(), |ui| {
        let header = panel_header(ui, kind, layout, true, interactive, &extra);
        toggle_collapse = header.toggle_collapse;
        mv = header.mv;
        drag_delta = header.drag_delta;
        add_layer = header.add_layer;
        if !collapsed {
            panel_body_framed(ui, kind, ctx, &mut sink.out, None);
        }
    });
    if add_layer {
        sink.out.layer_action = Some(LayersPanelAction::Add);
    }

    if let Some(response) = response {
        let rect = response.response.rect;
        let new_size = if collapsed { size } else { rect.size() };
        layout.set_floating_rect(kind, rect.min + drag_delta, new_size);
    }
    if toggle_collapse {
        layout.toggle_collapsed(kind);
    }
    if let Some(mv) = mv {
        sink.pending_move = Some((kind, mv));
    }
}

/// どのドックにも落ちなかったドロップ = フローティング化(SPEC §58)。
///
/// 既にフローティングのパネルは、ヘッダのドラッグで既に移動済みなので
/// 何もしない(ポインタ位置へ飛ばすと最後に一度ワープしてしまう)。
/// 配置が変わったら `true`。
fn float_on_drop_outside_docks(ui: &egui::Ui, layout: &mut PanelLayout) -> bool {
    if !ui.ctx().input(|i| i.pointer.any_released()) {
        return false;
    }
    let Some(kind) = egui::DragAndDrop::take_payload::<PanelKind>(ui.ctx()) else {
        return false;
    };
    let kind = *kind;
    if matches!(layout.placement(kind), PanelPlacement::Floating { .. }) {
        return false;
    }
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return false;
    };
    let size = panels::DEFAULT_FLOAT_SIZE;
    layout.float_at(
        kind,
        pointer - egui::vec2(size.x * 0.5, FLOAT_DROP_GRAB_Y),
        size,
    );
    true
}

/// パネル本体。中身は従来のセクション関数そのままで、配置には依存しない。
/// 将来パネルを増やすときはここに 1 行(`PanelKind` の追加と対で)足す。
fn panel_body(
    ui: &mut egui::Ui,
    kind: PanelKind,
    ctx: &mut PanelsCtx<'_>,
    out: &mut PanelsOutput,
    fill: Option<f32>,
) {
    match kind {
        PanelKind::Color => color_panel::show(ui, ctx.color.reborrow()),
        PanelKind::Layers => {
            let action = layers_panel::show(
                ui,
                LayersPanelCtx {
                    doc: ctx.doc,
                    rename: ctx.rename,
                    thumbnails: ctx.thumbnails,
                    fill_height: fill,
                },
            );
            if action.is_some() {
                out.layer_action = action;
            }
        }
        PanelKind::History => {
            let jump = history_panel::show(ui, ctx.history);
            if jump.is_some() {
                out.history_jump = jump;
            }
        }
        PanelKind::Pages => {
            let result = pages_panel::show(ui, ctx.pages.as_deref_mut(), ctx.page_thumbnails);
            if result.switch_to.is_some() {
                out.page_switch = result.switch_to;
            }
            out.page_errors.extend(result.errors);
        }
    }
}

/// パネル本体を左右の余白付きで描く(v13 §60)。
/// `fill` はレイヤーパネルに割り当てる本体の高さ(`layers_fill_height`)。
fn panel_body_framed(
    ui: &mut egui::Ui,
    kind: PanelKind,
    ctx: &mut PanelsCtx<'_>,
    out: &mut PanelsOutput,
    fill: Option<f32>,
) {
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: BODY_MARGIN_X,
            right: BODY_MARGIN_X,
            top: 0,
            bottom: BODY_MARGIN_BOTTOM as i8,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            panel_body(ui, kind, ctx, out, fill);
        });
}

/// 見出しに添える情報(個数バッジ・レイヤーの「追加」ボタン)。
struct HeaderExtra {
    badge: Option<String>,
    /// `Some(有効か)` のときだけ「+ 追加」を出す(レイヤーパネル)。
    add_layer: Option<bool>,
}

fn header_extra(kind: PanelKind, ctx: &PanelsCtx<'_>) -> HeaderExtra {
    match kind {
        PanelKind::Layers => HeaderExtra {
            badge: Some(ctx.doc.layers.len().to_string()),
            add_layer: Some(ctx.doc.layers.len() < crate::document::MAX_LAYERS),
        },
        PanelKind::History => HeaderExtra {
            badge: Some(ctx.history.undo_len().to_string()),
            add_layer: None,
        },
        PanelKind::Pages => HeaderExtra {
            badge: ctx
                .pages
                .as_ref()
                .map(|pages| pages.entries.len().to_string()),
            add_layer: None,
        },
        PanelKind::Color => HeaderExtra {
            badge: None,
            add_layer: None,
        },
    }
}

/// ヘッダ行の結果(呼び出し側が `PanelLayout` へ適用する)。
struct HeaderOutput {
    toggle_collapse: bool,
    mv: Option<PanelMove>,
    /// フローティング時のみ: このフレームのヘッダドラッグ量。
    drag_delta: egui::Vec2,
    /// レイヤーパネルの「+ 追加」が押された。
    add_layer: bool,
}

/// パネルのヘッダ(開閉矢印 + タイトル(ドラッグ源) + バッジ + [追加] + 「…」)。
///
/// **矢印・追加・「…」はドラッグ源の外に置く**(モジュールドキュメントコメントの
/// 落とし穴 2)。ドック中はドラッグ中の見た目(ゴースト)が欲しいので
/// `dnd_drag_source` を使い、フローティング中はウィンドウ自体が動くので
/// ゴーストを出さない `dnd_set_drag_payload` を使う。
///
/// `interactive == false`(モーダル表示中)のときは、どの部品も
/// `Sense::hover()` でしか登録せず、メニューも開かない = ヘッダからは何も
/// 起こらない(`show` のドキュメントコメント参照)。
fn panel_header(
    ui: &mut egui::Ui,
    kind: PanelKind,
    layout: &PanelLayout,
    floating: bool,
    interactive: bool,
    extra: &HeaderExtra,
) -> HeaderOutput {
    let collapsed = layout.collapsed(kind);
    let placement = layout.placement(kind);
    let mut out = HeaderOutput {
        toggle_collapse: false,
        mv: None,
        drag_delta: egui::Vec2::ZERO,
        add_layer: false,
    };
    // モーダル中は「押せない」= hover しか sense しない(id は変えないので
    // レイアウト・テストからの参照はそのまま)。
    let click_sense = if interactive {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let add_w = if extra.add_layer.is_some() { 58.0 } else { 0.0 };
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), HEADER_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_height(HEADER_H);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_space(6.0);
            // -- 開閉矢印(ドラッグ源の外) --------------------------------
            let (arrow_rect, _) =
                ui.allocate_exact_size(egui::Vec2::splat(HEADER_ARROW_SIZE), egui::Sense::hover());
            let arrow = ui.interact(
                arrow_rect,
                egui::Id::new(("darask_panel_arrow", kind.tag())),
                click_sense,
            );
            let arrow = if interactive {
                arrow.on_hover_text(if collapsed {
                    "展開する"
                } else {
                    "折りたたむ"
                })
            } else {
                arrow
            };
            if ui.is_rect_visible(arrow_rect) {
                let color = if arrow.hovered() {
                    egui::Color32::WHITE
                } else {
                    theme::TEXT_FAINT
                };
                icons::paint_disclosure_icon(ui.painter(), arrow_rect, color, !collapsed);
            }
            if arrow.clicked() {
                out.toggle_collapse = true;
            }

            // -- タイトル(ドラッグ源) -------------------------------------
            let title_width = (ui.available_width()
                - HEADER_MENU_SIZE
                - add_w
                - ui.spacing().item_spacing.x * 2.0
                - 4.0)
                .max(24.0);
            let drag_id = egui::Id::new(("darask_panel_header", kind.tag()));
            let badge = extra.badge.clone();
            // ドラッグ源にする/しないで 2 回書かないための共通の中身。
            let title_ui = |ui: &mut egui::Ui| {
                ui.scope(|ui| {
                    ui.set_min_width(title_width);
                    ui.set_max_width(title_width);
                    ui.set_min_height(HEADER_H - 6.0);
                    ui.horizontal_centered(|ui| {
                        ui.label(
                            egui::RichText::new(kind.title())
                                .strong()
                                .size(12.0)
                                .color(theme::TEXT),
                        );
                        if let Some(badge) = &badge {
                            widgets::count_badge(ui, badge);
                        }
                    });
                })
                .response
                .rect
            };
            if !interactive {
                // モーダル中: 見た目だけ(ドラッグ源にも押しボタンにも
                // しない。ドラッグ量も取らないのでウィンドウも動かない)。
                let rect = title_ui(ui);
                let _ = ui.interact(rect, drag_id, egui::Sense::hover());
            } else {
                let drag_response = if floating {
                    let rect = title_ui(ui);
                    let response = ui.interact(rect, drag_id, egui::Sense::click_and_drag());
                    response.dnd_set_drag_payload(kind);
                    out.drag_delta = response.drag_delta();
                    response
                } else {
                    ui.dnd_drag_source(drag_id, kind, |ui| {
                        title_ui(ui);
                    })
                    .response
                    // クリック(=右クリックメニュー)も受けられるようにする。
                    .interact(egui::Sense::click())
                };
                let drag_response = drag_response
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_hover_text("ドラッグで移動(ドックの外へ出すとフローティング)");
                drag_response.context_menu(|ui| placement_menu(ui, placement, &mut out.mv));
            }

            // -- 「+ 追加」(レイヤーパネルのみ。ドラッグ源の外) ------------
            if let Some(enabled) = extra.add_layer {
                if add_layer_button(ui, enabled && interactive) {
                    out.add_layer = true;
                }
            }

            // -- 「…」メニュー(ドラッグ源の外) ---------------------------
            let (menu_rect, _) =
                ui.allocate_exact_size(egui::Vec2::splat(HEADER_MENU_SIZE), egui::Sense::hover());
            let menu_button = ui.interact(
                menu_rect,
                egui::Id::new(("darask_panel_menu", kind.tag())),
                click_sense,
            );
            let menu_button = if interactive {
                menu_button.on_hover_text("パネルの配置")
            } else {
                menu_button
            };
            if ui.is_rect_visible(menu_rect) {
                let color = if menu_button.hovered() {
                    ui.painter().rect_filled(menu_rect, 5.0, theme::BG_HOVER);
                    egui::Color32::WHITE
                } else {
                    theme::TEXT_FAINT
                };
                icons::paint_more_icon(ui.painter(), menu_rect.shrink(5.0), color);
            }
            if interactive {
                egui::Popup::menu(&menu_button)
                    .show(|ui| placement_menu(ui, placement, &mut out.mv));
            }
        },
    );
    out
}

/// 見出しの「+ 追加」(アクセントの淡色ボタン)。押されたら `true`。
fn add_layer_button(ui: &mut egui::Ui, enabled: bool) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(52.0, 24.0), sense);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "新規レイヤー")
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, fg) = if !enabled {
            (theme::BG_WIDGET, theme::TEXT_DISABLED)
        } else if response.hovered() {
            (theme::ACCENT_LINE, egui::Color32::WHITE)
        } else {
            (theme::ACCENT_SOFT, theme::ACCENT)
        };
        painter.rect_filled(rect, 6.0, fill);
        icons::paint_plus_icon(
            painter,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 13.0, rect.center().y),
                egui::vec2(12.0, 12.0),
            ),
            fg,
        );
        painter.text(
            egui::pos2(rect.left() + 22.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "追加",
            egui::FontId::proportional(11.5),
            fg,
        );
    }
    let shortcut = crate::keymap::label_for(crate::keymap::Action::LayerAdd);
    response
        .on_hover_text(format!("新規レイヤー ({shortcut})"))
        .clicked()
}

/// 「右にドック / 左にドック / フローティング化」(SPEC §58)。
/// 現在の配置と同じ項目は選べない(無効表示)。
fn placement_menu(ui: &mut egui::Ui, placement: PanelPlacement, mv: &mut Option<PanelMove>) {
    let items = [
        (PanelMove::Dock(DockSide::Right), "右にドック"),
        (PanelMove::Dock(DockSide::Left), "左にドック"),
        (PanelMove::Float, "フローティング化"),
    ];
    for (target, label) in items {
        let already = match (target, placement) {
            (PanelMove::Dock(side), PanelPlacement::Dock { side: cur, .. }) => side == cur,
            (PanelMove::Float, PanelPlacement::Floating { .. }) => true,
            _ => false,
        };
        if ui.add_enabled(!already, egui::Button::new(label)).clicked() {
            *mv = Some(target);
            ui.close();
        }
    }
}

/// `egui::Id`・`ScrollArea` の salt に使う左右の識別子。
fn dock_tag(side: DockSide) -> &'static str {
    match side {
        DockSide::Left => "darask_dock_left",
        DockSide::Right => "darask_dock_right",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;
    use crate::ui::color_wheel::ColorWheelState;
    use eframe::egui::Color32;
    use std::collections::VecDeque;

    const SCREEN: egui::Vec2 = egui::vec2(1000.0, 700.0);

    /// `PanelsCtx` に必要な状態一式(`app.rs` の該当フィールド相当)。
    struct Harness {
        doc: Document,
        rename: RenameState,
        thumbnails: ThumbnailCache,
        history: History,
        page_thumbnails: PageThumbnailCache,
        primary: Color32,
        secondary: Color32,
        wheel: ColorWheelState,
        hex: String,
        recent: VecDeque<Color32>,
        palette: Vec<Color32>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                doc: Document::new(8, 8, Background::White),
                rename: None,
                thumbnails: ThumbnailCache::default(),
                history: History::new(),
                page_thumbnails: PageThumbnailCache::default(),
                primary: Color32::BLACK,
                secondary: Color32::WHITE,
                wheel: ColorWheelState::new(),
                hex: "#000000".to_owned(),
                recent: VecDeque::new(),
                palette: Vec::new(),
            }
        }

        fn ctx(&mut self) -> PanelsCtx<'_> {
            PanelsCtx {
                doc: &self.doc,
                rename: &mut self.rename,
                thumbnails: &mut self.thumbnails,
                history: &self.history,
                pages: None,
                page_thumbnails: &mut self.page_thumbnails,
                color: ColorPanelCtx {
                    primary: &mut self.primary,
                    secondary: &mut self.secondary,
                    wheel: &mut self.wheel,
                    hex_buffer: &mut self.hex,
                    recent_colors: &self.recent,
                    user_palette: &mut self.palette,
                },
            }
        }
    }

    /// 1 フレーム描く。`app.rs` と同じく `Context::run_ui` のルート `Ui` に
    /// 直接パネルを宣言する(`layers_panel` のテストと同じ手法)。戻り値は
    /// 中央領域(キャンバスが使える残り)の矩形。
    fn frame(
        ctx: &egui::Context,
        layout: &mut PanelLayout,
        state: &mut Harness,
        events: Vec<egui::Event>,
        pointer: Option<egui::Pos2>,
    ) -> egui::Rect {
        frame_with(ctx, layout, state, events, pointer, true)
    }

    /// `frame` の `interactive` 指定版(モーダル表示中 = `false`)。
    fn frame_with(
        ctx: &egui::Context,
        layout: &mut PanelLayout,
        state: &mut Harness,
        events: Vec<egui::Event>,
        pointer: Option<egui::Pos2>,
        interactive: bool,
    ) -> egui::Rect {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        if let Some(pos) = pointer {
            input.events.insert(0, egui::Event::PointerMoved(pos));
        }
        let mut central = egui::Rect::NOTHING;
        let _ = ctx.run_ui(input, |ui| {
            let _ = show(ui, layout, &mut state.ctx(), interactive);
            central = ui.available_rect_before_wrap();
        });
        central
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn rect_of(ctx: &egui::Context, id: (&str, &str)) -> egui::Rect {
        ctx.read_response(egui::Id::new(id))
            .map(|r| r.rect)
            .unwrap_or_else(|| panic!("{id:?} must have been laid out"))
    }

    /// 押下 → 移動 → 離す、の 3 フレーム(+当たり判定用に 1 フレーム前から
    /// ポインタを載せておく。egui は前フレームの widget 矩形で当たりを取る)。
    fn drag(
        ctx: &egui::Context,
        layout: &mut PanelLayout,
        state: &mut Harness,
        from: egui::Pos2,
        to: egui::Pos2,
    ) {
        frame(ctx, layout, state, Vec::new(), Some(from));
        frame(ctx, layout, state, vec![press(from, true)], Some(from));
        frame(ctx, layout, state, Vec::new(), Some(to));
        frame(ctx, layout, state, vec![press(to, false)], Some(to));
    }

    #[test]
    fn default_layout_renders_three_panels_in_the_right_dock_only() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let mut state = Harness::new();
        let central = frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        for kind in PanelKind::ALL {
            let header = rect_of(&ctx, ("darask_panel_header", kind.tag()));
            assert!(
                header.center().x > SCREEN.x - panels::DOCK_WIDTH,
                "{} のヘッダは右ドックの中にあるはず",
                kind.title()
            );
        }
        // SPEC §58: 左ドックは空なので出さない = 中央領域は左端から始まる。
        assert_eq!(central.left(), 0.0);
        assert!(
            central.right() <= SCREEN.x - panels::DOCK_WIDTH + 0.5,
            "central: {central:?}"
        );
    }

    /// SPEC §58: 「右ドックが空なら右パネル自体を出さない(キャンバス最大化)」。
    #[test]
    fn an_empty_dock_is_not_rendered_at_all() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        for kind in PanelKind::ALL {
            layout.apply_move(kind, PanelMove::Float);
        }
        let mut state = Harness::new();
        let central = frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        assert_eq!(
            central.width(),
            SCREEN.x,
            "全パネルがフローティングならドックは 1 つも出さない"
        );
    }

    /// v12 Phase 1 で踏んだ落とし穴の回帰テスト: `dnd_drag_source` の中に
    /// クリック widget を入れると押せなくなる。折りたたみ矢印はドラッグ源の
    /// **外**にあるので、ヘッダがドラッグ可能でもクリックが通ること。
    #[test]
    fn the_collapse_arrow_inside_a_draggable_header_is_still_clickable() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let mut state = Harness::new();

        frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        let pos = rect_of(&ctx, ("darask_panel_arrow", "color")).center();
        frame(&ctx, &mut layout, &mut state, Vec::new(), Some(pos));
        frame(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(pos, true)],
            Some(pos),
        );
        frame(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(pos, false)],
            Some(pos),
        );

        assert!(
            layout.collapsed(PanelKind::Color),
            "ヘッダの矢印クリックで折りたたまれること"
        );
    }

    /// SPEC §58: ヘッダの「▾」ボタンからも配置を変えられる。ボタン自体が
    /// ドラッグ源の外にあってクリックできること(落とし穴 2 の回帰)と、
    /// クリックでメニューが開くことを確認する(選んだ項目の意味論は
    /// `panels::PanelLayout::apply_move` のユニットテスト側で検証済み)。
    #[test]
    fn the_header_menu_button_opens_the_placement_menu() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let mut state = Harness::new();

        frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        assert!(!egui::Popup::is_any_open(&ctx));
        let pos = rect_of(&ctx, ("darask_panel_menu", "color")).center();
        frame(&ctx, &mut layout, &mut state, Vec::new(), Some(pos));
        frame(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(pos, true)],
            Some(pos),
        );
        frame(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(pos, false)],
            Some(pos),
        );

        assert!(
            egui::Popup::is_any_open(&ctx),
            "「▾」のクリックで配置メニューが開くこと"
        );
    }

    /// SPEC §58: ドック内のドラッグは並べ替え。
    #[test]
    fn dragging_a_header_above_the_first_panel_reorders_the_dock() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        // 中身の高さに依存しないよう、全部たたんでヘッダだけにしておく。
        for kind in PanelKind::ALL {
            layout.toggle_collapsed(kind);
        }
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        let color_header = rect_of(&ctx, ("darask_panel_header", "color"));
        // 先頭パネルの塊の上半分 = 挿入スロット 0(ドックの矩形の内側)。
        let to = egui::pos2(from.x, color_header.top() + 1.0);
        drag(&ctx, &mut layout, &mut state, from, to);

        assert_eq!(
            layout.docked(DockSide::Right),
            vec![
                PanelKind::History,
                PanelKind::Color,
                PanelKind::Layers,
                PanelKind::Pages
            ]
        );
    }

    /// SPEC §58: ドック領域の外へドロップするとフローティング化。
    #[test]
    fn dragging_a_header_out_of_the_dock_floats_the_panel() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let mut state = Harness::new();
        // v13 §60: レイヤーの高さ割り当ては前フレームの実測を使うので、
        // 2 フレーム目で配置が確定する(アプリは 1 回だけ再描画を予約する)。
        // 確定した配置の当たり判定を読むため、もう 1 フレーム描いておく。
        for _ in 0..3 {
            frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        }

        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        // 中央(キャンバス)へドロップ。
        let to = egui::pos2(300.0, 350.0);
        drag(&ctx, &mut layout, &mut state, from, to);

        match layout.placement(PanelKind::History) {
            PanelPlacement::Floating { pos, .. } => {
                assert!(pos.x < to.x && pos.y < to.y, "掴んだ位置の近くに置かれる");
            }
            other => panic!("フローティングになるはず: {other:?}"),
        }
        // 残りは右ドックに詰められる。
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::Color, PanelKind::Layers, PanelKind::Pages]
        );
    }

    /// SPEC §58: フローティングのヘッダをドック領域へドラッグすると再ドック。
    #[test]
    fn dragging_a_floating_header_onto_a_dock_redocks_it() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        layout.float_at(
            PanelKind::History,
            egui::pos2(300.0, 300.0),
            panels::DEFAULT_FLOAT_SIZE,
        );
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        // 右ドックの一番上(色パネルの塊の上半分)へ。
        let color_header = rect_of(&ctx, ("darask_panel_header", "color"));
        let to = egui::pos2(color_header.center().x, color_header.top() + 1.0);
        drag(&ctx, &mut layout, &mut state, from, to);

        assert_eq!(
            layout.docked(DockSide::Right),
            vec![
                PanelKind::History,
                PanelKind::Color,
                PanelKind::Layers,
                PanelKind::Pages
            ],
            "フローティングを右ドックの先頭へ戻せること"
        );
    }

    /// gpt-5.6-sol レビュー①の回帰テスト: ヘッダを掴んだままモーダルが
    /// 開いたら、そのドラッグは無効になる。ドロップ判定はポインタ座標の
    /// 幾何比較+`pointer.any_released()` の直読みなので、`interactive` の
    /// 遮断が無いと**モーダルの裏で**再ドック/フローティング化が確定して
    /// しまう(ドック内で離した場合・キャンバス上で離した場合の両方)。
    #[test]
    fn a_modal_blocks_panel_drag_and_drop_entirely() {
        for release_inside_dock in [true, false] {
            let ctx = egui::Context::default();
            let mut layout = PanelLayout::default();
            let mut state = Harness::new();
            frame(&ctx, &mut layout, &mut state, Vec::new(), None);

            let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
            let color_header = rect_of(&ctx, ("darask_panel_header", "color"));
            let to = if release_inside_dock {
                // 右ドックの先頭(通常なら並べ替えが起きる位置)。
                egui::pos2(color_header.center().x, color_header.top() + 1.0)
            } else {
                // キャンバス上(通常ならフローティング化する位置)。
                egui::pos2(300.0, 350.0)
            };

            // 通常状態でドラッグ開始(ここまではペイロードが載る)。
            frame(&ctx, &mut layout, &mut state, Vec::new(), Some(from));
            frame(
                &ctx,
                &mut layout,
                &mut state,
                vec![press(from, true)],
                Some(from),
            );
            frame(&ctx, &mut layout, &mut state, Vec::new(), Some(to));
            // ここでモーダルが開いた(= 以降 interactive=false)。
            frame_with(&ctx, &mut layout, &mut state, Vec::new(), Some(to), false);
            frame_with(
                &ctx,
                &mut layout,
                &mut state,
                vec![press(to, false)],
                Some(to),
                false,
            );

            assert_eq!(
                layout,
                PanelLayout::default(),
                "モーダル表示中に離しても配置は変わらない(dock内={release_inside_dock})"
            );
        }
    }

    /// 同上のフローティング版: モーダル中はヘッダのドラッグ量を取らないので
    /// ウィンドウ座標も動かない。
    #[test]
    fn a_modal_freezes_a_floating_panel_position() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let start = egui::pos2(300.0, 300.0);
        layout.float_at(PanelKind::History, start, panels::DEFAULT_FLOAT_SIZE);
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        let before = layout.placement(PanelKind::History);

        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        let to = from + egui::vec2(60.0, 40.0);
        frame_with(&ctx, &mut layout, &mut state, Vec::new(), Some(from), false);
        frame_with(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(from, true)],
            Some(from),
            false,
        );
        frame_with(&ctx, &mut layout, &mut state, Vec::new(), Some(to), false);
        frame_with(
            &ctx,
            &mut layout,
            &mut state,
            vec![press(to, false)],
            Some(to),
            false,
        );

        assert_eq!(
            layout.placement(PanelKind::History),
            before,
            "モーダル表示中はフローティングも動かない"
        );
    }

    /// gpt-5.6-sol レビュー③: 操作が終わったら再描画要求が止まること
    /// (アイドル CPU 0% 要件、CLAUDE.md)。移動・リサイズの確定後に無入力の
    /// フレームを重ねても `request_repaint` が呼ばれ続けない。
    #[test]
    fn idle_frames_after_a_float_drag_stop_requesting_repaint() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        layout.float_at(
            PanelKind::History,
            egui::pos2(280.0, 260.0),
            panels::DEFAULT_FLOAT_SIZE,
        );
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        // 無入力フレームを重ね、再描画要求が止まるまでの様子を見る。egui 側の
        // ホバー/ツールチップのアニメーションは数フレームで収束するので、
        // そのあと**こちらから要求し続けていない**ことを確認する
        // (要求し続けるとアイドル CPU が 0% にならない)。60 フレームは
        // 60fps 換算で約 1 秒 = egui の既定アニメーション時間の数倍。
        let settles = |ctx: &egui::Context, layout: &mut PanelLayout, state: &mut Harness| {
            let mut quiet = 0;
            for _ in 0..60 {
                frame(ctx, layout, state, Vec::new(), None);
                if ctx.requested_repaint_last_pass() {
                    quiet = 0;
                } else {
                    quiet += 1;
                    if quiet >= 5 {
                        return true;
                    }
                }
            }
            false
        };

        // ① 移動(ヘッダのドラッグ)を確定させたあと。
        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        drag(
            &ctx,
            &mut layout,
            &mut state,
            from,
            from + egui::vec2(35.0, 25.0),
        );
        assert!(
            settles(&ctx, &mut layout, &mut state),
            "移動の確定後、無入力なのに再描画要求が止まらない"
        );

        // ② 寸法が変わったあと(リサイズ確定相当)。egui が覚えている寸法と
        // `PanelLayout` の寸法が食い違っても、書き戻しが振動して毎フレーム
        // 再描画を要求し続けたりしないこと。
        let PanelPlacement::Floating { pos, size } = layout.placement(PanelKind::History) else {
            panic!("フローティングのはず");
        };
        layout.set_floating_rect(PanelKind::History, pos, size + egui::vec2(40.0, 30.0));
        assert!(
            settles(&ctx, &mut layout, &mut state),
            "寸法変更後、無入力なのに再描画要求が止まらない"
        );
    }

    /// フローティングのヘッダをドックの外でドラッグしたときは、ウィンドウが
    /// 動くだけ(ポインタ位置へワープしない)。
    #[test]
    fn dragging_a_floating_header_moves_the_window_by_the_drag_delta() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        let start = egui::pos2(300.0, 300.0);
        layout.float_at(PanelKind::History, start, panels::DEFAULT_FLOAT_SIZE);
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        let from = rect_of(&ctx, ("darask_panel_header", "history")).center();
        let to = from + egui::vec2(40.0, 30.0);
        drag(&ctx, &mut layout, &mut state, from, to);

        match layout.placement(PanelKind::History) {
            PanelPlacement::Floating { pos, .. } => {
                assert!(
                    (pos - start - egui::vec2(40.0, 30.0)).length() < 4.0,
                    "ドラッグ量ぶんだけ動く(実際: {pos:?})"
                );
            }
            other => panic!("フローティングのままのはず: {other:?}"),
        }
    }

    /// 折りたたみ状態はフローティングでも維持される(SPEC §58)。
    #[test]
    fn a_collapsed_floating_panel_renders_only_its_header() {
        let ctx = egui::Context::default();
        let mut layout = PanelLayout::default();
        layout.float_at(
            PanelKind::Layers,
            egui::pos2(200.0, 200.0),
            panels::DEFAULT_FLOAT_SIZE,
        );
        layout.toggle_collapsed(PanelKind::Layers);
        let mut state = Harness::new();
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);
        frame(&ctx, &mut layout, &mut state, Vec::new(), None);

        let header = rect_of(&ctx, ("darask_panel_header", "layers"));
        assert!(
            header.height() < 40.0,
            "折りたたみ中はヘッダだけ(実際の高さ {})",
            header.height()
        );
        // レイヤー一覧(本体)は描かれない。
        assert!(
            ctx.read_response(egui::Id::new(("darask_layer_eye", 0)))
                .is_none(),
            "折りたたみ中は本体を描かない"
        );
        assert!(layout.collapsed(PanelKind::Layers));
    }
}
