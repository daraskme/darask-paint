//! ツール基盤(ARCHITECTURE.md §4)。
//!
//! `Tool` トレイトと `ToolEvent`/`ToolCtx` を定義する。`canvas_view.rs` が
//! ポインタ入力を画像座標の `ToolEvent` に変換し、`app.rs` が現在選択中の
//! `ToolKind` に応じて対応するツールへディスパッチする。
//!
//! M2 で実際にドキュメントへ描画するのは `pen`(ペン)と `eraser`(消しゴム)
//! のみだった。M3 で `shapes`(直線・矩形・楕円)・`fill`(塗りつぶし)・
//! `picker`(スポイト)を追加した。「手のひら」と「選択」は
//! ツールとしての実体を持たず、`canvas_view`/`app.rs` が別枠で扱う
//! (ARCHITECTURE.md §4)。選択のデータ構造(`Selection`/`Floating`)と
//! 純粋な計算部分は M4 で `select` モジュールに追加した。
//!
//! スポイトの実際の色サンプリングは `app.rs::sample_eyedropper_color` に
//! 集約している(Alt+クリックの一時スポイトと同じ経路)。`ToolCtx` は
//! 色を読み取り専用の値として持つのみで書き込み手段を持たないため、
//! 「色を変える」操作は `Tool::event` の外側(app.rs のディスパッチ層)で
//! 行うという設計になっている。

use eframe::egui::{self, Color32, Modifiers, PointerButton, Pos2};

use crate::canvas_view::CanvasView;
use crate::document::Document;
use crate::history::History;

pub mod eraser;
pub mod fill;
pub mod pen;
pub mod picker;
pub mod select;
pub mod shapes;

/// SPEC §4 のツールバー一覧に対応する種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Pen,
    Eraser,
    Line,
    Rect,
    Ellipse,
    Fill,
    Picker,
    Select,
    /// 手のひら(H)。`Tool` の実体は持たず、canvas_view がパン操作として
    /// 横取りする際の判定にのみ使う(ARCHITECTURE.md §4)。
    Pan,
}

/// キャンバス上のポインタ入力(画像ピクセル座標系、ARCHITECTURE.md §4)。
#[derive(Debug, Clone, Copy)]
pub enum ToolEvent {
    Down {
        img: Pos2,
        button: PointerButton,
        mods: Modifiers,
    },
    Drag {
        img: Pos2,
        button: PointerButton,
        /// M3 の図形ツール(Shift で 45°/正方形/正円に拘束、SPEC §4)が使う。
        /// ペン/消しゴムは参照しない。
        #[allow(dead_code)]
        mods: Modifiers,
    },
    Up {
        /// 図形ツール(ドラッグ確定位置)・選択ツール(浮動片の最終位置、M4)が
        /// 使う。ペン/消しゴムはストローク中の位置を自前で追跡しているため
        /// 参照しない。
        img: Pos2,
        button: PointerButton,
    },
    Hover {
        /// 将来のツールが使う。ペン/消しゴム/選択は no-op。
        #[allow(dead_code)]
        img: Pos2,
    },
}

/// ツールがドキュメントを操作するための一式(ARCHITECTURE.md §4)。
pub struct ToolCtx<'a> {
    pub doc: &'a mut Document,
    pub history: &'a mut History,
    pub primary: Color32,
    pub secondary: Color32,
    pub brush_size: f32,
    /// この呼び出しで実際に描画確定に使われた色(SPEC §5:
    /// 「描画確定時に使用色を先頭に追加」)。ツール側は確定時にここへ push する
    /// だけでよく、「最近使った色」リストへの反映(重複の先頭移動・上限8件)は
    /// 呼び出し側(app.rs)が行う。矩形/楕円の「両方」モードのように 1 回の
    /// 確定で 2 色使うツールは複数 push してよい(最後に push した色が
    /// 最終的にリストの先頭に来る)。
    pub used_colors: &'a mut Vec<Color32>,
}

pub trait Tool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx);

    /// 進行中のジェスチャ(ドラッグ)があれば、`ToolEvent::Up` が来た場合と
    /// 同様に確定して終了する。ツール切替時に呼ばれる(`app.rs::set_tool`)。
    ///
    /// M4 で発見・修正したバグ: 以前は `set_tool` が選択ツールの浮動片以外
    /// 何も確定させなかったため、ペンでドラッグ中にツールショートカット
    /// キー(B/E/L/…)を押すと、進行中の `History` ストロークが次のツールの
    /// `begin_stroke` に無警告で置き換えられて消え、既に描画済みのピクセル
    /// が undo 履歴に一切残らない不具合があった(AA ペンではさらにカバレッジ
    /// マスクも残留し、次のストロークの見た目が乱れた)。既定は no-op
    /// (ドラッグ状態を持たないツール向け)。
    fn cancel(&mut self, _ctx: &mut ToolCtx) {}

    /// ドキュメントに触れないプレビュー(直線/図形/選択のドラッグ中表示)。
    /// ペン/消しゴムのようにハードエッジで直接確定するツールは no-op でよい。
    /// `primary`/`secondary`/`brush_size` は現在の色・ブラシサイズ
    /// (プレビューの見た目に必要なため、M3 で `ToolCtx` から独立して渡す
    /// 形に拡張した)。
    fn draw_preview(
        &self,
        painter: &egui::Painter,
        view: &CanvasView,
        primary: Color32,
        secondary: Color32,
        brush_size: f32,
    );

    fn cursor(&self) -> egui::CursorIcon;
}

