//! キャンバスビュー(ARCHITECTURE.md §2 座標系, §3 キャンバス描画, §4 ポインタ
//! ディスパッチ, §12 の落とし穴)。
//!
//! ズーム/パン状態、画像⇔スクリーン座標変換、ドキュメントテクスチャの
//! 全面/部分更新、ポインタ入力から `ToolEvent` への変換、市松模様の描画を
//! すべてここに集約する。座標変換とズーム/パンのクランプは egui の
//! `Context` に依存しない純関数として切り出してあり、`cargo test` から
//! そのまま検証できる(ARCHITECTURE.md §13)。

use eframe::egui::{
    self, pos2, vec2, Color32, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    Sense, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Vec2,
};

use crate::document::{Document, IRect};
use crate::tools::select::{self, Floating};
use crate::tools::ToolEvent;

/// SPEC §10: ズーム範囲 5%–3200%。
pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 32.0;
/// SPEC §10: Ctrl+ホイールは段階 ×1.25。
const ZOOM_STEP: f32 = 1.25;
/// SPEC §10: パンは画像が完全に画面外へ消えないようクランプ(最低 32px は見える)。
const PAN_VISIBLE_MARGIN: f32 = 32.0;
/// ネイティブ(winit)バックエンドでの egui 既定の行スクロール速度と揃える
/// (`egui::InputOptions::line_scroll_speed` のデフォルト値)。
const LINE_SCROLL_SPEED: f32 = 40.0;
/// 市松模様の 1 マスの論理ポイントサイズ。
const CHECKER_CELL: f32 = 16.0;

// ---------------------------------------------------------------------------
// 座標変換(ARCHITECTURE.md §2、egui::Context 非依存の純関数・テスト対象)
// ---------------------------------------------------------------------------

/// `img_to_screen(p) = viewport.min + pan + p * (zoom / ppp)`
pub fn img_to_screen(img: Pos2, viewport_min: Pos2, pan: Vec2, zoom: f32, ppp: f32) -> Pos2 {
    viewport_min + pan + vec2(img.x, img.y) * (zoom / ppp)
}

/// `screen_to_img(s) = (s - viewport.min - pan) * (ppp / zoom)`
pub fn screen_to_img(screen: Pos2, viewport_min: Pos2, pan: Vec2, zoom: f32, ppp: f32) -> Pos2 {
    let local = (screen - viewport_min - pan) * (ppp / zoom);
    pos2(local.x, local.y)
}

/// カーソル中心ズーム: `anchor_img` が `anchor_screen` に留まるように pan を
/// 解いて求める。
pub fn pan_for_anchor(
    anchor_img: Pos2,
    anchor_screen: Pos2,
    viewport_min: Pos2,
    zoom: f32,
    ppp: f32,
) -> Vec2 {
    (anchor_screen - viewport_min) - vec2(anchor_img.x, anchor_img.y) * (zoom / ppp)
}

pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// `notches` 段の ×1.25 ステップでズームする(SPEC §10)。
pub fn apply_zoom_step(zoom: f32, notches: i32) -> f32 {
    clamp_zoom(zoom * ZOOM_STEP.powi(notches))
}

/// 画像が完全に画面外へ消えないよう、片軸ぶんの pan をクランプする。
/// `img_size` はその軸の画像サイズ(スクリーン論理ポイント単位)。
/// マージンより画像や viewport が小さい場合は中央寄せにフォールバックする。
fn clamp_pan_axis(pan: f32, img_size: f32, viewport_size: f32, margin: f32) -> f32 {
    let lo = margin - img_size;
    let hi = viewport_size - margin;
    if lo <= hi {
        pan.clamp(lo, hi)
    } else {
        (lo + hi) / 2.0
    }
}

/// SPEC §10 のパンクランプ(両軸、最低 `margin` px は画像が見える)。
pub fn clamp_pan(pan: Vec2, img_size_screen: Vec2, viewport_size: Vec2, margin: f32) -> Vec2 {
    vec2(
        clamp_pan_axis(pan.x, img_size_screen.x, viewport_size.x, margin),
        clamp_pan_axis(pan.y, img_size_screen.y, viewport_size.y, margin),
    )
}

