//! 設定の永続化(SPEC §26、ARCHITECTURE.md §16.7)。
//!
//! 軽量な独自形式(`キー\t値` の UTF-8 テキスト、外部 crate 禁止・手書き
//! パーサ)で `%APPDATA%\darask-paint\settings.txt` に保存する。
//!
//! - 読み込みは起動時 1 回(`main()` がウィンドウ初期寸法を決めるために
//!   呼ぶ)。
//! - 書き込みは終了時と最近使ったファイル更新時のみ(`app.rs` が呼ぶ)。
//! - 壊れている・存在しない・値が範囲外 等はすべて黙って既定値にフォール
//!   バックする(`parse` は不正な行/値を無視するだけで絶対にパニックしない。
//!   ARCHITECTURE.md §16.10-5)。起動・終了を妨げないことが最優先で、設定の
//!   保存はあくまで利便性機能。
//!
//! パス(`recent.N`)は `Path::to_string_lossy` で文字列化する。Windows の
//! ファイル名は制御文字(タブ・改行を含む)を含められないため、`\t`/`\n` を
//! 区切りに使う本形式と衝突しない。非 UTF-8 の奇妙なパスは looselyな変換に
//! なりうるが、その場合も「開けなければトーストで通知し一覧から除去される」
//! (`app.rs::open_recent_file`)ため実害はない。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use eframe::egui::Color32;

use crate::raster::GradientKind;
use crate::tools::gradient::GradientColors;
use crate::tools::shapes::ShapeMode;
use crate::tools::ToolKind;
use crate::ui::color_panel::{format_hex, parse_hex_color};

/// SPEC §26: 「最近使ったファイル(最大 8)」。
pub const MAX_RECENT_FILES: usize = 8;

/// ユーザーパレット(SPEC §14: 「＋」でプライマリ色を追加)の件数上限。
/// v4 レビューで発見・修正したバグ: `recent.N` は `MAX_RECENT_FILES` で
/// キャップされる (`parse` 参照) のに `palette.N` だけ無制限に読み込んで
/// いたため、破損・悪意ある `settings.txt` に `palette.0`〜`palette.999999`
/// のような大量の行があると、起動時に全件をパース・保持し、右パネルの
/// パレット表示(仮想化なし、`ui/color_panel.rs`)が毎フレーム全スウォッチを
/// レイアウトしようとして UI が事実上ハングする(ARCHITECTURE.md §16.10-5
/// 「設定読込は…壊れた settings.txt を防御」に反する)。`recent.N` と同じ
/// 防御を対称に適用する。実運用で「＋」ボタンから手動追加する分には
/// まず到達しない、十分に余裕のある上限。
pub const MAX_USER_PALETTE: usize = 256;

/// SPEC §3 の初期ウィンドウ寸法(既定値。旧 main.rs のハードコード値と揃える)。
pub const DEFAULT_WINDOW_WIDTH: u32 = 1280;
pub const DEFAULT_WINDOW_HEIGHT: u32 = 800;
/// SPEC §3: 「最小 640×480」。読み込んだ値・書き込む値の双方をこの下限で
/// クランプする。
pub const MIN_WINDOW_WIDTH: u32 = 640;
pub const MIN_WINDOW_HEIGHT: u32 = 480;
/// 壊れた設定ファイルに極端な値(例: 桁溢れ手前の巨大な数)が書かれていても
/// 使い物にならない巨大ウィンドウを作らないための上限(ARCHITECTURE.md
/// §16.10-5 の防御的読み込みの一環。SPEC が定める値ではない)。
const MAX_WINDOW_DIMENSION: u32 = 20000;

/// ブラシ/ツールオプションの既定値(SPEC §17/§22/§23 のデフォルトと同じ、
/// `app.rs` の `DaraskApp::new`/テスト用コンストラクタもこの値を使う
/// —単一の情報源にして既定値のずれを防ぐ)。
pub const DEFAULT_BRUSH_SIZE: f32 = 4.0;
pub const DEFAULT_BRUSH_HARDNESS: u8 = 100;
pub const DEFAULT_BRUSH_OPACITY: u8 = 100;
pub const DEFAULT_BRUSH_SMOOTHING: u8 = 0;

