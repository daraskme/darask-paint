//! ドキュメント(レイヤー付きピクセルバッファ)の中核データ構造。
//!
//! v1(ARCHITECTURE.md §2, §6)では `Document` は単一のピクセルバッファ
//! だった。v2(ARCHITECTURE.md §14.1)でレイヤー(`Layer`)の配列を持つ形に
//! 拡張し、可視レイヤーを合成した結果を `composite` にキャッシュする。
//! `raster.rs` はこの `Document`/`Layer` を一切知らない(`Surface` という
//! 軽量なバッファビューだけを扱う)。ピクセル単位の描画・選択・クリップ
//! ボード操作は SPEC §13 のとおり**アクティブレイヤーのみ**に作用し、
//! スポイトや保存は `composite`(合成結果)を読む。
//!
//! 画像メニューの操作(resize/flip/rotate/crop)は SPEC §13 により
//! **全レイヤー**に適用する。これらの純粋な画素操作はモジュール下部の
//! フリー関数(`*_buffer`)として実装し、`Document` のメソッドが各レイヤーの
//! バッファへ順に適用する。

use std::path::PathBuf;

use crate::raster::{self, Surface};

/// SPEC §13: レイヤーは上限 64 枚。
pub const MAX_LAYERS: usize = 64;

/// 画像ピクセル座標系の矩形。半開区間 `[x0, x1) x [y0, y1)`。
///
/// `dirty` 領域(テクスチャ部分更新用)などに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    /// 幅(負にはならない。空矩形なら 0)。
    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    /// 高さ(負にはならない。空矩形なら 0)。
    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    /// 半開区間として中身が空(幅または高さが 0 以下)か。
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// 2 つの矩形を覆う最小の矩形(どちらかが空ならもう片方をそのまま返す)。
    pub fn union(&self, other: &IRect) -> IRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        IRect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// `0..width` x `0..height` の範囲にクランプする。
    /// `width`/`height` が 0 でもパニックしない。
    pub fn clamp_to(&self, width: u32, height: u32) -> IRect {
        let w = width as i32;
        let h = height as i32;
        IRect {
            x0: self.x0.clamp(0, w),
            y0: self.y0.clamp(0, h),
            x1: self.x1.clamp(0, w),
            y1: self.y1.clamp(0, h),
        }
    }
}

/// v4 §16.1(ARCHITECTURE.md): `Document::dirty` を単一矩形の union から、
/// 上限付きのセグメント列へ一般化したもの。
///
/// v1〜v3 は `Option<IRect>` で「マージされた 1 個の外接矩形」を持っていたが、
/// 高速ドラッグで 1 フレームに複数箇所へスタンプすると(ブラシが対角線を
/// 横切るように動いた場合など)外接矩形がその間の触れていない領域まで
/// 覆ってしまい、`recomposite`/テクスチャ部分更新のコストが実際の変更量に
/// 対して不釣り合いに大きくなっていた(SPEC §28: 「ドラッグを横断する
/// 巨大 dirty 矩形を作らない」)。`push` されたセグメントをそのまま
/// (union せずに)保持し、`canvas_view` が各セグメントごとに個別に
/// recomposite + テクスチャ部分更新することで、実際に触れた面積にコストを
/// 比例させる。
///
/// 上限 `MAX_SEGMENTS` を超えて積もうとした場合だけ、末尾のセグメントへ
/// union してまとめる(ARCHITECTURE.md §16.10-3: 「重複排除の複雑化より
/// 単純さ優先」。1 フレームに 32 回を大きく超えてスタンプするような異常系
/// でもメモリ・ループ回数が無制限には増えないことだけを保証する設計)。
#[derive(Default, Clone)]
pub struct DirtyRegion {
    rects: Vec<IRect>,
}

/// セグメントの上限(ARCHITECTURE.md §16.1: 「小さな `Vec<IRect>`、上限 32
/// 個で溢れたら合併」)。
const MAX_DIRTY_SEGMENTS: usize = 32;

impl DirtyRegion {
    pub fn new() -> Self {
        Self { rects: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// `rect` を新しいセグメントとして積む。空矩形は無視する
    /// (呼び出し側の `Document::mark_dirty` が既に画像境界へクランプ済み
    /// であること前提)。上限に達していれば、末尾のセグメントへ union して
    /// セグメント数を増やさない。
    pub fn push(&mut self, rect: IRect) {
        if rect.is_empty() {
            return;
        }
        if self.rects.len() >= MAX_DIRTY_SEGMENTS {
            if let Some(last) = self.rects.last_mut() {
                *last = last.union(&rect);
            }
            return;
        }
        self.rects.push(rect);
    }

    /// 現在のセグメント一覧(テスト・`recompose_if_dirty` 用)。
    pub fn rects(&self) -> &[IRect] {
        &self.rects
    }

    /// 蓄積したセグメントを取り出して空にする(`canvas_view` が毎フレーム、
    /// テクスチャ部分更新のために消費する)。
    pub fn take(&mut self) -> Vec<IRect> {
        std::mem::take(&mut self.rects)
    }

    /// 中身を空にするだけ(取り出した内容が不要なとき、ARCHITECTURE.md
    /// §16.1)。
    pub fn clear(&mut self) {
        self.rects.clear();
    }
}

/// v4 §16.3/§21(ARCHITECTURE.md): 矩形限定だった選択を一般化した
/// ビットマスク選択。`bbox`(半開区間)の内側だけを覆う `mask`(値は 0 か
/// 255、フェザーは非目標)を持つ。`mask.len() == bbox.width() as usize *
/// bbox.height() as usize` という不変条件を常に守る。
///
/// `IRect` と同じくここ(document.rs)に置くのは、`raster.rs` の `Surface`
/// (描画クリップ、ARCHITECTURE.md §16.3)がこの型を直接参照する必要が
/// あるため(`tools/select.rs` はこの型の上に `Selection`/`Floating` という
/// 高レベルな概念と、`rect_mask`/`mask_boundary`/`resample_mask_nearest` と
/// いった純関数を積む)。`document.rs`⇄`raster.rs` の相互参照は
/// `IRect`(raster.rs が使う)/`Surface`(document.rs が使う)で既に存在して
/// おり、同一クレート内の `mod` 間では問題にならない。
#[derive(Debug, Clone)]
pub struct SelMask {
    pub bbox: IRect,
    pub mask: Vec<u8>,
}

impl SelMask {
    /// 空(何も選択されていない)マスク。
    pub fn empty() -> Self {
        Self {
            bbox: IRect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            },
            mask: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bbox.is_empty()
    }

    /// `(x, y)`(画像座標)のマスク値。`bbox` の外は常に 0(パニックしない)。
    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < self.bbox.x0 || x >= self.bbox.x1 || y < self.bbox.y0 || y >= self.bbox.y1 {
            return 0;
        }
        let w = self.bbox.width() as usize;
        let idx = (y - self.bbox.y0) as usize * w + (x - self.bbox.x0) as usize;
        self.mask.get(idx).copied().unwrap_or(0)
    }

    /// `(x, y)` が選択されているか(`get` が非 0 を返すか)。
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.get(x, y) != 0
    }

    /// `width`×`height` の範囲へクランプする(`bbox` を切り詰め、マスクを
    /// 再インデックスして詰め直す)。選択は通常ドキュメントサイズが変わる
    /// 操作の前に必ずコミット済み(commit_selection)になるため実運用では
    /// 滅多に起きないが、防御的に用意しておく安全弁。
    pub fn clamp_to(&self, width: u32, height: u32) -> SelMask {
        let new_bbox = self.bbox.clamp_to(width, height);
        if new_bbox.is_empty() {
            return SelMask::empty();
        }
        if new_bbox == self.bbox {
            return self.clone();
        }
        let w = new_bbox.width() as usize;
        let h = new_bbox.height() as usize;
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                mask[y * w + x] = self.get(new_bbox.x0 + x as i32, new_bbox.y0 + y as i32);
            }
        }
        SelMask {
            bbox: new_bbox,
            mask,
        }
    }
}

/// 新規ドキュメント作成時の背景(SPEC §7 の「新規」ダイアログの選択肢)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    White,
    Transparent,
}

/// 画像サイズ変更(SPEC §7)の補間方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Bilinear,
    Nearest,
}

/// v2 §13/§14.1: 1 枚のレイヤー。RGBA8・行優先、`Document` と同寸。
#[derive(Clone)]
pub struct Layer {
    /// V2-M2(レイヤー UI)の右パネルに一覧表示・変更される(SPEC §13)。
    pub name: String,
    pub visible: bool,
    /// 0-255(UI 表示は %)。
    pub opacity: u8,
    /// `len() == width as usize * height as usize * 4`
    pub pixels: Vec<u8>,
}

