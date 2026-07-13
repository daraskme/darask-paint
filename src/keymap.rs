//! ショートカットキーマップの一元化(SPEC §20、ARCHITECTURE.md §15.4)。
//!
//! v1〜v2、および v3 M1〜M3 は `app.rs` の `handle_*_shortcuts` 群が
//! それぞれ独自に `KeyboardShortcut` リテラルを内包していた。そのため
//! 「実際に消費しているキー」と「メニュー/ツールバーに表示している文字列」
//! が別々の場所に書かれており、片方だけ直し忘れる余地があった。
//! v3 M4 でこれを本モジュールの `KEYMAP` という単一のデータテーブルに
//! 集約する:
//!
//! - `app.rs::handle_shortcuts` が [`poll`] 経由でショートカットを消費する。
//! - `ui/menu.rs` / `ui/toolbar.rs` は [`label_for`] / [`labels_for`] /
//!   [`tool_shortcut_label`] を使って表示文字列を **同じテーブルから**
//!   生成する。
//!
//! この2つが同じ `KEYMAP` だけを参照することで、表記と実挙動が構造的に
//! 乖離しなくなる。
//!
//! ## スコープ外(意図的に `KEYMAP` に含めないもの)
//!
//! - **Space(手のひらの一時押下)**: 「押しっぱなし」の連続状態であり、
//!   1 回きりの押下イベントを表す `Action` にはなじまない。
//!   `canvas_view.rs` が `key_down(Key::Space)` を直接見る(従来どおり)。
//! - **テキスト編集中の Ctrl+Enter(確定)/Esc(破棄)**: 他の全ショート
//!   カットと逆に「`wants_keyboard_input` なら無効」ではなく「編集中で
//!   なければ無効」という反対のガードを持つ(ARCHITECTURE.md §15.4 ①
//!   「モーダル/テキスト編集中の除外(従来どおり)」)。`app.rs::
//!   handle_text_edit_shortcuts` が従来どおり単独で処理する。
//! - **Alt+クリック(一時スポイト)・マウスクリックそのもの**: キーボード
//!   ショートカットではない。
//! - **Alt+F4(終了)**: OS/ウィンドウマネージャが `close_requested` として
//!   通知するものであり、egui が消費するキー入力ではない
//!   (`app.rs::handle_close_request` 参照)。メニュー表記は固定文字列のまま
//!   でよい。

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::tools::ToolKind;

/// キー割り当て(修飾キー + 1 キー)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl Binding {
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }

    /// 要求している修飾キーの数(Ctrl/Cmd・Shift・Alt をそれぞれ 1 として
    /// 数える)。ARCHITECTURE.md §15.4 ②「修飾キーが多いものから先に
    /// consume」の判定に使う。
    ///
    /// egui の `consume_shortcut` は `Modifiers::matches_logically` で判定
    /// するため、パターン側が要求していない Shift/Alt は「余分」として
    /// 無視される(例: `Ctrl+Z` というパターンは実際の `Ctrl+Shift+Z` にも
    /// マッチしてしまう)。そのため、より多くの修飾キーを要求するバインド
    /// (より限定的なもの)を先に消費しないと、狙った側と違う `Action` が
    /// 誤って発火する。[`poll`] はこの値の降順でソートしてから消費する。
    fn specificity(&self) -> u32 {
        u32::from(self.modifiers.ctrl || self.modifiers.command)
            + u32::from(self.modifiers.shift)
            + u32::from(self.modifiers.alt)
    }

    fn to_shortcut(self) -> KeyboardShortcut {
        KeyboardShortcut::new(self.modifiers, self.key)
    }

    /// メニュー/ツールチップ表示用の文字列(例: `"Ctrl+Shift+S"`, `"["`)。
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.modifiers.ctrl || self.modifiers.command {
            parts.push("Ctrl");
        }
        if self.modifiers.shift {
            parts.push("Shift");
        }
        if self.modifiers.alt {
            parts.push("Alt");
        }
        // README/SPEC の表記に合わせて 2 箇所だけ上書きする:
        // - Escape は `Key::name()` だと "Escape" になるが、他ドキュメントは
        //   "Esc"。
        // - Minus は `Key::symbol_or_name()` だと egui 内部の見た目重視の
        //   全角風マイナス記号("−", U+2212)になるが、SPEC/README は
        //   半角ハイフン "-"(`Ctrl+-`)で統一している。
        let key = match self.key {
            Key::Escape => "Esc",
            Key::Minus => "-",
            other => other.symbol_or_name(),
        };
        if parts.is_empty() {
            key.to_owned()
        } else {
            format!("{}+{key}", parts.join("+"))
        }
    }
}