/// SPEC §34/ARCHITECTURE.md §18.2: 「元に戻す履歴の保持数」(1–500、既定 50)。
/// `history.rs::DEFAULT_MAX_STEPS`(`History::new()` の既定値)と同じ 50 だが、
/// あちらは `usize`・こちら側は設定ファイルの値域を表す `u32` で、モジュールも
/// 独立している(`history.rs` は `settings.rs` を知らない)ため、値としてだけ
/// 揃える。どちらかを変えるときはもう片方も揃えること(このファイルの
/// `default_max_undo_steps_is_50` と `history.rs` 側の
/// `DEFAULT_MAX_STEPS` を使うテストの双方を確認する)。
pub const DEFAULT_MAX_UNDO_STEPS: u32 = 50;
/// SPEC §34: 「保持数」の入力範囲(1–500)。
pub const MIN_MAX_UNDO_STEPS: u32 = 1;
pub const MAX_MAX_UNDO_STEPS: u32 = 500;

/// 永続化する設定(SPEC §26 の列挙そのもの。これ以外のフィールドを足さない
/// こと — 「最近使った色」は同 SPEC の永続化対象リストに含まれていないため
/// ここには無い)。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub window_width: u32,
    pub window_height: u32,
    pub window_maximized: bool,
    /// 先頭が最新(MRU 順)。最大 `MAX_RECENT_FILES` 件。
    pub recent_files: VecDeque<PathBuf>,
    pub brush_size: f32,
    pub brush_hardness: u8,
    pub brush_opacity: u8,
    pub pencil_mode: bool,
    pub brush_smoothing: u8,
    /// 塗りつぶしツールの許容値(SPEC §4)。
    pub fill_tolerance: u8,
    /// 自動選択の許容値(SPEC §22)。
    pub magic_wand_tolerance: u8,
    pub rect_mode: ShapeMode,
    pub ellipse_mode: ShapeMode,
    pub gradient_kind: GradientKind,
    pub gradient_colors: GradientColors,
    pub primary: Color32,
    pub secondary: Color32,
    pub user_palette: Vec<Color32>,
    pub last_tool: ToolKind,
    pub show_pixel_grid: bool,
    /// SPEC §34/ARCHITECTURE.md §18.2: 「元に戻す履歴の保持数」
    /// (1–500、既定 50)。設定ダイアログ(v6-M2)の OK で更新され、開いている
    /// 全タブの `History::set_max_steps` へ即座に反映される
    /// (`app.rs::apply_preferences` 参照)。
    pub max_undo_steps: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window_width: DEFAULT_WINDOW_WIDTH,
            window_height: DEFAULT_WINDOW_HEIGHT,
            window_maximized: false,
            recent_files: VecDeque::new(),
            brush_size: DEFAULT_BRUSH_SIZE,
            brush_hardness: DEFAULT_BRUSH_HARDNESS,
            brush_opacity: DEFAULT_BRUSH_OPACITY,
            pencil_mode: false,
            brush_smoothing: DEFAULT_BRUSH_SMOOTHING,
            fill_tolerance: 0,
            magic_wand_tolerance: 0,
            rect_mode: ShapeMode::Outline,
            ellipse_mode: ShapeMode::Outline,
            gradient_kind: GradientKind::Linear,
            gradient_colors: GradientColors::PrimaryToSecondary,
            // MS ペイント等と同じ初期値(`app.rs::DaraskApp::new` と揃える)。
            primary: Color32::BLACK,
            secondary: Color32::WHITE,
            user_palette: Vec::new(),
            last_tool: ToolKind::Pen,
            // SPEC §25: 「デフォルト ON」。
            show_pixel_grid: true,
            max_undo_steps: DEFAULT_MAX_UNDO_STEPS,
        }
    }
}

impl Settings {
    /// 読み込んだ/書き込む前のウィンドウ寸法を正当な範囲へクランプする
    /// (SPEC §3 の最小値、および破損値からの防御 — `MAX_WINDOW_DIMENSION`
    /// 参照)。
    fn clamp_window_dims(&mut self) {
        self.window_width = self
            .window_width
            .clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_DIMENSION);
        self.window_height = self
            .window_height
            .clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_DIMENSION);
    }

    /// SPEC §34: 「保持数」を [1, 500] へクランプする(`clamp_window_dims` と
    /// 同じ流儀。手編集・破損した設定ファイルからの防御、ARCHITECTURE.md
    /// §16.10-5)。
    fn clamp_max_undo_steps(&mut self) {
        self.max_undo_steps = self
            .max_undo_steps
            .clamp(MIN_MAX_UNDO_STEPS, MAX_MAX_UNDO_STEPS);
    }
}

