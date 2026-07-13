# Darask Paint — アーキテクチャ設計書 (v1)

実装者へ: まず [SPEC.md](SPEC.md) を全部読むこと。本書は「どう作るか」を定める。
egui の API はバージョンで変わる。本書のコード断片は**意図を示す擬似コード**であり、実際に解決されたバージョンの API(コンパイルエラーと docs.rs で確認)に合わせて書くこと。ただし**挙動の仕様は変えない**。

## 1. モジュール構成

```
src/
  main.rs        — エントリ。Instant記録、CLI引数、NativeOptions、フォント設定、App起動
  app.rs         — DaraskApp (eframe::App)。全状態の所有、レイアウト、ショートカット、モーダル管理
  document.rs    — Document(ピクセルバッファ)と全画像操作(resize/flip/rotate/crop)
  canvas_view.rs — ズーム/パン状態、座標変換、テクスチャ管理、ポインタ→ToolEventディスパッチ
  history.rs     — アンドゥ/リドゥ(タイルCoW + パッチ)
  raster.rs      — 低レベル描画: スタンプ、線、矩形、楕円、flood fill、合成
  io.rs          — 開く/保存、クリップボード、D&D、ファイルダイアログ
  tools/
    mod.rs       — Tool トレイト、ToolKind、ToolCtx
    pen.rs eraser.rs shapes.rs fill.rs picker.rs select.rs pan.rs
  ui/
    toolbar.rs options_bar.rs status_bar.rs dialogs.rs menu.rs
```

## 2. 中核データ構造

```rust
/// RGBA8・行優先。座標は左上原点、x右・y下。
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,          // len == w*h*4
    pub path: Option<PathBuf>,
    pub modified: bool,
    pub dirty: Option<IRect>,     // 前フレーム以降に変更された領域(テクスチャ部分更新用)
}

pub struct IRect { pub x0: i32, pub y0: i32, pub x1: i32, pub y1: i32 } // 半開区間 [x0,x1)
```

- ピクセルアクセスは必ず境界クランプ/チェックを通す。範囲外書き込みでパニックしないこと。
- 描画関数は変更領域を `dirty` にマージする。

### 座標系(重要・純関数としてテスト必須)

3 つの座標系を厳密に区別する:
1. **画像ピクセル座標** (f32) — ドキュメント上の位置
2. **スクリーン論理ポイント** (egui Pos2)
3. **物理ピクセル** = 論理ポイント × `pixels_per_point (ppp)`

状態: `zoom: f32`(zoom=1.0 で画像1px=物理1px)、`pan: Vec2`(ビューポート原点から画像原点への論理ポイントオフセット)。

```
img_to_screen(p) = viewport.min + pan + p * (zoom / ppp)
screen_to_img(s) = (s - viewport.min - pan) * (ppp / zoom)
```

- カーソル中心ズーム: カーソル下の画像点が不動になるよう `pan` を再計算。
- 100% でくっきり表示するため、画像描画矩形の原点は物理ピクセル格子に丸める: `(x*ppp).round()/ppp`。

## 3. キャンバス描画

- egui の `TextureHandle` を 1 枚持つ(ドキュメント全体、RGBA)。
- `TextureOptions { magnification: Nearest, minification: Linear, .. }`。
- 毎フレーム: `doc.dirty` があればその矩形だけ `ImageDelta::partial(pos, sub_image, options)` で部分更新(`ctx.tex_manager()` 経由、または該当バージョンの部分更新 API)。**全面再アップロードは 新規/開く/サイズ変更時のみ**。
- キャンバスウィジェット: 中央パネル全域を `ui.allocate_rect(rect, Sense::click_and_drag())`。`painter.image(...)` で市松模様(小さなリピートテクスチャか矩形塗り)→ 画像 → ツールプレビュー → 選択枠(破線)の順に描画。
- 右クリック描画があるため、キャンバスにはコンテキストメニューを付けない。

### 再描画ポリシー(アイドル 0% の要)

egui はイベント駆動で再描画される。**無条件の `request_repaint()` を書かない**。例外はトーストの消滅タイマー(`request_repaint_after(残り時間)`)のみ。アニメーションなし。

## 4. ツールシステム

```rust
pub enum ToolEvent {
    Down { img: Pos2, button: PointerButton, mods: Modifiers },
    Drag { img: Pos2, button: PointerButton, mods: Modifiers },
    Up   { img: Pos2, button: PointerButton },
    Hover{ img: Pos2 },
}

pub trait Tool {
    fn event(&mut self, ev: ToolEvent, ctx: &mut ToolCtx);
    /// ドキュメントに触れないプレビュー(直線/図形/選択のドラッグ中表示)
    fn draw_preview(&self, painter: &egui::Painter, view: &CanvasView);
    fn cursor(&self) -> egui::CursorIcon;
}

pub struct ToolCtx<'a> {
    pub doc: &'a mut Document,
    pub history: &'a mut History,
    pub primary: Color32,
    pub secondary: Color32,
    pub brush_size: f32,
    // ツール固有オプション(fill許容値、図形モード、AA)もここ経由
}
```

- ポインタイベントの間隔が開いても線が途切れないよう、ペン/消しゴムは**前回位置と今回位置を線分で補間**してスタンプする。
- Space 押下中・中ボタンは、現在のツールに関係なく canvas_view がパンとして横取りする。
- Alt+クリックは描画ツール中でも一時スポイト。

## 5. ラスタ演算 (raster.rs)

すべて純関数的(Document とプリミティブ引数のみ)。ユニットテスト必須。

- `stamp_round(doc, cx, cy, radius, color, erase: bool)` — ハードエッジ円。erase は alpha=0 を書く。
- `stroke_segment(doc, from, to, radius, color, erase)` — 線分に沿ってスタンプ(間隔 ≤ max(1px, radius/2))。
- `draw_rect / fill_rect / draw_ellipse / fill_ellipse` — 枠線は stroke_segment ベース(太さ=ブラシ)。楕円は媒介変数 or 中点法。
- `flood_fill(doc, x, y, color, tolerance)` — スキャンライン法(スタック、再帰禁止)。tolerance は各チャンネル差の最大値で判定。開始色と塗色が同一なら no-op。4000×4000 全面でも 100ms 未満。
- `blend_over(dst, src)` — straight-alpha の source-over 合成。
- ペンの AA モード(オプション、M3): ストローク中は**カバレッジマスク**(ドキュメント寸法の `Vec<u8>`、遅延確保・再利用)に `max` で書き、undo 用に保存した CoW タイル(§6)を元画像として `out = blend(元, color, coverage)` で合成する。スタンプ重ね塗りによる縁の濃度ムラを防ぐため。ハードエッジ時はマスク不要で直接描いてよい。

## 6. アンドゥ / リドゥ (history.rs)

**タイル Copy-on-Write 方式**(タイル = 256×256):

1. ストローク開始時に空の `HashMap<(u32,u32), Box<[u8]>>` を用意。
2. raster 関数がタイルに初めて書く前に、そのタイルの元ピクセルを保存(ToolCtx 経由のフック、または「ストローク開始時に触れる予定はないので書き込み時に呼ぶ `ensure_tile_saved(tx,ty)`」)。
3. ストローク確定時: 触れたタイル群の bbox から `Patch { rect, before, after }` を作って push(before はタイルから復元、after は現在の doc から複写)。

```rust
pub enum HistoryOp {
    Patch { rect: IRect, before: Vec<u8>, after: Vec<u8> },
    /// サイズが変わる操作(resize/crop/rotate/canvas resize)
    Replace { before: (u32, u32, Vec<u8>), after: (u32, u32, Vec<u8>) },
}
```

- undo スタックと redo スタック。新規 push で redo クリア。
- メモリ会計: 全 op のバイト合計 ≤ 256MB。超過時は最古を破棄(ただし直近 10 件は保持)。
- 適用/巻き戻しは `dirty` を設定すること。

## 7. 選択・フローティング (tools/select.rs)

```rust
pub struct Selection { pub rect: IRect }
pub struct Floating {
    pub pixels: Vec<u8>, pub w: u32, pub h: u32,
    pub pos: Pos2,              // 画像座標(f32、キャンバス外もはみ出し可)
    pub cut_from: Option<IRect> // 浮動化時に透明で埋めた元領域(undo一体化用)
}
```