/// SPEC §20 で区別できる操作の一覧。`app.rs::handle_shortcuts` がこれを
/// 受け取って実際の処理へディスパッチする(値そのものは何も実行しない、
/// ARCHITECTURE.md §4 の `ToolEvent` と同じ「データとしてのイベント」の
/// 流儀)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // -- ツール(SPEC §20「ツール」) --------------------------------------
    SelectTool(ToolKind),
    /// `U`: 図形(直前に使った図形。`ToolKind::Line`/`Rect`/`Ellipse` の
    /// いずれか)。
    SelectLastShapeTool,
    /// `Shift+U`: 直線→矩形→楕円 を巡回。
    CycleShapeTool,
    /// v4 §22: `M`: 選択(直前に使ったマリキー形状。`ToolKind::Select`/
    /// `EllipseSelect` のいずれか)。`SelectLastShapeTool` の選択版。
    SelectLastMarqueeTool,
    /// v4 §22 §27: `Shift+M`: 矩形選択↔楕円選択を巡回。
    CycleMarquee,
    /// v4 §22 §27: `Shift+L`: なげなわの自由↔多角形モードを切替(`ToolKind`
    /// は変えない、`app.rs::LassoMode` を切り替えるだけ)。
    CycleLassoMode,
    /// v4 §23: `G`: 塗りつぶし系(直前に使った `ToolKind::Fill`/`Gradient`
    /// のいずれか)。`SelectLastShapeTool`/`SelectLastMarqueeTool` と同じ
    /// 「巡回はするが大半の挙動を共有する」設計(`app.rs::last_fill_tool`)。
    SelectLastFillTool,
    /// v4 §23 §27: `Shift+G`: 塗りつぶし↔グラデーションを巡回。
    CycleFillTool,

    // -- 色(SPEC §20「色」) -----------------------------------------------
    SwapColors,
    DefaultColors,
    /// 数字キー 1–9, 0 によるブラシ系ツールの不透明度設定(%)。
    SetBrushOpacity(u8),

    // -- ブラシ(SPEC §20「ブラシ」) ----------------------------------------
    BrushSizeDec,
    BrushSizeInc,
    BrushHardnessDec,
    BrushHardnessInc,

    // -- 編集(SPEC §20「編集」) --------------------------------------------
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Deselect,
    FreeTransform,
    /// Enter: 浮動片の確定(選択/移動ツール使用中のみ有効、SPEC §6/§18)。
    CommitFloating,
    /// Esc: 浮動片のキャンセル(選択/移動ツール使用中のみ有効、SPEC §18)。
    CancelFloating,
    /// v4 §24 §27: `Ctrl+U`: 色相・彩度・明度…モーダルを開く。
    HueSaturation,
    /// v4 §24 §27: `Ctrl+I`: 階調の反転(即時)。
    Invert,
    /// v4 §24 §27: `Ctrl+Shift+U`: グレースケール化(即時)。
    Grayscale,

    // -- レイヤー(SPEC §20「レイヤー」) -------------------------------------
    LayerAdd,
    LayerDuplicate,
    LayerMergeDown,
    LayerFlatten,

    // -- ファイル(SPEC §20「ファイル」) -------------------------------------
    New,
    Open,
    Save,
    SaveAs,

    // -- v5 §30/§32: タブ(ARCHITECTURE.md §17.6) ---------------------------
    /// `Ctrl+Tab`: 次のタブへ(端では先頭へ循環)。
    NextTab,
    /// `Ctrl+Shift+Tab`: 前のタブへ(端では末尾へ循環)。
    PrevTab,
    /// v5 §30/§32(V5-M3、ARCHITECTURE.md §17.4): `Ctrl+W`: アクティブタブを
    /// 閉じる(未保存なら確認ダイアログ、`app.rs::close_tab` 参照)。
    CloseTab,

    // -- 表示(SPEC §20「表示」) ---------------------------------------------
    ZoomIn,
    ZoomOut,
    Zoom100,
    FitWindow,
}

/// `KEYMAP` の 1 行。
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub binding: Binding,
    pub action: Action,
}

const fn e(modifiers: Modifiers, key: Key, action: Action) -> Entry {
    Entry {
        binding: Binding::new(modifiers, key),
        action,
    }
}

