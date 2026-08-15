//! v12 §58: ドッキングパネルの**配置モデル**(SPEC §58、ARCHITECTURE.md
//! §22.6b)。
//!
//! 「色 / レイヤー / 履歴(将来 §54 のページ)」を右パネル固定ではなく
//! **右ドック / 左ドック / フローティング**の 3 状態で配置できるようにする
//! ための、egui に依存しない純粋なデータ構造とその往復シリアライズ。
//! 実際の描画(ドック・`egui::Window`・ヘッダの DnD)は `ui/side_panel.rs`。
//!
//! # 設計方針
//!
//! - パネルの一覧は `PanelKind::ALL` 1 か所だけを情報源にする。将来
//!   「ページ」パネルを足すときは `PanelKind` に 1 行、`ALL`/`title`/`tag`
//!   に 1 行ずつ足せば、配置・並べ替え・永続化・DnD はすべて追随する
//!   (`ui/side_panel.rs` 側も `PanelKind::ALL` と本体描画の match だけ)。
//! - ドック内の順序は `order`(0 始まりの連番)で表し、**あらゆる変更操作の
//!   直後に 0..n へ振り直す**(`renumber`)。これにより
//!   `parse(serialize(x)) == x` が常に成立する(往復テスト必須 —
//!   ARCHITECTURE.md §16.7 の流儀)。
//! - 設定ファイルの破損・欠損に対しては SPEC §26 の「黙って既定値」を守る
//!   (`parse` はどんな入力でもパニックせず、読めた項目だけ反映する)。

use eframe::egui::{pos2, vec2, Pos2, Rect, Vec2};

/// SPEC §3/§58: ドックの固定幅(約 210px)。左右どちらのドックも同じ幅。
pub const DOCK_WIDTH: f32 = 210.0;

/// フローティング時の既定寸法と、破損値をクランプする範囲。
pub const DEFAULT_FLOAT_SIZE: Vec2 = vec2(DOCK_WIDTH, 300.0);
pub const MIN_FLOAT_SIZE: Vec2 = vec2(150.0, 56.0);
/// 壊れた設定ファイルの巨大値から守るための上限(`settings.rs` の
/// `MAX_WINDOW_DIMENSION` と同じ趣旨。SPEC が定める値ではない)。
pub const MAX_FLOAT_SIZE: Vec2 = vec2(4000.0, 4000.0);

/// 既定のフローティング位置(パネルごとに少しずつずらして重ならないように
/// する)。ドックから「フローティング化」した直後の初期位置に使う。
const FLOAT_CASCADE_ORIGIN: Pos2 = pos2(180.0, 120.0);
const FLOAT_CASCADE_STEP: f32 = 28.0;

/// パネルの種類(SPEC §58: 「色 / レイヤー / 履歴 / ページ」)。
///
/// ページ(§54)は Phase 5 で追加する — そのときはここに 1 行足すだけで
/// 配置・永続化・DnD が動く(モジュールドキュメントコメント参照)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Color,
    Layers,
    History,
}

/// `PanelKind` の総数(配列添字に使う)。
pub const PANEL_COUNT: usize = 3;

impl PanelKind {
    /// 既定の並び(SPEC §58: 「色→レイヤー→履歴」)。UI・永続化の反復は
    /// すべてこの配列を通す。
    pub const ALL: [PanelKind; PANEL_COUNT] =
        [PanelKind::Color, PanelKind::Layers, PanelKind::History];

    /// ヘッダに出す日本語名(UI 表示専用)。
    pub fn title(self) -> &'static str {
        match self {
            Self::Color => "色",
            Self::Layers => "レイヤー",
            Self::History => "履歴",
        }
    }

    /// 設定ファイル・`egui::Id` 用の安定した識別子(表示名 `title` とは
    /// 独立させる — 表示文言を変えても保存済みの設定と互換を保つ、
    /// `settings.rs` の `tool_kind_tag` と同じ流儀)。
    pub fn tag(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Layers => "layers",
            Self::History => "history",
        }
    }

    /// `PanelLayout` の配列添字(= `ALL` 内の位置)。
    fn index(self) -> usize {
        match self {
            Self::Color => 0,
            Self::Layers => 1,
            Self::History => 2,
        }
    }
}

