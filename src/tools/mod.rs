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
//! v3(SPEC §17, ARCHITECTURE.md §15.1)で `pen`/`eraser` は共通の
//! ストロークエンジン(硬さ・不透明度・鉛筆モード・Shift+クリック連結)を
//! 新設 `brush` モジュールへ切り出し、それぞれ薄いラッパーになった。
//!
//! スポイトの実際の色サンプリングは `app.rs::sample_eyedropper_color` に
//! 集約している(Alt+クリックの一時スポイトと同じ経路)。`ToolCtx` は
//! 色を読み取り専用の値として持つのみで書き込み手段を持たないため、
//! 「色を変える」操作は `Tool::event` の外側(app.rs のディスパッチ層)で
//! 行うという設計になっている。
//!
//! v3 V3-M2(SPEC §18, ARCHITECTURE.md §15.2)で `Move`(移動)/`Zoom`
//! (ズーム)を追加した。どちらも選択・手のひら・スポイトと同様 `Tool` の
//! 実体を持たず、app.rs が直接処理する: 移動は既存の `Selection`/`Floating`
//! 浮動化パスを「選択があればそれを、無ければアクティブレイヤー全体を」
//! 対象にして再利用し、ズームは `CanvasView` の既存ズーム関数をクリック
//! 位置アンカーで呼ぶだけ。

use eframe::egui::{self, Color32, Modifiers, PointerButton, Pos2};

use crate::canvas_view::CanvasView;
use crate::document::Document;
use crate::history::History;

pub mod brush;
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
    /// v3 §18: 移動(V)。選択と同様 `Tool` の実体は持たず、`Selection`/
    /// `Floating`(既存の浮動化パス)を app.rs が直接操作する
    /// (ARCHITECTURE.md §15.2)。
    Move,
    /// v3 §18: ズーム(Z)。`Tool` の実体は持たず、クリック位置を中心に
    /// `CanvasView` の既存ズーム関数を app.rs から直接呼ぶ
    /// (ARCHITECTURE.md §15.2)。
    Zoom,
    /// v3 §19: テキスト(T)。`Tool` の実体は持たず、インライン編集状態
    /// (`app.rs::TextEditState`)と確定後のラスタライズ・浮動片配置を
    /// app.rs が直接扱う(ARCHITECTURE.md §15.3)。選択・移動と同様、
    /// 確定後は既存の `Floating` 機構(移動・ハンドル拡縮・Enter確定・
    /// Esc破棄)にそのまま乗る。
    Text,
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
    /// SPEC §17: ブラシ/消しゴム共通の硬さ。0.0–1.0(UI は 0–100%)。
    /// 鉛筆モード中は無視される。`tools/brush.rs` のみが参照する。
    pub hardness: f32,
    /// SPEC §17: ブラシ/消しゴム共通の不透明度。0.0–1.0(UI は 1–100%。
    /// 消しゴムでは「強さ」として表示)。`tools/brush.rs` のみが参照する。
    pub opacity: f32,
    /// SPEC §17: 鉛筆モード(デフォルト OFF)。ON の間はアンチエイリアス
    /// なしの2値スタンプになり、硬さは無視される。`tools/brush.rs` のみが
    /// 参照する。
    pub pencil: bool,
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

/// ボタンから描画色を決める(左ドラッグ=プライマリ色、右ドラッグ=
/// セカンダリ色、SPEC §4)。`erase == true`(消しゴム)なら色自体は使われない
/// ためダミー値を返す(呼び出し側のシグネチャを揃えるため)。`tools/brush.rs`
/// (ブラシ/消しゴム共通エンジン)と `tools/shapes.rs` が共有する。
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