/// ホイールの生の `delta` を論理ポイントへ変換する
/// (`egui::WheelState` 相当の単位換算、ARCHITECTURE.md §12-5)。
pub fn wheel_delta_to_points(unit: MouseWheelUnit, delta: Vec2, viewport_height: f32) -> Vec2 {
    match unit {
        MouseWheelUnit::Point => delta,
        MouseWheelUnit::Line => delta * LINE_SCROLL_SPEED,
        MouseWheelUnit::Page => delta * viewport_height,
    }
}

/// 100% 表示などでくっきり見えるよう、描画矩形の原点を物理ピクセル格子に
/// 丸める(ARCHITECTURE.md §2)。ポインタ座標変換には使わない(丸めると
/// img⇔screen の往復が恒等でなくなるため、レンダリング専用)。
fn snap_to_pixel_grid(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

// ---------------------------------------------------------------------------
// CanvasView 本体
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Gesture {
    button: PointerButton,
    is_pan: bool,
}

/// キャンバス上のポインタ入力から生成された `ToolEvent` 群と、続けて
/// ツールのプレビュー描画(`Tool::draw_preview`)に使えるクリップ済み
/// `Painter`(ARCHITECTURE.md §3: 市松模様→画像→ツールプレビュー→選択枠の順)。
pub struct CanvasOutput {
    pub events: Vec<ToolEvent>,
    pub painter: egui::Painter,
}

pub struct CanvasView {
    pub zoom: f32,
    /// viewport 原点から画像原点への論理ポイントオフセット。
    pub pan: Vec2,
    viewport: Rect,
    texture: Option<TextureHandle>,
    texture_size: (u32, u32),
    /// 選択の浮動片(M4、ARCHITECTURE.md §7)を描くためのテクスチャ。
    /// `Floating::id` が変わったときだけ作り直す(移動だけなら内容は不変
    /// なので位置が変わるだけでは再アップロードしない。v2 §16 のスケール
    /// ハンドルでリサイズされたときは `app.rs` が新しい `id` を割り当てる
    /// ので、その場合はここで正しく作り直される)。
    floating_texture: Option<(u64, TextureHandle)>,
    gesture: Option<Gesture>,
    last_pointer: Pos2,
    /// 直近フレームでカーソル直下だった画像座標(ステータスバー表示用)。
    hover_img: Option<Pos2>,
    /// 直近の `show()` で取得した `pixels_per_point`(毎フレーム更新。
    /// ARCHITECTURE.md §12-6: キャッシュせず毎フレーム取得する方針を守るため、
    /// この値は `show()` 内で常に最新に上書きしてから使う)。
    /// `Tool::draw_preview`(M3 の直線/矩形/楕円のプレビュー)が画像座標を
    /// スクリーン座標に変換するために使う。
    last_ppp: f32,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self::new()
    }
}