// ---------------------------------------------------------------------------
// enum ⇔ 文字列タグ(設定ファイル専用の識別子。UI 表示文字列とは独立させる
// — 表示文言(`label()`)が変わっても保存済みファイルの互換性を壊さない)。
// ---------------------------------------------------------------------------

fn tool_kind_tag(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Pen => "pen",
        ToolKind::Eraser => "eraser",
        ToolKind::Line => "line",
        ToolKind::Rect => "rect",
        ToolKind::Ellipse => "ellipse",
        ToolKind::Fill => "fill",
        ToolKind::Picker => "picker",
        ToolKind::Select => "select",
        ToolKind::Pan => "pan",
        ToolKind::Move => "move",
        ToolKind::Zoom => "zoom",
        ToolKind::Text => "text",
        ToolKind::EllipseSelect => "ellipse_select",
        ToolKind::Lasso => "lasso",
        ToolKind::MagicWand => "magic_wand",
        ToolKind::Gradient => "gradient",
    }
}

fn tool_kind_from_tag(tag: &str) -> Option<ToolKind> {
    Some(match tag {
        "pen" => ToolKind::Pen,
        "eraser" => ToolKind::Eraser,
        "line" => ToolKind::Line,
        "rect" => ToolKind::Rect,
        "ellipse" => ToolKind::Ellipse,
        "fill" => ToolKind::Fill,
        "picker" => ToolKind::Picker,
        "select" => ToolKind::Select,
        "pan" => ToolKind::Pan,
        "move" => ToolKind::Move,
        "zoom" => ToolKind::Zoom,
        "text" => ToolKind::Text,
        "ellipse_select" => ToolKind::EllipseSelect,
        "lasso" => ToolKind::Lasso,
        "magic_wand" => ToolKind::MagicWand,
        "gradient" => ToolKind::Gradient,
        _ => return None,
    })
}

fn shape_mode_tag(mode: ShapeMode) -> &'static str {
    match mode {
        ShapeMode::Outline => "outline",
        ShapeMode::Fill => "fill",
        ShapeMode::Both => "both",
    }
}

fn shape_mode_from_tag(tag: &str) -> Option<ShapeMode> {
    Some(match tag {
        "outline" => ShapeMode::Outline,
        "fill" => ShapeMode::Fill,
        "both" => ShapeMode::Both,
        _ => return None,
    })
}

fn gradient_kind_tag(kind: GradientKind) -> &'static str {
    match kind {
        GradientKind::Linear => "linear",
        GradientKind::Radial => "radial",
    }
}

fn gradient_kind_from_tag(tag: &str) -> Option<GradientKind> {
    Some(match tag {
        "linear" => GradientKind::Linear,
        "radial" => GradientKind::Radial,
        _ => return None,
    })
}

fn gradient_colors_tag(colors: GradientColors) -> &'static str {
    match colors {
        GradientColors::PrimaryToSecondary => "primary_secondary",
        GradientColors::PrimaryToTransparent => "primary_transparent",
    }
}

fn gradient_colors_from_tag(tag: &str) -> Option<GradientColors> {
    Some(match tag {
        "primary_secondary" => GradientColors::PrimaryToSecondary,
        "primary_transparent" => GradientColors::PrimaryToTransparent,
        _ => return None,
    })
}

fn bool_tag(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// `キー.N` 形式(`recent.0`, `palette.3` 等)のリスト項目を、出現順ではなく
/// `N` の昇順で並べ直して返す(手編集で順序が入れ替わっていても MRU 順/
/// パレット順を保つ)。`N` が `usize` として読めない行・接頭辞が一致しない
/// 行は無視する。
fn collect_indexed<'a>(
    lines: impl Iterator<Item = (&'a str, &'a str)>,
    prefix: &str,
) -> Vec<&'a str> {
    let mut items: Vec<(usize, &str)> = lines
        .filter_map(|(key, value)| {
            let idx_str = key.strip_prefix(prefix)?;
            let idx: usize = idx_str.parse().ok()?;
            Some((idx, value))
        })
        .collect();
    items.sort_by_key(|(idx, _)| *idx);
    items.into_iter().map(|(_, v)| v).collect()
}

