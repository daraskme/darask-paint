//! `DaraskApp`: `eframe::App` 実装。全状態の所有、レイアウト、
//! ベンチマークモードの処理(ARCHITECTURE.md §10)。
//!
//! M1(骨組みとシェル)で実装したもの:
//! - フォント設定(§9、日本語 tofu 対策)
//! - 起動時の新規ドキュメント作成(SPEC §3)
//! - メニュー/ツールバー/オプションバー/ステータスバーのレイアウト
//! - DARASK_BENCH ベンチマークモード(SPEC §11)
//!
//! M2(キャンバスと描画コア)で追加したもの:
//! - `CanvasView` によるキャンバス描画・ズーム/パン・ポインタディスパッチ
//! - ペン/消しゴムツール(ハードエッジ、右ドラッグ=セカンダリ色)
//! - Alt+クリックの一時スポイト(SPEC §4)
//! - `History` によるアンドゥ/リドゥ(Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z)
//! - ツール切り替え(ツールバークリック、単一キーショートカット)
//!
//! M3(残りの描画ツールと色)で追加したもの:
//! - 直線/矩形/楕円ツール(ドラッグ→確定、Shift 拘束、モード切替)
//! - 塗りつぶし(flood fill)・スポイトツール
//! - 色 UI(スウォッチ+ピッカー、X 入替、最近使った色)・ブラシサイズ UI・`[`/`]`
//! - ペンのアンチエイリアスオプション
//!
//! M4(ファイル I/O・選択・仕上げ)で追加したもの:
//! - 開く/保存/名前を付けて保存/新規(ダイアログ)、JPEG 品質、CLI 引数、
//!   D&D、未保存ガード(`pending_action` + `ModalState::ConfirmUnsaved`)、
//!   タイトルバー表示。
//! - 選択ツール一式(ARCHITECTURE.md §7)+ クリップボード(コピー/切り取り/
//!   貼り付け、白紙時の置き換え貼り付け)。
//! - 画像メニュー(サイズ変更/キャンバスサイズ/トリミング/反転/回転、
//!   `HistoryOp::ReplaceAll` を使った undo)。
//! - 表示メニュー、ステータスバー実データ(選択サイズ・トースト)、
//!   全ショートカット総配線。
//!
//! v2(ARCHITECTURE.md §14.8 V2-M1)で `Document`/`raster`/`history`/`tools`/
//! `io` をレイヤー対応にリファクタした。UI は v1 のまま(常に「背景」1 枚)。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{pos2, Color32, Key, KeyboardShortcut, Modifiers, PointerButton, Pos2};

use crate::canvas_view::CanvasView;
use crate::document::{Background, Document, Interpolation, MAX_LAYERS};
use crate::history::{History, HistoryOp};
use crate::io::{self, SaveFormat};
use crate::tools::eraser::EraserTool;
use crate::tools::fill::FillTool;
use crate::tools::pen::PenTool;
use crate::tools::picker::PickerTool;
use crate::tools::select::{self, Floating, Selection};
use crate::tools::shapes::ShapeTool;
use crate::tools::{Tool, ToolCtx, ToolEvent, ToolKind};
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::color_wheel::ColorWheelState;
use crate::ui::dialogs::{ConfirmOutcome, DialogOutcome};
use crate::ui::layers_panel::{LayersPanelAction, RenameState};
use crate::ui::menu::{MenuAction, MenuState};
use crate::ui::options_bar::OptionsBarCtx;
use crate::ui::{dialogs, menu, options_bar, side_panel, status_bar, toolbar};

/// SPEC §5: 最近使った色は最大 8 個。
const MAX_RECENT_COLORS: usize = 8;

/// SPEC §4: ブラシサイズは 1–64px。
const MIN_BRUSH_SIZE: f32 = 1.0;
const MAX_BRUSH_SIZE: f32 = 64.0;

/// SPEC §8: トーストは約 4 秒表示する。
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// SPEC §7: 「新規」ダイアログのデフォルト値。
const DEFAULT_NEW_WIDTH: u32 = 1280;
const DEFAULT_NEW_HEIGHT: u32 = 720;

/// SPEC §8: JPEG 品質のデフォルト値。
const DEFAULT_JPEG_QUALITY: u8 = 90;

/// 日本語フォントの探索順(ARCHITECTURE.md §9)。最初に読めたものを使う。
/// フォントはバンドルしない(バイナリサイズ・起動時間のため)。
const JAPANESE_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\YuGothM.ttc",
    r"C:\Windows\Fonts\meiryo.ttc",
    r"C:\Windows\Fonts\msgothic.ttc",
];

/// DARASK_BENCH=1 のときのみ存在する、起動計測用の状態(SPEC §11)。
struct BenchState {
    /// `main()` 冒頭で取得した `Instant`(プロセス起動からの経過測定用)。
    process_start: Instant,
    /// これまでに `ui()` が呼ばれ描画された回数。
    frames_drawn: u32,
}

/// 選択ツールの進行中ドラッグ(ARCHITECTURE.md §7)。`Selection`/`Floating`
/// 自体は複数フレームにまたがって保持する必要があるため `DaraskApp` の
/// フィールドとして直接持つ(ARCHITECTURE.md §10 の状態機械どおり)が、
/// 「今まさにドラッグ中か、それは新規選択か浮動片移動か」はこの型でだけ
/// 追跡する。
#[derive(Debug, Clone, Copy)]
enum SelectDrag {
    /// 新規の矩形選択をドラッグ中。
    NewSelection { start: Pos2, current: Pos2 },
    /// 浮動片をドラッグで移動中(`offset` はポインタから浮動片原点までの
    /// オフセット、画像座標)。
    MoveFloating { offset: egui::Vec2 },
    /// 選択内部を Down したが、まだ実際には動いていない状態
    /// (M4 で発見・修正したバグ: 以前は選択内部への Down 即座に浮動化して
    /// いたため、ドラッグせずに離すだけの単クリックでも「浮動化して同位置に
    /// 再合成」という before==after の無意味な undo エントリが積まれ、
    /// Ctrl+Z が 1 回「何も起きない」まま消費されていた)。実際に動いた
    /// (`select_drag_move` で座標が変化した)時点で初めて浮動化する
    /// (SPEC §6: 「選択内部をドラッグ→浮動化」)。
    PendingFloating {
        rect: crate::document::IRect,
        down_img: Pos2,
    },
    /// スケールハンドルをドラッグ中(SPEC §16、ARCHITECTURE.md §14.6)。
    /// `anchor`/`start_w`/`start_h`/`start_center` はドラッグ開始時点で固定
    /// した値(画像座標)。`select::resize_floating_rect` に渡す。
    ResizeFloating {
        handle: select::Handle,
        anchor: Pos2,
        start_w: f32,
        start_h: f32,
        start_center: Pos2,
    },
}

/// 未保存ガード(SPEC §8)が「保存/破棄を選んだ後に何をするか」を覚えておく
/// ためのアクション(ARCHITECTURE.md §10: `pending_action: Option<PendingAction>`
/// (New/Open(path?)/Close))。
#[derive(Debug, Clone)]
enum PendingAction {
    New,
    /// `Some(path)` なら D&D/CLI 等で既にパスが分かっている。`None` なら
    /// ガード通過後に「開く」ダイアログを表示する。
    Open(Option<PathBuf>),
    Close,
}

/// SPEC §7 のダイアログ群(ARCHITECTURE.md §10: `modal: Option<ModalState>`)。
enum ModalState {
    New {
        width: u32,
        height: u32,
        background: Background,
    },
    ImageResize {
        width: u32,
        height: u32,
        keep_aspect: bool,
        interpolation: Interpolation,
    },
    CanvasResize {
        width: u32,
        height: u32,
    },
    JpegQuality {
        quality: u8,
        path: PathBuf,
    },
    ConfirmUnsaved,
}

/// rfd のネイティブダイアログはブロッキングでイベントループを止めるため、
/// フレーム処理の外側(次フレーム冒頭)で呼ぶ必要がある
/// (ARCHITECTURE.md §12-9)。クリックされた瞬間はこのフラグだけを立て、
/// 次フレームの `ui()` 冒頭で実際に呼び出す。
enum DialogRequest {
    OpenFile,
    SaveAs,
}

pub struct DaraskApp {
    doc: Document,
    view: CanvasView,
    history: History,
    tool: ToolKind,
    pen: PenTool,
    eraser: EraserTool,
    line: ShapeTool,
    rect_tool: ShapeTool,
    ellipse: ShapeTool,
    fill: FillTool,
    picker: PickerTool,
    primary: Color32,
    secondary: Color32,
    brush_size: f32,
    /// 最近使った色(SPEC §5: 最大 8、先頭が最新)。
    recent_colors: VecDeque<Color32>,
    /// Alt+クリックによる一時スポイト(SPEC §4)の最中、対応するボタンの
    /// Up が来るまで通常のツール処理を止めておくためのフラグ。
    alt_eyedropper_active: bool,

    // -- v2 §14: カラーパネル(ARCHITECTURE.md §14.3/§14.4, V2-M3) --------
    /// 色相リング + SV 三角形の編集中状態(ドラッグ中は HSV を正とする、
    /// ARCHITECTURE.md §14.9-1)。
    color_wheel: ColorWheelState,
    /// HEX 入力欄の編集中テキスト(`ui/color_panel.rs` 参照)。
    color_hex_buffer: String,
    /// ユーザーパレット(SPEC §14: 「＋」で追加、永続化はしない)。
    user_palette: Vec<Color32>,

    // -- M4: 選択・フローティング(ARCHITECTURE.md §7) --------------------
    selection: Option<Selection>,
    floating: Option<Floating>,
    select_drag: Option<SelectDrag>,
    /// `Floating` のテクスチャキャッシュキー用の採番(canvas_view.rs 参照)。
    next_floating_id: u64,

    // -- M4: ダイアログ・未保存ガード(ARCHITECTURE.md §8, §10) -----------
    modal: Option<ModalState>,
    pending_action: Option<PendingAction>,
    pending_dialog: Option<DialogRequest>,
    /// 保存が完了したら続けて実行するアクション(未保存ガードで「保存」を
    /// 選んだ場合に使う)。
    after_save_action: Option<PendingAction>,
    /// 直近使用した JPEG 品質(次回のデフォルト値、SPEC §8: デフォルト 90)。
    last_jpeg_quality: u8,
    /// 直近フレームで `send_viewport_cmd(Title)` した文字列(変化したときだけ
    /// 再送するためのキャッシュ)。
    last_title: String,
    /// ステータスバーのトースト(SPEC §8: 約 4 秒表示)。
    toast: Option<(String, Instant)>,

    // -- v2 §13: レイヤーパネル(ARCHITECTURE.md §14.8 V2-M2) --------------
    /// ダブルクリックで開始した名前編集の状態(`ui/layers_panel.rs`)。
    layer_rename: RenameState,
    /// 新規レイヤーの名前(SPEC §13: 「レイヤー N」)に使う次の番号。
    /// ドキュメントを新規作成/読み込みし直すたびに 1 にリセットする。
    next_layer_number: u32,

    bench: Option<BenchState>,
}