impl CanvasView {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::ZERO),
            texture: None,
            texture_size: (0, 0),
            floating_texture: None,
            gesture: None,
            last_pointer: Pos2::ZERO,
            hover_img: None,
            last_ppp: 1.0,
        }
    }

    /// 直近フレームでカーソル直下だった画像座標(ステータスバー表示用)。
    /// キャンバス外や未初期化なら `None`。
    pub fn hover_img(&self) -> Option<Pos2> {
        self.hover_img
    }

    /// 画像ピクセル座標をスクリーン論理ポイント座標に変換する
    /// (ARCHITECTURE.md §2)。直近の `show()` 呼び出し時点の `pixels_per_point`
    /// を使うため、同一フレーム内(`show()` の後、`Tool::draw_preview` から)
    /// でのみ正確。M3 の直線/矩形/楕円ツールのドラッグ中プレビュー描画に使う。
    pub fn img_to_screen_pos(&self, img: Pos2) -> Pos2 {
        img_to_screen(img, self.viewport.min, self.pan, self.zoom, self.last_ppp)
    }

    /// 直近の `show()` 時点の `pixels_per_point`(ARCHITECTURE.md §2:
    /// `img_to_screen`/`img_to_screen_pos` と同じ `zoom / ppp` 換算をツール側
    /// (`tools/shapes.rs` のプレビュー太さ計算)でも使えるようにするための
    /// 公開アクセサ)。
    pub fn ppp(&self) -> f32 {
        self.last_ppp
    }

    /// 現在のビューポート中央に対応する画像座標(SPEC §6: Ctrl+V はビュー
    /// 中央に貼り付ける)。
    pub fn view_center_img(&self) -> Pos2 {
        screen_to_img(
            self.viewport.center(),
            self.viewport.min,
            self.pan,
            self.zoom,
            self.last_ppp,
        )
    }

    /// 表示メニュー「拡大」(Ctrl++, SPEC §10: 段階 ×1.25)。ビューポート中央を
    /// アンカーにズームする。
    pub fn zoom_in(&mut self) {
        let anchor = self.viewport.center();
        self.zoom_at(1, anchor, self.last_ppp);
    }

    /// 表示メニュー「縮小」(Ctrl+-)。
    pub fn zoom_out(&mut self) {
        let anchor = self.viewport.center();
        self.zoom_at(-1, anchor, self.last_ppp);
    }

    /// 表示メニュー「100%」(Ctrl+1、SPEC §10)。ビューポート中央を固定して
    /// ズーム 1.0 にする。
    pub fn zoom_to_100(&mut self) {
        let anchor_screen = self.viewport.center();
        let ppp = self.last_ppp;
        let anchor_img = screen_to_img(anchor_screen, self.viewport.min, self.pan, self.zoom, ppp);
        self.zoom = 1.0;
        self.pan = pan_for_anchor(anchor_img, anchor_screen, self.viewport.min, self.zoom, ppp);
    }

    /// 表示メニュー「ウィンドウに合わせる」(Ctrl+0、SPEC §10)。画像全体が
    /// ビューポートに収まるズームに合わせ、中央に配置する。
    pub fn fit_to_window(&mut self, doc: &Document) {
        if doc.width == 0 || doc.height == 0 || self.viewport.width() <= 0.0 {
            return;
        }
        let ppp = self.last_ppp;
        let zx = self.viewport.width() * ppp / doc.width as f32;
        let zy = self.viewport.height() * ppp / doc.height as f32;
        self.zoom = clamp_zoom(zx.min(zy));
        let center_img = pos2(doc.width as f32 / 2.0, doc.height as f32 / 2.0);
        self.pan = pan_for_anchor(
            center_img,
            self.viewport.center(),
            self.viewport.min,
            self.zoom,
            ppp,
        );
    }

    /// 画像座標の矩形をスクリーン論理ポイント座標の矩形に変換する
    /// (`draw_selection_outline`/`draw_resize_handles`/`app.rs` のハンドル
    /// 当たり判定が共有する、ARCHITECTURE.md §14.6)。
    pub fn img_rect_to_screen(&self, rect_img: IRect) -> Rect {
        let min = self.img_to_screen_pos(pos2(rect_img.x0 as f32, rect_img.y0 as f32));
        let max = self.img_to_screen_pos(pos2(rect_img.x1 as f32, rect_img.y1 as f32));
        Rect::from_min_max(min, max)
    }

    /// 選択枠(点線、ARCHITECTURE.md §7: アニメーションさせない)を画像座標の
    /// 矩形から描く。
    pub fn draw_selection_outline(&self, painter: &egui::Painter, rect_img: IRect) {
        if rect_img.is_empty() {
            return;
        }
        draw_dashed_rect(painter, self.img_rect_to_screen(rect_img));
    }

    /// 選択矩形・浮動片の外周に 8 個のスケールハンドルを描く(SPEC §16、
    /// ARCHITECTURE.md §14.6)。ヒットテストは `select::handle_rects`/
    /// `select::hit_handle`(`app.rs` が使う)と同じ幾何を共有する。
    pub fn draw_resize_handles(&self, painter: &egui::Painter, rect_img: IRect) {
        if rect_img.is_empty() {
            return;
        }
        let screen_rect = self.img_rect_to_screen(rect_img);
        for handle_rect in select::handle_rects(screen_rect) {
            painter.rect_filled(handle_rect, 1.0, Color32::WHITE);
            painter.rect_stroke(
                handle_rect,
                1.0,
                egui::Stroke::new(1.0, Color32::from_rgb(30, 30, 30)),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// 浮動片(ARCHITECTURE.md §7: 「浮動片の表示は独立した小テクスチャで
    /// canvas_view が描く」)を描く。
    pub fn draw_floating(&mut self, painter: &egui::Painter, floating: &Floating) {
        if floating.w == 0 || floating.h == 0 {
            return;
        }
        let needs_rebuild = match &self.floating_texture {
            Some((id, _)) => *id != floating.id,
            None => true,
        };
        if needs_rebuild {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [floating.w as usize, floating.h as usize],
                &floating.pixels,
            );
            let tex = painter
                .ctx()
                .load_texture("darask-floating", image, texture_options());
            self.floating_texture = Some((floating.id, tex));
        }
        let Some((_, tex)) = &self.floating_texture else {
            return;
        };
        let ppp = self.last_ppp;
        let min = img_to_screen(floating.pos, self.viewport.min, self.pan, self.zoom, ppp);
        let size = vec2(floating.w as f32, floating.h as f32) * (self.zoom / ppp);
        painter.image(
            tex.id(),
            Rect::from_min_size(min, size),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// 中央パネル全域にキャンバスを描画し、ポインタ入力を `ToolEvent` に
    /// 変換して返す(ARCHITECTURE.md §3, §4)。
    ///
    /// `force_pan` は「手のひら」ツールが選択中であることを示し、その場合は
    /// 左ドラッグもパンとして扱う。`cursor` は(パン中でなければ)表示する
    /// カーソル形状。
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        force_pan: bool,
        cursor: egui::CursorIcon,
    ) -> CanvasOutput {
        let rect = ui.available_rect_before_wrap();
        self.viewport = rect;
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        self.last_ppp = ppp;

        self.ensure_texture(ui.ctx(), doc);
        self.clamp_pan_to_doc(doc, ppp);

        let painter = ui.painter_at(rect);
        let image_rect = self.image_screen_rect(doc, ppp);
        if let Some(image_rect) = image_rect {
            draw_checkerboard(&painter, image_rect.intersect(rect));
        }
        self.draw_image(&painter, doc, ppp);

        let mut events = Vec::new();
        self.handle_wheel(ui, &response, doc, ppp);
        self.handle_pointer(ui, &response, force_pan, ppp, &mut events);

        let effective_cursor = match self.gesture {
            Some(Gesture { is_pan: true, .. }) => egui::CursorIcon::Grabbing,
            _ if force_pan && response.hovered() => egui::CursorIcon::Grab,
            _ => cursor,
        };
        if response.hovered() || self.gesture.is_some() {
            ui.ctx().set_cursor_icon(effective_cursor);
        }

        CanvasOutput { events, painter }
    }

    fn image_screen_rect(&self, doc: &Document, ppp: f32) -> Option<Rect> {
        if doc.width == 0 || doc.height == 0 {
            return None;
        }
        let min = img_to_screen(pos2(0.0, 0.0), self.viewport.min, self.pan, self.zoom, ppp);
        let max = img_to_screen(
            pos2(doc.width as f32, doc.height as f32),
            self.viewport.min,
            self.pan,
            self.zoom,
            ppp,
        );
        Some(Rect::from_min_max(min, max))
    }

    fn img_size_screen(&self, doc: &Document, ppp: f32) -> Vec2 {
        vec2(doc.width as f32, doc.height as f32) * (self.zoom / ppp)
    }

    fn clamp_pan_to_doc(&mut self, doc: &Document, ppp: f32) {
        if doc.width == 0 || doc.height == 0 {
            return;
        }
        let img_size = self.img_size_screen(doc, ppp);
        self.pan = clamp_pan(self.pan, img_size, self.viewport.size(), PAN_VISIBLE_MARGIN);
    }

    fn ensure_texture(&mut self, ctx: &egui::Context, doc: &mut Document) {
        if doc.width == 0 || doc.height == 0 {
            self.texture = None;
            self.texture_size = (doc.width, doc.height);
            doc.dirty = None;
            return;
        }

        let size = (doc.width, doc.height);
        if self.texture.is_none() || self.texture_size != size {
            // v2 §14.1: 全面再アップロードは新規/開く/サイズ変更時のみ。この
            // 分岐でだけ合成を全面再計算する(`composite` が正しいサイズ・
            // 内容であることをここで保証する)。
            doc.recomposite_full();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [doc.width as usize, doc.height as usize],
                &doc.composite,
            );
            match &mut self.texture {
                Some(tex) => tex.set(image, texture_options()),
                None => {
                    self.texture = Some(ctx.load_texture("darask-doc", image, texture_options()))
                }
            }
            self.texture_size = size;
            doc.dirty = None;
            return;
        }

        if let Some(dirty) = doc.dirty.take() {
            let rect = dirty.clamp_to(doc.width, doc.height);
            if !rect.is_empty() {
                // v2 §14.1: 「毎フレーム: dirty があれば recomposite(rect) →
                // その rect をテクスチャ部分更新」。ストローク中に何度も
                // raster 関数を呼んでも、合成はフレームごとに 1 回だけ
                // (蓄積された dirty 矩形ぶん)で済む。
                doc.recomposite(rect);
                let sub = extract_sub_image(doc, rect);
                if let Some(tex) = &mut self.texture {
                    tex.set_partial([rect.x0 as usize, rect.y0 as usize], sub, texture_options());
                }
            }
        }
    }

    fn draw_image(&self, painter: &egui::Painter, doc: &Document, ppp: f32) {
        let Some(tex) = &self.texture else {
            return;
        };
        let Some(rect) = self.image_screen_rect(doc, ppp) else {
            return;
        };
        // ARCHITECTURE.md §2: 100% でくっきり表示するため描画矩形の原点を
        // 物理ピクセル格子に丸める(サイズはそのまま保つ)。
        let snapped_min = pos2(
            snap_to_pixel_grid(rect.min.x, ppp),
            snap_to_pixel_grid(rect.min.y, ppp),
        );
        let snapped_rect = Rect::from_min_size(snapped_min, rect.size());
        painter.image(
            tex.id(),
            snapped_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn handle_wheel(&mut self, ui: &egui::Ui, response: &egui::Response, doc: &Document, ppp: f32) {
        if !response.contains_pointer() {
            return;
        }
        let Some(pointer) = ui.ctx().input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let wheel_events: Vec<(MouseWheelUnit, Vec2, Modifiers)> = ui.ctx().input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } => Some((*unit, *delta, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        if wheel_events.is_empty() {
            return;
        }

        let mut changed = false;
        for (unit, delta, modifiers) in wheel_events {
            if modifiers.ctrl {
                let notches = if delta.y > 0.0 {
                    1
                } else if delta.y < 0.0 {
                    -1
                } else {
                    0
                };
                if notches != 0 {
                    self.zoom_at(notches, pointer, ppp);
                    changed = true;
                }
            } else {
                let pts = wheel_delta_to_points(unit, delta, self.viewport.height());
                if modifiers.shift {
                    self.pan.x += pts.x + pts.y;
                } else {
                    self.pan += pts;
                }
                changed = true;
            }
        }
        if changed {
            self.clamp_pan_to_doc(doc, ppp);
        }
    }

    fn zoom_at(&mut self, notches: i32, anchor_screen: Pos2, ppp: f32) {
        let anchor_img = screen_to_img(anchor_screen, self.viewport.min, self.pan, self.zoom, ppp);
        let new_zoom = apply_zoom_step(self.zoom, notches);
        if new_zoom == self.zoom {
            return;
        }
        self.zoom = new_zoom;
        self.pan = pan_for_anchor(anchor_img, anchor_screen, self.viewport.min, self.zoom, ppp);
    }

    /// ポインタの押下/ドラッグ/解放を `ToolEvent` に変換する。SPEC §4/§10:
    /// Space 押下中ドラッグ・中ボタンドラッグ・「手のひら」選択中の左ドラッグは
    /// (現在のツールに関係なく)パンとして横取りし、`ToolEvent` を出さない。
    ///
    /// egui の `Response::dragged()` はクリックとの判別のためドラッグ開始を
    /// 数ピクセル遅らせることがあるため使わず、生のポインタ状態を直接見て
    /// 押した瞬間から追従できるようにしている(ARCHITECTURE.md §12-4 の
    /// 「ボタン別に分岐」を、遅延のない生の状態で行う形)。
    fn handle_pointer(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        force_pan: bool,
        ppp: f32,
        events: &mut Vec<ToolEvent>,
    ) {
        let (modifiers, pos, primary, secondary, middle) = ui.ctx().input(|i| {
            (
                i.modifiers,
                i.pointer.latest_pos(),
                ButtonState::from_input(i, PointerButton::Primary),
                ButtonState::from_input(i, PointerButton::Secondary),
                ButtonState::from_input(i, PointerButton::Middle),
            )
        });

        let Some(pos) = pos else {
            self.hover_img = None;
            return;
        };

        if let Some(gesture) = self.gesture {
            let state = match gesture.button {
                PointerButton::Primary => primary,
                PointerButton::Secondary => secondary,
                PointerButton::Middle => middle,
                _ => ButtonState::default(),
            };
            if state.down {
                if gesture.is_pan {
                    self.pan += pos - self.last_pointer;
                } else {
                    let img = screen_to_img(pos, self.viewport.min, self.pan, self.zoom, ppp);
                    events.push(ToolEvent::Drag {
                        img,
                        button: gesture.button,
                        mods: modifiers,
                    });
                }
                self.last_pointer = pos;
            } else {
                if !gesture.is_pan {
                    let img = screen_to_img(pos, self.viewport.min, self.pan, self.zoom, ppp);
                    events.push(ToolEvent::Up {
                        img,
                        button: gesture.button,
                    });
                }
                self.gesture = None;
            }
            self.hover_img = Some(screen_to_img(
                pos,
                self.viewport.min,
                self.pan,
                self.zoom,
                ppp,
            ));
            return;
        }

        if response.contains_pointer() {
            if middle.pressed {
                self.gesture = Some(Gesture {
                    button: PointerButton::Middle,
                    is_pan: true,
                });
                self.last_pointer = pos;
            } else if primary.pressed {
                let is_pan = force_pan || ui.ctx().input(|i| i.key_down(Key::Space));
                self.gesture = Some(Gesture {
                    button: PointerButton::Primary,
                    is_pan,
                });
                self.last_pointer = pos;
                if !is_pan {
                    let img = screen_to_img(pos, self.viewport.min, self.pan, self.zoom, ppp);
                    events.push(ToolEvent::Down {
                        img,
                        button: PointerButton::Primary,
                        mods: modifiers,
                    });
                }
            } else if secondary.pressed {
                self.gesture = Some(Gesture {
                    button: PointerButton::Secondary,
                    is_pan: false,
                });
                self.last_pointer = pos;
                let img = screen_to_img(pos, self.viewport.min, self.pan, self.zoom, ppp);
                events.push(ToolEvent::Down {
                    img,
                    button: PointerButton::Secondary,
                    mods: modifiers,
                });
            }
        }

        if self.gesture.is_none() && response.contains_pointer() {
            let img = screen_to_img(pos, self.viewport.min, self.pan, self.zoom, ppp);
            self.hover_img = Some(img);
            events.push(ToolEvent::Hover { img });
        } else if self.gesture.is_none() {
            self.hover_img = None;
        }
    }
}

#[derive(Default, Clone, Copy)]
struct ButtonState {
    pressed: bool,
    down: bool,
}

impl ButtonState {
    fn from_input(input: &egui::InputState, button: PointerButton) -> Self {
        Self {
            pressed: input.pointer.button_pressed(button),
            down: input.pointer.button_down(button),
        }
    }
}

fn texture_options() -> TextureOptions {
    TextureOptions {
        magnification: TextureFilter::Nearest,
        minification: TextureFilter::Linear,
        wrap_mode: TextureWrapMode::ClampToEdge,
        mipmap_mode: None,
    }
}

/// v2 §14.1: テクスチャは合成 1 枚のまま(レイヤーごとにテクスチャを持たない)。
fn extract_sub_image(doc: &Document, rect: IRect) -> egui::ColorImage {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut bytes = vec![0u8; w * h * 4];
    for y in 0..h {
        let doc_start = ((rect.y0 as usize + y) * doc.width as usize + rect.x0 as usize) * 4;
        let out_start = y * w * 4;
        bytes[out_start..out_start + w * 4]
            .copy_from_slice(&doc.composite[doc_start..doc_start + w * 4]);
    }
    egui::ColorImage::from_rgba_unmultiplied([w, h], &bytes)
}

/// 市松模様(SPEC §3: 透明ピクセルの下に表示)。`rect` は画像の画面上矩形を
/// viewport にクリップしたもの。
fn draw_checkerboard(painter: &egui::Painter, rect: Rect) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    const LIGHT: Color32 = Color32::from_gray(205);
    const DARK: Color32 = Color32::from_gray(165);

    let col0 = (rect.min.x / CHECKER_CELL).floor() as i64;
    let row0 = (rect.min.y / CHECKER_CELL).floor() as i64;
    let col1 = (rect.max.x / CHECKER_CELL).ceil() as i64;
    let row1 = (rect.max.y / CHECKER_CELL).ceil() as i64;

    for row in row0..row1 {
        for col in col0..col1 {
            let cell = Rect::from_min_size(
                pos2(col as f32 * CHECKER_CELL, row as f32 * CHECKER_CELL),
                vec2(CHECKER_CELL, CHECKER_CELL),
            )
            .intersect(rect);
            if cell.width() <= 0.0 || cell.height() <= 0.0 {
                continue;
            }
            let color = if (row + col) % 2 == 0 { LIGHT } else { DARK };
            painter.rect_filled(cell, 0.0, color);
        }
    }
}

/// 選択枠の点線(ARCHITECTURE.md §7:「点線はアニメーションさせない」ため、
/// 位相は常にエッジの始点からの固定オフセットで、時刻に依存しない)。
fn draw_dashed_rect(painter: &egui::Painter, rect: Rect) {
    const DASH: f32 = 6.0;
    const GAP: f32 = 4.0;
    const COLOR: Color32 = Color32::from_rgb(30, 30, 30);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    for w in corners.windows(2) {
        draw_dashed_segment(painter, w[0], w[1], DASH, GAP, COLOR);
    }
}

fn draw_dashed_segment(
    painter: &egui::Painter,
    from: Pos2,
    to: Pos2,
    dash: f32,
    gap: f32,
    color: Color32,
) {
    let delta = to - from;
    let len = delta.length();
    if len <= 0.0 {
        return;
    }
    let dir = delta / len;
    let mut t = 0.0;
    while t < len {
        let seg_end = (t + dash).min(len);
        let a = from + dir * t;
        let b = from + dir * seg_end;
        painter.line_segment([a, b], egui::Stroke::new(1.5, color));
        t += dash + gap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn img_to_screen_screen_to_img_round_trip_identity() {
        let cases = [
            (1.0_f32, 1.0_f32, Vec2::ZERO),
            (2.0, 1.0, vec2(10.0, -5.0)),
            (0.5, 1.25, vec2(-100.0, 200.0)),
            (3.75, 2.0, vec2(0.0, 0.0)),
            (0.1, 1.0, vec2(500.0, 500.0)),
        ];
        let viewport_min = pos2(37.0, 12.0);
        let points = [
            pos2(0.0, 0.0),
            pos2(123.4, 56.7),
            pos2(-10.0, -20.0),
            pos2(4000.0, 4000.0),
        ];
        for (zoom, ppp, pan) in cases {
            for p in points {
                let s = img_to_screen(p, viewport_min, pan, zoom, ppp);
                let back = screen_to_img(s, viewport_min, pan, zoom, ppp);
                assert!(
                    (back.x - p.x).abs() < 1e-2,
                    "x round trip failed: {p:?} -> {s:?} -> {back:?} (zoom={zoom}, ppp={ppp})"
                );
                assert!(
                    (back.y - p.y).abs() < 1e-2,
                    "y round trip failed: {p:?} -> {s:?} -> {back:?} (zoom={zoom}, ppp={ppp})"
                );
            }
        }
    }

    #[test]
    fn zoom_at_cursor_keeps_anchor_point_fixed() {
        let viewport_min = pos2(0.0, 0.0);
        let ppp = 1.5_f32;
        let old_zoom = 1.0_f32;
        let pan = vec2(20.0, -30.0);
        let anchor_screen = pos2(200.0, 150.0);

        let anchor_img = screen_to_img(anchor_screen, viewport_min, pan, old_zoom, ppp);
        let new_zoom = apply_zoom_step(old_zoom, 1);
        let new_pan = pan_for_anchor(anchor_img, anchor_screen, viewport_min, new_zoom, ppp);

        let anchor_screen_after = img_to_screen(anchor_img, viewport_min, new_pan, new_zoom, ppp);
        assert!((anchor_screen_after.x - anchor_screen.x).abs() < 1e-3);
        assert!((anchor_screen_after.y - anchor_screen.y).abs() < 1e-3);
    }

    #[test]
    fn apply_zoom_step_multiplies_by_1_25_per_notch() {
        let z1 = apply_zoom_step(1.0, 1);
        assert!((z1 - 1.25).abs() < 1e-5);
        let z2 = apply_zoom_step(z1, 1);
        assert!((z2 - 1.5625).abs() < 1e-4);
        let back = apply_zoom_step(z2, -1);
        assert!((back - z1).abs() < 1e-4);
    }

    #[test]
    fn apply_zoom_step_clamps_to_range() {
        assert_eq!(apply_zoom_step(MAX_ZOOM, 5), MAX_ZOOM);
        assert_eq!(apply_zoom_step(MIN_ZOOM, -5), MIN_ZOOM);
    }

    #[test]
    fn clamp_pan_keeps_margin_visible_when_image_larger_than_viewport() {
        let img_size = vec2(2000.0, 1500.0);
        let viewport = vec2(800.0, 600.0);
        // 右下に大きくパンしすぎ -> 右端/下端が margin より内側に来てはいけない。
        let pan = clamp_pan(vec2(5000.0, 5000.0), img_size, viewport, 32.0);
        assert!(pan.x <= 32.0 - img_size.x + 1e-3 || pan.x + img_size.x >= 32.0 - 1e-3);
        assert!(pan.x + img_size.x >= 32.0 - 1e-3);
        assert!(pan.y + img_size.y >= 32.0 - 1e-3);

        // 左上に大きくパンしすぎ -> 左端/上端が margin より外に出てはいけない。
        let pan2 = clamp_pan(vec2(-5000.0, -5000.0), img_size, viewport, 32.0);
        assert!(pan2.x <= viewport.x - 32.0 + 1e-3);
        assert!(pan2.y <= viewport.y - 32.0 + 1e-3);
    }

    #[test]
    fn clamp_pan_centers_when_image_smaller_than_margins() {
        // 画像がマージンより小さい極端なケースでもパニックせず、有限な値を返す。
        let pan = clamp_pan(vec2(1000.0, 1000.0), vec2(5.0, 5.0), vec2(50.0, 50.0), 32.0);
        assert!(pan.x.is_finite());
        assert!(pan.y.is_finite());
    }

    #[test]
    fn wheel_delta_to_points_converts_units() {
        let p = wheel_delta_to_points(MouseWheelUnit::Point, vec2(1.0, 2.0), 600.0);
        assert_eq!(p, vec2(1.0, 2.0));

        let line = wheel_delta_to_points(MouseWheelUnit::Line, vec2(1.0, 0.0), 600.0);
        assert_eq!(line, vec2(LINE_SCROLL_SPEED, 0.0));

        let page = wheel_delta_to_points(MouseWheelUnit::Page, vec2(0.0, 1.0), 600.0);
        assert_eq!(page, vec2(0.0, 600.0));
    }

    #[test]
    fn new_canvas_view_has_sane_defaults() {
        let view = CanvasView::new();
        assert_eq!(view.zoom, 1.0);
        assert_eq!(view.pan, Vec2::ZERO);
        assert!(view.hover_img().is_none());
    }
}