/// SPEC §20「Photoshop 準拠ショートカット(最終キーマップ)」を 1:1 で表す
/// 唯一の情報源。並び順は表示・実装上の意味を持たない(`poll` は毎回
/// 修飾キー数の降順に並べ替えてから消費する、`Binding::specificity`)。
/// ただし `label_for`/`labels_for` は複数バインドを持つ `Action`
/// (例: `Redo`)についてこの配列の登場順で文字列を連結するため、
/// SPEC の表記順(「Ctrl+Y, Ctrl+Shift+Z」)に合わせて Ctrl+Y を先に置いている。
pub const KEYMAP: &[Entry] = &[
    // -- ツール ------------------------------------------------------------
    e(Modifiers::NONE, Key::V, Action::SelectTool(ToolKind::Move)),
    // v4 §22: `M` は「直前に使ったマリキー形状」(矩形/楕円)を選ぶ
    // (`SelectLastShapeTool`/`U` と同じ設計)。`Shift+M` で 2 つを巡回する。
    e(Modifiers::NONE, Key::M, Action::SelectLastMarqueeTool),
    e(Modifiers::SHIFT, Key::M, Action::CycleMarquee),
    e(Modifiers::NONE, Key::B, Action::SelectTool(ToolKind::Pen)),
    e(
        Modifiers::NONE,
        Key::E,
        Action::SelectTool(ToolKind::Eraser),
    ),
    // v4 §23: `G` は「直前に使った塗りつぶし系ツール」(塗りつぶし/
    // グラデーション)。`Shift+G` で 2 つを巡回する(M/Shift+M・U/Shift+U と
    // 同じ設計)。
    e(Modifiers::NONE, Key::G, Action::SelectLastFillTool),
    e(Modifiers::SHIFT, Key::G, Action::CycleFillTool),
    e(
        Modifiers::NONE,
        Key::I,
        Action::SelectTool(ToolKind::Picker),
    ),
    e(Modifiers::NONE, Key::T, Action::SelectTool(ToolKind::Text)),
    e(Modifiers::NONE, Key::U, Action::SelectLastShapeTool),
    e(Modifiers::SHIFT, Key::U, Action::CycleShapeTool),
    e(Modifiers::NONE, Key::H, Action::SelectTool(ToolKind::Pan)),
    e(Modifiers::NONE, Key::Z, Action::SelectTool(ToolKind::Zoom)),
    // v4 §22 §27: なげなわ(`L`)・自動選択(`W`)。
    e(Modifiers::NONE, Key::L, Action::SelectTool(ToolKind::Lasso)),
    e(Modifiers::SHIFT, Key::L, Action::CycleLassoMode),
    e(
        Modifiers::NONE,
        Key::W,
        Action::SelectTool(ToolKind::MagicWand),
    ),
    // -- 色 ------------------------------------------------------------
    e(Modifiers::NONE, Key::X, Action::SwapColors),
    e(Modifiers::NONE, Key::D, Action::DefaultColors),
    e(Modifiers::NONE, Key::Num1, Action::SetBrushOpacity(10)),
    e(Modifiers::NONE, Key::Num2, Action::SetBrushOpacity(20)),
    e(Modifiers::NONE, Key::Num3, Action::SetBrushOpacity(30)),
    e(Modifiers::NONE, Key::Num4, Action::SetBrushOpacity(40)),
    e(Modifiers::NONE, Key::Num5, Action::SetBrushOpacity(50)),
    e(Modifiers::NONE, Key::Num6, Action::SetBrushOpacity(60)),
    e(Modifiers::NONE, Key::Num7, Action::SetBrushOpacity(70)),
    e(Modifiers::NONE, Key::Num8, Action::SetBrushOpacity(80)),
    e(Modifiers::NONE, Key::Num9, Action::SetBrushOpacity(90)),
    e(Modifiers::NONE, Key::Num0, Action::SetBrushOpacity(100)),
    // -- ブラシ ------------------------------------------------------------
    e(Modifiers::NONE, Key::OpenBracket, Action::BrushSizeDec),
    e(Modifiers::NONE, Key::CloseBracket, Action::BrushSizeInc),
    e(Modifiers::SHIFT, Key::OpenBracket, Action::BrushHardnessDec),
    e(
        Modifiers::SHIFT,
        Key::CloseBracket,
        Action::BrushHardnessInc,
    ),
    // -- 編集 ------------------------------------------------------------
    e(Modifiers::CTRL, Key::Z, Action::Undo),
    e(Modifiers::CTRL, Key::Y, Action::Redo),
    e(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Z, Action::Redo),
    e(Modifiers::CTRL, Key::X, Action::Cut),
    e(Modifiers::CTRL, Key::C, Action::Copy),
    e(Modifiers::CTRL, Key::V, Action::Paste),
    e(Modifiers::NONE, Key::Delete, Action::Delete),
    e(Modifiers::CTRL, Key::A, Action::SelectAll),
    e(Modifiers::CTRL, Key::D, Action::Deselect),
    e(Modifiers::CTRL, Key::T, Action::FreeTransform),
    e(Modifiers::NONE, Key::Enter, Action::CommitFloating),
    e(Modifiers::NONE, Key::Escape, Action::CancelFloating),
    // v4 §24 §27: 色調補正。Ctrl+Shift+U は Ctrl+U より先に消費する必要が
    // ある(ARCHITECTURE.md §15.4 ②、Ctrl+Shift+Z が Ctrl+Z より先なのと同じ
    // 理由)。`poll` が修飾キー数の降順で並べ替えるので、登場順はどちらでもよい。
    e(Modifiers::CTRL, Key::U, Action::HueSaturation),
    e(Modifiers::CTRL, Key::I, Action::Invert),
    e(
        Modifiers::CTRL.plus(Modifiers::SHIFT),
        Key::U,
        Action::Grayscale,
    ),
    // -- レイヤー ------------------------------------------------------------
    e(
        Modifiers::CTRL.plus(Modifiers::SHIFT),
        Key::N,
        Action::LayerAdd,
    ),
    e(Modifiers::CTRL, Key::J, Action::LayerDuplicate),
    e(Modifiers::CTRL, Key::E, Action::LayerMergeDown),
    e(
        Modifiers::CTRL.plus(Modifiers::SHIFT),
        Key::E,
        Action::LayerFlatten,
    ),
    // -- ファイル ------------------------------------------------------------
    e(Modifiers::CTRL, Key::N, Action::New),
    e(Modifiers::CTRL, Key::O, Action::Open),
    e(Modifiers::CTRL, Key::S, Action::Save),
    e(
        Modifiers::CTRL.plus(Modifiers::SHIFT),
        Key::S,
        Action::SaveAs,
    ),
    // -- v5 §30/§32: タブ ---------------------------------------------------
    // ARCHITECTURE.md §17.8-2: egui 自身のフォーカス移動は Tab キーの修飾
    // なし(`FocusDirection::Next`)/ Shift のみ(`FocusDirection::Previous`)
    // のときだけ発火する(egui 0.35 `memory/mod.rs` の `begin_pass` で確認
    // 済み)。Ctrl を伴うこの 2 バインドはどちらにも該当しないため、egui の
    // 既定フォーカス移動と衝突しない。
    e(Modifiers::CTRL, Key::Tab, Action::NextTab),
    e(
        Modifiers::CTRL.plus(Modifiers::SHIFT),
        Key::Tab,
        Action::PrevTab,
    ),
    // v5 §30/§32(V5-M3): `Ctrl+W`(単一キーの `W` は既に自動選択
    // `ToolKind::MagicWand` に束縛済みだが、`Ctrl` 修飾があるので衝突しない、
    // `no_two_entries_share_the_same_binding` 参照)。
    e(Modifiers::CTRL, Key::W, Action::CloseTab),
    // -- 表示 ------------------------------------------------------------
    e(Modifiers::CTRL, Key::Plus, Action::ZoomIn),
    e(Modifiers::CTRL, Key::Minus, Action::ZoomOut),
    e(Modifiers::CTRL, Key::Num1, Action::Zoom100),
    e(Modifiers::CTRL, Key::Num0, Action::FitWindow),
];

