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
//!
//! v3 V3-M1(SPEC §17、ARCHITECTURE.md §15.5)でブラシ(旧ペン)/消しゴムを
//! 共通のストロークエンジン(`tools/brush.rs`)に刷新した: 硬さ・不透明度
//! (消しゴムは「強さ」)・鉛筆モード・Shift+クリック連結・ブラシ円カーソル・
//! 数字キーでの不透明度設定。旧「アンチエイリアス」チェックボックスは廃止
//! (ブラシは常時 AA になった)。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{pos2, Color32, Key, KeyboardShortcut, Modifiers, PointerButton, Pos2};

use crate::canvas_view::CanvasView;
use crate::document::{Background, Document, Interpolation, Layer, MAX_LAYERS};
use crate::history::{History, HistoryOp};
use crate::io::{self, SaveFormat};
use crate::keymap::{self, Action};
use crate::raster;
use crate::settings::{self, Settings};
use crate::text;
use crate::tools::color_to_straight_rgba;
use crate::tools::eraser::EraserTool;
use crate::tools::fill::FillTool;
use crate::tools::gradient::GradientTool;
use crate::tools::pen::PenTool;
use crate::tools::picker::PickerTool;
use crate::tools::select::{self, Floating, Selection};
use crate::tools::shapes::{ShapeMode, ShapeTool};
use crate::tools::{LassoMode, Tool, ToolCtx, ToolEvent, ToolKind};
use crate::ui::color_panel::{self, ColorPanelCtx};
use crate::ui::color_wheel::ColorWheelState;
use crate::ui::dialogs::{ConfirmOutcome, DialogOutcome};
use crate::ui::layers_panel::{LayersPanelAction, RenameState};
use crate::ui::menu::{MenuAction, MenuState};
use crate::ui::options_bar::OptionsBarCtx;
use crate::ui::tab_bar::{self, TabBarAction, TabInfo};
use crate::ui::toolbar::{self, ToolbarAction};
use crate::ui::{dialogs, menu, options_bar, side_panel, status_bar};

/// SPEC §5: 最近使った色は最大 8 個。
const MAX_RECENT_COLORS: usize = 8;

/// SPEC §4: ブラシサイズは 1–64px。
const MIN_BRUSH_SIZE: f32 = 1.0;
const MAX_BRUSH_SIZE: f32 = 64.0;

/// SPEC §17: 硬さ 0–100%(デフォルト値は `settings::DEFAULT_BRUSH_HARDNESS`
/// — v4 §26 で永続化対象になったため、既定値の情報源は `settings.rs` に
/// 一本化した)。
const MIN_BRUSH_HARDNESS: u8 = 0;
const MAX_BRUSH_HARDNESS: u8 = 100;
/// SPEC §17: 「Shift+[ / Shift+] で ±10」。
const HARDNESS_STEP: u8 = 10;

/// SPEC §17: 不透明度 1–100%(消しゴムは「強さ」として表示。既定値は
/// `settings::DEFAULT_BRUSH_OPACITY`、上と同じ理由)。
const MIN_BRUSH_OPACITY: u8 = 1;
const MAX_BRUSH_OPACITY: u8 = 100;

/// SPEC §8: トーストは約 4 秒表示する。
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// v4 §22: 多角形なげなわの「始点クリックで閉じる」判定距離(スクリーン
/// 論理ポイント。SPEC §16 のハンドルサイズ(7pt)と同程度の当たり判定)。
const LASSO_CLOSE_DISTANCE: f32 = 8.0;
/// v4 §22: 多角形なげなわの「ダブルクリックで閉じる」判定時間窓。
const LASSO_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);
/// v4 §22: ダブルクリック判定の距離しきい値。`LASSO_CLOSE_DISTANCE`
/// (始点クリック用、狙って当てる操作なので少し広め)とは別に、こちらは
/// 「ほぼ同じ位置を素早く 2 回クリックした」ことを狙う小さめの値にする —
/// 広すぎると、細かい頂点を素早く連続でクリックして多角形を描く通常操作
/// (隣り合う頂点同士がこの距離より近いことは普通にありうる)を誤ってダブル
/// クリックと判定し、意図せず多角形を閉じてしまう。
const LASSO_DOUBLE_CLICK_DISTANCE: f32 = 4.0;

/// SPEC §7: 「新規」ダイアログのデフォルト値。
const DEFAULT_NEW_WIDTH: u32 = 1280;
const DEFAULT_NEW_HEIGHT: u32 = 720;

/// SPEC §30: 「タブ数の上限は 24」。超えて新規タブを作ろうとしたら作成せず
/// トースト通知する(`tab_limit_toast_message`/`open_new_tab` 呼び出し元参照)。
const MAX_TABS: usize = 24;

/// SPEC §8: JPEG 品質のデフォルト値。
const DEFAULT_JPEG_QUALITY: u8 = 90;

/// SPEC §26: 「ヘルプ > バージョン情報」に表示するリポジトリ URL。
/// `Cargo.toml` の `repository` フィールドが単一情報源。
const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// SPEC §19: フォントサイズ 8–144px(デフォルト 24。範囲そのものは
/// `ui/options_bar.rs` のスライダーに直接持たせている、`brush_size`
/// (1.0..=64.0)と同じ流儀)。日本語フォントの探索順自体(ARCHITECTURE.md §9)
/// は `text::JAPANESE_FONT_CANDIDATES` に一本化した(UI 表示用フォント読み
/// 込みとテキストツールのラスタライズが同じファイルを使う、SPEC §19:
/// 「フォントは UI と同じシステム日本語フォント」)。
const DEFAULT_TEXT_FONT_SIZE: f32 = 24.0;

/// テキスト編集オーバーレイのプレビュー表示サイズ(論理ポイント)の上下限
/// (ARCHITECTURE.md §15.3: 「表示フォントサイズ ≈ size × zoom / ppp
/// (プレビューは近似で可、上限あり)」)。下限は極端なズームアウトでも
/// 編集操作ができるように、上限は極端なズームインで UI を圧迫しないように。
const TEXT_PREVIEW_MIN_PX: f32 = 6.0;
const TEXT_PREVIEW_MAX_PX: f32 = 200.0;

/// DARASK_BENCH=1 のときのみ存在する、起動計測用の状態(SPEC §11、
/// v4 §16.2: フェーズ内訳)。
struct BenchState {
    /// `main()` 冒頭で取得した `Instant`(プロセス起動からの経過測定用)。
    process_start: Instant,
    /// これまでに `ui()` が呼ばれ描画された回数。
    frames_drawn: u32,
    /// v4 §16.2: `bench.txt` に書き出すフェーズ内訳。
    /// `(ラベル, process_start からの経過ミリ秒)` を、到達した順に積む。
    /// `DaraskApp::new` が「window」(ウィンドウ/GL コンテキスト作成完了
    /// ≈ `new()` 開始時点)・「font」(フォント読込完了)・「app_new」
    /// (`new()` 完了)を積み、`update()` が「first_frame」・「second_frame」
    /// を追加する。
    phases: Vec<(&'static str, u128)>,
}

/// 起動直後 1 回だけ実行するウィンドウの微小リサイズ(白画面ワークアラウンド)。
///
/// eframe 0.35 のネイティブ(glow)バックエンドは「ウィンドウを非表示で
/// 作成 → 初回フレームを描画 → `set_visible(true)` → swap_buffers」という
/// 順序で起動時の白フラッシュを避けている(eframe
/// `glow_integration.rs` の `with_visible(false)` と
/// `EpiIntegration::post_rendering` 参照)。ところが Windows + NVIDIA 環境
/// ではこの「表示直後の初回 present」が DWM のウィンドウ合成準備と競合
/// することがあり、負けると DWM が初期状態の真っ白なサーフェスを保持した
/// まま以後の present を一切反映しなくなる(タイトルバーは正常・プロセスは
/// アイドルで健在・クライアント領域だけ真っ白)。この状態は追加の再描画・
/// `InvalidateRect`・`SetForegroundWindow` では直らず、**ウィンドウの
/// リサイズ(DWM がウィンドウサーフェスを作り直す操作)でのみ回復する**
/// ことを実機の計装で確認済み。発生はタイミング依存(間欠的)。
///
/// そこで起動から約 300ms 後(初回フレームの提示とウィンドウ表示が確実に
/// 完了した後)に内寸を +1pt → 100ms 後に元へ戻す、という 1 往復だけの
/// リサイズを送って DWM にサーフェスを確実に作り直させる。ユーザーには
/// 右端が 1〜2 物理ピクセルだけ一瞬伸縮するだけで、実質知覚されない。
///
/// 再描画ポリシー(ARCHITECTURE.md §3: 無条件 `request_repaint()` 禁止)に
/// ついて: 本ワークアラウンドが要求する追加フレームは起動後最初の約 400ms
/// 間の高々 2〜3 回のみで、`Done` に達した後は一切何もしない(恒久ループ
/// なし・アイドル CPU 0% 要件は不変)。
enum StartupNudge {
    /// 期限が来たら +1pt のリサイズを送る。
    Pending { deadline: Instant },
    /// +1pt を送った。期限が来たら元の内寸 `size`(ポイント)へ戻す。
    Restore { deadline: Instant, size: egui::Vec2 },
    /// 完了(以後は何もしない)。
    Done,
}

/// 起動からリサイズ実行までの待ち時間。初回フレーム提示より確実に後に
/// なるよう十分長く、かつ起動体感を損なわない値。
const STARTUP_NUDGE_DELAY: Duration = Duration::from_millis(300);
/// +1pt してから元寸法へ戻すまでの待ち時間。
const STARTUP_NUDGE_RESTORE_DELAY: Duration = Duration::from_millis(100);

/// 選択ツールの進行中ドラッグ(ARCHITECTURE.md §7)。`Selection`/`Floating`
/// 自体は複数フレームにまたがって保持する必要があるため `DaraskApp` の
/// フィールドとして直接持つ(ARCHITECTURE.md §10 の状態機械どおり)が、
/// 「今まさにドラッグ中か、それは新規選択か浮動片移動か」はこの型でだけ
/// 追跡する。
/// v4 §16.3: `PendingFloating` が `SelMask`(`Vec<u8>` を持つ)を保持するため、
/// もはや `Copy` にできない(`Clone` のみ)。
#[derive(Debug, Clone)]
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
    /// (SPEC §6: 「選択内部をドラッグ→浮動化」)。v4 §16.3: 浮動化される
    /// マスク(選択があればその形状、無ければ対象範囲の全 1 矩形マスク)を
    /// そのまま保持しておく。
    PendingFloating {
        mask: crate::document::SelMask,
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

/// v4 §22: 多角形なげなわの進行中状態(ARCHITECTURE.md §16.3)。
/// 「クリックで頂点追加、ダブルクリック/Enter/始点クリックで閉じる、Esc で
/// 中止」の状態機械: `points` が積み上がった頂点列(画像座標)、
/// `last_click` が直近のクリック時刻・スクリーン座標(ダブルクリック判定用)。
struct LassoPolygonState {
    points: Vec<Pos2>,
    last_click: Option<(Instant, Pos2)>,
}

/// v3 §19: テキストツールのインライン編集状態(ARCHITECTURE.md §15.3)。
/// `DaraskApp::text_edit` が `Some` の間、`draw_text_edit_overlay` が毎フレーム
/// `egui::TextEdit::multiline` のオーバーレイを表示する。確定(Ctrl+Enter/
/// ボックス外クリック)でラスタライズして浮動片になり、この状態は消える
/// (SPEC §19)。
struct TextEditState {
    /// クリック位置(画像座標)。SPEC §19: 「クリック位置=テキストボックスの
    /// 左上」。
    pos: Pos2,
    buffer: String,
    /// 生成直後の 1 フレームだけ `true`。そのフレームでのみ
    /// `Response::request_focus()` を呼ぶ(SPEC §19 のクリック開始で
    /// フォーカスを掴むため)。毎フレーム無条件に呼ぶと、egui の
    /// 「フォーカス中ウィジェットの外側をクリックすると自動的にフォーカスを
    /// 失う」判定(`SurrenderFocusOn::Clicks`)を直後に自前で上書きしてしまい、
    /// `Response::lost_focus()` が「ボックス外クリック」を検知できなくなる
    /// (`draw_text_edit_overlay` 参照)。
    needs_focus: bool,
}

/// 未保存ガード(SPEC §8)が「保存/破棄を選んだ後に何をするか」を覚えておく
/// ためのアクション(ARCHITECTURE.md §10: `pending_action: Option<PendingAction>`)。
///
/// v5 §30: Ctrl+N(新規)/Ctrl+O(開く)は「アクティブタブの内容を置き換える」
/// から「新規タブを追加してそこに開く」に意味変更された(v1 §7・§8 を
/// 上書き)。新規タブの追加は既存タブの内容を一切破壊しないため、もはや
/// 未保存ガードの対象ではない(`begin_new_tab`/`begin_open_tab`/
/// `open_path_in_new_tab` がこの列挙体を経由せず直接実行する)。この列挙体に
/// 残っているのは、実際に既存の内容を破棄しうる操作だけ:
/// SPEC §30 の「最後の 1 タブを閉じようとした場合…内容を白紙に戻す」
/// (`CloseLastTab`、`close_tab` 参照。「新規」と同じダイアログを経由するが、
/// こちらは唯一のタブの内容をその場で置き換えるため、v1〜v4 時代の `New` と
/// 同様に未保存ガードが必要)、v5 §17.4 の「タブを閉じる際、そのタブが
/// 未保存なら確認する」(`CloseTab`、`Ctrl+W`・タブの×・中クリック経由)、
/// および v5 §17.4 の「ウィンドウを閉じる/終了する際、未保存タブがあれば
/// タブごとに順番に確認する」(`CloseAllTabs`、`begin_quit` 参照)。
// v5 §17.4 でこの列挙体の全バリアントが「閉じる」系(`CloseLastTab`/
// `CloseTab`/`CloseAllTabs`)になったため clippy::enum_variant_names が
// 反応するが、これは実際に「未保存ガード後に閉じる操作を実行する」という
// この列挙体の役割そのものを表しており、プレフィックスを削ると
// (`LastTab`/`Tab`/`AllTabs`)かえって何のタブ操作か読み取りにくくなる
// (ARCHITECTURE.md §16.10-6: 「判断に迷う lint は allow+根拠コメント」)。
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingAction {
    /// SPEC §30: 最後の 1 タブを閉じようとした(`close_tab` 参照)。
    CloseLastTab,
    /// v5 §17.4: 2 枚以上あるタブのうちの 1 枚(`usize` は `tabs` への
    /// index)を閉じようとした。`close_tab` が確認前に
    /// `switch_tab(index)` 済みなので、確認モーダルが出ている間
    /// アクティブタブは常にこの index を指す。
    CloseTab(usize),
    /// v5 §17.4: ウィンドウを閉じる/アプリを終了する一連の流れ。まだ確認
    /// していない未保存タブの index 列(先頭が次に確認する対象)。
    /// アプリごと終了する前提のため、確認の都度 `tabs` から取り除くことは
    /// しない(`continue_closing_all_tabs` のドキュメントコメント参照 —
    /// 削除しないので `tabs` の長さは変わらず、残りの index がずれる
    /// 心配もない)。
    CloseAllTabs(VecDeque<usize>),
}

/// SPEC §7 のダイアログ群(ARCHITECTURE.md §10: `modal: Option<ModalState>`)。
enum ModalState {
    New {
        width: u32,
        height: u32,
        background: Background,
        /// `true` ならアクティブタブ(=唯一のタブ)の内容をその場で置き換える
        /// (SPEC §30: 「最後の 1 タブを閉じる」→`PendingAction::CloseLastTab`
        /// 経由)。`false` なら通常の Ctrl+N と同じく新規タブを追加する
        /// (`confirm_new` 参照)。
        replace_active: bool,
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
    /// SPEC §24: 「明るさ・コントラスト…」(各 -100..100、ライブプレビュー)。
    /// `rect` はモーダルを開いた時点の対象領域(選択 bbox、無ければアクティブ
    /// レイヤー全体。ARCHITECTURE.md §16.5)。開いた時点で
    /// `History::begin_stroke`/`ensure_tiles_saved(rect)` 済み。
    BrightnessContrast {
        brightness: i32,
        contrast: i32,
        rect: crate::document::IRect,
    },
    /// SPEC §24: 「色相・彩度・明度…」(Ctrl+U)。上と同じ仕組み。
    HueSaturation {
        hue: i32,
        saturation: i32,
        lightness: i32,
        rect: crate::document::IRect,
    },
    /// v4 §26: 「ヘルプ > バージョン情報」。表示するだけで状態を持たない。
    About,
    /// SPEC §34/ARCHITECTURE.md §18.2: 「設定(環境設定)」ダイアログ(Ctrl+K)。
    /// `New`/`ImageResize` と同じ「ドラフト値を持ち、OK で確定・キャンセルで
    /// 破棄する」パターン。確定前に実際の `self.max_undo_steps`/
    /// 各タブの `History::max_steps` を書き換えないよう、編集中の値は
    /// このドラフトにだけ保持する。
    Preferences {
        draft_max_undo_steps: u32,
    },
}

/// rfd のネイティブダイアログはブロッキングでイベントループを止めるため、
/// フレーム処理の外側(次フレーム冒頭)で呼ぶ必要がある
/// (ARCHITECTURE.md §12-9)。クリックされた瞬間はこのフラグだけを立て、
/// 次フレームの `ui()` 冒頭で実際に呼び出す。
enum DialogRequest {
    OpenFile,
    SaveAs,
}

/// v4 §26(ARCHITECTURE.md §16.7): 設定から復元する起動時のツール状態の
/// 純粋な計算部分。`DaraskApp::new` は `eframe::CreationContext` を要求する
/// ためユニットテストできないが(`new_for_test` のドキュメントコメント
/// 参照)、これは `Settings` だけから計算できる純関数なのでテストできる。
struct StartupToolState {
    brush_size: f32,
    brush_hardness: u8,
    brush_opacity: u8,
    brush_smoothing: u8,
    rect_mode: ShapeMode,
    ellipse_mode: ShapeMode,
    fill_tolerance: u8,
    gradient_kind: raster::GradientKind,
    gradient_colors: crate::tools::gradient::GradientColors,
    last_shape_tool: ToolKind,
    last_marquee_tool: ToolKind,
    last_fill_tool: ToolKind,
}

impl StartupToolState {
    /// `settings::parse` は型の範囲(例: u8 なら 0–255)までしか検証しない
    /// ため、各 UI が実際に許す範囲へここでクランプする(手編集・破損した
    /// 設定ファイルからの防御、ARCHITECTURE.md §16.10-5)。
    fn resolve(settings: &Settings) -> Self {
        // SPEC §20/§22/§23: `last_shape_tool`/`last_marquee_tool`/
        // `last_fill_tool` は `U`/`M`/`G` が戻る先(`set_tool` のドキュメント
        // コメント参照)。復元した `last_tool` がそれぞれの巡回グループに
        // 属していれば引き継ぎ、そうでなければ各グループの既定値(SPEC の
        // 表の先頭)のままにする。
        let last_shape_tool = match settings.last_tool {
            ToolKind::Line | ToolKind::Rect | ToolKind::Ellipse => settings.last_tool,
            _ => ToolKind::Line,
        };
        let last_marquee_tool = match settings.last_tool {
            ToolKind::Select | ToolKind::EllipseSelect => settings.last_tool,
            _ => ToolKind::Select,
        };
        let last_fill_tool = match settings.last_tool {
            ToolKind::Fill | ToolKind::Gradient => settings.last_tool,
            _ => ToolKind::Fill,
        };
        Self {
            brush_size: settings.brush_size.clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE),
            brush_hardness: settings
                .brush_hardness
                .clamp(MIN_BRUSH_HARDNESS, MAX_BRUSH_HARDNESS),
            brush_opacity: settings
                .brush_opacity
                .clamp(MIN_BRUSH_OPACITY, MAX_BRUSH_OPACITY),
            // SPEC §25: スムージングは 0–100%(オプションバーのスライダー範囲)。
            brush_smoothing: settings.brush_smoothing.min(100),
            rect_mode: settings.rect_mode,
            ellipse_mode: settings.ellipse_mode,
            fill_tolerance: settings.fill_tolerance,
            gradient_kind: settings.gradient_kind,
            gradient_colors: settings.gradient_colors,
            last_shape_tool,
            last_marquee_tool,
            last_fill_tool,
        }
    }
}

/// v5 §30(ARCHITECTURE.md §17.1): 1 つのドキュメントタブが持つ状態。
///
/// 読み替え規則(SPEC v5 冒頭): v1〜v4 で「ドキュメント」「doc」「画像」と
/// 書かれていた箇所は、v5 以降は**アクティブタブのドキュメント**を指す。
/// 選択・浮動片・アンドゥ履歴・ズーム/パン・ファイルパス・未保存フラグは
/// タブごとに独立する(`doc`/`history`/`view`/`selection`/`floating` が
/// それぞれ該当。`doc.path`/`doc.modified` は `Document` 自身が持つため
/// ここには重複させない)。
///
/// ツール・色・ブラシ設定・パレット・最近使ったファイル・ウィンドウ状態は
/// 引き続きアプリ全体で共有するため `DaraskApp` 側に残す(このタブ以外の
/// フィールドは変更しない、ARCHITECTURE.md §17.1 冒頭の読み替え規則どおり)。
///
/// ストローク進行中の一時状態(`select_drag`/なげなわの頂点列/
/// `text_edit` 等)は ARCHITECTURE.md §17.1 のコメントに従い `DaraskApp`
/// 側に残す。V5-M2(タブ切替 UI の実装)では、これらの一時状態を安全側に
/// 倒すため `commit_open_gesture()` をタブ切替の唯一の入口(`switch_tab`/
/// `open_new_tab`/`close_tab`)の内部で必ず呼ぶ(ARCHITECTURE.md §17.3:
/// 「タブ切替前に必ず commit_open_gesture() を呼ぶ」— 本プロジェクトで
/// 最も繰り返し発生してきたバグパターンの再発防止策)。
///
/// バグ修正: `layer_rename`/`next_layer_number` は、以前は本 struct では
/// なく `DaraskApp` 直下の共有フィールドだった。しかしこの 2 つは
/// `doc`/`selection` 等と全く同じ「アクティブタブのドキュメントに紐付く
/// 状態」であり、共有のままだと (1) タブ A でレイヤー名編集を開始した
/// ままタブ B へ切り替えると、`side_panel::show` が「タブ B の doc」+
/// 「タブ A で編集中だった rename」を組み合わせて描画してしまい、確定
/// (Enter/フォーカス外し)するとタブ B の無関係なレイヤーの名前をタブ A
/// での入力内容で上書きしてしまう(クロスタブ破損)、(2) 「レイヤー N」の
/// 採番がタブをまたいで共有され、`untitled_number` と違いタブごとに
/// 1 から連番にならず歯抜けになる、という 2 つの実在するバグを引き起こす。
/// `untitled_number` と同様にタブごとに独立させることで両方解消する。
struct Tab {
    doc: Document,
    history: History,
    view: CanvasView,
    selection: Option<Selection>,
    floating: Option<Floating>,
    /// SPEC §30: 「無題」「無題2」「無題3」…の番号。`doc.path` が `None`
    /// (=ファイルに紐付いていない)間だけ意味を持つ。生成時に
    /// `DaraskApp::next_untitled_number` から一度だけ払い出され、以後は
    /// 他のタブが閉じても採番し直さない(モノトニックに増え続けるだけ
    /// なので、同時に開いている「無題」タブ同士のラベルは常に重複しない)。
    untitled_number: Option<u32>,
    /// ダブルクリックで開始した名前編集の状態(`ui/layers_panel.rs`)。
    /// タブごとに独立(上記コメント参照)。
    layer_rename: RenameState,
    /// 新規レイヤーの名前(SPEC §13: 「レイヤー N」)に使う次の番号。
    /// このタブのドキュメントを新規作成/読み込みし直すたびに 1 に
    /// リセットする(タブごとに独立、上記コメント参照)。
    next_layer_number: u32,
}

impl Tab {
    /// 与えられた `Document` を唯一のドキュメントとする新規タブ(選択・
    /// 浮動片・アンドゥ履歴・ズーム/パンは初期状態)。`untitled_number` は
    /// `doc.path.is_none()` のときだけ渡す(呼び出し元は
    /// `DaraskApp::open_new_tab`/`DaraskApp::new` を参照)。
    ///
    /// `max_undo_steps`(SPEC §34/ARCHITECTURE.md §18.2・§18.6-2): 新規タブの
    /// `History` は `History::new()` の既定(50)のまま作られるため、設定
    /// ダイアログで既に別の値へ変更済みなら、ここで `set_max_steps` を呼んで
    /// 新規タブにも同じ上限を適用する(呼び出し元は常に
    /// `DaraskApp::max_undo_steps` を渡す — 新規タブだけ既定値に取り残される
    /// バグを防ぐ)。
    fn new(doc: Document, untitled_number: Option<u32>, max_undo_steps: u32) -> Self {
        let mut history = History::new();
        history.set_max_steps(max_undo_steps as usize);
        Self {
            doc,
            history,
            view: CanvasView::new(),
            selection: None,
            floating: None,
            untitled_number,
            layer_rename: None,
            next_layer_number: 1,
        }
    }

    /// SPEC §30: 「ファイル名(無題なら「無題」「無題2」「無題3」…と連番)」。
    /// ウィンドウタイトル(`DaraskApp::window_doc_label`)・タブバー・
    /// 「名前を付けて保存」の初期ファイル名(`DaraskApp::
    /// default_save_file_name`)がすべてこれを情報源とする。
    fn label(&self) -> String {
        match &self.doc.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "無題".to_owned()),
            // `untitled_number` は生成時に必ず `Some` が渡される前提だが
            // (`doc.path.is_none()` の唯一の生成経路)、プログラミング上の
            // 不変条件が破れても panic せず素の「無題」にフォールバックする。
            None => match self.untitled_number {
                Some(n) if n > 1 => format!("無題{n}"),
                _ => "無題".to_owned(),
            },
        }
    }
}

pub struct DaraskApp {
    /// v5 §30(ARCHITECTURE.md §17.1): 開いているタブ群。**常に 1 枚以上を
    /// 維持する**(SPEC §30: 「タブが 0 枚になる状態は作らない」、
    /// `close_tab` 参照)。上限は `MAX_TABS`(SPEC §30: 24)。
    tabs: Vec<Tab>,
    /// `tabs` への有効な index。**常に有効**であることを実行時に保証する
    /// (ARCHITECTURE.md §17.8-3: 型ではなく実行時に保証、境界チェックを
    /// 怠らない)。タブ切替は必ず `switch_tab`/`open_new_tab`/`close_tab`
    /// (ARCHITECTURE.md §17.3 の安全規則を内包する唯一の入口)を経由して
    /// 更新すること。
    active_tab: usize,
    /// v5 §30(ARCHITECTURE.md §17.1): 「無題」「無題2」…の採番用カウンタ
    /// (`next_layer_number` と同型)。`doc.path` が無い新規タブを作るたびに
    /// 1 つ消費してインクリメントする(`take_untitled_number` 参照)。
    /// タブが閉じても巻き戻さない(モノトニック、同時に開いている「無題」
    /// タブ同士のラベルが重複しないことを保証する)。
    next_untitled_number: u32,
    tool: ToolKind,
    /// SPEC §20: 「U: 図形(直前に使った図形)」。`ToolKind::Line`/`Rect`/
    /// `Ellipse` のいずれか(`set_tool` が唯一の更新箇所、`keymap::Action::
    /// SelectLastShapeTool`/`CycleShapeTool` 参照)。
    last_shape_tool: ToolKind,
    /// SPEC §22: 「M / Shift+M で巡回」。`ToolKind::Select`(矩形)/
    /// `EllipseSelect`(楕円)のいずれか(`last_shape_tool` と全く同じ役割、
    /// `set_tool`/`cycle_marquee_tool` 参照)。
    last_marquee_tool: ToolKind,
    /// SPEC §23: 「G / Shift+G で巡回」。`ToolKind::Fill`/`Gradient` の
    /// いずれか(`last_shape_tool`/`last_marquee_tool` と全く同じ役割、
    /// `set_tool`/`cycle_fill_tool` 参照)。
    last_fill_tool: ToolKind,
    pen: PenTool,
    eraser: EraserTool,
    line: ShapeTool,
    rect_tool: ShapeTool,
    ellipse: ShapeTool,
    fill: FillTool,
    picker: PickerTool,
    /// v4 §23: グラデーション(種類・色の組み合わせも自身で持つ、
    /// `ShapeTool::mode` と同じ設計)。
    gradient: GradientTool,
    /// v4 §22: なげなわの自由/多角形モード(Shift+L で切替)。
    lasso_mode: LassoMode,
    /// v4 §22: 自由なげなわのドラッグ中に記録した軌跡(画像座標)。ドラッグ
    /// 外・多角形モード中は空。
    lasso_freehand_points: Vec<Pos2>,
    /// v4 §22: 多角形なげなわの進行中状態(`None` = 未着手)。
    lasso_polygon: Option<LassoPolygonState>,
    /// v4 §22: 自動選択の許容値(SPEC §22: 「クリック画素から許容値
    /// (0–255、オプションバー)の連結領域」)。
    magic_wand_tolerance: u8,
    primary: Color32,
    secondary: Color32,
    brush_size: f32,
    /// SPEC §17: ブラシ/消しゴム共通の硬さ(0–100%)。`ToolCtx::hardness`
    /// へ 0.0–1.0 に正規化して渡す。
    brush_hardness: u8,
    /// SPEC §17: ブラシ/消しゴム共通の不透明度(1–100%。消しゴムでは
    /// 「強さ」として表示)。`ToolCtx::opacity` へ 0.0–1.0 に正規化して渡す。
    brush_opacity: u8,
    /// SPEC §17: 鉛筆モード(デフォルト OFF)。
    pencil_mode: bool,
    /// SPEC §25: ブラシ/消しゴム/鉛筆共通のスムージング(0–100%、デフォルト
    /// 0)。`ToolCtx::smoothing` へ 0.0–1.0 に正規化して渡す。
    brush_smoothing: u8,
    /// 最近使った色(SPEC §5: 最大 8、先頭が最新)。
    recent_colors: VecDeque<Color32>,
    /// Alt+クリックによる一時スポイト(SPEC §4)の最中、対応するボタンの
    /// Up が来るまで通常のツール処理を止めておくためのフラグ。
    alt_eyedropper_active: bool,
    /// SPEC §25: 「ピクセルグリッド…デフォルト ON」。表示メニューのトグル。
    /// `zoom >= 8.0`(800%)のときだけ実際に描かれる(`canvas_view::
    /// draw_pixel_grid`)。
    show_pixel_grid: bool,
    /// SPEC §34/ARCHITECTURE.md §18.2: 「元に戻す履歴の保持数」(1–500、
    /// 既定 50)。設定ダイアログの OK で更新される、アプリ全体で共有の値
    /// (`current_settings`/`Tab::new` の呼び出し元がこれを渡す。SPEC §26 の
    /// 永続化対象に追加)。開いている**全タブ**の `History::max_steps` へ即座に
    /// 反映するのは `apply_preferences` の責務(ARCHITECTURE.md §18.6-2:
    /// 「既存タブが取り残される、というバグを作らない」)。
    max_undo_steps: u32,

    // -- v2 §14: カラーパネル(ARCHITECTURE.md §14.3/§14.4, V2-M3) --------
    /// 色相リング + SV 三角形の編集中状態(ドラッグ中は HSV を正とする、
    /// ARCHITECTURE.md §14.9-1)。
    color_wheel: ColorWheelState,
    /// HEX 入力欄の編集中テキスト(`ui/color_panel.rs` 参照)。
    color_hex_buffer: String,
    /// ユーザーパレット(SPEC §14: 「＋」で追加)。v4 §26 で永続化対象に
    /// なった(`current_settings`/`DaraskApp::new` 参照。以前は「永続化は
    /// しない」だったが、SPEC §26 の一覧に明記されたため方針が変わった)。
    user_palette: Vec<Color32>,

    // -- M4: 選択・フローティング(ARCHITECTURE.md §7) --------------------
    // v5 §30(ARCHITECTURE.md §17.1): `selection`/`floating` は `Tab` へ移動
    // した(タブごとに独立、読み替え規則)。`select_drag`(進行中ドラッグの
    // 一時状態)はストローク進行中の一時状態として引き続きここに残す。
    select_drag: Option<SelectDrag>,
    /// `Floating` のテクスチャキャッシュキー用の採番(canvas_view.rs 参照)。
    next_floating_id: u64,

    // -- v3 §19: テキストツール(ARCHITECTURE.md §15.3) --------------------
    /// UI と同じシステム日本語フォントのバイト列(`setup_japanese_fonts` が
    /// 一度だけ読み込む)。`ab_glyph::FontRef` はこれを借用して呼び出しの
    /// たびに軽量に構築し直す(`text::rasterize_text` 参照)。見つからなければ
    /// `None`(テキストツールは使えないが、他機能はパニックせず動作する)。
    text_font: Option<Arc<Vec<u8>>>,
    /// SPEC §19: フォントサイズ 8–144px(デフォルト 24)。
    text_font_size: f32,
    /// 編集中のテキストボックス(`None` なら非編集中)。
    text_edit: Option<TextEditState>,

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

    // -- v4 §26: 設定の永続化・最近使ったファイル(ARCHITECTURE.md §16.7) --
    /// 最近使ったファイル(SPEC §26: 最大 8、先頭が最新)。「ファイル >
    /// 最近使ったファイル」サブメニュー(`ui/menu.rs`)がこれを表示する。
    recent_files: VecDeque<PathBuf>,

    // -- v2 §13: レイヤーパネル(ARCHITECTURE.md §14.8 V2-M2) --------------
    // バグ修正: `layer_rename`/`next_layer_number` は `Tab`(タブごとの状態)
    // へ移動した。詳細は `Tab` の docstring 参照(クロスタブ破損・採番の
    // 歯抜けを防ぐため)。
    /// 起動時白画面(DWM 合成の競合)ワークアラウンドの状態。
    /// `StartupNudge` のドキュメントコメント参照。
    startup_nudge: StartupNudge,
    /// 直近フレームの `screen_rect`(ウィンドウ内寸変化の検出用。
    /// `ui()` 冒頭の「追加提示」ワークアラウンドのコメント参照)。
    last_screen_rect: egui::Rect,

    // -- v4 §26: 終了時のウィンドウ状態保存用(ARCHITECTURE.md §16.7) ------
    /// 直近フレームで観測したウィンドウの内寸(論理ポイント)。終了処理
    /// (`on_exit`/`exit_process`)は `egui::Context` を持たないため、
    /// 「終了時の値」を都度ここへ観測しておいて使う(`ui()` 冒頭で毎フレーム
    /// 更新。SPEC §26 の「ウィンドウ寸法・最大化状態」の保存元)。
    window_size: egui::Vec2,
    /// 直近フレームで観測した最大化状態。
    window_maximized: bool,
    /// `false` ならユニットテスト(`new_for_test`)。実 `%APPDATA%` を汚さない
    /// ため `save_settings` を無効化する(`save_settings` のドキュメント
    /// コメント参照)。実アプリ(`DaraskApp::new`)は常に `true`。
    persist_settings: bool,

    bench: Option<BenchState>,
}