impl Layer {
    /// `fill` で塗りつぶした不透明・表示ありのレイヤーを作る。
    pub fn filled(name: impl Into<String>, width: u32, height: u32, fill: [u8; 4]) -> Self {
        let count = (width as usize).saturating_mul(height as usize);
        let mut pixels = Vec::with_capacity(count.saturating_mul(4));
        for _ in 0..count {
            pixels.extend_from_slice(&fill);
        }
        Self {
            name: name.into(),
            visible: true,
            opacity: 255,
            pixels,
        }
    }
}

/// v2 §14.2: `HistoryOp::ReplaceAll` が使う、全レイヤー+寸法のスナップショット
/// (resize/crop/rotate/反転/画像の統合など、サイズ・レイヤー構成が丸ごと
/// 変わりうる操作の undo 単位)。
#[derive(Clone)]
pub struct DocSnapshot {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>,
    pub active: usize,
}

/// レイヤー付きドキュメント(ARCHITECTURE.md §14.1)。座標は左上原点、x右・y下。
pub struct Document {
    pub width: u32,
    pub height: u32,
    /// index 0 = 最下層(SPEC §13: 下から上に合成)。常に 1 枚以上。
    pub layers: Vec<Layer>,
    /// `layers` への現在のアクティブレイヤーの添字。常に `layers` の範囲内。
    pub active: usize,
    /// 可視レイヤーの合成キャッシュ(RGBA、チェッカーは含まない)。
    /// 常に `len() == width as usize * height as usize * 4`。
    pub composite: Vec<u8>,
    pub path: Option<PathBuf>,
    pub modified: bool,
    /// 前フレーム以降に変更された領域群(合成の再計算・テクスチャ部分更新用、
    /// v4 §16.1: セグメント単位、`DirtyRegion` 参照)。
    pub dirty: DirtyRegion,
}

impl Document {
    /// 白背景または透明背景の新規ドキュメントを作成する(SPEC §3, §7)。
    /// 「背景」という名前の 1 枚のレイヤーを持つ(SPEC §13)。
    ///
    /// `width`/`height` に `0` を渡されてもパニックしない(空のピクセルバッファに
    /// なるだけ)。ユーザー入力経路で `unwrap()`/panic をしないという方針
    /// (CLAUDE.md 鉄則)をここでも守る。
    pub fn new(width: u32, height: u32, background: Background) -> Self {
        let fill: [u8; 4] = match background {
            Background::White => [255, 255, 255, 255],
            Background::Transparent => [0, 0, 0, 0],
        };
        let layer = Layer::filled("背景", width, height, fill);
        Self::from_layers(width, height, vec![layer], None)
    }

    /// 画像ファイルを読み込んだ直後のドキュメント(SPEC §13:「ファイルを
    /// 開いた直後は『背景』レイヤー1枚」)。`io::load_image` から使う。
    pub fn from_loaded(width: u32, height: u32, pixels: Vec<u8>, path: PathBuf) -> Self {
        let layer = Layer {
            name: "背景".to_owned(),
            visible: true,
            opacity: 255,
            pixels,
        };
        Self::from_layers(width, height, vec![layer], Some(path))
    }

    /// v5 §31(ARCHITECTURE.md §17.5): 「選択範囲を新規タブに複製」専用の
    /// コンストラクタ。呼び出し側(`app.rs::duplicate_selection_to_new_tab`)が
    /// 組み立てた `layers`(名前・表示・不透明度・重ね順は呼び出し側が既存の
    /// レイヤーからそのまま引き継ぐ)と、複製元でアクティブだったレイヤーの
    /// 添字をそのまま新規ドキュメントに反映する。新規タブは常に「無題」系の
    /// 命名になるため `path` は持たない(SPEC §31: 「パスは無し」)。
    /// `layers` が空(呼び出し側の想定外の入力)なら 1 枚以上の不変条件を守る
    /// ため透明の 1 枚にフォールバックする(`apply_snapshot` と同じ安全側の
    /// パターン)。
    pub fn from_duplicated_layers(
        width: u32,
        height: u32,
        layers: Vec<Layer>,
        active: usize,
    ) -> Self {
        let layers = if layers.is_empty() {
            vec![Layer::filled("背景", width, height, [0, 0, 0, 0])]
        } else {
            layers
        };
        let active = active.min(layers.len() - 1);
        let mut doc = Self::from_layers(width, height, layers, None);
        doc.active = active;
        // SPEC §31: 「新規タブの未保存フラグは true(ファイルに存在しない
        // 新しい内容のため)」。
        doc.modified = true;
        doc
    }

    fn from_layers(width: u32, height: u32, layers: Vec<Layer>, path: Option<PathBuf>) -> Self {
        let mut doc = Self {
            width,
            height,
            layers,
            active: 0,
            composite: Vec::new(),
            path,
            modified: false,
            dirty: DirtyRegion::new(),
        };
        doc.recomposite_full();
        doc
    }

    /// `.dpaint` の検証済みリビジョンからドキュメントを復元する。
    /// 外部入力の寸法・レイヤー数・画素長は呼び出し側が先に検証する。
    pub(crate) fn try_from_snapshot_owned(
        snap: DocSnapshot,
        path: Option<PathBuf>,
        modified: bool,
    ) -> Result<Self, String> {
        if snap.layers.is_empty() {
            return Err("プロジェクトのレイヤーが空です".to_owned());
        }
        let layers = snap.layers;
        let active = snap.active.min(layers.len().saturating_sub(1));
        let composite_len = (snap.width as usize)
            .checked_mul(snap.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "プロジェクトの画像寸法が大きすぎます".to_owned())?;
        let mut composite = Vec::new();
        composite
            .try_reserve_exact(composite_len)
            .map_err(|_| "プロジェクトの合成バッファを確保できません".to_owned())?;
        composite.resize(composite_len, 0);
        let mut doc = Self {
            width: snap.width,
            height: snap.height,
            layers,
            active,
            composite,
            path,
            modified,
            dirty: DirtyRegion::new(),
        };
        let mut refs = Vec::new();
        refs.try_reserve_exact(doc.layers.len())
            .map_err(|_| "プロジェクトのレイヤー参照を確保できません".to_owned())?;
        refs.extend(doc.layers.iter());
        composite_layers(
            &refs,
            doc.width,
            IRect {
                x0: 0,
                y0: 0,
                x1: doc.width as i32,
                y1: doc.height as i32,
            },
            &mut doc.composite,
        );
        Ok(doc)
    }

    // -----------------------------------------------------------------
    // アクティブレイヤーアクセス(SPEC §13: 描画・選択・クリップボードは
    // アクティブレイヤーのみに作用する)
    // -----------------------------------------------------------------

    /// `active` を `layers` の範囲内にクランプした添字(ARCHITECTURE.md
    /// §14.9-4: 削除等で `active` が範囲外になる off-by-one を防ぐため、
    /// レイヤー操作はすべてこれを経由する)。`layers` は常に 1 枚以上である
    /// 不変条件を各操作が保つ。
    pub fn active_index(&self) -> usize {
        self.active.min(self.layers.len().saturating_sub(1))
    }