/// `text`(ファイル全体)から `Settings` を組み立てる(ARCHITECTURE.md
/// §16.7: 「パーサ・シリアライザは往復テスト必須」)。1 行 1 項目の
/// `キー\t値`。不正な行(タブが無い/キー不明/値がパースできない)は個別に
/// 無視し、その項目だけ既定値のまま残す(ファイル全体を捨てない)。
pub fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();

    // 2 パス構成: まず全行を `(key, value)` に分解して保持し(順序も保つ)、
    // 単一値のキーはその場で反映、`recent.N`/`palette.N` は後段で
    // `collect_indexed` によりインデックス順に並べ直す。
    let entries: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();

    for &(key, value) in &entries {
        match key {
            "window.width" => {
                if let Ok(v) = value.parse::<u32>() {
                    settings.window_width = v;
                }
            }
            "window.height" => {
                if let Ok(v) = value.parse::<u32>() {
                    settings.window_height = v;
                }
            }
            "window.maximized" => {
                if let Some(v) = parse_bool(value) {
                    settings.window_maximized = v;
                }
            }
            "brush.size" => {
                if let Ok(v) = value.parse::<f32>() {
                    if v.is_finite() {
                        settings.brush_size = v;
                    }
                }
            }
            "brush.hardness" => {
                if let Ok(v) = value.parse::<u8>() {
                    settings.brush_hardness = v;
                }
            }
            "brush.opacity" => {
                if let Ok(v) = value.parse::<u8>() {
                    settings.brush_opacity = v;
                }
            }
            "brush.pencil" => {
                if let Some(v) = parse_bool(value) {
                    settings.pencil_mode = v;
                }
            }
            "brush.smoothing" => {
                if let Ok(v) = value.parse::<u8>() {
                    settings.brush_smoothing = v;
                }
            }
            "tool.fill_tolerance" => {
                if let Ok(v) = value.parse::<u8>() {
                    settings.fill_tolerance = v;
                }
            }
            "tool.magic_wand_tolerance" => {
                if let Ok(v) = value.parse::<u8>() {
                    settings.magic_wand_tolerance = v;
                }
            }
            "tool.rect_mode" => {
                if let Some(v) = shape_mode_from_tag(value) {
                    settings.rect_mode = v;
                }
            }
            "tool.ellipse_mode" => {
                if let Some(v) = shape_mode_from_tag(value) {
                    settings.ellipse_mode = v;
                }
            }
            "tool.gradient_kind" => {
                if let Some(v) = gradient_kind_from_tag(value) {
                    settings.gradient_kind = v;
                }
            }
            "tool.gradient_colors" => {
                if let Some(v) = gradient_colors_from_tag(value) {
                    settings.gradient_colors = v;
                }
            }
            "color.primary" => {
                if let Some(v) = parse_hex_color(value) {
                    settings.primary = v;
                }
            }
            "color.secondary" => {
                if let Some(v) = parse_hex_color(value) {
                    settings.secondary = v;
                }
            }
            "last_tool" => {
                if let Some(v) = tool_kind_from_tag(value) {
                    settings.last_tool = v;
                }
            }
            "pixel_grid" => {
                if let Some(v) = parse_bool(value) {
                    settings.show_pixel_grid = v;
                }
            }
            "history.max_steps" => {
                if let Ok(v) = value.parse::<u32>() {
                    settings.max_undo_steps = v;
                }
            }
            _ => {} // recent.N/palette.N は下で、未知キーはここで無視。
        }
    }

    settings.recent_files = collect_indexed(entries.iter().copied(), "recent.")
        .into_iter()
        .take(MAX_RECENT_FILES)
        .map(PathBuf::from)
        .collect();

    settings.user_palette = collect_indexed(entries.iter().copied(), "palette.")
        .into_iter()
        .take(MAX_USER_PALETTE)
        .filter_map(parse_hex_color)
        .collect();

    settings.clamp_window_dims();
    settings.clamp_max_undo_steps();
    settings
}

/// 1 行(`キー\t値\n`)を追記する(`serialize` の内部ヘルパー)。
fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('\t');
    out.push_str(value);
    out.push('\n');
}