- 浮動化(選択内ドラッグ開始時): 領域ピクセルを Floating に複写し、元領域を透明化。この透明化は**まだ履歴に積まない**。
- 確定(Enter/外クリック/ツール切替/Esc): 浮動片を doc に合成し、「切り出し元の透明化+合成先」をまとめて 1 つの Patch にする(bbox = 両領域の合併)。
- 浮動片の表示は独立した小テクスチャで canvas_view が描く。
- 選択枠の破線は painter の線分で描く(点線はアニメーションさせない — 再描画ポリシー遵守)。

## 8. I/O (io.rs)

- `image` crate で読み込み → `to_rgba8()`。保存は拡張子で分岐。JPEG は白合成後 `image::codecs::jpeg::JpegEncoder::new_with_quality`。
- ダイアログ: `rfd::FileDialog`(ブロッキングで良い。ネイティブモーダルなので UI スレッドで可)。フィルタ設定必須。
- クリップボード: `arboard::Clipboard`、`get_image`/`set_image`(RGBA)。失敗はトーストで通知。
- D&D: `ctx.input(|i| i.raw.dropped_files)` → 未保存ガードを通して open。
- 未保存ガードは app.rs の `pending_action: Option<PendingAction>`(New/Open(path?)/Close)+確認モーダルの状態機械で一元化。

## 9. フォント(日本語表示の必須要件)

egui のデフォルトフォントに日本語グリフは**無い**。`App::new`(創建時に一度)で:

1. 次の順で最初に読めたものを使う:
   `C:\Windows\Fonts\YuGothM.ttc` → `C:\Windows\Fonts\meiryo.ttc` → `C:\Windows\Fonts\msgothic.ttc`
2. `.ttc` はコレクションなので `FontData` の `index`(通常 0)を設定。
3. `Proportional` と `Monospace` 両ファミリの**先頭**に挿入(欧文はデフォルトフォントが先でも可。tofu が出ない構成なら順序は任せる)。
4. 全部読めなければ警告ログだけ出して続行(Win11 では起きない想定)。

フォントは**バンドルしない**(バイナリサイズと起動時間のため)。

## 10. app.rs の状態機械

```rust
pub struct DaraskApp {
    doc: Document,
    view: CanvasView,
    history: History,
    tool: ToolKind, tools: /* 各ツールの状態 */,
    primary: Color32, secondary: Color32, brush_size: f32,
    recent_colors: VecDeque<Color32>,     // 最大8
    selection: Option<Selection>, floating: Option<Floating>,
    modal: Option<ModalState>,            // New/Resize/CanvasResize/JpegQuality/ConfirmUnsaved
    pending_action: Option<PendingAction>,
    toast: Option<(String, Instant)>,
    bench: Option<BenchState>,            // DARASK_BENCH=1 のとき
}
```

- update() の順序: ①ベンチ処理 ②close_requested 検知 ③ショートカット処理(`consume_shortcut`、単一キーは `!ctx.wants_keyboard_input()` ガード)④メニュー ⑤オプションバー ⑥ツールバー ⑦中央キャンバス ⑧ステータスバー ⑨モーダル。
- モーダル表示中はキャンバスへの入力を渡さない。

## 11. マイルストーン(実装順序・各段階でビルド緑必須)

### M1 — 骨組みとシェル
- `cargo init`、依存追加(`cargo add eframe rfd arboard` と `cargo add image --no-default-features -F png,jpeg,bmp,gif,webp`)、リリースプロファイル設定。
- main.rs(bench 計測開始、windows_subsystem、NativeOptions: 1280×800/最小640×480/タイトル/centered)。
- フォント設定(§9)。**メニューバー・ツールバー・オプションバー・ステータスバーのレイアウトを日本語ラベルで表示**(中身は未配線でよい)。
- document.rs の Document::new(白/透明)。
- ベンチモード(SPEC §11)を完動させる。
- 受け入れ: ビルド緑・`DARASK_BENCH=1` で起動→bench.txt が書かれ自動終了。日本語が tofu にならない。

### M2 — キャンバスと描画コア
- canvas_view.rs 完成: 座標変換(純関数+テスト)、ズーム/パン(Ctrl+ホイール、Space/中ボタン、H)、市松模様、テクスチャ部分更新、クランプ。
- raster.rs: stamp_round / stroke_segment / blend_over(+テスト)。
- ツール基盤(トレイト、ディスパッチ)+ ペン(ハードエッジ)/ 消しゴム。右ドラッグ=セカンダリ色。Alt+一時スポイト。
- history.rs 完成(タイル CoW、メモリ上限、テスト)+ Ctrl+Z/Y。
- 受け入れ: ズーム/パンした状態で描いてもカーソル直下に正しく描ける(座標変換テストで担保)。undo/redo が正しい。

### M3 — 残りの描画ツールと色
- 直線 / 矩形 / 楕円(プレビュー→確定、Shift 拘束、モード切替)。
- flood fill(+許容値テスト、境界テスト)。スポイト。
- 色 UI: スウォッチ+ピッカー、X 入替、最近使った色。ブラシサイズ UI と `[` `]`。
- ペンの AA オプション(§5 のマスク方式)。
- 受け入れ: 全ツールがオプションバー連動で動作。テスト緑。

### M4 — ファイル I/O・選択・仕上げ
- 開く/保存/名前を付けて保存/新規(ダイアログ)、JPEG 品質、CLI 引数、D&D、未保存ガード、タイトルバー表示。
- 選択ツール一式(§7)+ クリップボード(コピー/切り取り/貼り付け、白紙時の置き換え貼り付け)。
- 画像メニュー(サイズ変更/キャンバスサイズ/トリミング/反転/回転、Replace undo)。
- 表示メニュー、ステータスバー実データ、全ショートカット総配線、トースト。
- 受け入れ: SPEC の機能表・ショートカット表をすべて満たす。`cargo test` 緑、警告 0。

## 12. egui 実装上の注意(既知の落とし穴)

1. ショートカットは `ctx.input_mut(|i| i.consume_shortcut(...))`。Ctrl+Z 等がテキストフィールドと衝突しないよう消費順序に注意。
2. `close_requested()` → `ViewportCommand::CancelClose` はフレーム内で即送ること。
3. 部分テクスチャ更新の `pos` は物理テクスチャ座標(画像ピクセル)。dirty 矩形を画像境界にクランプしてから切り出す。
4. `Sense::click_and_drag` の response では `drag_started_by / dragged_by / drag_stopped_by` でボタン別に分岐。`interact_pointer_pos()` を使う。
5. ホイール値は `raw_scroll_delta` / `smooth_scroll_delta` の別・単位に注意。Ctrl 併用時は egui が `zoom_delta` に変換することがある(`zoom_delta()` も確認)。
6. 高 DPI: `ctx.pixels_per_point()` を毎フレーム取得。キャッシュしない。
7. 描画中(ドラッグ中)は入力イベントがフレームを駆動するので `request_repaint` は不要。
8. `arboard` の ImageData は所有バイト(Cow)。寸法 0 チェック。
9. rfd のダイアログ中はイベントループが止まる。ダイアログ呼び出しはフレーム処理の外側(update 内の最後や、フラグ経由の次フレーム冒頭)で行い、painter 借用と衝突させない。

## 13. テスト方針

`cargo test` で走るユニットテストのみ(GUI テスト不要):
- 座標変換の往復(img→screen→img が恒等、複数の zoom/ppp/pan)。
- stamp/segment: 端点が確実に塗られる、半径境界、画像端で OOB しない。
- flood_fill: 閉領域を越えない、tolerance の閾値通り、同色 no-op。
- history: patch 適用→undo で完全復元(バイト一致)、redo 一致、メモリ上限で最古破棄、直近10保持。
- Document の flip/rotate/resize/crop の寸法とピクセル位置。
- io: PNG 保存→読込ラウンドトリップ(temp dir 使用)。

---

# v2 設計(レイヤー / カラーパネル / アイコン / ハンドル)

SPEC.md の「v2 拡張仕様」(§13〜§16)に対応する設計。v1 の不変条件(再描画ポリシー、パニック禁止、警告 0、依存 4 crate 固定)はすべて維持する。

## 14.1 レイヤーデータモデルと合成

```rust
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: u8,        // 0-255(UI 表示は %)
    pub pixels: Vec<u8>,    // RGBA8、Document と同寸
}

pub struct Document {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>, // index 0 = 最下層
    pub active: usize,
    pub composite: Vec<u8>, // 可視レイヤーの合成キャッシュ(RGBA、チェッカー含まず)
    pub dirty: Option<IRect>,
    pub path: Option<PathBuf>,
    pub modified: bool,
}
```