impl DaraskApp {
    /// v5 §30(ARCHITECTURE.md §17.1): アクティブタブへの参照。`active_tab`
    /// (index フィールド)は `DaraskApp::new`/`new_for_test` が常に
    /// `tabs` の有効な範囲で初期化し、タブを閉じる操作(v5 §17.4、
    /// V5-M1 ではまだ存在しない)が必ず追随させるため、境界チェックは
    /// `tabs[..]` の添字アクセスに委ねてよい(SPEC の不変条件が破れた場合は
    /// パニックで即座に検出したい、ARCHITECTURE.md §17.8-3)。
    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    /// 上と同じ(可変参照版)。
    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    /// `process_start` は `main()` 冒頭で取得した `Instant`。
    /// `bench_mode` は環境変数 `DARASK_BENCH=1` が設定されていたかどうか。
    /// `cli_path` は SPEC §3 の「プログラムから開く」用の起動引数(あれば)。
    /// `font_handle` は `main()` がウィンドウ作成と並行して起こしておいた
    /// 日本語フォント読込スレッド(v4 §16.2)。
    /// `settings` は `main()` が起動時に 1 回読み込んだ永続設定(v4 §26、
    /// `settings::load`)。`settings_loaded_ms` はベンチモード時のみ
    /// `Some`(`main()` が `settings::load` 直後に計測した経過ミリ秒。
    /// ARCHITECTURE.md §16.2 の「設定読込完了」フェーズ)。
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        process_start: Instant,
        bench_mode: bool,
        cli_path: Option<PathBuf>,
        font_handle: std::thread::JoinHandle<Option<Vec<u8>>>,
        settings: Settings,
        settings_loaded_ms: Option<u128>,
    ) -> Self {
        // v4 §16.2: 「設定読込完了」フェーズ(`main()` 側で計測済みの値を
        // そのまま記録する。設定ファイルの読み込みはウィンドウ作成より前に
        // 行われるため、ここが `phases` の先頭になる)。
        let mut phases: Vec<(&'static str, u128)> = Vec::new();
        if let Some(ms) = settings_loaded_ms {
            phases.push(("settings", ms));
        }
        // `new()` が呼ばれた時点 ≈ eframe のウィンドウ/GL コンテキスト作成が
        // 完了した時点(`run_native` はこのクロージャをウィンドウ作成後に
        // 呼ぶ)。ここまでの経過を「window」フェーズとして記録する。
        if bench_mode {
            phases.push(("window", process_start.elapsed().as_millis()));
        }

        // main() が別スレッドで先に開始していたフォント読込を join する
        // (ウィンドウ作成と並行していたぶん、実質の待ち時間は短縮される)。
        // `JoinHandle::join()` の `Err`(読み込みスレッドのパニック)は
        // `unwrap()` せず `None` にフォールバックする(CLAUDE.md 鉄則:
        // I/O・ユーザー入力経路で unwrap しない。`text::load_font_bytes` 自体は
        // パニックしない実装だが、スレッド境界を挟む以上は防御的に扱う)。
        let font_bytes = font_handle.join().unwrap_or(None);
        if bench_mode {
            phases.push(("font", process_start.elapsed().as_millis()));
        }
        let text_font = register_japanese_font(&cc.egui_ctx, font_bytes);

        // M4 で発見・修正したバグ: egui 0.35 は `Options::zoom_with_keyboard`
        // がデフォルト `true` で、`Context::end_pass` が Ctrl+Plus/Ctrl+Equals/
        // Ctrl+Minus/Ctrl+Num0 を消費してアプリ全体の UI ズーム
        // (`pixels_per_point`)を変更してしまう。本アプリは SPEC §10 で
        // キャンバス側の独自ズーム(`Action::ZoomIn`/`ZoomOut` 等)を持つため、
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
        // 開けたら「最近使ったファイル」にも反映する(`app` 構築後に
        // `remember_recent_file` を呼ぶ。SPEC §26)。
        let mut opened_cli_path = None;
        if let Some(path) = cli_path {
            match io::load_image(&path) {
                Ok(loaded) => {
                    doc = loaded;
                    opened_cli_path = Some(path);
                }
                Err(e) => startup_error = Some(format!("開けませんでした: {e}")),
            }
        }

        // v4 §16.2: `new()` を抜ける直前を「app_new」フェーズとして記録する
        // (この後は `run_native` が最初の `update()` を呼ぶだけ)。
        if bench_mode {
            phases.push(("app_new", process_start.elapsed().as_millis()));
        }
        let bench = bench_mode.then_some(BenchState {
            process_start,
            frames_drawn: 0,
            phases,
        });

        // v4 §26(ARCHITECTURE.md §16.7): 設定から復元するツール状態。
        // `DaraskApp::new` 自体は `eframe::CreationContext` を要求するため
        // ユニットテストできないが(`new_for_test` のドキュメントコメント
        // 参照)、この計算部分は純粋なので `StartupToolState::resolve` に
        // 切り出してテストしている。
        let startup = StartupToolState::resolve(&settings);
        let mut rect_tool = ShapeTool::new_rect();
        rect_tool.mode = startup.rect_mode;
        let mut ellipse = ShapeTool::new_ellipse();
        ellipse.mode = startup.ellipse_mode;
        let mut fill = FillTool::new();
        fill.tolerance = startup.fill_tolerance;
        let mut gradient = GradientTool::new();
        gradient.kind = startup.gradient_kind;
        gradient.colors = startup.gradient_colors;
        let window_size = egui::vec2(settings.window_width as f32, settings.window_height as f32);

        // v5 §30: 起動時のタブは「無題」の採番対象になるかどうかを、`doc` を
        // `Tab::new` へ移す前に確定しておく(CLI 引数で開けたファイルには
        // 採番しない、`open_new_tab`/`Tab::label` と同じ規則)。
        let initial_untitled_number = doc.path.is_none().then_some(1);
        let next_untitled_number = if doc.path.is_none() { 2 } else { 1 };

        let mut app = Self {
            // v5 §30(ARCHITECTURE.md §17.1): 起動時はタブ 1 枚
            // (CLI 引数があればそれを開いた状態、無ければ白紙の新規。
            // SPEC §30: 「セッション復元は非目標」)。
            tabs: vec![Tab::new(
                doc,
                initial_untitled_number,
                settings.max_undo_steps,
            )],
            active_tab: 0,
            next_untitled_number,
            // SPEC §26: 「最後に使ったツール」。
            tool: settings.last_tool,
            last_shape_tool: startup.last_shape_tool,
            last_marquee_tool: startup.last_marquee_tool,
            last_fill_tool: startup.last_fill_tool,
            pen: PenTool::new(),
            eraser: EraserTool::new(),
            line: ShapeTool::new_line(),
            rect_tool,
            ellipse,
            fill,
            picker: PickerTool::new(),
            gradient,
            // SPEC §22: 「自由: …」が表の先頭に書かれている方をデフォルトにする。
            // なげなわのモードは SPEC §26 の永続化対象に含まれていない。
            lasso_mode: LassoMode::Freehand,
            lasso_freehand_points: Vec::new(),
            lasso_polygon: None,
            magic_wand_tolerance: settings.magic_wand_tolerance,
            primary: settings.primary,
            secondary: settings.secondary,
            brush_size: startup.brush_size,
            brush_hardness: startup.brush_hardness,
            brush_opacity: startup.brush_opacity,
            pencil_mode: settings.pencil_mode,
            brush_smoothing: startup.brush_smoothing,
            // SPEC §26 の永続化対象に「最近使った色」は含まれていない
            // (対象は「最近使ったファイル」のみ)。
            recent_colors: VecDeque::new(),
            alt_eyedropper_active: false,
            show_pixel_grid: settings.show_pixel_grid,
            max_undo_steps: settings.max_undo_steps,
            color_wheel: ColorWheelState::new(),
            // 起動 1 フレーム目から正しい表記を出す(空文字だと 1 フレーム
            // だけ空欄がちらつく)。
            color_hex_buffer: color_panel::format_hex(settings.primary),
            user_palette: settings.user_palette,
            select_drag: None,
            next_floating_id: 0,
            text_font,
            text_font_size: DEFAULT_TEXT_FONT_SIZE,
            text_edit: None,
            modal: None,
            pending_action: None,
            pending_dialog: None,
            after_save_action: None,
            last_jpeg_quality: DEFAULT_JPEG_QUALITY,
            last_title: String::new(),
            toast: None,
            recent_files: settings.recent_files,
            // ベンチモードは 2 フレームで自動終了する決定的なスモーク
            // テストなので、リサイズを送らない(SPEC §11)。
            startup_nudge: if bench_mode {
                StartupNudge::Done
            } else {
                StartupNudge::Pending {
                    deadline: Instant::now() + STARTUP_NUDGE_DELAY,
                }
            },
            last_screen_rect: egui::Rect::NOTHING,
            window_size,
            window_maximized: settings.window_maximized,
            persist_settings: true,
            bench,
        };
        if let Some(message) = startup_error {
            app.show_toast(message);
        }
        if let Some(path) = opened_cli_path {
            app.remember_recent_file(path);
        }
        app
    }

    /// 起動時白画面ワークアラウンドの 1 フレームぶんの処理
    /// (`StartupNudge` のドキュメントコメント参照)。`ui()` の冒頭で毎
    /// フレーム呼ぶが、`Done` に達した後は何もしない。
    fn tick_startup_nudge(&mut self, ctx: &egui::Context) {
        match self.startup_nudge {
            StartupNudge::Pending { deadline } => {
                let now = Instant::now();
                if now < deadline {
                    // アイドルでも期限に必ず 1 フレーム起きるよう予約する
                    // (起動後 300ms 限定。恒久ループではない)。
                    ctx.request_repaint_after(deadline - now);
                } else if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                    let size = rect.size();
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                        size + egui::vec2(1.0, 0.0),
                    ));
                    self.startup_nudge = StartupNudge::Restore {
                        deadline: now + STARTUP_NUDGE_RESTORE_DELAY,
                        size,
                    };
                    ctx.request_repaint_after(STARTUP_NUDGE_RESTORE_DELAY);
                } else {
                    // 内寸が取れない(理論上 Windows では起きない)場合は
                    // 何もせず終了する。パニックしない(SPEC §12)。
                    self.startup_nudge = StartupNudge::Done;
                }
            }
            StartupNudge::Restore { deadline, size } => {
                let now = Instant::now();
                if now < deadline {
                    ctx.request_repaint_after(deadline - now);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    self.startup_nudge = StartupNudge::Done;
                }
            }
            StartupNudge::Done => {}
        }
    }

    // -----------------------------------------------------------------
    // ショートカット
    // -----------------------------------------------------------------

    /// SPEC §20(Photoshop 準拠ショートカット最終キーマップ)のショート
    /// カットをここで一括ディスパッチする(ARCHITECTURE.md §15.4: 従来
    /// バラバラだった `handle_tool_shortcuts`/`handle_color_and_brush_
    /// shortcuts`/`handle_undo_redo_shortcuts`/`handle_selection_shortcuts`/
    /// `handle_view_shortcuts`/`handle_file_shortcuts`/`handle_layer_
    /// shortcuts` を `keymap::poll` 経由の単一ディスパッチへ集約した)。
    /// キー割り当てそのもの(`Binding`)は `keymap::KEYMAP` が唯一の情報源
    /// であり、消費順序(修飾キーの多いものから先に consume、
    /// ARCHITECTURE.md §15.4 ②)も `keymap::poll` 側で一元的に保証する。
    ///
    /// テキスト入力中・モーダル表示中は無効(SPEC §4 最終行、
    /// ARCHITECTURE.md §10: 「モーダル表示中はキャンバスへの入力を渡さない」
    /// の趣旨をショートカットにも適用する、ARCHITECTURE.md §15.4 ①)。
    /// テキスト編集中専用の Ctrl+Enter/Esc だけは逆のガード(「編集中でな
    /// ければ無効」)を持つため、この関数の対象外(`handle_text_edit_
    /// shortcuts` が別枠のまま処理する、`keymap` モジュールドキュメント
    /// コメント参照)。
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        // v3 §18: Enter(確定)/Esc(キャンセル)は選択/移動ツール使用中のみ
        // 有効(移動ツールも選択と同じ `Selection`/`Floating` 浮動化パスを
        // 使うため、`commit_open_gesture`/`move_down` と同じ扱い)。
        // v4 §22: 楕円選択もこの仲間(`Select` と全く同じ状態機械を共有)。
        //
        // v4 レビューで発見・修正したバグ: `MagicWand`(自動選択)がここに
        // 含まれていなかったため、「W でクリックして選択を作る→Esc」が
        // 無反応だった(SPEC §18: 「Esc は…選択を解除する」はツール限定なし
        // の規定)。`magic_wand_select` は `Floating` を作らずプレーンな
        // `Selection` だけを設定するツールなので、`commit_selection`/
        // `cancel_floating` を素通りする(いずれも浮動片が無ければ選択解除
        // だけを行うため、`MagicWand` に対しても安全にそのまま使える)。
        // なげなわの Esc=進行中多角形の中止(選択には影響しない)は SPEC
        // §22 の明示的な例外規定なのでこのままでよい。
        let is_select_move_or_wand = matches!(
            self.tool,
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move | ToolKind::MagicWand
        );

        for action in keymap::poll(ctx) {
            match action {
                Action::SelectTool(kind) => self.set_tool(kind),
                // SPEC §20: 「U: 図形(直前に使った図形)」。
                Action::SelectLastShapeTool => self.set_tool(self.last_shape_tool),
                // SPEC §20: 「Shift+U で 直線→矩形→楕円 を巡回」。
                Action::CycleShapeTool => self.cycle_shape_tool(),
                // SPEC §22: 「M: 矩形選択/楕円選択(直前に使った形状)」。
                Action::SelectLastMarqueeTool => self.set_tool(self.last_marquee_tool),
                // SPEC §22 §27: 「Shift+M で巡回」。
                Action::CycleMarquee => self.cycle_marquee_tool(),
                // SPEC §23: 「G: 塗りつぶし系(直前に使ったツール)」。
                Action::SelectLastFillTool => self.set_tool(self.last_fill_tool),
                // SPEC §23 §27: 「Shift+G で巡回」。
                Action::CycleFillTool => self.cycle_fill_tool(),
                // SPEC §22 §27: 「Shift+L で自由↔多角形の切替」。進行中の
                // 多角形なげなわは(モードが変わる以上)継続不能なので破棄する
                // (Esc 中止と同じ挙動、選択自体には影響しない)。
                Action::CycleLassoMode => {
                    self.lasso_mode = self.lasso_mode.toggled();
                    self.lasso_polygon = None;
                    self.lasso_freehand_points.clear();
                }

                Action::SwapColors => std::mem::swap(&mut self.primary, &mut self.secondary),
                // SPEC §20: 「D 初期色(黒・白)」。MS ペイント等と同じ初期値
                // (`new()` の `primary`/`secondary` 初期化と揃える)。
                Action::DefaultColors => {
                    self.primary = Color32::BLACK;
                    self.secondary = Color32::WHITE;
                }
                Action::SetBrushOpacity(pct) => {
                    self.brush_opacity = pct.clamp(MIN_BRUSH_OPACITY, MAX_BRUSH_OPACITY);
                }

                Action::BrushSizeDec => {
                    self.brush_size = (self.brush_size - 1.0).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
                }
                Action::BrushSizeInc => {
                    self.brush_size = (self.brush_size + 1.0).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
                }
                Action::BrushHardnessDec => {
                    // u8::saturating_sub は既に 0(MIN_BRUSH_HARDNESS)で床止めされる。
                    self.brush_hardness = self.brush_hardness.saturating_sub(HARDNESS_STEP);
                }
                Action::BrushHardnessInc => {
                    self.brush_hardness = self
                        .brush_hardness
                        .saturating_add(HARDNESS_STEP)
                        .min(MAX_BRUSH_HARDNESS);
                }

                // SPEC §13 最終項/§9: 「レイヤー操作・アンドゥは浮動片や
                // ストローク進行中にはツール切替と同じ扱い(先に確定してから
                // 実行)」。`commit_open_gesture` で先に確定してしまえば
                // ストロークは「進行中」ではなくなる(M4 で確立した規則)。
                Action::Undo => {
                    self.commit_open_gesture();
                    let tab = self.active_tab_mut();
                    tab.history.undo(&mut tab.doc);
                    self.clamp_selection_to_doc();
                }
                Action::Redo => {
                    self.commit_open_gesture();
                    let tab = self.active_tab_mut();
                    tab.history.redo(&mut tab.doc);
                    self.clamp_selection_to_doc();
                }

                Action::Cut => self.cut_selection_to_clipboard(),
                Action::Copy => {
                    self.copy_selection_to_clipboard();
                }
                Action::Paste => self.paste_from_clipboard(),
                Action::Delete => self.delete_selection(),
                Action::SelectAll => self.select_all(),
                Action::Deselect => self.commit_selection(),
                Action::FreeTransform => self.free_transform(),
                Action::CommitFloating => {
                    if is_select_move_or_wand {
                        self.commit_selection();
                    } else if self.tool == ToolKind::Lasso {
                        // SPEC §22: 「Enter…で閉じる」(多角形なげなわ)。
                        self.finish_polygon_lasso_if_ready();
                    }
                }
                Action::CancelFloating => {
                    if is_select_move_or_wand {
                        self.cancel_floating();
                    } else if self.tool == ToolKind::Lasso {
                        // SPEC §22: 「Esc で中止」(多角形なげなわ)。選択には
                        // 何も影響しない(履歴にも積まない)。
                        self.lasso_polygon = None;
                        self.lasso_freehand_points.clear();
                    }
                }

                // SPEC §24 §27: 色調補正のショートカット。
                Action::HueSaturation => self.open_hue_saturation_modal(),
                Action::Invert => self.apply_invert(),
                Action::Grayscale => self.apply_grayscale(),

                Action::LayerAdd => self.layer_add(),
                Action::LayerDuplicate => self.layer_duplicate(),
                Action::LayerMergeDown => self.layer_merge_down(),
                Action::LayerFlatten => self.layer_flatten(),

                // v5 §30: 新規タブを追加するだけで既存タブを破壊しないため
                // `request_action`(未保存ガード)を経由しない
                // (`begin_new_tab`/`begin_open_tab` のドキュメントコメント参照)。
                Action::New => self.begin_new_tab(),
                Action::Open => self.begin_open_tab(),
                Action::Save => self.begin_save(),
                Action::SaveAs => self.begin_save_as(),

                // v5 §30/§32: タブ切替(ARCHITECTURE.md §17.6)。
                Action::NextTab => self.next_tab(),
                Action::PrevTab => self.prev_tab(),
                // v5 §30/§32(V5-M3): タブを閉じる(ARCHITECTURE.md §17.4)。
                Action::CloseTab => self.close_tab(self.active_tab),

                Action::ZoomIn => self.active_tab_mut().view.zoom_in(),
                Action::ZoomOut => self.active_tab_mut().view.zoom_out(),
                Action::Zoom100 => self.active_tab_mut().view.zoom_to_100(),
                Action::FitWindow => {
                    let tab = self.active_tab_mut();
                    tab.view.fit_to_window(&tab.doc);
                }

                // v6 §34(ARCHITECTURE.md §18.2): Ctrl+K で設定ダイアログ。
                Action::OpenPreferences => self.open_preferences_modal(),
            }
        }
    }

    // -----------------------------------------------------------------
    // ツール切り替え・カーソル・ディスパッチ
    // -----------------------------------------------------------------

    /// ツール切り替えの唯一の入口(ツールバークリック・単一キー双方から
    /// 呼ぶ)。選択・移動ツール(v3 §18)から離れるときは浮動片を確定させる
    /// (SPEC §6: 「ツール切替→浮動片をその位置に合成」)。それ以外の描画系
    /// ツールから離れるときも、進行中のジェスチャがあれば確定させる
    /// (M4 で発見・修正したバグ: `tools/mod.rs::Tool::cancel` のコメント
    /// 参照。以前はここで何もしなかったため、ドラッグ中にツール切替キーを
    /// 押すと進行中の `History` ストロークが次のツールの `begin_stroke` に
    /// 無警告で置き換えられ、既に描画済みのピクセルが undo 履歴に残らない
    /// まま失われていた)。
    fn set_tool(&mut self, new_tool: ToolKind) {
        // SPEC §20: 「U: 図形(直前に使った図形)」。図形系ツールへ切り替える
        // (または既にそれを使っている)たびに更新しておく。ツールバーの
        // 直接クリック(`toolbar::show` の呼び出し元)とキーボード
        // ショートカット(`Action::SelectTool`)は両方ここを通るため、この
        // 1 箇所だけで「直前に使った図形」の不変条件を保てる。
        if matches!(
            new_tool,
            ToolKind::Line | ToolKind::Rect | ToolKind::Ellipse
        ) {
            self.last_shape_tool = new_tool;
        }
        // SPEC §22: 「M / Shift+M で巡回」。`last_shape_tool` と全く同じ
        // 役割(`tool_shortcut_label`/`cycle_marquee_tool` 参照)。
        if matches!(new_tool, ToolKind::Select | ToolKind::EllipseSelect) {
            self.last_marquee_tool = new_tool;
        }
        // SPEC §23: 「G / Shift+G で巡回」。同上。
        if matches!(new_tool, ToolKind::Fill | ToolKind::Gradient) {
            self.last_fill_tool = new_tool;
        }
        if new_tool == self.tool {
            return;
        }
        self.commit_open_gesture();
        self.tool = new_tool;
    }

    /// SPEC §20: 「Shift+U で 直線→矩形→楕円 を巡回」。現在アクティブなのが
    /// 図形系ツールならそこから、そうでなければ `last_shape_tool`(= `U` が
    /// 選ぶツール)から次の図形へ進める。
    fn cycle_shape_tool(&mut self) {
        let current = if matches!(
            self.tool,
            ToolKind::Line | ToolKind::Rect | ToolKind::Ellipse
        ) {
            self.tool
        } else {
            self.last_shape_tool
        };
        let next = match current {
            ToolKind::Line => ToolKind::Rect,
            ToolKind::Rect => ToolKind::Ellipse,
            // `ToolKind::Ellipse` はもちろん、`last_shape_tool` が図形以外
            // (理論上は起きない初期値以外のケース)であっても直線へ戻す。
            _ => ToolKind::Line,
        };
        self.set_tool(next);
    }

    /// SPEC §22: 「Shift+M で巡回」。`cycle_shape_tool` の選択版
    /// (矩形選択↔楕円選択の 2 つだけを行き来する)。
    fn cycle_marquee_tool(&mut self) {
        let current = if matches!(self.tool, ToolKind::Select | ToolKind::EllipseSelect) {
            self.tool
        } else {
            self.last_marquee_tool
        };
        let next = match current {
            ToolKind::EllipseSelect => ToolKind::Select,
            _ => ToolKind::EllipseSelect,
        };
        self.set_tool(next);
    }

    /// SPEC §23: 「Shift+G で巡回」。`cycle_marquee_tool` と同じ形の 2 値巡回。
    fn cycle_fill_tool(&mut self) {
        let current = if matches!(self.tool, ToolKind::Fill | ToolKind::Gradient) {
            self.tool
        } else {
            self.last_fill_tool
        };
        let next = match current {
            ToolKind::Gradient => ToolKind::Fill,
            _ => ToolKind::Gradient,
        };
        self.set_tool(next);
    }

    /// 進行中のジェスチャ(選択ツールの浮動片、または他ツールのドラッグ中
    /// ストローク)を、それを中断させる操作の前に確定させる共通フック
    /// (ARCHITECTURE.md §14.2/§14.9-3: 「レイヤー操作・アンドゥは、浮動片や
    /// ストローク進行中にはツール切替と同じ扱い(先に確定してから実行)」を
    /// 一箇所に集約する。`set_tool` に加えて、レイヤー構造の変更・アクティブ
    /// レイヤーの切り替えの前にも呼ぶ)。
    fn commit_open_gesture(&mut self) {
        // バグ修正: レイヤー名編集中の入力(`Tab::layer_rename`)も、浮動片や
        // ストロークと全く同じ「先に確定してから実行」規則の対象にする
        // (`commit_pending_layer_rename` のドキュメントコメント参照)。以前は
        // ここで一切触れられておらず、ドキュメントを丸ごと差し替える一部の
        // 経路(`reset_active_tab_document` 等)が単に `= None` で入力内容を
        // 破棄するだけだった。ここで先に確定しておけば、それらの経路に
        // 到達するより前に `doc.modified` が正しく立ち、未保存ガード
        // (`request_action`)がリネームだけの変更も正しく検知できる。
        self.commit_pending_layer_rename();
        // v3 §18: 移動(V)も選択と同じ `Selection`/`Floating` 浮動化パスを
        // 使う(`move_down`/`handle_move_event` 参照)ため、ここでも浮動片の
        // 確定を経由させる必要がある。そうしないと、移動ツールでドラッグ中に
        // 他ツールへ切り替えたとき浮動片が確定されず消えてしまう(M4 で
        // 選択ツールについて発見・修正したバグと同じクラス、`Tool::cancel`
        // のコメント参照)。
        // v4 §22: `EllipseSelect` は `Select` と全く同じ `Selection`/
        // `Floating` 状態機械を共有する(唯一の違いは新規選択確定時のマスク
        // 形状だけ)ため、ここでも同列に扱う。
        //
        // v4 §23/§24 で発見・修正したバグ: 以前はここで `commit_selection`
        // (浮動片の確定に加え、無条件で `self.active_tab().selection` もクリアする)を
        // 呼んでいたため、「M/Lasso/W で選択してから、ツールを切り替えて
        // グラデーション/色調補正を選択範囲に適用する」という SPEC §21 が
        // 前提とする最も基本的な使い方で、ツール切替(=このメソッドの呼び出し)
        // の瞬間に選択そのものが消えてしまい、クリップ対象が無くなっていた
        // (`free_transform` が Ctrl+T について既に同じ理由で `commit_selection`
        // を避けていたのと同一クラスの問題、
        // `free_transform_from_select_tool_with_a_plain_selection_does_not_
        // lose_it` 参照)。浮動片だけを確定し、まだ浮動化していないプレーンな
        // 選択は残す(`flush_floating_keep_selection`)よう修正した。
        if matches!(
            self.tool,
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move
        ) {
            self.flush_floating_keep_selection();
        } else {
            self.end_active_gesture();
        }
    }

    /// 現在のツールに進行中のジェスチャ(ドラッグ)があれば、`Up` が来た
    /// 場合と同様に確定して終了する(`set_tool` からのみ呼ぶ)。
    fn end_active_gesture(&mut self) {
        // v3 §19: テキストは `ToolCtx`(`self.active_tab().doc`/`self.active_tab().history` の借用)を
        // 経由しない独自の確定処理を持つ。`ToolCtx` を組み立てる前に分岐する
        // 必要がある — 確定処理自体が `&mut self` を要求するメソッド
        // (`commit_pending_text_edit_and_composite`)を呼ぶため、`ctx` が
        // `self.active_tab().doc`/`self.active_tab().history` を借用したままだと借用チェッカーに
        // 弾かれる。
        if self.tool == ToolKind::Text {
            self.commit_pending_text_edit_and_composite();
            return;
        }
        // v4 §22: なげなわは `Tool`/`ToolCtx` を経由しない独自の進行中状態
        // (`lasso_freehand_points`/`lasso_polygon`)を持つ。ドキュメントには
        // まだ一切触れていない(選択が確定するのは `finish_lasso_points` の
        // 時点)ため、ツール切替時は単に破棄すればよい(SPEC §18 の「先に
        // 確定してから実行」は History ストローク/浮動片が対象であり、
        // なげなわの未確定な軌跡・頂点列はどちらでもない)。破棄せずに残すと、
        // 別ツールへ切り替えて戻ってきたときに古い頂点列へ継ぎ足されてしまう
        // バグになる。
        if self.tool == ToolKind::Lasso {
            self.lasso_freehand_points.clear();
            self.lasso_polygon = None;
            return;
        }
        let mut used_colors = Vec::new();
        // v5 §17.1: `active_tab_mut()` はメソッド呼び出しの向こう側にある
        // ため、これを経由すると借用チェッカーは `*self` 全体が借用中だと
        // 見なし、以後の `self.primary` 等の読み出しと衝突してしまう
        // (Rust はメソッド境界を越えたフィールド単位の非交差借用を認識
        // できない)。ここでは直接 `self.tabs[self.active_tab]` を経由して
        // `Tab` への参照を 1 回だけ取り、以後は `tab.doc`/`tab.history`/
        // `tab.selection` という直接のフィールドパスで分割借用する(同じ
        // 手法を要する箇所すべてに共通)。
        let tab = &mut self.tabs[self.active_tab];
        let mut ctx = ToolCtx {
            doc: &mut tab.doc,
            history: &mut tab.history,
            primary: self.primary,
            secondary: self.secondary,
            brush_size: self.brush_size,
            hardness: self.brush_hardness as f32 / 100.0,
            opacity: self.brush_opacity as f32 / 100.0,
            pencil: self.pencil_mode,
            smoothing: self.brush_smoothing as f32 / 100.0,
            used_colors: &mut used_colors,
            clip: tab.selection.as_ref().map(|s| &s.mask),
        };
        match self.tool {
            ToolKind::Pen => self.pen.cancel(&mut ctx),
            ToolKind::Eraser => self.eraser.cancel(&mut ctx),
            ToolKind::Line => self.line.cancel(&mut ctx),
            ToolKind::Rect => self.rect_tool.cancel(&mut ctx),
            ToolKind::Ellipse => self.ellipse.cancel(&mut ctx),
            // v4 §23: グラデーションもドラッグ状態を持つツール(図形と同じ、
            // ツール切替時は直近のドラッグ位置で確定する)。
            ToolKind::Gradient => self.gradient.cancel(&mut ctx),
            // 塗りつぶし/スポイト/手のひらはドラッグ状態(進行中のジェス
            // チャ)を持たない(塗りつぶしは Down で即座に確定する 1 ショット
            // のツール)。選択・移動は `commit_open_gesture` の分岐で別途
            // 扱う(ここには来ない)。ズームもドラッグ状態を持たない。
            // テキストは上で早期リターン済み(ここには来ない、網羅性のためだけ
            // に列挙する)。
            ToolKind::Fill
            | ToolKind::Picker
            | ToolKind::Select
            | ToolKind::Pan
            | ToolKind::Move
            | ToolKind::Zoom
            | ToolKind::Text
            // v4 §22: `EllipseSelect` は `commit_open_gesture` が Select と
            // 同じ扱い(`commit_selection` 経由)にするため、ここには来ない
            // (網羅性のためだけに列挙)。`MagicWand` は塗りつぶしと同じ
            // 1 ショットのツールでドラッグ状態を持たない。`Lasso` は上で
            // 早期リターン済み(ここには来ない)。
            | ToolKind::EllipseSelect
            | ToolKind::MagicWand
            | ToolKind::Lasso => {}
        }
        for color in used_colors {
            self.push_recent_color(color);
        }
    }

    /// 現在のツールに応じたカーソル形状(手のひらは `Tool` を持たないため
    /// ここで直接返す、ARCHITECTURE.md §4)。`alt_held` は v3 §18 のズーム
    /// ツール用(Alt 押下中は縮小になるので `ZoomOut` を出す)。
    fn cursor_for_active_tool(&self, alt_held: bool) -> egui::CursorIcon {
        match self.tool {
            ToolKind::Pen | ToolKind::Eraser => self.brush_cursor_icon(),
            ToolKind::Line => self.line.cursor(),
            ToolKind::Rect => self.rect_tool.cursor(),
            ToolKind::Ellipse => self.ellipse.cursor(),
            ToolKind::Fill => self.fill.cursor(),
            ToolKind::Gradient => self.gradient.cursor(),
            ToolKind::Picker => self.picker.cursor(),
            ToolKind::Pan => egui::CursorIcon::Grab,
            // v4 §22: `EllipseSelect` は `Select` と同じハンドル/浮動片状態
            // 機械を共有するので、カーソルも同じ規則(ハンドルホバーでリサイズ
            // カーソル)にする。
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move => self.select_cursor(),
            // v4 §22: なげなわ・自動選択は塗りつぶし/スポイトと同じ
            // クロスヘア(ドラッグ中の意匠は `draw_selection_overlay` 側の
            // プレビュー描画に任せる)。
            ToolKind::Lasso | ToolKind::MagicWand => egui::CursorIcon::Crosshair,
            // SPEC §18: 「カーソルは虫眼鏡」。ARCHITECTURE.md §15.2 は
            // ZoomIn/ZoomOut を明示する。
            ToolKind::Zoom => {
                if alt_held {
                    egui::CursorIcon::ZoomOut
                } else {
                    egui::CursorIcon::ZoomIn
                }
            }
            // v3 §19: テキスト。
            ToolKind::Text => egui::CursorIcon::Text,
        }
    }

    /// ブラシ半径(画像座標)をスクリーン論理ポイントへ換算する
    /// (ARCHITECTURE.md §15.1: `半径 = brush_r × zoom / ppp`)。
    fn brush_radius_screen(&self) -> f32 {
        crate::tools::brush_radius(self.brush_size) * self.active_tab().view.zoom
            / self.active_tab().view.ppp()
    }

    /// SPEC §17: 「ブラシカーソル: キャンバス上ではブラシ半径の円アウトライン
    /// …を表示し、OS カーソルは非表示。画面上の円が 3px 未満になる場合は
    /// 十字カーソルにフォールバック」。円自体は `draw_brush_cursor` が描く。
    fn brush_cursor_icon(&self) -> egui::CursorIcon {
        if self.brush_radius_screen() < 3.0 {
            egui::CursorIcon::Crosshair
        } else {
            egui::CursorIcon::None
        }
    }

    /// ブラシ/消しゴム使用中にキャンバス上へ描く円カーソル(白 1.5pt の
    /// 内側に黒 1pt の二重線、SPEC §17)。`cursor_for_active_tool` が
    /// `CursorIcon::None` を返したときだけ意味を持つので、3px 未満の場合と
    /// 同じ条件でここでも描かない(OS カーソル側は十字にフォールバック済み)。
    fn draw_brush_cursor(&self, painter: &egui::Painter, hover_img: Pos2) {
        let radius_screen = self.brush_radius_screen();
        if radius_screen < 3.0 {
            return;
        }
        let center = self.active_tab().view.img_to_screen_pos(hover_img);
        painter.circle_stroke(
            center,
            radius_screen,
            egui::Stroke::new(3.0, egui::Color32::WHITE),
        );
        painter.circle_stroke(
            center,
            radius_screen,
            egui::Stroke::new(1.0, egui::Color32::BLACK),
        );
    }

    /// SPEC §16: 「ハンドルホバー時はリサイズカーソルを表示」。
    /// `self.active_tab().view.hover_img()` は前フレームのホバー位置(ステータスバーと
    /// 同じ 1 フレーム遅延、`status_bar::show` 呼び出し箇所のコメント参照)
    /// だが、連続したポインタ移動で駆動されるため実用上は無視できる。
    fn select_cursor(&self) -> egui::CursorIcon {
        if let Some(SelectDrag::ResizeFloating { handle, .. }) = &self.select_drag {
            return select::handle_cursor(*handle);
        }
        if let Some(hover) = self.active_tab().view.hover_img() {
            if let Some(handle) = self.hit_resize_handle(hover) {
                return select::handle_cursor(handle);
            }
        }
        egui::CursorIcon::Default
    }

    /// キャンバスから出た `ToolEvent` を、Alt+一時スポイト(SPEC §4)または
    /// 現在のツールへディスパッチする。
    ///
    /// v4 レビューで発見・修正したバグ: ARCHITECTURE.md §10「モーダル表示中は
    /// キャンバスへの入力を渡さない」が、`handle_shortcuts`(app.rs 777行)や
    /// `handle_dropped_files` は `self.modal.is_some()` でガードしているのに、
    /// ここ(ポインタイベント経路)にはガードが無かった。`CanvasView::
    /// handle_pointer` の進行中ジェスチャ分岐は生のポインタ状態だけで
    /// Drag/Up を発行し `egui::Modal` のバックドロップ(新規の press にしか
    /// 効かない)をすり抜けるため、ブラシでドラッグ中に Ctrl+U 等でモーダルを
    /// 開き、ボタンを押したままマウスをモーダル上へ動かすと、その軌跡が
    /// モーダルの裏でレイヤーに描画され続けていた。モーダル表示中はここで
    /// 一括して何もディスパッチしない(`CanvasView` 側の内部状態
    /// (`gesture`/`hover_img`)はそのまま更新され続けてよい — パンやカーソル
    /// 追従だけで文書は一切変更しないため、モーダルを閉じた後の操作性を
    /// 損なわない)。
    fn dispatch_canvas_events(&mut self, events: Vec<ToolEvent>) {
        if self.modal.is_some() {
            return;
        }
        for ev in events {
            if let ToolEvent::Down { img, button, mods } = ev {
                // v3 §18: ズームツールは Alt+クリックに「縮小」という独自の
                // 意味を持つ(SPEC §18)ため、他ツール共通の一時スポイト
                // 横取りから除外する。
                if mods.alt && self.tool != ToolKind::Zoom {
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
            // (tools/select.rs のモジュールコメント参照)。v4 §22: 楕円選択
            // (`EllipseSelect`)は矩形選択と全く同じ状態機械を共有する
            // (`handle_select_event` 内部で新規選択確定時のマスク形状だけ
            // `self.tool` を見て切り替える)。
            if matches!(self.tool, ToolKind::Select | ToolKind::EllipseSelect) {
                self.handle_select_event(ev);
                continue;
            }

            // v3 §18: 移動ツールも選択と同じ `Selection`/`Floating` 機構を
            // 使う(`move_down` のみ選択と異なる、それ以外は共有)。
            if self.tool == ToolKind::Move {
                self.handle_move_event(ev);
                continue;
            }

            // v4 §22: なげなわ。自由/多角形のどちらのモードかは
            // `self.lasso_mode` を見て `handle_lasso_event` 内で分岐する。
            if self.tool == ToolKind::Lasso {
                self.handle_lasso_event(ev);
                continue;
            }

            // v4 §22: 自動選択。塗りつぶしと同じ 1 ショットのクリック操作
            // (ドラッグ/プレビューはない)。
            if self.tool == ToolKind::MagicWand {
                if let ToolEvent::Down { img, .. } = ev {
                    self.magic_wand_select(img);
                }
                continue;
            }

            // v3 §18: ズームツール。クリック=+1 段階、Alt+クリック=-1 段階
            // (SPEC §18)。右クリック・中クリックは何もしない(仕様に明記が
            // ないため、独自の挙動を足さない)。
            if self.tool == ToolKind::Zoom {
                if let ToolEvent::Down { img, button, mods } = ev {
                    if button == PointerButton::Primary {
                        let notches = if mods.alt { -1 } else { 1 };
                        self.active_tab_mut().view.zoom_at_point(notches, img);
                    }
                }
                continue;
            }

            // v3 §19: テキストツール。編集中でなければクリックで新規編集を
            // 開始する。編集中に届く Down は「ボックス外クリック」でしか
            // 起こり得ない(ボックス内クリックは `draw_text_edit_overlay` の
            // `Area` が占有するのでここまで届かない)ため、ここで新規編集を
            // 始めてはいけない — その確定は `draw_text_edit_overlay` の
            // `lost_focus()` 判定に任せる(SPEC §19: 「確定…ボックス外
            // クリック」)。二重に処理すると同じクリックで「確定」と「新規
            // 開始」が両方走ってしまう。
            if self.tool == ToolKind::Text {
                if self.text_edit.is_none() {
                    if let ToolEvent::Down { img, .. } = ev {
                        self.begin_text_edit(img);
                    }
                }
                continue;
            }

            let mut used_colors = Vec::new();
            // v5 §17.1: `end_active_gesture` の `ctx` 組み立てと同じ理由で
            // `self.tabs[self.active_tab]` を直接経由する(コメント参照)。
            let tab = &mut self.tabs[self.active_tab];
            let mut ctx = ToolCtx {
                doc: &mut tab.doc,
                history: &mut tab.history,
                primary: self.primary,
                secondary: self.secondary,
                brush_size: self.brush_size,
                hardness: self.brush_hardness as f32 / 100.0,
                opacity: self.brush_opacity as f32 / 100.0,
                pencil: self.pencil_mode,
                smoothing: self.brush_smoothing as f32 / 100.0,
                used_colors: &mut used_colors,
                clip: tab.selection.as_ref().map(|s| &s.mask),
            };
            match self.tool {
                ToolKind::Pen => self.pen.event(ev, &mut ctx),
                ToolKind::Eraser => self.eraser.event(ev, &mut ctx),
                ToolKind::Line => self.line.event(ev, &mut ctx),
                ToolKind::Rect => self.rect_tool.event(ev, &mut ctx),
                ToolKind::Ellipse => self.ellipse.event(ev, &mut ctx),
                ToolKind::Fill => self.fill.event(ev, &mut ctx),
                ToolKind::Gradient => self.gradient.event(ev, &mut ctx),
                // 手のひら(canvas_view が横取り)・選択・移動・ズーム・
                // スポイト・テキスト・楕円選択・なげなわ・自動選択は上で
                // 処理済み。
                ToolKind::Select
                | ToolKind::Pan
                | ToolKind::Picker
                | ToolKind::Move
                | ToolKind::Zoom
                | ToolKind::Text
                | ToolKind::EllipseSelect
                | ToolKind::Lasso
                | ToolKind::MagicWand => {}
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
        self.active_tab_mut().doc.recompose_if_dirty();
        let Some(px) = self.active_tab().doc.composite_pixel(x, y) else {
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

    // -----------------------------------------------------------------
    // v3 §18: 移動ツール(ARCHITECTURE.md §15.2)
    //
    // 「Down 時に選択があれば選択範囲を、なければ全範囲を、既存の浮動化パス
    // (`begin_floating_from_selection`)で浮動化し、以後は既存の浮動片
    // ドラッグと同一コード」。ドラッグ更新(`select_drag_move`)・確定
    // (`select_up`/`commit_selection`)・ハンドル拡縮は選択ツールと完全に
    // 共有する。異なるのは Down の初手だけ: 選択ツールは「クリックが選択
    // 矩形の外なら新規の矩形選択ドラッグを始める」が、移動ツールは矩形を
    // ドラッグで作らず、既存の選択(あれば)またはアクティブレイヤー全体を
    // 問答無用で浮動化して追従を始める(SPEC §18)。
    // -----------------------------------------------------------------

    fn handle_move_event(&mut self, ev: ToolEvent) {
        match ev {
            ToolEvent::Down { img, .. } => self.move_down(img),
            ToolEvent::Drag { img, mods, .. } => self.select_drag_move(img, mods),
            ToolEvent::Up { img, .. } => self.select_up(img),
            ToolEvent::Hover { .. } => {}
        }
    }

    /// アクティブレイヤー全体を覆う画像座標の矩形。
    fn doc_full_rect(&self) -> crate::document::IRect {
        crate::document::IRect {
            x0: 0,
            y0: 0,
            x1: self.active_tab().doc.width as i32,
            y1: self.active_tab().doc.height as i32,
        }
    }

    // -----------------------------------------------------------------
    // v4 §22: なげなわ(自由/多角形)
    // -----------------------------------------------------------------

    fn handle_lasso_event(&mut self, ev: ToolEvent) {
        match self.lasso_mode {
            LassoMode::Freehand => self.handle_lasso_freehand_event(ev),
            // 多角形モードはクリック列で状態を持つ(ドラッグではない)ため
            // `Down` だけを見る(SPEC §22: 「クリックで頂点追加」)。
            LassoMode::Polygon => {
                if let ToolEvent::Down { img, .. } = ev {
                    self.lasso_polygon_click(img);
                }
            }
        }
    }

    /// SPEC §22: 「自由: ドラッグの軌跡を閉じてマスク化」。
    fn handle_lasso_freehand_event(&mut self, ev: ToolEvent) {
        match ev {
            ToolEvent::Down { img, .. } => {
                // v4 §22: 新規選択は既存の選択/浮動片を置き換える。浮動片が
                // あれば先に合成して(通常のツール切替と同じ確定順序)から
                // 新しい軌跡の記録を始める。
                self.commit_selection();
                self.lasso_freehand_points = vec![img];
            }
            ToolEvent::Drag { img, .. } => {
                if !self.lasso_freehand_points.is_empty() {
                    self.lasso_freehand_points.push(img);
                }
            }
            ToolEvent::Up { .. } => {
                let points = std::mem::take(&mut self.lasso_freehand_points);
                self.finish_lasso_points(points);
            }
            ToolEvent::Hover { .. } => {}
        }
    }

    /// SPEC §22: 「多角形: クリックで頂点追加、ダブルクリック/Enter/始点
    /// クリックで閉じる、Esc で中止」。始点クリック・ダブルクリックのどちらも
    /// スクリーン論理ポイント距離で判定する(ズームに関係なく一定の当たり
    /// 判定になる、SPEC §16 のハンドルサイズと同じ考え方)。
    fn lasso_polygon_click(&mut self, img: Pos2) {
        let now = Instant::now();
        let screen_pos = self.active_tab().view.img_to_screen_pos(img);
        if let Some(state) = &mut self.lasso_polygon {
            if state.points.len() >= 3 {
                // `state` は `self.lasso_polygon` を可変借用したままなので、
                // `self.active_tab()`(メソッド呼び出しは `*self` 全体を
                // 借用してしまう)ではなく直接 `self.tabs[..]` を経由して
                // 借用チェッカーに非交差を示す。
                let start_screen = self.tabs[self.active_tab]
                    .view
                    .img_to_screen_pos(state.points[0]);
                if (screen_pos - start_screen).length() <= LASSO_CLOSE_DISTANCE {
                    let points = std::mem::take(&mut state.points);
                    self.lasso_polygon = None;
                    self.finish_lasso_points(points);
                    return;
                }
                if let Some((last_time, last_pos)) = state.last_click {
                    if now.duration_since(last_time) <= LASSO_DOUBLE_CLICK_WINDOW
                        && (screen_pos - last_pos).length() <= LASSO_DOUBLE_CLICK_DISTANCE
                    {
                        // ダブルクリックで閉じる: 2 回目のクリックは新しい
                        // 頂点として追加しない(ほぼ同じ位置の重複頂点を
                        // 避ける)。
                        let points = std::mem::take(&mut state.points);
                        self.lasso_polygon = None;
                        self.finish_lasso_points(points);
                        return;
                    }
                }
            }
            state.points.push(img);
            state.last_click = Some((now, screen_pos));
        } else {
            // v4 §22: 新規選択は既存の選択/浮動片を置き換える。
            self.commit_selection();
            self.lasso_polygon = Some(LassoPolygonState {
                points: vec![img],
                last_click: Some((now, screen_pos)),
            });
        }
    }

    /// Enter(`Action::CommitFloating`)で多角形なげなわを確定する
    /// (SPEC §22: 「Enter…で閉じる」)。進行中でなければ何もしない。
    fn finish_polygon_lasso_if_ready(&mut self) {
        if let Some(state) = self.lasso_polygon.take() {
            self.finish_lasso_points(state.points);
        }
    }

    /// 軌跡・頂点列から選択マスクを作って確定する(自由/多角形どちらの
    /// なげなわも最終的にここへ合流する)。3 点未満(実質的な選択にならない)
    /// なら選択を作らない(矩形選択の「単クリックは選択を残さない」と同じ
    /// 考え方)。
    fn finish_lasso_points(&mut self, points: Vec<Pos2>) {
        let mask = select::polygon_mask(&points)
            .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        self.active_tab_mut().selection = if mask.is_empty() {
            None
        } else {
            Some(Selection::new(mask))
        };
    }

    // -----------------------------------------------------------------
    // v4 §22: 自動選択(マジックワンド)
    // -----------------------------------------------------------------

    /// SPEC §22: 「クリック画素から許容値の連結領域をマスク選択(flood fill
    /// と同じ判定、アクティブレイヤー基準)」。塗りつぶしと同じ 1 ショットの
    /// クリック操作。新規選択は既存の選択/浮動片を置き換える。
    fn magic_wand_select(&mut self, img: Pos2) {
        self.commit_selection();
        let x = img.x.floor() as i32;
        let y = img.y.floor() as i32;
        // `self.magic_wand_tolerance` を同じ文で読むため、`active_tab_mut()`
        // (`*self` 全体を借用してしまうメソッド呼び出し)ではなく直接
        // `self.tabs[..]` を経由する(`lasso_polygon_click` と同じ理由)。
        let surface = self.tabs[self.active_tab].doc.active_surface_mut(None);
        let mask = raster::flood_mask(&surface, x, y, self.magic_wand_tolerance);
        self.active_tab_mut().selection = if mask.is_empty() {
            None
        } else {
            Some(Selection::new(mask))
        };
    }

    fn move_down(&mut self, img: Pos2) {
        if let Some(handle) = self.hit_resize_handle(img) {
            self.begin_resize_handle(handle, img);
            return;
        }
        if let Some(floating) = &self.active_tab().floating {
            // 既に浮動中(前フレームまでの移動が未確定): クリック位置に
            // 関係なくそのまま追従を続ける(SPEC §18: ドラッグでレイヤー/
            // 選択範囲全体を動かす、選択ツールのような「範囲外クリックは
            // 選択扱いしない」という区別は移動ツールには無い)。
            let offset = img - floating.pos;
            self.select_drag = Some(SelectDrag::MoveFloating { offset });
            return;
        }
        // SPEC §18: 「選択があればその範囲だけを移動。空レイヤー(全透明)
        // でも動作(確定時 before==after 抑制が効く)」。`PendingFloating`
        // 経由にすることで、実際にドラッグしなかった単クリックは選択ツール
        // と同じく浮動化せず、undo エントリも積まない
        // (`select_drag_move`/`select_up` の `PendingFloating` 分岐参照)。
        let mask = self
            .active_tab()
            .selection
            .as_ref()
            .map(|s| s.mask.clone())
            .unwrap_or_else(|| select::rect_mask(self.doc_full_rect()));
        self.select_drag = Some(SelectDrag::PendingFloating {
            mask,
            down_img: img,
        });
    }

    /// 選択矩形・浮動片の外周にある矩形(画像座標)。どちらも無ければ `None`
    /// (`self.active_tab().floating`/`self.active_tab().selection` は互いに排他、ARCHITECTURE.md §7)。
    fn current_selection_or_floating_rect(&self) -> Option<crate::document::IRect> {
        if let Some(floating) = &self.active_tab().floating {
            return Some(select::floating_target_rect(floating));
        }
        self.active_tab().selection.as_ref().map(|s| s.mask.bbox)
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
        let screen_rect = self.active_tab().view.img_rect_to_screen(rect);
        let handles = select::handle_rects(screen_rect);
        let screen_pos = self.active_tab().view.img_to_screen_pos(img);
        select::hit_handle(&handles, screen_pos)
    }

    /// ハンドルドラッグを開始する。未浮動の選択でハンドルを掴んだ場合は、
    /// 内部ドラッグと同様にまず浮動化してから拡縮する(SPEC §16)。
    fn begin_resize_handle(&mut self, handle: select::Handle, img: Pos2) {
        if self.active_tab().floating.is_none() {
            let Some(mask) = self.active_tab().selection.as_ref().map(|s| s.mask.clone()) else {
                return;
            };
            self.begin_floating_from_selection(mask, img);
        }
        let Some(floating) = &self.active_tab().floating else {
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
        let Some(floating) = self.active_tab().floating.as_ref() else {
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
            // v4 §16.3/SPEC §16: ピクセルは bilinear、マスクは nearest で、
            // どちらも「浮動化時点の元」(`original`/`orig_mask`)から毎回
            // 再サンプリングする(累積劣化させない)。
            Some((
                select::resample_bilinear(
                    &floating.original,
                    floating.orig_w,
                    floating.orig_h,
                    new_w_px,
                    new_h_px,
                ),
                select::resample_mask_nearest(
                    &floating.orig_mask,
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
        let Some(floating) = self.active_tab_mut().floating.as_mut() else {
            return;
        };
        if let Some((pixels, mask, id)) = resampled {
            floating.pixels = pixels;
            floating.mask = mask;
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
        if let Some(floating) = &self.active_tab().floating {
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
        if let Some(selection) = &self.active_tab().selection {
            if select::point_in_mask(&selection.mask, img) {
                // M4 で発見・修正したバグ: ここで即座に `begin_floating_from_
                // selection` を呼んでいたため、ドラッグせずに離すだけの
                // 単クリックでも浮動化(元領域の透明化+同位置への再合成)が
                // 起き、before==after の無意味な undo エントリが積まれて
                // いた。実際に動いた場合(`select_drag_move`)にのみ浮動化
                // するよう、まずは「保留」状態を記録するだけにする
                // (SPEC §6: 「選択内部をドラッグ→浮動化」)。
                self.select_drag = Some(SelectDrag::PendingFloating {
                    mask: selection.mask.clone(),
                    down_img: img,
                });
                return;
            }
        }
        self.active_tab_mut().selection = None;
        self.select_drag = Some(SelectDrag::NewSelection {
            start: img,
            current: img,
        });
    }

    fn select_drag_move(&mut self, img: Pos2, mods: Modifiers) {
        // v4 §16.3: `PendingFloating` が `SelMask`(`Vec<u8>`)を保持するように
        // なり `SelectDrag` はもう `Copy` にできないため、`self.select_drag`
        // を(一時的に `None` にして)`take()` で取り出し、各アームが必要なら
        // 明示的に書き戻す形にした(以前は `Copy` によって暗黙にコピーを
        // 読んでいた)。
        match self.select_drag.take() {
            Some(SelectDrag::NewSelection { start, .. }) => {
                // SPEC §22: 「Shift ドラッグで正方形/正円」。矩形選択の図形
                // ツールと全く同じ拘束計算(`shapes::snap_square`)を使う。
                let current = if mods.shift {
                    crate::tools::shapes::snap_square(start, img)
                } else {
                    img
                };
                self.select_drag = Some(SelectDrag::NewSelection { start, current });
            }
            Some(SelectDrag::MoveFloating { offset }) => {
                if let Some(floating) = &mut self.active_tab_mut().floating {
                    floating.pos = img - offset;
                }
                self.select_drag = Some(SelectDrag::MoveFloating { offset });
            }
            Some(SelectDrag::PendingFloating { mask, down_img }) => {
                if img != down_img {
                    // 実際に動いた: ここで初めて浮動化する。
                    // `begin_floating_from_selection` は `select_drag` を
                    // `MoveFloating` に設定するので、続けて同じ `img` で
                    // 再度呼び出すことで、浮動片を 1 フレーム遅れず現在位置
                    // まで追従させる。
                    self.begin_floating_from_selection(mask, down_img);
                    self.select_drag_move(img, mods);
                } else {
                    self.select_drag = Some(SelectDrag::PendingFloating { mask, down_img });
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
                self.select_drag = Some(SelectDrag::ResizeFloating {
                    handle,
                    anchor,
                    start_w,
                    start_h,
                    start_center,
                });
            }
            None => {}
        }
    }

    fn select_up(&mut self, img: Pos2) {
        match self.select_drag.take() {
            Some(SelectDrag::NewSelection { start, current }) => {
                // v2 レビューで発見・修正したバグ: `irect_from_points` は
                // floor/ceil で外側に丸めるため、`start`/終点の画像座標が
                // 整数ちょうどでない限り(高 DPI スケーリングや 100% 以外の
                // ズーム、端数パンでは頻繁に起こる)、ドラッグせずに離した
                // だけの単クリックでも幅・高さ 1 の非空矩形が残ってしまって
                // いた(SPEC §6: 「ドラッグで矩形選択」、単クリックは選択を
                // 残さないのが期待動作)。
                //
                // v4 §22: `current` は `select_drag_move`(Drag イベント)が
                // Shift 拘束(正方形/正円)込みで更新する値。ただし `Down`→
                // `Up` の間に一度も `Drag` が届かない(1 フレームに満たない
                // 高速なクリック&ドラッグ)場合は `current` が `start` の
                // ままになるため、その場合だけ `Up` の生のポインタ位置
                // `img`(Shift 拘束はできないが、従来どおりの矩形になる)を
                // 使う。`Drag` が 1 回でも届いていれば(`current != start`)
                // Shift 拘束済みの `current` を優先する(`Up` イベント自体は
                // Shift の状態を運ばないため、`apply_resize_floating` と
                // 同じ「離す瞬間の数ピクセルのズレは無視する」割り切り)。
                let end = if current != start { current } else { img };
                self.active_tab_mut().selection = if end == start {
                    None
                } else {
                    // v4 レビューで発見・修正したバグ: 以前はここで先に
                    // `irect_from_points(..).clamp_to(doc)` と矩形をキャン
                    // バス境界へクランプしてから `ellipse_mask` に渡していた。
                    // 矩形選択はクランプ=クリップで同値だが、楕円は
                    // 「クランプ後の(小さい)矩形に内接する別の楕円」に
                    // なってしまい、`raster::fill_ellipse`(非クランプの
                    // 外接矩形から楕円方程式を評価し、はみ出し分だけ切り
                    // 落とす)と選択の楕円の画素集合が、ドラッグがキャン
                    // バス境界を跨ぐ場合に食い違っていた(select.rs の
                    // `ellipse_mask` ドキュメントコメントが保証する「同じ
                    // 外接矩形なら図形と選択の楕円が画素単位で一致する」が
                    // 破れる、SPEC §22 の見た目一致に反する)。
                    // `begin_floating_from_selection` と同じ「先に作って
                    // から `SelMask::clamp_to` でクリップする」順序に直す。
                    let rect = select::irect_from_points(start, end);
                    // SPEC §22: 楕円選択ツールなら楕円マスク、それ以外
                    // (矩形選択)は従来どおり矩形マスク。
                    let mask = if self.tool == ToolKind::EllipseSelect {
                        select::ellipse_mask(rect)
                    } else {
                        select::rect_mask(rect)
                    };
                    let mask =
                        mask.clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
                    if mask.is_empty() {
                        None
                    } else {
                        Some(Selection::new(mask))
                    }
                };
            }
            Some(SelectDrag::MoveFloating { offset }) => {
                if let Some(floating) = &mut self.active_tab_mut().floating {
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

    /// 選択内部をドラッグ開始 = 浮動化(SPEC §6、v4 §16.3: マスク形状のまま)。
    /// `mask` の画素だけ `Floating` に複写し、元領域も `mask` の画素だけ透明化
    /// する。この透明化は History のストロークを開いたまま(まだ push しない)
    /// にしておき、確定時(`commit_selection`)に「切り出し元の透明化+合成先」
    /// をまとめて 1 つの `Patch` にする(ARCHITECTURE.md §7)。
    fn begin_floating_from_selection(&mut self, mask: crate::document::SelMask, img: Pos2) {
        let mask = mask.clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        let rect = mask.bbox;
        if rect.is_empty() {
            self.active_tab_mut().selection = None;
            return;
        }
        // `history`/`doc` を同時に触るため、`self.tabs[..]` を直接経由する
        // (`active_tab_mut()` を複数回呼ぶと `*self` の二重可変借用になる、
        // `end_active_gesture` のコメント参照)。
        let tab = &mut self.tabs[self.active_tab];
        tab.history.begin_stroke(tab.doc.active);
        tab.history.ensure_tiles_saved(&tab.doc, rect);
        let pixels = select::extract_region(&tab.doc, &mask);
        select::clear_region_transparent(&mut tab.doc, &mask);
        tab.doc.modified = true;

        let pos = pos2(rect.x0 as f32, rect.y0 as f32);
        let id = self.alloc_floating_id();
        let mask_bits = mask.mask.clone();
        self.active_tab_mut().floating = Some(Floating::new(
            pixels,
            rect.width() as u32,
            rect.height() as u32,
            mask_bits,
            pos,
            Some(mask),
            id,
        ));
        self.active_tab_mut().selection = None;
        let offset = img - pos;
        self.select_drag = Some(SelectDrag::MoveFloating { offset });
    }

    fn alloc_floating_id(&mut self) -> u64 {
        self.next_floating_id += 1;
        self.next_floating_id
    }

    /// 浮動片を現在位置に合成して 1 つの undo 単位にし、選択を解除する
    /// (SPEC §6: Enter/選択外クリック/ツール切替での確定、Ctrl+D。v3 §18 で
    /// Esc はここではなく `cancel_floating` に切り替わった)。浮動片が無い
    /// (単なる矩形選択だけ、または何も無い)場合は選択を解除するだけ。
    fn commit_selection(&mut self) {
        self.flush_floating_keep_selection();
        self.active_tab_mut().selection = None;
    }

    /// `commit_selection` から浮動片の確定処理だけを切り出したもの
    /// (`self.active_tab().selection` はクリアしない)。SPEC §21 の「選択がある間は他
    /// ツールの描画をクリップし続ける」を満たすため、まだ浮動化していない
    /// プレーンな選択を保持したまま浮動片だけを確定したい呼び出し元
    /// (`commit_open_gesture`、`free_transform` と同じ理由)向け。
    fn flush_floating_keep_selection(&mut self) {
        self.select_drag = None;
        if let Some(floating) = self.active_tab_mut().floating.take() {
            let target = select::floating_target_rect(&floating);
            // ARCHITECTURE.md §18.3: この浮動片がどう生成されたか(選択の
            // 移動/貼り付け/テキスト)によってラベルが変わるため、
            // `Floating::label`(生成時に決まる)をそのまま使う。
            let label = floating.label;
            let tab = &mut self.tabs[self.active_tab];
            tab.history.ensure_tiles_saved(&tab.doc, target);
            select::composite_floating(&mut tab.doc, &floating);
            tab.history.commit_stroke(&mut tab.doc, label);
        }
    }

    /// SPEC §18(v1 §6 を上書き): Esc = キャンセル。浮動片を破棄して元の
    /// 位置・内容に完全復元し(切り出し元も戻す)、選択を解除する。履歴には
    /// 何も積まない。
    ///
    /// `commit_selection` と対になる終了経路: `commit_selection` は浮動片を
    /// 現在位置へ合成して 1 undo 単位にするが、こちらは合成せずに捨てる。
    /// `Floating::cut_from` が `Some` なら、浮動化した瞬間に
    /// `ensure_tiles_saved` で退避しておいた CoW タイルから元ピクセルを
    /// 書き戻す(`History::restore_stroke_region`)。クリップボードからの
    /// 貼り付け(`cut_from == None`)は戻すべき元領域が無いので、単に
    /// ストロークを破棄するだけでよい。
    fn cancel_floating(&mut self) {
        self.select_drag = None;
        if let Some(floating) = self.active_tab_mut().floating.take() {
            if let Some(cut_from) = floating.cut_from {
                // v4 §16.3: `cut_from` は `SelMask` になったが、復元は
                // `bbox` 全体をタイルから一括コピーするだけでよい —
                // マスク外の画素は浮動化時に一切変更していない(`SelMask`
                // の画素だけ透明化する `clear_region_transparent`)ため、
                // bbox 全体を復元してもマスク外は「既に元の値のまま」で
                // 変化しない(ARCHITECTURE.md §16.1 のタイル一括コピーと
                // 同じ考え方を維持できる)。
                let tab = &mut self.tabs[self.active_tab];
                tab.history
                    .restore_stroke_region(&mut tab.doc, cut_from.bbox);
            }
        }
        self.active_tab_mut().history.cancel_stroke();
        self.active_tab_mut().selection = None;
    }

    /// v4 レビューで発見・修正したバグ: `Action`/`MenuAction` の
    /// Undo/Redo は `commit_open_gesture` 後に `history.undo`/`redo` を
    /// 呼ぶだけで、`self.active_tab().selection` のクランプ/解除を一切行っていなかった。
    /// `HistoryOp::ReplaceAll`(サイズ変更/キャンバスサイズ変更/トリミング/
    /// 回転)を undo/redo するとドキュメント寸法が変わるが、選択はそのまま
    /// 残るため、古い(縮んだ後の寸法から見て範囲外の)座標を指した選択が
    /// 残ってしまう。以後はブラシ/塗りつぶし/グラデーション/色調補正が
    /// すべて `ToolCtx::clip` 経由で `SelMask::contains` を通すため、選択
    /// bbox が文書の外にはみ出していると全画素が「選択外」判定になり、
    /// エラーも出ずに 1 画素も描けなくなる(`SelMask::clamp_to` のドキュメント
    /// コメント参照)。Undo/Redo の直後に必ずこれを呼び、新しい寸法へ
    /// クランプする(空になれば選択解除。`begin_floating_from_selection` と
    /// 同じ「作ってからクリップ」の安全弁パターン)。寸法が変わらない
    /// 一般的な undo/redo(ブラシの Patch 等)ではクランプは恒等写像になり
    /// コストもほぼゼロ。
    fn clamp_selection_to_doc(&mut self) {
        if let Some(selection) = &self.active_tab().selection {
            let clamped = selection
                .mask
                .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
            self.active_tab_mut().selection = if clamped.is_empty() {
                None
            } else {
                Some(Selection::new(clamped))
            };
        }
    }

    /// v6-M3(SPEC §35、ARCHITECTURE.md §18.4): 履歴パネルの行クリック(複数
    /// 手順のジャンプ)。安全規則は既存の Undo/Redo(`Action::Undo`/`Redo`、
    /// 上の `handle_shortcuts`/`handle_menu_action` 参照)と**全く同じ**
    /// 順序(`commit_open_gesture()` で進行中のストローク・浮動片を先に
    /// 確定 → `History` を操作 → `clamp_selection_to_doc()`)にする
    /// (ARCHITECTURE.md §18.6-1: 「ジャンプだけを特別扱いして安全確認を
    /// 省略しない」「ジャンプ専用の別ルートを作らない」)。
    fn jump_history_to(&mut self, target_len: usize) {
        self.commit_open_gesture();
        let tab = self.active_tab_mut();
        tab.history.jump_to(&mut tab.doc, target_len);
        self.clamp_selection_to_doc();
    }

    /// SPEC §18: Ctrl+T(自由変形)。選択範囲があれば浮動化してハンドル表示。
    /// なければ全選択→アクティブレイヤーを浮動化してハンドル表示。以降は
    /// §16(ハンドルドラッグ)・§18(Esc キャンセル)と同じ操作になる。
    fn free_transform(&mut self) {
        // 進行中のジェスチャを先に確定する。ただし選択/移動ツールで「まだ
        // 浮動化していないプレーンな選択」がある場合は、Ctrl+T がまさに
        // それを対象にするため、`commit_open_gesture`/`commit_selection`
        // (常に `self.active_tab().selection` をクリアしてしまう)を経由させずに残す
        // (ARCHITECTURE.md §15.2: 「選択範囲があれば浮動化して」を壊さない
        // ため)。
        match self.tool {
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move
                if self.active_tab().floating.is_some() =>
            {
                self.commit_selection();
            }
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move => {
                self.select_drag = None;
            }
            ToolKind::Lasso => {
                self.lasso_freehand_points.clear();
                self.lasso_polygon = None;
            }
            _ => self.end_active_gesture(),
        }
        self.tool = ToolKind::Select;

        let mask = self
            .active_tab()
            .selection
            .as_ref()
            .map(|s| s.mask.clone())
            .unwrap_or_else(|| select::rect_mask(self.doc_full_rect()));
        let mask = mask.clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        if mask.is_empty() {
            return;
        }
        let anchor = pos2(mask.bbox.x0 as f32, mask.bbox.y0 as f32);
        self.begin_floating_from_selection(mask, anchor);
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
    ///
    /// ARCHITECTURE.md §18.3 の対応表: 「Delete による消去」→「削除」。
    /// Ctrl+X はラベルだけ異なる(「切り取り」)ため、実処理は
    /// `delete_selection_labeled` に共通化し、こちらはその既定ラベル版の
    /// 薄いラッパー(`cut_selection_to_clipboard` 参照)。
    fn delete_selection(&mut self) {
        self.delete_selection_labeled("削除");
    }

    fn delete_selection_labeled(&mut self, label: &'static str) {
        self.end_active_gesture();
        self.select_drag = None;
        if self.active_tab_mut().floating.take().is_some() {
            let tab = &mut self.tabs[self.active_tab];
            tab.history.commit_stroke(&mut tab.doc, label);
            self.active_tab_mut().selection = None;
            return;
        }
        if let Some(selection) = self.active_tab_mut().selection.take() {
            let mask = selection
                .mask
                .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
            if !mask.is_empty() {
                let tab = &mut self.tabs[self.active_tab];
                tab.history.begin_stroke(tab.doc.active);
                tab.history.ensure_tiles_saved(&tab.doc, mask.bbox);
                select::clear_region_transparent(&mut tab.doc, &mask);
                tab.history.commit_stroke(&mut tab.doc, label);
            }
        }
    }

    /// 現在の選択(浮動片優先)の画素を取得する。無ければ `None`。
    fn selected_pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
        if let Some(floating) = &self.active_tab().floating {
            return Some((floating.w, floating.h, floating.pixels.clone()));
        }
        if let Some(selection) = &self.active_tab().selection {
            let mask = selection
                .mask
                .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
            if mask.is_empty() {
                return None;
            }
            let rect = mask.bbox;
            return Some((
                rect.width() as u32,
                rect.height() as u32,
                select::extract_region(&self.active_tab().doc, &mask),
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
            // ARCHITECTURE.md §18.3 の対応表: 「切り取り」(Delete による
            // 消去の「削除」とはラベルだけ異なる、`delete_selection` 参照)。
            self.delete_selection_labeled("切り取り");
        }
    }

    /// Ctrl+A(SPEC §6: 全選択)。既存の浮動片は先に確定する。
    fn select_all(&mut self) {
        self.commit_selection();
        if self.active_tab().doc.width == 0 || self.active_tab().doc.height == 0 {
            self.active_tab_mut().selection = None;
            return;
        }
        self.active_tab_mut().selection =
            Some(Selection::new(select::rect_mask(crate::document::IRect {
                x0: 0,
                y0: 0,
                x1: self.active_tab().doc.width as i32,
                y1: self.active_tab().doc.height as i32,
            })));
    }

    /// SPEC §6: 「ドキュメントが完全に未編集・未保存(起動直後の白紙)」の
    /// 判定。パスが無く(新規保存されていない)、かつ一度も編集されていない
    /// (`History::commit_stroke` が実際に何かを push したことがない)ことを
    /// もって「白紙」とみなす。
    fn doc_is_pristine(&self) -> bool {
        self.active_tab().doc.path.is_none() && !self.active_tab().doc.modified
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
        let center = self.active_tab().view.view_center_img();
        let pos = pos2(center.x - w as f32 / 2.0, center.y - h as f32 / 2.0);
        // ARCHITECTURE.md §18.3 の対応表: 「貼り付け」。
        self.place_new_floating(pos, w, h, pixels, "貼り付け");
    }

    /// 新規コンテンツ(クリップボード貼り付け・v3 §19 のテキストラスタライズ)
    /// を「切り出し元を持たない」浮動片として配置する共通処理
    /// (`begin_paste_floating` から抽出。挙動は元のコードと同一)。ツールを
    /// 選択に切り替えることで、以後は既存の浮動片ハンドリング(移動・
    /// ハンドル拡縮・Enter確定・Esc破棄)にそのまま乗る(上の
    /// `begin_paste_floating` のコメント参照)。
    ///
    /// `self.tool = ToolKind::Select` は **`set_tool` 経由ではなく直接代入**
    /// する。テキストツールの `commit_pending_text_edit`(Ctrl+Enter/ボックス
    /// 外クリックの通常確定)はここを問題なく通れるが、もし将来
    /// `end_active_gesture`(`set_tool`/`commit_open_gesture` の内側)から
    /// 呼ばれる経路が増えた場合、`set_tool` 経由だと再入(`commit_open_
    /// gesture` の呼び出し元が後で `self.tool = 元々要求されたツール` を
    /// 上書きしてしまう)が起きる(`free_transform` が同じ理由で直接代入して
    /// いるのと同じ落とし穴)。呼び出し側は既に先行ジェスチャを確定済みで
    /// あることが前提。
    ///
    /// `label` は確定時に History へ積む undo ラベル(ARCHITECTURE.md
    /// §18.3: 貼り付けは「貼り付け」、テキストは「テキスト」)。
    /// `Floating::label` に載せておき、実際の commit は
    /// `flush_floating_keep_selection` がそれを読んで行う。
    fn place_new_floating(
        &mut self,
        pos: Pos2,
        w: u32,
        h: u32,
        pixels: Vec<u8>,
        label: &'static str,
    ) {
        let id = self.alloc_floating_id();
        self.tool = ToolKind::Select;
        // 切り出し元が無いので `begin_stroke` するだけで `ensure_tiles_saved`
        // は呼ばない(confirm 時に合成先だけ保存すれば十分、
        // `commit_selection` 参照)。
        let tab = &mut self.tabs[self.active_tab];
        tab.history.begin_stroke(tab.doc.active);
        self.active_tab_mut().floating =
            Some(Floating::new_rect(pixels, w, h, pos, None, id).with_label(label));
        self.active_tab_mut().selection = None;
        // M4 で発見・修正したバグ: 浮動片は画面に見えている未保存の変更
        // だが、以前は `commit_selection` で合成されるまで `doc.modified` が
        // 立たなかった。このため貼り付け直後にウィンドウを閉じる/新規/開く
        // (`handle_close_request`/`request_action` はいずれも
        // `doc.modified` だけを見る)と、確認なしに貼り付け内容が破棄
        // されていた(SPEC §8 の未保存ガードの趣旨に反する)。
        self.active_tab_mut().doc.modified = true;
    }

    /// SPEC §6: 白紙時の置き換え貼り付け。ドキュメント全体を貼り付け画像の
    /// サイズに置き換える(スクリーンショット→保存が最短になるように)。
    /// SPEC §13: 新規作成直後と同様「背景」レイヤー 1 枚になる。
    fn replace_document_with_pasted_image(&mut self, w: u32, h: u32, pixels: Vec<u8>) {
        // バグ修正: レイヤー名編集中の入力を確定する処理は、呼び出し元の
        // `paste_pixels` が先頭で呼ぶ `commit_open_gesture()`(実体は
        // `commit_pending_layer_rename`。ドキュメントコメント参照)が既に
        // 行っている。そこで確定されて `doc.modified` が立てば
        // `doc_is_pristine()` が偽になり、そもそもこの関数(白紙置き換え)
        // ではなく `begin_paste_floating` の経路を通るようになる —
        // 「編集中のレイヤー名」も §6 の「完全に未編集」の判定に含まれる
        // べきものとして扱われる。
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut()
            .doc
            .replace_with_single_layer(w, h, pixels);
        self.active_tab_mut().doc.modified = true;
        let after = self.active_tab().doc.snapshot();
        // ARCHITECTURE.md §18.3 の対応表: これも実質「貼り付け」確定
        // (SPEC §6 の白紙置き換え貼り付け)。
        self.active_tab_mut()
            .history
            .push(HistoryOp::ReplaceAll { before, after }, "貼り付け");
        self.active_tab_mut().next_layer_number = 1;
        self.reset_tool_state_for_new_document();
    }

    fn draw_selection_overlay(&mut self, painter: &egui::Painter) {
        if let Some(SelectDrag::NewSelection { start, current }) = &self.select_drag {
            let rect = select::irect_from_points(*start, *current);
            self.active_tab().view.draw_selection_outline(painter, rect);
            return;
        }
        // v4 §22: なげなわの進行中の軌跡/頂点列(確定前のプレビュー)。
        if self.tool == ToolKind::Lasso {
            if !self.lasso_freehand_points.is_empty() {
                self.active_tab()
                    .view
                    .draw_lasso_preview(painter, &self.lasso_freehand_points);
                return;
            }
            if let Some(state) = &self.lasso_polygon {
                self.active_tab()
                    .view
                    .draw_lasso_preview(painter, &state.points);
                return;
            }
        }
        // `draw_floating` は `&mut CanvasView` を要求する一方、`floating` は
        // 同じ `Tab` の別フィールドを不変借用したまま参照し続けるため、
        // `self.tabs[..]` を直接経由して単一の `&mut Tab` からフィールドを
        // 分割借用する(`active_tab()`/`active_tab_mut()` を混在させると
        // `*self` の借用が競合する)。
        let tab = &mut self.tabs[self.active_tab];
        if let Some(floating) = tab.floating.as_ref() {
            tab.view.draw_floating(painter, floating);
            let bounds = select::floating_target_rect(floating);
            tab.view.draw_selection_outline(painter, bounds);
            tab.view.draw_resize_handles(painter, bounds);
            return;
        }
        if let Some(selection) = &self.active_tab().selection {
            // v4 §16.3: 矩形限定の `draw_selection_outline` ではなく、選択
            // 確定時に 1 回だけ計算済みのマスク境界線分(`Selection::
            // boundary`)を描く(既存の矩形選択は 4 本の線分になるので見た目
            // は変わらない、ARCHITECTURE.md §16.10-1)。
            self.active_tab()
                .view
                .draw_selection_mask_outline(painter, &selection.boundary);
            self.active_tab()
                .view
                .draw_resize_handles(painter, selection.mask.bbox);
        }
    }

    /// ステータスバーの「選択サイズ」欄(SPEC §3)。浮動片があればその
    /// サイズ、無ければ選択矩形のサイズ。
    fn current_selection_size(&self) -> Option<(u32, u32)> {
        if let Some(floating) = &self.active_tab().floating {
            return Some((floating.w, floating.h));
        }
        self.active_tab()
            .selection
            .as_ref()
            .map(|s| (s.mask.bbox.width() as u32, s.mask.bbox.height() as u32))
    }

    // -----------------------------------------------------------------
    // v3 §19: テキストツール(ARCHITECTURE.md §15.3)
    // -----------------------------------------------------------------

    /// キャンバスクリックで新規のテキスト編集を開始する(SPEC §19:
    /// 「クリック位置=テキストボックスの左上」)。フォントが読み込めていない
    /// (`self.text_font.is_none()`)場合は編集を始めても最終的に何もラスタ
    /// ライズできないため、その場でトーストを出して編集自体を始めない
    /// (パニックしない、CLAUDE.md 鉄則。編集を許してしまうと「打てるのに
    /// 確定しても何も起きない」という分かりにくい行き止まりになる)。
    fn begin_text_edit(&mut self, img: Pos2) {
        if self.text_font.is_none() {
            self.show_toast(
                "日本語フォントが見つからないため、テキストツールを使用できません".to_owned(),
            );
            return;
        }
        self.text_edit = Some(TextEditState {
            pos: img,
            buffer: String::new(),
            needs_focus: true,
        });
    }

    /// SPEC §19: 「Esc は入力破棄」。ラスタライズせず、履歴にも何も積まない。
    fn discard_pending_text_edit(&mut self) {
        self.text_edit = None;
    }

    /// テキスト編集中の Ctrl+Enter(確定)/Esc(破棄)。`ctx.egui_wants_
    /// keyboard_input()` を見る他のショートカットハンドラとは逆に、
    /// 「編集中でなければ何もしない」だけをガードにする(編集中は
    /// `TextEdit` がフォーカスを持つので `wants_keyboard_input()` は真になり
    /// 他のハンドラは自動的に無効化される。ここはそのフォーカスを持つ本人
    /// のためのハンドラなので、同じガードを使ってはいけない)。
    fn handle_text_edit_shortcuts(&mut self, ctx: &egui::Context) {
        if self.text_edit.is_none() {
            return;
        }
        let commit_shortcut = KeyboardShortcut::new(Modifiers::CTRL, Key::Enter);
        let cancel_shortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Escape);
        let (commit, cancel) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&commit_shortcut),
                i.consume_shortcut(&cancel_shortcut),
            )
        });
        // Ctrl+Enter を先に判定・消費する(素の Enter は複数行入力の改行に
        // 使うため、`TextEdit` 自身に渡さなければならない。ここで消費するのは
        // Ctrl 修飾つきの Enter イベントだけなので、素の改行入力は影響を
        // 受けない)。
        if commit {
            self.commit_pending_text_edit();
        } else if cancel {
            self.discard_pending_text_edit();
        }
    }

    /// テキスト編集の内容をラスタライズする(SPEC §19: 「空文字列の確定は
    /// 何もしない」)。フォント未読み込み・レイアウト結果が空(空白のみ等)
    /// なら `None`。色は確定時点の `self.primary`(編集中の色変更をそのまま
    /// 反映する、`draw_text_edit_overlay` のプレビューと同じ色)。
    fn rasterize_pending_text(&mut self, text: &str) -> Option<(u32, u32, Vec<u8>)> {
        if text.is_empty() {
            return None;
        }
        let Some(font_bytes) = self.text_font.clone() else {
            self.show_toast(
                "日本語フォントが見つからないため、テキストを描画できません".to_owned(),
            );
            return None;
        };
        let rgba = color_to_straight_rgba(self.primary);
        let (w, h, pixels) = text::rasterize_text(&font_bytes, text, self.text_font_size, rgba);
        if w == 0 || h == 0 {
            None
        } else {
            Some((w, h, pixels))
        }
    }

    /// SPEC §19 の通常確定(Ctrl+Enter または ボックス外クリック): ラスタ
    /// ライズして**浮動片として配置**する(移動・ハンドル拡縮可、Enter 等で
    /// 通常確定=1 undo 単位、既存の `Floating` 機構をそのまま使う)。
    fn commit_pending_text_edit(&mut self) {
        let Some(state) = self.text_edit.take() else {
            return;
        };
        let Some((w, h, pixels)) = self.rasterize_pending_text(&state.buffer) else {
            return;
        };
        // ARCHITECTURE.md §18.3 の対応表: 「テキスト」。
        self.place_new_floating(state.pos, w, h, pixels, "テキスト");
        self.push_recent_color(self.primary);
    }

    /// ツール切替(ツールバークリック等)でテキスト編集が中断された場合の
    /// 確定。SPEC §19 は「Ctrl+Enter またはボックス外クリック」でしか確定を
    /// 定めていないが、他のツール(選択/移動の浮動片、ペン等のストローク)は
    /// 「ツール切替=進行中のジェスチャを 1 undo 単位として確定する」という
    /// 一貫した規則に従っている(`Tool::cancel` のドキュメントコメント参照)
    /// ため、テキストもそれに合わせる。ただし通常確定と違い**浮動片にはせず
    /// 直接レイヤーへ合成**する(ユーザーは既に別のツールへ意識を移して
    /// いるので、宙ぶらりんの浮動片を残さない、選択/移動ツールでの
    /// 「ツール切替=浮動片を確定」と同じ扱い、`commit_selection` 参照)。
    ///
    /// `end_active_gesture`(`set_tool`/`commit_open_gesture` の内側)からのみ
    /// 呼ばれる。**`self.tool`/`set_tool` に一切触れてはいけない** —
    /// 再入すると呼び出し元の `set_tool` が後で `self.tool` を上書きして
    /// しまう(`place_new_floating` のコメントと同じ落とし穴)。そのため
    /// ここは `place_new_floating` を経由せず、`Floating`/`select::
    /// composite_floating` を直接使って合成する。
    fn commit_pending_text_edit_and_composite(&mut self) {
        let Some(state) = self.text_edit.take() else {
            return;
        };
        let Some((w, h, pixels)) = self.rasterize_pending_text(&state.buffer) else {
            return;
        };
        // id は合成後すぐ破棄する使い捨ての `Floating` なので値は問わない
        // (`canvas_view` のテクスチャキャッシュには載らない)。
        let floating = Floating::new_rect(pixels, w, h, state.pos, None, 0);
        let target = select::floating_target_rect(&floating);
        let tab = &mut self.tabs[self.active_tab];
        tab.history.begin_stroke(tab.doc.active);
        tab.history.ensure_tiles_saved(&tab.doc, target);
        select::composite_floating(&mut tab.doc, &floating);
        // ARCHITECTURE.md §18.3 の対応表: 「テキスト」。
        tab.history.commit_stroke(&mut tab.doc, "テキスト");
        tab.doc.modified = true;
        self.push_recent_color(self.primary);
    }

    /// テキスト編集中のインラインオーバーレイ(SPEC §19: 「クリック位置に
    /// インラインのテキスト入力ボックス(egui TextEdit、複数行、IME
    /// 対応)を表示」)。呼び出し順は `dispatch_canvas_events` の**後**
    /// (`ui()` 内の呼び出し箇所のコメント参照)。
    fn draw_text_edit_overlay(&mut self, ui: &mut egui::Ui) {
        let Some(state) = self.text_edit.take() else {
            return;
        };
        let TextEditState {
            pos,
            mut buffer,
            needs_focus,
        } = state;
        let screen_pos = self.active_tab().view.img_to_screen_pos(pos);
        // ARCHITECTURE.md §15.3: 「表示フォントサイズ ≈ size × zoom / ppp
        // (プレビューは近似で可、上限あり)」。
        let display_size = (self.text_font_size * self.active_tab().view.zoom
            / self.active_tab().view.ppp())
        .clamp(TEXT_PREVIEW_MIN_PX, TEXT_PREVIEW_MAX_PX);
        let color = self.primary;

        let mut lost_focus = false;
        let mut area = egui::Area::new(egui::Id::new("darask_text_edit_area"))
            .fixed_pos(screen_pos)
            // Foreground: キャンバス(Middle)より確実に上に描き、かつ
            // その領域だけクリックを占有させる(SPEC §19: 「ボックス外
            // クリック」で確定 ⇔ ボックス内クリックは編集続行)。
            .order(egui::Order::Foreground);
        let viewport = self.active_tab().view.viewport_rect();
        if viewport.width() > 0.0 && viewport.height() > 0.0 {
            // v3 レビューで発見・修正したバグ: `constrain` を指定しないと
            // egui 0.35 の既定 `constrain_to(ctx.content_rect())`
            // (ウィンドウ全域)になり、キャンバス右端・下端付近をクリック
            // するとボックスがクリック位置(=確定時のラスタライズ位置、
            // SPEC §19「クリック位置=テキストボックスの左上」)から見た目
            // 上ずれて表示され、ツールバー・右パネルの上にも被さり得る。
            // 中央キャンバスの viewport だけへ constrain することで、常に
            // キャンバス内に収まるようにする。
            area = area.constrain_to(viewport);
        }
        area.show(ui.ctx(), |ui| {
            let response = ui.add(
                egui::TextEdit::multiline(&mut buffer)
                    .frame(egui::Frame::NONE)
                    .font(egui::FontId::proportional(display_size))
                    .text_color(color)
                    // SPEC §19 のラスタライズは `\n` 区切りの明示的な
                    // 改行のみで行う(自動折り返しはしない)。プレビュー
                    // 側で意図しない折り返しが起きないよう十分広く取る。
                    .desired_width(f32::INFINITY)
                    .id(egui::Id::new("darask_text_edit_box")),
            );
            // 生成直後の 1 フレームだけフォーカスを要求する
            // (`TextEditState::needs_focus` のコメント参照)。
            if needs_focus {
                response.request_focus();
            }
            lost_focus = response.lost_focus();
        });

        self.text_edit = Some(TextEditState {
            pos,
            buffer,
            needs_focus: false,
        });
        if lost_focus {
            self.commit_pending_text_edit();
        }
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

    /// D&D でファイルが落とされたら新規タブとして開く(SPEC §30: 「ドラッグ
    /// &ドロップ…からのオープンも同じ規則に従う。複数ファイルを同時ドロップ
    /// した場合は複数タブを開く」)。新規タブの追加は既存タブを破壊しない
    /// ため、v1〜v4 と異なり未保存ガードは不要(`open_path_in_new_tab` 参照)。
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            // 他のモーダル(新規/サイズ変更ダイアログ等)が表示中に新規タブを
            // 追加するとモーダルの状態と食い違うため、何もしない(モーダルを
            // 閉じてから再度ドロップしてもらう)。
            return;
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for path in dropped.into_iter().filter_map(|f| f.path) {
            self.open_path_in_new_tab(path);
        }
    }

    /// ウィンドウの閉じる要求(SPEC §8: 未保存変更ガード、v5 §17.4: 「タブ
    /// ごとに順番に確認ダイアログを出す」)。`close_requested` は検知した
    /// フレーム内で即座に `ViewportCommand::CancelClose` を送る必要がある
    /// (ARCHITECTURE.md §12-2)。**どのタブにも**変更が無ければキャンセル
    /// せずそのまま閉じさせる。
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested {
            return;
        }
        // v3 レビューで発見・修正したバグ: テキスト編集中(まだ確定して
        // いない入力ボックス)は `begin_text_edit`/入力中のバッファ更新の
        // どちらも `doc.modified` を立てないため、以前はここで即座に
        // `!self.active_tab().doc.modified` を見て未保存ガードを素通りし、確認なしに
        // 入力中のテキストが失われていた(SPEC §8 の未保存ガードの趣旨に
        // 反する)。他の「先に確定してから実行」規則(SPEC §13 最終項、
        // `commit_open_gesture` のドキュメントコメント参照)と同じく、
        // `doc.modified` を見る前にここで確定させる。
        self.commit_open_gesture();
        // v5 §17.4: 「ウィンドウを閉じる操作は、未保存のタブがあれば
        // タブごとに順番に確認ダイアログを出す」— アクティブタブだけでなく
        // **全タブ**を見る(v1〜v4 は単一ドキュメントだったので
        // `active_tab().doc.modified` だけで足りていたが、v5 ではそれだけ
        // だと非アクティブな未保存タブが確認なしに失われてしまう)。
        if !self.tabs.iter().any(|tab| tab.doc.modified) {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.modal.is_none() {
            self.begin_quit();
        } else if self.pending_action.is_none() {
            // M4 で発見・修正したバグ: 以前は別のモーダル(画像サイズ変更等)
            // が表示中に閉じる要求が来ると、ここで何もせずに握りつぶして
            // いた(`CancelClose` は送るのでプロセスは終了しないが、
            // ユーザーには「閉じられない理由」が一切示されず、そのモーダルを
            // 閉じた後も再度確認は出なかった)。`pending_action` に空の
            // キューを予約しておき、`show_modal` がそのモーダルを閉じた
            // タイミングで `resume_queued_close_after_modal` 経由で
            // `begin_quit()`(全タブを再計算)へ引き継ぐ(SPEC §8「閉じる前に
            // 保存確認」の趣旨)。
            //
            // `self.pending_action.is_none()` を条件にしているのは、既に
            // 別の未保存確認(`CloseTab(idx)`/`CloseLastTab` — 例えば
            // Ctrl+W で個別タブの確認モーダルが出ている最中に、さらに
            // ウィンドウの × も押された、という稀な二重要求)が進行中の
            // ときにそれを上書きして壊さないため。この場合は今回の閉じる
            // 要求を静かに諦める(そのタブの確認が終わった後、もう一度
            // ウィンドウを閉じてもらえば `self.modal.is_none()` の分岐に
            // 入れる)。
            self.pending_action = Some(PendingAction::CloseAllTabs(VecDeque::new()));
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
                    self.open_path_in_new_tab(path);
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

    /// SPEC §30: 「無題」タブの既定保存名も番号付けに追随させる(タブの
    /// ラベルと同じ情報源、`Tab::label` 参照)。
    fn default_save_file_name(&self) -> String {
        match &self.active_tab().doc.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "無題.png".to_owned()),
            None => format!("{}.png", self.active_tab().label()),
        }
    }

    /// 未保存ガードを通してからアクションを実行する(SPEC §8)。v5 時点では
    /// `PendingAction::CloseLastTab`(SPEC §30: 「最後の 1 タブを閉じようと
    /// した場合」)のみがこれを経由する — Ctrl+N/Ctrl+O は新規タブを追加
    /// するだけで既存タブを破壊しないためこの関数を経由しない
    /// (`begin_new_tab`/`begin_open_tab` 参照)。`CloseTab(usize)` は
    /// 「該当タブをアクティブ化してから確認する」という追加のひと手間が
    /// 要るため `close_tab` が独自に処理し、`CloseAllTabs` は `begin_quit`/
    /// `continue_closing_all_tabs` が独自に処理する(いずれもこの汎用関数は
    /// 経由しない)。
    fn request_action(&mut self, action: PendingAction) {
        // v3 レビューで発見・修正したバグ: テキスト編集中は `doc.modified`
        // が立たないため、以前はここで `!self.active_tab().doc.modified` を
        // 素通りしてしまい、編集中のドキュメントごと差し替わってしまう
        // ことがあった。ツール切替と同じ「先に確定」規則(SPEC §13 最終項)
        // をここでも適用し、`doc.modified` の判定より前に確定させる
        // (確定した内容が実際にドキュメントを変えていれば `doc.modified`
        // が立ち、未保存ガードも正しく発動するようになる)。
        self.commit_open_gesture();
        if self.active_tab().doc.modified {
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
            // SPEC §30: 「最後の 1 タブを閉じようとした場合…「新規」と同じ
            // 扱い(未保存ガードを通してから内容を白紙に戻す)」。通常の
            // Ctrl+N(`begin_new_tab`)と同じ「新規」ダイアログを経由するが、
            // `replace_active: true` によりタブを追加せずその場で置き換える
            // (`confirm_new` 参照)。
            PendingAction::CloseLastTab => {
                self.modal = Some(ModalState::New {
                    width: DEFAULT_NEW_WIDTH,
                    height: DEFAULT_NEW_HEIGHT,
                    background: Background::White,
                    replace_active: true,
                });
            }
            // v5 §17.4: `close_tab` が確認前に既にそのタブをアクティブ化
            // 済み(`switch_tab(index)`)なので、`index` はここでもまだ
            // 正しいタブを指す(確認モーダル表示中は他のタブ操作が割り込め
            // ないため index がずれる余地はない)。
            PendingAction::CloseTab(index) => {
                self.remove_tab_and_adjust_active(index);
            }
            // v5 §17.4: 「先頭から 1 つずつ確認フローを回す」の続きを行う。
            PendingAction::CloseAllTabs(queue) => {
                self.continue_closing_all_tabs(queue);
            }
        }
    }

    /// `close_tab`/`execute_pending_action(CloseTab)` の両方が使う、実際に
    /// `tabs` から取り除いて `active_tab` を整合させる処理(ARCHITECTURE.md
    /// §17.8-3: 「境界チェックを怠らない」)。
    fn remove_tab_and_adjust_active(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.active_tab > index {
            self.active_tab -= 1;
        } else if self.active_tab == index {
            // 閉じたタブがアクティブだった場合、同じ位置に来たタブ(元の
            // 1 つ後ろ、無ければ最後)をアクティブにする(ブラウザのタブと
            // 同じ挙動)。
            self.active_tab = index.min(self.tabs.len() - 1);
        }
    }

    /// v5 §17.4(ARCHITECTURE.md §17.4): ウィンドウを閉じる/アプリを終了
    /// する唯一の入口。`MenuAction::Exit`・`handle_close_request`(モーダル
    /// 非表示時)・`resume_queued_close_after_modal` の 3 箇所から呼ぶ。
    /// 未保存タブが 1 つも無ければ確認なしで即座に終了する。
    fn begin_quit(&mut self) {
        self.commit_open_gesture();
        let modified_tabs: VecDeque<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.doc.modified)
            .map(|(index, _)| index)
            .collect();
        self.continue_closing_all_tabs(modified_tabs);
    }

    /// v5 §17.4: 「未保存のタブがあればタブごとに順番に確認ダイアログを
    /// 出す」の本体。`queue` の先頭から順に未保存タブを確認する(1 枚ずつ
    /// `ConfirmUnsaved` モーダルを出し、保存/破棄されたら次へ進む)。
    /// アプリごと終了する前提のため、確認した後も `tabs` からは取り除かない
    /// (`PendingAction::CloseAllTabs` のドキュメントコメント参照 — 削除
    /// しないので `tabs` の長さが変わらず、キューに残った index が途中で
    /// ずれる心配が無い)。全て確認し終えたら実際に終了する。
    fn continue_closing_all_tabs(&mut self, mut queue: VecDeque<usize>) {
        while let Some(index) = queue.pop_front() {
            // 既に保存済み(直前のループで `doc.modified` が false に
            // なった)、または範囲外(通常は起きない防御的チェック、
            // ARCHITECTURE.md §17.8-3)ならスキップする。
            if index >= self.tabs.len() || !self.tabs[index].doc.modified {
                continue;
            }
            self.switch_tab(index);
            self.pending_action = Some(PendingAction::CloseAllTabs(queue));
            self.modal = Some(ModalState::ConfirmUnsaved);
            return;
        }
        self.exit_process();
    }

    /// ドキュメントを丸ごと差し替える(新規作成/開く/白紙貼り付け置換)前後で
    /// 揃えてリセットすべき、ドキュメント本体以外のツール状態。
    ///
    /// v3 レビューで発見・修正したバグ: `pen`/`eraser` の `BrushEngine::
    /// last_end`(SPEC §17 の Shift+クリック連結の終点)はここまでリセット
    /// されておらず、旧ドキュメントの画像座標が残り続けていた。まだ一度も
    /// 描いていない新ドキュメントで最初に Shift+クリックすると、存在しない
    /// はずの「直前のストローク」の終点(旧ドキュメント上の座標)から新
    /// キャンバスを横切る直線が引かれてしまう。
    fn reset_tool_state_for_new_document(&mut self) {
        self.pen.reset_for_new_document();
        self.eraser.reset_for_new_document();
        // v4 §22: 新規/開く/貼り付け置換の直後になげなわの進行中状態が
        // 古いドキュメント座標のまま残ってしまわないようにする。
        self.lasso_freehand_points.clear();
        self.lasso_polygon = None;
    }

    /// SPEC §30: 「開こうとしたファイルが既に開いているタブがあれば(パスを
    /// 正規化して比較)、新規タブを作らずそのタブへ切り替える」。「開く」
    /// ダイアログ・D&D・最近使ったファイルの全経路がここを通る。新規タブの
    /// 追加は既存タブを一切破壊しないため未保存ガードは不要
    /// (`begin_open_tab`/`request_action` のドキュメントコメント参照)。
    fn open_path_in_new_tab(&mut self, path: PathBuf) {
        if let Some(existing) = self.find_tab_by_path(&path) {
            self.switch_tab(existing);
            return;
        }
        if self.tabs.len() >= MAX_TABS {
            self.show_toast(tab_limit_toast_message());
            return;
        }
        match io::load_image(&path) {
            Ok(doc) => {
                self.open_new_tab(doc);
                // SPEC §26: 「最近使ったファイル」。
                self.remember_recent_file(path);
            }
            Err(e) => self.show_toast(format!("開けませんでした: {e}")),
        }
    }

    /// SPEC §30: 「パスを正規化して比較」。既に同じファイルを開いている
    /// タブがあればその index を返す。
    fn find_tab_by_path(&self, path: &Path) -> Option<usize> {
        let target = normalize_path_for_compare(path);
        self.tabs.iter().position(|t| {
            t.doc
                .path
                .as_deref()
                .is_some_and(|p| normalize_path_for_compare(p) == target)
        })
    }

    /// v5 §30/ARCHITECTURE.md §17.3: タブ切替の唯一の入口。Ctrl+Tab/
    /// Ctrl+Shift+Tab・タブバークリックはすべてこれを経由する(「タブ切替前に
    /// 必ず commit_open_gesture() を呼ぶ」— 本プロジェクトで最も繰り返し
    /// 発生してきたバグパターンの再発防止策、ARCHITECTURE.md §17.8-1)。
    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.commit_open_gesture();
        self.active_tab = index;
    }

    /// SPEC §30: 「Ctrl+Tab: 次のタブへ切り替え(端では反対側へ循環)」。
    fn next_tab(&mut self) {
        let next = (self.active_tab + 1) % self.tabs.len();
        self.switch_tab(next);
    }

    /// SPEC §30: 「Ctrl+Shift+Tab: 前のタブへ切り替え(端では反対側へ循環)」。
    fn prev_tab(&mut self) {
        let prev = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        self.switch_tab(prev);
    }

    /// v5 §30(ARCHITECTURE.md §17.1/§17.2/§17.3): 新しいタブを末尾に追加して
    /// アクティブにする唯一の入口。タブ切替の安全規則(§17.3)に従い、
    /// アクティブタブを差し替える前に必ず進行中のジェスチャを確定する。
    /// タブ数上限(SPEC §30: 24)は呼び出し元(`begin_new_tab`/
    /// `open_path_in_new_tab`)が事前に確認しておくこと(呼び出し文脈により
    /// 上限チェックの要否・タイミングが異なるため、ここでは行わない)。
    fn open_new_tab(&mut self, doc: Document) -> usize {
        self.commit_open_gesture();
        let untitled_number = doc.path.is_none().then(|| self.take_untitled_number());
        // バグ修正: 以前はここで `self.layer_rename = None` を無条件に
        // 実行していたが、これは共有フィールドだった頃の名残。新規タブは
        // 追加されるだけで既存タブ(そのタブ自身の `layer_rename`)を一切
        // 変更しないため、もはや不要(新規 `Tab` は `Tab::new` が
        // `layer_rename: None`/`next_layer_number: 1` で初期化する。
        // `Tab` の docstring 参照)。
        self.tabs
            .push(Tab::new(doc, untitled_number, self.max_undo_steps));
        let index = self.tabs.len() - 1;
        self.active_tab = index;
        self.reset_tool_state_for_new_document();
        index
    }

    /// SPEC §30: 「無題」「無題2」…の採番(`Tab::untitled_number` 参照)。
    fn take_untitled_number(&mut self) -> u32 {
        let n = self.next_untitled_number;
        self.next_untitled_number += 1;
        n
    }

    /// SPEC §30: Ctrl+N / メニュー「新規」。新規タブを追加する方式に変わった
    /// (v1 §7 を上書き)。既存タブの内容は一切変更しないため、`request_action`
    /// (未保存ガード)を経由しない。進行中のジェスチャだけ先に確定する
    /// (モーダル表示中はキャンバスへの入力を渡さないため、ダイアログを開く
    /// 前に確定しておく必要がある、`request_action` と同じ理由)。
    fn begin_new_tab(&mut self) {
        self.commit_open_gesture();
        if self.tabs.len() >= MAX_TABS {
            self.show_toast(tab_limit_toast_message());
            return;
        }
        self.modal = Some(ModalState::New {
            width: DEFAULT_NEW_WIDTH,
            height: DEFAULT_NEW_HEIGHT,
            background: Background::White,
            replace_active: false,
        });
    }

    /// SPEC §30: Ctrl+O / メニュー「開く」。同上の理由で未保存ガードを
    /// 適用しない。タブ数上限は「開こうとしたファイルが既に開いているタブ」
    /// への切替を妨げないよう、パスが分かった後(`open_path_in_new_tab`)で
    /// 確認する。
    fn begin_open_tab(&mut self) {
        self.commit_open_gesture();
        self.pending_dialog = Some(DialogRequest::OpenFile);
    }

    /// SPEC §30: 「常に 1 タブ以上を維持する」。v5 §17.4(ARCHITECTURE.md
    /// §17.4): 「タブを閉じる際、そのタブの `doc.modified` が true なら
    /// 該当タブをアクティブ化した上で既存の `ConfirmUnsaved` モーダルを
    /// 出す」。Ctrl+W(`Action::CloseTab`)・タブの×・中クリックの全経路が
    /// これを通る(ARCHITECTURE.md §17.7 V5-M2/M3)。
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        // SPEC §30: 「最後の 1 タブを閉じようとした場合、そのタブを閉じる
        // のではなく「新規」と同じ扱い(未保存ガードを通してから内容を白紙に
        // 戻す)にする」。`request_action` が進行中のジェスチャの確定・
        // 未保存ガードの両方を担う(`PendingAction::CloseLastTab` 参照)。
        if self.tabs.len() == 1 {
            self.request_action(PendingAction::CloseLastTab);
            return;
        }
        // v5 §17.3: アクティブタブ自身を閉じる場合、進行中のジェスチャ
        // (ブラシのドラッグ・浮動片・なげなわ等)を先に確定する。閉じる
        // タブが非アクティブなら、そのタブにジェスチャが乗っていることは
        // ない(ジェスチャは常にアクティブタブにのみ存在する)ので不要。
        if index == self.active_tab {
            self.commit_open_gesture();
        }
        if self.tabs[index].doc.modified {
            // v5 §17.4: 「該当タブをアクティブ化した上で」確認する。
            // `switch_tab` は既にアクティブなタブに対しては no-op なので
            // (上で `commit_open_gesture` 済みの場合も)二重に確定は
            // 起きない。
            self.switch_tab(index);
            self.pending_action = Some(PendingAction::CloseTab(index));
            self.modal = Some(ModalState::ConfirmUnsaved);
            return;
        }
        self.remove_tab_and_adjust_active(index);
    }

    /// 「上書き保存」(SPEC §7: Ctrl+S)。パスが未知(無題)なら「名前を
    /// 付けて保存」ダイアログにフォールバックする。
    fn begin_save(&mut self) {
        match self.active_tab().doc.path.clone() {
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
        let had_multiple_layers = self.active_tab().doc.layers.len() > 1;
        match io::save_image(&mut self.active_tab_mut().doc, &path, format) {
            Ok(()) => {
                self.active_tab_mut().doc.path = Some(path.clone());
                self.active_tab_mut().doc.modified = false;
                if had_multiple_layers {
                    self.show_toast("レイヤーは統合して保存されました".to_owned());
                }
                // SPEC §26: 「最近使ったファイル」。保存先も対象にする
                // (MS ペイント等と同様、開いたファイルだけでなく保存先も
                // 「最近使った」に含める)。
                self.remember_recent_file(path);
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

    /// 「新規」ダイアログの確定(`ModalState::New`)。`replace_active` が
    /// `true` なら唯一のタブの内容をその場で置き換える(SPEC §30: 「最後の
    /// 1 タブを閉じる」、`PendingAction::CloseLastTab` 経由)。`false` なら
    /// 通常の Ctrl+N と同じく新規タブを追加する(SPEC §30 の読み替え規則、
    /// v1 §7 を上書き)。
    fn confirm_new(
        &mut self,
        width: u32,
        height: u32,
        background: Background,
        replace_active: bool,
    ) {
        let doc = Document::new(width.clamp(1, 8192), height.clamp(1, 8192), background);
        if replace_active {
            self.reset_active_tab_document(doc);
        } else {
            self.open_new_tab(doc);
        }
    }

    /// アクティブタブの `Document` をその場で丸ごと差し替える(v1〜v4 の
    /// 「新規作成」と同じ in-place reset)。v5 ではタブを追加せず唯一の
    /// タブを白紙に戻す経路(SPEC §30: 「最後の 1 タブを閉じる」)専用。
    fn reset_active_tab_document(&mut self, doc: Document) {
        // バグ修正: レイヤー名編集中の入力を確定する処理はここでは行わない。
        // この関数へ至る唯一の経路(`confirm_new(replace_active: true)` ←
        // `PendingAction::CloseLastTab`)は必ず `request_action` の
        // `commit_open_gesture()` 呼び出しを経由済みで、そこで既に確定
        // されている(`commit_open_gesture`/`commit_pending_layer_rename`
        // のドキュメントコメント参照)。
        // バグ修正: 以前はここで `untitled_number` を採番し直さなかった
        // ため、「無題3」を最後の1タブとして閉じて新規化しても、タブ
        // ラベルが「無題3」のまま(`next_untitled_number` が既に進んで
        // いても「無題4」に更新されない)残っていた。通常の Ctrl+N
        // (`open_new_tab`)と同じく、パスの無い新規ドキュメントには
        // 必ず新しい番号を払い出す。
        let untitled_number = doc.path.is_none().then(|| self.take_untitled_number());
        self.active_tab_mut().doc = doc;
        self.active_tab_mut().history = History::new();
        self.active_tab_mut().selection = None;
        self.active_tab_mut().floating = None;
        self.select_drag = None;
        self.active_tab_mut().view = CanvasView::new();
        self.active_tab_mut().untitled_number = untitled_number;
        self.active_tab_mut().next_layer_number = 1;
        self.reset_tool_state_for_new_document();
    }

    // -----------------------------------------------------------------
    // v4 §26: 設定の永続化・最近使ったファイル(ARCHITECTURE.md §16.7)
    // -----------------------------------------------------------------

    /// 現在の状態から保存用の `Settings` スナップショットを組み立てる。
    /// `current_settings`/`save_settings` は `on_exit`(`egui::Context` を
    /// 持たない)からも呼ばれるため、ウィンドウ寸法は毎フレーム観測して
    /// おいた `self.window_size`/`window_maximized` を使う
    /// (`ui()` 冒頭の更新箇所参照)。
    fn current_settings(&self) -> Settings {
        Settings {
            window_width: self.window_size.x.round().max(1.0) as u32,
            window_height: self.window_size.y.round().max(1.0) as u32,
            window_maximized: self.window_maximized,
            recent_files: self.recent_files.clone(),
            brush_size: self.brush_size,
            brush_hardness: self.brush_hardness,
            brush_opacity: self.brush_opacity,
            pencil_mode: self.pencil_mode,
            brush_smoothing: self.brush_smoothing,
            fill_tolerance: self.fill.tolerance,
            magic_wand_tolerance: self.magic_wand_tolerance,
            rect_mode: self.rect_tool.mode,
            ellipse_mode: self.ellipse.mode,
            gradient_kind: self.gradient.kind,
            gradient_colors: self.gradient.colors,
            primary: self.primary,
            secondary: self.secondary,
            user_palette: self.user_palette.clone(),
            last_tool: self.tool,
            show_pixel_grid: self.show_pixel_grid,
            max_undo_steps: self.max_undo_steps,
        }
    }

    /// ARCHITECTURE.md §16.7: 「書き込みは終了時と最近使ったファイル更新時
    /// のみ」。この 2 箇所(`remember_recent_file`/`on_exit`/`exit_process`)
    /// だけがこれを呼ぶ。書き込み失敗は無視する(`settings::save` 自体が
    /// パニックしない、SPEC §26)。
    ///
    /// `self.persist_settings` が `false`(`new_for_test` 経由のユニット
    /// テストは常にこう)なら何もしない — `open_path`/`finish_save` 等の
    /// 既存テストがこの関数を間接的に何度も踏むため、素朴に実装すると
    /// `cargo test` のたびに実 `%APPDATA%\darask-paint\settings.txt`
    /// (開発者・CI 実行環境の実ファイル)を上書きしてしまう。テストは
    /// 副作用としてグローバルな実ファイルへ書き込んではならない
    /// (`settings.rs` 自体の I/O テストは temp dir 経由の
    /// `save_to_path`/`load_from_path` で既に検証済み、ここでの実書き込みは
    /// 不要)。
    fn save_settings(&self) {
        if self.persist_settings {
            settings::save(&self.current_settings());
        }
    }

    /// 確認済みの終了(SPEC §8 の未保存ガードを通過済み、または最初から
    /// 不要だった)を実行する唯一の入口。`std::process::exit` は Rust の
    /// 通常のアンワインドを経ないため `eframe::App::on_exit` は呼ばれない
    /// (`impl eframe::App for DaraskApp` のコメント参照) — ここで明示的に
    /// 設定を保存してから終了する。
    ///
    /// ベンチモード(`ui()` 内の `bench.frames_drawn >= 2` の終了)は意図的に
    /// これを経由させない: ベンチはユーザー操作を伴わない決定的なスモーク
    /// テストであり、実行するたびに実 `%APPDATA%` の設定ファイルを上書きする
    /// のは望ましくない副作用になる。
    fn exit_process(&self) -> ! {
        self.save_settings();
        std::process::exit(0);
    }

    /// 「最近使ったファイル」を更新する(SPEC §26: 最大 8、先頭が最新。
    /// 既存の同一パスは先頭へ移動)。開く(`open_path`)・保存
    /// (`finish_save`)の成功時、および CLI 引数で開いた場合(`new`)に呼ぶ。
    /// 更新のたびに即座に保存する(ARCHITECTURE.md §16.7)。
    fn remember_recent_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.push_front(path);
        self.recent_files.truncate(settings::MAX_RECENT_FILES);
        self.save_settings();
    }

    /// 「ファイル > 最近使ったファイル」のクリック(SPEC §26: 「存在しない
    /// パスは選択時に一覧から除去してトースト」)。存在しなければここで
    /// 一覧から除去して終わる(未保存ガードを通す前に確認するので、開けない
    /// と分かっているファイルのために保存確認を挟まずに済む)。存在すれば
    /// 通常の「開く」(未保存ガード込み)に委ねる。
    fn open_recent_file(&mut self, index: usize) {
        let Some(path) = self.recent_files.get(index).cloned() else {
            return;
        };
        if path.exists() {
            self.open_path_in_new_tab(path);
        } else {
            self.recent_files.retain(|p| p != &path);
            self.save_settings();
            self.show_toast(format!("ファイルが見つかりません: {}", path.display()));
        }
    }

    /// SPEC §26: 「ヘルプ > バージョン情報」。
    fn open_about_modal(&mut self) {
        self.modal = Some(ModalState::About);
    }

    /// SPEC §34/ARCHITECTURE.md §18.2: 「設定(環境設定)」ダイアログを開く
    /// (Ctrl+K・ツールバーの歯車ボタン)。ドラフト値は現在の
    /// `self.max_undo_steps` から始める(`New`/`ImageResize` と同じ
    /// 「開いた時点の実際の値をドラフトの初期値にする」パターン)。
    fn open_preferences_modal(&mut self) {
        self.modal = Some(ModalState::Preferences {
            draft_max_undo_steps: self.max_undo_steps,
        });
    }

    /// SPEC §34/ARCHITECTURE.md §18.2・§18.3: 設定ダイアログの OK 確定
    /// (`show_modal` の `ModalState::Preferences` 分岐から呼ぶ)。
    ///
    /// - `self.max_undo_steps` を更新し、即座に設定ファイルへ保存する
    ///   (ARCHITECTURE.md §18.2: 「OK で即座に適用+設定ファイルへ保存」)。
    /// - **開いている全タブ**の `History::set_max_steps` を呼ぶ
    ///   (ARCHITECTURE.md §18.6-2: 「既存タブが取り残される、というバグを
    ///   作らない」)。値を下げて現在の段数がそれを超えていれば、
    ///   `History::set_max_steps` 内部の `trim_to_limits` がその場で
    ///   古いものから切り詰める(ARCHITECTURE.md §18.3、新しい仕組みを
    ///   増やさない)。
    fn apply_preferences(&mut self, new_max_undo_steps: u32) {
        self.max_undo_steps = new_max_undo_steps;
        for tab in &mut self.tabs {
            tab.history.set_max_steps(new_max_undo_steps as usize);
        }
        self.save_settings();
    }

    // -----------------------------------------------------------------
    // M4: 画像メニュー(SPEC §7)
    // -----------------------------------------------------------------

    /// `before`(操作前の全レイヤースナップショット)と現在のドキュメントの
    /// 差分から `HistoryOp::ReplaceAll` を積む(SPEC §13: 画像メニューの
    /// 操作は全レイヤーに適用されるため、v1 の単一バッファ `Replace` ではなく
    /// 全レイヤー+寸法のスナップショットを使う、ARCHITECTURE.md §14.2)。
    fn push_replace_all(&mut self, before: crate::document::DocSnapshot, label: impl Into<String>) {
        let after = self.active_tab().doc.snapshot();
        self.active_tab_mut()
            .history
            .push(HistoryOp::ReplaceAll { before, after }, label);
        self.active_tab_mut().doc.mark_all_dirty();
        self.active_tab_mut().doc.modified = true;
    }

    fn apply_flip_horizontal(&mut self) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut().doc.flip_horizontal();
        self.push_replace_all(before, "左右反転");
    }

    fn apply_flip_vertical(&mut self) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut().doc.flip_vertical();
        self.push_replace_all(before, "上下反転");
    }

    fn apply_rotate_cw(&mut self) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut().doc.rotate_cw();
        self.push_replace_all(before, "右に90°回転");
    }

    fn apply_rotate_ccw(&mut self) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut().doc.rotate_ccw();
        self.push_replace_all(before, "左に90°回転");
    }

    /// SPEC §7: 「選択範囲でトリミング」。選択(または浮動片)が無ければ
    /// 何もしない(メニュー側で無効化もしている)。
    fn apply_crop_to_selection(&mut self) {
        // SPEC §21: 「選択範囲でトリミング」は bbox でトリミング(マスク形状は
        // 見ない)。
        let rect = match (&self.active_tab().selection, &self.active_tab().floating) {
            (Some(sel), _) => Some(sel.mask.bbox),
            (None, Some(floating)) => Some(select::floating_target_rect(floating)),
            (None, None) => None,
        };
        let Some(rect) = rect else {
            return;
        };
        self.commit_selection();
        let rect = rect.clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        if rect.is_empty() {
            return;
        }
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut().doc.crop_to(rect);
        self.push_replace_all(before, "選択範囲でトリミング");
        self.active_tab_mut().selection = None;
    }

    /// SPEC §31(ARCHITECTURE.md §17.5): 画像メニュー「選択範囲を新規タブに
    /// 複製」。選択または浮動片がアクティブなときのみ意味を持つ(メニュー側で
    /// 無効化もしているが、直接呼ばれた場合に備えて二重にガードする)。
    fn duplicate_selection_to_new_tab(&mut self) {
        if self.active_tab().selection.is_none() && self.active_tab().floating.is_none() {
            return;
        }
        // SPEC §30: 「タブ数の上限は24。超えて新規タブを作ろうとしたら作成
        // せずトースト通知」(既存の新規タブ経路と同じ流儀)。
        if self.tabs.len() >= MAX_TABS {
            self.show_toast(tab_limit_toast_message());
            return;
        }

        // v5 §17.3/§17.8-1: 「タブ切替前に必ず commit_open_gesture() を呼ぶ」
        // ―― ただしその一般形は選択/移動ツール中に浮動片を
        // `flush_floating_keep_selection` でアクティブレイヤーへ合成して
        // しまう。SPEC §31 は「浮動片がある場合はそのピクセルをそのまま
        // 複製先へ渡し、元のタブは一切変更しない」ことを要求するため、ここで
        // 合成してしまうと (a) 元タブを書き換えてしまい非破壊の要件に反し、
        // (b) 複製先が「浮動片そのもの」ではなく「合成後にもう一度切り出した
        // もの」になってしまう(浮動片が矩形より大きいレイヤーからはみ出た
        // 位置にある場合、キャンバス外の画素が失われる違いが生じうる)。
        // 浮動片が存在しうるのは `self.tool` が Select/EllipseSelect/Move の
        // ときだけ(`place_new_floating` 参照。他のツールに切り替えた時点で
        // 必ず `commit_open_gesture` を経由して確定済みになる)であり、その
        // ときの唯一の「進行中ジェスチャ」は浮動片の移動/リサイズドラッグ
        // (`select_drag`)だけなので、それだけを終了させる。それ以外の
        // ツールでは浮動片は存在しえない(以下の抽出処理はプレーンな選択だけ
        // を見る)ため、通常どおり `end_active_gesture` で他ツール(なげなわの
        // 頂点列・テキスト編集・図形/グラデーションのドラッグ等)の進行中
        // 状態を確定してよい ―― これらはタブをまたぐと座標が壊れる
        // (ARCHITECTURE.md §17.8-1 と同じバグクラス)ため、タブ挿入前に
        // 必ず終わらせておく必要がある。
        if matches!(
            self.tool,
            ToolKind::Select | ToolKind::EllipseSelect | ToolKind::Move
        ) {
            self.select_drag = None;
        } else {
            self.end_active_gesture();
        }

        let (width, height, layers, active) = if let Some(floating) = &self.active_tab().floating {
            // SPEC §31: 「浮動片がある場合: その浮動片のピクセル(mask込み)を
            // そのまま新規タブの唯一のレイヤーにする」。
            let pixels = select::floating_layer_pixels(floating);
            (
                floating.w,
                floating.h,
                vec![Layer {
                    name: "背景".to_owned(),
                    visible: true,
                    opacity: 255,
                    pixels,
                }],
                0,
            )
        } else {
            // SPEC §31: 「静的な選択のみの場合: 選択マスクの bbox で、全
            // レイヤーをマスク外transparentで切り出し、レイヤー構成(名前・
            // 表示・不透明度・重ね順・アクティブレイヤー)を保ったまま新規
            // タブに複製する」(「選択範囲でトリミング」の全レイヤー方針を
            // 踏襲、`apply_crop_to_selection` 参照)。
            let mask = self
                .active_tab()
                .selection
                .as_ref()
                .expect("no-selection/no-floating is checked at the top of this function")
                .mask
                .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
            let rect = mask.bbox;
            if rect.is_empty() {
                return;
            }
            // ARCHITECTURE.md §17.5: `tools::select::extract_region` を
            // 「各レイヤーに対して」呼ぶ ―― `extract_region` はアクティブ
            // レイヤーしか読まないため、レイヤーごとに `doc.active` を一時的に
            // 差し替えて呼び出し、抽出後は必ず元へ戻す(§17.8-5: 「元のタブは
            // 一切変更しない」を満たすための読み取り専用操作であることを
            // 保証する)。
            let tab = &mut self.tabs[self.active_tab];
            let saved_active = tab.doc.active;
            let mut layers = Vec::with_capacity(tab.doc.layers.len());
            for i in 0..tab.doc.layers.len() {
                tab.doc.active = i;
                let pixels = select::extract_region(&tab.doc, &mask);
                let src = &tab.doc.layers[i];
                layers.push(Layer {
                    name: src.name.clone(),
                    visible: src.visible,
                    opacity: src.opacity,
                    pixels,
                });
            }
            tab.doc.active = saved_active;
            (
                rect.width() as u32,
                rect.height() as u32,
                layers,
                saved_active,
            )
        };

        let new_doc = Document::from_duplicated_layers(width, height, layers, active);
        self.insert_duplicated_tab(new_doc);
    }

    /// `duplicate_selection_to_new_tab` 専用のタブ挿入。`open_new_tab`
    /// (末尾に追加)と違い、SPEC §31: 「新規タブは元タブの直後に挿入され
    /// アクティブになる」ため挿入位置が異なる。また `open_new_tab` は内部で
    /// 無条件に `commit_open_gesture` を呼ぶが、`duplicate_selection_to_new_tab`
    /// は既に浮動片を壊さない形でジェスチャを終わらせた後にここへ来るため、
    /// ここでもう一度 `commit_open_gesture` を呼ぶと浮動片が元タブへ合成
    /// されてしまい SPEC §31 の「元のタブは一切変更しない」に反する。
    /// そのため `open_new_tab` を再利用せず専用の経路にする(タブ数上限は
    /// 呼び出し元が確認済み)。
    fn insert_duplicated_tab(&mut self, doc: Document) -> usize {
        // SPEC §31: 「パスは無し(「無題」系の命名)」なので常に採番する。
        let untitled_number = Some(self.take_untitled_number());
        let insert_at = self.active_tab + 1;
        // バグ修正: 以前はここで `self.layer_rename = None` を無条件に
        // 実行していたが、新規タブは挿入されるだけで元タブ(そのタブ自身の
        // `layer_rename`)を一切変更しないため不要(`open_new_tab` と同じ
        // 理由、`Tab` の docstring 参照)。
        self.tabs.insert(
            insert_at,
            Tab::new(doc, untitled_number, self.max_undo_steps),
        );
        self.active_tab = insert_at;
        self.reset_tool_state_for_new_document();
        insert_at
    }

    fn confirm_image_resize(&mut self, width: u32, height: u32, interpolation: Interpolation) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut()
            .doc
            .resize(width.max(1), height.max(1), interpolation);
        self.push_replace_all(before, "画像サイズ変更");
    }

    fn confirm_canvas_resize(&mut self, width: u32, height: u32) {
        self.commit_selection();
        let before = self.active_tab().doc.snapshot();
        self.active_tab_mut()
            .doc
            .resize_canvas(width.max(1), height.max(1));
        self.push_replace_all(before, "キャンバスサイズ変更");
    }

    // -----------------------------------------------------------------
    // v4 §24: 色調補正(ARCHITECTURE.md §16.5)
    //
    // すべてアクティブレイヤー対象、選択があればその中だけ(選択 bbox に
    // クリップする、ブラシ/グラデーションと同じ `Surface::clip` 経由)。
    // 即時適用(反転・グレースケール化)は「現在のピクセルを読んで書き換える」
    // 1 回のループ、ライブプレビュー付きモーダル(明るさ・コントラスト/
    // 色相・彩度・明度)は「モーダルを開いた時点のスナップショットから毎回
    // 計算し直す」ループ(スライダーを往復しても劣化する累積適用にならない、
    // ARCHITECTURE.md §16.10-4)。
    // -----------------------------------------------------------------

    /// 色調補正の対象領域(SPEC §24: 「選択範囲があればその中だけ」)。
    fn tone_adjust_target_rect(&self) -> crate::document::IRect {
        self.active_tab()
            .selection
            .as_ref()
            .map(|s| s.mask.bbox)
            .unwrap_or_else(|| self.doc_full_rect())
    }

    /// 即時適用の色調補正(階調の反転・グレースケール化)。1 undo 単位。
    ///
    /// v4 レビューで発見・修正した重大なバグ: 以前はここで
    /// `flush_floating_keep_selection`(浮動片だけを確定し、ブラシ等の
    /// 進行中ストロークは見ない)しか呼んでいなかった。keymap.rs で
    /// Ctrl+I/Ctrl+Shift+U はテキスト入力中・モーダル中以外は常に有効
    /// (`handle_shortcuts` のガードはキーボードフォーカスと modal のみ、
    /// キャンバスは `Sense::click_and_drag` で `request_focus` しない)ため、
    /// ブラシ/消しゴム/図形/グラデーションで左ボタンを押したままドラッグ
    /// 中でも発火する。その状態で直後の `history.begin_stroke` を呼ぶと、
    /// `History::begin_stroke` は進行中の `StrokeRecorder` を無警告で置換し、
    /// 退避済みの「ストローク開始前」タイルが `HistoryOp` を積まずに失われる
    /// (`delete_selection` のドキュメントコメントに記録されている、v2 で
    /// 発見・修正したのと同じクラスのバグ)。その結果、反転/グレースケール
    /// の `before` に描きかけの画素が「元からあった画素」として混入し、
    /// かつストローク後半は `History::stroke == None` のまま描画され続けて
    /// 一切 undo できなくなる。`delete_selection`/`paste_pixels` と同じ
    /// 規則で、進行中のジェスチャ(ブラシ等のドラッグ、または選択の浮動片)
    /// を種類を問わず先に確定する `commit_open_gesture` を呼ぶ(選択自体は
    /// 残す、SPEC §21)。
    fn apply_tone_adjustment_immediate(
        &mut self,
        label: &'static str,
        f: impl Fn([u8; 4]) -> [u8; 4],
    ) {
        self.commit_open_gesture();
        let bounds = self
            .tone_adjust_target_rect()
            .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        if bounds.is_empty() {
            return;
        }
        // `clip`(`tab.selection` 由来)と `tab.doc` を同時に借用するため
        // 単一の `&mut Tab` を経由する(`apply_tone_adjustment_immediate`
        // 系すべてに共通、`end_active_gesture` のコメント参照)。
        let tab = &mut self.tabs[self.active_tab];
        tab.history.begin_stroke(tab.doc.active);
        tab.history.ensure_tiles_saved(&tab.doc, bounds);
        let clip = tab.selection.as_ref().map(|s| &s.mask);
        {
            let mut surface = tab.doc.active_surface_mut(clip);
            for y in bounds.y0..bounds.y1 {
                for x in bounds.x0..bounds.x1 {
                    if let Some(px) = surface.get_pixel(x, y) {
                        surface.set_pixel(x, y, f(px));
                    }
                }
            }
        }
        tab.doc.mark_dirty(bounds);
        tab.history.commit_stroke(&mut tab.doc, label);
    }

    /// SPEC §24: 「階調の反転 (Ctrl+I) — 即時(RGB反転、アルファ不変)」。
    fn apply_invert(&mut self) {
        self.apply_tone_adjustment_immediate("階調の反転", raster::invert_pixel);
    }

    /// SPEC §24: 「グレースケール化 (Ctrl+Shift+U) — 即時(Rec.709 輝度)」。
    fn apply_grayscale(&mut self) {
        self.apply_tone_adjustment_immediate("グレースケール化", raster::grayscale_pixel);
    }

    /// ライブプレビュー付きモーダルを開く共通処理。開いた時点で
    /// `History::begin_stroke`/`ensure_tiles_saved` により対象領域全体を
    /// 退避しておく(以後のプレビュー再計算がこのスナップショットから行われる、
    /// ARCHITECTURE.md §16.5)。
    ///
    /// v4 レビューで発見・修正した重大なバグ: `apply_tone_adjustment_
    /// immediate` と全く同じ理由で、ここも以前は `flush_floating_keep_
    /// selection` しか呼んでいなかった。ブラシ等でドラッグ中に Ctrl+U を
    /// 押すと `history.begin_stroke` が進行中の `StrokeRecorder` を無警告で
    /// 置換し、部分ストロークがモーダルの `before` スナップショットに
    /// 混入したまま undo 不能になる(OK 時)、またはキャンセル時も
    /// `restore_stroke_region` はモーダルを開いた時点(部分ストローク込み)
    /// へ戻すだけで、その部分ストロークの履歴自体は失われたまま。
    /// `commit_open_gesture` で先に進行中のジェスチャ(ブラシ等のドラッグ、
    /// または選択の浮動片)を種類を問わず確定してから対象領域を退避する
    /// (選択自体は残す、SPEC §21)。
    fn begin_tone_adjust_stroke(&mut self) -> crate::document::IRect {
        self.commit_open_gesture();
        let bounds = self
            .tone_adjust_target_rect()
            .clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        let tab = &mut self.tabs[self.active_tab];
        tab.history.begin_stroke(tab.doc.active);
        if !bounds.is_empty() {
            tab.history.ensure_tiles_saved(&tab.doc, bounds);
        }
        bounds
    }

    /// SPEC §24: 「明るさ・コントラスト…」モーダルを開く。
    fn open_brightness_contrast_modal(&mut self) {
        let rect = self.begin_tone_adjust_stroke();
        self.modal = Some(ModalState::BrightnessContrast {
            brightness: 0,
            contrast: 0,
            rect,
        });
    }

    /// SPEC §24: 「色相・彩度・明度… (Ctrl+U)」モーダルを開く。
    fn open_hue_saturation_modal(&mut self) {
        let rect = self.begin_tone_adjust_stroke();
        self.modal = Some(ModalState::HueSaturation {
            hue: 0,
            saturation: 0,
            lightness: 0,
            rect,
        });
    }

    /// ライブプレビューの再計算(ARCHITECTURE.md §16.5: 「スナップショットから
    /// 毎回計算」)。`rect` はモーダルを開いた時点の対象領域。値が変わった
    /// フレームだけ呼ぶこと(ARCHITECTURE.md §14.9-8 と同じ「変わったフレーム
    /// だけ再適用」方式、呼び出し元の `show_modal` 参照)。
    fn reapply_tone_preview(
        &mut self,
        rect: crate::document::IRect,
        f: impl Fn([u8; 4]) -> [u8; 4],
    ) {
        let bounds = rect.clamp_to(self.active_tab().doc.width, self.active_tab().doc.height);
        if bounds.is_empty() {
            return;
        }
        // `clip`(`tab.selection` 由来)・`original_cursor`(`tab.history` 由来)・
        // `surface`(`tab.doc` の可変借用)を同時に生かすため、単一の
        // `&mut Tab` を経由してフィールドごとに分割借用する。
        let tab = &mut self.tabs[self.active_tab];
        let clip = tab.selection.as_ref().map(|s| &s.mask);
        // v4-M2 性能改善(ARCHITECTURE.md §16.1、`OriginalPixelCursor` の
        // ドキュメント参照): 対象領域全体の画素ループで 1 個のカーソルを
        // 使い回し、行ごとに `stroke.tiles` の `HashMap` を引き直さない。
        let mut original_cursor = tab.history.original_pixel_cursor();
        let mut surface = tab.doc.active_surface_mut(clip);
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                if let Some(original) = original_cursor.get(x, y) {
                    surface.set_pixel(x, y, f(original));
                }
            }
        }
        tab.doc.mark_dirty(bounds);
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
    fn push_layer_history(&mut self, op: HistoryOp, label: impl Into<String>) {
        self.active_tab_mut().history.push(op, label);
        self.active_tab_mut().doc.mark_all_dirty();
        self.active_tab_mut().doc.modified = true;
    }

    /// SPEC §13: 「新規レイヤーは透明で名前は『レイヤー N』」。バグ修正:
    /// 採番カウンタはタブごとに独立させた(`Tab::next_layer_number`。以前は
    /// `DaraskApp` 直下の共有フィールドで、タブを切り替えて別タブでレイヤー
    /// 追加すると番号が続きから採番され、タブ単体で見ると 1 から連番に
    /// ならず歯抜けになっていた)。
    fn next_layer_name(&mut self) -> String {
        let tab = self.active_tab_mut();
        let name = format!("レイヤー {}", tab.next_layer_number);
        tab.next_layer_number += 1;
        name
    }

    /// バグ修正: 以前はレイヤー名編集中(`layer_rename` が `Some`)に
    /// ドキュメントを丸ごと差し替える操作(`reset_active_tab_document`/
    /// `replace_document_with_pasted_image`)が単に `layer_rename = None`
    /// で破棄するだけで、`text_edit` に対して行っている
    /// `commit_pending_text_edit_and_composite` のような「先に確定してから
    /// 実行」を行っていなかった。`commit_open_gesture`(ツール切替・タブ
    /// 切替・レイヤー操作・アンドゥ/リドゥ・未保存ガード判定の唯一の
    /// 共通フック)の内部から呼ぶことで、これらすべての「割り込み」操作の
    /// 前に入力中のテキストを確定させ、確定によって立った `doc.modified`
    /// が未保存ガードにも正しく反映されるようにする。
    fn commit_pending_layer_rename(&mut self) {
        let Some((idx, text, _)) = self.active_tab_mut().layer_rename.take() else {
            return;
        };
        let trimmed = text.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let tab = self.active_tab_mut();
        if let Some(layer) = tab.doc.layers.get_mut(idx) {
            layer.name = trimmed;
            // `ui/layers_panel.rs::show_layer_row` の通常の確定経路と同様、
            // 不透明度・表示切替の隣接ハンドラに揃えて `modified` を立てる
            // (立てないと `doc_is_pristine()` がリネームだけされた文書を
            // 「白紙」のまま誤判定し、Ctrl+V の白紙置換パスに載ってしまう)。
            tab.doc.modified = true;
        }
    }

    fn layer_add(&mut self) {
        self.commit_open_gesture();
        let name = self.next_layer_name();
        let before_active = self.active_tab().doc.active_index();
        if self.active_tab_mut().doc.add_layer(name.clone()) {
            let index = self.active_tab().doc.active_index();
            self.push_layer_history(
                HistoryOp::AddLayer {
                    index,
                    name,
                    before_active,
                },
                "レイヤーを追加",
            );
        }
    }

    fn layer_duplicate(&mut self) {
        self.commit_open_gesture();
        let before_active = self.active_tab().doc.active_index();
        if self.active_tab_mut().doc.duplicate_active_layer() {
            let index = self.active_tab().doc.active_index();
            let layer = self.active_tab().doc.layers[index].clone();
            self.push_layer_history(
                HistoryOp::DuplicateLayer {
                    index,
                    layer,
                    before_active,
                },
                "レイヤーを複製",
            );
        }
    }

    fn layer_delete(&mut self) {
        self.commit_open_gesture();
        // `Document::remove_active_layer` 自身の拒否条件(レイヤー1枚)を
        // 先に確認し、拒否される呼び出しでは複製コストを払わない。
        if self.active_tab().doc.layers.len() <= 1 {
            return;
        }
        let before_active = self.active_tab().doc.active_index();
        let layer = self.active_tab().doc.layers[before_active].clone();
        if self.active_tab_mut().doc.remove_active_layer() {
            self.push_layer_history(
                HistoryOp::RemoveLayer {
                    index: before_active,
                    layer,
                    before_active,
                },
                "レイヤーを削除",
            );
        }
    }

    fn layer_move_up(&mut self) {
        self.commit_open_gesture();
        let from = self.active_tab().doc.active_index();
        if self.active_tab_mut().doc.move_active_layer_up() {
            let to = self.active_tab().doc.active_index();
            self.push_layer_history(HistoryOp::MoveLayer { from, to }, "レイヤーの並び替え");
        }
    }

    fn layer_move_down(&mut self) {
        self.commit_open_gesture();
        let from = self.active_tab().doc.active_index();
        if self.active_tab_mut().doc.move_active_layer_down() {
            let to = self.active_tab().doc.active_index();
            self.push_layer_history(HistoryOp::MoveLayer { from, to }, "レイヤーの並び替え");
        }
    }

    fn layer_merge_down(&mut self) {
        self.commit_open_gesture();
        // `Document::merge_active_down` 自身の拒否条件(レイヤー1枚・
        // アクティブが最下位)を先に確認し、拒否される呼び出しでは複製
        // コストを払わない。
        let index = self.active_tab().doc.active_index();
        if index == 0 || self.active_tab().doc.layers.len() <= 1 {
            return;
        }
        let upper = self.active_tab().doc.layers[index].clone();
        let lower_before = self.active_tab().doc.layers[index - 1].clone();
        if self.active_tab_mut().doc.merge_active_down() {
            self.push_layer_history(
                HistoryOp::MergeDown {
                    index,
                    upper,
                    lower_before,
                },
                "レイヤーの結合",
            );
        }
    }

    /// SPEC §13: メニュー「画像の統合」(Ctrl+Shift+E)。複数レイヤーを
    /// 1 枚へ合成する操作は全レイヤーの前後スナップショットが本質的に
    /// 必要なため、ここだけ `ReplaceAll`(`push_replace_all`)を使う
    /// (ARCHITECTURE.md §14.2)。
    fn layer_flatten(&mut self) {
        self.commit_open_gesture();
        if self.active_tab().doc.layers.len() <= 1 {
            return;
        }
        let before = self.active_tab().doc.snapshot();
        if self.active_tab_mut().doc.flatten_all() {
            self.push_replace_all(before, "画像の統合");
        }
    }

    /// レイヤーパネルの行クリック(SPEC §13: 「クリックでアクティブ化」)。
    /// アクティブレイヤーの切り替えは履歴に積まないが、進行中のジェスチャは
    /// 「先に確定」してから切り替える(ARCHITECTURE.md §14.9-3: 「浮動片
    /// 保持中にアクティブレイヤーを変えると確定先が変わってしまう」の対策)。
    fn set_active_layer(&mut self, index: usize) {
        if index >= self.active_tab().doc.layers.len()
            || index == self.active_tab().doc.active_index()
        {
            return;
        }
        self.commit_open_gesture();
        self.active_tab_mut().doc.active = index;
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
            // v5 §30: `begin_new_tab`/`begin_open_tab` のドキュメントコメント
            // 参照(新規タブ追加は既存タブを破壊しないため未保存ガード不要)。
            MenuAction::New => self.begin_new_tab(),
            MenuAction::Open => self.begin_open_tab(),
            MenuAction::OpenRecent(index) => self.open_recent_file(index),
            MenuAction::Save => self.begin_save(),
            MenuAction::SaveAs => self.begin_save_as(),
            // v5 §17.4: 「終了」もウィンドウを閉じる操作と同じく全タブを
            // 確認する(`begin_quit` 参照。単体タブの `request_action` は
            // アクティブタブしか見ないため、ここでは使わない)。
            MenuAction::Exit => self.begin_quit(),
            // v5 §17.6: ファイルメニュー「タブを閉じる」。アクティブタブを
            // 閉じる(`Action::CloseTab`/Ctrl+W と同じ、`close_tab` 参照)。
            MenuAction::CloseTab => self.close_tab(self.active_tab),
            MenuAction::Undo => {
                // SPEC §13 最終項: 浮動片/ストローク進行中は先に確定してから
                // 実行する(`handle_shortcuts` の `Action::Undo`/`Redo` と
                // 同じ規則)。
                self.commit_open_gesture();
                let tab = self.active_tab_mut();
                tab.history.undo(&mut tab.doc);
                self.clamp_selection_to_doc();
            }
            MenuAction::Redo => {
                self.commit_open_gesture();
                let tab = self.active_tab_mut();
                tab.history.redo(&mut tab.doc);
                self.clamp_selection_to_doc();
            }
            MenuAction::Cut => self.cut_selection_to_clipboard(),
            MenuAction::Copy => {
                self.copy_selection_to_clipboard();
            }
            MenuAction::Paste => self.paste_from_clipboard(),
            MenuAction::Delete => self.delete_selection(),
            MenuAction::SelectAll => self.select_all(),
            MenuAction::Deselect => self.commit_selection(),
            // v6 §33(ARCHITECTURE.md §18.1): 編集メニューに追加した
            // 「自由変形」。Ctrl+T(`Action::FreeTransform`)と全く同じ処理を
            // 呼ぶだけ(`free_transform` 自身が commit-first ガードを持つ、
            // ドキュメントコメント参照)。
            MenuAction::FreeTransform => self.free_transform(),
            MenuAction::ImageResize => {
                self.modal = Some(ModalState::ImageResize {
                    width: self.active_tab().doc.width,
                    height: self.active_tab().doc.height,
                    keep_aspect: true,
                    interpolation: Interpolation::Bilinear,
                });
            }
            MenuAction::CanvasResize => {
                self.modal = Some(ModalState::CanvasResize {
                    width: self.active_tab().doc.width,
                    height: self.active_tab().doc.height,
                });
            }
            MenuAction::Crop => self.apply_crop_to_selection(),
            MenuAction::DuplicateSelectionToTab => self.duplicate_selection_to_new_tab(),
            MenuAction::FlipHorizontal => self.apply_flip_horizontal(),
            MenuAction::FlipVertical => self.apply_flip_vertical(),
            MenuAction::RotateCw => self.apply_rotate_cw(),
            MenuAction::RotateCcw => self.apply_rotate_ccw(),
            MenuAction::BrightnessContrast => self.open_brightness_contrast_modal(),
            MenuAction::HueSaturation => self.open_hue_saturation_modal(),
            MenuAction::Invert => self.apply_invert(),
            MenuAction::Grayscale => self.apply_grayscale(),
            MenuAction::ZoomIn => self.active_tab_mut().view.zoom_in(),
            MenuAction::ZoomOut => self.active_tab_mut().view.zoom_out(),
            MenuAction::Zoom100 => self.active_tab_mut().view.zoom_to_100(),
            MenuAction::FitWindow => {
                let tab = self.active_tab_mut();
                tab.view.fit_to_window(&tab.doc);
            }
            MenuAction::TogglePixelGrid => self.show_pixel_grid = !self.show_pixel_grid,
            MenuAction::LayerAdd => self.layer_add(),
            MenuAction::LayerDuplicate => self.layer_duplicate(),
            MenuAction::LayerDelete => self.layer_delete(),
            MenuAction::LayerMoveUp => self.layer_move_up(),
            MenuAction::LayerMoveDown => self.layer_move_down(),
            MenuAction::LayerMergeDown => self.layer_merge_down(),
            MenuAction::LayerFlatten => self.layer_flatten(),
            MenuAction::About => self.open_about_modal(),
            // v6 §33/§34: メニューバー「その他」の設定ボタン。ツールバーの
            // 歯車ボタン(`ToolbarAction::OpenPreferences`)と同じ処理。
            MenuAction::OpenPreferences => self.open_preferences_modal(),
        }
    }

    /// SPEC §30(ARCHITECTURE.md §17.7 V5-M2): タブバーのクリック/中クリック
    /// を実際の操作へディスパッチする(`ui/tab_bar.rs::show` は
    /// `TabBarAction` を返すだけ、他の `ui/*` パネルと同じ流儀)。
    fn handle_tab_bar_action(&mut self, action: TabBarAction) {
        match action {
            TabBarAction::Activate(index) => self.switch_tab(index),
            TabBarAction::Close(index) => self.close_tab(index),
        }
    }

    /// 表示中のモーダル(あれば)を描き、確定/キャンセルを処理する。
    fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        // M4 で発見・修正したバグ(`handle_close_request` 参照): このモーダル
        // が表示されている間に閉じる要求が来ていた(`pending_action` に
        // `CloseAllTabs` が予約された)かどうかを、各分岐が `pending_action`
        // を書き換えるより前に覚えておく(例えば「新規」ダイアログの
        // キャンセルは無条件に `pending_action = None` するため、後から
        // 読み直すと消えてしまう)。v5 §17.4: `handle_close_request` は
        // 既に `CloseTab`/`CloseLastTab` の確認が進行中なら上書きしない
        // (`pending_action.is_none()` ガード)ので、ここで `CloseAllTabs`
        // を見つけたときだけが「閉じる要求が割り込んだ」ケースになる。
        let close_was_queued = matches!(self.pending_action, Some(PendingAction::CloseAllTabs(_)));
        let mut keep_open = true;
        match &mut modal {
            ModalState::New {
                width,
                height,
                background,
                replace_active,
            } => match dialogs::show_new(ctx, width, height, background) {
                DialogOutcome::Confirmed => {
                    self.confirm_new(*width, *height, *background, *replace_active);
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
                let (orig_w, orig_h) = (self.active_tab().doc.width, self.active_tab().doc.height);
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
            ModalState::BrightnessContrast {
                brightness,
                contrast,
                rect,
            } => {
                let rect = *rect;
                let (outcome, changed) =
                    dialogs::show_brightness_contrast(ctx, brightness, contrast);
                if changed {
                    let lut = raster::brightness_contrast_lut(*brightness, *contrast);
                    self.reapply_tone_preview(rect, |px| raster::apply_lut_pixel(px, &lut));
                }
                match outcome {
                    DialogOutcome::Confirmed => {
                        let tab = self.active_tab_mut();
                        // ARCHITECTURE.md §18.3 の対応表: 「明るさ・コントラスト」。
                        tab.history
                            .commit_stroke(&mut tab.doc, "明るさ・コントラスト");
                        keep_open = false;
                    }
                    DialogOutcome::Cancelled => {
                        let tab = self.active_tab_mut();
                        tab.history.restore_stroke_region(&mut tab.doc, rect);
                        tab.history.cancel_stroke();
                        keep_open = false;
                    }
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::HueSaturation {
                hue,
                saturation,
                lightness,
                rect,
            } => {
                let rect = *rect;
                let (outcome, changed) =
                    dialogs::show_hue_saturation(ctx, hue, saturation, lightness);
                if changed {
                    let (h, s, l) = (*hue, *saturation, *lightness);
                    self.reapply_tone_preview(rect, move |px| {
                        raster::adjust_hsl_pixel(px, h, s, l)
                    });
                }
                match outcome {
                    DialogOutcome::Confirmed => {
                        let tab = self.active_tab_mut();
                        // ARCHITECTURE.md §18.3 の対応表: 「色相・彩度・明度」。
                        tab.history.commit_stroke(&mut tab.doc, "色相・彩度・明度");
                        keep_open = false;
                    }
                    DialogOutcome::Cancelled => {
                        let tab = self.active_tab_mut();
                        tab.history.restore_stroke_region(&mut tab.doc, rect);
                        tab.history.cancel_stroke();
                        keep_open = false;
                    }
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::About => {
                // SPEC §26: 「バージョン(CARGO_PKG_VERSION)・リポジトリ URL
                // を表示する小モーダル」。
                match dialogs::show_about(ctx, env!("CARGO_PKG_VERSION"), REPOSITORY_URL) {
                    DialogOutcome::Confirmed | DialogOutcome::Cancelled => keep_open = false,
                    DialogOutcome::Pending => {}
                }
            }
            ModalState::Preferences {
                draft_max_undo_steps,
            } => match dialogs::show_preferences(ctx, draft_max_undo_steps) {
                DialogOutcome::Confirmed => {
                    self.apply_preferences(*draft_max_undo_steps);
                    keep_open = false;
                }
                DialogOutcome::Cancelled => {
                    keep_open = false;
                }
                DialogOutcome::Pending => {}
            },
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
    /// 来ていた(`close_was_queued`)なら `begin_quit()` に引き継ぐ(SPEC
    /// §8、v5 §17.4: 「未保存のタブがあればタブごとに順番に確認する」)。
    /// `begin_quit` は全タブを見直すので、この一時モーダルが表示されて
    /// いた間にどのタブが変更されたかを気にする必要はない。既に未保存
    /// 変更が無くなっていれば `begin_quit` が即座に終了する
    /// (`CancelClose` を既に送ってしまっているため OS の既定動作には
    /// 戻れず、明示的に終了する必要がある)。`show_modal` から切り出して
    /// あるのは、egui の `Context` を必要とせずユニットテストできるように
    /// するため。
    fn resume_queued_close_after_modal(&mut self, close_was_queued: bool) {
        if !close_was_queued {
            return;
        }
        self.begin_quit();
    }

    // -----------------------------------------------------------------
    // タイトルバー(SPEC §3)
    // -----------------------------------------------------------------

    /// SPEC §30: 「ウィンドウタイトルは引き続き `{アクティブタブのファイル名}
    /// {*} - Darask Paint` を表示する」(`Tab::label` が「無題」の番号付けも
    /// 含めて算出する)。
    fn window_doc_label(&self) -> String {
        self.active_tab().label()
    }

    /// `{ファイル名}{*} - Darask Paint`(SPEC §3、`*` は未保存変更あり)。
    fn compute_window_title(&self) -> String {
        let star = if self.active_tab().doc.modified {
            "*"
        } else {
            ""
        };
        format!("{}{star} - Darask Paint", self.window_doc_label())
    }
}

impl eframe::App for DaraskApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 起動時白画面(DWM 合成の競合)ワークアラウンド。`StartupNudge` の
        // ドキュメントコメント参照。
        self.tick_startup_nudge(ui.ctx());

        // 同ワークアラウンドの後段: 初回フレーム直後とウィンドウ内寸の変化
        // 直後は、合成器(DWM)のサーフェス作り直しと present が競合して
        // 「描画は成功しているのに画面に反映されない」ことがある(実機で
        // 確認)。競合の恐れがなくなった頃に 1 フレームだけ追加で提示して、
        // 最後の present が確実に画面へ届くようにする。サイズ変化時限定の
        // 一発予約であり、恒久ループではない(アイドル CPU 0% 要件は不変)。
        let content_rect = ui.ctx().content_rect();
        if content_rect != self.last_screen_rect {
            self.last_screen_rect = content_rect;
            ui.ctx().request_repaint_after(Duration::from_millis(150));
        }

        // v4 §26(ARCHITECTURE.md §16.7): 終了時に設定へ書き出すウィンドウ
        // 寸法・最大化状態を、毎フレーム観測して覚えておく。終了経路
        // (`on_exit`/`exit_process`)は `egui::Context` を持たないため、
        // 「今の値」をここで先に控えておく必要がある(`Option` が `None` の
        // 場合 — Android/Wayland 等 — は前回値のまま据え置く)。
        ui.ctx().input(|i| {
            let viewport = i.viewport();
            track_window_size(
                &mut self.window_size,
                &mut self.window_maximized,
                viewport.maximized,
                viewport.inner_rect,
            );
        });

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

        // v3 §19: テキスト編集中の Ctrl+Enter(確定)/Esc(破棄)は、他の
        // ショートカットと逆に「wants_keyboard_input なら無効」ではなく
        // 「編集中でなければ何もしない」というガードなので、最優先で消費する
        // (`handle_shortcuts` は `egui_wants_keyboard_input()` で自らを
        // 無効化するため衝突しないが、消費順は明示的に最初にしておく、
        // `keymap` モジュールドキュメントコメント参照)。
        self.handle_text_edit_shortcuts(ui.ctx());
        // ARCHITECTURE.md §15.4: SPEC §20 のショートカット群(ツール/色/
        // ブラシ/編集/レイヤー/表示/ファイル)は `keymap::KEYMAP` を単一の
        // 情報源とする 1 つのディスパッチに集約されている(`keymap::poll`)。
        self.handle_shortcuts(ui.ctx());
        self.handle_dropped_files(ui.ctx());

        let title = self.compute_window_title();
        if title != self.last_title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        let toast_text = self.tick_toast(ui.ctx());

        let layer_count = self.active_tab().doc.layers.len();
        let active_layer_index = self.active_tab().doc.active_index();
        // v5 §31(ARCHITECTURE.md §17.6): 「選択範囲を新規タブに複製」も
        // 「選択または浮動片がアクティブなときのみ有効」という同じ条件。
        let has_selection =
            self.active_tab().selection.is_some() || self.active_tab().floating.is_some();
        let menu_state = MenuState {
            // SPEC §13 最終項: 進行中のストロークがあっても、Undo は「先に
            // 確定してから実行」できるので有効表示にする(has_open_stroke の
            // ときは、確定によって少なくとも 1 件の undo 単位が生まれる)。
            can_undo: self.active_tab().history.can_undo()
                || self.active_tab().history.has_open_stroke(),
            can_redo: self.active_tab().history.can_redo(),
            has_selection,
            can_duplicate_selection_to_tab: has_selection,
            can_add_layer: layer_count < MAX_LAYERS,
            can_delete_layer: layer_count > 1,
            can_move_layer_up: active_layer_index + 1 < layer_count,
            can_move_layer_down: active_layer_index > 0,
            can_merge_layer_down: layer_count > 1 && active_layer_index > 0,
            can_flatten_layers: layer_count > 1,
            pixel_grid_visible: self.show_pixel_grid,
            recent_files: &self.recent_files,
        };
        if let Some(action) = menu::show(ui, &menu_state) {
            self.handle_menu_action(action);
        }

        // SPEC §30: メニューバーの直下に置くタブバー(横1列、水平スクロール)。
        // `egui::Panel::top` はメニュー(直前)の宣言順で全幅の帯を確保する
        // (ARCHITECTURE.md §14.9-7: パネルは宣言順にレイアウトが決まる —
        // ツールバー(left)/右パネル(right)/オプションバー(top)より
        // 前に置くことで、これらより先に全幅を確保できる)。
        let tab_infos: Vec<TabInfo> = self
            .tabs
            .iter()
            .map(|t| TabInfo {
                label: t.label(),
                modified: t.doc.modified,
            })
            .collect();
        if let Some(action) = tab_bar::show(ui, &tab_infos, self.active_tab) {
            self.handle_tab_bar_action(action);
        }

        // ステータスバーはレイアウト順の都合上キャンバスより先に描くため、
        // 表示するカーソル座標/ズームは 1 フレーム前の値になる
        // (ポインタ移動のたびにフレームが駆動されるため実用上は無視できる)。
        status_bar::show(
            ui,
            &self.active_tab().doc,
            self.active_tab().view.hover_img(),
            self.active_tab().view.zoom,
            self.current_selection_size(),
            toast_text.as_deref(),
        );

        if let Some(action) = toolbar::show(ui, self.tool, self.lasso_mode) {
            match action {
                ToolbarAction::SelectTool(new_tool) => self.set_tool(new_tool),
                // v6 §34: ツールバーの歯車ボタン(Ctrl+K と同じ)。
                ToolbarAction::OpenPreferences => self.open_preferences_modal(),
            }
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
        // `doc`/`layer_rename` は今や同じ `Tab` の disjoint なフィールドに
        // なった(バグ修正: クロスタブ破損防止のためタブごとに独立させた、
        // `Tab` の docstring 参照)ため、`active_tab_mut()`(`*self` 全体を
        // 借用してしまうメソッド呼び出し)ではなく `Tab` への可変参照を
        // 1 回だけ取って両フィールドへ分割借用する。
        let tab = &mut self.tabs[self.active_tab];
        let (layer_action, history_jump) = side_panel::show(
            ui,
            &mut tab.doc,
            &mut tab.layer_rename,
            &tab.history,
            color_ctx,
        );
        if let Some(action) = layer_action {
            self.handle_layers_panel_action(action);
        }
        // v6-M3(SPEC §35、ARCHITECTURE.md §18.4): 履歴パネルの行クリック。
        if let Some(target_len) = history_jump {
            self.jump_history_to(target_len);
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
                    brush_hardness: &mut self.brush_hardness,
                    brush_opacity: &mut self.brush_opacity,
                    pencil_mode: &mut self.pencil_mode,
                    brush_smoothing: &mut self.brush_smoothing,
                    shape_mode,
                    fill_tolerance: &mut self.fill.tolerance,
                    gradient_kind: &mut self.gradient.kind,
                    gradient_colors: &mut self.gradient.colors,
                    text_font_size: &mut self.text_font_size,
                    lasso_mode: self.lasso_mode,
                    magic_wand_tolerance: &mut self.magic_wand_tolerance,
                },
            );
        }

        let force_pan = self.tool == ToolKind::Pan;
        let alt_held = ui.ctx().input(|i| i.modifiers.alt);
        let cursor = self.cursor_for_active_tool(alt_held);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(64)))
            .show(ui, |ui| {
                let tab = &mut self.tabs[self.active_tab];
                let output = tab.view.show(ui, &mut tab.doc, force_pan, cursor);
                // SPEC §25: ピクセルグリッド(トグル ON かつズーム 800% 以上
                // のときだけ)。画像の直後・ツールプレビューより前に描く。
                if self.show_pixel_grid {
                    let tab = &self.tabs[self.active_tab];
                    tab.view.draw_pixel_grid(&output.painter, &tab.doc);
                }
                // ARCHITECTURE.md §3: 市松模様→画像→ツールプレビュー→選択枠の順。
                match self.tool {
                    ToolKind::Pen => self.pen.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Eraser => self.eraser.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Line => self.line.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Rect => self.rect_tool.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Ellipse => self.ellipse.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    ToolKind::Gradient => self.gradient.draw_preview(
                        &output.painter,
                        &self.active_tab().view,
                        self.primary,
                        self.secondary,
                        self.brush_size,
                    ),
                    // 選択・移動・楕円選択は `draw_selection_overlay` が
                    // 浮動片/ハンドルを描く(下記、v4 §22: 楕円選択も同じ
                    // オーバーレイを共有する)。なげなわは同じ
                    // `draw_selection_overlay` の中で進行中の軌跡/頂点列も
                    // 描く。ズーム・自動選択はプレビューを持たない。テキストは
                    // `draw_text_edit_overlay`(下記)が別枠で描く。
                    ToolKind::Fill
                    | ToolKind::Picker
                    | ToolKind::Select
                    | ToolKind::Pan
                    | ToolKind::Move
                    | ToolKind::Zoom
                    | ToolKind::Text
                    | ToolKind::EllipseSelect
                    | ToolKind::Lasso
                    | ToolKind::MagicWand => {}
                }
                // SPEC §17: ブラシ/消しゴム使用中は OS カーソルの代わりに
                // 円アウトラインを描く(ARCHITECTURE.md §15.6 落とし穴5:
                // 「選択・移動・テキスト等では出さない」— ブラシ/消しゴム/
                // 鉛筆モードのみ)。v3 レビューで発見・修正したバグ: Space/
                // 中ボタンでのパンジェスチャ中も無条件に円を描いていたため、
                // OS カーソルが Grabbing に切り替わる(`CanvasView::show` の
                // `effective_cursor`)のと二重表示になっていた。パン中は
                // 円を出さない(SPEC §17「円表示時は OS カーソル非表示」の
                // 排他が崩れないようにする)。
                if matches!(self.tool, ToolKind::Pen | ToolKind::Eraser)
                    && !self.active_tab().view.is_panning()
                {
                    if let Some(hover) = self.active_tab().view.hover_img() {
                        self.draw_brush_cursor(&output.painter, hover);
                    }
                }
                self.draw_selection_overlay(&output.painter);
                self.dispatch_canvas_events(output.events);
                // v3 §19: テキスト編集オーバーレイ。`dispatch_canvas_events` の
                // **後**に呼ぶこと — 先に呼ぶと、このフレームで「ボックス外
                // クリック」による確定(`lost_focus()`)が起きた場合に
                // `self.text_edit` が既に `None` になり、直後に処理される同じ
                // フレームの Down イベントを「未編集」と誤認して同じ位置に
                // 即座に新しい編集を開始してしまう(ARCHITECTURE.md §15.6-1
                // と同種の確定順序の罠)。
                self.draw_text_edit_overlay(ui);
            });

        self.show_modal(ui.ctx());

        // ①ベンチ処理(SPEC §11): 2 フレーム目の描画が終わった時点で
        // bench.txt に経過ミリ秒を書き出し、直ちにプロセスを終了する。
        // v4 §16.2(SPEC §28): フェーズ内訳(設定読込/フォント/ウィンドウ
        // 作成/初フレーム)も合わせて書き出す。
        if let Some(bench) = &mut self.bench {
            bench.frames_drawn += 1;
            if bench.frames_drawn == 1 {
                bench
                    .phases
                    .push(("first_frame", bench.process_start.elapsed().as_millis()));
            }
            if bench.frames_drawn >= 2 {
                let elapsed_ms = bench.process_start.elapsed().as_millis();
                bench.phases.push(("second_frame", elapsed_ms));
                // 1 行目は `total_ms`(後方互換、SPEC §11)。以降は
                // `phase\tms` 行(ARCHITECTURE.md §16.2)。
                let mut content = elapsed_ms.to_string();
                for (name, ms) in &bench.phases {
                    content.push('\n');
                    content.push_str(name);
                    content.push('\t');
                    content.push_str(&ms.to_string());
                }
                // I/O エラーでパニックしないこと(SPEC §12)。書き込みに
                // 失敗してもスモークテストとしてはプロセスを終了させる。
                let _ = std::fs::write("bench.txt", content);
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

    /// SPEC §26(ARCHITECTURE.md §16.7): 「書き込みは終了時…のみ」。
    /// `eframe` は「未保存変更が無い状態でウィンドウの X を閉じる/Alt+F4」
    /// のように `handle_close_request` が `CancelClose` を送らずに戻った
    /// 場合、通常の(`std::process::exit` を経ない)シャットダウン処理として
    /// これを 1 回だけ呼ぶ。一方、本アプリが未保存確認を経て自ら終了する
    /// 経路(`exit_process`、`メニュー>終了`・確認モーダルの保存/破棄後)は
    /// `std::process::exit` で即座にプロセスを終了するため、この
    /// `on_exit` は呼ばれない(Rust の通常のアンワインド/デストラクタ・
    /// トレイトメソッド呼び出しを経ないため) — その経路では
    /// `exit_process` 自身が `save_settings` を呼ぶことで同じ保証を満たす。
    /// ベンチモード(SPEC §11)は `std::process::exit` で終了するためここは
    /// 呼ばれず、実 `%APPDATA%` を書き換えない(意図的、`exit_process` の
    /// ドキュメントコメント参照)。
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

/// v4 §26(ARCHITECTURE.md §16.7)の「終了時に保存するウィンドウ寸法・
/// 最大化状態」の追跡ロジック(`DaraskApp::ui` から毎フレーム呼ぶ)。
/// egui の `Context` に依存しない純粋関数として切り出してあるのでテスト
/// できる。
///
/// v4 レビューで発見・修正したバグ: 以前は `maximized` 中でも `inner_rect`
/// (最大化時は画面全体のクライアント寸法)を無条件で `window_size` に
/// 上書きしていた。そのため設定保存時に `window.maximized=1` と「最大化時の
/// 寸法」が同時に書き出され、次回起動が最大化で復元された後にユーザーが
/// 最大化を解除すると、ウィンドウはほぼ画面いっぱいのサイズになり、元の
/// (最大化前の)サイズはどこにも残っていなかった。`maximized` フラグを
/// 先に更新してから、それが偽のフレームでだけ `window_size` を更新する
/// ことで、「直近の非最大化時サイズ」を保持し続ける。`viewport_maximized`/
/// `viewport_inner_rect` が `None`(Android/Wayland 等でウィンドウ情報が
/// 取れない場合)なら、それぞれ前回値のまま据え置く。
fn track_window_size(
    window_size: &mut egui::Vec2,
    window_maximized: &mut bool,
    viewport_maximized: Option<bool>,
    viewport_inner_rect: Option<egui::Rect>,
) {
    if let Some(maximized) = viewport_maximized {
        *window_maximized = maximized;
    }
    if !*window_maximized {
        if let Some(inner_rect) = viewport_inner_rect {
            *window_size = inner_rect.size();
        }
    }
}

/// v5 §30: 「開こうとしたファイルが既に開いているタブがあれば(パスを
/// 正規化して比較)」。シンボリックリンク解決・大文字小文字/`..`/相対パスの
/// 違いを吸収した絶対パスにする。存在しない・アクセスできないパス(理論上
/// ここには来ないはずだが、I/O は常に失敗しうる、CLAUDE.md 鉄則: I/O 経路で
/// `unwrap()` しない)の場合は元のパスをそのまま返す(比較の精度は落ちるが
/// panic はしない)。
fn normalize_path_for_compare(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// SPEC §30: 「タブ数の上限は 24。超えて新規タブを作ろうとしたら作成せず
/// トースト通知」。
fn tab_limit_toast_message() -> String {
    format!("これ以上タブを開けません(上限{MAX_TABS}件)")
}

/// ARCHITECTURE.md §9: egui のデフォルトフォントに日本語グリフは無いため、
/// Windows システムフォントを追加する。`App::new` 相当(ここでは
/// `DaraskApp::new`)で一度だけ呼ぶ。
///
/// v4 §16.2: ファイル読み込み自体(`text::load_font_bytes`)は `main()` が
/// ウィンドウ作成と並行する別スレッドで先に行い、ここでは読み込み済みの
/// バイト列(見つからなければ `None`)を受け取って egui へ登録するだけに
/// した(旧実装は `ctx.add_font` の直前でファイル読み込みも行っており、
/// ウィンドウ作成と直列だった)。
///
/// v3 §19(ARCHITECTURE.md §15.3)でテキストツールが同じバイト列を
/// `ab_glyph::FontRef` の構築に使うため、読み込んだバイト列を `Arc<Vec<u8>>`
/// として返す。egui にはこのバイト列の複製を渡す(egui 側は `FontData` と
/// して所有権ごと消費するため、テキストツール用に別途保持する分は 1 回だけ
/// メモリ上でクローンする — ディスク読み込みは 1 回きりで済む)。
fn register_japanese_font(ctx: &egui::Context, bytes: Option<Vec<u8>>) -> Option<Arc<Vec<u8>>> {
    let Some(bytes) = bytes else {
        // ARCHITECTURE.md §9-4: 全部読めなければ警告ログだけ出して続行する
        // (Win11 では起きない想定)。`log` crate は依存に追加しない方針
        // (CLAUDE.md)のため `eprintln!` で代替する。
        // `windows_subsystem = "windows"` によりコンソールが無い環境では
        // 単に出力先が失われるだけでパニックはしない。
        eprintln!(
            "警告: 日本語フォントが見つかりませんでした(YuGothM/meiryo/msgothic)。文字が正しく表示されない可能性があります。"
        );
        return None;
    };
    ctx.add_font(FontInsert::new(
        "darask-jp",
        egui::FontData::from_owned(bytes.clone()),
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
    Some(Arc::new(bytes))
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
            tabs: vec![Tab::new(doc, Some(1), settings::DEFAULT_MAX_UNDO_STEPS)],
            active_tab: 0,
            next_untitled_number: 2,
            tool: ToolKind::Pen,
            last_shape_tool: ToolKind::Line,
            last_marquee_tool: ToolKind::Select,
            last_fill_tool: ToolKind::Fill,
            pen: PenTool::new(),
            eraser: EraserTool::new(),
            line: ShapeTool::new_line(),
            rect_tool: ShapeTool::new_rect(),
            ellipse: ShapeTool::new_ellipse(),
            fill: FillTool::new(),
            picker: PickerTool::new(),
            gradient: GradientTool::new(),
            lasso_mode: LassoMode::Freehand,
            lasso_freehand_points: Vec::new(),
            lasso_polygon: None,
            magic_wand_tolerance: 0,
            primary: Color32::BLACK,
            secondary: Color32::WHITE,
            brush_size: settings::DEFAULT_BRUSH_SIZE,
            brush_hardness: settings::DEFAULT_BRUSH_HARDNESS,
            brush_opacity: settings::DEFAULT_BRUSH_OPACITY,
            pencil_mode: false,
            brush_smoothing: settings::DEFAULT_BRUSH_SMOOTHING,
            recent_colors: VecDeque::new(),
            alt_eyedropper_active: false,
            show_pixel_grid: true,
            max_undo_steps: settings::DEFAULT_MAX_UNDO_STEPS,
            color_wheel: ColorWheelState::new(),
            // 起動 1 フレーム目から正しい表記を出す(空文字だと 1 フレーム
            // だけ空欄がちらつく)。プライマリの初期値(黒)に合わせる。
            color_hex_buffer: color_panel::format_hex(Color32::BLACK),
            user_palette: Vec::new(),
            select_drag: None,
            next_floating_id: 0,
            text_font: None,
            text_font_size: DEFAULT_TEXT_FONT_SIZE,
            text_edit: None,
            modal: None,
            pending_action: None,
            pending_dialog: None,
            after_save_action: None,
            last_jpeg_quality: DEFAULT_JPEG_QUALITY,
            last_title: String::new(),
            toast: None,
            recent_files: VecDeque::new(),
            // テストにはウィンドウが無いため、ワークアラウンドは常に完了
            // 状態にしておく。
            startup_nudge: StartupNudge::Done,
            last_screen_rect: egui::Rect::NOTHING,
            window_size: egui::vec2(
                settings::DEFAULT_WINDOW_WIDTH as f32,
                settings::DEFAULT_WINDOW_HEIGHT as f32,
            ),
            window_maximized: false,
            // テストは実 `%APPDATA%` を書き換えない(`save_settings` 参照)。
            persist_settings: false,
            bench: None,
        }
    }

    // -- V3-M4: SPEC §20「U: 図形(直前に使った図形)」/「Shift+U で巡回」 ---

    #[test]
    fn set_tool_tracks_last_shape_tool_only_for_shape_kinds() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        assert_eq!(
            app.last_shape_tool,
            ToolKind::Line,
            "initial default is Line"
        );

        app.set_tool(ToolKind::Rect);
        assert_eq!(app.last_shape_tool, ToolKind::Rect);

        // 図形以外へ切り替えても最後に使った図形は保持される。
        app.set_tool(ToolKind::Pen);
        assert_eq!(app.tool, ToolKind::Pen);
        assert_eq!(app.last_shape_tool, ToolKind::Rect);

        app.set_tool(ToolKind::Ellipse);
        assert_eq!(app.last_shape_tool, ToolKind::Ellipse);
    }

    #[test]
    fn cycle_shape_tool_goes_line_rect_ellipse_and_wraps() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::Line);

        app.cycle_shape_tool();
        assert_eq!(app.tool, ToolKind::Rect);
        app.cycle_shape_tool();
        assert_eq!(app.tool, ToolKind::Ellipse);
        app.cycle_shape_tool();
        assert_eq!(app.tool, ToolKind::Line, "cycle wraps back to Line");
    }

    #[test]
    fn cycle_shape_tool_from_a_non_shape_tool_starts_from_last_shape_tool() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::Rect);
        app.set_tool(ToolKind::Pen); // Rect が「直前に使った図形」のまま。

        app.cycle_shape_tool();

        assert_eq!(
            app.tool,
            ToolKind::Ellipse,
            "cycling while on a non-shape tool advances from last_shape_tool, not from Pen"
        );
    }

    // -- V3-M4: `handle_shortcuts` 経由の end-to-end ディスパッチ確認 --------
    // egui は `Context::begin_pass` にバックエンド(ウィンドウ)を要求しない
    // ため、実際のキー入力イベントを注入して `app.handle_shortcuts` を
    // 直接駆動できる。`keymap::KEYMAP` のバインド自体は `keymap.rs` の
    // 単体テストで確認済みなので、ここでは「バインドから `app.rs` の
    // 実処理まで実際につながっているか」(結線)だけを確認する。

    fn ctx_with_key_event(key: Key, modifiers: Modifiers) -> egui::Context {
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        ctx.begin_pass(raw_input);
        ctx
    }

    #[test]
    fn d_key_resets_colors_to_black_and_white() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.primary = Color32::from_rgb(10, 20, 30);
        app.secondary = Color32::from_rgb(200, 150, 100);

        // SPEC §20: 「D 初期色(黒・白)」。
        let ctx = ctx_with_key_event(Key::D, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.primary, Color32::BLACK);
        assert_eq!(app.secondary, Color32::WHITE);
    }

    #[test]
    fn ctrl_j_duplicates_the_active_layer() {
        // SPEC §20: 「Ctrl+J 複製」(旧 v2 はレイヤーパネル/メニューのみ)。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        let before = app.active_tab().doc.layers.len();

        let ctx = ctx_with_key_event(Key::J, Modifiers::CTRL);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.active_tab().doc.layers.len(), before + 1);
        assert!(app.active_tab().history.can_undo());
    }

    #[test]
    fn g_key_selects_fill_tool_replacing_old_f() {
        // SPEC §20: 「G 塗りつぶし(旧 F から変更)」。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tool = ToolKind::Pen;

        let ctx = ctx_with_key_event(Key::G, Modifiers::NONE);
        app.handle_shortcuts(&ctx);
        assert_eq!(app.tool, ToolKind::Fill);
    }

    #[test]
    fn old_r_c_f_keys_no_longer_change_the_tool() {
        // SPEC §20: 「旧 L/R/C は廃止」。塗りつぶしは F→G に変わったので F も
        // 含める。v4 §22 で `L` はなげなわとして復活した(下の
        // `l_key_selects_lasso` が別途検証する)ので、ここでは対象外にする。
        for key in [Key::R, Key::C, Key::F] {
            let mut app = new_for_test(Document::new(4, 4, Background::White));
            app.tool = ToolKind::Pen;

            let ctx = ctx_with_key_event(key, Modifiers::NONE);
            app.handle_shortcuts(&ctx);

            assert_eq!(
                app.tool,
                ToolKind::Pen,
                "{key:?} must no longer switch tools"
            );
        }
    }

    #[test]
    fn l_key_selects_lasso() {
        // v4 §22: `L` は廃止された旧ショートカットではなく、なげなわを選ぶ。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tool = ToolKind::Pen;

        let ctx = ctx_with_key_event(Key::L, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tool, ToolKind::Lasso);
    }

    #[test]
    fn u_key_selects_the_last_used_shape_tool() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.set_tool(ToolKind::Rect); // last_shape_tool = Rect
        app.set_tool(ToolKind::Pen);

        let ctx = ctx_with_key_event(Key::U, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tool, ToolKind::Rect);
    }

    #[test]
    fn shift_u_cycles_without_also_triggering_bare_u() {
        // ARCHITECTURE.md §15.6 落とし穴6: Shift+U は素の U より先に消費
        // されなければならない。誤って両方発火すると「巡回してから直前の
        // 図形に戻る」ような二重発火が起き、実質何も進まなくなる。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.set_tool(ToolKind::Line);

        let ctx = ctx_with_key_event(Key::U, Modifiers::SHIFT);
        app.handle_shortcuts(&ctx);

        assert_eq!(
            app.tool,
            ToolKind::Rect,
            "Shift+U must cycle exactly once (Line -> Rect), not be swallowed by bare U too"
        );
    }

    // -- 貼り付けが他ツールの begin_stroke に破棄されるバグ(修正済み) ------

    #[test]
    fn begin_paste_floating_switches_tool_to_select() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.active_tab_mut().doc.modified = true; // 白紙ではない状態を再現する。
        app.tool = ToolKind::Pen;

        app.begin_paste_floating(4, 4, [255, 0, 0, 255].repeat(16));

        assert_eq!(
            app.tool,
            ToolKind::Select,
            "paste must switch to Select so a later Pen Down cannot discard the open stroke"
        );
        assert!(app.active_tab().floating.is_some());
        assert!(app.active_tab().history.has_open_stroke());
        assert!(
            app.active_tab().doc.modified,
            "an uncommitted floating paste must already count as an unsaved change"
        );
    }

    #[test]
    fn begin_paste_floating_commit_pushes_a_single_undo_unit() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.active_tab_mut().doc.modified = true;
        app.tool = ToolKind::Pen;

        app.begin_paste_floating(4, 4, [255, 0, 0, 255].repeat(16));
        // ペンでキャンバスをクリックしても(tool は既に Select なので)ペンの
        // begin_stroke には届かず、貼り付け用のレコーダは破棄されない。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(1.0, 1.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(
            app.active_tab().history.has_open_stroke(),
            "recorder must survive"
        );

        app.commit_selection();
        assert!(
            app.active_tab().history.can_undo(),
            "committing the pasted floating must push exactly one undo unit"
        );
        assert!(!app.active_tab().history.has_open_stroke());
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
        assert!(app.active_tab().history.has_open_stroke());
        // StrokeTool は commit_stroke までは modified を立てないため、
        // 修正前はここでまだ pristine 判定のままだった。
        let painted_stroke = app.active_tab().doc.get_pixel(5, 5);
        assert_ne!(painted_stroke, Some([255, 255, 255, 255]));

        app.paste_pixels(16, 16, [0, 255, 0, 255].repeat(256));

        // 元のペンストロークは、貼り付け用の(まだ未確定の)浮動片とは
        // 独立した undo 単位として、先に確定されているはず。
        assert!(
            app.active_tab().history.can_undo(),
            "the pen stroke must have been committed as its own undo unit before pasting"
        );
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (20, 20),
            "the document must not be replaced while a stroke was open (it is no longer pristine after the commit)"
        );
        assert!(
            app.active_tab().floating.is_some(),
            "the paste must float onto the existing document instead of replacing it"
        );
        assert!(
            app.active_tab().history.has_open_stroke(),
            "the paste itself legitimately opens its own separate, not-yet-committed stroke for the pending floating piece"
        );

        // undo を 2 回: ①貼り付け確定 ②ペンストローク。どちらも壊れずに
        // バイト正確に復元できる。
        app.commit_selection(); // Enter 相当で貼り付けを確定する。
        assert!(!app.active_tab().history.has_open_stroke());
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            painted_stroke,
            "undoing the paste must restore the just-drawn pen pixel, not a stale white one"
        );
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            Some([255, 255, 255, 255])
        );
    }

    #[test]
    fn pasting_onto_a_pristine_document_commits_a_pending_layer_rename_first() {
        // 回帰テスト(バグ修正): `replace_document_with_pasted_image`(SPEC
        // §6 の白紙置き換え貼り付け)は以前、レイヤー名編集中でも単に
        // `layer_rename = None` で入力内容を破棄するだけだった。しかも
        // `doc.modified` も立てていなかったため `doc_is_pristine()` が
        // 誤って「白紙」のままと判定し、レイヤー名を編集しただけの
        // ドキュメントごと貼り付け画像に置き換えてしまっていた。
        // `commit_open_gesture`(`paste_pixels` が先頭で呼ぶ)が編集中の
        // レイヤー名を先に確定するようになったことで、`doc.modified` が
        // 正しく立ち、「完全に未編集」ではなくなるため、この場合は白紙
        // 置き換えではなく通常の浮動片貼り付け(ドキュメントは無傷)になる。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        assert!(app.doc_is_pristine());
        app.active_tab_mut().layer_rename = Some((0, "新しい名前".to_owned(), false));

        app.paste_pixels(4, 4, [0, 255, 0, 255].repeat(16));

        assert!(
            app.active_tab().layer_rename.is_none(),
            "the rename box must be closed, not left dangling"
        );
        assert_eq!(
            app.active_tab().doc.layers[0].name,
            "新しい名前",
            "the typed name must be committed, not silently discarded"
        );
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (20, 20),
            "an in-progress rename counts as a real edit, so the document must not be \
             replaced wholesale by the pristine-paste shortcut"
        );
        assert!(
            app.active_tab().floating.is_some(),
            "the paste must float onto the existing (renamed) document instead"
        );
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
        assert!(app.active_tab().history.has_open_stroke());
        let painted = app.active_tab().doc.get_pixel(5, 5);
        assert_ne!(painted, Some([255, 255, 255, 255]));

        app.set_tool(ToolKind::Eraser);

        assert!(
            !app.active_tab().history.has_open_stroke(),
            "switching tools must commit the in-progress stroke, not discard it"
        );
        assert!(app.active_tab().history.can_undo());
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            Some([255, 255, 255, 255])
        );
        assert!({
            let tab = app.active_tab_mut();
            tab.history.redo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.get_pixel(5, 5), painted);
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
        assert!(app.active_tab().history.can_undo());
        assert!(!app.active_tab().history.has_open_stroke());
        let painted_stroke1 = app.active_tab().doc.get_pixel(2, 2);
        assert_ne!(painted_stroke1, Some([255, 255, 255, 255]));

        // ストローク2: Down だけ送ってストロークを開いたままにする。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.active_tab().history.has_open_stroke());
        let painted_stroke2 = app.active_tab().doc.get_pixel(10, 10);
        assert_ne!(painted_stroke2, Some([255, 255, 255, 255]));

        app.handle_menu_action(MenuAction::Undo);

        assert!(
            !app.active_tab().history.has_open_stroke(),
            "undo must commit the open stroke first (same as switching tools)"
        );
        // ストローク2 は確定直後に取り消されるので消えている。
        assert_eq!(
            app.active_tab().doc.get_pixel(10, 10),
            Some([255, 255, 255, 255])
        );
        // ストローク1 は無傷のまま残る。
        assert_eq!(app.active_tab().doc.get_pixel(2, 2), painted_stroke1);
        assert!(
            app.active_tab().history.can_undo(),
            "stroke1 must remain on the undo stack"
        );
        assert!(
            app.active_tab().history.can_redo(),
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
        assert!(app.active_tab().history.can_redo());

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.active_tab().history.has_open_stroke());
        let painted_stroke2 = app.active_tab().doc.get_pixel(10, 10);

        app.handle_menu_action(MenuAction::Redo);

        assert!(
            !app.active_tab().history.has_open_stroke(),
            "redo must commit the open stroke first (same as switching tools)"
        );
        // ストローク2 の確定が新規 push なので redo スタック(stroke1)は
        // クリアされ、この redo 呼び出し自体は何もしない(no-op)。
        assert!(
            !app.active_tab().history.can_redo(),
            "committing stroke2 must have cleared the redo stack"
        );
        assert_eq!(app.active_tab().doc.get_pixel(10, 10), painted_stroke2);
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
            app.active_tab().selection.is_none(),
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

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("a real drag must create a selection");
        assert_eq!(
            (
                selection.mask.bbox.x0,
                selection.mask.bbox.y0,
                selection.mask.bbox.x1,
                selection.mask.bbox.y1
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
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 18,
            y1: 18,
        })));

        app.handle_select_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(
            app.active_tab().floating.is_none(),
            "a plain click must not float the selection yet"
        );

        app.handle_select_event(ToolEvent::Up {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
        });
        assert!(app.active_tab().floating.is_none());
        assert!(
            app.active_tab().selection.is_some(),
            "selection should remain after a no-op click"
        );
        assert!(
            !app.active_tab().history.can_undo(),
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
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 18,
            y1: 18,
        })));

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
            .active_tab()
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
        assert!(app.active_tab().selection.is_some());
        assert_eq!(app.tool, ToolKind::Pen, "select_all must not switch tools");

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.active_tab().history.has_open_stroke());
        let painted_stroke = app.active_tab().doc.get_pixel(5, 5);
        assert_ne!(painted_stroke, Some([255, 255, 255, 255]));

        app.delete_selection();

        assert!(
            !app.active_tab().history.has_open_stroke(),
            "the pen stroke must have been committed as its own undo unit"
        );
        assert!(
            app.active_tab().selection.is_none(),
            "the full-canvas selection must have been deleted (made transparent)"
        );
        // 削除パッチは全面透明化のはず(選択の消去、SPEC §6)。
        assert_eq!(app.active_tab().doc.get_pixel(0, 0), Some([0, 0, 0, 0]));

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
        let painted_after_delete = app.active_tab().doc.get_pixel(10, 10);
        assert_ne!(
            painted_after_delete,
            Some([0, 0, 0, 0]),
            "a fresh stroke after delete must actually draw, proving StrokeTool's state was not corrupted"
        );

        // undo 3 回: ③新ストローク ②選択削除 ①ペンの最初のストローク、で
        // それぞれバイト正確に復元できる(ストロークが破損したパッチに
        // 焼き込まれたり、未確定のまま残ったりしていない)。
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            painted_stroke,
            "undoing the delete must restore the pen dot painted just before Delete was pressed"
        );
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            Some([255, 255, 255, 255])
        );
    }

    // -- Ctrl+X がコピー失敗時にも削除してしまうバグ(修正済み) -------------

    #[test]
    fn cut_does_not_delete_when_clipboard_copy_fails() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        // 幅/高さ 0 は io::copy_image_to_clipboard が OS クリップボードに
        // 触れる前に決定的に失敗させる(ARCHITECTURE.md §12-8)ので、実際の
        // OS クリップボード状態に依存せずにこの経路をテストできる。
        app.active_tab_mut().floating =
            Some(Floating::new_rect(vec![], 0, 0, pos2(0.0, 0.0), None, 1));

        app.cut_selection_to_clipboard();

        assert!(
            app.active_tab().floating.is_some(),
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
            app.active_tab().doc.path,
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
        app.active_tab_mut().doc.modified = true;
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
        app.active_tab_mut().doc.modified = true;

        app.resume_queued_close_after_modal(false);

        assert!(app.modal.is_none());
    }

    // -- v2 §13: レイヤー操作(ARCHITECTURE.md §14.8 V2-M2 受け入れ基準) -----

    #[test]
    fn layer_add_inserts_a_new_layer_as_a_single_undo_unit() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.layer_add();
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        assert_eq!(app.active_tab().doc.layers[1].name, "レイヤー 1");
        assert!(app.active_tab().history.can_undo());

        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        assert!(app.active_tab().history.can_redo());
        assert!({
            let tab = app.active_tab_mut();
            tab.history.redo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 2);
    }

    #[test]
    fn layer_add_names_increment_regardless_of_deletions() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_add();
        assert_eq!(app.active_tab().doc.layers[1].name, "レイヤー 1");
        app.layer_delete();
        app.layer_add();
        assert_eq!(app.active_tab().doc.layers[1].name, "レイヤー 2");
    }

    #[test]
    fn layer_add_numbering_is_independent_per_tab() {
        // 回帰テスト(バグ修正): 「レイヤー N」の採番カウンタは以前
        // `DaraskApp` 直下の共有フィールドだったため、タブを切り替えて
        // 別タブでレイヤーを追加すると番号が続きから採番され、タブ単体で
        // 見ると 1 から連番にならず歯抜けになっていた
        // (`Tab::next_layer_number` のドキュメントコメント参照)。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_add();
        assert_eq!(app.active_tab().doc.layers[1].name, "レイヤー 1");

        app.open_new_tab(Document::new(4, 4, Background::White));
        app.layer_add();
        assert_eq!(
            app.active_tab().doc.layers[1].name,
            "レイヤー 1",
            "a brand-new tab's first added layer must be numbered 1, not continue \
             from another tab's counter"
        );

        app.switch_tab(0);
        app.layer_add();
        assert_eq!(
            app.active_tab().doc.layers[2].name,
            "レイヤー 2",
            "switching back must not have disturbed this tab's own counter"
        );
    }

    #[test]
    fn layer_delete_and_merge_down_are_no_ops_with_a_single_layer() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_delete();
        assert_eq!(
            app.active_tab().doc.layers.len(),
            1,
            "must refuse to delete the last layer"
        );
        assert!(
            !app.active_tab().history.can_undo(),
            "a refused op must not push undo"
        );

        app.layer_merge_down();
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        assert!(!app.active_tab().history.can_undo());
    }

    #[test]
    fn layer_add_refuses_past_the_64_layer_cap() {
        let mut app = new_for_test(Document::new(1, 1, Background::White));
        // v6 §34/ARCHITECTURE.md §18.3: `History` の既定の保持数上限
        // (`DEFAULT_MAX_STEPS` = 50)は `MAX_LAYERS - 1`(63)より小さいため、
        // このテスト固有の目的(レイヤー上限到達時に undo エントリが
        // 積まれないこと)を保持数キャップの影響で汚染しないよう、ここだけ
        // 明示的に引き上げておく(このテストは History の保持数キャップ自体を
        // 検証する場ではない、それは history.rs 側のテストが担う)。
        app.active_tab_mut().history.set_max_steps(MAX_LAYERS);
        for _ in 0..(MAX_LAYERS - 1) {
            app.layer_add();
        }
        assert_eq!(app.active_tab().doc.layers.len(), MAX_LAYERS);

        app.layer_add(); // 上限到達、拒否されるはず。
        assert_eq!(
            app.active_tab().doc.layers.len(),
            MAX_LAYERS,
            "must refuse to exceed MAX_LAYERS"
        );

        // 上限到達で拒否された呼び出しは undo エントリを積まない: ちょうど
        // MAX_LAYERS - 1 回の undo で元の 1 枚まで戻り、それ以上は戻せない。
        for _ in 0..(MAX_LAYERS - 1) {
            assert!({
                let tab = app.active_tab_mut();
                tab.history.undo(&mut tab.doc)
            });
        }
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        assert!(
            !{
                let tab = app.active_tab_mut();
                tab.history.undo(&mut tab.doc)
            },
            "the refused add must not have pushed an undo entry"
        );
    }

    // -- v6 §34(V6-M2、ARCHITECTURE.md §18.2): 設定(環境設定)ダイアログ ----

    /// `History` は `undo_stack` の長さを公開しないため、既存の公開 API
    /// (`undo`/`redo`)だけを使って積まれているエントリ数を数える(このテスト
    /// セクション専用のヘルパー)。呼び出し後は全部やり直して呼び出し側の
    /// 状態を壊さないようにする。
    fn count_undo_entries(history: &mut History, doc: &mut Document) -> usize {
        let mut n = 0;
        while history.undo(doc) {
            n += 1;
        }
        while history.redo(doc) {}
        n
    }

    #[test]
    fn open_preferences_modal_seeds_draft_from_current_value() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.max_undo_steps = 77;
        app.open_preferences_modal();
        let Some(ModalState::Preferences {
            draft_max_undo_steps,
        }) = app.modal
        else {
            panic!("expected ModalState::Preferences to be open");
        };
        assert_eq!(draft_max_undo_steps, 77);
    }

    #[test]
    fn apply_preferences_updates_max_undo_steps_field() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        assert_eq!(app.max_undo_steps, settings::DEFAULT_MAX_UNDO_STEPS);
        app.apply_preferences(200);
        assert_eq!(app.max_undo_steps, 200);
    }

    /// SPEC §34/ARCHITECTURE.md §18.6-2: 「`max_steps` を小さく設定変更した
    /// 瞬間に現在開いている全タブへ反映すること」。2 つのタブそれぞれに
    /// デフォルト上限(50)より多くない範囲でエントリを積んでおき、保持数を
    /// 3 へ下げたら**両方の**タブがその場で 3 件まで切り詰められることを
    /// 確認する(新規タブだけに適用され既存タブが取り残される、という
    /// バグの回帰テスト)。
    #[test]
    fn apply_preferences_truncates_every_open_tabs_history_immediately() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        for _ in 0..5 {
            app.apply_invert();
        }
        app.open_new_tab(Document::new(4, 4, Background::White));
        for _ in 0..5 {
            app.apply_invert();
        }
        assert_eq!(app.tabs.len(), 2);

        app.apply_preferences(3);

        for tab in &mut app.tabs {
            let n = count_undo_entries(&mut tab.history, &mut tab.doc);
            assert_eq!(
                n, 3,
                "every open tab's history must be capped to the new limit, not just the active one"
            );
        }
    }

    /// SPEC §34: 新規タブも、既に変更済みの保持数を最初から使うこと(既定の
    /// 50 に一瞬でも取り残されない)。
    #[test]
    fn new_tab_inherits_the_currently_configured_max_undo_steps() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.apply_preferences(3);
        app.open_new_tab(Document::new(4, 4, Background::White));

        for _ in 0..5 {
            app.apply_invert();
        }
        let tab = &mut app.tabs[app.active_tab];
        let n = count_undo_entries(&mut tab.history, &mut tab.doc);
        assert_eq!(
            n, 3,
            "a newly created tab must already use the app-wide max_undo_steps, not the History default"
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
        assert_eq!(app.active_tab().doc.layers.len(), 41);
        for i in 0..40 {
            assert!(
                {
                    let tab = app.active_tab_mut();
                    tab.history.undo(&mut tab.doc)
                },
                "AddLayer entry #{i} must not have been evicted by the 256MB history limit"
            );
        }
        assert_eq!(app.active_tab().doc.layers.len(), 1);
    }

    #[test]
    fn layer_duplicate_history_round_trips_via_app() {
        let mut app = new_for_test(Document::new(4, 4, Background::Transparent));
        app.active_tab_mut().doc.set_pixel(0, 0, [5, 6, 7, 255]);
        app.layer_duplicate();
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        assert_eq!(app.active_tab().doc.layers[1].pixels[0..4], [5, 6, 7, 255]);

        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 1);

        assert!({
            let tab = app.active_tab_mut();
            tab.history.redo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        assert_eq!(app.active_tab().doc.layers[1].pixels[0..4], [5, 6, 7, 255]);
    }

    #[test]
    fn layer_move_up_and_down_history_round_trip_via_app() {
        let mut app = new_for_test(Document::new(2, 2, Background::White));
        app.active_tab_mut().doc.layers[0].name = "下".to_owned();
        app.layer_add();
        app.active_tab_mut().doc.layers[1].name = "上".to_owned();
        app.layer_move_down();
        assert_eq!(app.active_tab().doc.layers[0].name, "上");
        assert_eq!(app.active_tab().doc.active, 0);

        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers[0].name, "下");
        assert_eq!(app.active_tab().doc.active, 1);

        assert!({
            let tab = app.active_tab_mut();
            tab.history.redo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers[0].name, "上");
        assert_eq!(app.active_tab().doc.active, 0);
    }

    #[test]
    fn layer_merge_down_history_round_trips_via_app() {
        let mut app = new_for_test(Document::new(1, 1, Background::Transparent));
        app.active_tab_mut().doc.layers[0] =
            crate::document::Layer::filled("下", 1, 1, [255, 255, 255, 255]);
        app.layer_add();
        app.active_tab_mut().doc.layers[1] =
            crate::document::Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        app.active_tab_mut().doc.layers[1].opacity = 128;

        app.layer_merge_down();
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        let merged = app.active_tab().doc.layers[0].pixels.clone();

        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        assert_eq!(app.active_tab().doc.layers[1].opacity, 128);

        assert!({
            let tab = app.active_tab_mut();
            tab.history.redo(&mut tab.doc)
        });
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        assert_eq!(app.active_tab().doc.layers[0].pixels, merged);
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
        assert!(app.active_tab().history.has_open_stroke());
        let painted = app.active_tab().doc.get_pixel(5, 5);
        assert_ne!(painted, Some([255, 255, 255, 255]));

        app.layer_add();

        assert!(
            !app.active_tab().history.has_open_stroke(),
            "the in-progress stroke must be committed before the layer is added"
        );
        // 2 つの undo 単位が積まれているはず: ①ペンストローク ②レイヤー追加。
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        }); // レイヤー追加を取り消す
        assert_eq!(app.active_tab().doc.layers.len(), 1);
        assert!({
            let tab = app.active_tab_mut();
            tab.history.undo(&mut tab.doc)
        }); // ストロークを取り消す
        assert_eq!(
            app.active_tab().doc.get_pixel(5, 5),
            Some([255, 255, 255, 255])
        );
    }

    #[test]
    fn layer_add_commits_an_open_floating_selection_first() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        // v2 §16: ハンドルの当たり判定を避けて内部ドラッグを起こす
        // (上の `dragging_inside_selection_floats_it_and_tracks_the_pointer`
        // と同じ理由)。
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 18,
            y1: 18,
        })));
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
            app.active_tab().floating.is_some(),
            "drag must have floated the selection"
        );

        app.layer_add();

        assert!(
            app.active_tab().floating.is_none(),
            "the floating piece must be committed before the layer is added"
        );
        assert_eq!(app.active_tab().doc.layers.len(), 2);
    }

    #[test]
    fn set_active_layer_commits_open_floating_before_switching() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.layer_add();
        assert_eq!(app.active_tab().doc.active, 1);

        app.tool = ToolKind::Select;
        // v2 §16: 同上、ハンドルの当たり判定を避ける。
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 18,
            y1: 18,
        })));
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
        assert!(app.active_tab().floating.is_some());

        app.set_active_layer(0);

        assert!(
            app.active_tab().floating.is_none(),
            "switching the active layer must commit the floating piece to the previously active layer"
        );
        assert_eq!(app.active_tab().doc.active, 0);
    }

    #[test]
    fn set_active_layer_does_not_push_undo() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        // 履歴を経由せず直接レイヤーを増やし、切り替え自体が undo 単位に
        // ならないことだけを検証する(`layer_add` 自体の undo は別テスト
        // `layer_add_inserts_a_new_layer_as_a_single_undo_unit` で担保済み)。
        app.active_tab_mut()
            .doc
            .layers
            .push(crate::document::Layer::filled("上", 10, 10, [0, 0, 0, 0]));
        assert!(!app.active_tab().history.can_undo());

        app.set_active_layer(1);
        assert_eq!(app.active_tab().doc.active, 1);
        assert!(
            !app.active_tab().history.can_undo(),
            "switching the active layer must not be a history op (SPEC §13)"
        );
    }

    #[test]
    fn layers_panel_action_dispatch_wires_through_to_document() {
        let mut app = new_for_test(Document::new(6, 6, Background::White));
        app.handle_layers_panel_action(LayersPanelAction::Add);
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        app.handle_layers_panel_action(LayersPanelAction::Activate(0));
        assert_eq!(app.active_tab().doc.active, 0);
        app.handle_layers_panel_action(LayersPanelAction::MoveUp);
        assert_eq!(app.active_tab().doc.active, 1);
        app.handle_layers_panel_action(LayersPanelAction::Duplicate);
        assert_eq!(app.active_tab().doc.layers.len(), 3);
        app.handle_layers_panel_action(LayersPanelAction::MergeDown);
        assert_eq!(app.active_tab().doc.layers.len(), 2);
        app.handle_layers_panel_action(LayersPanelAction::Delete);
        assert_eq!(app.active_tab().doc.layers.len(), 1);
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

    // -- v4 §26: 設定の永続化・最近使ったファイル ---------------------------

    fn temp_dir_for_app_test(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_app_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn remember_recent_file_adds_to_front_and_dedupes_existing_entry() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.remember_recent_file(PathBuf::from("a.png"));
        app.remember_recent_file(PathBuf::from("b.png"));
        assert_eq!(
            app.recent_files,
            VecDeque::from(vec![PathBuf::from("b.png"), PathBuf::from("a.png")])
        );

        // 既存の同一パスは先頭へ移動するだけ(重複は残らない、SPEC §26)。
        app.remember_recent_file(PathBuf::from("a.png"));
        assert_eq!(
            app.recent_files,
            VecDeque::from(vec![PathBuf::from("a.png"), PathBuf::from("b.png")])
        );
    }

    #[test]
    fn remember_recent_file_caps_at_max_recent_files() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        for i in 0..(settings::MAX_RECENT_FILES + 3) {
            app.remember_recent_file(PathBuf::from(format!("{i}.png")));
        }
        assert_eq!(app.recent_files.len(), settings::MAX_RECENT_FILES);
        // 先頭は最後に追加したもの(最新)。
        assert_eq!(
            app.recent_files[0],
            PathBuf::from(format!("{}.png", settings::MAX_RECENT_FILES + 2))
        );
    }

    #[test]
    fn open_recent_file_missing_path_is_removed_and_toast_shown() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        let missing = PathBuf::from("__darask_paint_definitely_missing__.png");
        app.recent_files.push_back(missing.clone());

        app.open_recent_file(0);

        assert!(
            !app.recent_files.contains(&missing),
            "a missing recent file must be removed from the list (SPEC §26)"
        );
        assert!(
            app.toast.is_some(),
            "selecting a missing recent file must show a toast"
        );
    }

    #[test]
    fn open_recent_file_out_of_range_index_does_nothing() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_recent_file(0); // 空の一覧に対するインデックス。
        assert!(app.recent_files.is_empty());
        assert!(app.toast.is_none());
    }

    #[test]
    fn open_recent_file_existing_path_opens_it_and_moves_to_front() {
        let dir = temp_dir_for_app_test("open_recent");
        let path = dir.join("existing.png");
        let mut seed_doc = Document::new(3, 3, Background::White);
        io::save_image(&mut seed_doc, &path, SaveFormat::Png).expect("seed file should save");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.recent_files.push_back(PathBuf::from("other.png"));
        app.recent_files.push_back(path.clone());

        app.open_recent_file(1);

        assert_eq!(app.active_tab().doc.path, Some(path.clone()));
        assert_eq!(
            app.recent_files.front(),
            Some(&path),
            "opening a recent file must move it to the front (MRU)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_settings_reflects_live_app_state() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.primary = Color32::from_rgb(1, 2, 3);
        app.secondary = Color32::from_rgb(4, 5, 6);
        app.brush_size = 22.0;
        app.brush_hardness = 55;
        app.brush_opacity = 66;
        app.pencil_mode = true;
        app.brush_smoothing = 40;
        app.show_pixel_grid = false;
        app.tool = ToolKind::Gradient;
        app.user_palette.push(Color32::from_rgb(9, 9, 9));
        app.recent_files.push_back(PathBuf::from("x.png"));
        app.window_size = egui::vec2(1600.0, 900.0);
        app.window_maximized = true;
        app.max_undo_steps = 250;

        let s = app.current_settings();
        assert_eq!(s.primary, app.primary);
        assert_eq!(s.secondary, app.secondary);
        assert_eq!(s.brush_size, app.brush_size);
        assert_eq!(s.brush_hardness, app.brush_hardness);
        assert_eq!(s.brush_opacity, app.brush_opacity);
        assert_eq!(s.pencil_mode, app.pencil_mode);
        assert_eq!(s.brush_smoothing, app.brush_smoothing);
        assert_eq!(s.show_pixel_grid, app.show_pixel_grid);
        assert_eq!(s.last_tool, ToolKind::Gradient);
        assert_eq!(s.user_palette, app.user_palette);
        assert_eq!(s.recent_files, app.recent_files);
        assert_eq!(s.window_width, 1600);
        assert_eq!(s.window_height, 900);
        assert!(s.window_maximized);
        assert_eq!(s.max_undo_steps, 250);
    }

    // -- v4 レビューで発見・修正したバグ: 最大化中のウィンドウ内寸を
    // window_size として保存してしまい、復元後の「元に戻す」サイズが
    // 画面いっぱいになる -----------------------------------------------

    #[test]
    fn track_window_size_ignores_inner_rect_while_maximized() {
        let mut size = egui::vec2(1280.0, 800.0);
        let mut maximized = false;

        // 通常サイズで使用中: inner_rect がそのまま反映される。
        track_window_size(
            &mut size,
            &mut maximized,
            Some(false),
            Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
        );
        assert_eq!(size, egui::vec2(1280.0, 800.0));
        assert!(!maximized);

        // 最大化: `maximized` は更新されるが、最大化時のクライアント全体の
        // 寸法(1920x1040 のような画面いっぱいのサイズ)は `window_size` に
        // 反映してはいけない(バグ版はここで無条件に上書きしていた)。
        track_window_size(
            &mut size,
            &mut maximized,
            Some(true),
            Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 1040.0),
            )),
        );
        assert!(maximized);
        assert_eq!(
            size,
            egui::vec2(1280.0, 800.0),
            "the pre-maximize window size must be preserved while maximized"
        );

        // 最大化解除: 次に報告される(通常サイズの)inner_rect が改めて
        // 反映される。
        track_window_size(
            &mut size,
            &mut maximized,
            Some(false),
            Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
        );
        assert!(!maximized);
        assert_eq!(size, egui::vec2(1280.0, 800.0));
    }

    #[test]
    fn track_window_size_keeps_previous_values_when_viewport_info_is_unavailable() {
        let mut size = egui::vec2(640.0, 480.0);
        let mut maximized = true;

        // Android/Wayland 等で `None` が返る場合は前回値を据え置く。
        track_window_size(&mut size, &mut maximized, None, None);

        assert!(maximized);
        assert_eq!(size, egui::vec2(640.0, 480.0));
    }

    #[test]
    fn opening_a_file_adds_it_to_recent_files() {
        let dir = temp_dir_for_app_test("open_adds_recent");
        let path = dir.join("photo.png");
        let mut doc = Document::new(3, 3, Background::White);
        io::save_image(&mut doc, &path, SaveFormat::Png).expect("seed file should save");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_path_in_new_tab(path.clone());

        assert_eq!(app.recent_files.front(), Some(&path));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_file_adds_it_to_recent_files() {
        let dir = temp_dir_for_app_test("save_adds_recent");
        let path = dir.join("saved.png");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.begin_save_to_path(path.clone());

        assert_eq!(app.recent_files.front(), Some(&path));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_tool_state_clamps_out_of_range_settings_values() {
        // settings::parse は型の範囲(u8 なら 0-255)までしか検証しないため、
        // 手編集・破損した設定ファイルはドメイン範囲外の値を持ちうる
        // (ARCHITECTURE.md §16.10-5)。
        let settings = Settings {
            brush_size: 9999.0,   // MAX_BRUSH_SIZE (64) を大きく超える。
            brush_hardness: 250,  // MAX_BRUSH_HARDNESS (100) 超え。
            brush_opacity: 0,     // MIN_BRUSH_OPACITY (1) 未満。
            brush_smoothing: 200, // 100 超え。
            ..Default::default()
        };

        let startup = StartupToolState::resolve(&settings);
        assert_eq!(startup.brush_size, MAX_BRUSH_SIZE);
        assert_eq!(startup.brush_hardness, MAX_BRUSH_HARDNESS);
        assert_eq!(startup.brush_opacity, MIN_BRUSH_OPACITY);
        assert_eq!(startup.brush_smoothing, 100);
    }

    #[test]
    fn startup_tool_state_passes_through_in_range_values_unchanged() {
        let settings = Settings {
            brush_size: 22.0,
            brush_hardness: 55,
            brush_opacity: 66,
            brush_smoothing: 40,
            fill_tolerance: 12,
            rect_mode: crate::tools::shapes::ShapeMode::Both,
            ellipse_mode: crate::tools::shapes::ShapeMode::Fill,
            gradient_kind: raster::GradientKind::Radial,
            gradient_colors: crate::tools::gradient::GradientColors::PrimaryToTransparent,
            ..Default::default()
        };

        let startup = StartupToolState::resolve(&settings);
        assert_eq!(startup.brush_size, 22.0);
        assert_eq!(startup.brush_hardness, 55);
        assert_eq!(startup.brush_opacity, 66);
        assert_eq!(startup.brush_smoothing, 40);
        assert_eq!(startup.fill_tolerance, 12);
        assert_eq!(startup.rect_mode, crate::tools::shapes::ShapeMode::Both);
        assert_eq!(startup.ellipse_mode, crate::tools::shapes::ShapeMode::Fill);
        assert_eq!(startup.gradient_kind, raster::GradientKind::Radial);
        assert_eq!(
            startup.gradient_colors,
            crate::tools::gradient::GradientColors::PrimaryToTransparent
        );
    }

    #[test]
    fn startup_tool_state_last_tool_bookkeeping_for_each_cycle_group() {
        // last_tool が図形/マリキー/塗りつぶし系のいずれかなら、対応する
        // last_*_tool へそのまま引き継がれる(SPEC §20/§22/§23)。
        for (last_tool, expect_shape, expect_marquee, expect_fill) in [
            (
                ToolKind::Rect,
                ToolKind::Rect,
                ToolKind::Select,
                ToolKind::Fill,
            ),
            (
                ToolKind::Ellipse,
                ToolKind::Ellipse,
                ToolKind::Select,
                ToolKind::Fill,
            ),
            (
                ToolKind::EllipseSelect,
                ToolKind::Line,
                ToolKind::EllipseSelect,
                ToolKind::Fill,
            ),
            (
                ToolKind::Gradient,
                ToolKind::Line,
                ToolKind::Select,
                ToolKind::Gradient,
            ),
        ] {
            let settings = Settings {
                last_tool,
                ..Default::default()
            };
            let startup = StartupToolState::resolve(&settings);
            assert_eq!(
                startup.last_shape_tool, expect_shape,
                "last_tool={last_tool:?}"
            );
            assert_eq!(
                startup.last_marquee_tool, expect_marquee,
                "last_tool={last_tool:?}"
            );
            assert_eq!(
                startup.last_fill_tool, expect_fill,
                "last_tool={last_tool:?}"
            );
        }

        // last_tool がどの巡回グループにも属さない場合、各グループは
        // SPEC の表の先頭(既定値)のままになる。
        let settings = Settings {
            last_tool: ToolKind::Pan,
            ..Default::default()
        };
        let startup = StartupToolState::resolve(&settings);
        assert_eq!(startup.last_shape_tool, ToolKind::Line);
        assert_eq!(startup.last_marquee_tool, ToolKind::Select);
        assert_eq!(startup.last_fill_tool, ToolKind::Fill);
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
        app.active_tab_mut().floating = Some(Floating::new_rect(
            [255, 0, 0, 255].repeat(100), // 10x10 の不透明赤
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

        let floating = app.active_tab().floating.as_ref().expect("still floating");
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
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.handle_select_event(ToolEvent::Down {
            img: pos2(15.0, 15.0), // BottomRight ハンドル。
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        assert!(
            app.active_tab().floating.is_some(),
            "grabbing a handle on a plain selection must float it first"
        );
        assert!(app.active_tab().selection.is_none());
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
        let floating = app.active_tab().floating.as_ref().expect("still floating");
        assert_eq!(floating.pos, pos2(5.0, 5.0));
        assert_eq!((floating.w, floating.h), (20, 20));
    }

    #[test]
    fn shift_held_while_dragging_a_handle_locks_the_aspect_ratio() {
        let mut app = new_for_test(Document::new(100, 100, Background::White));
        app.tool = ToolKind::Select;
        app.active_tab_mut().floating = Some(Floating::new_rect(
            [0, 0, 0, 255].repeat(200), // 10x20
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

        let floating = app.active_tab().floating.as_ref().expect("still floating");
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
        // 表示」)は `self.active_tab().view.hover_img()` 経由でこの判定を使う。
        // `hover_img` はキャンバス上のポインタ移動(`CanvasView::show`、
        // egui::Context 必須)でしか更新できないため、ここではその下位関数
        // `hit_resize_handle`/`handle_cursor` を直接検証する。
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));
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

    // -- v3 §18: 移動ツール(ARCHITECTURE.md §15.2 受け入れ基準) -------------

    #[test]
    fn move_tool_floats_and_moves_the_whole_active_layer_when_no_selection() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Move;
        app.active_tab_mut().doc.set_pixel(2, 2, [9, 9, 9, 255]);

        app.handle_move_event(ToolEvent::Down {
            img: pos2(3.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Drag {
            img: pos2(8.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        let floating = app
            .active_tab()
            .floating
            .as_ref()
            .expect("dragging with no selection must float the whole active layer");
        assert_eq!(
            (floating.w, floating.h),
            (20, 20),
            "no selection -> whole active layer floats"
        );
        assert_eq!(
            floating.pos,
            pos2(5.0, 0.0),
            "must track the pointer delta from the down position"
        );
        // 切り出し元(全面)は浮動化と同時に透明化されている(未確定)。
        assert_eq!(app.active_tab().doc.get_pixel(2, 2), Some([0, 0, 0, 0]));
    }

    #[test]
    fn move_tool_moves_only_the_existing_selection_rect() {
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Move;
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.handle_move_event(ToolEvent::Down {
            img: pos2(20.0, 20.0), // 選択の外だが、移動ツールはクリック位置を問わない。
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Drag {
            img: pos2(25.0, 20.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });

        let floating = app
            .active_tab()
            .floating
            .as_ref()
            .expect("must float the existing selection");
        assert_eq!(
            (floating.w, floating.h),
            (10, 10),
            "must float only the selection rect, not the whole 40x40 layer"
        );
        assert_eq!(floating.pos, pos2(10.0, 5.0));
    }

    #[test]
    fn move_tool_single_click_without_drag_does_not_float_or_push_undo() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Move;

        app.handle_move_event(ToolEvent::Down {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Up {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
        });

        assert!(
            app.active_tab().floating.is_none(),
            "a plain click (no drag) must not float the layer"
        );
        assert!(
            !app.active_tab().history.can_undo(),
            "a no-op click must not push an undo entry (SPEC §18: before==after suppression)"
        );
    }

    #[test]
    fn switching_away_from_move_tool_mid_drag_commits_the_floating_piece() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Move;
        app.handle_move_event(ToolEvent::Down {
            img: pos2(3.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Drag {
            img: pos2(8.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(app.active_tab().floating.is_some());

        app.set_tool(ToolKind::Pen);

        assert!(
            app.active_tab().floating.is_none(),
            "switching tools must commit the open floating (same rule as the select tool)"
        );
        assert!(app.active_tab().history.can_undo());
    }

    #[test]
    fn layer_add_commits_an_open_floating_move_first() {
        // `layer_add_commits_an_open_floating_selection_first` の移動ツール版
        // (ARCHITECTURE.md §15.6 落とし穴1: 「自動確定は確定のまま」)。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Move;
        app.handle_move_event(ToolEvent::Down {
            img: pos2(3.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Drag {
            img: pos2(8.0, 3.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(app.active_tab().floating.is_some());

        app.layer_add();

        assert!(
            app.active_tab().floating.is_none(),
            "the floating piece must be committed before the layer is added"
        );
        assert_eq!(app.active_tab().doc.layers.len(), 2);
    }

    // -- v3 §18: Esc = キャンセル(ARCHITECTURE.md §15.2, §15.6 落とし穴1) ---

    #[test]
    fn cancel_floating_after_dragging_a_selection_restores_original_bytes_exactly() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        app.active_tab_mut().doc.set_pixel(7, 7, [10, 20, 30, 255]);
        let original = app.active_tab().doc.active_pixels().to_vec();
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 2,
            y0: 2,
            x1: 12,
            y1: 12,
        })));

        app.handle_select_event(ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(9.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(
            app.active_tab().floating.is_some(),
            "drag must float the selection"
        );
        assert_ne!(
            app.active_tab().doc.active_pixels(),
            original.as_slice(),
            "the cut_from region must already be transparent while floating"
        );

        app.cancel_floating();

        assert_eq!(
            app.active_tab().doc.active_pixels(),
            original.as_slice(),
            "Esc must byte-exactly restore the pre-float document"
        );
        assert!(app.active_tab().floating.is_none());
        assert!(app.active_tab().selection.is_none());
        assert!(
            !app.active_tab().history.can_undo(),
            "cancel must not push any undo entry"
        );
        assert!(!app.active_tab().history.has_open_stroke());
    }

    #[test]
    fn cancel_floating_after_move_tool_restores_the_whole_layer() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::Move;
        app.active_tab_mut().doc.set_pixel(3, 3, [1, 2, 3, 255]);
        let original = app.active_tab().doc.active_pixels().to_vec();

        app.handle_move_event(ToolEvent::Down {
            img: pos2(1.0, 1.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_move_event(ToolEvent::Drag {
            img: pos2(4.0, 1.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(app.active_tab().floating.is_some());

        app.cancel_floating();

        assert_eq!(app.active_tab().doc.active_pixels(), original.as_slice());
        assert!(app.active_tab().floating.is_none());
        assert!(!app.active_tab().history.can_undo());
    }

    #[test]
    fn cancel_floating_after_paste_just_discards_without_touching_the_document() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.active_tab_mut().doc.modified = true; // 白紙ではない状態を再現する。
        let original = app.active_tab().doc.active_pixels().to_vec();

        app.begin_paste_floating(3, 3, [1, 2, 3, 255].repeat(9));
        assert!(app.active_tab().floating.is_some());
        assert_eq!(app.tool, ToolKind::Select);

        app.cancel_floating();

        assert_eq!(
            app.active_tab().doc.active_pixels(),
            original.as_slice(),
            "a pasted floating never touched the document before commit, so cancel leaves it untouched"
        );
        assert!(app.active_tab().floating.is_none());
        assert!(!app.active_tab().history.can_undo());
        assert!(!app.active_tab().history.has_open_stroke());
    }

    // -- v3 §18: 自由変形(Ctrl+T、ARCHITECTURE.md §15.2) ---------------------

    #[test]
    fn free_transform_floats_the_existing_selection_and_preserves_its_rect() {
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Pen; // 直前のツールが何であっても働くことを示す。
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.free_transform();

        assert_eq!(app.tool, ToolKind::Select);
        let floating = app
            .active_tab()
            .floating
            .as_ref()
            .expect("Ctrl+T must float the existing selection");
        assert_eq!((floating.w, floating.h), (10, 10));
        assert_eq!(floating.pos, pos2(5.0, 5.0));
    }

    #[test]
    fn free_transform_from_select_tool_with_a_plain_selection_does_not_lose_it() {
        // 回帰テスト: 進行中ジェスチャの確定に無条件で `commit_selection`
        // (常に `self.active_tab().selection` をクリアする)を使うと、選択ツールで
        // 「まだ浮動化していない」選択を持っている状態で Ctrl+T を押したとき
        // にその選択自体が消えてしまい、変形対象がキャンバス全体に化けて
        // しまうバグになる(`free_transform` 実装時に発見・回避)。
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.free_transform();

        let floating = app.active_tab().floating.as_ref().expect("must float");
        assert_eq!(
            (floating.w, floating.h),
            (10, 10),
            "the plain selection must survive and be the transform target, not the whole 40x40 layer"
        );
    }

    // -- v4 §23/§24: 回帰テスト(commit_open_gesture が選択を残すこと) --------

    #[test]
    fn switching_tools_away_from_select_preserves_a_plain_selection() {
        // 回帰テスト: `commit_open_gesture`(`set_tool` が呼ぶ)が無条件で
        // `commit_selection`(常に `self.active_tab().selection` をクリアする)を使うと、
        // 「M で選択してから G/Shift+G でグラデーションに切り替える」という
        // SPEC §21/§23 が前提とする使い方で、ツール切替の瞬間に選択が消えて
        // しまいクリップ対象が無くなるバグになる(`free_transform` が Ctrl+T
        // について既に回避していたのと同一クラス、上のテスト参照)。
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Select;
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.set_tool(ToolKind::Gradient);

        assert!(
            app.active_tab().selection.is_some(),
            "a plain (non-floating) selection must survive a plain tool switch"
        );
        assert!(app.active_tab().floating.is_none());
    }

    #[test]
    fn switching_tools_away_from_select_still_commits_an_in_progress_floating() {
        // 浮動化済みの浮動片(=まさに動かしている最中)は、従来どおりツール
        // 切替で確定合成されなければならない(`flush_floating_keep_selection`
        // が `commit_selection` の浮動片確定ロジックをそのまま引き継いで
        // いることの確認)。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Select;
        app.begin_floating_from_selection(
            select::rect_mask(IRect {
                x0: 2,
                y0: 2,
                x1: 8,
                y1: 8,
            }),
            pos2(2.0, 2.0),
        );
        assert!(app.active_tab().floating.is_some());
        // 実際に動かしていないと、切り出し元へそのまま同じ画素を貼り戻すだけ
        // になり before==after 抑制(ARCHITECTURE.md §15.2)で undo 単位が
        // 積まれない。「移動した」ことにするため位置をずらす。
        app.active_tab_mut().floating.as_mut().unwrap().pos = pos2(5.0, 5.0);

        app.set_tool(ToolKind::Pen);

        assert!(
            app.active_tab().floating.is_none(),
            "the floating piece must be committed"
        );
        assert!(app.active_tab().history.can_undo());
    }

    #[test]
    fn free_transform_without_a_selection_floats_the_whole_active_layer() {
        let mut app = new_for_test(Document::new(12, 8, Background::White));
        app.tool = ToolKind::Pen;

        app.free_transform();

        let floating = app
            .active_tab()
            .floating
            .as_ref()
            .expect("Ctrl+T must float the whole layer when there is no selection");
        assert_eq!((floating.w, floating.h), (12, 8));
    }

    #[test]
    fn free_transform_can_be_cancelled_with_esc_restoring_the_original_document() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.active_tab_mut().doc.set_pixel(4, 4, [5, 6, 7, 255]);
        let original = app.active_tab().doc.active_pixels().to_vec();

        app.free_transform();
        assert!(app.active_tab().floating.is_some());

        app.cancel_floating();

        assert_eq!(app.active_tab().doc.active_pixels(), original.as_slice());
        assert!(app.active_tab().floating.is_none());
        assert!(!app.active_tab().history.can_undo());
    }

    // -- v3 §18: ズームツール(ARCHITECTURE.md §15.2) -------------------------

    #[test]
    fn zoom_tool_click_zooms_in_around_the_click_point() {
        let mut app = new_for_test(Document::new(100, 100, Background::White));
        app.tool = ToolKind::Zoom;
        let before_zoom = app.active_tab().view.zoom;

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(30.0, 40.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);

        assert!(
            app.active_tab().view.zoom > before_zoom,
            "a plain click must zoom in"
        );
    }

    #[test]
    fn zoom_tool_alt_click_zooms_out_instead_of_sampling_a_color() {
        let mut app = new_for_test(Document::new(100, 100, Background::White));
        app.tool = ToolKind::Zoom;
        app.active_tab_mut().view.zoom = 2.0; // まず拡大しておき、縮小できることを確認する。
        let before_zoom = app.active_tab().view.zoom;
        let before_primary = app.primary;

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(30.0, 40.0),
            button: PointerButton::Primary,
            mods: Modifiers::ALT,
        }]);

        assert!(
            app.active_tab().view.zoom < before_zoom,
            "Alt+click must zoom out"
        );
        assert_eq!(
            app.primary, before_primary,
            "Alt+click on the zoom tool must not trigger the temporary eyedropper (SPEC §18 overrides SPEC §4 here)"
        );
    }

    // -- v3 §19: テキストツール(ARCHITECTURE.md §15.3) ----------------------

    /// 開発機(Windows)のシステム日本語フォントを読み込む。無ければテストを
    /// スキップする(`text.rs` のテストと同じ方針)。
    fn test_font() -> Option<Arc<Vec<u8>>> {
        text::load_font_bytes().map(Arc::new)
    }

    #[test]
    fn begin_text_edit_without_font_shows_toast_and_does_not_start_editing() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Text;
        assert!(app.text_font.is_none());

        app.begin_text_edit(pos2(3.0, 4.0));

        assert!(
            app.text_edit.is_none(),
            "without a loaded font, editing must not start at all"
        );
        assert!(app.toast.is_some(), "must toast why it refused to start");
    }

    #[test]
    fn begin_text_edit_with_font_starts_editing_at_click_position() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);

        app.begin_text_edit(pos2(3.0, 4.0));

        let state = app.text_edit.as_ref().expect("editing must start");
        assert_eq!(state.pos, pos2(3.0, 4.0));
        assert!(state.buffer.is_empty());
        assert!(state.needs_focus);
    }

    #[test]
    fn discard_pending_text_edit_clears_state_without_touching_history() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(0.0, 0.0));
        app.text_edit.as_mut().unwrap().buffer = "hello".to_owned();

        app.discard_pending_text_edit();

        assert!(app.text_edit.is_none());
        assert!(
            !app.active_tab().history.can_undo(),
            "SPEC §19: Esc discards without pushing any history"
        );
        assert!(app.active_tab().floating.is_none());
    }

    #[test]
    fn commit_pending_text_edit_with_empty_buffer_does_nothing() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(0.0, 0.0));
        // buffer は空文字列のまま。

        app.commit_pending_text_edit();

        assert!(
            app.text_edit.is_none(),
            "the pending edit is consumed either way"
        );
        assert!(
            app.active_tab().floating.is_none(),
            "SPEC §19: an empty-string commit must do nothing"
        );
        assert!(!app.active_tab().history.can_undo());
        assert_eq!(
            app.tool,
            ToolKind::Text,
            "an empty commit must not switch tools"
        );
    }

    #[test]
    fn commit_pending_text_edit_creates_a_floating_and_switches_to_select() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.primary = Color32::from_rgb(200, 10, 10);
        app.begin_text_edit(pos2(5.0, 6.0));
        app.text_edit.as_mut().unwrap().buffer = "A".to_owned();

        app.commit_pending_text_edit();

        assert!(app.text_edit.is_none());
        let floating = app
            .active_tab()
            .floating
            .as_ref()
            .expect("non-empty text must float");
        assert_eq!(
            floating.pos,
            pos2(5.0, 6.0),
            "SPEC §19: click position is the box's top-left"
        );
        assert_eq!(
            app.tool,
            ToolKind::Select,
            "committed text reuses the selection tool's floating machinery"
        );
        assert!(
            app.active_tab().history.has_open_stroke(),
            "not yet finalized until the floating itself is confirmed (Enter/outside click/Esc)"
        );
        assert_eq!(
            app.recent_colors.front().copied(),
            Some(app.primary),
            "SPEC §5: committing text records the color used"
        );
    }

    #[test]
    fn commit_pending_text_edit_and_composite_writes_directly_without_leaving_a_floating() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(2.0, 2.0));
        app.text_edit.as_mut().unwrap().buffer = "A".to_owned();

        app.commit_pending_text_edit_and_composite();

        assert!(app.text_edit.is_none());
        assert!(
            app.active_tab().floating.is_none(),
            "a tool-switch interruption composites directly, no adjustable floating left behind"
        );
        assert_eq!(
            app.tool,
            ToolKind::Text,
            "this helper must never touch self.tool (called from inside set_tool's own commit step)"
        );
        assert!(
            app.active_tab().history.can_undo(),
            "must be exactly one finished undo unit"
        );
        assert!(!app.active_tab().history.has_open_stroke());
    }

    #[test]
    fn switching_tool_away_from_text_mid_edit_commits_it() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(2.0, 2.0));
        app.text_edit.as_mut().unwrap().buffer = "A".to_owned();

        app.set_tool(ToolKind::Pen);

        assert_eq!(
            app.tool,
            ToolKind::Pen,
            "set_tool must end up on the tool that was actually requested, not get clobbered \
             by the text-commit's own tool switching (the reentrancy pitfall documented on \
             `place_new_floating`/`commit_pending_text_edit_and_composite`)"
        );
        assert!(app.text_edit.is_none());
        assert!(app.active_tab().floating.is_none());
        assert!(app.active_tab().history.can_undo());
    }

    #[test]
    fn dispatch_canvas_events_text_tool_begins_edit_and_ignores_further_clicks_while_editing() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(4.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        let state = app.text_edit.as_ref().expect("click must start editing");
        assert_eq!(state.pos, pos2(4.0, 5.0));

        // A second click while already editing must not restart editing at a
        // new position; the box-outside-click confirm path lives in
        // `draw_text_edit_overlay`'s `lost_focus()` check, not here (double
        // firing both would commit *and* immediately reopen a new box).
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(20.0, 20.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert_eq!(
            app.text_edit.as_ref().unwrap().pos,
            pos2(4.0, 5.0),
            "a click while already editing must be ignored here, not start a new box"
        );
    }

    // -- v3 レビューで発見・修正したバグ: `confirm_new`(新規作成)が
    // `pen`/`eraser` の `BrushEngine::last_end`(Shift+クリック連結の終点)
    // をリセットしていなかった(`reset_tool_state_for_new_document` 参照)。
    // ----------------------------------------------------------------

    #[test]
    fn confirm_new_resets_stale_shift_click_endpoint_from_the_previous_document() {
        let mut app = new_for_test(Document::new(40, 10, Background::Transparent));
        app.tool = ToolKind::Pen;

        // 旧ドキュメントで (5,5) に単クリック(ドット)を打ち、
        // last_end を (5,5) にする。
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(5.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(5.0, 5.0),
                button: PointerButton::Primary,
            },
        ]);

        // 新規作成(SPEC §7: Ctrl+N のダイアログ確定に相当。v5 §30 では
        // 新規タブを追加する方式になった — `active_tab()` は以後この
        // 新しいタブを指す)。
        app.confirm_new(40, 10, Background::Transparent, false);

        // 新ドキュメントで最初の Shift+クリックを (35,5) に打つ。
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(35.0, 5.0),
                button: PointerButton::Primary,
                mods: Modifiers::SHIFT,
            },
            ToolEvent::Up {
                img: pos2(35.0, 5.0),
                button: PointerButton::Primary,
            },
        ]);

        assert_eq!(
            app.active_tab().doc.get_pixel(20, 5).unwrap()[3],
            0,
            "confirm_new must reset last_end; shift+click in the new document must not draw \
             a line back to the stale endpoint from the document that was just replaced"
        );
        assert_ne!(
            app.active_tab().doc.get_pixel(35, 5).unwrap()[3],
            0,
            "the shift+click point itself must still be painted as a dot"
        );
    }

    // -- v3 レビューで発見・修正したバグ: 進行中のテキスト編集を確定も
    // 破棄もせず `doc.modified` だけを見ていたため、D&D やウィンドウを
    // 閉じる操作が未保存ガードをすり抜けて編集中の内容を失っていた。
    //
    // v5 §30 でこのバグの根本原因(`request_action` の「先に確定してから
    // 判定する」規則)は Ctrl+N/Ctrl+O には当てはまらなくなった —
    // 新規タブの追加は既存タブの内容を一切破壊しないため、そもそも
    // 未保存ガード自体が不要になった(`begin_new_tab`/`begin_open_tab`/
    // `open_path_in_new_tab` のドキュメントコメント参照)。ただし「進行中の
    // ジェスチャを先に確定する」という安全側の性質そのものは
    // タブ切替の安全規則(ARCHITECTURE.md §17.3)として引き続き必須なので、
    // 以下はその性質を新しい関数群に対して検証する。
    // ------------------------------------------------------------------

    #[test]
    fn begin_open_tab_commits_pending_text_edit_before_opening_a_new_tab() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let dir = temp_dir_for_app_test("open_commits_text_edit");
        let path = dir.join("photo.png");
        io::save_image(
            &mut Document::new(3, 3, Background::White),
            &path,
            SaveFormat::Png,
        )
        .expect("seed file should save");

        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(5.0, 5.0));
        app.text_edit.as_mut().unwrap().buffer = "A".to_owned();
        assert!(
            !app.active_tab().doc.modified,
            "typing alone must not mark the doc modified yet (sanity check on the bug's \
             precondition)"
        );

        // D&D・「開く」ダイアログ・最近使ったファイルはすべて
        // `open_path_in_new_tab` を通る(ここで直接駆動する)。
        app.open_path_in_new_tab(path.clone());

        assert!(
            app.text_edit.is_none(),
            "the pending text edit on the OLD tab must have been committed before switching \
             away from it, not left dangling"
        );
        assert_eq!(
            app.tabs.len(),
            2,
            "v5 §30: opening a file must add a new tab, not replace the old one"
        );
        assert!(
            app.tabs[0].doc.modified,
            "the original tab's committed text edit must remain intact on the OLD tab, not \
             be discarded (opening a file no longer destroys existing tabs)"
        );
        assert_eq!(
            app.active_tab, 1,
            "the newly opened file must become the active tab"
        );
        assert_eq!(app.active_tab().doc.path, Some(path));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn begin_new_tab_commits_pending_text_edit_and_shows_the_new_dialog_immediately() {
        // v5 §30: Ctrl+N はもはや未保存ガードの対象ではない(新規タブの
        // 追加は既存タブを破壊しない)ので、進行中の(空の)テキスト編集を
        // 確定した直後、確認モーダルを挟まずに「新規」ダイアログが
        // 即座に表示されるはずである。
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(5.0, 5.0));
        // buffer は空のまま。

        app.begin_new_tab();

        assert!(app.text_edit.is_none());
        assert!(
            matches!(app.modal, Some(ModalState::New { .. })),
            "Ctrl+N no longer needs an unsaved-changes guard; the New dialog must show \
             immediately"
        );
        assert_eq!(
            app.tabs.len(),
            1,
            "the dialog hasn't been confirmed yet, so no tab has been added"
        );
    }

    // -- v5 §30/§32(ARCHITECTURE.md §17.7 V5-M2): タブ切替・タブを閉じる・
    // 重複オープン検出・タブ数上限・「無題」番号付け ----------------------

    #[test]
    fn next_tab_and_prev_tab_cycle_through_all_tabs_and_wrap_at_the_ends() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        app.open_new_tab(Document::new(6, 6, Background::White));
        assert_eq!(app.active_tab, 2);

        app.next_tab();
        assert_eq!(app.active_tab, 0, "SPEC §30: 端では反対側へ循環する");
        app.next_tab();
        assert_eq!(app.active_tab, 1);

        app.prev_tab();
        assert_eq!(app.active_tab, 0);
        app.prev_tab();
        assert_eq!(app.active_tab, 2, "SPEC §30: 前のタブも端で循環する");
    }

    #[test]
    fn switch_tab_commits_an_open_pen_stroke_on_the_tab_being_left() {
        // ARCHITECTURE.md §17.3/§17.8-1: 「タブ切替前に必ず
        // commit_open_gesture() を呼ぶ」の直接的な回帰テスト。
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.open_new_tab(Document::new(20, 20, Background::White));
        assert_eq!(app.active_tab, 1);

        app.switch_tab(0);
        assert_eq!(app.active_tab, 0);
        app.tool = ToolKind::Pen;
        // Down のみ(Up を送らない) = ストロークが進行中のまま。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(
            app.tabs[0].history.has_open_stroke(),
            "sanity check: a stroke must be open before switching"
        );

        app.switch_tab(1);

        assert_eq!(app.active_tab, 1);
        assert!(
            !app.tabs[0].history.has_open_stroke(),
            "switching tabs must commit the open stroke on the tab being left, not abandon it"
        );
        assert!(
            app.tabs[0].history.can_undo(),
            "the committed stroke must be a real undo unit"
        );
        assert_ne!(
            app.tabs[0].doc.get_pixel(5, 5).unwrap()[3],
            0,
            "the pixel painted before switching must have been kept"
        );
    }

    #[test]
    fn per_tab_layer_rename_state_cannot_corrupt_another_tabs_layer_name() {
        // 回帰テスト(バグ修正): `layer_rename` は以前 `DaraskApp` 直下の
        // 共有フィールドだった。タブ A でレイヤー名編集を開始したまま
        // 別のタブ B がアクティブになると、B のレイヤーパネル描画がタブ A
        // の未確定の編集状態を(B 自身のレイヤー構成と無関係に)引き継いで
        // しまい、確定すると B の無関係なレイヤーの名前が A での入力内容で
        // 上書きされていた(クロスタブ破損)。`Tab::layer_rename` として
        // タブごとに独立させたことで、この漏洩は構造的に起こり得なくなった:
        // `active_tab` がどう変わろうと、各タブは自分自身の rename 状態
        // しか持たない。ここでは `switch_tab`(=先に確定してしまう安全な
        // 経路)を経由せず直接 `active_tab` を切り替え、万一どこかで安全
        // フックの呼び出しが漏れても構造的に破損が起きないことを確認する。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.layer_add(); // タブ A(index 0)を2枚レイヤーにする。
        app.open_new_tab(Document::new(4, 4, Background::White));
        app.layer_add(); // タブ B(index 1)も2枚レイヤーにする(同じ index で衝突させる)。
        let original_tab_b_layer1_name = app.tabs[1].doc.layers[1].name.clone();

        // タブ A でレイヤー1(index 1)の名前編集を「開始」した状態
        // (まだ確定していない)を直接作る。
        app.tabs[0].layer_rename = Some((1, "タブAで入力中".to_owned(), false));
        app.active_tab = 1;

        // タブ B は自分自身の(未編集の)rename 状態を持つため、タブ A の
        // 編集中テキストを一切引き継がない。
        assert!(
            app.tabs[1].layer_rename.is_none(),
            "each tab must have its own independent rename state, not a shared one"
        );
        assert_eq!(
            app.tabs[1].doc.layers[1].name, original_tab_b_layer1_name,
            "tab B's layer name must be unaffected by tab A's in-progress edit"
        );

        // タブ A 自身の未確定の編集は無傷のまま残っている。
        assert_eq!(
            app.tabs[0].layer_rename,
            Some((1, "タブAで入力中".to_owned(), false))
        );
    }

    #[test]
    fn switch_tab_out_of_range_or_to_the_current_tab_is_a_no_op() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert_eq!(app.active_tab, 1);

        app.switch_tab(1); // 既にアクティブなタブ。
        assert_eq!(app.active_tab, 1);

        app.switch_tab(99); // 範囲外。
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn close_tab_removes_a_background_tab_and_shifts_the_active_index_down() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        app.open_new_tab(Document::new(6, 6, Background::White));
        assert_eq!(app.active_tab, 2);

        app.close_tab(0); // アクティブより前のタブを閉じる。

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.active_tab, 1,
            "closing a tab before the active one must shift the active index down by one"
        );
        assert_eq!(app.active_tab().doc.width, 6, "still the same logical tab");
    }

    #[test]
    fn close_tab_the_active_tab_activates_the_tab_that_slides_into_its_place() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        app.open_new_tab(Document::new(6, 6, Background::White));
        app.switch_tab(1);
        assert_eq!(app.active_tab, 1);

        app.close_tab(1); // アクティブ自身を閉じる。

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.active_tab, 1,
            "the tab that slid into the closed slot must become active"
        );
        assert_eq!(app.active_tab().doc.width, 6);
    }

    #[test]
    fn close_tab_the_last_tab_in_the_vec_activates_the_new_last_tab() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert_eq!(app.active_tab, 1);

        app.close_tab(1); // 末尾かつアクティブなタブを閉じる。

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn close_tab_on_a_single_unmodified_tab_resets_it_to_blank_immediately() {
        // SPEC §30: 「常に 1 タブ以上を維持する…最後の 1 タブを閉じようと
        // した場合…「新規」と同じ扱い」。未変更なら未保存ガードは発動せず、
        // 「新規」ダイアログが即座に出る(`request_action` の従来どおりの
        // 挙動)。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        assert!(!app.active_tab().doc.modified);

        app.close_tab(0);

        assert_eq!(app.tabs.len(), 1, "SPEC §30: タブが 0 枚になってはいけない");
        assert!(
            matches!(
                app.modal,
                Some(ModalState::New {
                    replace_active: true,
                    ..
                })
            ),
            "closing the last tab must show the New dialog in in-place-replace mode"
        );
    }

    #[test]
    fn close_tab_on_a_single_modified_tab_asks_for_confirmation_first() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.active_tab_mut().doc.modified = true;

        app.close_tab(0);

        assert_eq!(app.tabs.len(), 1);
        assert!(
            matches!(app.modal, Some(ModalState::ConfirmUnsaved)),
            "SPEC §30: 「未保存ガードを通してから内容を白紙に戻す」"
        );

        // 破棄を選ぶと、続けて「新規」ダイアログ(置き換えモード)が出る。
        app.confirm_unsaved_discard();
        assert!(matches!(
            app.modal,
            Some(ModalState::New {
                replace_active: true,
                ..
            })
        ));
    }

    #[test]
    fn closing_the_last_tab_commits_a_pending_layer_rename_and_asks_to_save() {
        // 回帰テスト(バグ修正): 以前は `reset_active_tab_document` が単に
        // `layer_rename = None` で編集中の入力を破棄するだけで、
        // `text_edit` に対して行っている「先に確定してから実行」が無かった。
        // さらに確定処理自体が `doc.modified` を立てていなかったため、
        // レイヤー名を編集しただけの(他は何も変更していない)ドキュメント
        // でも `request_action` の未保存ガードが発動せず、確認なしにいきなり
        // 「新規」ダイアログへ進んで入力内容がどこにも残らず消えていた。
        // `commit_open_gesture` が編集中のレイヤー名を先に確定するように
        // なったことで、この場合も「未保存の変更」として正しく検知される。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        assert!(!app.active_tab().doc.modified);
        app.active_tab_mut().layer_rename = Some((0, "新しい名前".to_owned(), false));

        app.close_tab(0);

        assert_eq!(app.tabs.len(), 1, "SPEC §30: タブが 0 枚になってはいけない");
        assert!(
            matches!(app.modal, Some(ModalState::ConfirmUnsaved)),
            "an in-progress layer rename must count as an unsaved change, not be silently \
             discarded without a chance to save"
        );
        assert_eq!(
            app.active_tab().doc.layers[0].name,
            "新しい名前",
            "the typed name must have been committed, not discarded"
        );
        assert!(app.active_tab().layer_rename.is_none());
    }

    #[test]
    fn closing_the_last_tab_and_recreating_it_advances_the_untitled_number() {
        // 回帰テスト(バグ修正): `reset_active_tab_document` は以前
        // `Tab::untitled_number` を採番し直さなかったため、「無題3」を
        // 最後の1タブとして閉じて新規化しても、タブラベルが「無題3」の
        // まま(`next_untitled_number` が既に 4 に進んでいても更新されない)
        // 残っていた。通常の Ctrl+N(`open_new_tab`)が必ず新しい番号を
        // 払い出すのと非対称だった。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(4, 4, Background::White));
        assert_eq!(app.tabs[2].label(), "無題3");

        // タブ0・タブ1を閉じ、「無題3」だけが残る唯一のタブにする
        // (いずれも未変更なので確認モーダルは出ない)。
        app.close_tab(0);
        app.close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab().label(), "無題3");

        // 唯一のタブを閉じようとする(SPEC §30: 「新規」と同じ扱い)。
        app.close_tab(0);
        assert!(matches!(
            app.modal,
            Some(ModalState::New {
                replace_active: true,
                ..
            })
        ));
        app.confirm_new(4, 4, Background::White, true);

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.active_tab().label(),
            "無題4",
            "recreating the last tab as blank must mint a fresh untitled number, just \
             like a normal Ctrl+N tab does, instead of keeping the stale old label"
        );
    }

    // ===================================================================
    // V5-M3(SPEC §30/§17.4): 未保存ガードの一般化・タブを閉じる
    // ===================================================================

    #[test]
    fn close_tab_on_an_unmodified_background_tab_closes_immediately_without_a_modal() {
        // 2枚以上あるうちの1枚が未変更なら、確認モーダルを出さず即座に閉じる
        // (v1〜v4 と同じ「変更が無ければガードは発動しない」規則の延長)。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert!(!app.active_tab().doc.modified);

        app.close_tab(0);

        assert_eq!(app.tabs.len(), 1);
        assert!(
            app.modal.is_none(),
            "an unmodified tab must not trigger the unsaved-changes guard"
        );
    }

    #[test]
    fn close_tab_on_a_modified_background_tab_activates_it_before_confirming() {
        // v5 §17.4: 「そのタブの doc.modified が true なら該当タブを
        // アクティブ化した上で…ConfirmUnsaved モーダルを出す」。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tabs[0].doc.modified = true;
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert_eq!(
            app.active_tab, 1,
            "tab 1 is active, tab 0 is in the background"
        );

        app.close_tab(0);

        // まだ何も削除されていない(モーダルの結果を待つ)。
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.active_tab, 0,
            "the tab being closed must become active so its unsaved content is visible"
        );
        assert!(matches!(app.modal, Some(ModalState::ConfirmUnsaved)));

        // 破棄を選ぶと、そのタブだけが実際に取り除かれる(タブは0枚に
        // ならず、常に1枚以上を維持するルールとは別の経路)。
        app.confirm_unsaved_discard();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.active_tab().doc.width,
            5,
            "the remaining tab must be the one that was never closed"
        );
    }

    #[test]
    fn close_tab_on_a_modified_background_tab_cancel_leaves_both_tabs_untouched() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tabs[0].doc.modified = true;
        app.open_new_tab(Document::new(5, 5, Background::White));

        app.close_tab(0);
        assert!(matches!(app.modal, Some(ModalState::ConfirmUnsaved)));

        app.confirm_unsaved_cancel();

        assert_eq!(app.tabs.len(), 2, "cancelling must not close anything");
        assert!(app.tabs[0].doc.modified, "the unsaved change must survive");
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn ctrl_w_closes_the_active_unmodified_tab_via_shortcut() {
        // v5 §30/§32: Ctrl+W(`Action::CloseTab`、`keymap.rs` 参照)。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert_eq!(app.tabs.len(), 2);

        let ctx = ctx_with_key_event(Key::W, Modifiers::CTRL);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tabs.len(), 1, "Ctrl+W must close the active tab");
        assert_eq!(
            app.active_tab().doc.width,
            4,
            "the background tab must survive"
        );
    }

    #[test]
    fn begin_quit_walks_every_modified_tab_in_order_and_skips_unmodified_ones() {
        // v5 §17.4: 「未保存のタブがあればタブごとに順番に確認ダイアログを
        // 出す」。3枚のうち先頭と末尾だけが未保存。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tabs[0].doc.modified = true;
        app.open_new_tab(Document::new(5, 5, Background::White)); // index 1, unmodified
        app.open_new_tab(Document::new(6, 6, Background::White)); // index 2
        app.tabs[2].doc.modified = true;

        app.begin_quit();

        assert_eq!(
            app.active_tab, 0,
            "must confirm the first modified tab (index 0) first, skipping the unmodified one"
        );
        assert!(matches!(app.modal, Some(ModalState::ConfirmUnsaved)));

        // 破棄すると、次に未保存のタブ(index 2、1 は未変更なので飛ばす)へ進む。
        app.confirm_unsaved_discard();
        assert_eq!(app.active_tab, 2);
        assert!(matches!(app.modal, Some(ModalState::ConfirmUnsaved)));
    }

    #[test]
    fn begin_quit_cancel_mid_queue_aborts_the_whole_close_all_tabs_flow() {
        // ARCHITECTURE.md §17.4: 「途中でキャンセルされたら全体を中止」。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tabs[0].doc.modified = true;
        app.open_new_tab(Document::new(5, 5, Background::White));
        app.tabs[1].doc.modified = true;

        app.begin_quit();
        assert_eq!(app.active_tab, 0);
        assert!(matches!(app.modal, Some(ModalState::ConfirmUnsaved)));

        app.confirm_unsaved_cancel();

        assert!(
            app.pending_action.is_none(),
            "cancelling must drop the rest of the queue, not just this one tab"
        );
        assert_eq!(app.tabs.len(), 2, "no tab may be closed by an aborted quit");
        assert!(app.tabs[0].doc.modified);
        assert!(app.tabs[1].doc.modified);
    }

    #[test]
    fn handle_close_request_checks_every_tab_not_just_the_active_one() {
        // v5 §17.4: v1〜v4 は単一ドキュメントだったので活性タブだけ見れば
        // 足りたが、v5 では非アクティブな未保存タブも見なければならない。
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.tabs[0].doc.modified = true;
        app.open_new_tab(Document::new(5, 5, Background::White));
        assert_eq!(app.active_tab, 1);
        assert!(!app.active_tab().doc.modified);

        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        });

        app.handle_close_request(&ctx);
        let _ = ctx.end_pass();

        assert!(
            matches!(app.modal, Some(ModalState::ConfirmUnsaved)),
            "a background tab's unsaved change must still arm the unsaved-changes guard"
        );
        assert_eq!(
            app.active_tab, 0,
            "must have switched to the modified tab to show its confirmation"
        );
    }

    #[test]
    fn open_path_in_new_tab_switches_to_an_already_open_tab_instead_of_duplicating() {
        let dir = temp_dir_for_app_test("dedupe_open");
        let path = dir.join("shared.png");
        io::save_image(
            &mut Document::new(3, 3, Background::White),
            &path,
            SaveFormat::Png,
        )
        .expect("seed file should save");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_path_in_new_tab(path.clone());
        assert_eq!(app.tabs.len(), 2);
        app.switch_tab(0);
        assert_eq!(app.active_tab, 0);

        // SPEC §30: 「開こうとしたファイルが既に開いているタブがあれば
        // (パスを正規化して比較)、新規タブを作らずそのタブへ切り替える」。
        app.open_path_in_new_tab(path.clone());

        assert_eq!(
            app.tabs.len(),
            2,
            "opening an already-open file must not create a duplicate tab"
        );
        assert_eq!(app.active_tab, 1, "must switch to the existing tab");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_path_in_new_tab_refuses_past_the_tab_limit_and_shows_a_toast() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        for _ in 0..(MAX_TABS - 1) {
            app.open_new_tab(Document::new(4, 4, Background::White));
        }
        assert_eq!(app.tabs.len(), MAX_TABS);

        let dir = temp_dir_for_app_test("tab_limit");
        let path = dir.join("one_too_many.png");
        io::save_image(
            &mut Document::new(3, 3, Background::White),
            &path,
            SaveFormat::Png,
        )
        .expect("seed file should save");

        app.open_path_in_new_tab(path);

        assert_eq!(
            app.tabs.len(),
            MAX_TABS,
            "must refuse to exceed MAX_TABS (SPEC §30: 上限 24)"
        );
        assert!(app.toast.is_some(), "must show a toast when refusing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn begin_new_tab_refuses_past_the_tab_limit_and_shows_a_toast() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        for _ in 0..(MAX_TABS - 1) {
            app.open_new_tab(Document::new(4, 4, Background::White));
        }
        assert_eq!(app.tabs.len(), MAX_TABS);

        app.begin_new_tab();

        assert_eq!(app.tabs.len(), MAX_TABS);
        assert!(app.modal.is_none(), "must not even show the New dialog");
        assert!(app.toast.is_some());
    }

    #[test]
    fn tab_label_numbers_untitled_tabs_sequentially_and_never_reuses_a_number() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        assert_eq!(
            app.tabs[0].label(),
            "無題",
            "the very first tab has no suffix"
        );

        app.open_new_tab(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(4, 4, Background::White));
        assert_eq!(app.tabs[1].label(), "無題2");
        assert_eq!(app.tabs[2].label(), "無題3");

        // 途中のタブを閉じても、残っているタブの番号は採番し直さない。
        app.close_tab(1);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].label(), "無題");
        assert_eq!(
            app.tabs[1].label(),
            "無題3",
            "closing another tab must not renumber the remaining ones"
        );

        // 新しく開くタブは、既に使われた番号を飛ばしてさらに先へ進む。
        app.open_new_tab(Document::new(4, 4, Background::White));
        assert_eq!(app.tabs[2].label(), "無題4");
    }

    #[test]
    fn tab_label_uses_the_file_name_once_a_path_is_set() {
        let dir = temp_dir_for_app_test("tab_label_path");
        let path = dir.join("photo.png");
        io::save_image(
            &mut Document::new(3, 3, Background::White),
            &path,
            SaveFormat::Png,
        )
        .expect("seed file should save");

        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_path_in_new_tab(path);

        assert_eq!(app.tabs[1].label(), "photo.png");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- v5 §31(ARCHITECTURE.md §17.5): 選択範囲を新規タブに複製 -------------

    #[test]
    fn duplicate_selection_to_new_tab_is_a_no_op_without_a_selection_or_floating() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.duplicate_selection_to_new_tab();
        assert_eq!(
            app.tabs.len(),
            1,
            "no selection/floating means nothing to duplicate"
        );
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn duplicate_selection_to_new_tab_with_a_static_selection_preserves_layer_structure_and_leaves_the_source_untouched(
    ) {
        let mut app = new_for_test(Document::new(6, 6, Background::White));
        app.active_tab_mut().doc.layers[0] =
            crate::document::Layer::filled("下", 6, 6, [255, 255, 255, 255]);
        app.layer_add();
        app.active_tab_mut().doc.layers[1] =
            crate::document::Layer::filled("上", 6, 6, [10, 20, 30, 200]);
        app.active_tab_mut().doc.layers[1].visible = false;
        app.active_tab_mut().doc.layers[1].opacity = 128;
        assert_eq!(
            app.active_tab().doc.active,
            1,
            "layer_add activates the new layer"
        );

        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 1,
            y0: 1,
            x1: 4,
            y1: 4,
        })));

        let before_layers_0 = app.active_tab().doc.layers[0].pixels.clone();
        let before_layers_1 = app.active_tab().doc.layers[1].pixels.clone();

        app.duplicate_selection_to_new_tab();

        assert_eq!(app.tabs.len(), 2, "must insert exactly one new tab");
        assert_eq!(app.active_tab, 1, "the new tab must become active");

        // 元のタブは一切変更されていない(SPEC §31: 「元のタブは一切変更
        // しない」)。
        assert!(
            app.tabs[0].selection.is_some(),
            "the source tab keeps its selection"
        );
        assert_eq!(app.tabs[0].doc.layers[0].pixels, before_layers_0);
        assert_eq!(app.tabs[0].doc.layers[1].pixels, before_layers_1);
        assert_eq!(app.tabs[0].doc.active, 1);
        assert_eq!(app.tabs[0].doc.layers[1].opacity, 128);
        assert!(!app.tabs[0].doc.layers[1].visible);

        // 新規タブ: bbox サイズ・レイヤー構成(名前・表示・不透明度・重ね順・
        // アクティブレイヤー)を保ったまま複製されている。
        let new_doc = &app.tabs[1].doc;
        assert_eq!((new_doc.width, new_doc.height), (3, 3));
        assert_eq!(new_doc.layers.len(), 2);
        assert_eq!(new_doc.layers[0].name, "下");
        assert_eq!(new_doc.layers[1].name, "上");
        assert!(new_doc.layers[0].visible);
        assert!(!new_doc.layers[1].visible);
        assert_eq!(new_doc.layers[1].opacity, 128);
        assert_eq!(new_doc.active, 1, "active layer index is preserved");
        assert!(new_doc.path.is_none(), "duplicated tab has no path (無題)");
        assert!(new_doc.modified, "duplicated tab starts as unsaved");
        // 矩形選択(全 1 マスク)なので、選択範囲の全画素がそのまま複製される。
        assert_eq!(new_doc.layers[0].pixels[0..4], [255, 255, 255, 255]);
        assert_eq!(new_doc.layers[1].pixels[0..4], [10, 20, 30, 200]);

        assert_eq!(app.tabs[1].label(), "無題2");
        assert!(
            !app.tabs[1].history.can_undo() && !app.tabs[1].history.can_redo(),
            "the new tab starts with an empty undo history"
        );
    }

    #[test]
    fn duplicate_selection_to_new_tab_with_a_floating_piece_masks_pixels_and_does_not_touch_the_source_tab(
    ) {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::Select;
        // `begin_floating_from_selection` が実際にやること(切り出し元を
        // 透明化する)を模倣し、「複製後も元タブが一切変わらない」ことを
        // 確認できるようにする。
        for y in 2..4 {
            for x in 2..4 {
                app.active_tab_mut().doc.set_pixel(x, y, [0, 0, 0, 0]);
            }
        }
        let cut_from = crate::document::SelMask {
            bbox: IRect {
                x0: 2,
                y0: 2,
                x1: 4,
                y1: 4,
            },
            mask: vec![255, 255, 255, 255],
        };
        // 2x2: 左上・右下だけ選択。右上・左下は mask=0 だが、pixels 側には
        // わざと不透明な値を入れておく(ハンドルの再サンプリング後に
        // 起こりうる状態、`floating_layer_pixels` がそれでも透明にすることを
        // 確認するため)。
        let pixels = vec![
            10, 20, 30, 255, // top-left, masked in
            9, 9, 9, 255, // top-right, masked out but "dirty"
            9, 9, 9, 255, // bottom-left, masked out but "dirty"
            40, 50, 60, 128, // bottom-right, masked in
        ];
        let mask = vec![255, 0, 0, 255];
        let floating = Floating::new(pixels, 2, 2, mask, pos2(2.0, 2.0), Some(cut_from), 42);
        app.active_tab_mut().floating = Some(floating);
        app.active_tab_mut().selection = None;

        app.duplicate_selection_to_new_tab();

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);

        // 元のタブは一切変更されていない: 浮動片はまだそこにあり(合成
        // されていない)、切り出し元も透明のままである。
        assert!(
            app.tabs[0].floating.is_some(),
            "the source tab's floating piece must not be flushed/merged"
        );
        assert_eq!(app.tabs[0].floating.as_ref().unwrap().id, 42);
        assert_eq!(app.tabs[0].doc.get_pixel(2, 2), Some([0, 0, 0, 0]));
        assert_eq!(app.tabs[0].doc.get_pixel(0, 0), Some([255, 255, 255, 255]));

        let new_doc = &app.tabs[1].doc;
        assert_eq!((new_doc.width, new_doc.height), (2, 2));
        assert_eq!(
            new_doc.layers.len(),
            1,
            "a floating piece becomes a single layer"
        );
        assert_eq!(new_doc.active, 0);
        assert!(new_doc.modified);
        assert!(new_doc.path.is_none());
        let px = &new_doc.layers[0].pixels;
        assert_eq!(&px[0..4], &[10, 20, 30, 255], "masked-in top-left kept");
        assert_eq!(&px[4..8], &[0, 0, 0, 0], "masked-out top-right zeroed");
        assert_eq!(&px[8..12], &[0, 0, 0, 0], "masked-out bottom-left zeroed");
        assert_eq!(
            &px[12..16],
            &[40, 50, 60, 128],
            "masked-in bottom-right kept"
        );
    }

    #[test]
    fn duplicate_selection_to_new_tab_inserts_immediately_after_the_active_tab_and_shifts_later_tabs(
    ) {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        app.open_new_tab(Document::new(5, 5, Background::White));
        app.open_new_tab(Document::new(6, 6, Background::White));
        assert_eq!(app.tabs.len(), 3);

        app.switch_tab(1);
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        })));

        app.duplicate_selection_to_new_tab();

        assert_eq!(app.tabs.len(), 4);
        assert_eq!(
            app.active_tab, 2,
            "the new tab lands right after the source tab"
        );
        assert_eq!(
            (app.tabs[2].doc.width, app.tabs[2].doc.height),
            (2, 2),
            "index 2 is the freshly duplicated tab"
        );
        assert_eq!(
            app.tabs[3].doc.width, 6,
            "the tab that used to be at index 2 must have shifted to index 3"
        );
    }

    #[test]
    fn duplicate_selection_to_new_tab_refuses_past_the_tab_limit_and_shows_a_toast() {
        let mut app = new_for_test(Document::new(4, 4, Background::White));
        for _ in 0..(MAX_TABS - 1) {
            app.open_new_tab(Document::new(4, 4, Background::White));
        }
        assert_eq!(app.tabs.len(), MAX_TABS);

        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        })));
        app.duplicate_selection_to_new_tab();

        assert_eq!(
            app.tabs.len(),
            MAX_TABS,
            "must refuse to exceed MAX_TABS (SPEC §30: 上限 24)"
        );
        assert!(app.toast.is_some(), "must show a toast when refusing");
    }

    #[test]
    fn duplicate_selection_to_new_tab_ends_another_tools_in_progress_gesture_without_touching_the_selection(
    ) {
        // v5 §17.8-1: 進行中のジェスチャ(ここではなげなわの自由選択の軌跡)を
        // タブ挿入前に確定/破棄しないと、複製後にタブを跨いだ座標のまま
        // 古い軌跡へ点が継ぎ足されてしまう。浮動片は Select/EllipseSelect/
        // Move 中しか存在し得ないため、ここでは浮動片を伴わないプレーンな
        // 選択+ 別ツール(なげなわ)の進行中状態、という組み合わせを検証する。
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        })));
        app.tool = ToolKind::Lasso;
        app.lasso_freehand_points = vec![pos2(1.0, 1.0), pos2(2.0, 2.0)];

        app.duplicate_selection_to_new_tab();

        assert!(
            app.lasso_freehand_points.is_empty(),
            "the lasso's in-progress trail must be ended before switching tabs"
        );
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        assert_eq!((app.tabs[1].doc.width, app.tabs[1].doc.height), (4, 4));
    }

    #[test]
    fn normalize_path_for_compare_resolves_existing_paths_to_the_same_canonical_form() {
        let dir = temp_dir_for_app_test("normalize_path");
        let path = dir.join("a.png");
        io::save_image(
            &mut Document::new(2, 2, Background::White),
            &path,
            SaveFormat::Png,
        )
        .expect("seed file should save");

        // 同じファイルを指す 2 つの異なる書き方(`./` を挟む)が同一の
        // 正規化結果になること。
        let via_current_dir = dir.join(".").join("a.png");
        assert_eq!(
            normalize_path_for_compare(&path),
            normalize_path_for_compare(&via_current_dir)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_path_for_compare_falls_back_to_the_original_path_when_missing() {
        // 存在しないパスは canonicalize が失敗するため、panic せず元の
        // パスをそのまま返す(CLAUDE.md 鉄則: I/O 経路で unwrap しない)。
        let missing = PathBuf::from("__darask_paint_definitely_missing_dir__/x.png");
        assert_eq!(normalize_path_for_compare(&missing), missing);
    }

    #[test]
    fn handle_close_request_commits_pending_text_edit_before_allowing_close() {
        let Some(font) = test_font() else {
            eprintln!("skip: no system Japanese font found");
            return;
        };
        let mut app = new_for_test(Document::new(40, 40, Background::White));
        app.tool = ToolKind::Text;
        app.text_font = Some(font);
        app.begin_text_edit(pos2(5.0, 5.0));
        app.text_edit.as_mut().unwrap().buffer = "A".to_owned();

        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    events: vec![egui::ViewportEvent::Close],
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        });

        app.handle_close_request(&ctx);
        let _ = ctx.end_pass();

        assert!(
            app.text_edit.is_none(),
            "the pending text edit must have been committed before deciding whether to \
             allow the window to close"
        );
        assert!(
            matches!(app.modal, Some(ModalState::ConfirmUnsaved)),
            "a real committed edit must arm the unsaved-changes guard so closing the \
             window doesn't silently discard it"
        );
    }

    // ===================================================================
    // V4-M3(SPEC §22/§27): 楕円選択・なげなわ・自動選択
    // ===================================================================

    #[test]
    fn ellipse_select_tool_creates_an_ellipse_shaped_selection() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::EllipseSelect;

        app.handle_select_event(ToolEvent::Down {
            img: pos2(0.0, 0.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Up {
            img: pos2(10.0, 10.0),
            button: PointerButton::Primary,
        });

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("a real drag must create a selection");
        let expected = select::ellipse_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        });
        assert_eq!(selection.mask.bbox, expected.bbox);
        assert_eq!(selection.mask.mask, expected.mask);
        assert!(
            !selection.mask.contains(0, 0),
            "the bounding box corner must be outside the inscribed ellipse"
        );
    }

    #[test]
    fn shift_drag_constrains_marquee_selection_to_a_square() {
        // SPEC §22: 「Shift ドラッグで正方形/正円」。矩形選択でも適用される。
        let mut app = new_for_test(Document::new(30, 30, Background::White));
        app.tool = ToolKind::Select;

        app.handle_select_event(ToolEvent::Down {
            img: pos2(0.0, 0.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_select_event(ToolEvent::Drag {
            img: pos2(20.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::SHIFT,
        });
        app.handle_select_event(ToolEvent::Up {
            img: pos2(20.0, 5.0),
            button: PointerButton::Primary,
        });

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("a real drag must create a selection");
        let bbox = selection.mask.bbox;
        assert_eq!(
            bbox.width(),
            bbox.height(),
            "shift-drag must produce a square selection, got {bbox:?}"
        );
        assert_eq!(bbox.width(), 20);
    }

    #[test]
    fn cycle_marquee_tool_toggles_select_and_ellipse_select() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::Select);

        app.cycle_marquee_tool();
        assert_eq!(app.tool, ToolKind::EllipseSelect);
        app.cycle_marquee_tool();
        assert_eq!(app.tool, ToolKind::Select);
    }

    #[test]
    fn cycle_marquee_tool_from_a_non_marquee_tool_starts_from_last_marquee_tool() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::EllipseSelect);
        app.set_tool(ToolKind::Pen); // EllipseSelect が「直前に使った形状」のまま。

        app.cycle_marquee_tool();

        assert_eq!(app.tool, ToolKind::Select);
    }

    #[test]
    fn m_key_selects_last_used_marquee_tool_via_shortcuts() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::EllipseSelect);
        app.set_tool(ToolKind::Pen);

        let ctx = ctx_with_key_event(Key::M, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tool, ToolKind::EllipseSelect);
    }

    #[test]
    fn shift_m_cycles_marquee_via_shortcuts() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.set_tool(ToolKind::Select);

        let ctx = ctx_with_key_event(Key::M, Modifiers::SHIFT);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tool, ToolKind::EllipseSelect);
    }

    #[test]
    fn w_key_selects_magic_wand_via_shortcuts() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;

        let ctx = ctx_with_key_event(Key::W, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.tool, ToolKind::MagicWand);
    }

    // -- なげなわ(自由) ----------------------------------------------------

    #[test]
    fn lasso_freehand_drag_creates_a_selection_matching_the_trail() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        assert_eq!(app.lasso_mode, LassoMode::Freehand);

        app.handle_lasso_event(ToolEvent::Down {
            img: pos2(2.0, 2.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        for img in [pos2(10.0, 2.0), pos2(10.0, 10.0), pos2(2.0, 10.0)] {
            app.handle_lasso_event(ToolEvent::Drag {
                img,
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            });
        }
        app.handle_lasso_event(ToolEvent::Up {
            img: pos2(2.0, 10.0),
            button: PointerButton::Primary,
        });

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("closing the free-hand trail must create a selection");
        assert!(selection.mask.contains(5, 5));
        assert!(
            app.lasso_freehand_points.is_empty(),
            "the in-progress trail must be cleared once committed"
        );
    }

    #[test]
    fn lasso_freehand_single_click_creates_no_selection() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;

        app.handle_lasso_event(ToolEvent::Down {
            img: pos2(2.0, 2.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_lasso_event(ToolEvent::Up {
            img: pos2(2.0, 2.0),
            button: PointerButton::Primary,
        });

        assert!(app.active_tab().selection.is_none());
    }

    #[test]
    fn switching_tool_away_from_lasso_mid_drag_discards_the_trail() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.handle_lasso_event(ToolEvent::Down {
            img: pos2(2.0, 2.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        app.handle_lasso_event(ToolEvent::Drag {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        });
        assert!(!app.lasso_freehand_points.is_empty());

        app.set_tool(ToolKind::Pen);

        assert!(app.lasso_freehand_points.is_empty());
        assert!(app.active_tab().selection.is_none());
    }

    // -- なげなわ(多角形) --------------------------------------------------

    #[test]
    fn lasso_polygon_click_adds_vertices_and_closes_near_the_start_point() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.lasso_mode = LassoMode::Polygon;

        app.lasso_polygon_click(pos2(2.0, 2.0));
        assert!(app.lasso_polygon.is_some());
        app.lasso_polygon_click(pos2(16.0, 2.0));
        app.lasso_polygon_click(pos2(16.0, 16.0));
        app.lasso_polygon_click(pos2(2.0, 16.0));
        assert_eq!(app.lasso_polygon.as_ref().unwrap().points.len(), 4);

        // 始点付近をクリックして閉じる(SPEC §22:「始点クリックで閉じる」)。
        app.lasso_polygon_click(pos2(2.4, 2.4));

        assert!(app.lasso_polygon.is_none());
        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("closing near the start point must create a selection");
        assert!(selection.mask.contains(5, 5));
    }

    #[test]
    fn lasso_polygon_double_click_closes_without_adding_a_duplicate_vertex() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.lasso_mode = LassoMode::Polygon;

        app.lasso_polygon_click(pos2(2.0, 2.0));
        app.lasso_polygon_click(pos2(10.0, 2.0));
        app.lasso_polygon_click(pos2(10.0, 10.0));
        // ダブルクリック(ほぼ同じ位置ですぐに 2 回クリック)。
        app.lasso_polygon_click(pos2(6.0, 15.0));
        app.lasso_polygon_click(pos2(6.0, 15.0));

        assert!(
            app.lasso_polygon.is_none(),
            "a double click must close the polygon (SPEC §22)"
        );
        assert!(app.active_tab().selection.is_some());
    }

    #[test]
    fn lasso_polygon_enter_commits_the_selection() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.lasso_mode = LassoMode::Polygon;
        app.lasso_polygon_click(pos2(2.0, 2.0));
        app.lasso_polygon_click(pos2(16.0, 2.0));
        app.lasso_polygon_click(pos2(16.0, 16.0));
        app.lasso_polygon_click(pos2(2.0, 16.0));
        assert!(
            app.lasso_polygon.is_some(),
            "the polygon must still be open before Enter (vertices are far enough from the \
             start point to not auto-close)"
        );

        let ctx = ctx_with_key_event(Key::Enter, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert!(app.lasso_polygon.is_none());
        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("Enter must commit the in-progress polygon (SPEC §22)");
        assert!(selection.mask.contains(5, 5));
    }

    #[test]
    fn lasso_polygon_esc_cancels_without_creating_a_selection() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.lasso_mode = LassoMode::Polygon;
        app.lasso_polygon_click(pos2(2.0, 2.0));
        app.lasso_polygon_click(pos2(10.0, 2.0));
        app.lasso_polygon_click(pos2(10.0, 10.0));
        assert!(app.lasso_polygon.is_some());

        let ctx = ctx_with_key_event(Key::Escape, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert!(
            app.lasso_polygon.is_none(),
            "Esc must discard the in-progress polygon (SPEC §22)"
        );
        assert!(app.active_tab().selection.is_none());
    }

    #[test]
    fn shift_l_toggles_lasso_mode_and_discards_an_in_progress_polygon() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Lasso;
        app.lasso_mode = LassoMode::Polygon;
        app.lasso_polygon_click(pos2(2.0, 2.0));
        assert!(app.lasso_polygon.is_some());

        let ctx = ctx_with_key_event(Key::L, Modifiers::SHIFT);
        app.handle_shortcuts(&ctx);

        assert_eq!(app.lasso_mode, LassoMode::Freehand);
        assert!(app.lasso_polygon.is_none());
    }

    // -- 自動選択(マジックワンド) --------------------------------------------

    #[test]
    fn magic_wand_select_picks_the_connected_region_only() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        for y in 0..10 {
            for x in 5..10 {
                app.active_tab_mut().doc.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
        app.tool = ToolKind::MagicWand;
        app.magic_wand_tolerance = 0;

        app.magic_wand_select(pos2(1.0, 1.0));

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("magic wand must select the connected white region");
        assert!(selection.mask.contains(0, 0));
        assert!(
            !selection.mask.contains(5, 0),
            "must not cross into the black half"
        );
    }

    #[test]
    fn magic_wand_select_replaces_any_existing_selection() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::MagicWand;
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 3,
        })));

        app.magic_wand_select(pos2(5.0, 5.0));

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("magic wand always creates a fresh selection");
        assert_eq!(
            selection.mask.bbox,
            IRect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 10
            },
            "the whole document is one connected color so it must all be selected"
        );
    }

    // -- v4 レビューで発見・修正したバグ: SPEC §18「Esc は選択を解除する」が
    // 自動選択(MagicWand)には配線されていなかった -----------------------

    #[test]
    fn magic_wand_esc_deselects_a_plain_selection() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::MagicWand;
        app.magic_wand_select(pos2(1.0, 1.0));
        assert!(
            app.active_tab().selection.is_some(),
            "W + click must have created a selection"
        );

        let ctx = ctx_with_key_event(Key::Escape, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert!(
            app.active_tab().selection.is_none(),
            "SPEC §18: Esc must deselect regardless of the active tool"
        );
    }

    #[test]
    fn magic_wand_enter_also_clears_the_selection() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::MagicWand;
        app.magic_wand_select(pos2(1.0, 1.0));
        assert!(app.active_tab().selection.is_some());

        let ctx = ctx_with_key_event(Key::Enter, Modifiers::NONE);
        app.handle_shortcuts(&ctx);

        assert!(app.active_tab().selection.is_none());
    }

    // -- v4 レビューで発見・修正したバグ: 色調補正が進行中ストロークを
    // 確定せず undo 履歴を破壊する ------------------------------------------

    #[test]
    fn apply_invert_mid_pen_drag_commits_the_open_stroke_as_its_own_undo_unit() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.primary = Color32::BLACK;
        let pristine = app.active_tab().doc.active_pixels().to_vec();

        // ドラッグ開始(まだ Up していない = 開いたストローク)。
        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        app.dispatch_canvas_events(vec![ToolEvent::Drag {
            img: pos2(8.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(
            app.active_tab().history.has_open_stroke(),
            "the pen drag must still be open"
        );
        assert!(
            !app.active_tab().history.can_undo(),
            "nothing committed yet"
        );
        let mid_stroke_pixels = app.active_tab().doc.active_pixels().to_vec();
        assert_ne!(
            mid_stroke_pixels, pristine,
            "the drag must have painted something already"
        );

        // ドラッグ中に Ctrl+I(階調の反転、即時適用)。
        app.apply_invert();

        // 反転は「ドラッグでこれまでに確定した画素」に対して行われるはず
        // (ドキュメント全体に反転をかけているので、期待値は
        // mid_stroke_pixels 全体の反転)。
        let mut expected_inverted = mid_stroke_pixels.clone();
        for px in expected_inverted.chunks_exact_mut(4) {
            px[0] = 255 - px[0];
            px[1] = 255 - px[1];
            px[2] = 255 - px[2];
        }
        assert_eq!(
            app.active_tab().doc.active_pixels(),
            expected_inverted.as_slice()
        );
        assert!(
            !app.active_tab().history.has_open_stroke(),
            "Ctrl+I must fully commit the drag, not leave it dangling"
        );

        // ペンストロークと反転はそれぞれ独立した undo 単位でなければならない
        // (バグ版では前者が `History::begin_stroke` の無警告置換で undo
        // 履歴に一切残らず、1 回しか undo できない)。
        assert!(
            {
                let tab = app.active_tab_mut();
                tab.history.undo(&mut tab.doc)
            },
            "undo #1: revert the invert"
        );
        assert_eq!(
            app.active_tab().doc.active_pixels(),
            mid_stroke_pixels.as_slice(),
            "undoing the invert must restore the pre-invert (mid-stroke) pixels exactly"
        );
        assert!(
            {
                let tab = app.active_tab_mut();
                tab.history.undo(&mut tab.doc)
            },
            "undo #2: the pen stroke drawn before Ctrl+I must be its own undo unit, \
             not silently discarded"
        );
        assert_eq!(
            app.active_tab().doc.active_pixels(),
            pristine.as_slice(),
            "undoing everything must restore the pristine canvas byte-exactly"
        );
        assert!(!app.active_tab().history.can_undo());
    }

    #[test]
    fn open_hue_saturation_modal_mid_pen_drag_commits_the_open_stroke_first() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.tool = ToolKind::Pen;
        app.primary = Color32::BLACK;

        app.dispatch_canvas_events(vec![ToolEvent::Down {
            img: pos2(5.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        app.dispatch_canvas_events(vec![ToolEvent::Drag {
            img: pos2(8.0, 5.0),
            button: PointerButton::Primary,
            mods: Modifiers::NONE,
        }]);
        assert!(app.active_tab().history.has_open_stroke());
        assert!(!app.active_tab().history.can_undo());

        app.open_hue_saturation_modal();

        assert!(
            app.active_tab().history.can_undo(),
            "the pen drag must have been committed as its own undo unit before Ctrl+U's \
             own live-preview snapshot stroke begins"
        );
        assert!(
            app.active_tab().history.has_open_stroke(),
            "begin_tone_adjust_stroke itself opens a fresh snapshot stroke for the live preview"
        );
        assert!(app.modal.is_some());
    }

    // -- v4 レビューで発見・修正したバグ: モーダル表示中も進行中ドラッグが
    // キャンバスに描画され続ける ---------------------------------------------

    #[test]
    fn dispatch_canvas_events_is_a_no_op_while_a_modal_is_open() {
        let mut app = new_for_test(Document::new(10, 10, Background::White));
        app.tool = ToolKind::Pen;
        app.primary = Color32::BLACK;
        let pristine = app.active_tab().doc.active_pixels().to_vec();

        app.modal = Some(ModalState::About);
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(2.0, 2.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Drag {
                img: pos2(6.0, 2.0),
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(6.0, 2.0),
                button: PointerButton::Primary,
            },
        ]);

        assert_eq!(
            app.active_tab().doc.active_pixels(),
            pristine.as_slice(),
            "no pointer event may reach the canvas while a modal is open (ARCHITECTURE.md §10)"
        );
        assert!(!app.active_tab().history.has_open_stroke());
        assert!(!app.active_tab().history.can_undo());
    }

    // -- v4 レビューで発見・修正したバグ: undo/redo が選択を新しい文書寸法へ
    // クランプしない -----------------------------------------------------

    #[test]
    fn redo_of_a_shrinking_resize_drops_a_selection_that_no_longer_overlaps_the_document() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.confirm_canvas_resize(5, 5);
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (5, 5)
        );

        app.handle_menu_action(MenuAction::Undo);
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (20, 20),
            "undo must restore the original 20x20 canvas"
        );

        // 元の(20x20 の)キャンバスの右下領域を選択する。redo 後の 5x5 の
        // 範囲には一切かからない。
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 10,
            y0: 10,
            x1: 18,
            y1: 18,
        })));

        app.handle_menu_action(MenuAction::Redo);
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (5, 5),
            "redo must reapply the canvas resize to 5x5"
        );

        assert!(
            app.active_tab().selection.is_none(),
            "a selection that no longer overlaps the resized document must be dropped, \
             not left dangling with stale out-of-bounds coordinates"
        );
    }

    #[test]
    fn undo_of_a_shrinking_resize_keeps_a_still_overlapping_selection_clamped_and_paintable() {
        let mut app = new_for_test(Document::new(20, 20, Background::White));
        app.confirm_canvas_resize(10, 10);
        app.handle_menu_action(MenuAction::Undo);
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (20, 20)
        );

        // (5,5)-(15,15) は 10x10 に縮小すると右下半分がはみ出す。
        app.active_tab_mut().selection = Some(Selection::new(select::rect_mask(IRect {
            x0: 5,
            y0: 5,
            x1: 15,
            y1: 15,
        })));

        app.handle_menu_action(MenuAction::Redo);
        assert_eq!(
            (app.active_tab().doc.width, app.active_tab().doc.height),
            (10, 10)
        );

        let selection = app.active_tab().selection.as_ref().expect(
            "the selection still partially overlaps the new bounds, so it must be kept (clamped), \
             not dropped",
        );
        assert_eq!(
            selection.mask.bbox,
            IRect {
                x0: 5,
                y0: 5,
                x1: 10,
                y1: 10
            },
            "the selection bbox must be clamped to the new, smaller document bounds"
        );

        // クランプ後の選択は実際にクリップとして機能し続けるはず: 選択内は
        // 描け、選択外(文書内だが選択の外)は描けない(バグ版では選択の
        // bbox が文書外を指したままになり、SelMask::contains が全画素
        // false を返して 1 画素も描けなくなる)。
        app.tool = ToolKind::Pen;
        app.primary = Color32::BLACK;
        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(7.0, 7.0), // 選択内
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(7.0, 7.0),
                button: PointerButton::Primary,
            },
        ]);
        assert_ne!(
            app.active_tab().doc.get_pixel(7, 7),
            Some([255, 255, 255, 255]),
            "painting inside the clamped selection must work"
        );

        app.dispatch_canvas_events(vec![
            ToolEvent::Down {
                img: pos2(1.0, 1.0), // 選択外(文書内)
                button: PointerButton::Primary,
                mods: Modifiers::NONE,
            },
            ToolEvent::Up {
                img: pos2(1.0, 1.0),
                button: PointerButton::Primary,
            },
        ]);
        assert_eq!(
            app.active_tab().doc.get_pixel(1, 1),
            Some([255, 255, 255, 255]),
            "painting outside the selection must still be clipped"
        );
    }

    // -- v4 レビューで発見・修正したバグ: キャンバス境界を跨ぐ楕円選択の
    // ドラッグが「クランプ後の矩形に内接する縮んだ楕円」になってしまう ------

    #[test]
    fn ellipse_selection_dragged_past_the_canvas_edge_matches_the_unclamped_ellipse() {
        let mut app = new_for_test(Document::new(100, 100, Background::White));
        app.tool = ToolKind::EllipseSelect;

        // 外接矩形 (-50,-50)-(100,100) の楕円ドラッグ(キャンバス境界を
        // 跨ぐ)。
        app.select_down(pos2(-50.0, -50.0));
        app.select_drag_move(pos2(100.0, 100.0), Modifiers::NONE);
        app.select_up(pos2(100.0, 100.0));

        let selection = app
            .active_tab()
            .selection
            .as_ref()
            .expect("a non-degenerate ellipse drag must produce a selection");

        // 期待値: raster::fill_ellipse と同じ判定式(非クランプの外接矩形
        // から楕円方程式を評価し、はみ出し分だけを画素単位で切り落とす)。
        // バグ版は先に矩形を (0,0)-(100,100) にクランプしてから楕円を
        // 内接させるため、半径 50 の正円(中心 (50,50))という別の図形に
        // なってしまう。
        let unclamped_rect = IRect {
            x0: -50,
            y0: -50,
            x1: 100,
            y1: 100,
        };
        let expected = select::ellipse_mask(unclamped_rect).clamp_to(100, 100);
        assert_eq!(selection.mask.bbox, expected.bbox);
        assert_eq!(selection.mask.mask, expected.mask);

        // (25, 99) は正しい(非クランプ楕円: 中心(25,25), rx=ry=75)には
        // 含まれるが、バグ版の縮んだ正円(中心(50,50), 半径50)には
        // ((25-50)^2+(99-50)^2 = 3026 > 2500 なので)含まれない。
        assert!(
            selection.mask.contains(25, 99),
            "the correct (build-then-clip) ellipse must include this pixel"
        );
    }
}