/// `Settings` をファイル内容の文字列へ直列化する(`parse` の逆)。
pub fn serialize(settings: &Settings) -> String {
    let mut s = settings.clone();
    s.clamp_window_dims();
    s.clamp_max_undo_steps();

    let mut out = String::new();
    push_line(&mut out, "window.width", &s.window_width.to_string());
    push_line(&mut out, "window.height", &s.window_height.to_string());
    push_line(&mut out, "window.maximized", bool_tag(s.window_maximized));
    for (i, path) in s.recent_files.iter().take(MAX_RECENT_FILES).enumerate() {
        push_line(&mut out, &format!("recent.{i}"), &path.to_string_lossy());
    }
    push_line(&mut out, "brush.size", &s.brush_size.to_string());
    push_line(&mut out, "brush.hardness", &s.brush_hardness.to_string());
    push_line(&mut out, "brush.opacity", &s.brush_opacity.to_string());
    push_line(&mut out, "brush.pencil", bool_tag(s.pencil_mode));
    push_line(&mut out, "brush.smoothing", &s.brush_smoothing.to_string());
    push_line(
        &mut out,
        "tool.fill_tolerance",
        &s.fill_tolerance.to_string(),
    );
    push_line(
        &mut out,
        "tool.magic_wand_tolerance",
        &s.magic_wand_tolerance.to_string(),
    );
    push_line(&mut out, "tool.rect_mode", shape_mode_tag(s.rect_mode));
    push_line(
        &mut out,
        "tool.ellipse_mode",
        shape_mode_tag(s.ellipse_mode),
    );
    push_line(
        &mut out,
        "tool.gradient_kind",
        gradient_kind_tag(s.gradient_kind),
    );
    push_line(
        &mut out,
        "tool.gradient_colors",
        gradient_colors_tag(s.gradient_colors),
    );
    push_line(&mut out, "color.primary", &format_hex(s.primary));
    push_line(&mut out, "color.secondary", &format_hex(s.secondary));
    for (i, color) in s.user_palette.iter().take(MAX_USER_PALETTE).enumerate() {
        push_line(&mut out, &format!("palette.{i}"), &format_hex(*color));
    }
    push_line(&mut out, "last_tool", tool_kind_tag(s.last_tool));
    push_line(&mut out, "pixel_grid", bool_tag(s.show_pixel_grid));
    push_line(&mut out, "history.max_steps", &s.max_undo_steps.to_string());
    out
}

/// `%APPDATA%\darask-paint\settings.txt`(存在しない/`APPDATA` 未設定なら
/// `None`。ARCHITECTURE.md §16.7)。
fn settings_file_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    if appdata.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(appdata)
            .join("darask-paint")
            .join("settings.txt"),
    )
}

/// テスト可能にするため、実際のパスを取る内部実装を分離する
/// (`load`/`save` はこれの薄いラッパー)。
fn load_from_path(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Settings::default(),
    }
}

fn save_to_path(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serialize(settings))
}

/// 起動時に 1 回だけ呼ぶ(`main()`)。ファイルが無い・読めない・
/// `APPDATA` が取れない場合はすべて黙って既定値を返す(パニックしない、
/// SPEC §26)。
pub fn load() -> Settings {
    match settings_file_path() {
        Some(path) => load_from_path(&path),
        None => Settings::default(),
    }
}