/// ドックの左右(SPEC §58: 「左ドックはツールバーの右隣」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockSide {
    Right,
    Left,
}

impl DockSide {
    /// egui のパネル宣言順(左→右→中央)に合わせた反復順
    /// (ARCHITECTURE.md §22.6b 落とし穴 1)。
    pub const DECLARATION_ORDER: [DockSide; 2] = [DockSide::Left, DockSide::Right];

    fn tag(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "right" => Self::Right,
            "left" => Self::Left,
            _ => return None,
        })
    }

    fn other(self) -> Self {
        match self {
            Self::Right => Self::Left,
            Self::Left => Self::Right,
        }
    }
}

/// 1 つのパネルの配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelPlacement {
    /// 左右いずれかのドックの中(`order` は同じドック内の 0 始まり連番)。
    Dock { side: DockSide, order: usize },
    /// 独立ウィンドウ(`egui::Window`)。位置・寸法はユーザー操作の確定値を
    /// 書き戻す(`ui/side_panel.rs`)。
    Floating { pos: Pos2, size: Vec2 },
}

/// ヘッダの「▾」メニュー(および右クリックメニュー)で選べる移動先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMove {
    Dock(DockSide),
    Float,
}

/// 1 つのパネルの状態(配置+折りたたみ)。折りたたみは配置を変えても
/// 維持される(SPEC §58: 「折りたたみは全状態で従来どおり」)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelEntry {
    pub placement: PanelPlacement,
    pub collapsed: bool,
}

/// 全パネルの配置(SPEC §58 の永続化対象そのもの)。
#[derive(Debug, Clone, PartialEq)]
pub struct PanelLayout {
    entries: [PanelEntry; PANEL_COUNT],
}

impl Default for PanelLayout {
    /// SPEC §58: 「既定は全パネル右ドック(順: 色→レイヤー→履歴)」。
    fn default() -> Self {
        let mut entries = [PanelEntry {
            placement: PanelPlacement::Dock {
                side: DockSide::Right,
                order: 0,
            },
            collapsed: false,
        }; PANEL_COUNT];
        for (order, kind) in PanelKind::ALL.iter().enumerate() {
            entries[kind.index()].placement = PanelPlacement::Dock {
                side: DockSide::Right,
                order,
            };
        }
        Self { entries }
    }
}

impl PanelLayout {
    pub fn placement(&self, kind: PanelKind) -> PanelPlacement {
        self.entries[kind.index()].placement
    }

    pub fn collapsed(&self, kind: PanelKind) -> bool {
        self.entries[kind.index()].collapsed
    }

    pub fn toggle_collapsed(&mut self, kind: PanelKind) {
        let entry = &mut self.entries[kind.index()];
        entry.collapsed = !entry.collapsed;
    }

    /// `side` のドックにあるパネルを**表示順**(order 昇順、同点は
    /// `PanelKind::ALL` の順)で返す。
    pub fn docked(&self, side: DockSide) -> Vec<PanelKind> {
        let mut items: Vec<(usize, usize, PanelKind)> = PanelKind::ALL
            .iter()
            .filter_map(|&kind| match self.placement(kind) {
                PanelPlacement::Dock { side: s, order } if s == side => {
                    Some((order, kind.index(), kind))
                }
                _ => None,
            })
            .collect();
        items.sort_by_key(|&(order, index, _)| (order, index));
        items.into_iter().map(|(_, _, kind)| kind).collect()
    }

    /// フローティング中のパネル(`PanelKind::ALL` の順)。
    pub fn floating(&self) -> Vec<PanelKind> {
        PanelKind::ALL
            .iter()
            .copied()
            .filter(|&kind| matches!(self.placement(kind), PanelPlacement::Floating { .. }))
            .collect()
    }

    /// SPEC §58: 「右ドックが空なら右パネル自体を出さない」。
    pub fn is_dock_empty(&self, side: DockSide) -> bool {
        self.docked(side).is_empty()
    }