- **raster.rs はレイヤーを知らない**: 描画関数はピクセルバッファ(`&mut [u8]` + 幅高、または軽量な Surface ビュー)を受け取る形にリファクタし、呼び出し側がアクティブレイヤーのバッファを渡す。既存テストは新シグネチャに追随させる(挙動は不変)。
- `dirty` は「合成の再計算 + テクスチャ更新が必要な領域」。描画・レイヤー構造変更・表示/不透明度変更のすべてがここにマージする。
- 毎フレーム: `dirty` があれば `recomposite(rect)`(rect 内を透明から始めて可視レイヤーを下から straight-alpha + レイヤー不透明度で合成)→ その rect をテクスチャ部分更新。全レイヤー合成は dirty 領域のみなのでストローク中のコストは v1 と同水準。
- スポイトは `composite` を読む。塗りつぶし・選択・浮動片はアクティブレイヤーの `pixels` を読む。
- 保存: `composite` を全再計算してから書き出し(JPEG/BMP は白合成)。開く/新規は「背景」1 枚。

## 14.2 履歴の拡張

```rust
pub enum HistoryOp {
    Patch { layer: usize, rect: IRect, before: Vec<u8>, after: Vec<u8> },
    AddLayer { index: usize, name: String },                  // undo=削除
    RemoveLayer { index: usize, layer: Layer },               // undo=復元(複製の undo は RemoveLayer で表現)
    MoveLayer { from: usize, to: usize },
    MergeDown { index: usize, upper: Layer, lower_before: Vec<u8> },
    ReplaceAll { before: DocSnapshot, after: DocSnapshot },   // 全レイヤー+寸法(サイズ変更/回転/反転/トリミング/統合)
}
```

- 各 op は `active` の変化も復元する(op 内に before/after の active を持たせてよい)。
- 表示切替・不透明度は履歴に積まない(SPEC §13)。ただし変更時に dirty は立てる。
- タイル CoW(StrokeRecorder)は「アクティブレイヤーのバッファ」に対して動く。ストローク開始時のレイヤー index を記録し、Patch に焼き込む。
- レイヤー構造の変更・アンドゥは、浮動片/ストローク進行中は `end_active_gesture()`(v1 の修正で導入済み)で先に確定してから行う。**この確定順序を必ず守る**(v1 レビューで確定した破損パターンの再発防止)。

## 14.3 カラーホイール(色相リング + SV 三角形)

新規 `src/ui/color_wheel.rs`。純関数(テスト必須)と描画/入力を分離する:

```
角度系: 色相 h∈[0,360)。マーカー角 θ = h に対し 12時方向が 0°、時計回り。
  pos_on_ring(center, radius, h) / hue_from_pos(center, pos) は往復一致をテスト。
三角形(固定向き、内接円半径 r_in):
  P_hue   = center + r_in * dir(0°)      (上)
  P_black = center + r_in * dir(240°)    (左下)
  P_white = center + r_in * dir(120°)    (右下)
S/V ↔ 重心座標: a=S*V (P_hue), c=V*(1-S) (P_white), b=1-V (P_black)
  逆変換: V = a + c, S = V>0 ? a/V : 0
  クランプ: 三角形外のポインタは重心座標を負値切り捨て→正規化で最近傍に丸める。
  roundtrip (S,V)→pos→(S,V) をテスト。
```