impl DaraskApp {
    /// `process_start` は `main()` 冒頭で取得した `Instant`。
    /// `bench_mode` は環境変数 `DARASK_BENCH=1` が設定されていたかどうか。
    /// `cli_path` は SPEC §3 の「プログラムから開く」用の起動引数(あれば)。
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        process_start: Instant,
        bench_mode: bool,
        cli_path: Option<PathBuf>,
    ) -> Self {
        setup_japanese_fonts(&cc.egui_ctx);

        // M4 で発見・修正したバグ: egui 0.35 は `Options::zoom_with_keyboard`
        // がデフォルト `true` で、`Context::end_pass` が Ctrl+Plus/Ctrl+Equals/
        // Ctrl+Minus/Ctrl+Num0 を消費してアプリ全体の UI ズーム
        // (`pixels_per_point`)を変更してしまう。本アプリは SPEC §10 で
        // キャンバス側の独自ズーム(`handle_view_shortcuts`)を持つため、
        // この egui 組み込みのグローバル UI ズームは無効化する(特に
        // Ctrl+=(Shift 不要の「+」)はどのショートカットにも束縛していない
        // ため、無効化しないと必ず egui 側に奪われ、UI 全体の拡大率が
        // ユーザーの意図しないまま変化し続けてしまう)。
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

        // SPEC §3: 起動時は新規 1280×720・白背景のドキュメントを自動作成する
        // (MS ペイント方式)。CLI 引数でファイルが指定されていればそれを開く
        // (SPEC §3: 「プログラムから開く」対応)。
        let mut doc = Document::new(DEFAULT_NEW_WIDTH, DEFAULT_NEW_HEIGHT, Background::White);
        let mut startup_error = None;
        if let Some(path) = cli_path {
            match io::load_image(&path) {
                Ok(loaded) => doc = loaded,
                Err(e) => startup_error = Some(format!("開けませんでした: {e}")),
            }
        }

        let bench = bench_mode.then(|| BenchState {
            process_start,
            frames_drawn: 0,
        });

        let mut app = Self {
            doc,
            view: CanvasView::new(),
            history: History::new(),
            tool: ToolKind::Pen,
            pen: PenTool::new(),
            eraser: EraserTool::new(),
            line: ShapeTool::new_line(),
            rect_tool: ShapeTool::new_rect(),
            ellipse: ShapeTool::new_ellipse(),
            fill: FillTool::new(),
            picker: PickerTool::new(),
            // MS ペイント等と同様、初期値はプライマリ=黒・セカンダリ=白。
            primary: Color32::BLACK,
            secondary: Color32::WHITE,
            brush_size: 4.0,
            recent_colors: VecDeque::new(),
            alt_eyedropper_active: false,
            color_wheel: ColorWheelState::new(),
            // 起動 1 フレーム目から正しい表記を出す(空文字だと 1 フレーム
            // だけ空欄がちらつく)。プライマリの初期値(黒)に合わせる。
            color_hex_buffer: color_panel::format_hex(Color32::BLACK),
            user_palette: Vec::new(),
            selection: None,
            floating: None,
            select_drag: None,
            next_floating_id: 0,
            modal: None,
            pending_action: None,
            pending_dialog: None,
            after_save_action: None,
            last_jpeg_quality: DEFAULT_JPEG_QUALITY,
            last_title: String::new(),
            toast: None,
            layer_rename: None,
            next_layer_number: 1,
            bench,
        };
        if let Some(message) = startup_error {
            app.show_toast(message);
        }
        eprintln!("[DIAG] DaraskApp::new() done");
        app
    }

    // -----------------------------------------------------------------
    // ショートカット
    // -----------------------------------------------------------------

    /// 単一キーのツールショートカット(SPEC §4)。テキスト入力中・モーダル
    /// 表示中は無効(SPEC §4 最終行、ARCHITECTURE.md §10:
    /// 「モーダル表示中はキャンバスへの入力を渡さない」の趣旨をショートカット
    /// にも適用する)。
    fn handle_tool_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        let mut requested = None;
        ctx.input(|i| {
            if i.key_pressed(Key::B) {
                requested = Some(ToolKind::Pen);
            } else if i.key_pressed(Key::E) {
                requested = Some(ToolKind::Eraser);
            } else if i.key_pressed(Key::L) {
                requested = Some(ToolKind::Line);
            } else if i.key_pressed(Key::R) {
                requested = Some(ToolKind::Rect);
            } else if i.key_pressed(Key::C) {
                requested = Some(ToolKind::Ellipse);
            } else if i.key_pressed(Key::F) {
                requested = Some(ToolKind::Fill);
            } else if i.key_pressed(Key::I) {
                requested = Some(ToolKind::Picker);
            } else if i.key_pressed(Key::M) {
                requested = Some(ToolKind::Select);
            } else if i.key_pressed(Key::H) {
                requested = Some(ToolKind::Pan);
            }
        });
        if let Some(tool) = requested {
            self.set_tool(tool);
        }
    }

    /// 色/ブラシサイズのショートカット(SPEC §5: X で入れ替え。SPEC §4:
    /// `[`/`]` でブラシサイズ増減)。テキスト入力中・モーダル表示中は無効。
    fn handle_color_and_brush_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        ctx.input(|i| {
            if i.key_pressed(Key::X) {
                std::mem::swap(&mut self.primary, &mut self.secondary);
            }
            if i.key_pressed(Key::OpenBracket) {
                self.brush_size = (self.brush_size - 1.0).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
            }
            if i.key_pressed(Key::CloseBracket) {
                self.brush_size = (self.brush_size + 1.0).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
            }
        });
    }

    /// アンドゥ/リドゥのショートカット(SPEC §9: Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z)。
    fn handle_undo_redo_shortcuts(&mut self, ctx: &egui::Context) {
        // M4 で発見・修正したバグ: 他のショートカットハンドラ(ツール/色/
        // 選択/表示/ファイル)はすべて `egui_wants_keyboard_input()` を確認
        // しているのに、ここだけモーダルの有無しか見ていなかった。オプション
        // バーのブラシサイズ/許容値スライダーの `DragValue` はモーダルでは
        // ないため、その数値編集中に Ctrl+Z を押すとテキスト編集ではなく
        // ドキュメントの undo が奪ってしまっていた(ARCHITECTURE.md §12-1
        // 「Ctrl+Z 等がテキストフィールドと衝突しないよう消費順序に注意」に
        // 違反)。
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        let redo_shortcut_alt = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Z);
        let redo_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Y);
        let undo_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Z);

        let (undo, redo) = ctx.input_mut(|i| {
            // egui::InputState::consume_shortcut は `matches_logically` で
            // 判定するため、余分な Shift 修飾は無視される。つまり
            // Ctrl+Shift+Z は Ctrl+Z にもマッチしてしまうので、最も限定的な
            // ショートカットを先に消費する必要がある(そうしないと
            // Ctrl+Shift+Z が常に「元に戻す」として誤消費されてしまう)。
            // M4 で発見・修正したバグ: 以前は Ctrl+Z を先に判定していたため
            // SPEC §7 の「やり直し (Ctrl+Y, Ctrl+Shift+Z)」の後者が機能して
            // いなかった。
            let redo_alt = i.consume_shortcut(&redo_shortcut_alt);
            let redo = i.consume_shortcut(&redo_shortcut) || redo_alt;
            let undo = i.consume_shortcut(&undo_shortcut);
            (undo, redo)
        });

        // SPEC §13 最終項(v2 で修正): 「レイヤー操作・アンドゥは浮動片や
        // ストローク進行中にはツール切替と同じ扱い(先に確定してから
        // 実行)」。以前は `can_undo_redo_now()` で丸ごとブロックしていたが、
        // それでは浮動片保持中(例: 貼り付け直後)に Ctrl+Z を押しても
        // 「何も起きない」ように見え、SPEC の「先に確定してから実行」
        // (=確定して、その確定を取り消す=実質キャンセル)という仕様に
        // 反していた。`commit_open_gesture` で先に確定してしまえば
        // ストロークは「進行中」ではなくなるため、旧コメントが懸念していた
        // 「CoW タイルの退避時点とドキュメント実状態の食い違い」は起こらない。
        if undo {
            self.commit_open_gesture();
            self.history.undo(&mut self.doc);
        }
        if redo {
            self.commit_open_gesture();
            self.history.redo(&mut self.doc);
        }
    }

    /// 選択関連のショートカット(SPEC §6: Delete/Ctrl+X/Ctrl+C/Ctrl+V/
    /// Ctrl+A、Enter/Esc は選択ツール使用中のみ)。
    fn handle_selection_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        let delete_shortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Delete);
        let cut_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::X);
        let copy_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::C);
        let paste_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::V);
        let select_all_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::A);
        let deselect_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::D);
        let enter_shortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Enter);
        let escape_shortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Escape);

        let is_select = self.tool == ToolKind::Select;
        let (delete, cut, copy, paste, select_all, deselect, commit) = ctx.input_mut(|i| {
            let delete = i.consume_shortcut(&delete_shortcut);
            let cut = i.consume_shortcut(&cut_shortcut);
            let copy = i.consume_shortcut(&copy_shortcut);
            let paste = i.consume_shortcut(&paste_shortcut);
            let select_all = i.consume_shortcut(&select_all_shortcut);
            let deselect = i.consume_shortcut(&deselect_shortcut);
            let commit = is_select
                && (i.consume_shortcut(&enter_shortcut) || i.consume_shortcut(&escape_shortcut));
            (delete, cut, copy, paste, select_all, deselect, commit)
        });

        if delete {
            self.delete_selection();
        }
        if cut {
            self.cut_selection_to_clipboard();
        }
        if copy {
            self.copy_selection_to_clipboard();
        }
        if paste {
            self.paste_from_clipboard();
        }
        if select_all {
            self.select_all();
        }
        if deselect {
            self.commit_selection();
        }
        if commit {
            self.commit_selection();
        }
    }

    /// 表示メニューのショートカット(SPEC §10: Ctrl++/Ctrl+-/Ctrl+1/Ctrl+0)。
    fn handle_view_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        let zoom_in_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Plus);
        let zoom_out_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Minus);
        let zoom_100_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Num1);
        let fit_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Num0);
        let (zoom_in, zoom_out, zoom_100, fit) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&zoom_in_shortcut),
                i.consume_shortcut(&zoom_out_shortcut),
                i.consume_shortcut(&zoom_100_shortcut),
                i.consume_shortcut(&fit_shortcut),
            )
        });
        if zoom_in {
            self.view.zoom_in();
        }
        if zoom_out {
            self.view.zoom_out();
        }
        if zoom_100 {
            self.view.zoom_to_100();
        }
        if fit {
            self.view.fit_to_window(&self.doc);
        }
    }

    /// ファイルメニューのショートカット(SPEC §7: Ctrl+N/Ctrl+O/Ctrl+S/
    /// Ctrl+Shift+S)。Alt+F4 は OS/ウィンドウマネージャが `close_requested`
    /// として通知するため、ここでは扱わない(`handle_close_request` 参照)。
    fn handle_file_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        // Ctrl+Shift+S を Ctrl+S より先に消費する(上の
        // `handle_undo_redo_shortcuts` と同じ理由)。
        let save_as_shortcut = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::S);
        let save_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::S);
        let new_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::N);
        let open_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::O);
        let (save_as, save_doc, new_doc, open_doc) = ctx.input_mut(|i| {
            let save_as = i.consume_shortcut(&save_as_shortcut);
            let save_doc = i.consume_shortcut(&save_shortcut);
            let new_doc = i.consume_shortcut(&new_shortcut);
            let open_doc = i.consume_shortcut(&open_shortcut);
            (save_as, save_doc, new_doc, open_doc)
        });
        if save_as {
            self.begin_save_as();
        } else if save_doc {
            self.begin_save();
        }
        if new_doc {
            self.request_action(PendingAction::New);
        }
        if open_doc {
            self.request_action(PendingAction::Open(None));
        }
    }

    /// レイヤーメニューのショートカット(SPEC §13: Ctrl+Shift+N 新規 /
    /// Ctrl+E 下と結合 / Ctrl+Shift+E 画像の統合)。
    ///
    /// M4 で確立した「余分な Shift 修飾は無視されるため、より限定的な
    /// ショートカットを先に消費する」規則(`handle_undo_redo_shortcuts` 参照)
    /// をここでも守る: Ctrl+Shift+E を Ctrl+E より先に消費する。同じ理由で、
    /// このハンドラは `handle_file_shortcuts`(Ctrl+N)より前に呼ぶ必要がある
    /// (Ctrl+Shift+N が Ctrl+N として誤消費されないように、`ui()` 内の
    /// 呼び出し順序で保証する)。
    fn handle_layer_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        let add_layer_shortcut = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::N);
        let flatten_shortcut = KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::E);
        let merge_down_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::E);

        let (add_layer, flatten, merge_down) = ctx.input_mut(|i| {
            let add_layer = i.consume_shortcut(&add_layer_shortcut);
            let flatten = i.consume_shortcut(&flatten_shortcut);
            let merge_down = i.consume_shortcut(&merge_down_shortcut);
            (add_layer, flatten, merge_down)
        });

        if add_layer {
            self.layer_add();
        }
        if flatten {
            self.layer_flatten();
        }
        if merge_down {
            self.layer_merge_down();
        }
    }

    // -----------------------------------------------------------------
    // ツール切り替え・カーソル・ディスパッチ
    // -----------------------------------------------------------------

    /// ツール切り替えの唯一の入口(ツールバークリック・単一キー双方から
    /// 呼ぶ)。選択ツールから離れるときは浮動片を確定させる
    /// (SPEC §6: 「ツール切替→浮動片をその位置に合成」)。それ以外の描画系
    /// ツールから離れるときも、進行中のジェスチャがあれば確定させる
    /// (M4 で発見・修正したバグ: `tools/mod.rs::Tool::cancel` のコメント
    /// 参照。以前はここで何もしなかったため、ドラッグ中にツール切替キーを
    /// 押すと進行中の `History` ストロークが次のツールの `begin_stroke` に
    /// 無警告で置き換えられ、既に描画済みのピクセルが undo 履歴に残らない
    /// まま失われていた)。
    fn set_tool(&mut self, new_tool: ToolKind) {
        if new_tool == self.tool {
            return;
        }
        self.commit_open_gesture();
        self.tool = new_tool;
    }

    /// 進行中のジェスチャ(選択ツールの浮動片、または他ツールのドラッグ中
    /// ストローク)を、それを中断させる操作の前に確定させる共通フック
    /// (ARCHITECTURE.md §14.2/§14.9-3: 「レイヤー操作・アンドゥは、浮動片や
    /// ストローク進行中にはツール切替と同じ扱い(先に確定してから実行)」を
    /// 一箇所に集約する。`set_tool` に加えて、レイヤー構造の変更・アクティブ
    /// レイヤーの切り替えの前にも呼ぶ)。
    fn commit_open_gesture(&mut self) {
        if self.tool == ToolKind::Select {
            self.commit_selection();
        } else {
            self.end_active_gesture();
        }
    }

    /// 現在のツールに進行中のジェスチャ(ドラッグ)があれば、`Up` が来た
    /// 場合と同様に確定して終了する(`set_tool` からのみ呼ぶ)。
    fn end_active_gesture(&mut self) {
        let mut used_colors = Vec::new();
        let mut ctx = ToolCtx {
            doc: &mut self.doc,
            history: &mut self.history,
            primary: self.primary,
            secondary: self.secondary,
            brush_size: self.brush_size,
            used_colors: &mut used_colors,
        };
        match self.tool {
            ToolKind::Pen => self.pen.cancel(&mut ctx),
            ToolKind::Eraser => self.eraser.cancel(&mut ctx),
            ToolKind::Line => self.line.cancel(&mut ctx),
            ToolKind::Rect => self.rect_tool.cancel(&mut ctx),
            ToolKind::Ellipse => self.ellipse.cancel(&mut ctx),
            // 塗りつぶし/スポイト/手のひらはドラッグ状態(進行中のジェス
            // チャ)を持たない(塗りつぶしは Down で即座に確定する 1 ショット
            // のツール)。選択は上の分岐で別途扱う。
            ToolKind::Fill | ToolKind::Picker | ToolKind::Select | ToolKind::Pan => {}
        }
        for color in used_colors {
            self.push_recent_color(color);
        }
    }

    /// 現在のツールに応じたカーソル形状(手のひらは `Tool` を持たないため
    /// ここで直接返す、ARCHITECTURE.md §4)。
    fn cursor_for_active_tool(&self) -> egui::CursorIcon {
        match self.tool {
            ToolKind::Pen => self.pen.cursor(),
            ToolKind::Eraser => self.eraser.cursor(),
            ToolKind::Line => self.line.cursor(),
            ToolKind::Rect => self.rect_tool.cursor(),
            ToolKind::Ellipse => self.ellipse.cursor(),
            ToolKind::Fill => self.fill.cursor(),
            ToolKind::Picker => self.picker.cursor(),
            ToolKind::Pan => egui::CursorIcon::Grab,
            ToolKind::Select => self.select_cursor(),
        }
    }

    /// SPEC §16: 「ハンドルホバー時はリサイズカーソルを表示」。
    /// `self.view.hover_img()` は前フレームのホバー位置(ステータスバーと
    /// 同じ 1 フレーム遅延、`status_bar::show` 呼び出し箇所のコメント参照)
    /// だが、連続したポインタ移動で駆動されるため実用上は無視できる。
    fn select_cursor(&self) -> egui::CursorIcon {
        if let Some(SelectDrag::ResizeFloating { handle, .. }) = self.select_drag {
            return select::handle_cursor(handle);
        }
        if let Some(hover) = self.view.hover_img() {
            if let Some(handle) = self.hit_resize_handle(hover) {
                return select::handle_cursor(handle);
            }
        }
        egui::CursorIcon::Default
    }

    /// キャンバスから出た `ToolEvent` を、Alt+一時スポイト(SPEC §4)または
    /// 現在のツールへディスパッチする。
    fn dispatch_canvas_events(&mut self, events: Vec<ToolEvent>) {
        for ev in events {
            if let ToolEvent::Down { img, button, mods } = ev {
                if mods.alt {
                    self.sample_eyedropper_color(img, button);
                    self.alt_eyedropper_active = true;
                    continue;
                }
            }
            if self.alt_eyedropper_active {
                if matches!(ev, ToolEvent::Up { .. }) {
                    self.alt_eyedropper_active = false;
                }
                continue;
            }

            // スポイトツール(SPEC §4)は色を書き込む手段が ToolCtx にないため、
            // Alt 一時スポイトと同じ経路(app.rs 直結)で扱う
            // (tools/picker.rs のコメント参照)。
            if self.tool == ToolKind::Picker {
                if let ToolEvent::Down { img, button, .. } = ev {
                    self.sample_eyedropper_color(img, button);
                }
                continue;
            }

            // 選択ツール(SPEC §6)も同様に、`Selection`/`Floating` が
            // `ToolCtx` の外(app.rs 直結)にあるため、ここで直接処理する
            // (tools/select.rs のモジュールコメント参照)。
            if self.tool == ToolKind::Select {
                self.handle_select_event(ev);
                continue;
            }

            let mut used_colors = Vec::new();
            let mut ctx = ToolCtx {
                doc: &mut self.doc,
                history: &mut self.history,
                primary: self.primary,
                secondary: self.secondary,
                brush_size: self.brush_size,
                used_colors: &mut used_colors,
            };
            match self.tool {
                ToolKind::Pen => self.pen.event(ev, &mut ctx),
                ToolKind::Eraser => self.eraser.event(ev, &mut ctx),
                ToolKind::Line => self.line.event(ev, &mut ctx),
                ToolKind::Rect => self.rect_tool.event(ev, &mut ctx),
                ToolKind::Ellipse => self.ellipse.event(ev, &mut ctx),
                ToolKind::Fill => self.fill.event(ev, &mut ctx),
                // 手のひら(canvas_view が横取り)・選択・スポイトは上で
                // 処理済み。
                ToolKind::Select | ToolKind::Pan | ToolKind::Picker => {}
            }
            for color in used_colors {
                self.push_recent_color(color);
            }
        }
    }

    /// 「最近使った色」の先頭に `color` を追加する(SPEC §5: 重複は先頭へ
    /// 移動、最大 8 個)。
    fn push_recent_color(&mut self, color: Color32) {
        self.recent_colors.retain(|c| *c != color);
        self.recent_colors.push_front(color);
        while self.recent_colors.len() > MAX_RECENT_COLORS {
            self.recent_colors.pop_back();
        }
    }

    /// SPEC §4 の「描画系ツール使用中も Alt+クリックで一時スポイト」。
    /// 左クリック=プライマリに、右クリック=セカンダリに取得する
    /// (通常のスポイトツールと同じ割り当て)。範囲外なら何もしない。
    /// SPEC §13: スポイトは合成結果から色を取る(アクティブレイヤーではない)。
    fn sample_eyedropper_color(&mut self, img: egui::Pos2, button: PointerButton) {
        let x = img.x.floor() as i32;
        let y = img.y.floor() as i32;
        // このフレームでまだ合成に反映されていない編集があれば先に反映する
        // (canvas_view のテクスチャ更新はフレーム冒頭で一度だけ走るため)。
        self.doc.recompose_if_dirty();
        let Some(px) = self.doc.composite_pixel(x, y) else {
            return;
        };
        let color = Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]);
        if button == PointerButton::Secondary {
            self.secondary = color;
        } else {
            self.primary = color;
        }
    }

    // -----------------------------------------------------------------
    // M4: 選択・フローティング(ARCHITECTURE.md §7、SPEC §6)
    // -----------------------------------------------------------------

    fn handle_select_event(&mut self, ev: ToolEvent) {
        match ev {
            ToolEvent::Down { img, .. } => self.select_down(img),
            ToolEvent::Drag { img, mods, .. } => self.select_drag_move(img, mods),
            ToolEvent::Up { img, .. } => self.select_up(img),
            ToolEvent::Hover { .. } => {}
        }
    }

    /// 選択矩形・浮動片の外周にある矩形(画像座標)。どちらも無ければ `None`
    /// (`self.floating`/`self.selection` は互いに排他、ARCHITECTURE.md §7)。
    fn current_selection_or_floating_rect(&self) -> Option<crate::document::IRect> {
        if let Some(floating) = &self.floating {
            return Some(select::floating_target_rect(floating));
        }
        self.selection.map(|s| s.rect)
    }

    /// `img`(画像座標)がどのスケールハンドルに当たっているか
    /// (SPEC §16、ARCHITECTURE.md §14.6)。ハンドルはスクリーン論理ポイント
    /// 単位の固定サイズなので、画像座標の矩形を一旦スクリーン座標へ変換
    /// してから判定する。
    fn hit_resize_handle(&self, img: Pos2) -> Option<select::Handle> {
        let rect = self.current_selection_or_floating_rect()?;
        if rect.is_empty() {
            return None;
        }
        let screen_rect = self.view.img_rect_to_screen(rect);
        let handles = select::handle_rects(screen_rect);
        let screen_pos = self.view.img_to_screen_pos(img);
        select::hit_handle(&handles, screen_pos)
    }

    /// ハンドルドラッグを開始する。未浮動の選択でハンドルを掴んだ場合は、
    /// 内部ドラッグと同様にまず浮動化してから拡縮する(SPEC §16)。
    fn begin_resize_handle(&mut self, handle: select::Handle, img: Pos2) {
        if self.floating.is_none() {
            let Some(selection) = self.selection else {
                return;
            };
            self.begin_floating_from_selection(selection.rect, img);
        }
        let Some(floating) = &self.floating else {
            return;
        };
        let (fx, fy) = handle.fraction();
        let pos = floating.pos;
        let w = floating.w as f32;
        let h = floating.h as f32;
        let anchor = pos2(pos.x + (1.0 - fx) * w, pos.y + (1.0 - fy) * h);
        let start_center = pos2(pos.x + w / 2.0, pos.y + h / 2.0);
        self.select_drag = Some(SelectDrag::ResizeFloating {
            handle,
            anchor,
            start_w: w,
            start_h: h,
            start_center,
        });
    }

    /// ハンドルドラッグの更新(SPEC §16)。浮動片のピクセルは常に
    /// `Floating::original`(浮動化時点の画素)からバイリニアで再サンプリング
    /// する(累積劣化させない、ARCHITECTURE.md §14.6)。サイズが変わったとき
    /// だけ新しい `id` を割り当ててテクスチャを作り直させる。
    #[allow(clippy::too_many_arguments)]
    fn apply_resize_floating(
        &mut self,
        handle: select::Handle,
        anchor: Pos2,
        start_w: f32,
        start_h: f32,
        start_center: Pos2,
        img: Pos2,
        lock_aspect: bool,
    ) {
        let Some(floating) = self.floating.as_ref() else {
            return;
        };
        let (new_pos, new_w, new_h) = select::resize_floating_rect(
            handle,
            anchor,
            start_w,
            start_h,
            start_center,
            img,
            lock_aspect,
            select::MIN_FLOATING_SIZE,
            select::MAX_FLOATING_SIZE,
        );
        let new_w_px = (new_w.round() as u32).max(1);
        let new_h_px = (new_h.round() as u32).max(1);
        let resampled = if new_w_px != floating.w || new_h_px != floating.h {
            Some((
                select::resample_bilinear(
                    &floating.original,
                    floating.orig_w,
                    floating.orig_h,
                    new_w_px,
                    new_h_px,
                ),
                self.alloc_floating_id(),
            ))
        } else {
            None
        };
        let Some(floating) = self.floating.as_mut() else {
            return;
        };
        if let Some((pixels, id)) = resampled {
            floating.pixels = pixels;
            floating.w = new_w_px;
            floating.h = new_h_px;
            floating.id = id;
        }
        floating.pos = new_pos;
    }

    fn select_down(&mut self, img: Pos2) {
        if let Some(handle) = self.hit_resize_handle(img) {
            self.begin_resize_handle(handle, img);
            return;
        }
        if let Some(floating) = &self.floating {
            let bounds = select::floating_target_rect(floating);
            if select::rect_contains(bounds, img) {
                let offset = img - floating.pos;
                self.select_drag = Some(SelectDrag::MoveFloating { offset });
                return;
            }
            // 浮動片の外をクリック: 現在位置で確定してから、新規選択として
            // 扱う(SPEC §6: 「選択外クリック」で確定)。
            self.commit_selection();
        }
        if let Some(selection) = self.selection {
            if select::rect_contains(selection.rect, img) {
                // M4 で発見・修正したバグ: ここで即座に `begin_floating_from_
                // selection` を呼んでいたため、ドラッグせずに離すだけの
                // 単クリックでも浮動化(元領域の透明化+同位置への再合成)が
                // 起き、before==after の無意味な undo エントリが積まれて
                // いた。実際に動いた場合(`select_drag_move`)にのみ浮動化
                // するよう、まずは「保留」状態を記録するだけにする
                // (SPEC §6: 「選択内部をドラッグ→浮動化」)。
                self.select_drag = Some(SelectDrag::PendingFloating {
                    rect: selection.rect,
                    down_img: img,
                });
                return;
            }
        }
        self.selection = None;
        self.select_drag = Some(SelectDrag::NewSelection {
            start: img,
            current: img,
        });
    }

    fn select_drag_move(&mut self, img: Pos2, mods: Modifiers) {
        match self.select_drag {
            Some(SelectDrag::NewSelection { start, .. }) => {
                self.select_drag = Some(SelectDrag::NewSelection {
                    start,
                    current: img,
                });
            }
            Some(SelectDrag::MoveFloating { offset }) => {
                if let Some(floating) = &mut self.floating {
                    floating.pos = img - offset;
                }
            }
            Some(SelectDrag::PendingFloating { rect, down_img }) => {
                if img != down_img {
                    // 実際に動いた: ここで初めて浮動化する。
                    // `begin_floating_from_selection` は `select_drag` を
                    // `MoveFloating` に設定するので、続けて同じ `img` で
                    // 再度呼び出すことで、浮動片を 1 フレーム遅れず現在位置
                    // まで追従させる。
                    self.begin_floating_from_selection(rect, down_img);
                    self.select_drag_move(img, mods);
                }
            }
            Some(SelectDrag::ResizeFloating {
                handle,
                anchor,
                start_w,
                start_h,
                start_center,
            }) => {
                // SPEC §16: 「Shift で縦横比固定」。
                self.apply_resize_floating(
                    handle,
                    anchor,
                    start_w,
                    start_h,
                    start_center,
                    img,
                    mods.shift,
                );
            }
            None => {}
        }
    }

    fn select_up(&mut self, img: Pos2) {
        match self.select_drag.take() {
            Some(SelectDrag::NewSelection { start, .. }) => {
                // v2 レビューで発見・修正したバグ: `irect_from_points` は
                // floor/ceil で外側に丸めるため、`start`/`img` の画像座標が
                // 整数ちょうどでない限り(高 DPI スケーリングや 100% 以外の
                // ズーム、端数パンでは頻繁に起こる)、ドラッグせずに離した
                // だけの単クリックでも幅・高さ 1 の非空矩形が残ってしまって
                // いた(SPEC §6: 「ドラッグで矩形選択」、単クリックは選択を
                // 残さないのが期待動作)。ポインタが実際に動いていなければ
                // (`img` が Down 時の `start` と一致していれば)選択を作らない
                // (`PendingFloating` の `img != down_img` チェックと同じ
                // 考え方)。
                self.selection = if img == start {
                    None
                } else {
                    let rect = select::irect_from_points(start, img)
                        .clamp_to(self.doc.width, self.doc.height);
                    if rect.is_empty() {
                        None
                    } else {
                        Some(Selection { rect })
                    }
                };
            }
            Some(SelectDrag::MoveFloating { offset }) => {
                if let Some(floating) = &mut self.floating {
                    floating.pos = img - offset;
                }
            }
            // 単クリック(移動なし)で離した: 浮動化せず選択をそのまま維持
            // する(上の `select_drag_move` のコメント参照)。
            //
            // ResizeFloating: 最後の `select_drag_move` で既に反映済み
            // (`ToolEvent::Up` は Shift の状態を運ばないため、ここでは
            // 追加のリサイズ適用をしない。canvas_view のポインタ処理は
            // ボタンが離れたフレームで最後の位置を Drag ではなく Up として
            // 送るが、その差はドラッグ中の 1 フレーム未満のポインタ移動分
            // でしかなく、見た目には現れない)。
            Some(SelectDrag::PendingFloating { .. } | SelectDrag::ResizeFloating { .. }) | None => {
            }
        }
    }

    /// 選択内部をドラッグ開始 = 浮動化(SPEC §6)。領域ピクセルを `Floating`
    /// に複写し、元領域を透明化する。この透明化は History のストロークを
    /// 開いたまま(まだ push しない)にしておき、確定時(`commit_selection`)
    /// に「切り出し元の透明化+合成先」をまとめて 1 つの `Patch` にする
    /// (ARCHITECTURE.md §7)。
    fn begin_floating_from_selection(&mut self, rect: crate::document::IRect, img: Pos2) {
        let rect = rect.clamp_to(self.doc.width, self.doc.height);
        if rect.is_empty() {
            self.selection = None;
            return;
        }
        self.history.begin_stroke(self.doc.active);
        self.history.ensure_tiles_saved(&self.doc, rect);
        let pixels = select::extract_region(&self.doc, rect);
        select::clear_region_transparent(&mut self.doc, rect);
        self.doc.modified = true;

        let pos = pos2(rect.x0 as f32, rect.y0 as f32);
        let id = self.alloc_floating_id();
        self.floating = Some(Floating::new(
            pixels,
            rect.width() as u32,
            rect.height() as u32,
            pos,
            Some(rect),
            id,
        ));
        self.selection = None;
        let offset = img - pos;
        self.select_drag = Some(SelectDrag::MoveFloating { offset });
    }

    fn alloc_floating_id(&mut self) -> u64 {
        self.next_floating_id += 1;
        self.next_floating_id
    }

    /// 浮動片を現在位置に合成して 1 つの undo 単位にし、選択を解除する
    /// (SPEC §6: Enter/選択外クリック/ツール切替での確定、Ctrl+D、Esc)。
    /// 浮動片が無い(単なる矩形選択だけ、または何も無い)場合は選択を
    /// 解除するだけ。
    fn commit_selection(&mut self) {
        self.select_drag = None;
        if let Some(floating) = self.floating.take() {
            let target = select::floating_target_rect(&floating);
            self.history.ensure_tiles_saved(&self.doc, target);
            select::composite_floating(&mut self.doc, &floating);
            self.history.commit_stroke(&mut self.doc);
        }
        self.selection = None;
    }

    /// 選択領域(または浮動片)を消去する(SPEC §6: Delete)。浮動片がある
    /// 場合は合成せずに捨てる(= 既に開いているストロークをそのまま
    /// 確定させる。切り出し元の透明化だけが 1 つの undo 単位になる。
    /// クリップボードからの貼り付けで `cut_from` が無い浮動片を削除した
    /// 場合は、何も書き込まれていないので undo 単位も積まれない)。
    ///
    /// v2 レビューで発見・修正したバグ: `Ctrl+A`(`select_all`)は現在の
    /// ツールを問わず選択を作れるため、例えばペンツールでドラッグ描画中に
    /// Delete/Ctrl+X を押すと、下の `history.begin_stroke` が進行中の
    /// ペンストロークのレコーダを無警告で置き換えてしまい、(1) 描きかけの
    /// 画素が削除パッチの `before` に「元からあった画素」として混入する、
    /// (2) 以降のドラッグは `history.stroke == None` のまま画素を書き続け、
    /// `Up` の `commit_stroke` が no-op になって永久に undo 不能になる、
    /// という 2 重の破損があった(SPEC §9「1 ストローク = 1 undo 単位」
    /// 違反)。`end_active_gesture` は現在のツールが描画系(ペン/消しゴム/
    /// 図形)なら進行中のストロークを独立した undo 単位として先に確定し、
    /// 選択ツールでは何もしない(`commit_open_gesture` と違い
    /// `commit_selection` は呼ばないため、これから処理する浮動片/選択を
    /// 誤って確定・消費しない)。
    fn delete_selection(&mut self) {
        self.end_active_gesture();
        self.select_drag = None;
        if self.floating.take().is_some() {
            self.history.commit_stroke(&mut self.doc);
            self.selection = None;
            return;
        }
        if let Some(selection) = self.selection.take() {
            let rect = selection.rect.clamp_to(self.doc.width, self.doc.height);
            if !rect.is_empty() {
                self.history.begin_stroke(self.doc.active);
                self.history.ensure_tiles_saved(&self.doc, rect);
                select::clear_region_transparent(&mut self.doc, rect);
                self.history.commit_stroke(&mut self.doc);
            }
        }
    }

    /// 現在の選択(浮動片優先)の画素を取得する。無ければ `None`。
    fn selected_pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
        if let Some(floating) = &self.floating {
            return Some((floating.w, floating.h, floating.pixels.clone()));
        }
        if let Some(selection) = &self.selection {
            let rect = selection.rect.clamp_to(self.doc.width, self.doc.height);
            if rect.is_empty() {
                return None;
            }
            return Some((
                rect.width() as u32,
                rect.height() as u32,
                select::extract_region(&self.doc, rect),
            ));
        }
        None
    }

    /// Ctrl+C(SPEC §6)。クリップボードへの書き込みに成功したら `true` を
    /// 返す(`cut_selection_to_clipboard` が「コピーが成功した場合のみ
    /// 削除する」という契約を守るために使う)。
    fn copy_selection_to_clipboard(&mut self) -> bool {
        let Some((w, h, pixels)) = self.selected_pixels() else {
            return false;
        };
        match io::copy_image_to_clipboard(w, h, &pixels) {
            Ok(()) => true,
            Err(e) => {
                self.show_toast(format!("コピーに失敗しました: {e}"));
                false
            }
        }
    }

    /// Ctrl+X(SPEC §6: 「切り取りは透明で埋める」)。
    ///
    /// M4 で発見・修正したバグ: 以前はコピーの成否を確認せずに常に
    /// `delete_selection` していたため、クリップボードが他プロセスに
    /// ロックされている等でコピーが失敗した場合でも選択領域が透明化されて
    /// しまい、「コピーに失敗しました」のトーストが出るのに貼り付け先には
    /// データが無い、という「切り取り=コピー成功時のみ削除」の操作契約
    /// 違反が起きていた。
    fn cut_selection_to_clipboard(&mut self) {
        if self.selected_pixels().is_none() {
            return;
        }
        if self.copy_selection_to_clipboard() {
            self.delete_selection();
        }
    }

    /// Ctrl+A(SPEC §6: 全選択)。既存の浮動片は先に確定する。
    fn select_all(&mut self) {
        self.commit_selection();
        if self.doc.width == 0 || self.doc.height == 0 {
            self.selection = None;
            return;
        }
        self.selection = Some(Selection {
            rect: crate::document::IRect {
                x0: 0,
                y0: 0,
                x1: self.doc.width as i32,
                y1: self.doc.height as i32,
            },
        });
    }

    /// SPEC §6: 「ドキュメントが完全に未編集・未保存(起動直後の白紙)」の
    /// 判定。パスが無く(新規保存されていない)、かつ一度も編集されていない
    /// (`History::commit_stroke` が実際に何かを push したことがない)ことを
    /// もって「白紙」とみなす。
    fn doc_is_pristine(&self) -> bool {
        self.doc.path.is_none() && !self.doc.modified
    }

    /// Ctrl+V(SPEC §6)。
    fn paste_from_clipboard(&mut self) {
        match io::read_clipboard_image() {
            Ok((w, h, pixels)) => self.paste_pixels(w, h, pixels),
            Err(e) => self.show_toast(format!("貼り付けに失敗しました: {e}")),
        }
    }

    /// `paste_from_clipboard` の、実際の OS クリップボードアクセスを含まない
    /// 部分(`io::read_clipboard_image` は決定的にテストできないため、
    /// ユニットテストから直接呼べるよう分離している)。
    ///
    /// v2 レビューで発見・修正した重大なバグ: 以前はここで `commit_selection`
    /// (選択ツールの浮動片だけを確定する)しか呼んでいなかったため、
    /// 描画系ツール(ペン等)でボタンを押したままドラッグ中(=
    /// `History::has_open_stroke() == true` だが `StrokeTool` は
    /// `commit_stroke` まで `doc.modified` を立てない)に Ctrl+V を押すと、
    /// `doc_is_pristine()`(`path.is_none() && !modified` だけを見る)が
    /// 誤って「白紙」と判定してしまい、`replace_document_with_pasted_image`
    /// でドキュメント自体(レイヤー・寸法)が丸ごと差し替わっていた。この
    /// とき進行中のストロークのレコーダ(旧ドキュメントのタイルを退避した
    /// もの)は開いたまま残り、ボタンを離した時点で新ドキュメントに対して
    /// 旧ドキュメントの CoW タイル内容から再構成した壊れた `Patch` が push
    /// されていた(undo するとバイト正確な復元が破損する、v1 の『ストローク
    /// 進行中に構造操作』型バグの再発)。他ツール切替と同様、`Ctrl+V` の
    /// 冒頭でも先に `commit_open_gesture()` を呼ぶことで、ストロークを
    /// 独立した undo 単位として確定させてから `doc_is_pristine()` を判定する
    /// (確定によって `doc.modified` が正しく立つため、白紙判定も正しくなる)。
    fn paste_pixels(&mut self, w: u32, h: u32, pixels: Vec<u8>) {
        self.commit_open_gesture();
        if w == 0 || h == 0 {
            return;
        }
        if self.doc_is_pristine() {
            self.replace_document_with_pasted_image(w, h, pixels);
        } else {
            self.begin_paste_floating(w, h, pixels);
        }
    }

    /// SPEC §6: 白紙でない場合の貼り付け、クリップボード画像をビュー中央に
    /// 浮動片として配置する。
    ///
    /// M4 で発見・修正した重大なバグ: 以前はツールを選択(Select)に切り替え
    /// ないまま `history.begin_stroke()` を呼んで浮動片を作っていた。この
    /// ため、貼り付け後にペン等の描画ツールでキャンバスをクリックすると、
    /// そのツールの `begin_stroke`(history.rs::begin_stroke は既存レコーダを
    /// 無警告で置き換える)が貼り付け用のストロークレコーダを破棄してしまい、
    /// (1) Enter/Esc による確定が効かない(選択ツール専用のため)、
    /// (2) Ctrl+D 等で `commit_selection` しても `history.stroke == None` の
    /// ため `ensure_tiles_saved`/`commit_stroke` が no-op のまま
    /// `composite_floating` だけがピクセルを書き込み、貼り付け確定が
    /// undo 履歴に一切積まれない(SPEC §9「1 貼り付け確定 = 1 undo 単位」
    /// 違反、Ctrl+Z で取り消せない)、という 2 つの不具合があった。貼り付け
    /// 時点で明示的に選択ツールへ切り替えることで、以後のイベントディス
    /// パッチが選択の浮動片ハンドリング(`handle_select_event`)へ向かい、
    /// 他ツールの `begin_stroke` に晒されなくなる。
    fn begin_paste_floating(&mut self, w: u32, h: u32, pixels: Vec<u8>) {
        let center = self.view.view_center_img();
        let pos = pos2(center.x - w as f32 / 2.0, center.y - h as f32 / 2.0);
        let id = self.alloc_floating_id();
        self.set_tool(ToolKind::Select);
        // 切り出し元が無いので `begin_stroke` するだけで `ensure_tiles_saved`
        // は呼ばない(confirm 時に合成先だけ保存すれば十分、
        // `commit_selection` 参照)。
        self.history.begin_stroke(self.doc.active);
        self.floating = Some(Floating::new(pixels, w, h, pos, None, id));
        self.selection = None;
        // M4 で発見・修正したバグ: 浮動片は画面に見えている未保存の変更
        // だが、以前は `commit_selection` で合成されるまで `doc.modified` が
        // 立たなかった。このため貼り付け直後にウィンドウを閉じる/新規/開く
        // (`handle_close_request`/`request_action` はいずれも
        // `doc.modified` だけを見る)と、確認なしに貼り付け内容が破棄
        // されていた(SPEC §8 の未保存ガードの趣旨に反する)。
        self.doc.modified = true;
    }

    /// SPEC §6: 白紙時の置き換え貼り付け。ドキュメント全体を貼り付け画像の
    /// サイズに置き換える(スクリーンショット→保存が最短になるように)。
    /// SPEC §13: 新規作成直後と同様「背景」レイヤー 1 枚になる。
    fn replace_document_with_pasted_image(&mut self, w: u32, h: u32, pixels: Vec<u8>) {
        let before = self.doc.snapshot();
        self.doc.replace_with_single_layer(w, h, pixels);
        self.doc.modified = true;
        let after = self.doc.snapshot();
        self.history.push(HistoryOp::ReplaceAll { before, after });
        self.layer_rename = None;
        self.next_layer_number = 1;
    }

    fn draw_selection_overlay(&mut self, painter: &egui::Painter) {
        if let Some(SelectDrag::NewSelection { start, current }) = self.select_drag {
            let rect = select::irect_from_points(start, current);
            self.view.draw_selection_outline(painter, rect);
            return;
        }
        if let Some(floating) = self.floating.as_ref() {
            self.view.draw_floating(painter, floating);
            let bounds = select::floating_target_rect(floating);
            self.view.draw_selection_outline(painter, bounds);
            self.view.draw_resize_handles(painter, bounds);
            return;
        }
        if let Some(selection) = &self.selection {
            self.view.draw_selection_outline(painter, selection.rect);
            self.view.draw_resize_handles(painter, selection.rect);
        }
    }

    /// ステータスバーの「選択サイズ」欄(SPEC §3)。浮動片があればその
    /// サイズ、無ければ選択矩形のサイズ。
    fn current_selection_size(&self) -> Option<(u32, u32)> {
        if let Some(floating) = &self.floating {
            return Some((floating.w, floating.h));
        }
        self.selection
            .map(|s| (s.rect.width() as u32, s.rect.height() as u32))
    }

    // -----------------------------------------------------------------
    // M4: ファイル I/O・未保存ガード(ARCHITECTURE.md §8, SPEC §8)
    // -----------------------------------------------------------------

    fn show_toast(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    /// トーストの残り時間を管理し、表示中なら再描画タイマーを予約する
    /// (ARCHITECTURE.md §3 の再描画ポリシーの唯一の例外)。表示すべき文言を
    /// 返す。
    fn tick_toast(&mut self, ctx: &egui::Context) -> Option<String> {
        let (message, started) = self.toast.as_ref()?;
        let elapsed = started.elapsed();
        if elapsed >= TOAST_DURATION {
            self.toast = None;
            None
        } else {
            ctx.request_repaint_after(TOAST_DURATION - elapsed);
            Some(message.clone())
        }
    }

    /// D&D でファイルが落とされたら、未保存ガードを通して開く(SPEC §8)。
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            // 他のモーダル(新規/サイズ変更ダイアログ等)が表示中に
            // ドキュメントを差し替えるとモーダルの状態と食い違うため、
            // 何もしない(モーダルを閉じてから再度ドロップしてもらう)。
            return;
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|f| f.path) {
            self.request_action(PendingAction::Open(Some(path)));
        }
    }

    /// ウィンドウの閉じる要求(SPEC §8: 未保存変更ガード)。
    /// `close_requested` は検知したフレーム内で即座に
    /// `ViewportCommand::CancelClose` を送る必要がある(ARCHITECTURE.md
    /// §12-2)。変更が無ければキャンセルせずそのまま閉じさせる。
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested || !self.doc.modified {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.modal.is_none() {
            self.pending_action = Some(PendingAction::Close);
            self.modal = Some(ModalState::ConfirmUnsaved);
        } else {
            // M4 で発見・修正したバグ: 以前は別のモーダル(画像サイズ変更等)
            // が表示中に閉じる要求が来ると、ここで何もせずに握りつぶして
            // いた(`CancelClose` は送るのでプロセスは終了しないが、
            // ユーザーには「閉じられない理由」が一切示されず、そのモーダルを
            // 閉じた後も再度確認は出なかった)。`pending_action` だけを
            // 予約しておき、`show_modal` がそのモーダルを閉じたタイミングで
            // 未保存確認へ引き継ぐ(SPEC §8「閉じる前に保存確認」の趣旨)。
            self.pending_action = Some(PendingAction::Close);
        }
    }

    /// rfd のダイアログ呼び出し(ARCHITECTURE.md §12-9: フレーム処理の
    /// 外側、次フレーム冒頭で行う)。
    fn process_pending_dialog(&mut self) {
        let Some(request) = self.pending_dialog.take() else {
            return;
        };
        match request {
            DialogRequest::OpenFile => {
                if let Some(path) = io::open_dialog() {
                    self.open_path(path);
                }
            }
            DialogRequest::SaveAs => {
                let default_name = self.default_save_file_name();
                match io::save_dialog(&default_name) {
                    Some(path) => {
                        let path = io::ensure_extension(path);
                        self.begin_save_to_path(path);
                    }
                    None => self.after_save_action = None,
                }
            }
        }
    }

    fn default_save_file_name(&self) -> String {
        match &self.doc.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "無題.png".to_owned()),
            None => "無題.png".to_owned(),
        }
    }

    /// 未保存ガードを通してからアクションを実行する(SPEC §8)。
    fn request_action(&mut self, action: PendingAction) {
        if self.doc.modified {
            self.pending_action = Some(action);
            self.modal = Some(ModalState::ConfirmUnsaved);
        } else {
            self.execute_pending_action(action);
        }
    }

    /// 未保存ガードを通過した(または最初から不要だった)アクションを
    /// 実際に行う。
    fn execute_pending_action(&mut self, action: PendingAction) {
        self.commit_selection();
        match action {
            PendingAction::New => {
                self.modal = Some(ModalState::New {
                    width: DEFAULT_NEW_WIDTH,
                    height: DEFAULT_NEW_HEIGHT,
                    background: Background::White,
                });
            }
            PendingAction::Open(Some(path)) => self.open_path(path),
            PendingAction::Open(None) => self.pending_dialog = Some(DialogRequest::OpenFile),
            PendingAction::Close => std::process::exit(0),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        match io::load_image(&path) {
            Ok(doc) => {
                self.doc = doc;
                self.history = History::new();
                self.selection = None;
                self.floating = None;
                self.select_drag = None;
                self.view = CanvasView::new();
                self.layer_rename = None;
                self.next_layer_number = 1;
            }
            Err(e) => self.show_toast(format!("開けませんでした: {e}")),
        }
    }

    /// 「上書き保存」(SPEC §7: Ctrl+S)。パスが未知(無題)なら「名前を
    /// 付けて保存」ダイアログにフォールバックする。
    fn begin_save(&mut self) {
        match self.doc.path.clone() {
            Some(path) => self.begin_save_to_path(path),
            None => self.pending_dialog = Some(DialogRequest::SaveAs),
        }
    }

    /// 「名前を付けて保存」(SPEC §7: Ctrl+Shift+S)。常にダイアログを表示。
    fn begin_save_as(&mut self) {
        self.pending_dialog = Some(DialogRequest::SaveAs);
    }

    /// 保存先が決まった後の共通処理。JPEG なら品質ダイアログを挟む
    /// (SPEC §8)。
    fn begin_save_to_path(&mut self, path: PathBuf) {
        match io::format_for_path(&path) {
            Some(SaveFormat::Jpeg { .. }) => {
                self.modal = Some(ModalState::JpegQuality {
                    quality: self.last_jpeg_quality,
                    path,
                });
            }
            Some(format) => self.finish_save(path, format),
            None => {
                // M4 で発見・修正したバグ: 「名前を付けて保存」経路
                // (`process_pending_dialog`)は `io::ensure_extension` を通す
                // ため、対応外拡張子(.gif/.webp 等)には確実に `.png` が
                // 付く。しかし「上書き保存」(Ctrl+S)は `doc.path` が既に
                // 決まっていればここへ直行し `ensure_extension` を通らない
                // ため、GIF/WebP を開いて Ctrl+S すると拡張子は `.gif`/
                // `.webp` のまま中身だけ PNG バイト列で上書きされてしまって
                // いた(SPEC §8: 「拡張子で判定、不明な拡張子なら .png を
                // 付ける」に違反、かつ拡張子と実体が食い違うファイルが
                // サイレントに生成される)。ここでも同じく拡張子を補正する。
                let path = io::ensure_extension(path);
                self.finish_save(path, SaveFormat::Png);
            }
        }
    }

    fn confirm_jpeg_quality(&mut self, quality: u8, path: PathBuf) {
        self.last_jpeg_quality = quality;
        self.finish_save(path, SaveFormat::Jpeg { quality });
    }

    fn finish_save(&mut self, path: PathBuf, format: SaveFormat) {
        // SPEC §13: 保存は常に可視レイヤーの合成(統合)結果を書き出す。
        // レイヤーが複数ある状態で保存したことをトーストで知らせる
        // (`io::save_image` 自体が統合するため、ここでは判定のみ)。
        let had_multiple_layers = self.doc.layers.len() > 1;
        match io::save_image(&mut self.doc, &path, format) {
            Ok(()) => {
                self.doc.path = Some(path);
                self.doc.modified = false;
                if had_multiple_layers {
                    self.show_toast("レイヤーは統合して保存されました".to_owned());
                }
                if let Some(action) = self.after_save_action.take() {
                    self.execute_pending_action(action);
                }
            }
            Err(e) => {
                self.after_save_action = None;
                self.show_toast(format!("保存に失敗しました: {e}"));
            }
        }
    }

    fn confirm_unsaved_save(&mut self) {
        let action = self.pending_action.take();
        self.after_save_action = action;
        self.begin_save();
    }

    fn confirm_unsaved_discard(&mut self) {
        if let Some(action) = self.pending_action.take() {
            self.execute_pending_action(action);
        }
    }

    fn confirm_unsaved_cancel(&mut self) {
        self.pending_action = None;
    }

    fn confirm_new(&mut self, width: u32, height: u32, background: Background) {
        self.doc = Document::new(width.clamp(1, 8192), height.clamp(1, 8192), background);
        self.history = History::new();
        self.selection = None;
        self.floating = None;
        self.select_drag = None;
        self.view = CanvasView::new();
        self.layer_rename = None;
        self.next_layer_number = 1;
    }

    // -----------------------------------------------------------------
    // M4: 画像メニュー(SPEC §7)
    // -----------------------------------------------------------------

    /// `before`(操作前の全レイヤースナップショット)と現在のドキュメントの
    /// 差分から `HistoryOp::ReplaceAll` を積む(SPEC §13: 画像メニューの
    /// 操作は全レイヤーに適用されるため、v1 の単一バッファ `Replace` ではなく
    /// 全レイヤー+寸法のスナップショットを使う、ARCHITECTURE.md §14.2)。
    fn push_replace_all(&mut self, before: crate::document::DocSnapshot) {
        let after = self.doc.snapshot();
        self.history.push(HistoryOp::ReplaceAll { before, after });
        self.doc.mark_all_dirty();
        self.doc.modified = true;
    }

    fn apply_flip_horizontal(&mut self) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.flip_horizontal();
        self.push_replace_all(before);
    }

    fn apply_flip_vertical(&mut self) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.flip_vertical();
        self.push_replace_all(before);
    }

    fn apply_rotate_cw(&mut self) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.rotate_cw();
        self.push_replace_all(before);
    }

    fn apply_rotate_ccw(&mut self) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.rotate_ccw();
        self.push_replace_all(before);
    }

    /// SPEC §7: 「選択範囲でトリミング」。選択(または浮動片)が無ければ
    /// 何もしない(メニュー側で無効化もしている)。
    fn apply_crop_to_selection(&mut self) {
        let rect = match (&self.selection, &self.floating) {
            (Some(sel), _) => Some(sel.rect),
            (None, Some(floating)) => Some(select::floating_target_rect(floating)),
            (None, None) => None,
        };
        let Some(rect) = rect else {
            return;
        };
        self.commit_selection();
        let rect = rect.clamp_to(self.doc.width, self.doc.height);
        if rect.is_empty() {
            return;
        }
        let before = self.doc.snapshot();
        self.doc.crop_to(rect);
        self.push_replace_all(before);
        self.selection = None;
    }

    fn confirm_image_resize(&mut self, width: u32, height: u32, interpolation: Interpolation) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.resize(width.max(1), height.max(1), interpolation);
        self.push_replace_all(before);
    }

    fn confirm_canvas_resize(&mut self, width: u32, height: u32) {
        self.commit_selection();
        let before = self.doc.snapshot();
        self.doc.resize_canvas(width.max(1), height.max(1));
        self.push_replace_all(before);
    }

    // -----------------------------------------------------------------
    // v2 §13: レイヤー操作(ARCHITECTURE.md §14.2, §14.8 V2-M2)
    //
    // 構造を変える操作(新規/複製/削除/上下移動/下と結合/画像の統合)は
    // すべて「①進行中のジェスチャを先に確定 ②`Document` の純粋な操作
    // (成功したときだけ) ③成功していれば 1 undo 単位で push」という同じ
    // 手順を踏む(SPEC §13: 「表示切替と不透明度変更は履歴に積まない」の
    // 対比として、これらは全部 1 undo 単位になる)。
    //
    // v2 レビューで発見・修正したバグ: 以前は全操作が `apply_layer_op` で
    // 全レイヤーの前後スナップショット(`Document::snapshot`)を取り
    // `HistoryOp::ReplaceAll` として push していた。4000×4000・10 レイヤーの
    // ような大きめの文書で「新規レイヤー」を 1 クリックしただけで
    // before+after 合わせて 1GB 超のクローンが走り、履歴メモリ上限
    // (256MB)を単独の op で超過して直近 10 件を除く undo 履歴が丸ごと
    // 破棄される、という問題があった。ARCHITECTURE.md §14.2 の設計どおり、
    // 新規/複製/削除/移動/下と結合は「影響するレイヤー(最大 2 枚)だけ」を
    // 保持する軽量な `HistoryOp` 専用バリアントを push する
    // (`push_layer_history`)。全レイヤーの合成が必要な「画像の統合」だけは
    // 引き続き `ReplaceAll` を使う(ARCHITECTURE.md §14.2 の
    // `ReplaceAll` docstring どおり)。
    // -----------------------------------------------------------------

    /// 軽量なレイヤー構造操作を 1 undo 単位として push する
    /// (`push_replace_all` と同じ副作用(全面 dirty・`modified` 設定)を、
    /// 全レイヤースナップショットを取らずに行う)。
    fn push_layer_history(&mut self, op: HistoryOp) {
        self.history.push(op);
        self.doc.mark_all_dirty();
        self.doc.modified = true;
    }

    /// SPEC §13: 「新規レイヤーは透明で名前は『レイヤー N』」。
    fn next_layer_name(&mut self) -> String {
        let name = format!("レイヤー {}", self.next_layer_number);
        self.next_layer_number += 1;
        name
    }

    fn layer_add(&mut self) {
        self.commit_open_gesture();
        let name = self.next_layer_name();
        let before_active = self.doc.active_index();
        if self.doc.add_layer(name.clone()) {
            let index = self.doc.active_index();
            self.push_layer_history(HistoryOp::AddLayer {
                index,
                name,
                before_active,
            });
        }
    }

    fn layer_duplicate(&mut self) {
        self.commit_open_gesture();
        let before_active = self.doc.active_index();
        if self.doc.duplicate_active_layer() {
            let index = self.doc.active_index();
            let layer = self.doc.layers[index].clone();
            self.push_layer_history(HistoryOp::DuplicateLayer {
                index,
                layer,
                before_active,
            });
        }
    }

    fn layer_delete(&mut self) {
        self.commit_open_gesture();
        // `Document::remove_active_layer` 自身の拒否条件(レイヤー1枚)を
        // 先に確認し、拒否される呼び出しでは複製コストを払わない。
        if self.doc.layers.len() <= 1 {
            return;
        }
        let before_active = self.doc.active_index();
        let layer = self.doc.layers[before_active].clone();
        if self.doc.remove_active_layer() {
            self.push_layer_history(HistoryOp::RemoveLayer {
                index: before_active,
                layer,
                before_active,
            });
        }
    }

    fn layer_move_up(&mut self) {
        self.commit_open_gesture();
        let from = self.doc.active_index();
        if self.doc.move_active_layer_up() {
            let to = self.doc.active_index();
            self.push_layer_history(HistoryOp::MoveLayer { from, to });
        }
    }

    fn layer_move_down(&mut self) {
        self.commit_open_gesture();
        let from = self.doc.active_index();
        if self.doc.move_active_layer_down() {
            let to = self.doc.active_index();
            self.push_layer_history(HistoryOp::MoveLayer { from, to });
        }
    }

    fn layer_merge_down(&mut self) {
        self.commit_open_gesture();
        // `Document::merge_active_down` 自身の拒否条件(レイヤー1枚・
        // アクティブが最下位)を先に確認し、拒否される呼び出しでは複製
        // コストを払わない。
        let index = self.doc.active_index();
        if index == 0 || self.doc.layers.len() <= 1 {
            return;
        }
        let upper = self.doc.layers[index].clone();
        let lower_before = self.doc.layers[index - 1].clone();
        if self.doc.merge_active_down() {
            self.push_layer_history(HistoryOp::MergeDown {
                index,
                upper,
                lower_before,
            });
        }
    }

    /// SPEC §13: メニュー「画像の統合」(Ctrl+Shift+E)。複数レイヤーを
    /// 1 枚へ合成する操作は全レイヤーの前後スナップショットが本質的に
    /// 必要なため、ここだけ `ReplaceAll`(`push_replace_all`)を使う
    /// (ARCHITECTURE.md §14.2)。
    fn layer_flatten(&mut self) {
        self.commit_open_gesture();
        if self.doc.layers.len() <= 1 {
            return;
        }
        let before = self.doc.snapshot();
        if self.doc.flatten_all() {
            self.push_replace_all(before);
        }
    }

    /// レイヤーパネルの行クリック(SPEC §13: 「クリックでアクティブ化」)。
    /// アクティブレイヤーの切り替えは履歴に積まないが、進行中のジェスチャは
    /// 「先に確定」してから切り替える(ARCHITECTURE.md §14.9-3: 「浮動片
    /// 保持中にアクティブレイヤーを変えると確定先が変わってしまう」の対策)。
    fn set_active_layer(&mut self, index: usize) {
        if index >= self.doc.layers.len() || index == self.doc.active_index() {
            return;
        }
        self.commit_open_gesture();
        self.doc.active = index;
    }

    /// レイヤーパネルからの操作を配線する。
    fn handle_layers_panel_action(&mut self, action: LayersPanelAction) {
        match action {
            LayersPanelAction::Activate(idx) => self.set_active_layer(idx),
            LayersPanelAction::Add => self.layer_add(),
            LayersPanelAction::Duplicate => self.layer_duplicate(),
            LayersPanelAction::Delete => self.layer_delete(),
            LayersPanelAction::MoveUp => self.layer_move_up(),
            LayersPanelAction::MoveDown => self.layer_move_down(),
            LayersPanelAction::MergeDown => self.layer_merge_down(),
        }
    }

    // -----------------------------------------------------------------
    // メニュー・モーダルのディスパッチ
    // -----------------------------------------------------------------

    fn handle_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::New => self.request_action(PendingAction::New),
            MenuAction::Open => self.request_action(PendingAction::Open(None)),
            MenuAction::Save => self.begin_save(),
            MenuAction::SaveAs => self.begin_save_as(),
            MenuAction::Exit => self.request_action(PendingAction::Close),
            MenuAction::Undo => {
                // SPEC §13 最終項: 浮動片/ストローク進行中は先に確定してから
                // 実行する(`handle_undo_redo_shortcuts` と同じ規則)。
                self.commit_open_gesture();
                self.history.undo(&mut self.doc);
            }
            MenuAction::Redo => {
                self.commit_open_gesture();
                self.history.redo(&mut self.doc);
            }
            MenuAction::Cut => self.cut_selection_to_clipboard(),
            MenuAction::Copy => {
                self.copy_selection_to_clipboard();
            }
            MenuAction::Paste => self.paste_from_clipboard(),
            MenuAction::Delete => self.delete_selection(),
            MenuAction::SelectAll => self.select_all(),
            MenuAction::Deselect => self.commit_selection(),
            MenuAction::ImageResize => {
                self.modal = Some(ModalState::ImageResize {
                    width: self.doc.width,
                    height: self.doc.height,
                    keep_aspect: true,
                    interpolation: Interpolation::Bilinear,
                });
            }
            MenuAction::CanvasResize => {
                self.modal = Some(ModalState::CanvasResize {
                    width: self.doc.width,
                    height: self.doc.height,
                });
            }
            MenuAction::Crop => self.apply_crop_to_selection(),
            MenuAction::FlipHorizontal => self.apply_flip_horizontal(),
            MenuAction::FlipVertical => self.apply_flip_vertical(),
            MenuAction::RotateCw => self.apply_rotate_cw(),
            MenuAction::RotateCcw => self.apply_rotate_ccw(),
            MenuAction::ZoomIn => self.view.zoom_in(),
            MenuAction::ZoomOut => self.view.zoom_out(),
            MenuAction::Zoom100 => self.view.zoom_to_100(),
            MenuAction::FitWindow => self.view.fit_to_window(&self.doc),
            MenuAction::LayerAdd => self.layer_add(),
            MenuAction::LayerDuplicate => self.layer_duplicate(),
            MenuAction::LayerDelete => self.layer_delete(),
            MenuAction::LayerMoveUp => self.layer_move_up(),
            MenuAction::LayerMoveDown => self.layer_move_down(),
            MenuAction::LayerMergeDown => self.layer_merge_down(),
            MenuAction::LayerFlatten => self.layer_flatten(),
        }
    }

    /// 表示中のモーダル(あれば)を描き、確定/キャンセルを処理する。
    fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        // M4 で発見・修正したバグ(`handle_close_request` 参照): このモーダル
        // が表示されている間に閉じる要求が来ていた(`pending_action` に
        // `Close` が予約された)かどうかを、各分岐が `pending_action` を
        // 書き換えるより前に覚えておく(例えば「新規」ダイアログの
        // キャンセルは無条件に `pending_action = None` するため、後から
        // 読み直すと消えてしまう)。
        let close_was_queued = matches!(self.pending_action, Some(PendingAction::Close));
        let mut keep_open = true;
        match &mut modal {
            ModalState::New {
                width,
                height,
                background,
            } => match dialogs::show_new(ctx, width, height, background) {
                DialogOutcome::Confirmed => {
                    self.confirm_new(*width, *height, *background);
                    keep_open = false;
                }
                DialogOutcome::Cancelled => {
                    self.pending_action = None;
                    keep_open = false;
                }
                DialogOutcome::Pending => {}
            },
            ModalState::ImageResize {
                width,
                height,
                keep_aspect,
                interpolation,
            } => {
                let (orig_w, orig_h) = (self.doc.width, self.doc.height);
                match dialogs::show_image_resize(
                    ctx,
                    width,
                    height,
                    keep_aspect,
                    interpolation,
                    orig_w,
                    orig_h,
                ) {
                    DialogOutcome::Confirmed => {
                        self.confirm_image_resize(*width, *height, *interpolation);
                        keep_open = false;
                    }
                    DialogOutcome::Cancelled => keep_open = false,
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::CanvasResize { width, height } => {
                match dialogs::show_canvas_resize(ctx, width, height) {
                    DialogOutcome::Confirmed => {
                        self.confirm_canvas_resize(*width, *height);
                        keep_open = false;
                    }
                    DialogOutcome::Cancelled => keep_open = false,
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::JpegQuality { quality, path } => {
                match dialogs::show_jpeg_quality(ctx, quality) {
                    DialogOutcome::Confirmed => {
                        self.confirm_jpeg_quality(*quality, path.clone());
                        keep_open = false;
                    }
                    DialogOutcome::Cancelled => {
                        self.after_save_action = None;
                        keep_open = false;
                    }
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::ConfirmUnsaved => {
                let label = self.window_doc_label();
                match dialogs::show_confirm_unsaved(ctx, &label) {
                    ConfirmOutcome::Save => {
                        self.confirm_unsaved_save();
                        return;
                    }
                    ConfirmOutcome::Discard => {
                        self.confirm_unsaved_discard();
                        return;
                    }
                    ConfirmOutcome::Cancel => {
                        self.confirm_unsaved_cancel();
                        return;
                    }
                    ConfirmOutcome::Pending => {}
                }
            }
        }
        if keep_open {
            self.modal = Some(modal);
            return;
        }
        // このモーダルはたった今閉じた(ConfirmUnsaved の Save/Discard/
        // Cancel は上のいずれの分岐も `return` 済みなのでここには来ない)。
        self.resume_queued_close_after_modal(close_was_queued);
    }

    /// `show_modal` がモーダルを閉じた直後に呼ぶ。その間に閉じる要求が
    /// 来ていた(`close_was_queued`)なら、未保存確認へ引き継ぐ(SPEC §8)。
    /// 既に未保存変更が無くなっていれば、そのまま閉じる(`CancelClose` を
    /// 既に送ってしまっているため OS の既定動作には戻れず、
    /// `PendingAction::Close` の通常経路(`execute_pending_action`)と同じく
    /// 明示的に終了する必要がある)。`show_modal` から切り出してあるのは、
    /// egui の `Context` を必要とせずユニットテストできるようにするため。
    fn resume_queued_close_after_modal(&mut self, close_was_queued: bool) {
        if !close_was_queued {
            return;
        }
        if self.doc.modified {
            self.modal = Some(ModalState::ConfirmUnsaved);
        } else {
            self.pending_action = None;
            std::process::exit(0);
        }
    }

    // -----------------------------------------------------------------
    // タイトルバー(SPEC §3)
    // -----------------------------------------------------------------

    fn window_doc_label(&self) -> String {
        match &self.doc.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "無題".to_owned()),
            None => "無題".to_owned(),
        }
    }

    /// `{ファイル名}{*} - Darask Paint`(SPEC §3、`*` は未保存変更あり)。
    fn compute_window_title(&self) -> String {
        let star = if self.doc.modified { "*" } else { "" };
        format!("{}{star} - Darask Paint", self.window_doc_label())
    }
}

impl eframe::App for DaraskApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // TEMP DIAGNOSTIC (v2 white-screen regression investigation).
        {
            static FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            eprintln!(
                "[DIAG] ui() frame={n} ppp={} doc={}x{} layers={}",
                ui.ctx().pixels_per_point(),
                self.doc.width,
                self.doc.height,
                self.doc.layers.len()
            );
        }
        // ARCHITECTURE.md §12-9: rfd はブロッキングなので、直前のフレームで
        // 要求されたダイアログはここ(フレーム冒頭、まだパネル/painter を
        // 何も作っていない状態)で処理する。
        self.process_pending_dialog();

        // ARCHITECTURE.md §10 の update() 順序(②close_requested検知
        // ③ショートカット処理④メニュー⑨モーダル)に沿ってレイアウトする。
        // egui のパネルは宣言順で残り領域を確保するため(v2 で右パネルが
        // 増えた際に発見・修正したバグの教訓、下の side_panel::show の
        // コメント参照)、実際のパネル宣言順は
        // メニュー→ステータスバー→ツールバー(左)→右パネル→
        // オプションバー(上)→中央キャンバス、というレイアウト都合の順序に
        // なっている(ARCHITECTURE.md §10 の「⑤⑥」の記述はレイヤーパネル
        // 追加前の v1 の順序であり、egui のパネル確保規則までは規定して
        // いない)。
        self.handle_close_request(ui.ctx());

        self.handle_tool_shortcuts(ui.ctx());
        self.handle_color_and_brush_shortcuts(ui.ctx());
        self.handle_undo_redo_shortcuts(ui.ctx());
        self.handle_selection_shortcuts(ui.ctx());
        self.handle_layer_shortcuts(ui.ctx());
        self.handle_view_shortcuts(ui.ctx());
        self.handle_file_shortcuts(ui.ctx());
        self.handle_dropped_files(ui.ctx());

        let title = self.compute_window_title();
        if title != self.last_title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        let toast_text = self.tick_toast(ui.ctx());

        let layer_count = self.doc.layers.len();
        let active_layer_index = self.doc.active_index();
        let menu_state = MenuState {
            // SPEC §13 最終項: 進行中のストロークがあっても、Undo は「先に
            // 確定してから実行」できるので有効表示にする(has_open_stroke の
            // ときは、確定によって少なくとも 1 件の undo 単位が生まれる)。
            can_undo: self.history.can_undo() || self.history.has_open_stroke(),
            can_redo: self.history.can_redo(),
            has_selection: self.selection.is_some() || self.floating.is_some(),
            can_add_layer: layer_count < MAX_LAYERS,
            can_delete_layer: layer_count > 1,
            can_move_layer_up: active_layer_index + 1 < layer_count,
            can_move_layer_down: active_layer_index > 0,
            can_merge_layer_down: layer_count > 1 && active_layer_index > 0,
            can_flatten_layers: layer_count > 1,
        };
        if let Some(action) = menu::show(ui, &menu_state) {
            self.handle_menu_action(action);
        }

        // ステータスバーはレイアウト順の都合上キャンバスより先に描くため、
        // 表示するカーソル座標/ズームは 1 フレーム前の値になる
        // (ポインタ移動のたびにフレームが駆動されるため実用上は無視できる)。
        status_bar::show(
            ui,
            &self.doc,
            self.view.hover_img(),
            self.view.zoom,
            self.current_selection_size(),
            toast_text.as_deref(),
        );

        if let Some(new_tool) = toolbar::show(ui, self.tool) {
            self.set_tool(new_tool);
        }

        // v2 レビューで発見・修正したバグ: egui のパネルは宣言順に残り領域の
        // 辺を確保する(ARCHITECTURE.md §14.9-7 のコメントどおり)。以前は
        // `options_bar`(top)をここより先に宣言していたため、右パネルが
        // まだ何も予約していない状態でオプションバーがウィンドウ右端まで
        // (本来右パネルが占めるべき領域の上まで)広がってしまい、右パネルは
        // メニュー直下ではなくオプションバーの高さぶん下から始まっていた
        // (SPEC §3 の画面構成図は右パネルをメニュー直下からステータス
        // バー直上まで通しで描いている)。右パネル(right)をオプションバー
        // (top)より先に宣言することで、右パネルはツールバーの右から
        // ウィンドウ右端までの帯を(メニュー直下から通しで)先に確保し、
        // オプションバーはその残り(ツールバーと右パネルの間)だけを使う
        // ようになる。
        let color_ctx = ColorPanelCtx {
            primary: &mut self.primary,
            secondary: &mut self.secondary,
            wheel: &mut self.color_wheel,
            hex_buffer: &mut self.color_hex_buffer,
            recent_colors: &self.recent_colors,
            user_palette: &mut self.user_palette,
        };
        if let Some(action) = side_panel::show(ui, &mut self.doc, &mut self.layer_rename, color_ctx)
        {
            self.handle_layers_panel_action(action);
        }

        {
            // SPEC §3: オプションバーの「ツール固有」は矩形/楕円のときだけ
            // モード選択(枠線のみ/塗りつぶし/両方)を出す。
            let shape_mode = match self.tool {
                ToolKind::Rect => Some(&mut self.rect_tool.mode),
                ToolKind::Ellipse => Some(&mut self.ellipse.mode),
                _ => None,
            };
            options_bar::show(
                ui,
                OptionsBarCtx {
                    tool: self.tool,
                    brush_size: &mut self.brush_size,
                    pen_aa: &mut self.pen.aa,
                    shape_mode,
                    fill_tolerance: &mut self.fill.tolerance,
                },
            );
        }

        let force_pan = self.tool == ToolKind::Pan;
        let cursor = self.cursor_for_active_tool();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(64)))
            .show(ui, |ui| {
                let output = self.view.show(ui, &mut self.doc, force_pan, cursor);
                // ARCHITECTURE.md §3: 市松模様→画像→ツールプレビュー→選択枠の順。
                match self.tool {
                    ToolKind::Pen => self.pen.draw_preview(
                        &output.painter,
                        &self.view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Eraser => self.eraser.draw_preview(
                        &output.painter,
                        &self.view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Line => self.line.draw_preview(
                        &output.painter,
                        &self.view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Rect => self.rect_tool.draw_preview(
                        &output.painter,
                        &self.view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Ellipse => self.ellipse.draw_preview(
                        &output.painter,
                        &self.view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Fill | ToolKind::Picker | ToolKind::Select | ToolKind::Pan => {}
                }
                self.draw_selection_overlay(&output.painter);
                self.dispatch_canvas_events(output.events);
            });

        self.show_modal(ui.ctx());

        // ①ベンチ処理(SPEC §11): 2 フレーム目の描画が終わった時点で
        // bench.txt に経過ミリ秒を書き出し、直ちにプロセスを終了する。
        if let Some(bench) = &mut self.bench {
            bench.frames_drawn += 1;
            if bench.frames_drawn >= 2 {
                let elapsed_ms = bench.process_start.elapsed().as_millis();
                // I/O エラーでパニックしないこと(SPEC §12)。書き込みに
                // 失敗してもスモークテストとしてはプロセスを終了させる。
                let _ = std::fs::write("bench.txt", elapsed_ms.to_string());
                std::process::exit(0);
            }
            // 通常運用では無条件の request_repaint() は禁止(アイドル CPU 0%
            // 要件、ARCHITECTURE.md §3)。ベンチモードは自動終了するまでの
            // 特別な非アイドル区間であり、確実に 2 フレーム目を発生させて
            // スモークテストを決定的にするためだけにここで要求する。
            // DARASK_BENCH=1 のときしか実行されないため、通常運用時の
            // アイドル CPU 0% には影響しない。
            ui.ctx().request_repaint();
        }
    }
}

/// ARCHITECTURE.md §9: egui のデフォルトフォントに日本語グリフは無いため、
/// Windows システムフォントを実行時に読み込んで追加する。
/// `App::new` 相当(ここでは `DaraskApp::new`)で一度だけ呼ぶ。
fn setup_japanese_fonts(ctx: &egui::Context) {
    for path in JAPANESE_FONT_CANDIDATES {
        match std::fs::read(path) {
            Ok(bytes) => {
                ctx.add_font(FontInsert::new(
                    "darask-jp",
                    egui::FontData::from_owned(bytes),
                    vec![
                        InsertFontFamily {
                            family: egui::FontFamily::Proportional,
                            priority: FontPriority::Highest,
                        },
                        InsertFontFamily {
                            family: egui::FontFamily::Monospace,
                            priority: FontPriority::Highest,
                        },
                    ],
                ));
                return;
            }
            Err(_) => continue,
        }
    }

    // ARCHITECTURE.md §9-4: 全部読めなければ警告ログだけ出して続行する
    // (Win11 では起きない想定)。`log` crate は依存に追加しない方針
    // (CLAUDE.md)のため `eprintln!` で代替する。`windows_subsystem = "windows"`
    // によりコンソールが無い環境では単に出力先が失われるだけでパニックはしない。
    eprintln!(
        "警告: 日本語フォントが見つかりませんでした(YuGothM/meiryo/msgothic)。文字が正しく表示されない可能性があります。"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, IRect};

    /// `DaraskApp::new` は `eframe::CreationContext`(フォント読み込みに
    /// egui の実 `Context` を要求)を必要とし、ユニットテストからは構築
    /// できない。`DaraskApp` の各フィールド自体は(フォント設定を除けば)
    /// egui の `Context` を必要とせずに構築できる素の Rust 構造体なので、
    /// テスト専用にフィールドを直接組み立てるコンストラクタを用意する。
    fn new_for_test(doc: Document) -> DaraskApp {
        DaraskApp {
            doc,
            view: CanvasView::new(),
            history: History::new(),
            tool: ToolKind::Pen,
            pen: PenTool::new(),
            eraser: EraserTool::new(),
            line: ShapeTool::new_line(),
            rect_tool: ShapeTool::new_rect(),
            ellipse: ShapeTool::new_ellipse(),
            fill: FillTool::new(),
            picker: PickerTool::new(),
            primary: Color32::BLACK,
            secondary: Color32::WHITE,
            brush_size: 4.0,
            recent_colors: VecDeque::new(),
            alt_eyedropper_active: false,
            color_wheel: ColorWheelState::new(),
            // 起動 1 フレーム目から正しい表記を出す(空文字だと 1 フレーム
            // だけ空欄がちらつく)。プライマリの初期値(黒)に合わせる。
            color_hex_buffer: color_panel::format_hex(Color32::BLACK),
            user_palette: Vec::new(),
            selection: None,
            floating: None,
            select_drag: None,
            next_floating_id: 0,
            modal: None,
            pending_action: None,
            pending_dialog: None,
            after_save_action: None,
            last_jpeg_quality: DEFAULT_JPEG_QUALITY,
            last_title: String::new(),
            toast: None,
            layer_rename: None,
            next_layer_number: 1,
            bench: None,
        }
    }

    // -- 貼り付けが他ツールの begin_stroke に破棄されるバグ(修正済み) ------

    #[test]
    fn begin_paste_floating_switches_tool_to_select() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.doc.modified = true; // 白紙ではない状態を再現する。
        app.tool = ToolKind::Pen;

        app.begin_paste_floating(4, 4, vec![255, 0, 0, 255].repeat(16));

        assert_eq!(
            app.tool,
            ToolKind::Select,
            "paste must switch to Select so a later Pen Down cannot discard the open stroke"
        );
        assert!(app.floating.is_some());
        assert!(app.history.has_open_stroke());
        assert!(
            app.doc.modified,
            "an uncommitted floating paste must already count as an unsaved change"
        );
    }

    #[test]
    fn begin_paste_floating_commit_pushes_a_single_undo_unit() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.doc.modified = true;
        app.tool = ToolKind::Pen;

        app.begin_paste_floating(4, 4, vec![255, 0, 0, 255].repeat(16));
        // ペンでキャンバスをクリックしても(tool は既に Select なので)ペンの
        // begin_stroke には届かず、貼り付け用のレコーダは破棄されない。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(1.0, 1.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke(), "recorder must survive");

        app.commit_selection();
        assert!(
            app.history.can_undo(),
            "committing the pasted floating must push exactly one undo unit"
        );
        assert!(!app.history.has_open_stroke());
    }

    // -- v2 レビューで発見・修正したバグ: 白紙置き換え貼り付けが進行中
    // ストロークを確定せず、stale な CoW タイルから壊れた Patch が
    // 作られる ------------------------------------------------------------

    #[test]
    fn paste_commits_an_open_pen_stroke_before_replacing_a_pristine_document() {
        // 起動直後の白紙(pristine)でペンを押下したまま(=StrokeTool は
        // Down/Drag で画素を書いても doc.modified を立てないため、
        // commit_stroke までは doc_is_pristine() が誤って true のまま)
        // Ctrl+V したときの再現。修正前は `replace_document_with_pasted_
        // image` が先に走ってドキュメントごと差し替わり、開いたままの
        // ペンのストロークレコーダ(旧ドキュメントのタイルを退避したもの)
        // が新ドキュメントに対して壊れた Patch を作っていた。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        assert!(app.doc_is_pristine());

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        // StrokeTool は commit_stroke までは modified を立てないため、
        // 修正前はここでまだ pristine 判定のままだった。
        let painted_stroke = app.doc.get_pixel(5, 5);
        assert_ne!(painted_stroke, Some([255, 255, 255, 255]));

        app.paste_pixels(16, 16, vec![0, 255, 0, 255].repeat(256));

        // 元のペンストロークは、貼り付け用の(まだ未確定の)浮動片とは
        // 独立した undo 単位として、先に確定されているはず。
        assert!(
            app.history.can_undo(),
            "the pen stroke must have been committed as its own undo unit before pasting"
        );
        assert_eq!(
            (app.doc.width, app.doc.height),
            (20, 20),
            "the document must not be replaced while a stroke was open (it is no longer pristine after the commit)"
        );
        assert!(
            app.floating.is_some(),
            "the paste must float onto the existing document instead of replacing it"
        );
        assert!(
            app.history.has_open_stroke(),
            "the paste itself legitimately opens its own separate, not-yet-committed stroke for the pending floating piece"
        );

        // undo を 2 回: ①貼り付け確定 ②ペンストローク。どちらも壊れずに
        // バイト正確に復元できる。
        app.commit_selection(); // Enter 相当で貼り付けを確定する。
        assert!(!app.history.has_open_stroke());
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(
            app.doc.get_pixel(5, 5),
            painted_stroke,
            "undoing the paste must restore the just-drawn pen pixel, not a stale white one"
        );
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.get_pixel(5, 5), Some([255, 255, 255, 255]));
    }

    // -- ドラッグ中のツール切替で進行中ストロークが破棄されるバグ(修正済み) --

    #[test]
    fn switching_tool_mid_drag_commits_partial_pen_stroke() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        let painted = app.doc.get_pixel(5, 5);
        assert_ne!(painted, Some([255, 255, 255, 255]));

        app.set_tool(ToolKind::Eraser);

        assert!(
            !app.history.has_open_stroke(),
            "switching tools must commit the in-progress stroke, not discard it"
        );
        assert!(app.history.can_undo());
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.get_pixel(5, 5), Some([255, 255, 255, 255]));
        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc.get_pixel(5, 5), painted);
    }

    // -- SPEC §13 最終項(v2 で修正): 進行中のストローク中の undo/redo は
    // 「ツール切替と同じ扱い(先に確定してから実行)」であるべき。以前は
    // `can_undo_redo_now()` で undo/redo を丸ごとブロックしていた(浮動片
    // 保持中に Ctrl+Z を押しても「何も起きない」ように見えていた)。
    // 先に確定してから undo/redo するようにしたので、進行中のストロークは
    // 「1 つの undo 単位として確定され、直後にそれ自身が取り消される」
    // (実質キャンセル相当)という挙動になる。 -----------------------------

    #[test]
    fn handle_menu_action_undo_commits_an_open_stroke_first_then_undoes_it() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;

        // ストローク1: 完全に描いて確定する(undo スタックに 1 件積む)。
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(2.0, 2.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(2.0, 2.0),
                button: PointerButton::Primary,
            },
        ]);
        assert!(app.history.can_undo());
        assert!(!app.history.has_open_stroke());
        let painted_stroke1 = app.doc.get_pixel(2, 2);
        assert_ne!(painted_stroke1, Some([255, 255, 255, 255]));

        // ストローク2: Down だけ送ってストロークを開いたままにする。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        let painted_stroke2 = app.doc.get_pixel(10, 10);
        assert_ne!(painted_stroke2, Some([255, 255, 255, 255]));

        app.handle_menu_action(MenuAction::Undo);

        assert!(
            !app.history.has_open_stroke(),
            "undo must commit the open stroke first (same as switching tools)"
        );
        // ストローク2 は確定直後に取り消されるので消えている。
        assert_eq!(app.doc.get_pixel(10, 10), Some([255, 255, 255, 255]));
        // ストローク1 は無傷のまま残る。
        assert_eq!(app.doc.get_pixel(2, 2), painted_stroke1);
        assert!(
            app.history.can_undo(),
            "stroke1 must remain on the undo stack"
        );
        assert!(
            app.history.can_redo(),
            "the just-undone stroke2 must be on the redo stack"
        );
    }

    #[test]
    fn handle_menu_action_redo_commits_an_open_stroke_first() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(2.0, 2.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(2.0, 2.0),
                button: PointerButton::Primary,
            },
        ]);
        app.handle_menu_action(MenuAction::Undo); // stroke1 -> redo スタックへ。
        assert!(app.history.can_redo());

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        let painted_stroke2 = app.doc.get_pixel(10, 10);

        app.handle_menu_action(MenuAction::Redo);

        assert!(
            !app.history.has_open_stroke(),
            "redo must commit the open stroke first (same as switching tools)"
        );
        // ストローク2 の確定が新規 push なので redo スタック(stroke1)は
        // クリアされ、この redo 呼び出し自体は何もしない(no-op)。
        assert!(
            !app.history.can_redo(),
            "committing stroke2 must have cleared the redo stack"
        );
        assert_eq!(app.doc.get_pixel(10, 10), painted_stroke2);
    }

    // -- v2 レビューで発見・修正したバグ: 選択ツールの単クリック(ドラッグ
    // なし)で 1×1 の選択が生成される --------------------------------------

    #[test]
    fn single_click_with_select_tool_does_not_create_a_1x1_selection() {
        // `irect_from_points` は floor/ceil で外側に丸めるため、画像座標が
        // 非整数(高 DPI スケーリングや 100% 以外のズームでは頻繁に起こる)
        // だと、ドラッグなしの単クリックでも幅・高さ 1 の非空矩形が残って
        // いた(SPEC §6: 「ドラッグで矩形選択」)。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;

        let click_pos = pos2(5.3, 7.8); // 非整数座標。
        app.handle_select_event(ToolEvent::Down {
            img: click_pos,
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Up {
            img: click_pos,
            button: PointerButton::Primary,
        });

        assert!(
            app.selection.is_none(),
            "a plain click (no drag) must not leave behind a 1x1 selection"
        );
    }

    #[test]
    fn dragging_with_select_tool_still_creates_a_selection() {
        // 上のテストの反例: 実際にドラッグした場合は従来どおり選択される
        // (単クリック対策が過剰に効いていないことの確認)。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;

        app.handle_select_event(ToolEvent::Down {
            img: pos2(2.0, 2.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Up {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
        });

        let selection = app.selection.expect("a real drag must create a selection");
        assert_eq!(
            (
                selection.rect.x0,
                selection.rect.y0,
                selection.rect.x1,
                selection.rect.y1
            ),
            (2, 2, 10, 10)
        );
    }

    // -- 選択内部の単クリックが無意味な undo エントリを積むバグ(修正済み) --

    #[test]
    fn clicking_inside_selection_without_dragging_does_not_float_or_push_undo() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        // v2 §16: 選択の外周には約 7pt 角のスケールハンドルが乗るため、
        // 「内部クリック」を検証するにはハンドルの当たり判定(中心から
        // ±3.5pt)から十分離れた点でクリックする必要がある(16x16 の選択で
        // 中心 (10,10) を使えば、どの辺・角ハンドルからも 8pt 以上離れる)。
        app.selection = Some(Selection {
            rect: IRect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18,
            },
        });

        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(
            app.floating.is_none(),
            "a plain click must not float the selection yet"
        );

        app.handle_select_event(ToolEvent::Up {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
        });
        assert!(app.floating.is_none());
        assert!(
            app.selection.is_some(),
            "selection should remain after a no-op click"
        );
        assert!(
            !app.history.can_undo(),
            "a click without drag must not push an undo entry"
        );
    }

    #[test]
    fn dragging_inside_selection_floats_it_and_tracks_the_pointer() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        // v2 §16: 上のテストと同じ理由で、ハンドルの当たり判定を避けた中心
        // 付近でドラッグする(rect 原点 (2,2) からの down オフセットは (8,8)
        // で、旧テストの (3,3) と役割は同じ)。
        app.selection = Some(Selection {
            rect: IRect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18,
            },
        });

        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(13.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        let floating = app
            .floating
            .as_ref()
            .expect("an actual drag must float the selection");
        assert_eq!(floating.pos, pos2(5.0, 2.0));
    }

    // -- v2 レビューで発見・修正したバグ: Ctrl+A→Delete/Ctrl+X が開いた
    // ストロークのレコーダを begin_stroke で黙って置換し、以降のドラッグ
    // 描画が undo 不能になる -----------------------------------------------

    #[test]
    fn delete_selection_commits_an_open_pen_stroke_first_instead_of_clobbering_it() {
        // Ctrl+A はツールを問わず選択を作れるため、ペンツールでドラッグ中に
        // Delete/Ctrl+X を押すと、修正前は delete_selection の
        // `history.begin_stroke` が進行中のペンストロークのレコーダを
        // 無警告で置換していた(SPEC §9「1 ストローク = 1 undo 単位」違反、
        // 以降のドラッグが undo 不能になる)。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.select_all();
        assert!(app.selection.is_some());
        assert_eq!(app.tool, ToolKind::Pen, "select_all must not switch tools");

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        let painted_stroke = app.doc.get_pixel(5, 5);
        assert_ne!(painted_stroke, Some([255, 255, 255, 255]));

        app.delete_selection();

        assert!(
            !app.history.has_open_stroke(),
            "the pen stroke must have been committed as its own undo unit"
        );
        assert!(
            app.selection.is_none(),
            "the full-canvas selection must have been deleted (made transparent)"
        );
        // 削除パッチは全面透明化のはず(選択の消去、SPEC §6)。
        assert_eq!(app.doc.get_pixel(0, 0), Some([0, 0, 0, 0]));

        // 新しいストローク(Down+Up)を描いても、正常な(壊れていない)
        // undo 単位として記録される(`Tool::cancel` はストローク確定時に
        // `StrokeTool` の内部状態をリセットするため、ここは新しい Down から
        // 始める)。
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(10.0, 10.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(10.0, 10.0),
                button: PointerButton::Primary,
            },
        ]);
        let painted_after_delete = app.doc.get_pixel(10, 10);
        assert_ne!(
            painted_after_delete,
            Some([0, 0, 0, 0]),
            "a fresh stroke after delete must actually draw, proving StrokeTool's state was not corrupted"
        );

        // undo 3 回: ③新ストローク ②選択削除 ①ペンの最初のストローク、で
        // それぞれバイト正確に復元できる(ストロークが破損したパッチに
        // 焼き込まれたり、未確定のまま残ったりしていない)。
        assert!(app.history.undo(&mut app.doc));
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(
            app.doc.get_pixel(5, 5),
            painted_stroke,
            "undoing the delete must restore the pen dot painted just before Delete was pressed"
        );
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.get_pixel(5, 5), Some([255, 255, 255, 255]));
    }

    // -- Ctrl+X がコピー失敗時にも削除してしまうバグ(修正済み) -------------

    #[test]
    fn cut_does_not_delete_when_clipboard_copy_fails() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        // 幅/高さ 0 は io::copy_image_to_clipboard が OS クリップボードに
        // 触れる前に決定的に失敗させる(ARCHITECTURE.md §12-8)ので、実際の
        // OS クリップボード状態に依存せずにこの経路をテストできる。
        app.floating = Some(Floating::new(vec![], 0, 0, pos2(0.0, 0.0), None, 1));

        app.cut_selection_to_clipboard();

        assert!(
            app.floating.is_some(),
            "cut must not delete the selection when the clipboard copy failed"
        );
    }

    // -- GIF/WebP への上書き保存が拡張子を補正しないバグ(修正済み) ---------

    #[test]
    fn begin_save_to_path_corrects_unsupported_extension_to_png() {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_savepath_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let gif_path = dir.join("photo.gif");

        let mut app = new_for_test(Document::new(2, 2, Background::White));
        app.begin_save_to_path(gif_path.clone());

        let png_path = dir.join("photo.png");
        assert_eq!(
            app.doc.path,
            Some(png_path.clone()),
            "saving to an unsupported extension must redirect to .png instead of writing PNG bytes under .gif"
        );
        assert!(png_path.exists());
        assert!(!gif_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- モーダル表示中の閉じる要求が握りつぶされるバグ(修正済み) -----------

    #[test]
    fn resume_queued_close_after_modal_reopens_as_confirm_unsaved_when_still_modified() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.doc.modified = true;
        app.modal = None; // CanvasResize 等、直前のモーダルが閉じた直後を再現。

        app.resume_queued_close_after_modal(true);

        assert!(
            matches!(app.modal, Some(ModalState::ConfirmUnsaved)),
            "a close request queued while another modal was open must not be dropped"
        );
    }

    #[test]
    fn resume_queued_close_after_modal_does_nothing_when_no_close_was_queued() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.doc.modified = true;

        app.resume_queued_close_after_modal(false);

        assert!(app.modal.is_none());
    }

    // -- v2 §13: レイヤー操作(ARCHITECTURE.md §14.8 V2-M2 受け入れ基準) -----

    #[test]
    fn layer_add_inserts_a_new_layer_as_a_single_undo_unit() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.layer_add();
        assert_eq!(app.doc.layers.len(), 2);
        assert_eq!(app.doc.layers[1].name, "レイヤー 1");
        assert!(app.history.can_undo());

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 1);
        assert!(app.history.can_redo());
        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 2);
    }

    #[test]
    fn layer_add_names_increment_regardless_of_deletions() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_add();
        assert_eq!(app.doc.layers[1].name, "レイヤー 1");
        app.layer_delete();
        app.layer_add();
        assert_eq!(app.doc.layers[1].name, "レイヤー 2");
    }

    #[test]
    fn layer_delete_and_merge_down_are_no_ops_with_a_single_layer() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_delete();
        assert_eq!(
            app.doc.layers.len(),
            1,
            "must refuse to delete the last layer"
        );
        assert!(!app.history.can_undo(), "a refused op must not push undo");

        app.layer_merge_down();
        assert_eq!(app.doc.layers.len(), 1);
        assert!(!app.history.can_undo());
    }

    #[test]
    fn layer_add_refuses_past_the_64_layer_cap() {
        let mut app = new_for_test(Document::new(1, 1, Background::White));
        for _ in 0..(MAX_LAYERS - 1) {
            app.layer_add();
        }
        assert_eq!(app.doc.layers.len(), MAX_LAYERS);

        app.layer_add(); // 上限到達、拒否されるはず。
        assert_eq!(
            app.doc.layers.len(),
            MAX_LAYERS,
            "must refuse to exceed MAX_LAYERS"
        );

        // 上限到達で拒否された呼び出しは undo エントリを積まない: ちょうど
        // MAX_LAYERS - 1 回の undo で元の 1 枚まで戻り、それ以上は戻せない。
        for _ in 0..(MAX_LAYERS - 1) {
            assert!(app.history.undo(&mut app.doc));
        }
        assert_eq!(app.doc.layers.len(), 1);
        assert!(
            !app.history.undo(&mut app.doc),
            "the refused add must not have pushed an undo entry"
        );
    }

    // -- v2 レビューで発見・修正したバグ: レイヤー構造操作が全て ReplaceAll
    // (全レイヤー×before/after の全画素スナップショット)で、
    // ARCHITECTURE.md §14.2 の軽量 op(AddLayer/MoveLayer/…)が未実装
    // だった。大きめ・多レイヤーのドキュメントで「新規レイヤー」を 1 回
    // 押しただけで履歴が全レイヤー×2 分のメモリを消費し、256MB 上限を
    // 単独の op で超過して直近 10 件を除く undo 履歴が丸ごと破棄されて
    // いた。--------------------------------------------------------------

    #[test]
    fn layer_add_history_stays_within_memory_limit_for_many_layers_on_a_large_document() {
        // 250×250(1 層 250,000 バイト)に 40 層追加する。旧実装(全レイヤー
        // ×2 の ReplaceAll)なら合計はおよそ Σ(2i+1)*250,000 ≈ 420MB (i=1..40)
        // となり 256MB 上限を大きく超え、`push` が最古から破棄するため
        // 40 件のうち一部しか undo できなくなる。軽量な `AddLayer`(名前の
        // 文字列だけを保持)なら 40 件合計でも無視できるサイズで、すべて
        // undo できるはず。
        let mut app = new_for_test(Document::new(250, 250, Background::White));
        for _ in 0..40 {
            app.layer_add();
        }
        assert_eq!(app.doc.layers.len(), 41);
        for i in 0..40 {
            assert!(
                app.history.undo(&mut app.doc),
                "AddLayer entry #{i} must not have been evicted by the 256MB history limit"
            );
        }
        assert_eq!(app.doc.layers.len(), 1);
    }

    #[test]
    fn layer_duplicate_history_round_trips_via_app() {
        let mut app = new_for_test(Document::new(4, 4, Background::Transparent));
        app.doc.set_pixel(0, 0, [5, 6, 7, 255]);
        app.layer_duplicate();
        assert_eq!(app.doc.layers.len(), 2);
        assert_eq!(app.doc.layers[1].pixels[0..4], [5, 6, 7, 255]);

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 1);

        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 2);
        assert_eq!(app.doc.layers[1].pixels[0..4], [5, 6, 7, 255]);
    }

    #[test]
    fn layer_move_up_and_down_history_round_trip_via_app() {
        let mut app = new_for_test(Document::new(2, 2, Background::White));
        app.doc.layers[0].name = "下".to_owned();
        app.layer_add();
        app.doc.layers[1].name = "上".to_owned();
        app.layer_move_down();
        assert_eq!(app.doc.layers[0].name, "上");
        assert_eq!(app.doc.active, 0);

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.layers[0].name, "下");
        assert_eq!(app.doc.active, 1);

        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc.layers[0].name, "上");
        assert_eq!(app.doc.active, 0);
    }

    #[test]
    fn layer_merge_down_history_round_trips_via_app() {
        let mut app = new_for_test(Document::new(1, 1, Background::Transparent));
        app.doc.layers[0] = crate::document::Layer::filled("下", 1, 1, [255, 255, 255, 255]);
        app.layer_add();
        app.doc.layers[1] = crate::document::Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        app.doc.layers[1].opacity = 128;

        app.layer_merge_down();
        assert_eq!(app.doc.layers.len(), 1);
        let merged = app.doc.layers[0].pixels.clone();

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 2);
        assert_eq!(app.doc.layers[1].opacity, 128);

        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc.layers.len(), 1);
        assert_eq!(app.doc.layers[0].pixels, merged);
    }

    // -- ARCHITECTURE.md §14.9-3: レイヤー操作は浮動片/ストローク進行中に
    // 「先に確定」してから実行すること -------------------------------------

    #[test]
    fn layer_add_commits_an_open_pen_stroke_first() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.history.has_open_stroke());
        let painted = app.doc.get_pixel(5, 5);
        assert_ne!(painted, Some([255, 255, 255, 255]));

        app.layer_add();

        assert!(
            !app.history.has_open_stroke(),
            "the in-progress stroke must be committed before the layer is added"
        );
        // 2 つの undo 単位が積まれているはず: ①ペンストローク ②レイヤー追加。
        assert!(app.history.undo(&mut app.doc)); // レイヤー追加を取り消す
        assert_eq!(app.doc.layers.len(), 1);
        assert!(app.history.undo(&mut app.doc)); // ストロークを取り消す
        assert_eq!(app.doc.get_pixel(5, 5), Some([255, 255, 255, 255]));
    }

    #[test]
    fn layer_add_commits_an_open_floating_selection_first() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        // v2 §16: ハンドルの当たり判定を避けて内部ドラッグを起こす
        // (上の `dragging_inside_selection_floats_it_and_tracks_the_pointer`
        // と同じ理由)。
        app.selection = Some(Selection {
            rect: IRect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18,
            },
        });
        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(13.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(
            app.floating.is_some(),
            "drag must have floated the selection"
        );

        app.layer_add();

        assert!(
            app.floating.is_none(),
            "the floating piece must be committed before the layer is added"
        );
        assert_eq!(app.doc.layers.len(), 2);
    }

    #[test]
    fn set_active_layer_commits_open_floating_before_switching() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.layer_add();
        assert_eq!(app.doc.active, 1);

        app.tool = ToolKind::Select;
        // v2 §16: 同上、ハンドルの当たり判定を避ける。
        app.selection = Some(Selection {
            rect: IRect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18,
            },
        });
        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(13.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(app.floating.is_some());

        app.set_active_layer(0);

        assert!(
            app.floating.is_none(),
            "switching the active layer must commit the floating piece to the previously active layer"
        );
        assert_eq!(app.doc.active, 0);
    }

    #[test]
    fn set_active_layer_does_not_push_undo() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        // 履歴を経由せず直接レイヤーを増やし、切り替え自体が undo 単位に
        // ならないことだけを検証する(`layer_add` 自体の undo は別テスト
        // `layer_add_inserts_a_new_layer_as_a_single_undo_unit` で担保済み)。
        app.doc
            .layers
            .push(crate::document::Layer::filled("上", 10, 10, [0, 0, 0, 0]));
        assert!(!app.history.can_undo());

        app.set_active_layer(1);
        assert_eq!(app.doc.active, 1);
        assert!(
            !app.history.can_undo(),
            "switching the active layer must not be a history op (SPEC §13)"
        );
    }

    #[test]
    fn layers_panel_action_dispatch_wires_through_to_document() {
        let mut app = new_for_test(Document::new(6, 6, Background::White));
        app.handle_layers_panel_action(LayersPanelAction::Add);
        assert_eq!(app.doc.layers.len(), 2);
        app.handle_layers_panel_action(LayersPanelAction::Activate(0));
        assert_eq!(app.doc.active, 0);
        app.handle_layers_panel_action(LayersPanelAction::MoveUp);
        assert_eq!(app.doc.active, 1);
        app.handle_layers_panel_action(LayersPanelAction::Duplicate);
        assert_eq!(app.doc.layers.len(), 3);
        app.handle_layers_panel_action(LayersPanelAction::MergeDown);
        assert_eq!(app.doc.layers.len(), 2);
        app.handle_layers_panel_action(LayersPanelAction::Delete);
        assert_eq!(app.doc.layers.len(), 1);
    }

    #[test]
    fn saving_a_multi_layer_document_shows_a_flatten_toast() {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_layer_save_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("multi.png");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_add();
        app.begin_save_to_path(path.clone());

        assert!(
            app.toast.is_some(),
            "saving a multi-layer document must show a toast"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_single_layer_document_shows_no_toast() {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_single_layer_save_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("single.png");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.begin_save_to_path(path.clone());

        assert!(app.toast.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- v2 §16: スケールハンドル(ARCHITECTURE.md §14.6 受け入れ基準) -------
    //
    // `new_for_test` の `CanvasView::new()` は zoom=1.0/pan=0/ppp=1.0/
    // viewport.min=(0,0) のままなので、画像座標とスクリーン座標が一致する
    // (`hit_resize_handle` の当たり判定を素直な数値で検証できる)。

    #[test]
    fn dragging_a_handle_on_an_already_floating_piece_resizes_it() {
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.floating = Some(Floating::new(
            vec![255, 0, 0, 255].repeat(100), // 10x10 の不透明赤
            10,
            10,
            pos2(5.0, 5.0),
            None,
            999,
        ));

        // BottomRight ハンドルは floating の右下 (15,15) にある。
        app.handle_select_event(ToolEvent::Down {
            img: pos2(15.0, 15.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(
            matches!(
                app.select_drag,
                Some(SelectDrag::ResizeFloating {
                    handle: select::Handle::BottomRight,
                    ..
                })
            ),
            "grabbing the handle must start a resize drag, not a move"
        );

        app.handle_select_event(ToolEvent::Drag {
            img: pos2(25.0, 25.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        let floating = app.floating.as_ref().expect("still floating");
        // 左上(反対側の角)は固定されたまま、右下がポインタに追従して 20x20 に拡大する。
        assert_eq!(floating.pos, pos2(5.0, 5.0));
        assert_eq!((floating.w, floating.h), (20, 20));
        assert_ne!(floating.id, 999, "resizing must assign a new texture id");
        assert_eq!(floating.pixels.len(), 20 * 20 * 4);
        assert!(
            floating
                .pixels
                .chunks_exact(4)
                .all(|p| p == [255, 0, 0, 255]),
            "bilinear resample of a flat color must stay flat"
        );
        // 拡縮は常に浮動化時の元ピクセルから再サンプリングする(累積劣化
        // させない、ARCHITECTURE.md §14.6): original は変わらない。
        assert_eq!(floating.original.len(), 10 * 10 * 4);
        assert_eq!((floating.orig_w, floating.orig_h), (10, 10));
    }

    #[test]
    fn grabbing_a_handle_on_an_unfloated_selection_floats_it_first_then_resizes() {
        // SPEC §16: 「未浮動の選択でハンドルを掴んだ場合は、内部ドラッグと
        // 同様にまず浮動化してから拡縮する」。
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.selection = Some(Selection {
            rect: IRect {
                x0: 5,
                y0: 5,
                x1: 15,
                y1: 15,
            },
        });

        app.handle_select_event(ToolEvent::Down {
            img: pos2(15.0, 15.0), // BottomRight ハンドル。
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        assert!(
            app.floating.is_some(),
            "grabbing a handle on a plain selection must float it first"
        );
        assert!(app.selection.is_none());
        assert!(matches!(
            app.select_drag,
            Some(SelectDrag::ResizeFloating {
                handle: select::Handle::BottomRight,
                ..
            })
        ));

        app.handle_select_event(ToolEvent::Drag {
            img: pos2(25.0, 25.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        let floating = app.floating.as_ref().expect("still floating");
        assert_eq!(floating.pos, pos2(5.0, 5.0));
        assert_eq!((floating.w, floating.h), (20, 20));
    }

    #[test]
    fn shift_held_while_dragging_a_handle_locks_the_aspect_ratio() {
        let mut app = new_for_test(Document::new(100, 100, Background::White));
        app.tool = ToolKind::Select;
        app.floating = Some(Floating::new(
            vec![0, 0, 0, 255].repeat(200), // 10x20
            10,
            20,
            pos2(0.0, 0.0),
            None,
            1,
        ));

        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 20.0), // BottomRight ハンドル。
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(50.0, 20.0), // 幅だけ大きく伸ばそうとする。
            button: PointerButton::Primary,
            mods: Modifiers::SHIFT,
        });

        let floating = app.floating.as_ref().expect("still floating");
        // 元の比率は 10:20 = 1:2。Shift でこの比率が保たれるはず。
        let ratio = floating.w as f32 / floating.h as f32;
        assert!(
            (ratio - 0.5).abs() < 0.05,
            "expected ~1:2 aspect ratio, got {}x{}",
            floating.w,
            floating.h
        );
    }

    #[test]
    fn hit_resize_handle_detects_the_handle_under_a_plain_selection() {
        // `select_cursor`(SPEC §16: 「ハンドルホバー時はリサイズカーソルを
        // 表示」)は `self.view.hover_img()` 経由でこの判定を使う。
        // `hover_img` はキャンバス上のポインタ移動(`CanvasView::show`、
        // egui::Context 必須)でしか更新できないため、ここではその下位関数
        // `hit_resize_handle`/`handle_cursor` を直接検証する。
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.selection = Some(Selection {
            rect: IRect {
                x0: 5,
                y0: 5,
                x1: 15,
                y1: 15,
            },
        });
        assert_eq!(
            app.hit_resize_handle(pos2(15.0, 15.0)),
            Some(select::Handle::BottomRight)
        );
        assert_eq!(
            select::handle_cursor(select::Handle::BottomRight),
            egui::CursorIcon::ResizeNwSe
        );
        // 内部(ハンドルから十分離れた点)ではハンドル判定に掛からない。
        assert_eq!(app.hit_resize_handle(pos2(10.0, 10.0)), None);
    }
}