    /// 表示順スロット `slot`(0..=件数)へドッキングする。
    ///
    /// 同じドックの元の位置へ落とした場合(自分の直前・直後)は何もせず
    /// `false` を返す(SPEC §50.1 のレイヤー DnD と同じ no-op 規則)。
    pub fn dock_at_slot(&mut self, kind: PanelKind, side: DockSide, slot: usize) -> bool {
        let mut list = self.docked(side);
        match list.iter().position(|&k| k == kind) {
            Some(current) => {
                if slot == current || slot == current + 1 {
                    return false;
                }
                list.remove(current);
                let insert = if slot > current { slot - 1 } else { slot };
                list.insert(insert.min(list.len()), kind);
            }
            None => list.insert(slot.min(list.len()), kind),
        }
        for (order, &k) in list.iter().enumerate() {
            self.entries[k.index()].placement = PanelPlacement::Dock { side, order };
        }
        // 元居たドック(反対側)から抜けた場合の連番詰め直し。
        self.renumber(side.other());
        true
    }

    /// フローティング化(ドロップ位置・寸法を指定)。
    pub fn float_at(&mut self, kind: PanelKind, pos: Pos2, size: Vec2) {
        self.entries[kind.index()].placement = PanelPlacement::Floating {
            pos: sane_pos(pos).unwrap_or_else(|| default_float_pos(kind)),
            size: clamp_float_size(size),
        };
        for side in DockSide::DECLARATION_ORDER {
            self.renumber(side);
        }
    }

    /// egui が確定させたウィンドウ位置・寸法の書き戻し(毎フレーム)。
    /// フローティングでないパネルには何もしない。
    pub fn set_floating_rect(&mut self, kind: PanelKind, pos: Pos2, size: Vec2) {
        if let PanelPlacement::Floating {
            pos: old_pos,
            size: old_size,
        } = self.entries[kind.index()].placement
        {
            self.entries[kind.index()].placement = PanelPlacement::Floating {
                pos: sane_pos(pos).unwrap_or(old_pos),
                size: sane_size(size).map(clamp_float_size).unwrap_or(old_size),
            };
        }
    }

    /// ヘッダの「▾」メニューからの移動(ドラッグ操作の代替、SPEC §58)。
    /// ドックへの移動はそのドックの**末尾**へ、フローティング化は既定の
    /// 位置・寸法(既にフローティングなら現状維持)。
    pub fn apply_move(&mut self, kind: PanelKind, mv: PanelMove) {
        match mv {
            PanelMove::Dock(side) => {
                let slot = self.docked(side).len();
                self.dock_at_slot(kind, side, slot);
            }
            PanelMove::Float => {
                if matches!(self.placement(kind), PanelPlacement::Floating { .. }) {
                    return;
                }
                self.float_at(kind, default_float_pos(kind), DEFAULT_FLOAT_SIZE);
            }
        }
    }

    /// 表示メニューの「パネル配置をリセット」(SPEC §58)。
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// SPEC §58: 「フローティング座標が画面外になった場合は表示範囲内へ
    /// クランプして復元」。復元時(起動後の最初のフレーム)に 1 回だけ呼ぶ
    /// — 実行中の位置は egui の `constrain` が面倒を見る
    /// (ARCHITECTURE.md §22.6b 落とし穴 3)。
    ///
    /// 画面矩形がまだ確定していない(退化している)場合は**何もせず
    /// `false`** を返す。呼び出し側(`app.rs`)は `true` を受け取るまで
    /// 「復元済み」フラグを落としてはいけない — 1 フレーム目の
    /// `content_rect` が `Rect::NOTHING` だったときにクランプが永久に
    /// スキップされる、というレビュー指摘への対応。
    #[must_use]
    pub fn clamp_floating_to_screen(&mut self, screen: Rect) -> bool {
        if !screen.is_finite() || screen.width() <= 0.0 || screen.height() <= 0.0 {
            return false;
        }
        for kind in PanelKind::ALL {
            let PanelPlacement::Floating { pos, size } = self.placement(kind) else {
                continue;
            };
            let size = vec2(
                size.x.min(screen.width()).max(MIN_FLOAT_SIZE.x),
                size.y.min(screen.height()).max(MIN_FLOAT_SIZE.y),
            );
            // 収まるなら全体を画面内へ、収まらないなら左上を画面内へ。
            let pos = pos2(
                pos.x
                    .clamp(screen.left(), (screen.right() - size.x).max(screen.left())),
                pos.y
                    .clamp(screen.top(), (screen.bottom() - size.y).max(screen.top())),
            );
            self.entries[kind.index()].placement = PanelPlacement::Floating { pos, size };
        }
        true
    }