/// ブラシサイズ(直径、px)から半径を求める。1px ブラシでも何かしら塗れるよう
/// 最小半径を設ける。
pub fn brush_radius(brush_size: f32) -> f32 {
    (brush_size / 2.0).max(0.5)
}

/// egui の `Color32`(内部は premultiplied)から、ドキュメントが保持する
/// straight-alpha RGBA8 バイト列に変換する。
pub fn color_to_straight_rgba(color: Color32) -> [u8; 4] {
    color.to_srgba_unmultiplied()
}

/// ペン/消しゴムに共通するストローク処理(ARCHITECTURE.md §4:
/// 「ポインタイベントの間隔が開いても線が途切れないよう…前回位置と今回位置を
/// 線分で補間してスタンプする」)。`pen`/`eraser` の各 `Tool` 実装から使う。
pub(crate) struct StrokeTool {
    last: Option<(f32, f32)>,
    button: Option<PointerButton>,
}

impl StrokeTool {
    pub(crate) fn new() -> Self {
        Self {
            last: None,
            button: None,
        }
    }

    /// `erase == true` なら消しゴム(常に透明化・色は無視)、`false` なら
    /// ペン(左ドラッグ=プライマリ色、右ドラッグ=セカンダリ色、SPEC §4)。
    /// ストロークが実際にこの呼び出しで確定した(= Up で commit した)場合、
    /// そのボタンを `Some` で返す(呼び出し側が「最近使った色」を記録するのに
    /// 使う。消しゴムには色の概念がないため呼び出し側は無視してよい)。
    pub(crate) fn handle(
        &mut self,
        ev: ToolEvent,
        ctx: &mut ToolCtx,
        erase: bool,
    ) -> Option<PointerButton> {
        match ev {
            ToolEvent::Down { img, button, .. } => {
                if !matches!(button, PointerButton::Primary | PointerButton::Secondary) {
                    return None;
                }
                let color = color_bytes_for(ctx, button, erase);
                let radius = brush_radius(ctx.brush_size);
                let bounds = crate::raster::stamp_bounds(img.x, img.y, radius);
                ctx.history.begin_stroke(ctx.doc.active);
                ctx.history.ensure_tiles_saved(ctx.doc, bounds);
                let touched = {
                    let mut surface = ctx.doc.active_surface_mut();
                    crate::raster::stamp_round(&mut surface, img.x, img.y, radius, color, erase)
                };
                ctx.doc.mark_dirty(touched);
                self.last = Some((img.x, img.y));
                self.button = Some(button);
            }
            ToolEvent::Drag { img, button, .. } => {
                if self.button != Some(button) {
                    return None;
                }
                let Some(last) = self.last else {
                    return None;
                };
                let color = color_bytes_for(ctx, button, erase);
                let radius = brush_radius(ctx.brush_size);
                let to = (img.x, img.y);
                let bounds = crate::raster::segment_bounds(last, to, radius);
                ctx.history.ensure_tiles_saved(ctx.doc, bounds);
                let touched = {
                    let mut surface = ctx.doc.active_surface_mut();
                    crate::raster::stroke_segment(&mut surface, last, to, radius, color, erase)
                };
                ctx.doc.mark_dirty(touched);
                self.last = Some(to);
            }
            ToolEvent::Up { button, .. } => {
                if self.button == Some(button) {
                    ctx.history.commit_stroke(ctx.doc);
                    self.last = None;
                    self.button = None;
                    return Some(button);
                }
            }
            ToolEvent::Hover { .. } => {}
        }
        None
    }

    /// `Tool::cancel` の共通実装。進行中のストロークがあれば、`Up` が来た
    /// 場合と同じく `History::commit_stroke` で確定する(ARCHITECTURE.md §6
    /// の「1 ストローク = 1 undo 単位」を、ツール切替という中断経路でも
    /// 守るため)。確定した場合はそのボタンを返す(呼び出し側が「最近使った
    /// 色」に記録できるよう、`handle` の `Up` と同じ規約にする)。
    pub(crate) fn cancel(&mut self, ctx: &mut ToolCtx) -> Option<PointerButton> {
        let button = self.button.take();
        if button.is_some() {
            ctx.history.commit_stroke(ctx.doc);
            self.last = None;
        }
        button
    }
}

pub(crate) fn color_bytes_for(ctx: &ToolCtx, button: PointerButton, erase: bool) -> [u8; 4] {
    if erase {
        // erase 時は stamp_round が alpha=0 を書くため色自体は使われないが、
        // シグネチャを揃えるためダミー値を返す。
        return [0, 0, 0, 0];
    }
    let color = if button == PointerButton::Secondary {
        ctx.secondary
    } else {
        ctx.primary
    };
    color_to_straight_rgba(color)
}