    /// アクティブレイヤーへの参照。`active` が(万一)範囲外でもパニックせず
    /// 末尾のレイヤーにクランプする。
    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active_index()]
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        let idx = self.active_index();
        &mut self.layers[idx]
    }

    /// `raster.rs` の関数へ渡すための、アクティブレイヤーのバッファビュー。
    /// `clip`(v4 §16.3: 選択があるときの描画クリップ)は `Surface::set_pixel`
    /// が内部で見る。選択が無ければ `None` を渡すこと(コストがゼロになる、
    /// ARCHITECTURE.md §16.10-2)。
    pub fn active_surface_mut<'a>(&'a mut self, clip: Option<&'a SelMask>) -> Surface<'a> {
        let (width, height) = (self.width, self.height);
        Surface {
            width,
            height,
            pixels: &mut self.active_layer_mut().pixels,
            clip,
        }
    }

    /// アクティブレイヤーの `(x, y)` を読む(SPEC §13 の対象範囲)。範囲外
    /// なら `None`。
    pub fn get_pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        read_pixel(&self.active_layer().pixels, self.width, self.height, x, y)
    }

    /// アクティブレイヤーの `(x, y)` に書く。範囲外なら何もしない。
    pub fn set_pixel(&mut self, x: i32, y: i32, color: [u8; 4]) {
        let (w, h) = (self.width, self.height);
        write_pixel(&mut self.active_layer_mut().pixels, w, h, x, y, color);
    }

    /// アクティブレイヤーの生のピクセルバッファ(テスト・ラウンドトリップ
    /// 確認用)。本体コードは `get_pixel`/`set_pixel`/`active_surface_mut` を
    /// 使うため、通常ビルドでは呼ばれない(`#[cfg(test)]` 各所から使う)。
    #[allow(dead_code)]
    pub fn active_pixels(&self) -> &[u8] {
        &self.active_layer().pixels
    }

    /// 合成結果の `(x, y)` を読む(SPEC §13: スポイトは合成結果から色を取る)。
    /// 呼び出し側は事前に `recompose_if_dirty`/`recomposite_full` で
    /// `composite` を最新化しておくこと。
    pub fn composite_pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        read_pixel(&self.composite, self.width, self.height, x, y)
    }

    // -----------------------------------------------------------------
    // dirty 管理・合成(ARCHITECTURE.md §14.1, §14.9-2)
    // -----------------------------------------------------------------

    /// `rect` を `dirty` に新しいセグメントとして積む(v4 §16.1: もはや 1 個の
    /// 外接矩形へ union しない、`DirtyRegion` 参照)。空矩形は無視する。
    pub fn mark_dirty(&mut self, rect: IRect) {
        let rect = rect.clamp_to(self.width, self.height);
        self.dirty.push(rect);
    }

    /// 画像全体を `dirty` にする(レイヤー構造の変更・サイズ変更・反転など、
    /// 局所矩形では表せない/全画素が変わりうる操作向け、
    /// ARCHITECTURE.md §14.9-2: 「合成キャッシュの整合: レイヤー構造変更は
    /// 必ず全面 dirty」)。全面矩形は他のどんなセグメントも覆うので、既存の
    /// セグメントは先にクリアしてから積む(無駄なセグメントを残さない)。
    pub fn mark_all_dirty(&mut self) {
        self.dirty.clear();
        self.dirty.push(IRect {
            x0: 0,
            y0: 0,
            x1: self.width as i32,
            y1: self.height as i32,
        });
    }

    /// 可視レイヤーを下から straight-alpha + レイヤー不透明度で `rect` の
    /// 範囲だけ合成し直す(ARCHITECTURE.md §14.1)。`composite` は常に
    /// `width*height*4` バイトに正しくサイズされている前提(サイズが変わる
    /// 操作は必ず `recomposite_full`/`apply_snapshot` 等でバッファを
    /// 作り直すため、この関数自身はサイズ変更しない)。
    pub fn recomposite(&mut self, rect: IRect) {
        let rect = rect.clamp_to(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        let width = self.width;
        let refs: Vec<&Layer> = self.layers.iter().collect();
        composite_layers(&refs, width, rect, &mut self.composite);
    }

    /// `composite` を正しいサイズに作り直してから全画面を合成する
    /// (レイヤー構成・寸法が変わった直後、および保存前に呼ぶ)。
    pub fn recomposite_full(&mut self) {
        self.composite = vec![0u8; self.width as usize * self.height as usize * 4];
        self.recomposite(IRect {
            x0: 0,
            y0: 0,
            x1: self.width as i32,
            y1: self.height as i32,
        });
    }

    /// `dirty`(未反映の編集領域)があれば、そのセグメントごとに `composite`
    /// を最新化する。`dirty` 自体はクリアしない(テクスチャ部分更新
    /// (`canvas_view`)が別途消費するため)。スポイト(SPEC §13)など、
    /// フレームの描画を待たずに合成結果を読みたい箇所から呼ぶ。
    pub fn recompose_if_dirty(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        // `self.dirty.rects()` は `&self` を借用するため、`self.recomposite`
        // (`&mut self`)と同時には持てない。セグメント数は高々
        // `MAX_DIRTY_SEGMENTS` 個なので複製のコストは無視できる。
        let rects = self.dirty.rects().to_vec();
        for rect in rects {
            self.recomposite(rect);
        }
    }

    // -----------------------------------------------------------------
    // undo/redo 用スナップショット(ARCHITECTURE.md §14.2: ReplaceAll)
    // -----------------------------------------------------------------

    pub fn snapshot(&self) -> DocSnapshot {
        DocSnapshot {
            width: self.width,
            height: self.height,
            layers: self.layers.clone(),
            active: self.active,
        }
    }

    /// `snapshot` を丸ごと復元する(undo/redo の `ReplaceAll` 適用)。
    pub fn apply_snapshot(&mut self, snap: &DocSnapshot) {
        self.width = snap.width;
        self.height = snap.height;
        self.layers = if snap.layers.is_empty() {
            // 不変条件(1 枚以上)を守るための安全側フォールバック。実際には
            // 起こらない想定(レイヤーが 0 枚になる操作は存在しない)。
            vec![Layer::filled("背景", snap.width, snap.height, [0, 0, 0, 0])]
        } else {
            snap.layers.clone()
        };
        self.active = snap.active.min(self.layers.len().saturating_sub(1));
        self.composite = vec![0u8; self.width as usize * self.height as usize * 4];
        self.mark_all_dirty();
    }

    /// クリップボードからの「白紙時の置き換え貼り付け」(SPEC §6)専用:
    /// ドキュメント全体を単一の「背景」レイヤーに置き換える。
    pub fn replace_with_single_layer(&mut self, width: u32, height: u32, pixels: Vec<u8>) {
        self.width = width;
        self.height = height;
        self.layers = vec![Layer {
            name: "背景".to_owned(),
            visible: true,
            opacity: 255,
            pixels,
        }];
        self.active = 0;
        self.composite = vec![0u8; width as usize * height as usize * 4];
        self.mark_all_dirty();
    }

    // -----------------------------------------------------------------
    // 画像メニュー(SPEC §7, §13: 全レイヤーに適用)
    // -----------------------------------------------------------------

    /// 左右反転。サイズは変わらない。
    pub fn flip_horizontal(&mut self) {
        let w = self.width;
        for layer in &mut self.layers {
            flip_horizontal_buffer(&mut layer.pixels, w);
        }
        self.mark_all_dirty();
    }

    /// 上下反転。サイズは変わらない。
    pub fn flip_vertical(&mut self) {
        let (w, h) = (self.width, self.height);
        for layer in &mut self.layers {
            flip_vertical_buffer(&mut layer.pixels, w, h);
        }
        self.mark_all_dirty();
    }

    /// 右に 90° 回転。幅と高さが入れ替わる。
    pub fn rotate_cw(&mut self) {
        let (w, h) = (self.width, self.height);
        for layer in &mut self.layers {
            layer.pixels = rotate_cw_buffer(w, h, &layer.pixels);
        }
        self.width = h;
        self.height = w;
        self.composite = vec![0u8; self.width as usize * self.height as usize * 4];
        self.mark_all_dirty();
    }

    /// 左に 90° 回転。幅と高さが入れ替わる。
    pub fn rotate_ccw(&mut self) {
        let (w, h) = (self.width, self.height);
        for layer in &mut self.layers {
            layer.pixels = rotate_ccw_buffer(w, h, &layer.pixels);
        }
        self.width = h;
        self.height = w;
        self.composite = vec![0u8; self.width as usize * self.height as usize * 4];
        self.mark_all_dirty();
    }

    /// 画像サイズ変更(SPEC §7: 補間 = バイリニア / ニアレスト)。
    pub fn resize(&mut self, new_width: u32, new_height: u32, interp: Interpolation) {
        let (w, h) = (self.width, self.height);
        for layer in &mut self.layers {
            layer.pixels = resize_buffer(w, h, &layer.pixels, new_width, new_height, interp);
        }
        self.width = new_width;
        self.height = new_height;
        self.composite = vec![0u8; new_width as usize * new_height as usize * 4];
        self.mark_all_dirty();
    }

    /// キャンバスサイズ変更(SPEC §7: 既存画像は左上基準で配置、拡張部分は
    /// 透明)。
    pub fn resize_canvas(&mut self, new_width: u32, new_height: u32) {
        let (w, h) = (self.width, self.height);
        for layer in &mut self.layers {
            layer.pixels = resize_canvas_buffer(w, h, &layer.pixels, new_width, new_height);
        }
        self.width = new_width;
        self.height = new_height;
        self.composite = vec![0u8; new_width as usize * new_height as usize * 4];
        self.mark_all_dirty();
    }

    /// `rect`(画像座標、境界外は自動クランプ)へトリミングする
    /// (SPEC §7: 選択範囲でトリミング)。
    pub fn crop_to(&mut self, rect: IRect) {
        let rect = rect.clamp_to(self.width, self.height);
        let (w, h) = (self.width, self.height);
        let new_w = rect.width() as u32;
        let new_h = rect.height() as u32;
        for layer in &mut self.layers {
            layer.pixels = crop_buffer(w, h, &layer.pixels, rect);
        }
        self.width = new_w;
        self.height = new_h;
        self.composite = vec![0u8; new_w as usize * new_h as usize * 4];
        self.mark_all_dirty();
    }

    // -----------------------------------------------------------------
    // レイヤー構造の操作(SPEC §13、ARCHITECTURE.md §14.8 V2-M2)。
    //
    // ここに並ぶ操作はすべて「1 回の呼び出しで 1 undo 単位」(呼び出し側の
    // `app.rs` が `snapshot()`/`ReplaceAll` で包む)であり、成功したときだけ
    // `true` を返す。失敗(上限到達・レイヤーが 1 枚しかない等)のときは
    // 何も変更せず `false` を返すので、呼び出し側は無意味な undo エントリを
    // 積まずに済む。
    // -----------------------------------------------------------------

    /// 新規レイヤー(SPEC §13: 「新規レイヤーは透明で名前は『レイヤー N』」)。
    /// アクティブレイヤーのすぐ上に挿入し、それをアクティブにする。上限
    /// (`MAX_LAYERS`)に達していれば何もせず `false`。
    pub fn add_layer(&mut self, name: String) -> bool {
        if self.layers.len() >= MAX_LAYERS {
            return false;
        }
        let (width, height) = (self.width, self.height);
        let insert_at = self.active_index() + 1;
        self.layers.insert(
            insert_at,
            Layer {
                name,
                visible: true,
                opacity: 255,
                pixels: vec![0u8; width as usize * height as usize * 4],
            },
        );
        self.active = insert_at;
        true
    }

    /// アクティブレイヤーを複製し、すぐ上に挿入してアクティブにする
    /// (SPEC §13: 「複製」ボタン)。上限に達していれば `false`。
    pub fn duplicate_active_layer(&mut self) -> bool {
        if self.layers.len() >= MAX_LAYERS {
            return false;
        }
        let idx = self.active_index();
        let dup = self.layers[idx].clone();
        let insert_at = idx + 1;
        self.layers.insert(insert_at, dup);
        self.active = insert_at;
        true
    }

    /// アクティブレイヤーを削除する(SPEC §13: 「レイヤーが 1 枚のときは
    /// 削除…は無効」)。削除後のアクティブ添字は、削除位置に居た(=繰り
    /// 上がった)レイヤーへ、最上位を消した場合は新しい最上位へクランプする
    /// (ARCHITECTURE.md §14.9-4 の off-by-one 対策)。
    pub fn remove_active_layer(&mut self) -> bool {
        if self.layers.len() <= 1 {
            return false;
        }
        let idx = self.active_index();
        self.layers.remove(idx);
        self.active = idx.min(self.layers.len().saturating_sub(1));
        true
    }

    /// アクティブレイヤーを 1 つ上へ移動する(SPEC §13: 「上へ」)。既に
    /// 最上位なら `false`。
    pub fn move_active_layer_up(&mut self) -> bool {
        let idx = self.active_index();
        if idx + 1 >= self.layers.len() {
            return false;
        }
        self.layers.swap(idx, idx + 1);
        self.active = idx + 1;
        true
    }

    /// アクティブレイヤーを 1 つ下へ移動する(SPEC §13: 「下へ」)。既に
    /// 最下位なら `false`。
    pub fn move_active_layer_down(&mut self) -> bool {
        let idx = self.active_index();
        if idx == 0 {
            return false;
        }
        self.layers.swap(idx, idx - 1);
        self.active = idx - 1;
        true
    }

    /// アクティブレイヤーを直下のレイヤーへ結合する(SPEC §13: 「下と結合」、
    /// レイヤーが 1 枚 or アクティブが最下位なら無効)。結合後のレイヤーは
    /// 不透明度 255・表示ありになり(両レイヤーの見た目をそのまま焼き込む
    /// ため)、名前は下側レイヤーのものを引き継ぐ。
    pub fn merge_active_down(&mut self) -> bool {
        if self.layers.len() <= 1 {
            return false;
        }
        let upper_idx = self.active_index();
        if upper_idx == 0 {
            return false;
        }
        let lower_idx = upper_idx - 1;
        let (width, height) = (self.width, self.height);
        let merged = composite_two(
            &self.layers[lower_idx],
            &self.layers[upper_idx],
            width,
            height,
        );
        let name = self.layers[lower_idx].name.clone();
        self.layers[lower_idx] = Layer {
            name,
            visible: true,
            opacity: 255,
            pixels: merged,
        };
        self.layers.remove(upper_idx);
        self.active = lower_idx;
        true
    }

    /// 画像の統合(SPEC §13: メニュー「画像の統合」)。可視レイヤーの合成
    /// 結果(非表示レイヤーは寄与しない、`recomposite` と同じ規則)を単一の
    /// 「背景」レイヤーに焼き込む。レイヤーが既に 1 枚なら何もせず `false`。
    pub fn flatten_all(&mut self) -> bool {
        if self.layers.len() <= 1 {
            return false;
        }
        let (width, height) = (self.width, self.height);
        let merged = {
            let refs: Vec<&Layer> = self.layers.iter().collect();
            let mut out = vec![0u8; width as usize * height as usize * 4];
            let full = IRect {
                x0: 0,
                y0: 0,
                x1: width as i32,
                y1: height as i32,
            };
            composite_layers(&refs, width, full, &mut out);
            out
        };
        self.layers = vec![Layer {
            name: "背景".to_owned(),
            visible: true,
            opacity: 255,
            pixels: merged,
        }];
        self.active = 0;
        true
    }
}