    /// `side` のドック内の `order` を表示順のまま 0..n へ振り直す。
    fn renumber(&mut self, side: DockSide) {
        for (order, kind) in self.docked(side).into_iter().enumerate() {
            self.entries[kind.index()].placement = PanelPlacement::Dock { side, order };
        }
    }
}

/// 既定のフローティング位置(パネルごとにずらす)。
fn default_float_pos(kind: PanelKind) -> Pos2 {
    let step = FLOAT_CASCADE_STEP * kind.index() as f32;
    pos2(FLOAT_CASCADE_ORIGIN.x + step, FLOAT_CASCADE_ORIGIN.y + step)
}

/// NaN/∞ を弾く(壊れた設定ファイル・異常な入力からの防御)。
fn sane_pos(pos: Pos2) -> Option<Pos2> {
    (pos.x.is_finite() && pos.y.is_finite()).then_some(pos)
}

fn sane_size(size: Vec2) -> Option<Vec2> {
    (size.x.is_finite() && size.y.is_finite()).then_some(size)
}

fn clamp_float_size(size: Vec2) -> Vec2 {
    vec2(
        size.x.clamp(MIN_FLOAT_SIZE.x, MAX_FLOAT_SIZE.x),
        size.y.clamp(MIN_FLOAT_SIZE.y, MAX_FLOAT_SIZE.y),
    )
}

// ---------------------------------------------------------------------------
// 設定ファイルとの往復(SPEC §26 追加分、ARCHITECTURE.md §22.6b)
//   panel.<kind>.place   = right | left | float
//   panel.<kind>.order   = ドック内の並び順
//   panel.<kind>.x|y|w|h = フローティングの位置・寸法
//   panel.<kind>.collapsed = 0 | 1
// ---------------------------------------------------------------------------

/// `settings.rs` が分解済みの `(キー, 値)` 列から配置を組み立てる。
/// 不正・欠損はその項目だけ既定値へ落とす(SPEC §58/§26)。
pub fn parse(entries: &[(&str, &str)]) -> PanelLayout {
    let mut layout = PanelLayout::default();
    for kind in PanelKind::ALL {
        let value = |suffix: &str| -> Option<&str> {
            let key = format!("panel.{}.{suffix}", kind.tag());
            // 同じキーが複数あれば最後の行を採用する(`settings.rs` の
            // 単一値キーと同じ「後勝ち」)。
            entries
                .iter()
                .rev()
                .find(|(k, _)| *k == key)
                .map(|&(_, v)| v)
        };

        let placement = match value("place").and_then(placement_kind_from_tag) {
            Some(PlaceTag::Dock(side)) => {
                let order = value("order")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or_else(|| kind.index());
                PanelPlacement::Dock { side, order }
            }
            Some(PlaceTag::Float) => {
                let coord = |suffix: &str| value(suffix).and_then(|v| v.parse::<f32>().ok());
                let default_pos = default_float_pos(kind);
                let pos = pos2(
                    coord("x")
                        .filter(|v| v.is_finite())
                        .unwrap_or(default_pos.x),
                    coord("y")
                        .filter(|v| v.is_finite())
                        .unwrap_or(default_pos.y),
                );
                let size = clamp_float_size(vec2(
                    coord("w")
                        .filter(|v| v.is_finite())
                        .unwrap_or(DEFAULT_FLOAT_SIZE.x),
                    coord("h")
                        .filter(|v| v.is_finite())
                        .unwrap_or(DEFAULT_FLOAT_SIZE.y),
                ));
                PanelPlacement::Floating { pos, size }
            }
            // place 自体が無い・読めない → 既定配置(SPEC §58)。
            None => layout.placement(kind),
        };
        let collapsed = value("collapsed").map(|v| v == "1").unwrap_or(false);
        layout.entries[kind.index()] = PanelEntry {
            placement,
            collapsed,
        };
    }
    // 手編集・破損で order が重複/歯抜けでも、表示順を保ったまま 0..n の
    // 連番へ直す(往復の正規形)。
    for side in DockSide::DECLARATION_ORDER {
        layout.renumber(side);
    }
    layout
}