/// 終了時・最近使ったファイル更新時に呼ぶ(`app.rs`)。書き込み失敗
/// (権限・ディスク満杯等)は無視する(起動・終了を妨げない、SPEC §26)。
pub fn save(settings: &Settings) {
    if let Some(path) = settings_file_path() {
        let _ = save_to_path(&path, settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settings() -> Settings {
        let mut recent = VecDeque::new();
        recent.push_back(PathBuf::from(r"C:\Users\test\Pictures\a.png"));
        recent.push_back(PathBuf::from(r"C:\Users\test\Pictures\b.jpg"));
        Settings {
            window_width: 1600,
            window_height: 900,
            window_maximized: true,
            recent_files: recent,
            brush_size: 12.5,
            brush_hardness: 42,
            brush_opacity: 77,
            pencil_mode: true,
            brush_smoothing: 33,
            fill_tolerance: 10,
            magic_wand_tolerance: 20,
            rect_mode: ShapeMode::Fill,
            ellipse_mode: ShapeMode::Both,
            gradient_kind: GradientKind::Radial,
            gradient_colors: GradientColors::PrimaryToTransparent,
            primary: Color32::from_rgba_unmultiplied(10, 20, 30, 255),
            secondary: Color32::from_rgba_unmultiplied(200, 150, 100, 128),
            user_palette: vec![
                Color32::from_rgb(1, 2, 3),
                Color32::from_rgba_unmultiplied(4, 5, 6, 7),
            ],
            last_tool: ToolKind::Gradient,
            show_pixel_grid: false,
            max_undo_steps: 123,
        }
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let original = sample_settings();
        let text = serialize(&original);
        let parsed = parse(&text);
        assert_eq!(parsed, original);
    }

    #[test]
    fn default_round_trips_too() {
        let original = Settings::default();
        let parsed = parse(&serialize(&original));
        assert_eq!(parsed, original);
    }

    #[test]
    fn empty_text_yields_defaults() {
        let parsed = parse("");
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn unknown_keys_and_malformed_lines_are_ignored_not_fatal() {
        let text = "\
未知のキー\t何か
this line has no tab at all
window.width\tnot_a_number
brush.hardness\t999999999999999999999
color.primary\tzzzzzz
tool.rect_mode\tnonsense
\t
window.height\t900
";
        let parsed = parse(text);
        // 壊れた行は個別に無視され、パースできた行(window.height)だけ反映
        // される。壊れていない他のフィールドは既定値のまま。
        assert_eq!(parsed.window_height, 900);
        assert_eq!(parsed.window_width, Settings::default().window_width);
        assert_eq!(parsed.brush_hardness, Settings::default().brush_hardness);
        assert_eq!(parsed.primary, Settings::default().primary);
        assert_eq!(parsed.rect_mode, Settings::default().rect_mode);
    }

    #[test]
    fn recent_and_palette_lists_are_reordered_by_index_not_file_order() {
        // 手編集等で行の出現順とインデックスがずれていても、インデックス
        // 昇順で復元する。
        let text = "\
recent.1\tC:\\b.png
recent.0\tC:\\a.png
palette.1\t#00FF00
palette.0\t#FF0000
";
        let parsed = parse(text);
        assert_eq!(
            parsed.recent_files,
            VecDeque::from(vec![PathBuf::from("C:\\a.png"), PathBuf::from("C:\\b.png")])
        );
        assert_eq!(
            parsed.user_palette,
            vec![Color32::from_rgb(0xFF, 0, 0), Color32::from_rgb(0, 0xFF, 0)]
        );
    }

    #[test]
    fn recent_files_are_capped_at_max_even_if_file_has_more() {
        let mut text = String::new();
        for i in 0..(MAX_RECENT_FILES + 5) {
            text.push_str(&format!("recent.{i}\tC:\\file{i}.png\n"));
        }
        let parsed = parse(&text);
        assert_eq!(parsed.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(parsed.recent_files[0], PathBuf::from("C:\\file0.png"));
    }

    /// v4 レビューで発見・修正したバグの回帰テスト: `recent.N` は
    /// `MAX_RECENT_FILES` でキャップされるのに `palette.N` は無制限に
    /// パースされていた。破損・悪意ある `settings.txt` に大量の
    /// `palette.N` 行があると、起動時に全件を読み込み・保持してしまい、
    /// 右パネルのパレット表示(仮想化なし)が毎フレーム全件をレイアウト
    /// しようとして UI が事実上ハングする(ARCHITECTURE.md §16.10-5)。
    /// `recent.N` と対称に `MAX_USER_PALETTE` でキャップする。
    #[test]
    fn user_palette_is_capped_at_max_even_if_file_has_far_more() {
        let mut text = String::new();
        // MAX_USER_PALETTE をゆうに超える行数(壊れた/悪意あるファイルの
        // 想定)。1 万行でも「無制限」バグを再現するには十分。
        let huge_count = MAX_USER_PALETTE + 10_000;
        for i in 0..huge_count {
            text.push_str(&format!("palette.{i}\t#{i:06X}\n"));
        }
        let parsed = parse(&text);
        assert_eq!(
            parsed.user_palette.len(),
            MAX_USER_PALETTE,
            "a corrupted settings.txt with {huge_count} palette entries must not all be loaded"
        );
    }

    /// キャップは書き込み側(`serialize`)にも対称に効くこと(壊れた
    /// ファイルを一度読み込んでそのまま保存し直しても、無制限のまま
    /// 増殖しない)。
    #[test]
    fn serialize_also_caps_user_palette_at_max() {
        let settings = Settings {
            user_palette: (0..(MAX_USER_PALETTE + 50))
                .map(|i| Color32::from_rgb((i % 256) as u8, 0, 0))
                .collect(),
            ..Settings::default()
        };
        let text = serialize(&settings);
        let palette_lines = text
            .lines()
            .filter(|line| line.starts_with("palette."))
            .count();
        assert_eq!(palette_lines, MAX_USER_PALETTE);
    }

    #[test]
    fn window_dims_are_clamped_to_minimum_on_parse() {
        let text = "window.width\t10\nwindow.height\t5\n";
        let parsed = parse(text);
        assert_eq!(parsed.window_width, MIN_WINDOW_WIDTH);
        assert_eq!(parsed.window_height, MIN_WINDOW_HEIGHT);
    }

    #[test]
    fn window_dims_are_clamped_to_sane_maximum_on_parse() {
        let text = "window.width\t999999999\nwindow.height\t999999999\n";
        let parsed = parse(text);
        assert!(parsed.window_width <= MAX_WINDOW_DIMENSION);
        assert!(parsed.window_height <= MAX_WINDOW_DIMENSION);
    }

    // -- SPEC §34/ARCHITECTURE.md §18.2: 「元に戻す履歴の保持数」 -------------

    #[test]
    fn default_max_undo_steps_is_50() {
        assert_eq!(Settings::default().max_undo_steps, 50);
    }

    #[test]
    fn max_undo_steps_round_trips() {
        let text = "history.max_steps\t250\n";
        let parsed = parse(text);
        assert_eq!(parsed.max_undo_steps, 250);
        assert_eq!(
            parse(&serialize(&parsed)).max_undo_steps,
            250,
            "serialize→parse must preserve the value"
        );
    }

    #[test]
    fn max_undo_steps_is_clamped_to_1_500_on_parse() {
        let parsed = parse("history.max_steps\t0\n");
        assert_eq!(parsed.max_undo_steps, MIN_MAX_UNDO_STEPS);
        let parsed = parse("history.max_steps\t999999\n");
        assert_eq!(parsed.max_undo_steps, MAX_MAX_UNDO_STEPS);
    }

    #[test]
    fn max_undo_steps_is_clamped_on_serialize_too() {
        let settings = Settings {
            max_undo_steps: 999_999,
            ..Settings::default()
        };
        let text = serialize(&settings);
        let parsed = parse(&text);
        assert_eq!(parsed.max_undo_steps, MAX_MAX_UNDO_STEPS);
    }

    #[test]
    fn garbage_max_undo_steps_value_falls_back_to_default() {
        let parsed = parse("history.max_steps\tnot_a_number\n");
        assert_eq!(parsed.max_undo_steps, Settings::default().max_undo_steps);
    }

    #[test]
    fn nan_or_infinite_brush_size_is_rejected() {
        let text = "brush.size\tNaN\n";
        let parsed = parse(text);
        assert_eq!(parsed.brush_size, Settings::default().brush_size);
        let text = "brush.size\tinf\n";
        let parsed = parse(text);
        assert_eq!(parsed.brush_size, Settings::default().brush_size);
    }

    // -- ファイル I/O(ARCHITECTURE.md §13 の io.rs テストと同じ流儀、temp dir) --

    fn temp_dir_for(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_settings_test_{name}_{}_{}",
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
    fn save_to_path_then_load_from_path_round_trips() {
        let dir = temp_dir_for("roundtrip");
        let path = dir.join("nested").join("settings.txt");
        let original = sample_settings();

        save_to_path(&path, &original).expect("save should succeed");
        let loaded = load_from_path(&path);
        assert_eq!(loaded, original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_path_missing_file_falls_back_to_defaults_without_panicking() {
        let dir = temp_dir_for("missing");
        let path = dir.join("does_not_exist.txt");
        let loaded = load_from_path(&path);
        assert_eq!(loaded, Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_path_garbage_binary_content_falls_back_gracefully() {
        let dir = temp_dir_for("garbage");
        let path = dir.join("settings.txt");
        // 有効な UTF-8 ではないバイト列(壊れたファイルの極端な例)。
        std::fs::write(&path, [0xFFu8, 0xFE, 0x00, 0x01, 0x02]).expect("write raw bytes");
        let loaded = load_from_path(&path);
        assert_eq!(loaded, Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_to_path_creates_parent_directories() {
        let dir = temp_dir_for("mkdir");
        let path = dir.join("a").join("b").join("c").join("settings.txt");
        save_to_path(&path, &Settings::default()).expect("save should succeed");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