/// `history.rs` の `HistoryOp::MergeDown` の redo が使う: 下・上の 2 層だけを
/// 合成する(`composite_layers` の 2 層版)。`merge_active_down` 自身もこれを
/// 使う(結合結果は保持せず、undo された結合を redo するときに 2 層の元データ
/// から再計算することでメモリを節約する、ARCHITECTURE.md §14.2)。
pub(crate) fn composite_two(lower: &Layer, upper: &Layer, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; width as usize * height as usize * 4];
    let full = IRect {
        x0: 0,
        y0: 0,
        x1: width as i32,
        y1: height as i32,
    };
    composite_layers(&[lower, upper], width, full, &mut out);
    out
}

/// 可視レイヤーを下から straight-alpha + レイヤー不透明度で合成する共通処理。
/// `Document::recomposite`(部分矩形)・`merge_active_down`・`flatten_all`
/// (いずれも全面)が共有する(ARCHITECTURE.md §14.1 の合成規則を 1 箇所に
/// まとめ、実装がずれないようにするため)。`out` は `width*height*4` バイト
/// (`Document::composite` と同じレイアウト)であること。
///
/// v4 §16.1: 画素ごとに `y*w+x` のインデックス計算と境界チェック付き
/// `get`/`get_mut` を呼んでいたホットループを、行(`rect` の 1 行ぶん)単位の
/// スライスに書き直した。行の開始オフセットは 1 行につき 1 回だけ計算し、
/// 行の内側は `chunks_exact(4)` の `zip` で画素を回すため、画素単位の
/// 境界チェック呼び出しがなくなる(`recomposite` は起動時・レイヤー変更・
/// 描画のたびに `dirty` セグメントぶん呼ばれるため、4000×4000 全面合成
/// (画像の統合・保存前など)のコストに直結する)。
fn composite_layers(layers: &[&Layer], width: u32, rect: IRect, out: &mut [u8]) {
    let w = width as usize;
    if rect.is_empty() {
        return;
    }
    let x0 = rect.x0 as usize;
    let x1 = rect.x1 as usize;
    let row_bytes = (x1 - x0) * 4;

    for y in rect.y0..rect.y1 {
        let row_start = (y as usize * w + x0) * 4;
        let row_end = row_start + row_bytes;
        let Some(out_row) = out.get_mut(row_start..row_end) else {
            continue;
        };
        // 各画素は透明(acc=[0,0,0,0])から積み上げる(v1 の意味論どおり)。
        out_row.fill(0);
        for layer in layers {
            if !layer.visible || layer.opacity == 0 {
                continue;
            }
            let Some(layer_row) = layer.pixels.get(row_start..row_end) else {
                continue;
            };
            let opacity = layer.opacity;
            for (dst, src) in out_row.chunks_exact_mut(4).zip(layer_row.chunks_exact(4)) {
                let mut s = [src[0], src[1], src[2], src[3]];
                if opacity != 255 {
                    s[3] = ((s[3] as u32 * opacity as u32) / 255) as u8;
                }
                let d = [dst[0], dst[1], dst[2], dst[3]];
                dst.copy_from_slice(&raster::blend_over(d, s));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 汎用ピクセルバッファ操作(Document/Surface 双方の get/set と同じロジック)
// ---------------------------------------------------------------------------

fn read_pixel(pixels: &[u8], width: u32, height: u32, x: i32, y: i32) -> Option<[u8; 4]> {
    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
        return None;
    }
    let idx = (y as usize * width as usize + x as usize) * 4;
    pixels.get(idx..idx + 4).map(|s| [s[0], s[1], s[2], s[3]])
}

fn write_pixel(pixels: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
        return;
    }
    let idx = (y as usize * width as usize + x as usize) * 4;
    if let Some(slice) = pixels.get_mut(idx..idx + 4) {
        slice.copy_from_slice(&color);
    }
}

// ---------------------------------------------------------------------------
// 全画像変換(1 レイヤーぶんのバッファに対する純粋な操作。`Document` の各
// メソッドがすべてのレイヤーに順に適用する、SPEC §13)。
// ---------------------------------------------------------------------------

fn flip_horizontal_buffer(pixels: &mut [u8], width: u32) {
    let w = width as usize;
    if w == 0 {
        return;
    }
    for row in pixels.chunks_mut(w * 4) {
        let mut l = 0usize;
        let mut r = w - 1;
        while l < r {
            let (left, right) = row.split_at_mut(r * 4);
            left[l * 4..l * 4 + 4].swap_with_slice(&mut right[0..4]);
            l += 1;
            r -= 1;
        }
    }
}

fn flip_vertical_buffer(pixels: &mut [u8], width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let row_bytes = w * 4;
    let mut top = 0usize;
    let mut bottom = h - 1;
    while top < bottom {
        let (a, b) = pixels.split_at_mut(bottom * row_bytes);
        a[top * row_bytes..top * row_bytes + row_bytes].swap_with_slice(&mut b[0..row_bytes]);
        top += 1;
        bottom -= 1;
    }
}

fn rotate_cw_buffer(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (width, height);
    let new_w = h;
    let new_h = w;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
    for row in 0..new_h {
        for col in 0..new_w {
            // new(col, row) = old(row, h-1-col)(document.rs のテストで検証)。
            let x = row;
            let y = h - 1 - col;
            let px = read_pixel(pixels, w, h, x as i32, y as i32).unwrap_or([0, 0, 0, 0]);
            let idx = (row as usize * new_w as usize + col as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

fn rotate_ccw_buffer(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (width, height);
    let new_w = h;
    let new_h = w;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
    for row in 0..new_h {
        for col in 0..new_w {
            // new(col, row) = old(w-1-row, col)(rotate_cw の逆写像、テストで検証)。
            let x = w - 1 - row;
            let y = col;
            let px = read_pixel(pixels, w, h, x as i32, y as i32).unwrap_or([0, 0, 0, 0]);
            let idx = (row as usize * new_w as usize + col as usize) * 4;
            out[idx..idx + 4].copy_from_slice(&px);
        }
    }
    out
}

fn resize_buffer(
    width: u32,
    height: u32,
    pixels: &[u8],
    new_width: u32,
    new_height: u32,
    interp: Interpolation,
) -> Vec<u8> {
    if new_width == 0 || new_height == 0 {
        return Vec::new();
    }
    if width == 0 || height == 0 {
        return vec![0u8; new_width as usize * new_height as usize * 4];
    }

    let mut out = vec![0u8; new_width as usize * new_height as usize * 4];
    let scale_x = width as f32 / new_width as f32;
    let scale_y = height as f32 / new_height as f32;

    match interp {
        Interpolation::Nearest => {
            for ny in 0..new_height {
                let sy =
                    (((ny as f32 + 0.5) * scale_y).floor()).clamp(0.0, height as f32 - 1.0) as i32;
                for nx in 0..new_width {
                    let sx = (((nx as f32 + 0.5) * scale_x).floor()).clamp(0.0, width as f32 - 1.0)
                        as i32;
                    let px = read_pixel(pixels, width, height, sx, sy).unwrap_or([0, 0, 0, 0]);
                    let idx = (ny as usize * new_width as usize + nx as usize) * 4;
                    out[idx..idx + 4].copy_from_slice(&px);
                }
            }
        }
        Interpolation::Bilinear => {
            for ny in 0..new_height {
                let sy = ((ny as f32 + 0.5) * scale_y - 0.5).clamp(0.0, height as f32 - 1.0);
                let y0 = sy.floor() as i32;
                let y1 = (y0 + 1).min(height as i32 - 1);
                let fy = sy - y0 as f32;
                for nx in 0..new_width {
                    let sx = ((nx as f32 + 0.5) * scale_x - 0.5).clamp(0.0, width as f32 - 1.0);
                    let x0 = sx.floor() as i32;
                    let x1 = (x0 + 1).min(width as i32 - 1);
                    let fx = sx - x0 as f32;
                    let p00 = read_pixel(pixels, width, height, x0, y0).unwrap_or([0, 0, 0, 0]);
                    let p10 = read_pixel(pixels, width, height, x1, y0).unwrap_or([0, 0, 0, 0]);
                    let p01 = read_pixel(pixels, width, height, x0, y1).unwrap_or([0, 0, 0, 0]);
                    let p11 = read_pixel(pixels, width, height, x1, y1).unwrap_or([0, 0, 0, 0]);
                    let px = bilerp(p00, p10, p01, p11, fx, fy);
                    let idx = (ny as usize * new_width as usize + nx as usize) * 4;
                    out[idx..idx + 4].copy_from_slice(&px);
                }
            }
        }
    }

    out
}

fn resize_canvas_buffer(
    width: u32,
    height: u32,
    pixels: &[u8],
    new_width: u32,
    new_height: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; new_width as usize * new_height as usize * 4];
    let copy_w = width.min(new_width) as usize;
    let copy_h = height.min(new_height) as usize;
    for y in 0..copy_h {
        let src_start = (y * width as usize) * 4;
        let dst_start = (y * new_width as usize) * 4;
        let len = copy_w * 4;
        if let (Some(src), Some(dst)) = (
            pixels.get(src_start..src_start + len),
            out.get_mut(dst_start..dst_start + len),
        ) {
            dst.copy_from_slice(src);
        }
    }
    out
}

fn crop_buffer(width: u32, height: u32, pixels: &[u8], rect: IRect) -> Vec<u8> {
    let rect = rect.clamp_to(width, height);
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let src_start = ((rect.y0 + y as i32) as usize * width as usize + rect.x0 as usize) * 4;
        let dst_start = y * w * 4;
        let len = w * 4;
        if let (Some(src), Some(dst)) = (
            pixels.get(src_start..src_start + len),
            out.get_mut(dst_start..dst_start + len),
        ) {
            dst.copy_from_slice(src);
        }
    }
    out
}

/// 双線形補間で 4 近傍から 1 画素を求める(`resize_buffer` の
/// `Interpolation::Bilinear` が使う)。straight alpha のままチャンネルごとに
/// 線形補間する簡易実装(SPEC は premultiplied 前提の高精度リサイズまでは
/// 要求していない)。
fn bilerp(p00: [u8; 4], p10: [u8; 4], p01: [u8; 4], p11: [u8; 4], fx: f32, fy: f32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
        let bottom = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
        let v = top * (1.0 - fy) + bottom * fy;
        out[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_white_fills_opaque_white() {
        let doc = Document::new(4, 3, Background::White);
        assert_eq!(doc.width, 4);
        assert_eq!(doc.height, 3);
        assert_eq!(doc.active_pixels().len(), 4 * 3 * 4);
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));
        assert!(!doc.modified);
        assert!(doc.path.is_none());
        assert!(doc.dirty.is_empty());
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "背景");
        assert_eq!(doc.active, 0);
    }

    #[test]
    fn new_transparent_fills_zero_alpha() {
        let doc = Document::new(2, 2, Background::Transparent);
        assert_eq!(doc.active_pixels().len(), 2 * 2 * 4);
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 0]));
    }

    #[test]
    fn new_zero_size_does_not_panic() {
        let doc = Document::new(0, 0, Background::White);
        assert_eq!(doc.width, 0);
        assert_eq!(doc.height, 0);
        assert_eq!(doc.active_pixels().len(), 0);
    }

    #[test]
    fn new_composite_matches_single_layer_exactly() {
        // 単一の不透明・表示ありレイヤーの合成は、そのレイヤー自身の画素と
        // 厳密に一致するはず(v1 の全機能が 1 レイヤーで従来どおり動くことの
        // 土台、ARCHITECTURE.md §14.8 の受け入れ基準)。
        let doc = Document::new(3, 3, Background::White);
        assert_eq!(doc.composite, doc.active_pixels());
    }

    #[test]
    fn project_snapshot_constructor_rebuilds_composite_and_state() {
        let bottom = Layer {
            name: "背景".to_owned(),
            visible: true,
            opacity: 255,
            pixels: vec![10, 20, 30, 255],
        };
        let top = Layer {
            name: "上".to_owned(),
            visible: false,
            opacity: 255,
            pixels: vec![200, 210, 220, 255],
        };
        let doc = Document::try_from_snapshot_owned(
            DocSnapshot {
                width: 1,
                height: 1,
                layers: vec![bottom, top],
                active: 1,
            },
            None,
            true,
        )
        .expect("validated project snapshot");

        assert_eq!(doc.active, 1);
        assert!(doc.modified);
        assert_eq!(doc.composite, [10, 20, 30, 255]);
    }

    #[test]
    fn project_snapshot_constructor_rejects_empty_layers() {
        let result = Document::try_from_snapshot_owned(
            DocSnapshot {
                width: 1,
                height: 1,
                layers: Vec::new(),
                active: 0,
            },
            None,
            false,
        );

        assert!(matches!(result, Err(message) if message.contains("レイヤー")));
    }

    #[test]
    fn irect_is_constructible_with_half_open_fields() {
        let r = IRect {
            x0: 2,
            y0: 3,
            x1: 10,
            y1: 5,
        };
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (2, 3, 10, 5));
    }

    #[test]
    fn irect_width_height_and_is_empty() {
        let r = IRect {
            x0: 2,
            y0: 3,
            x1: 10,
            y1: 7,
        };
        assert_eq!(r.width(), 8);
        assert_eq!(r.height(), 4);
        assert!(!r.is_empty());

        let empty = IRect {
            x0: 5,
            y0: 5,
            x1: 5,
            y1: 5,
        };
        assert!(empty.is_empty());
        assert_eq!(empty.width(), 0);

        let inverted = IRect {
            x0: 5,
            y0: 5,
            x1: 1,
            y1: 1,
        };
        assert!(inverted.is_empty());
        assert_eq!(inverted.width(), 0);
        assert_eq!(inverted.height(), 0);
    }

    #[test]
    fn irect_union_covers_both_rects() {
        let a = IRect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 5,
        };
        let b = IRect {
            x0: 3,
            y0: -2,
            x1: 10,
            y1: 4,
        };
        let u = a.union(&b);
        assert_eq!((u.x0, u.y0, u.x1, u.y1), (0, -2, 10, 5));
    }

    #[test]
    fn irect_union_with_empty_returns_other() {
        let a = IRect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 5,
        };
        let empty = IRect {
            x0: 9,
            y0: 9,
            x1: 9,
            y1: 9,
        };
        assert_eq!(a.union(&empty), a);
        assert_eq!(empty.union(&a), a);
    }

    #[test]
    fn irect_clamp_to_bounds() {
        let r = IRect {
            x0: -5,
            y0: -5,
            x1: 15,
            y1: 15,
        };
        let clamped = r.clamp_to(10, 8);
        assert_eq!(
            (clamped.x0, clamped.y0, clamped.x1, clamped.y1),
            (0, 0, 10, 8)
        );
    }

    #[test]
    fn irect_clamp_to_zero_size_does_not_panic() {
        let r = IRect {
            x0: -1,
            y0: -1,
            x1: 1,
            y1: 1,
        };
        let clamped = r.clamp_to(0, 0);
        assert!(clamped.is_empty());
    }

    #[test]
    fn get_set_pixel_round_trip() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.set_pixel(1, 2, [10, 20, 30, 40]);
        assert_eq!(doc.get_pixel(1, 2), Some([10, 20, 30, 40]));
        assert_eq!(doc.get_pixel(0, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn get_set_pixel_out_of_bounds_does_not_panic() {
        let mut doc = Document::new(4, 4, Background::White);
        assert_eq!(doc.get_pixel(-1, 0), None);
        assert_eq!(doc.get_pixel(4, 0), None);
        assert_eq!(doc.get_pixel(0, 4), None);
        // out-of-bounds 書き込みは無視される(パニックしない)。
        doc.set_pixel(-1, -1, [1, 2, 3, 4]);
        doc.set_pixel(100, 100, [1, 2, 3, 4]);
    }

    #[test]
    fn mark_dirty_keeps_segments_separate_instead_of_bbox_union() {
        // v4 §16.1: 離れた矩形を 1 個の外接矩形へ union してしまうと、その間の
        // 触れていない領域まで recomposite/テクスチャ部分更新の対象になり、
        // 高速ドラッグで「ドラッグを横断する巨大 dirty 矩形」(SPEC §28)が
        // できてしまう。新しい実装は各セグメントを個別に保持する。
        let mut doc = Document::new(10, 10, Background::White);
        doc.mark_dirty(IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        });
        doc.mark_dirty(IRect {
            x0: 5,
            y0: 5,
            x1: 7,
            y1: 7,
        });
        let rects = doc.dirty.rects();
        assert_eq!(
            rects.len(),
            2,
            "expected two separate dirty segments, got {rects:?}"
        );
        assert!(rects.contains(&IRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2
        }));
        assert!(rects.contains(&IRect {
            x0: 5,
            y0: 5,
            x1: 7,
            y1: 7
        }));
    }

    #[test]
    fn mark_dirty_clamps_to_document_bounds() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.mark_dirty(IRect {
            x0: -5,
            y0: -5,
            x1: 100,
            y1: 100,
        });
        let rects = doc.dirty.rects();
        assert_eq!(rects.len(), 1);
        assert_eq!(
            rects[0],
            IRect {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 4
            }
        );
    }

    #[test]
    fn mark_all_dirty_covers_full_image() {
        let mut doc = Document::new(6, 5, Background::White);
        doc.mark_all_dirty();
        assert_eq!(
            doc.dirty.rects(),
            &[IRect {
                x0: 0,
                y0: 0,
                x1: 6,
                y1: 5
            }]
        );
    }

    #[test]
    fn mark_all_dirty_clears_previously_queued_segments() {
        let mut doc = Document::new(6, 5, Background::White);
        doc.mark_dirty(IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        });
        doc.mark_all_dirty();
        // 全面 dirty は他のどのセグメントも覆うので、古いセグメントを残さない。
        assert_eq!(doc.dirty.rects().len(), 1);
    }

    #[test]
    fn dirty_region_caps_segment_count_by_merging_into_the_last_one() {
        // v4 §16.1: 上限(32)を超えて積もうとしたら末尾のセグメントへ union
        // する(セグメント数を無制限に増やさない)。
        let mut doc = Document::new(2000, 10, Background::White);
        for i in 0..40 {
            let x = i * 10;
            doc.mark_dirty(IRect {
                x0: x,
                y0: 0,
                x1: x + 2,
                y1: 2,
            });
        }
        assert!(
            doc.dirty.rects().len() <= 32,
            "expected the segment count to stay capped, got {}",
            doc.dirty.rects().len()
        );
    }

    // -- v2: レイヤー合成(ARCHITECTURE.md §14.8 受け入れ基準) --------------

    #[test]
    fn recomposite_skips_hidden_layers() {
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 2, 2, [255, 0, 0, 255]);
        let mut top = Layer::filled("上", 2, 2, [0, 255, 0, 255]);
        top.visible = false;
        doc.layers.push(top);
        doc.active = 0;
        doc.recomposite_full();
        assert_eq!(doc.composite_pixel(0, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn recomposite_applies_layer_opacity() {
        let mut doc = Document::new(1, 1, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 1, 1, [255, 255, 255, 255]);
        let mut top = Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        top.opacity = 128; // 約 50%
        doc.layers.push(top);
        doc.recomposite_full();
        let px = doc.composite_pixel(0, 0).unwrap();
        // 白の上に 50% 黒 -> 中間のグレーになるはず。
        assert!(px[0] > 100 && px[0] < 160, "got {px:?}");
        assert_eq!(px[3], 255);
    }

    #[test]
    fn recomposite_blends_multiple_visible_layers_bottom_to_top() {
        let mut doc = Document::new(1, 1, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 1, 1, [255, 0, 0, 255]);
        doc.layers.push(Layer::filled("上", 1, 1, [0, 255, 0, 128]));
        doc.recomposite_full();
        let px = doc.composite_pixel(0, 0).unwrap();
        // 上層は半透明の緑 -> 下層の赤と混ざり、緑が優位になるはず。
        assert!(px[1] > px[0], "green should dominate over red, got {px:?}");
    }

    #[test]
    fn recomposite_only_touches_requested_rect() {
        let mut doc = Document::new(4, 4, Background::Transparent);
        doc.layers[0] = Layer::filled("背景", 4, 4, [10, 20, 30, 255]);
        doc.recomposite_full();
        // 合成後にレイヤーを直接いじり、rect 外は古いままであることを確認する。
        doc.layers[0].pixels = [99, 99, 99, 255].repeat(16);
        doc.recomposite(IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        });
        assert_eq!(doc.composite_pixel(0, 0), Some([99, 99, 99, 255]));
        assert_eq!(doc.composite_pixel(3, 3), Some([10, 20, 30, 255]));
    }

    #[test]
    fn recompose_if_dirty_is_noop_when_not_dirty() {
        let mut doc = Document::new(2, 2, Background::White);
        doc.dirty.clear();
        doc.composite[0] = 7; // 手動で汚しても recompose_if_dirty は触らない。
        doc.recompose_if_dirty();
        assert_eq!(doc.composite[0], 7);
    }

    // -- v2: undo/redo スナップショット --------------------------------------

    #[test]
    fn snapshot_round_trips_through_apply_snapshot() {
        let mut doc = Document::new(3, 3, Background::White);
        doc.set_pixel(1, 1, [1, 2, 3, 4]);
        let before = doc.snapshot();

        doc.layers.push(Layer::filled("追加", 3, 3, [9, 9, 9, 9]));
        doc.active = 1;
        assert_eq!(doc.layers.len(), 2);

        doc.apply_snapshot(&before);
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
        assert_eq!(doc.get_pixel(1, 1), Some([1, 2, 3, 4]));
        assert_eq!(
            doc.composite.len(),
            doc.width as usize * doc.height as usize * 4
        );
    }

    // -- M4: 画像メニュー操作(SPEC §13: 全レイヤーに適用) --------------------

    #[test]
    fn flip_horizontal_mirrors_columns() {
        let mut doc = Document::new(3, 2, Background::Transparent);
        doc.set_pixel(0, 0, [1, 0, 0, 255]);
        doc.set_pixel(2, 0, [2, 0, 0, 255]);
        doc.flip_horizontal();
        assert_eq!(doc.get_pixel(0, 0), Some([2, 0, 0, 255]));
        assert_eq!(doc.get_pixel(2, 0), Some([1, 0, 0, 255]));
        assert_eq!(doc.width, 3);
        assert_eq!(doc.height, 2);
    }

    #[test]
    fn flip_horizontal_odd_width_center_column_unchanged() {
        let mut doc = Document::new(3, 1, Background::Transparent);
        doc.set_pixel(1, 0, [9, 9, 9, 9]);
        doc.flip_horizontal();
        assert_eq!(doc.get_pixel(1, 0), Some([9, 9, 9, 9]));
    }

    #[test]
    fn flip_vertical_mirrors_rows() {
        let mut doc = Document::new(2, 3, Background::Transparent);
        doc.set_pixel(0, 0, [1, 0, 0, 255]);
        doc.set_pixel(0, 2, [2, 0, 0, 255]);
        doc.flip_vertical();
        assert_eq!(doc.get_pixel(0, 0), Some([2, 0, 0, 255]));
        assert_eq!(doc.get_pixel(0, 2), Some([1, 0, 0, 255]));
    }

    #[test]
    fn flip_zero_size_does_not_panic() {
        let mut doc = Document::new(0, 0, Background::Transparent);
        doc.flip_horizontal();
        doc.flip_vertical();
        assert_eq!(doc.active_pixels().len(), 0);
    }

    #[test]
    fn flip_applies_to_all_layers() {
        let mut doc = Document::new(2, 1, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 2, 1, [0, 0, 0, 0]);
        doc.layers[0].pixels[0..4].copy_from_slice(&[1, 0, 0, 255]);
        doc.layers.push(Layer::filled("上", 2, 1, [0, 0, 0, 0]));
        doc.layers[1].pixels[0..4].copy_from_slice(&[2, 0, 0, 255]);
        doc.flip_horizontal();
        assert_eq!(&doc.layers[0].pixels[4..8], &[1, 0, 0, 255]);
        assert_eq!(&doc.layers[1].pixels[4..8], &[2, 0, 0, 255]);
    }

    #[test]
    fn rotate_cw_swaps_dimensions_and_corners() {
        // 3(w) x 2(h) の四隅に印を付けて回転後の位置を確認する。
        let mut doc = Document::new(3, 2, Background::Transparent);
        doc.set_pixel(0, 0, [1, 0, 0, 255]); // top-left
        doc.set_pixel(2, 0, [2, 0, 0, 255]); // top-right
        doc.set_pixel(0, 1, [3, 0, 0, 255]); // bottom-left
        doc.set_pixel(2, 1, [4, 0, 0, 255]); // bottom-right
        doc.rotate_cw();
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 3);
        // top-left の画素は右上へ移動する。
        assert_eq!(doc.get_pixel(1, 0), Some([1, 0, 0, 255]));
        // top-right は右下へ。
        assert_eq!(doc.get_pixel(1, 2), Some([2, 0, 0, 255]));
        // bottom-left は左上へ。
        assert_eq!(doc.get_pixel(0, 0), Some([3, 0, 0, 255]));
        // bottom-right は左下へ。
        assert_eq!(doc.get_pixel(0, 2), Some([4, 0, 0, 255]));
        assert_eq!(doc.composite.len(), 2 * 3 * 4);
    }

    #[test]
    fn rotate_ccw_is_inverse_of_rotate_cw() {
        let mut doc = Document::new(5, 3, Background::Transparent);
        for y in 0..3 {
            for x in 0..5 {
                doc.set_pixel(x, y, [(x * 10 + y) as u8, 0, 0, 255]);
            }
        }
        let original = doc.active_pixels().to_vec();
        doc.rotate_cw();
        doc.rotate_ccw();
        assert_eq!(doc.width, 5);
        assert_eq!(doc.height, 3);
        assert_eq!(doc.active_pixels(), original.as_slice());
    }

    #[test]
    fn rotate_zero_size_does_not_panic() {
        let mut doc = Document::new(0, 5, Background::Transparent);
        doc.rotate_cw();
        assert_eq!((doc.width, doc.height), (5, 0));
    }

    #[test]
    fn resize_changes_dimensions_and_keeps_buffer_consistent() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.resize(8, 2, Interpolation::Nearest);
        assert_eq!(doc.width, 8);
        assert_eq!(doc.height, 2);
        assert_eq!(doc.active_pixels().len(), 8 * 2 * 4);
        assert_eq!(doc.composite.len(), 8 * 2 * 4);
    }

    #[test]
    fn resize_nearest_upsize_preserves_flat_color() {
        let doc0 = Document::new(2, 2, Background::White);
        let mut doc = doc0;
        doc.resize(4, 4, Interpolation::Nearest);
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn resize_bilinear_upsize_preserves_flat_color() {
        let mut doc = Document::new(2, 2, Background::White);
        doc.resize(6, 6, Interpolation::Bilinear);
        assert!(doc
            .active_pixels()
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn resize_to_zero_does_not_panic() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.resize(0, 0, Interpolation::Bilinear);
        assert_eq!(doc.active_pixels().len(), 0);
    }

    #[test]
    fn resize_from_zero_size_does_not_panic() {
        let mut doc = Document::new(0, 0, Background::Transparent);
        doc.resize(4, 4, Interpolation::Bilinear);
        assert_eq!(doc.active_pixels().len(), 4 * 4 * 4);
    }

    #[test]
    fn resize_canvas_keeps_existing_pixels_top_left_and_extends_transparent() {
        let mut doc = Document::new(2, 2, Background::White);
        doc.resize_canvas(4, 3);
        assert_eq!(doc.width, 4);
        assert_eq!(doc.height, 3);
        // 既存の 2x2 は左上に White のまま残る。
        assert_eq!(doc.get_pixel(0, 0), Some([255, 255, 255, 255]));
        assert_eq!(doc.get_pixel(1, 1), Some([255, 255, 255, 255]));
        // 拡張部分は透明。
        assert_eq!(doc.get_pixel(3, 0), Some([0, 0, 0, 0]));
        assert_eq!(doc.get_pixel(0, 2), Some([0, 0, 0, 0]));
    }

    #[test]
    fn resize_canvas_smaller_crops_from_bottom_right() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.set_pixel(0, 0, [1, 2, 3, 4]);
        doc.resize_canvas(2, 2);
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 2);
        assert_eq!(doc.get_pixel(0, 0), Some([1, 2, 3, 4]));
    }

    #[test]
    fn crop_to_extracts_subregion() {
        let mut doc = Document::new(10, 10, Background::Transparent);
        doc.set_pixel(5, 5, [7, 7, 7, 255]);
        doc.crop_to(IRect {
            x0: 4,
            y0: 4,
            x1: 7,
            y1: 7,
        });
        assert_eq!(doc.width, 3);
        assert_eq!(doc.height, 3);
        assert_eq!(doc.get_pixel(1, 1), Some([7, 7, 7, 255]));
    }

    #[test]
    fn crop_to_clamps_out_of_bounds_rect() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.crop_to(IRect {
            x0: -5,
            y0: -5,
            x1: 100,
            y1: 100,
        });
        assert_eq!(doc.width, 4);
        assert_eq!(doc.height, 4);
    }

    #[test]
    fn crop_to_empty_rect_yields_zero_size_document() {
        let mut doc = Document::new(4, 4, Background::White);
        doc.crop_to(IRect {
            x0: 2,
            y0: 2,
            x1: 2,
            y1: 2,
        });
        assert_eq!(doc.width, 0);
        assert_eq!(doc.height, 0);
        assert_eq!(doc.active_pixels().len(), 0);
    }

    // -- V2-M2: レイヤー構造の操作(ARCHITECTURE.md §14.8 受け入れ基準) ------

    #[test]
    fn add_layer_inserts_transparent_layer_above_active_and_activates_it() {
        let mut doc = Document::new(2, 2, Background::White);
        assert!(doc.add_layer("レイヤー 1".to_owned()));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[1].name, "レイヤー 1");
        assert!(doc.layers[1].visible);
        assert_eq!(doc.layers[1].opacity, 255);
        assert!(doc.layers[1]
            .pixels
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 0]));
        // 下のレイヤーは触れられていない。
        assert!(doc.layers[0]
            .pixels
            .chunks_exact(4)
            .all(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn add_layer_refuses_past_max_layers() {
        let mut doc = Document::new(1, 1, Background::White);
        for _ in 0..(MAX_LAYERS - 1) {
            assert!(doc.add_layer("レイヤー".to_owned()));
        }
        assert_eq!(doc.layers.len(), MAX_LAYERS);
        assert!(!doc.add_layer("溢れ".to_owned()));
        assert_eq!(doc.layers.len(), MAX_LAYERS);
    }

    #[test]
    fn duplicate_active_layer_copies_pixels_and_activates_copy() {
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.set_pixel(0, 0, [1, 2, 3, 4]);
        assert!(doc.duplicate_active_layer());
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[1].pixels, doc.layers[0].pixels);
        assert_eq!(doc.layers[1].name, doc.layers[0].name);
    }

    #[test]
    fn remove_active_layer_refuses_when_only_one_layer() {
        let mut doc = Document::new(2, 2, Background::White);
        assert!(!doc.remove_active_layer());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn remove_active_layer_removes_and_shifts_active_into_its_place() {
        let mut doc = Document::new(2, 2, Background::White);
        doc.layers[0].name = "下".to_owned();
        doc.add_layer("中".to_owned());
        doc.add_layer("上".to_owned());
        doc.active = 1; // 「中」をアクティブに。
        assert!(doc.remove_active_layer());
        assert_eq!(doc.layers.len(), 2);
        // 「中」が消え、繰り上がった「上」がこの位置に来ているはず。
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[doc.active].name, "上");
    }

    #[test]
    fn remove_topmost_active_layer_clamps_active_to_new_top() {
        // ARCHITECTURE.md §14.9-4: 最上位を削除したときの off-by-one 対策。
        let mut doc = Document::new(2, 2, Background::White);
        doc.add_layer("上".to_owned());
        assert_eq!(doc.active, 1);
        assert!(doc.remove_active_layer());
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
    }

    #[test]
    fn move_active_layer_up_and_down_swap_and_track_active() {
        let mut doc = Document::new(1, 1, Background::White);
        doc.layers[0].name = "下".to_owned();
        doc.add_layer("上".to_owned());
        // いま active=1(「上」)。最上位なので上へは移動できない。
        assert!(!doc.move_active_layer_up());
        assert!(doc.move_active_layer_down());
        assert_eq!(doc.active, 0);
        assert_eq!(doc.layers[0].name, "上");
        assert_eq!(doc.layers[1].name, "下");
        // いま最下位なので下へは移動できない。
        assert!(!doc.move_active_layer_down());
        assert!(doc.move_active_layer_up());
        assert_eq!(doc.active, 1);
        assert_eq!(doc.layers[0].name, "下");
        assert_eq!(doc.layers[1].name, "上");
    }

    #[test]
    fn merge_active_down_refuses_for_bottom_layer_or_single_layer() {
        let mut doc = Document::new(1, 1, Background::White);
        assert!(!doc.merge_active_down(), "single layer must refuse");

        doc.add_layer("上".to_owned());
        doc.active = 0;
        assert!(!doc.merge_active_down(), "bottom layer has nothing below");
    }

    #[test]
    fn merge_active_down_blends_and_removes_upper_layer() {
        let mut doc = Document::new(1, 1, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 1, 1, [255, 255, 255, 255]);
        let mut upper = Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        upper.opacity = 128; // 約 50% 黒
        doc.layers.push(upper);
        doc.active = 1;

        assert!(doc.merge_active_down());
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
        assert!(doc.layers[0].visible);
        assert_eq!(doc.layers[0].opacity, 255);
        // 白の上に 50% 黒 -> 中間のグレーになるはず(recomposite と同じ規則)。
        let px = doc.layers[0].pixels[0..4].to_vec();
        assert!(px[0] > 100 && px[0] < 160, "got {px:?}");
        assert_eq!(px[3], 255);
    }

    #[test]
    fn merge_active_down_skips_hidden_layers_contribution() {
        let mut doc = Document::new(1, 1, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 1, 1, [10, 20, 30, 255]);
        let mut upper = Layer::filled("上", 1, 1, [0, 0, 0, 255]);
        upper.visible = false;
        doc.layers.push(upper);
        doc.active = 1;

        assert!(doc.merge_active_down());
        // 非表示レイヤーは寄与しないので、下のレイヤーの色がそのまま残る。
        assert_eq!(doc.layers[0].pixels[0..4], [10, 20, 30, 255]);
    }

    #[test]
    fn flatten_all_refuses_when_only_one_layer() {
        let mut doc = Document::new(2, 2, Background::White);
        assert!(!doc.flatten_all());
        assert_eq!(doc.layers.len(), 1);
    }

    #[test]
    fn flatten_all_collapses_to_single_visible_layer_matching_composite() {
        let mut doc = Document::new(2, 2, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 2, 2, [255, 0, 0, 255]);
        doc.layers.push(Layer::filled("上", 2, 2, [0, 255, 0, 128]));
        doc.recomposite_full();
        let expected = doc.composite.clone();

        assert!(doc.flatten_all());
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, 0);
        assert_eq!(doc.layers[0].name, "背景");
        assert_eq!(doc.layers[0].opacity, 255);
        assert!(doc.layers[0].visible);
        assert_eq!(doc.layers[0].pixels, expected);
    }

    #[test]
    fn active_index_clamps_when_active_is_out_of_range() {
        let mut doc = Document::new(1, 1, Background::White);
        doc.active = 99;
        assert_eq!(doc.active_index(), 0);
    }

    // -- v4 §16.1: recomposite の行スライス化(回帰検知用の緩い時間上限) ------

    #[test]
    fn recomposite_full_multi_layer_4000x4000_is_correct_and_terminates_quickly() {
        // ARCHITECTURE.md §16.1: 「recomposite … の行処理を chunks_exact(4)
        // ベースの行スライスに書き直し」「release の時間計測テスト(緩い上限
        // +回帰検知)を追加」。`cargo test` は最適化なしのデバッグビルドで
        // 走る(境界チェック付きピクセルアクセスの定数倍がリリースの数十倍
        // になりうる、raster.rs の flood_fill 4000x4000 テストと同じ注記)
        // ため、ここでは実際の性能目標(SPEC §28)そのものではなく、
        // O(n^2) 的な劣化や無限ループが無いことだけを緩い上限で保証する。
        // 実測のリリースビルド計測は別途行う。
        let mut doc = Document::new(4000, 4000, Background::Transparent);
        doc.layers[0] = Layer::filled("下", 4000, 4000, [255, 0, 0, 255]);
        doc.layers
            .push(Layer::filled("上", 4000, 4000, [0, 255, 0, 128]));
        doc.layers
            .push(Layer::filled("最上", 4000, 4000, [0, 0, 255, 64]));

        let start = std::time::Instant::now();
        doc.recomposite_full();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 10,
            "recomposite_full took suspiciously long (possible regression): {elapsed:?}"
        );
        // 3 層(不透明赤 → 半透明緑 → 薄い青)を合成した結果が透明ではない
        // ことだけ、正しさの簡易確認として見ておく。
        let px = doc.composite_pixel(0, 0).unwrap();
        assert_eq!(px[3], 255);
    }
}