- 描画: リングは 72 分割の三角形ストリップ(egui `Mesh`、頂点色 = hsv(h,1,1))。三角形は頂点色(純色相/黒/白)1 枚のメッシュ(RGB 線形補間で標準的な見た目になる)。マーカーは白フチ+黒フチの小円。
- 入力: `Sense::click_and_drag()`。ドラッグ開始位置がリング帯かどうかでモード(Hue/SV)を固定し、離すまで維持。
- 色状態はアプリ全体で RGBA(Color32)が正。ホイールは編集中のみ HSV を保持し、**ドラッグ中は HSV を正としてドラッグ終了時に RGB へ確定**する(RGB↔HSV 往復での色相消失 — 例: 彩度 0 で h が失われる — を防ぐ)。
- egui の `color_edit_button_srgba` ポップアップは廃止。HEX 欄は `TextEdit` + 確定時パース(#RGB 形式は非対応でよい、#RRGGBB / #RRGGBBAA のみ)。

## 14.4 パレット / 14.5 アイコン / 14.6 ハンドル

- パレット: `const PALETTE: [Color32; 24]`(SPEC §14 の色)+ `user_palette: Vec<Color32>`(App 状態、非永続)。スウォッチは 16×16、クリック=プライマリ/右クリック=セカンダリ。ユーザー色の削除は `Response::context_menu`(キャンバス外なので右クリックメニュー可)。
- アイコン: 新規 `src/ui/icons.rs` に `pub fn paint_tool_icon(kind: ToolKind, painter: &egui::Painter, rect: Rect, color: Color32)`。線幅 1.5pt 基調、`rect` は正方形前提で相対座標(0..1)から組み立てる。ボタン側は `ui.allocate_response` + hover/selected の背景を自前で塗ってからアイコンを重ねる(egui Button の text 用 API に依存しない)。
- ハンドル: 選択/浮動片の矩形をスクリーン座標に変換し、8 点の Rect を求める純関数 `handle_rects(screen_rect) -> [Rect; 8]` と、ヒットテスト `hit_handle(pos) -> Option<Handle>` をテスト付きで実装。ドラッグ中は `Floating { original: Vec<u8>, orig_w, orig_h, .. }` から `resample_bilinear(original, new_w, new_h)`(純関数・テスト付き)で作り直す。カーソルは対角/水平/垂直のリサイズアイコン。

## 14.7 モジュール変更

```
src/ui/color_wheel.rs  (新規) ホイール+三角形ウィジェット
src/ui/color_panel.rs  (新規) スウォッチ/ホイール/アルファ/HEX/パレット/最近色
src/ui/layers_panel.rs (新規) レイヤー一覧+ボタン+不透明度
src/ui/icons.rs        (新規) ツールアイコンのベクター描画
src/ui/side_panel.rs   (新規・任意) 右パネルの枠(色+レイヤーを縦に配置、幅約210px)
document.rs / history.rs / raster.rs / tools/* / io.rs — レイヤー対応リファクタ
ui/options_bar.rs — 色関連を右パネルへ移設(サイズ・ツール固有オプションは残す)
ui/menu.rs — 「レイヤー」メニュー追加
```

## 14.8 v2 マイルストーン(各段階でビルド緑・警告 0・テスト緑必須)

### V2-M1 — レイヤー基盤(UI なし)
- Document/raster/history/tools/io を §14.1〜14.2 の形にリファクタ。UI は従来のまま(常に 1 枚の「背景」で v1 と同挙動)。
- recomposite の正しさ(不透明度・非表示・多層合成)、レイヤー付き Patch の undo/redo、ReplaceAll をテスト。
- 受け入れ: 既存の全機能が 1 レイヤーで完全に従来どおり動く。テスト全緑(既存テストは新 API に追随)。ベンチ 300ms 以内。

### V2-M2 — レイヤー UI
- 右パネル骨格(side_panel + layers_panel)、レイヤーメニュー、全レイヤー操作(追加/複製/削除/上下移動/結合/統合)+ ショートカット、名前変更、表示/不透明度、複数レイヤー保存時のトースト。
- 受け入れ: SPEC §13 の全項目。浮動片・ストローク進行中のレイヤー操作が先確定になること。

### V2-M3 — カラーパネル
- color_wheel(数式テスト付き)、color_panel(スウォッチ/アルファ/HEX/パレット/ユーザー色/最近色移設)、options_bar から色 UI を撤去、egui ポップアップピッカー廃止。
- 受け入れ: SPEC §14 の全項目。ホイール往復テスト緑。ドラッグ中も 60fps(dirty 再合成は発生しない — 色変更はテクスチャに影響しない)。

### V2-M4 — アイコンとスケールハンドル
- icons.rs(9 ツール)+ ツールバー置き換え。選択/浮動片の 8 ハンドル(§14.6)、Shift 比率固定、bilinear 再サンプル、カーソル。README.md を v2 機能で更新。
- 受け入れ: SPEC §15・§16 の全項目。テスト全緑・ベンチ 300ms 以内。

## 14.9 v2 の落とし穴(v3 は §15 参照)

1. **RGB↔HSV 往復**: S=0 や V=0 で色相が失われ、三角形ドラッグ中にマーカーが飛ぶ。ドラッグ中は HSV 状態を正とする(§14.3)。
2. **合成キャッシュの整合**: レイヤー構造変更(削除/移動/結合/統合/不透明度)は必ず全面 dirty。描画は従来どおり局所 dirty。
3. **アクティブレイヤーと浮動片**: 浮動片保持中にアクティブレイヤーを変えると確定先が変わってしまう。レイヤー切替も「先に確定」フックを通す。
4. **削除で active が範囲外**になる off-by-one(最上位削除時)。
5. **テクスチャは合成 1 枚のまま**(レイヤーごとにテクスチャを持たない)。浮動片テクスチャだけ v1 同様に別枠。
6. **リングのヒット判定**は内外半径の間のみ。三角形ヒットは重心座標で判定(外周ぎわのデッドゾーンを作らない)。
7. パネル追加で `CentralPanel` より先に右パネルを show する(egui のパネルは宣言順でレイアウトが決まる)。
8. 不透明度スライダーのドラッグ中は毎フレーム全面 recomposite になる — 4000×4000 では重いので、**ドラッグ中はフレームごとに 1 回だけ**(値が変わったときのみ)再合成し、変わらなければ何もしない。

---

# v3 設計(ブラシエンジン / 移動・ズーム・テキスト / PS キーマップ)

SPEC.md「v3 拡張仕様」(§17〜§20)に対応。**v2 マイルストーン実装中のエージェントはこの章を無視すること。** v3 は v2 完了後に着手する。

## 15.1 ストロークエンジン(ブラシ/消しゴム/鉛筆の統一)

v2 までの AA ペンの「カバレッジマスク + CoW タイル元画像」方式を全ストロークに一般化する:

```
mask: Vec<u8>(ドキュメント寸法、遅延確保・ストローク間で再利用、touched矩形リストでクリア)
stamp_soft(mask, cx, cy, r, hardness): d≤r*h → 255、r*h<d≤r → smoothstep で 255→0、max 書き込み
鉛筆: 2値スタンプ(d≤r → 255)
毎フレーム、ストロークの dirty 領域について:
  base = History の CoW タイル(ストローク開始時点のレイヤー画素)
  ブラシ: out = blend_over(base, color × (mask/255 × opacity))
  消しゴム: out.a = base.a × (1 − mask/255 × strength)
```

- これにより「1 ストローク内で重ねても不透明度上限を超えない」(PS opacity 意味論)が mask の max 合成から自然に出る。
- スタンプ間隔: ソフト時 ≤ max(1px, r/4)。
- Shift+クリック連結: ツールごとに直近ストローク終点(画像座標)を保持し、Shift+Down で終点→クリック点の segment を 1 ストロークとして実行。
- ブラシ円カーソル: `CursorIcon::None` + painter で円2重線(白 1.5pt の内側に黒 1pt)。半径 = brush_r × zoom / ppp。3px 未満は Crosshair にフォールバック。UI パネル上では通常カーソル。
- 数字キー: `!ctx.egui_wants_keyboard_input()` ガード付きで 1–9→10–90%、0→100%。

## 15.2 移動・ズーム・自由変形・Esc キャンセル

- **移動 (V)**: Down 時に「選択があれば選択範囲を、なければ全範囲を」既存の浮動化パス(begin_floating_from_selection 相当)で浮動化し、以後は既存の浮動片ドラッグと同一コード。空レイヤー(全透明)でも動作(確定時 before==after 抑制が効く)。
- **ズーム (Z)**: click で apply_zoom_step(+1, クリック位置アンカー)、Alt+click で −1。既存のズーム関数を再利用。カーソル ZoomIn/ZoomOut。
- **Ctrl+T**: `free_transform()` = 選択なし→全選択、以後 浮動化+ハンドル表示。既存機構の糊付けのみ。
- **Esc = キャンセル**: `cancel_floating()` を新設 — 浮動片破棄、cut_from 領域に元画素を書き戻し(浮動化時に退避した元画素を Floating に保持しておく)、選択解除、履歴に積まない、dirty 設定。テキスト編集中の Esc は入力破棄。**既存の「Esc=確定」を前提にしたコード・テストを全て洗い出して更新すること。**
- 浮動化時に「切り出し元の元画素」を Floating に保持する形にすると cancel が単純化する(現在は CoW タイル経由 — どちらでも良いが復元のバイト一致をテストすること)。

## 15.3 テキストツール

- 依存追加: `ab_glyph`(**`=0.2.32` に `=` 固定**。egui/epaint 0.35 は内部で ab_glyph を使っていない(skrifa/harfrust 系)ため「egui と同じ版に揃える」相手は存在しないが、再現性のためのバージョン固定自体は必須。`cargo tree -i ab_glyph` で darask-paint 単独の依存元であること・重複がないことを確認する)。
- App はフォントバイト列(§9 で読んだものと同じファイル)を `Arc<Vec<u8>>` で保持し、`ab_glyph::FontRef`(ttc は index 付き)を作る。
- 編集中: クリック画像座標に egui `TextEdit::multiline` をオーバーレイ(`Area`/`Window` 無枠)。表示フォントサイズ ≈ size × zoom / ppp(プレビューは近似で可、上限あり)。IME は egui/winit に任せる。
- 確定: 行分割 → 各行を ab_glyph でレイアウト(h_advance + kerning、ベースライン = ascent、行送り = (ascent−descent+line_gap)×1.1 目安)→ カバレッジをプライマリ色でバッファに合成 → その画素群を**浮動片**として配置(以後は既存の移動/拡縮/確定/キャンセル機構)。
- 純関数 `rasterize_text(font, text, px_size, color) -> (w, h, Vec<u8>)` としてテスト(空文字、複数行、日本語グリフが非ゼロ画素を生むこと)。

## 15.4 keymap.rs(ショートカットの一元化)

- 散在する handle_*_shortcuts を新規 `src/keymap.rs` に集約:
  `enum Action { SelectTool(ToolKind), CycleShapeTool, SwapColors, DefaultColors, Undo, Redo, ... }` と
  `const KEYMAP: &[(Binding, Action)]`(Binding = 修飾キー+キー or 単一キー)。
- ディスパッチ規則: ①モーダル/テキスト編集中の除外(従来どおり)②**修飾キーが多いものから先に consume**(v1 の Ctrl+Shift+Z 教訓)③単一キーは `!wants_keyboard_input` ガード。
- メニュー表記・ツールチップは KEYMAP から文字列生成(表記と実挙動の乖離を構造的に防ぐ)。
- SPEC §20 の表と 1:1 であることをテスト(KEYMAP に SPEC の全項目が存在するか、少なくとも件数と主要キーの静的テスト)。

## 15.5 v3 マイルストーン(各段階でビルド緑・警告 0・テスト緑・ベンチ 300ms 以内)

### V3-M1 — ストロークエンジン刷新
§15.1 の全部(硬さ/不透明度/鉛筆モード/消しゴム強さ/Shift+クリック連結/円カーソル/数字キー)。旧 AA チェックボックス廃止。オプションバー更新。エンジンの純関数テスト(falloff 境界、max 合成、opacity 上限、消しゴム減衰)。

### V3-M2 — 移動・ズーム・自由変形・Esc キャンセル
§15.2 の全部 + 移動/ズームツールのアイコン(icons.rs 拡張)+ ツールバー追加。Esc 挙動変更の全経路更新(テスト含む)。cancel_floating のバイト一致テスト。

### V3-M3 — テキストツール
§15.3 の全部 + テキストツールのアイコン + オプションバー(サイズ)。ab_glyph 追加(版固定)。rasterize_text テスト。

### V3-M4 — PS キーマップ集約と総配線
§15.4 の keymap.rs + SPEC §20 の完全適用(U 巡回含む)+ 旧キー(L/R/C/F)の削除 + メニュー/ツールチップ/README.md の全面追随 + Ctrl+J/D の配線。回帰: v1/v2 の全機能スモーク(テストで担保できる範囲)+ ベンチ。

## 15.6 v3 の落とし穴

1. Esc 意味論の変更は既存テスト・確定経路(Enter/外クリック/ツール切替/レイヤー操作前の自動確定)に波及する。「自動確定」は**確定のまま**(キャンセルにしない)こと — Esc だけがキャンセル。
2. mask バッファはドキュメントのサイズ変更で再確保。サイズ変更中にストロークは開いていない(v1 のガードが保証)。
3. 数字キー・単一キーは必ず wants_keyboard_input ガード(テキストツール編集中に B や 5 でツールが変わらないこと)。
4. ab_glyph は egui/epaint 0.35 が内部で使っているものではない(skrifa/harfrust 系)ため、「egui とバージョンをずれさせない」という意味での重複コンパイルの心配はそもそも無い(egui 側に合わせるべき ab_glyph 依存が存在しない)。それでも再現性のため `=` 固定を Cargo.toml に書き、`cargo tree -d` で(darask-paint 自身の重複がないか)・`cargo tree -i ab_glyph` で(darask-paint 単独の依存元であること)を確認する。
5. ブラシ円カーソルは選択・移動・テキスト等では出さない(ブラシ/消しゴム/鉛筆のみ)。
6. Shift+U 巡回と Shift+[ ] は「Shift 付き単一キー」— egui の consume 順で素の U / [ ] より先に判定。
7. 不透明度 <100 のストローク合成は毎フレーム「base からの再合成」— 直接レイヤーに blend を重ねると 1 ストローク内で濃度が累積してしまう(mask 方式を崩さない)。
8. 移動ツールで浮動化した全レイヤーの Patch は全面矩形になる — 履歴メモリ会計(256MB)に正しく算入されること。

---

# v4 設計(マスク選択 / グラデーション / 色調補正 / 永続化 / 性能 / CI/CD)

SPEC.md「v4 拡張仕様」(§21〜§29)に対応。実装順は **性能基盤 → 選択基盤 → その上の機能** とする(選択の一般化は全ツールに波及するため、先に性能の土台を固めてから 1 回だけ載せ替える)。

## 16.1 履歴のタイル化と dirty のセグメント化(§28)

- `HistoryOp::Patch { layer, rect, before, after }` を
  `HistoryOp::Patch { layer, tiles: Vec<TilePatch> }`(`TilePatch { rect: IRect, before: Vec<u8>, after: Vec<u8> }`、rect は 256×256 タイル境界にアラインした実タイル矩形)に変更する。StrokeRecorder は既にタイル CoW なので、commit 時に bbox 合併へ潰していたのを**タイルのまま保持**するだけ。undo/redo・メモリ会計・before==after 抑制(タイル単位で判定)を全て追随。
- `Document::dirty: Option<IRect>` を `dirty: DirtyRegion`(小さな `Vec<IRect>`、上限 32 個で溢れたら合併)に一般化。ストローク中はセグメント単位の矩形を積み、フレーム側は**各矩形ごとに** recomposite + テクスチャ部分更新する(高速ドラッグの対角 bbox 爆発を防ぐ)。
- ホットループ: `recomposite` / `blend_over` 系 / flood fill の行処理を `chunks_exact(4)` ベースの行スライスに書き直し、`get_pixel/set_pixel`(毎回境界チェック+インデックス計算)をループ内から排除。release の時間計測テスト(緩い上限+回帰検知)を追加。
- 浮動片ハンドルの再サンプルはフレームに 1 回(直近のポインタ位置だけ処理)。Esc キャンセルの復元はタイルごとに行スライスで一括コピー。

## 16.2 起動フェーズ計測(§28)

- `main()` 冒頭からのフェーズ時刻(設定読込完了 / フォント読込完了 / `App::new` 完了 / 初フレーム / 2 フレーム)を記録し、`DARASK_BENCH=1` のとき bench.txt を
  `total_ms` の 1 行目(後方互換)+ `phase\tms` 行に拡張する。
- 最適化は計測結果に従う。候補: 設定/フォント読込の直列化解消(フォント読込 ~10MB を `std::thread::spawn` で window 作成と並行にし `App::new` で join)、初期 composite の二重計算排除、`lto = "fat"` の実測比較(改善しなければ thin のまま)。**目標: 中央値 160ms、必須 300ms**。効果がない最適化は入れない(計測値を deviations で報告)。

## 16.3 マスク選択(§21〜22)

```rust
pub struct SelMask { pub bbox: IRect, pub mask: Vec<u8> } // len = bbox.w*bbox.h、値は 0 か 255
pub struct Selection { pub mask: SelMask }
pub struct Floating {
    pub pixels: Vec<u8>, pub w: u32, pub h: u32,
    pub mask: Vec<u8>,            // pixels と同寸。非矩形浮動片の合成に使用
    pub pos: Pos2, pub cut_from: Option<SelMask>, ...
}
```

- 純関数群(全てテスト必須): `rect_mask` / `ellipse_mask`(スキャンライン)/ `polygon_mask`(偶奇規則スキャンライン、自由なげなわは軌跡+終点→始点で閉多角形)/ `flood_mask`(自動選択。既存 flood fill の visit を流用)/ `mask_boundary(SelMask) -> Vec<[Pos2;2]>`(選択枠描画用の境界線分。選択変更時のみ再計算しキャッシュ)/ `resample_mask_nearest`(ハンドル拡縮用。ピクセルは bilinear、マスクは nearest)。
- **描画クリップ**: `ToolCtx` に `clip: Option<&SelMask>` を通し、ストローク合成(カバレッジ×clip)、図形・塗りつぶし・グラデーション・色調補正の書き込みで `mask==0` の画素をスキップ。塗りつぶしの連結探索は clip 外を壁として扱う。
- 浮動化: mask の画素だけ複写し、元領域は mask の画素だけ透明化。確定合成も mask 経由。矩形選択は `rect_mask` を通すだけで既存挙動と一致させ、既存テストは同値性で担保。
- なげなわ(多角形モード)は「クリックで頂点追加」のためドラッグではなくクリック列で状態を持つ(Esc=中止、Enter/ダブルクリック/始点近傍クリック=確定)。頂点列のプレビューは painter の線分。
- 巡回系(Shift+M / Shift+L / Shift+G)は keymap.rs の Action に `CycleMarquee / CycleLasso / CycleFillTool` を追加し、ツールバーは「現在のバリアント」をアイコン表示(ツールチップに巡回キーを明記)。

## 16.4 グラデーション(§23)

- 純関数 `gradient_span(kind, p0, p1, p) -> t`(線形=射影クランプ、円形=距離/半径クランプ)+ 行スライスで `lerp(色0, 色1, t)` を書き込み。選択 clip・レイヤー clip 対応。ドラッグ中はライブプレビュー(不透明度スライダーと同じ「値が変わったフレームだけ」方式で、開始時スナップショット=StrokeRecorder の CoW タイルから再合成)。確定で 1 Patch。

## 16.5 色調補正(§24)

- 純関数 `adjust_brightness_contrast(&mut [u8], b, c)` / `adjust_hsl(&mut [u8], dh, ds, dl)` / `invert` / `grayscale`(Rec.709)。LUT(256 要素)を作ってから行スライス適用(HSL は画素ごと変換で可、4000×4000 < 150ms 目標)。
- ライブプレビューモーダルの状態機械: 開始時に対象領域(選択 bbox∩レイヤー)のスナップショットを取り、スライダー値が変わったフレームだけ「スナップショット→補正→書き戻し→dirty」。OK で Patch 化、キャンセルでスナップショット書き戻し。モーダル中は他入力を遮断(既存モーダル基盤)。

## 16.6 スムージング・ピクセルグリッド(§25)

- `smoothed = prev + (raw - prev) * α`、α = 1.0 - strength*0.9(strength∈[0,1])。Up 時に残差を最終セグメントとして描き切る。純関数+テスト。
- ピクセルグリッドは zoom ≥ 8.0 のとき、可視範囲の画像ピクセル境界に 1 物理 px の線(色は市松と区別できる薄グレー、アルファ 64)。可視範囲だけ描く(全画像分の線を作らない)。

## 16.7 設定の永続化(§26)

- 新規 `src/settings.rs`: `Settings` 構造体 + `parse(text) -> Settings`(不正行は無視)+ `serialize(&Settings) -> String` + 読み書き(`%APPDATA%\darask-paint\settings.txt`、`std::env::var_os("APPDATA")`)。形式は 1 行 1 項目の `キー\t値`。リスト(recent/palette)は `キー.0`〜 の連番キー。**パーサ・シリアライザは往復テスト必須**。
- 読み込みは `main()` で 1 回(ウィンドウ初期寸法に必要)。保存は 終了時(close 確定後)+ recent 更新時。I/O 失敗は無視(トーストも不要、起動・終了を妨げない)。
- ウィンドウ寸法の取得は終了時の `ctx.input(|i| i.viewport())` の inner_rect / maximized。

## 16.8 CI/CD・アイコン・About(§29)

- `.github/workflows/ci.yml`: `on: [push(main), pull_request]`、`runs-on: windows-latest`、`dtolnay/rust-toolchain@stable`(components: clippy, rustfmt)、`Swatinem/rust-cache@v2`、steps: fmt → clippy(`-D warnings`)→ build --release → test --release。別ジョブ `bench-smoke`(`continue-on-error: true`、DARASK_BENCH=1 で release exe 起動→bench.txt を表示)。
- `.github/workflows/release.yml`: `on: push: tags: ['v*']`、ビルド → `Compress-Archive` で `darask-paint-v{ver}-windows-x64.zip` → `softprops/action-gh-release@v2` で Release 作成(`generate_release_notes: true`)。
- **clippy 全解消**を CI 追加前に行う(`cargo clippy --all-targets -- -D warnings` をローカルでグリーンに。無意味な lint は根拠コメント付きで `#[allow]` 可、乱用禁止)。
- アイコン: `examples/gen_icon.rs`(image crate の ico エンコーダ。Cargo.toml の image に `ico` feature 追加)で 16/24/32/48/64/128/256px を含む `assets/icon.ico` を生成しコミット。デザインは「角丸正方形+筆のストローク」程度のシンプルな図形(コードで描く)。`build.rs` + `[build-dependencies] winresource` で exe に埋め込み、eframe の `viewport.with_icon` にも同じ絵(生成関数を共有)を設定。
- `Cargo.toml` version = "0.4.0"。ヘルプ > バージョン情報 モーダル(版数・リポジトリ URL 表示)。
- CI の YAML はローカル検証できないため、**構文をシンプルに保ち**、push 後の初回実行結果で修正する前提(メインループ側=Fable が監視・修正指示)。

## 16.9 v4 マイルストーン(各段階でビルド緑・警告 0・clippy 0(M6 以降)・テスト緑・ベンチ 300ms 以内)

### V4-M1 — 性能基盤
§16.1(タイル Patch・dirty セグメント化・ホットループ行スライス化・ハンドル再サンプル制限・Esc 復元一括化)+ §16.2(フェーズ計測と実測に基づく起動最適化)。受け入れ: 既存テスト全緑(API 追随)、§28 の表の各項目を実測して deviations で報告。

### V4-M2 — マスク選択基盤
§16.3 のデータ構造・純関数・矩形選択の載せ替え・浮動片のマスク対応・描画クリップ。UI 上の新ツールはまだ増やさない(矩形選択が従来どおり動くこと)。受け入れ: 既存の選択関連テストが同値で全緑+マスク純関数テスト。

### V4-M3 — 選択ツール拡充
楕円選択(Shift+M 巡回)・なげなわ自由/多角形(L / Shift+L)・自動選択(W、許容値)+ 各アイコン + keymap 追加(§27)。

### V4-M4 — グラデーション・色調補正・スムージング・グリッド
§16.4〜16.6 の全部 + Shift+G 巡回 + Ctrl+U / Ctrl+I / Ctrl+Shift+U + 画像メニュー拡張 + アイコン。

### V4-M5 — 永続化・About
§16.7 + 最近使ったファイル + ヘルプメニュー + ウィンドウ状態復元。受け入れ: 設定往復テスト、破損ファイルで既定値起動、ベンチ悪化なし(計測値報告)。

### V4-M6 — CI/CD・アイコン・リリース準備
§16.8 の全部(clippy 全解消 → CI/Release YAML → アイコン生成と埋め込み → version 0.4.0 → README に CI バッジとインストール節)。受け入れ: ローカルで fmt/clippy/build/test 全緑、YAML は自己レビュー、`git status` に生成物の取りこぼしがない(assets/icon.ico はコミット対象、bench.txt 等は除外のまま)。

## 16.10 v4 の落とし穴

1. マスク選択の載せ替えで**既存の矩形選択の挙動を 1 ビットも変えない**こと(既存テストを同値性の証拠として使う。テストの期待値を書き換える必要が出たら設計ミスを疑う)。
2. 描画クリップは「選択が無いとき」のコストがゼロであること(Option が None なら従来コードパスと同一)。
3. dirty のセグメント化で、同一フレーム内の重複矩形の recomposite 二重実行を許容する(重複排除の複雑化より単純さ優先。ただしタイル単位の部分テクスチャ更新が矩形ごとに走ることによる劣化がないか計測)。
4. 色調補正のライブプレビューは**スナップショットから**毎回適用(スライダーを往復すると劣化する累積適用は禁止)。
5. 設定読込・保存は絶対にパニックしない・ブロックしない(壊れた settings.txt、APPDATA 不在、書込禁止をテスト/防御)。
6. clippy 解消で挙動を変えない(`too_many_arguments` 等の構造 lint は機械的リファクタのみ。判断に迷う lint は allow+根拠コメント)。
7. winresource は Windows 以外では何もしないよう `build.rs` を組む(CI も windows のみなので実害はないが、コンパイルエラーの種を残さない)。
8. GitHub Actions の windows ランナーはヘッドレスで GL ウィンドウ生成に失敗し得る — bench ジョブは必ず `continue-on-error`。CI 必須ジョブに GUI 起動を含めない。
9. 楕円/多角形マスクの境界線分抽出は選択確定時のみ(毎フレーム再計算しない)。巨大選択(4000×4000 全選択)でも境界抽出 < 50ms。
10. Shift+M/Shift+L/Shift+G の巡回はツールバーのアイコン・ツールチップ・オプションバーの整合(現在バリアントの表示)を忘れない。

---

# v5 設計(タブ・複数ドキュメント)

SPEC.md「v5 拡張仕様」(§30〜§32)に対応。**これは v1〜v4 で「単一ドキュメント」を前提に組んできた `DaraskApp` の構造変更**であり、機械的だが広範囲な書き換えになる。実装順は「データモデルをタブ化 → 既存の全呼び出し箇所を追随させてビルドを通す(挙動は変えない)→ タブ UI・切替 → 新規タブ複製機能」とする。

## 17.1 データモデル

```rust
struct Tab {
    doc: Document,
    history: History,
    view: CanvasView,           // zoom/pan/viewport はタブごとに独立して記憶される
    selection: Option<Selection>,
    floating: Option<Floating>,
    // ストローク進行中の一時状態(StrokeRecorder は history 内、なげなわ多角形の
    // 頂点列・Shift+クリック終点等は既存どおり DaraskApp 側のツール状態に残してよいが、
    // 17.6-1 の「切替前に必ず確定」規則でタブ切替時に安全側へ倒す)。
}

struct DaraskApp {
    tabs: Vec<Tab>,
    active_tab: usize,          // 常に tabs.len() > 0、常に有効な index
    next_untitled_number: u32,  // 「無題」「無題2」…の採番(既存 next_layer_number と同型)
    // 以下は変更なし(アプリ全体で共有): tool, tools状態, primary/secondary,
    // recent_colors, user_palette, brush_size/hardness/opacity/..., settings,
    // modal, pending_action, toast, keymap 関連, recent_files 等
}
```

- `self.doc` / `self.history` / `self.view` / `self.selection` / `self.floating` への既存の全アクセスは `self.tabs[self.active_tab].doc` 等に変わる。ヘルパー `active_tab(&self) -> &Tab` / `active_tab_mut(&mut self) -> &mut Tab` を用意し、既存コードの書き換え量を減らす。
- 起動時(`DaraskApp::new`)は `tabs: vec![Tab::new(...)]`(CLI 引数があればそれを開いた Tab、無ければ白紙)、`active_tab: 0`。ベンチモード(`DARASK_BENCH=1`)はタブ 1 枚のまま従来どおり動作する(変更不要)。

## 17.2 タブ切替とテクスチャ

- `CanvasView` は現状どおり「自分が持つ `TextureHandle`」の仕組みをそのまま使う。タブ切替時(`switch_tab(new_index)`)は、切替先タブの `view.texture = None` に**強制的に戻す**(実際には切替先タブ自身の `view` はそもそも別インスタンスなので、単に「切替先の `CanvasView` を今の viewport 情報だけ更新して使う」だけでよい。同一タブに戻ってきた場合に古いテクスチャがそのまま使えるなら再アップロード不要 ―― ただし他のタブがアクティブな間にそのタブの `doc` は変更されない(操作はアクティブタブにしか効かない)ため、非アクティブタブのテクスチャは古びない。**タブ切替のたびに毎回全面再アップロードする必要はない**。切替先タブの `view.texture` が既に存在し `texture_size == doc.width/height` ならそのまま使う。存在しない(初回切替)場合のみ、既存の「新規/サイズ変更時のみ全面再アップロード」の分岐(§14.1)がそのまま効いて 1 回だけ全面 recompose+upload される。
- `floating_texture` も同様にタブごと(`Tab::view` の一部)なので自然に分離される。
- 結果として: 初めて開くタブへの切替時だけ全面 recompose 相当のコスト(既存の「開く」と同等)が掛かり、2 回目以降の再訪問は追加コスト無し。新しい仕組みを作らない。

## 17.3 タブ切替時の安全規則(最重要の落とし穴の再発防止)

このプロジェクトでは「進行中の操作(ストローク・浮動片・なげなわ多角形)を確定せずに別の操作(ツール切替・レイヤー操作・undo・貼り付け)を割り込ませると履歴が壊れる」というバグが v3〜v4 のレビューで繰り返し見つかっている(§15.6, §16.10 参照)。**タブ切替は構造的に同じ危険を持つ**ため、既存の `commit_open_gesture()`(ツール切替・レイヤー操作の前に呼んでいるもの)を **タブ切替の前に必ず呼ぶ**。Ctrl+Tab・Ctrl+Shift+Tab・タブバークリック・タブを閉じる・新規タブを開く、のすべての経路がこの一箇所(`switch_tab`/`close_tab`/`open_new_tab` 共通のエントリ)を通ること。

## 17.4 未保存ガードの一般化

- `pending_action` に `CloseTab(usize)` を追加。タブを閉じる際、そのタブの `doc.modified` が true なら該当タブをアクティブ化した上で既存の `ConfirmUnsaved` モーダルを出す(保存/破棄/キャンセル)。
- ウィンドウを閉じる要求(`close_requested`)は、`modified` なタブの index 列を集めて `pending_action = CloseAllTabs(VecDeque<usize>)` とし、先頭から 1 つずつ `CloseTab` と同じ確認フローを回す。全部処理し終えたら `std::process::exit`。途中でキャンセルされたら全体を中止(`ViewportCommand::CancelClose` は最初の要求時点で既に送っているので、キャンセル時は何もしない=閉じない)。
- 最後の 1 タブに対する Ctrl+W / タブの× は「閉じる」ではなく「新規」と同じ処理(§30)に差し替える(タブ数チェックを `close_tab` の先頭で行う)。

## 17.5 選択範囲を新規タブに複製(§31)の実装

- 新規タブの内容構築は既存の抽出コードを再利用する(新しいピクセル演算を書かない):
  - 浮動片がある場合: `Floating.pixels/mask/w/h` をそのまま新規 `Document`(1 レイヤー)の初期値にする。
  - 静的選択のみの場合: `tools::select::extract_region`(v4-M2 で実装済み)を選択 bbox・現在の `Selection.mask` で各レイヤーに対して呼び、結果を新規 `Document.layers` として順番どおり組み立てる。
- 新規 `Tab` は `active_tab + 1` の位置に挿入し、`active_tab` をその index に更新(§17.3 の切替安全規則を通す)。`doc.modified = true`、`doc.path = None`、`history` は新規(空)。タブ名は `next_untitled_number` を消費して「無題」「無題2」…と採番(通常の新規作成と同じ命名関数を共有)。
- 全レイヤー版の抽出コストは既存の「選択範囲でトリミング」と同程度(新規の性能懸念なし)。

## 17.6 メニュー・keymap の変更

- keymap.rs に `Action::CloseTab` / `Action::NextTab` / `Action::PrevTab`(前タブ)を追加、Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab を束縛。**Ctrl+Tab を egui 自身のフォーカス移動(Tab キー)が横取りしていないか実機で確認する**(§17.7-2)。
- 画像メニューに「選択範囲を新規タブに複製」を追加、`MenuState` に `can_duplicate_selection_to_tab: bool`(`has_selection` と同じ値でよい)を追加。
- ファイルメニューに「タブを閉じる (Ctrl+W)」を追加(名前を付けて保存の後、終了の前)。

## 17.7 v5 マイルストーン(各段階でビルド緑・警告 0・clippy 0・テスト緑・ベンチ 300ms 以内)

### V5-M1 — データモデルのタブ化(挙動不変)
§17.1〜17.2。`DaraskApp` の `doc`/`history`/`view`/`selection`/`floating` を `tabs: Vec<Tab>` + `active_tab` に置き換え、全呼び出し箇所を `active_tab()`/`active_tab_mut()` 経由に書き換える。**UI にタブバーはまだ出さない**(常にタブ 1 枚のまま、v1〜v4 の全既存テスト・全既存挙動が同一であることを担保するマイルストーン)。受け入れ: 既存の全テストが(API 追随のみで)無傷で通ること。

### V5-M2 — タブバー UI・切替・新規/開くの意味変更
タブバーウィジェット、Ctrl+Tab/Ctrl+Shift+Tab/中クリック、Ctrl+N/Ctrl+Oを新規タブ方式に変更、パス正規化での重複オープン検出、D&D複数ファイル、タブ数上限24+トースト、常時1タブ維持、§17.3の安全規則配線。

### V5-M3 — 未保存ガードの一般化・タブを閉じる
§17.4 全部(Ctrl+W、タブの×、ウィンドウを閉じる際の複数タブ確認)。

### V5-M4 — 選択範囲を新規タブに複製・最終検証
§17.5〜17.6 全部 + README 更新 + 最終検証(fmt/clippy/build/test/bench + 実機目視確認)。

## 17.8 v5 の落とし穴

1. **タブ切替前に必ず `commit_open_gesture()` を呼ぶ**(§17.3)。これを忘れるクラスのバグは本プロジェクトで最も繰り返し発生してきたパターンなので、実装時は既存の tool-switch・layer-op の呼び出し箇所をそのまま模倣すること。
2. Ctrl+Tab が egui のデフォルトのフォーカス移動と衝突しないか実機確認(衝突する場合はショートカット処理をフォーカス移動より先に consume する)。
3. `active_tab` が指す index は「常に有効」を型ではなく実行時に保証している(境界チェックを怠らない。タブ削除後の index シフトに注意)。
4. 非アクティブタブの `doc`/`history` はメモリに保持され続ける(N タブ = N 個の Document + History)。タブ数上限(24)がこの膨張の安全弁であることをコード上のコメントで明記する。
5. 「選択範囲を新規タブに複製」は元タブを一切変更しない(読み取りのみ)。誤って元タブの `doc`/`selection` を書き換えないこと。
6. ドラッグ&ドロップで複数ファイルを開く際、既存の未保存ガード(単一ファイルの Open 相当)をファイルごとに正しく通すこと(重複オープン検出も含む)。
7. ベンチモード(`DARASK_BENCH=1`)・CLI 引数起動は「タブ 1 枚」のまま(タブ関連 UI の初期化コストを起動時間に載せない)。

---

# v6 設計(メニューの全展開アイコン化・アンドゥ履歴パネル)

SPEC.md「v6 拡張仕様」(§33〜§35)に対応。**v5 マイルストーン実装中のエージェントはこの章を無視すること。** v6 は v5 完了後に着手し、v5 が作るタブ機構(タブバー・`Tab`/`active_tab` データモデル)を前提とする。

## 18.1 menu.rs の再設計(ドロップダウン → 常時表示アイコン行)

- 既存の `ui/menu.rs::show(ui, &MenuState) -> Option<MenuAction>` の**シグネチャは変えない**(`MenuAction` enum も変えない)。変わるのは内部実装のみ: `egui::menu::bar` + `ui.menu_button(...)` によるドロップダウンを、`ui.horizontal_wrapped(|ui| { ... })` の中に各アクションのアイコンボタンを並べる形に書き換える。
- グループ順序と区切り: ファイル → (区切り) → 編集 → (区切り) → 画像 → (区切り) → レイヤー → (区切り) → 表示 → (区切り) → ヘルプ/設定。区切りは `ui.separator()` の縦版、またはグループ間の余白+薄い縦線。
- 各アイコンボタンは `ui/icons.rs` に新規追加する描画関数群(SPEC §33 の全項目ぶん、既存の 9 ツールアイコンと同じ関数シグネチャ規約: `fn paint_xxx_icon(painter: &egui::Painter, rect: Rect, color: Color32)`)。20×24px 目安、ホバー/押下時のハイライトは既存のツールボタンと同じ描画パターンを踏襲する。
- 「最近使ったファイル」だけは `egui::popup` 相当(既に色ピッカーやなげなわ確定などで使っている軽量ポップアップの流儀)でパス一覧を表示する小さな例外。他は全てワンクリックで即発火(既存の `MenuAction` を返す)。
- 「ピクセルグリッド表示」は他のツールボタンと同じ `.selected(bool)` ハイライトでトグル状態を示す(即実行系ではなく状態表示系のボタン)。
- `MenuState` に `pixel_grid_visible: bool` を追加(既に app 側に該当フラグがあるはずなので、それを渡すだけ)。

## 18.2 設定(Preferences)ダイアログと保持数(SPEC §34)

- `src/settings.rs`: `Settings` に `max_undo_steps: u32`(既定 `DEFAULT_MAX_UNDO_STEPS = 50`)を追加。キー `history.max_steps`、読込時 `[1, 500]` にクランプ(既存の `clamp_window_dims` と同じ流儀)。往復テストを追加。
- 新規 `ModalState::Preferences { draft_max_undo_steps: u32 }`(ダイアログ内はドラフト値を持ち、OK で確定・キャンセルで破棄する既存の New/Resize ダイアログと同じパターン)。
- OK 確定時: `self.settings.max_undo_steps` を更新して即保存(`save_settings()`)、かつ **開いている全タブ**に対して `tab.history.set_max_steps(new_value)` を呼ぶ(下方変更で現在の段数がそれを超えていれば§18.3のロジックでその場で切り詰める)。
- Ctrl+K を keymap.rs に追加(`Action::OpenPreferences`)。ツールバーにも歯車アイコンのボタンを 1 つ(§33 末尾)。

## 18.3 History のラベル付けと保持数キャップ・ジャンプ

```rust
pub struct HistoryEntry { pub op: HistoryOp, pub label: String }

pub struct History {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    max_bytes: usize,     // 既存 256MB(不変)
    max_steps: usize,     // 新規。既定 50、Settings から注入
    // ...
}
```

- `push`/`commit_stroke`/`apply_layer_op` 等、`HistoryOp` を積む**全ての箇所**が呼び出し元由来の短い日本語ラベルを渡すようにシグネチャを変更する(`impl Into<String>` 引数を 1 つ追加するだけの機械的変更)。推奨ラベル対応表:

  | 操作 | ラベル |
  |---|---|
  | ブラシ/消しゴム/鉛筆ストローク | "ブラシ" / "消しゴム" |
  | 直線・矩形・楕円確定 | "直線" / "矩形" / "楕円" |
  | 塗りつぶし | "塗りつぶし" |
  | グラデーション確定 | "グラデーション" |
  | テキスト確定 | "テキスト" |
  | 選択/浮動片の移動確定(選択ドラッグ・移動ツール・自由変形) | "選択の移動" |
  | 貼り付け確定 | "貼り付け" |
  | 切り取り | "切り取り" |
  | Delete による消去 | "削除" |
  | 画像サイズ変更 / キャンバスサイズ変更 / トリミング | 同名そのまま |
  | 左右反転 / 上下反転 / 右に90°回転 / 左に90°回転 | 同名そのまま |
  | 明るさ・コントラスト / 色相・彩度 / 階調の反転 / グレースケール化 | 同名そのまま |
  | レイヤー追加/複製/削除/移動/結合/統合 | "レイヤーを追加" / "レイヤーを複製" / "レイヤーを削除" / "レイヤーの並び替え" / "レイヤーの結合" / "画像の統合" |

- **保持数キャップの適用**: 新規 push 時、`undo_stack.len() > max_steps` または合計バイトが `max_bytes` を超える限り先頭(最古)から破棄する。**下限は `min(10, max_steps)` 件を必ず保持**(v1 §9 の「直近10件」floor を `max_steps` 自体がそれより小さい設定の場合に矛盾させないための最小値クランプ)。
- `set_max_steps(new: usize)`: `max_steps` を更新し、即座に上記の破棄ロジックを 1 回走らせて超過分を切り詰める(§18.2 のダイアログ OK 時に全タブへ呼ぶ)。
- **ジャンプ**: `History::jump_to(&mut doc, target_len: usize)` は「新しい仕組みを増やさない」方針で、既存の単発 `undo()`/`redo()` を `target_len` に達するまでループ呼び出しするだけの薄い関数として実装する(最大 `max_steps` 回のループ、既定 50 なら軽量。新しいスナップショット合成ロジックは書かない)。
- 破棄された古いエントリの有無(履歴パネルの「(これ以前の履歴は破棄されました)」注記用)は `History` に `truncated: bool`(一度でも先頭を破棄したら true のまま)を持たせて公開する。

## 18.4 履歴パネル(SPEC §35)

- 新規 `src/ui/history_panel.rs`: `show(ui, history: &History) -> Option<usize>`(クリックされた行の「その時点でのundo_stack長」を返すだけ。実際のジャンプ実行は呼び出し側=app.rs が既存の commit-first ガードを通してから `History::jump_to` を呼ぶ)。
- 表示は `egui::ScrollArea::vertical()` 内に、`truncated` なら先頭に灰色の注記行、続いて仮想「(初期状態)」行、続いて `undo_stack` の各 `HistoryEntry.label` を先頭(最古)から順に、続いて `redo_stack` の各 `HistoryEntry.label` を逆順(直近の undo ほど上)に淡色で表示。現在位置(`undo_stack` の末尾に対応する行)をハイライト。
- `side_panel.rs` に「色」「レイヤー」に続く 3 セクション目として配線。タブ切替時は自然にアクティブタブの `History` を渡すだけで追随する(v5 のタブ化により `history` は既にタブごとに独立しているため、パネル側に特別な対応は不要)。
- クリック時の安全規則(app.rs 側): 履歴パネルからのジャンプ要求は、既存の `handle_undo_redo_shortcuts`/メニューの Undo/Redo が使っている「進行中のストローク・浮動片があれば先に確定してから実行する」ロジック(`commit_open_gesture()` 等)を**そのまま再利用**する。ジャンプ専用の別ルートを作らない。

## 18.5 v6 マイルストーン(各段階でビルド緑・警告 0・clippy 0・テスト緑・ベンチ 300ms 以内)

### V6-M1 — History のラベル付け・保持数キャップ・ジャンプ(UIなし)
§18.3 全部。全 push 呼び出し箇所にラベルを追加、保持数キャップと `jump_to` を実装。既存の undo/redo 挙動(バイト上限・直近10件floor)は不変であることを既存テストで担保。

### V6-M2 — 設定ダイアログ
§18.2 全部。Ctrl+K、Preferences モーダル、settings.rs 拡張、全タブへの即時反映。

### V6-M3 — 履歴パネル
§18.4 全部。history_panel.rs、side_panel.rs への配線、クリックジャンプの安全配線。

### V6-M4 — メニューの全展開アイコン化・最終検証
§18.1 全部(SPEC §33 の全項目のアイコン化)+ README 更新 + 最終検証(fmt/clippy/build/test/bench + 実機目視確認、ウィンドウ最小幅 640px での折り返し確認も含む)。

## 18.6 v6 の落とし穴

1. 履歴パネルのジャンプは**必ず既存の commit-first ガードを再利用**する(§17.3/§17.8-1 と全く同じクラスの罠。ジャンプだけを特別扱いして安全確認を省略しない)。
2. `max_steps` を小さく設定変更した瞬間に**現在開いている全タブ**へ反映すること(新規タブだけに適用され既存タブが取り残される、というバグを作らない)。
3. ラベル付けの機械的変更(§18.3 の対応表)で、既存の `HistoryOp` 構築ロジック自体(何が起きるか)は一切変えない。ラベル文字列を追加するだけ。
4. メニューの全展開アイコン化は**純粋に見た目とレイアウトの変更**であり、各 `MenuAction` の実際の意味・確認ダイアログ・キーボードショートカットは 1 つも変えない(既存の `handle_menu_action` 等は無改造で流用できるはず)。
5. 「最近使ったファイル」のポップアップだけがドロップダウン的な UI として残る例外であることを実装コメントに明記する(SPEC §33 が認める唯一の例外)。
6. ウィンドウ最小幅 640px でアイコン行が折り返しても操作不能にならないこと(実機で確認)。
7. `HistoryEntry` へのラップで `HistoryOp` のシリアライズ/比較を前提にした既存テストがあれば、期待値をラベル込みの新シグネチャに追随させる(削除しない)。