/// このフレームぶんのショートカットを消費し、発火した [`Action`] を
/// 発火順(= `KEYMAP` を修飾キー数の降順に並べ替えた順)で返す。
///
/// 呼び出し側は「モーダル表示中・テキスト入力中は無効」
/// (ARCHITECTURE.md §15.4 ①、`ctx.egui_wants_keyboard_input()` 等)を
/// **事前に**判定してから呼ぶこと。ツール/色/ブラシ/編集/レイヤー/表示/
/// ファイルの全ショートカットに共通のガードなので、ここでは持たない
/// (呼び出し側 1 箇所で判定すれば足りる、`app.rs::handle_shortcuts`)。
///
/// 修飾キーの多いものから先に `consume_shortcut` する(ARCHITECTURE.md
/// §15.4 ②)。egui は `matches_logically` で余分な Shift を無視するため、
/// 先に `Ctrl+Shift+Z` を消費しておかないと `Ctrl+Z` のパターンに誤って
/// マッチしてしまう(同じ理由で `Shift+U`/`Shift+[`/`Shift+]` も素の
/// `U`/`[`/`]` より先に消費する、ARCHITECTURE.md §15.6 落とし穴6)。
pub fn poll(ctx: &egui::Context) -> Vec<Action> {
    let mut order: Vec<usize> = (0..KEYMAP.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(KEYMAP[i].binding.specificity()));

    let mut triggered = Vec::new();
    ctx.input_mut(|input| {
        for i in order {
            let entry = &KEYMAP[i];
            if input.consume_shortcut(&entry.binding.to_shortcut()) {
                triggered.push(entry.action);
            }
        }
    });
    triggered
}

/// `action` に対応する最初のバインディング(`KEYMAP` の登場順)。
fn binding_for(action: Action) -> Option<Binding> {
    KEYMAP
        .iter()
        .find(|entry| entry.action == action)
        .map(|entry| entry.binding)
}