enum PlaceTag {
    Dock(DockSide),
    Float,
}

fn placement_kind_from_tag(tag: &str) -> Option<PlaceTag> {
    if tag == "float" {
        return Some(PlaceTag::Float);
    }
    DockSide::from_tag(tag).map(PlaceTag::Dock)
}

/// `parse` の逆。`settings.rs::serialize` が末尾へ連結する `キー\t値\n` 群。
pub fn serialize(layout: &PanelLayout) -> String {
    let mut out = String::new();
    let mut push = |key: String, value: String| {
        out.push_str(&key);
        out.push('\t');
        out.push_str(&value);
        out.push('\n');
    };
    for kind in PanelKind::ALL {
        let prefix = format!("panel.{}", kind.tag());
        match layout.placement(kind) {
            PanelPlacement::Dock { side, order } => {
                push(format!("{prefix}.place"), side.tag().to_string());
                push(format!("{prefix}.order"), order.to_string());
            }
            PanelPlacement::Floating { pos, size } => {
                push(format!("{prefix}.place"), "float".to_string());
                push(format!("{prefix}.x"), pos.x.to_string());
                push(format!("{prefix}.y"), pos.y.to_string());
                push(format!("{prefix}.w"), size.x.to_string());
                push(format!("{prefix}.h"), size.y.to_string());
            }
        }
        push(
            format!("{prefix}.collapsed"),
            if layout.collapsed(kind) { "1" } else { "0" }.to_string(),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// DnD の挿入位置(`ui/layers_panel.rs::insertion_slot` と同じ流儀の純関数)
// ---------------------------------------------------------------------------

/// ドック内のパネル塊(ヘッダ+本体)の矩形列とポインタの y から、
/// 「何番目の直前へ挿入するか」(0..=件数)を求める。
pub fn insertion_slot(blocks: &[Rect], pointer_y: f32) -> usize {
    blocks
        .iter()
        .position(|rect| pointer_y < rect.center().y)
        .unwrap_or(blocks.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries_of(text: &str) -> Vec<(&str, &str)> {
        text.lines()
            .filter_map(|line| line.split_once('\t'))
            .collect()
    }

    fn round_trip(layout: &PanelLayout) -> PanelLayout {
        let text = serialize(layout);
        parse(&entries_of(&text))
    }

    #[test]
    fn default_layout_is_all_right_docked_in_spec_order() {
        let layout = PanelLayout::default();
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::Color, PanelKind::Layers, PanelKind::History],
            "SPEC §58: 既定は全部右ドック・色→レイヤー→履歴"
        );
        assert!(layout.is_dock_empty(DockSide::Left));
        assert!(layout.floating().is_empty());
        for kind in PanelKind::ALL {
            assert!(!layout.collapsed(kind));
        }
    }

    #[test]
    fn default_round_trips() {
        let layout = PanelLayout::default();
        assert_eq!(round_trip(&layout), layout);
    }

    #[test]
    fn mixed_placements_round_trip() {
        let mut layout = PanelLayout::default();
        layout.dock_at_slot(PanelKind::History, DockSide::Left, 0);
        layout.float_at(PanelKind::Color, pos2(321.5, 88.25), vec2(240.0, 410.5));
        layout.toggle_collapsed(PanelKind::Layers);
        assert_eq!(round_trip(&layout), layout);
    }

    #[test]
    fn empty_or_unrelated_settings_yield_the_default_layout() {
        assert_eq!(parse(&[]), PanelLayout::default());
        let text = "window.width\t1280\nbrush.size\t4\n";
        assert_eq!(parse(&entries_of(text)), PanelLayout::default());
    }

    #[test]
    fn invalid_place_tag_falls_back_to_the_default_placement() {
        let text = "panel.color.place\tnonsense\npanel.color.order\t9\n";
        let layout = parse(&entries_of(text));
        assert_eq!(layout, PanelLayout::default());
    }

    #[test]
    fn malformed_float_geometry_falls_back_to_defaults_without_panicking() {
        let text = "\
panel.color.place\tfloat
panel.color.x\tNaN
panel.color.y\t
panel.color.w\tnot_a_number
panel.color.h\tinf
";
        let layout = parse(&entries_of(text));
        let PanelPlacement::Floating { pos, size } = layout.placement(PanelKind::Color) else {
            panic!("place=float は維持されるべき");
        };
        assert_eq!(pos, default_float_pos(PanelKind::Color));
        assert_eq!(size, DEFAULT_FLOAT_SIZE);
    }

    #[test]
    fn float_size_is_clamped_on_parse() {
        let text = "\
panel.history.place\tfloat
panel.history.x\t10
panel.history.y\t10
panel.history.w\t1
panel.history.h\t999999
";
        let layout = parse(&entries_of(text));
        let PanelPlacement::Floating { size, .. } = layout.placement(PanelKind::History) else {
            panic!("expected floating");
        };
        assert_eq!(size.x, MIN_FLOAT_SIZE.x);
        assert_eq!(size.y, MAX_FLOAT_SIZE.y);
    }

    #[test]
    fn duplicate_or_sparse_orders_are_normalized_to_0_n() {
        let text = "\
panel.color.place\tright
panel.color.order\t7
panel.layers.place\tright
panel.layers.order\t7
panel.history.place\tright
panel.history.order\t0
";
        let layout = parse(&entries_of(text));
        // order 0 の履歴が先頭、同点の色/レイヤーは ALL の順で続く。
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::History, PanelKind::Color, PanelKind::Layers]
        );
        for (expected, kind) in layout.docked(DockSide::Right).iter().enumerate().map(
            |(i, &k)| (i, k), // (order, kind)
        ) {
            match layout.placement(kind) {
                PanelPlacement::Dock { order, .. } => assert_eq!(order, expected),
                other => panic!("unexpected placement: {other:?}"),
            }
        }
        assert_eq!(round_trip(&layout), layout, "正規化後は往復で不変");
    }

    #[test]
    fn later_duplicate_keys_win() {
        let text = "panel.color.place\tleft\npanel.color.place\tright\n";
        let layout = parse(&entries_of(text));
        assert!(layout.is_dock_empty(DockSide::Left));
    }

    #[test]
    fn dock_at_slot_reorders_within_the_same_side() {
        let mut layout = PanelLayout::default();
        // 履歴(index 2)を先頭へ。
        assert!(layout.dock_at_slot(PanelKind::History, DockSide::Right, 0));
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::History, PanelKind::Color, PanelKind::Layers]
        );
        // 末尾へ戻す。
        assert!(layout.dock_at_slot(PanelKind::History, DockSide::Right, 3));
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::Color, PanelKind::Layers, PanelKind::History]
        );
    }

    #[test]
    fn dropping_a_panel_on_its_own_position_is_a_no_op() {
        let mut layout = PanelLayout::default();
        let before = layout.clone();
        // 色は表示位置 0 — スロット 0(自分の直前)も 1(自分の直後)も no-op。
        assert!(!layout.dock_at_slot(PanelKind::Color, DockSide::Right, 0));
        assert!(!layout.dock_at_slot(PanelKind::Color, DockSide::Right, 1));
        assert_eq!(layout, before);
    }

    #[test]
    fn dock_at_slot_moves_between_sides_and_renumbers_both() {
        let mut layout = PanelLayout::default();
        assert!(layout.dock_at_slot(PanelKind::Color, DockSide::Left, 0));
        assert_eq!(layout.docked(DockSide::Left), vec![PanelKind::Color]);
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::Layers, PanelKind::History]
        );
        for (expected, kind) in [(0, PanelKind::Layers), (1, PanelKind::History)] {
            match layout.placement(kind) {
                PanelPlacement::Dock { order, .. } => assert_eq!(order, expected),
                other => panic!("unexpected placement: {other:?}"),
            }
        }
    }

    #[test]
    fn float_at_leaves_the_dock_and_renumbers_it() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(50.0, 60.0), vec2(200.0, 300.0));
        assert_eq!(
            layout.docked(DockSide::Right),
            vec![PanelKind::Layers, PanelKind::History]
        );
        assert_eq!(layout.floating(), vec![PanelKind::Color]);
        match layout.placement(PanelKind::Layers) {
            PanelPlacement::Dock { order, .. } => assert_eq!(order, 0),
            other => panic!("unexpected placement: {other:?}"),
        }
    }

    #[test]
    fn float_at_rejects_non_finite_positions() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(f32::NAN, 0.0), DEFAULT_FLOAT_SIZE);
        match layout.placement(PanelKind::Color) {
            PanelPlacement::Floating { pos, .. } => {
                assert_eq!(pos, default_float_pos(PanelKind::Color))
            }
            other => panic!("unexpected placement: {other:?}"),
        }
    }

    #[test]
    fn set_floating_rect_keeps_the_old_value_for_garbage_input() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(10.0, 20.0), vec2(200.0, 300.0));
        layout.set_floating_rect(PanelKind::Color, pos2(f32::NAN, 5.0), vec2(0.0, f32::NAN));
        assert_eq!(
            layout.placement(PanelKind::Color),
            PanelPlacement::Floating {
                pos: pos2(10.0, 20.0),
                size: vec2(200.0, 300.0)
            }
        );
        // ドック中のパネルには何も起こらない。
        let before = layout.placement(PanelKind::Layers);
        layout.set_floating_rect(PanelKind::Layers, pos2(1.0, 1.0), vec2(200.0, 200.0));
        assert_eq!(layout.placement(PanelKind::Layers), before);
    }

    #[test]
    fn apply_move_appends_to_the_end_of_the_target_dock() {
        let mut layout = PanelLayout::default();
        layout.apply_move(PanelKind::Color, PanelMove::Dock(DockSide::Left));
        layout.apply_move(PanelKind::Layers, PanelMove::Dock(DockSide::Left));
        assert_eq!(
            layout.docked(DockSide::Left),
            vec![PanelKind::Color, PanelKind::Layers]
        );
        assert_eq!(layout.docked(DockSide::Right), vec![PanelKind::History]);
    }

    #[test]
    fn apply_move_float_keeps_an_existing_floating_rect() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(11.0, 22.0), vec2(180.0, 200.0));
        layout.apply_move(PanelKind::Color, PanelMove::Float);
        assert_eq!(
            layout.placement(PanelKind::Color),
            PanelPlacement::Floating {
                pos: pos2(11.0, 22.0),
                size: vec2(180.0, 200.0)
            }
        );
    }

    #[test]
    fn collapsed_state_survives_placement_changes_and_round_trip() {
        let mut layout = PanelLayout::default();
        layout.toggle_collapsed(PanelKind::History);
        layout.float_at(PanelKind::History, pos2(30.0, 40.0), DEFAULT_FLOAT_SIZE);
        assert!(layout.collapsed(PanelKind::History));
        layout.dock_at_slot(PanelKind::History, DockSide::Left, 0);
        assert!(layout.collapsed(PanelKind::History));
        assert!(round_trip(&layout).collapsed(PanelKind::History));
        layout.toggle_collapsed(PanelKind::History);
        assert!(!layout.collapsed(PanelKind::History));
    }

    #[test]
    fn reset_restores_the_default_layout() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(10.0, 10.0), DEFAULT_FLOAT_SIZE);
        layout.dock_at_slot(PanelKind::History, DockSide::Left, 0);
        layout.toggle_collapsed(PanelKind::Layers);
        layout.reset();
        assert_eq!(layout, PanelLayout::default());
    }

    #[test]
    fn clamp_floating_pulls_off_screen_windows_back_into_view() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(5000.0, -400.0), vec2(200.0, 300.0));
        assert!(layout.clamp_floating_to_screen(screen));
        match layout.placement(PanelKind::Color) {
            PanelPlacement::Floating { pos, size } => {
                assert_eq!(pos, pos2(800.0, 0.0));
                assert_eq!(size, vec2(200.0, 300.0));
                assert!(screen.contains_rect(Rect::from_min_size(pos, size)));
            }
            other => panic!("unexpected placement: {other:?}"),
        }
    }

    /// gpt-5.6-sol レビュー②の回帰テスト: 1 フレーム目の画面矩形が退化して
    /// いても、クランプの「やり残し」が分かるように成否を返す(呼び出し側は
    /// `true` が返るまで復元フラグを落とさない — `app.rs`)。
    #[test]
    fn clamp_floating_reports_failure_for_a_degenerate_screen_and_succeeds_later() {
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(5000.0, 5000.0), vec2(200.0, 300.0));
        let before = layout.clone();

        assert!(
            !layout.clamp_floating_to_screen(Rect::NOTHING),
            "画面矩形が未確定のフレームでは何もせず false"
        );
        assert_eq!(layout, before, "false のときは配置を触っていない");
        assert!(
            !layout.clamp_floating_to_screen(Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0)))
        );

        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        assert!(
            layout.clamp_floating_to_screen(screen),
            "有効な矩形では成功"
        );
        match layout.placement(PanelKind::Color) {
            PanelPlacement::Floating { pos, size } => {
                assert!(screen.contains_rect(Rect::from_min_size(pos, size)));
            }
            other => panic!("unexpected placement: {other:?}"),
        }
    }

    #[test]
    fn clamp_floating_leaves_visible_windows_untouched_and_ignores_a_degenerate_screen() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(100.0, 100.0), vec2(200.0, 300.0));
        let before = layout.clone();
        assert!(layout.clamp_floating_to_screen(screen));
        assert_eq!(layout, before);
        // `Rect::NOTHING` のような退化した画面矩形では何もしない
        // (1 フレーム目に content_rect が未確定でも壊さない)。
        assert!(!layout.clamp_floating_to_screen(Rect::NOTHING));
        assert_eq!(layout, before);
    }

    #[test]
    fn clamp_floating_shrinks_windows_larger_than_the_screen() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        let mut layout = PanelLayout::default();
        layout.float_at(PanelKind::Color, pos2(-50.0, -50.0), vec2(900.0, 900.0));
        assert!(layout.clamp_floating_to_screen(screen));
        match layout.placement(PanelKind::Color) {
            PanelPlacement::Floating { pos, size } => {
                assert_eq!(size, vec2(400.0, 300.0));
                assert_eq!(pos, pos2(0.0, 0.0));
            }
            other => panic!("unexpected placement: {other:?}"),
        }
    }

    #[test]
    fn insertion_slot_uses_block_midpoints() {
        let blocks = [
            Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0)),
            Rect::from_min_size(pos2(0.0, 100.0), vec2(200.0, 100.0)),
        ];
        assert_eq!(insertion_slot(&blocks, 10.0), 0);
        assert_eq!(insertion_slot(&blocks, 60.0), 1);
        assert_eq!(insertion_slot(&blocks, 190.0), 2);
        assert_eq!(insertion_slot(&[], 0.0), 0, "空のドックは常にスロット 0");
    }

    #[test]
    fn panel_and_side_tags_are_unique_and_reversible() {
        // 設定キーが衝突すると別パネルの配置を上書きしてしまう。
        let mut tags: Vec<&str> = PanelKind::ALL.iter().map(|k| k.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), PANEL_COUNT);
        for kind in PanelKind::ALL {
            assert!(!kind.title().is_empty());
            assert!(!kind.tag().is_empty());
        }
        for side in DockSide::DECLARATION_ORDER {
            assert_eq!(DockSide::from_tag(side.tag()), Some(side));
        }
        // "float" は place の第 3 の値なので、ドック側のタグとは衝突しない。
        assert_eq!(DockSide::from_tag("float"), None);
    }
}