/// `action` に対応する全バインディング(`KEYMAP` の登場順)。`Redo` のように
/// 複数のキーが同じ操作に束縛されている場合に使う。
fn bindings_for(action: Action) -> Vec<Binding> {
    KEYMAP
        .iter()
        .filter(|entry| entry.action == action)
        .map(|entry| entry.binding)
        .collect()
}

/// `action` の表示用文字列(最初のバインディングのみ、例: `"Ctrl+Z"`)。
/// 束縛が無ければ空文字列を返す(UI 側は「サフィックスなしで表示」に
/// フォールバックできる)。
pub fn label_for(action: Action) -> String {
    binding_for(action).map(|b| b.label()).unwrap_or_default()
}

/// `action` の全バインディングをカンマ区切りにした表示用文字列
/// (例: `Redo` → `"Ctrl+Y, Ctrl+Shift+Z"`)。
pub fn labels_for(action: Action) -> String {
    bindings_for(action)
        .iter()
        .map(Binding::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `"操作名 (ショートカット)"` の形のメニュー項目文字列を組み立てる
/// (`labels_for` を使うので複数バインドも自動的に列挙される)。バインドが
/// 無い操作は `text` だけを返す(サフィックスを付けない)。
pub fn menu_label(text: &str, action: Action) -> String {
    let shortcut = labels_for(action);
    if shortcut.is_empty() {
        text.to_owned()
    } else {
        format!("{text} ({shortcut})")
    }
}

/// ツールバーのツールチップに使うショートカット表記。SPEC §20 の
/// Photoshop 準拠キーマップでは直線/矩形/楕円は「U」1 本にまとめられて
/// いる(Shift+U で巡回)ため、この 3 ツールは `SelectLastShapeTool`
/// (= `U`)のバインドを表示する。v4 §22: 矩形選択/楕円選択も同様に「M」
/// (`SelectLastMarqueeTool`)1 本にまとめる。
pub fn tool_shortcut_label(kind: ToolKind) -> String {
    let action = match kind {
        ToolKind::Line | ToolKind::Rect | ToolKind::Ellipse => Action::SelectLastShapeTool,
        ToolKind::Select | ToolKind::EllipseSelect => Action::SelectLastMarqueeTool,
        // v4 §23: 塗りつぶし/グラデーションも「G」1 本にまとまる。
        ToolKind::Fill | ToolKind::Gradient => Action::SelectLastFillTool,
        other => Action::SelectTool(other),
    };
    label_for(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ARCHITECTURE.md §15.4 の受け入れ基準:「SPEC §20 の表と 1:1 である
    /// ことをテストする(KEYMAP に SPEC の全項目が存在するか、少なくとも
    /// 件数と主要キーの静的テスト)」。
    #[test]
    fn keymap_covers_every_tool_key_in_spec_20() {
        // v4 §22: `M` は `SelectTool(Select)` ではなく `SelectLastMarqueeTool`
        // になった(`m_and_shift_m_select_and_cycle_marquee` 参照)ので、この
        // ループからは外す。v4 §23: `G` も同様に `SelectLastFillTool` になった
        // (`g_and_shift_g_select_and_cycle_fill_tool` が別途検証する)ので
        // ここでは対象外にする。
        let tools = [
            (Key::V, ToolKind::Move),
            (Key::B, ToolKind::Pen),
            (Key::E, ToolKind::Eraser),
            (Key::I, ToolKind::Picker),
            (Key::T, ToolKind::Text),
            (Key::H, ToolKind::Pan),
            (Key::Z, ToolKind::Zoom),
        ];
        for (key, kind) in tools {
            assert_eq!(
                binding_for(Action::SelectTool(kind)),
                Some(Binding::new(Modifiers::NONE, key)),
                "{kind:?} のバインドが SPEC §20 と一致しない"
            );
        }
    }

    #[test]
    fn old_r_c_f_keys_are_gone() {
        // SPEC §20: 「旧 L/R/C は廃止」。塗りつぶしも F→G に変更された。
        // v4 §22 で `L` はなげなわとして復活したので、ここでは対象外にする
        // (`l_and_shift_l_select_lasso_and_cycle_mode` が別途検証する)。
        for key in [Key::R, Key::C, Key::F] {
            assert!(
                !KEYMAP
                    .iter()
                    .any(|entry| entry.binding.modifiers == Modifiers::NONE
                        && entry.binding.key == key),
                "{key:?} が単一キーのバインドとして残っている"
            );
        }
    }

    // -- v4 §22/§27: マリキー(M/Shift+M)・なげなわ(L/Shift+L)・
    // 自動選択(W) ------------------------------------------------------

    #[test]
    fn m_and_shift_m_select_and_cycle_marquee() {
        assert_eq!(
            binding_for(Action::SelectLastMarqueeTool),
            Some(Binding::new(Modifiers::NONE, Key::M))
        );
        assert_eq!(
            binding_for(Action::CycleMarquee),
            Some(Binding::new(Modifiers::SHIFT, Key::M))
        );
    }

    #[test]
    fn l_and_shift_l_select_lasso_and_cycle_mode() {
        assert_eq!(
            binding_for(Action::SelectTool(ToolKind::Lasso)),
            Some(Binding::new(Modifiers::NONE, Key::L))
        );
        assert_eq!(
            binding_for(Action::CycleLassoMode),
            Some(Binding::new(Modifiers::SHIFT, Key::L))
        );
    }

    #[test]
    fn w_selects_magic_wand() {
        assert_eq!(
            binding_for(Action::SelectTool(ToolKind::MagicWand)),
            Some(Binding::new(Modifiers::NONE, Key::W))
        );
    }

    // -- v4 §23/§27: 塗りつぶし↔グラデーション(G/Shift+G) ---------------------

    #[test]
    fn g_and_shift_g_select_and_cycle_fill_tool() {
        assert_eq!(
            binding_for(Action::SelectLastFillTool),
            Some(Binding::new(Modifiers::NONE, Key::G))
        );
        assert_eq!(
            binding_for(Action::CycleFillTool),
            Some(Binding::new(Modifiers::SHIFT, Key::G))
        );
    }

    #[test]
    fn tool_shortcut_label_groups_fill_tools_under_g() {
        assert_eq!(tool_shortcut_label(ToolKind::Fill), "G");
        assert_eq!(tool_shortcut_label(ToolKind::Gradient), "G");
    }

    // -- v4 §24/§27: 色調補正(Ctrl+U / Ctrl+I / Ctrl+Shift+U) -----------------

    #[test]
    fn tone_adjustment_keys_match_spec_24_27() {
        assert_eq!(
            binding_for(Action::HueSaturation),
            Some(Binding::new(Modifiers::CTRL, Key::U))
        );
        assert_eq!(
            binding_for(Action::Invert),
            Some(Binding::new(Modifiers::CTRL, Key::I))
        );
        assert_eq!(
            binding_for(Action::Grayscale),
            Some(Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::U))
        );
    }

    #[test]
    fn ctrl_shift_u_takes_priority_over_ctrl_u() {
        // ARCHITECTURE.md §15.4 ②(Ctrl+Shift+Z が Ctrl+Z より先なのと同じ
        // 理由): Ctrl+Shift+U の specificity が Ctrl+U より高いこと。
        let ctrl_u = Binding::new(Modifiers::CTRL, Key::U).specificity();
        let ctrl_shift_u =
            Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::U).specificity();
        assert!(ctrl_shift_u > ctrl_u);
    }

    #[test]
    fn tool_shortcut_label_groups_marquee_tools_under_m() {
        assert_eq!(tool_shortcut_label(ToolKind::Select), "M");
        assert_eq!(tool_shortcut_label(ToolKind::EllipseSelect), "M");
        assert_eq!(tool_shortcut_label(ToolKind::Lasso), "L");
        assert_eq!(tool_shortcut_label(ToolKind::MagicWand), "W");
    }

    #[test]
    fn u_and_shift_u_select_and_cycle_shapes() {
        assert_eq!(
            binding_for(Action::SelectLastShapeTool),
            Some(Binding::new(Modifiers::NONE, Key::U))
        );
        assert_eq!(
            binding_for(Action::CycleShapeTool),
            Some(Binding::new(Modifiers::SHIFT, Key::U))
        );
    }

    #[test]
    fn color_keys_match_spec_20() {
        assert_eq!(
            binding_for(Action::SwapColors),
            Some(Binding::new(Modifiers::NONE, Key::X))
        );
        assert_eq!(
            binding_for(Action::DefaultColors),
            Some(Binding::new(Modifiers::NONE, Key::D))
        );
        assert_eq!(
            binding_for(Action::SetBrushOpacity(100)),
            Some(Binding::new(Modifiers::NONE, Key::Num0))
        );
        for (key, pct) in [
            (Key::Num1, 10),
            (Key::Num2, 20),
            (Key::Num3, 30),
            (Key::Num4, 40),
            (Key::Num5, 50),
            (Key::Num6, 60),
            (Key::Num7, 70),
            (Key::Num8, 80),
            (Key::Num9, 90),
        ] {
            assert_eq!(
                binding_for(Action::SetBrushOpacity(pct)),
                Some(Binding::new(Modifiers::NONE, key))
            );
        }
    }

    #[test]
    fn brush_size_and_hardness_keys_match_spec_20() {
        assert_eq!(
            binding_for(Action::BrushSizeDec),
            Some(Binding::new(Modifiers::NONE, Key::OpenBracket))
        );
        assert_eq!(
            binding_for(Action::BrushSizeInc),
            Some(Binding::new(Modifiers::NONE, Key::CloseBracket))
        );
        assert_eq!(
            binding_for(Action::BrushHardnessDec),
            Some(Binding::new(Modifiers::SHIFT, Key::OpenBracket))
        );
        assert_eq!(
            binding_for(Action::BrushHardnessInc),
            Some(Binding::new(Modifiers::SHIFT, Key::CloseBracket))
        );
    }

    #[test]
    fn edit_keys_match_spec_20() {
        assert_eq!(
            binding_for(Action::Undo),
            Some(Binding::new(Modifiers::CTRL, Key::Z))
        );
        assert_eq!(
            bindings_for(Action::Redo),
            vec![
                Binding::new(Modifiers::CTRL, Key::Y),
                Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Z),
            ]
        );
        assert_eq!(
            binding_for(Action::Cut),
            Some(Binding::new(Modifiers::CTRL, Key::X))
        );
        assert_eq!(
            binding_for(Action::Copy),
            Some(Binding::new(Modifiers::CTRL, Key::C))
        );
        assert_eq!(
            binding_for(Action::Paste),
            Some(Binding::new(Modifiers::CTRL, Key::V))
        );
        assert_eq!(
            binding_for(Action::Delete),
            Some(Binding::new(Modifiers::NONE, Key::Delete))
        );
        assert_eq!(
            binding_for(Action::SelectAll),
            Some(Binding::new(Modifiers::CTRL, Key::A))
        );
        assert_eq!(
            binding_for(Action::Deselect),
            Some(Binding::new(Modifiers::CTRL, Key::D))
        );
        assert_eq!(
            binding_for(Action::FreeTransform),
            Some(Binding::new(Modifiers::CTRL, Key::T))
        );
        assert_eq!(
            binding_for(Action::CommitFloating),
            Some(Binding::new(Modifiers::NONE, Key::Enter))
        );
        assert_eq!(
            binding_for(Action::CancelFloating),
            Some(Binding::new(Modifiers::NONE, Key::Escape))
        );
    }

    #[test]
    fn layer_keys_match_spec_20() {
        assert_eq!(
            binding_for(Action::LayerAdd),
            Some(Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::N))
        );
        assert_eq!(
            binding_for(Action::LayerDuplicate),
            Some(Binding::new(Modifiers::CTRL, Key::J))
        );
        assert_eq!(
            binding_for(Action::LayerMergeDown),
            Some(Binding::new(Modifiers::CTRL, Key::E))
        );
        assert_eq!(
            binding_for(Action::LayerFlatten),
            Some(Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::E))
        );
    }

    #[test]
    fn file_and_view_keys_match_spec_20() {
        assert_eq!(
            binding_for(Action::New),
            Some(Binding::new(Modifiers::CTRL, Key::N))
        );
        assert_eq!(
            binding_for(Action::Open),
            Some(Binding::new(Modifiers::CTRL, Key::O))
        );
        assert_eq!(
            binding_for(Action::Save),
            Some(Binding::new(Modifiers::CTRL, Key::S))
        );
        assert_eq!(
            binding_for(Action::SaveAs),
            Some(Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::S))
        );
        assert_eq!(
            binding_for(Action::ZoomIn),
            Some(Binding::new(Modifiers::CTRL, Key::Plus))
        );
        assert_eq!(
            binding_for(Action::ZoomOut),
            Some(Binding::new(Modifiers::CTRL, Key::Minus))
        );
        assert_eq!(
            binding_for(Action::Zoom100),
            Some(Binding::new(Modifiers::CTRL, Key::Num1))
        );
        assert_eq!(
            binding_for(Action::FitWindow),
            Some(Binding::new(Modifiers::CTRL, Key::Num0))
        );
    }

    // -- v5 §30/§32: タブ(Ctrl+Tab / Ctrl+Shift+Tab) -----------------------

    #[test]
    fn tab_switch_keys_match_spec_30_32() {
        assert_eq!(
            binding_for(Action::NextTab),
            Some(Binding::new(Modifiers::CTRL, Key::Tab))
        );
        assert_eq!(
            binding_for(Action::PrevTab),
            Some(Binding::new(
                Modifiers::CTRL.plus(Modifiers::SHIFT),
                Key::Tab
            ))
        );
    }

    #[test]
    fn close_tab_key_matches_spec_30_32() {
        assert_eq!(
            binding_for(Action::CloseTab),
            Some(Binding::new(Modifiers::CTRL, Key::W))
        );
    }

    #[test]
    fn ctrl_shift_tab_takes_priority_over_ctrl_tab() {
        // ARCHITECTURE.md §15.4 ②と同じ理由(Ctrl+Shift+Z が Ctrl+Z より先の
        // ケースと同型): `poll` が specificity 降順で consume するため、
        // Ctrl+Shift+Tab は Ctrl+Tab のパターンに誤ってマッチする前に
        // 消費される必要がある。
        let ctrl_tab = Binding::new(Modifiers::CTRL, Key::Tab).specificity();
        let ctrl_shift_tab =
            Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Tab).specificity();
        assert!(ctrl_shift_tab > ctrl_tab);
    }

    /// SPEC §20 の全項目数(ツール9 + 色4(X/D/数字10種をまとめて1項目と
    /// 数えず個別カウント) …ここでは「表の行」ではなく「区別できる実際の
    /// キー割り当て数」で数える: 数字キーは 10 個で 10 バインド)。
    #[test]
    fn keymap_entry_count_matches_spec_20() {
        // ツール11(V,M,B,E,G,I,T,U,Shift+U,H,Z) + 色2(X,D) +
        // 数字10 + ブラシ4([,],Shift+[,Shift+]) + 編集11(Undo,Redo×2,
        // Cut,Copy,Paste,Delete,SelectAll,Deselect,FreeTransform,
        // CommitFloating,CancelFloating) + レイヤー4 + ファイル4 + 表示4
        // = 11 + 2 + 10 + 4 + 12 + 4 + 4 + 4 = 51 (v1〜v3)
        // v4 §22/§27 で 4 件追加: Shift+M(CycleMarquee) + L(Lasso) +
        // Shift+L(CycleLassoMode) + W(MagicWand) = 51 + 4 = 55。
        // v4 §23〜§24/§27 でさらに 4 件追加: Shift+G(CycleFillTool、G 自体は
        // `SelectTool(Fill)` → `SelectLastFillTool` に置き換わっただけで
        // バインド数は変わらない) + Ctrl+U(HueSaturation) +
        // Ctrl+I(Invert) + Ctrl+Shift+U(Grayscale) = 55 + 4 = 59。
        // v5 §30/§32(V5-M2)でさらに 2 件追加: Ctrl+Tab(NextTab) +
        // Ctrl+Shift+Tab(PrevTab) = 59 + 2 = 61。
        // v5 §30/§32(V5-M3)でさらに 1 件追加: Ctrl+W(CloseTab) = 61 + 1 = 62。
        assert_eq!(KEYMAP.len(), 62);
    }

    #[test]
    fn no_two_entries_share_the_same_binding() {
        for (i, a) in KEYMAP.iter().enumerate() {
            for b in &KEYMAP[i + 1..] {
                assert_ne!(
                    (a.binding.modifiers, a.binding.key),
                    (b.binding.modifiers, b.binding.key),
                    "重複バインド: {:?}",
                    a.binding
                );
            }
        }
    }

    #[test]
    fn poll_prioritizes_more_specific_bindings_by_specificity_score() {
        let bare_u = Binding::new(Modifiers::NONE, Key::U).specificity();
        let shift_u = Binding::new(Modifiers::SHIFT, Key::U).specificity();
        assert!(shift_u > bare_u);

        let ctrl_z = Binding::new(Modifiers::CTRL, Key::Z).specificity();
        let ctrl_shift_z =
            Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Z).specificity();
        assert!(ctrl_shift_z > ctrl_z);
    }

    #[test]
    fn label_formats_modifiers_and_symbol_keys() {
        assert_eq!(Binding::new(Modifiers::NONE, Key::U).label(), "U");
        assert_eq!(Binding::new(Modifiers::SHIFT, Key::U).label(), "Shift+U");
        assert_eq!(Binding::new(Modifiers::CTRL, Key::Z).label(), "Ctrl+Z");
        assert_eq!(
            Binding::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::S).label(),
            "Ctrl+Shift+S"
        );
        assert_eq!(Binding::new(Modifiers::NONE, Key::OpenBracket).label(), "[");
        assert_eq!(Binding::new(Modifiers::NONE, Key::Escape).label(), "Esc");
        assert_eq!(Binding::new(Modifiers::CTRL, Key::Minus).label(), "Ctrl+-");
        assert_eq!(Binding::new(Modifiers::CTRL, Key::Plus).label(), "Ctrl++");
    }

    #[test]
    fn labels_for_redo_lists_both_bindings_in_spec_order() {
        assert_eq!(labels_for(Action::Redo), "Ctrl+Y, Ctrl+Shift+Z");
    }

    #[test]
    fn menu_label_appends_shortcut_suffix() {
        assert_eq!(menu_label("元に戻す", Action::Undo), "元に戻す (Ctrl+Z)");
    }

    #[test]
    fn tool_shortcut_label_groups_shape_tools_under_u() {
        assert_eq!(tool_shortcut_label(ToolKind::Line), "U");
        assert_eq!(tool_shortcut_label(ToolKind::Rect), "U");
        assert_eq!(tool_shortcut_label(ToolKind::Ellipse), "U");
        assert_eq!(tool_shortcut_label(ToolKind::Pen), "B");
    }
}
