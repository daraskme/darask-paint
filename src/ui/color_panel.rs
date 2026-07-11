//! カラーパネル(右パネル上段、SPEC §14、ARCHITECTURE.md §14.3/§14.4/§14.7)。
//!
//! egui 標準のポップアップカラーピッカー(`color_edit_button_srgba`)は
//! v1/M3 で使っていたが、v2 M3 でここへ置き換えて廃止する
//! (ARCHITECTURE.md §14.3)。表示するのは常時:
//! 1. プライマリ/セカンダリの重ね表示スウォッチ + 入替ボタン。
//! 2. 色相リング + SV 三角形(`color_wheel::ColorWheelState`)。
//! 3. アルファスライダー(市松模様の下地付き)+ HEX 入力欄。
//! 4. パレット(定義済み 24 色 + ユーザーパレット)。
//! 5. 最近使った色(v1 ではオプションバーにあったものをここへ移設)。
//!
//! SPEC §14: 「編集対象は常にプライマリ」。セカンダリはパレット/最近使った
//! 色の右クリック、Alt+右クリックの一時スポイト、または入替ボタン(X)
//! でのみ変わる。

use std::collections::VecDeque;

use eframe::egui::{self, vec2, Color32, PointerButton, Rect, Sense, Stroke, StrokeKind};

use crate::ui::color_wheel::{rgba_unmultiplied_keep_rgb, ColorWheelState};

/// SPEC §14 の 1 段目 12 色。
pub const PALETTE_ROW1: [Color32; 12] = [
    Color32::from_rgb(0x00, 0x00, 0x00), // 黒
    Color32::from_rgb(0x7F, 0x7F, 0x7F), // 灰
    Color32::from_rgb(0xFF, 0xFF, 0xFF), // 白
    Color32::from_rgb(0xE5, 0x39, 0x35), // 赤
    Color32::from_rgb(0xFB, 0x8C, 0x00), // 橙
    Color32::from_rgb(0xFD, 0xD8, 0x35), // 黄
    Color32::from_rgb(0x43, 0xA0, 0x47), // 緑
    Color32::from_rgb(0x00, 0xAC, 0xC1), // 水
    Color32::from_rgb(0x1E, 0x88, 0xE5), // 青
    Color32::from_rgb(0x8E, 0x24, 0xAA), // 紫
    Color32::from_rgb(0xEC, 0x40, 0x7A), // 桃
    Color32::from_rgb(0x6D, 0x4C, 0x41), // 茶
];

/// 2 段目(各色の明るいパステル調)を白へ寄せる割合。SPEC §14 は正確な
/// HEX 値を指定していないため、1 段目から決定的に導出する(deviations 参照)。
const PALETTE_PASTEL_WHITE_MIX: f32 = 0.55;

const RECENT_SWATCH_SIZE: egui::Vec2 = vec2(20.0, 20.0);
const PALETTE_SWATCH_SIZE: egui::Vec2 = vec2(14.0, 14.0);
const ALPHA_SLIDER_SIZE: egui::Vec2 = vec2(150.0, 18.0);
const SWATCH_BORDER: Color32 = Color32::from_gray(60);

/// `show` に渡すカラーパネルの状態一式(`app.rs` が各フィールドの持ち主
/// から可変参照を集めて構築する、`OptionsBarCtx`/`LayersPanelAction` と
/// 同じ設計)。
pub struct ColorPanelCtx<'a> {
    pub primary: &'a mut Color32,
    pub secondary: &'a mut Color32,
    pub wheel: &'a mut ColorWheelState,
    /// HEX 入力欄の編集中テキスト(`DaraskApp` が保持、フォーカス中は
    /// `primary` から再同期しない)。
    pub hex_buffer: &'a mut String,
    pub recent_colors: &'a VecDeque<Color32>,
    /// SPEC §14: 「永続化はしない」。
    pub user_palette: &'a mut Vec<Color32>,
}

pub fn show(ui: &mut egui::Ui, ctx: ColorPanelCtx) {
    let ColorPanelCtx {
        primary,
        secondary,
        wheel,
        hex_buffer,
        recent_colors,
        user_palette,
    } = ctx;

    ui.heading("色");
    ui.add_space(4.0);

    show_swatches(ui, primary, secondary);
    ui.add_space(6.0);

    // ARCHITECTURE.md §14.3: ドラッグ中でなければ primary から HSV を
    // 同期する(パレット/HEX/最近使った色/スポイト等、ホイール以外の
    // 経路での色変更をマーカー位置へ反映するため)。
    wheel.sync_if_idle(*primary);
    ui.vertical_centered(|ui| {
        wheel.ui(ui, primary);
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("A:");
        alpha_slider(ui, primary);
    });
    ui.horizontal(|ui| {
        ui.label("HEX:");
        hex_input(ui, primary, hex_buffer);
    });
    ui.add_space(8.0);

    palette_section(ui, primary, secondary, user_palette);
    ui.add_space(8.0);

    recent_colors_row(ui, recent_colors, primary);
}

/// SPEC §14 項目1: プライマリ/セカンダリの重ね表示 + 入替ボタン
/// (X キーと同じ)。
fn show_swatches(ui: &mut egui::Ui, primary: &mut Color32, secondary: &mut Color32) {
    ui.horizontal(|ui| {
        let (outer, _response) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
        let secondary_rect = Rect::from_min_size(outer.min + vec2(12.0, 12.0), vec2(26.0, 26.0));
        let primary_rect = Rect::from_min_size(outer.min, vec2(26.0, 26.0));
        if ui.is_rect_visible(outer) {
            draw_swatch_at(ui.painter(), secondary_rect, *secondary, 1.0);
            draw_swatch_at(ui.painter(), primary_rect, *primary, 1.5);
        }
        if ui
            .button("入替")
            .on_hover_text("プライマリ/セカンダリを入れ替え(X)")
            .clicked()
        {
            std::mem::swap(primary, secondary);
        }
    });
}

/// SPEC §14 項目3: 「アルファスライダー(0–255、市松模様の下地付き)」。
/// `primary` の RGB は保ったままアルファだけを変える。
///
/// v2 レビューで発見・修正したバグ2件:
/// (1) ecolor 0.35 の `Color32::from_rgba_unmultiplied` は alpha==0 のとき
///     RGB を捨てて `TRANSPARENT`(黒相当)を返す(共通ケース最適化、
///     ecolor-0.35.0/src/color32.rs 参照)。左端までドラッグして A=0 に
///     すると選んだ色相が失われ、A を戻すと黒になっていた。下の勾配
///     描画(136 行目以降)はこの罠を回避する専用コードを既に持っていた
///     のに、スライダー本体(実際に `primary` へ書き戻す側)は無防備
///     だった。RGB を保つ `rgba_unmultiplied_keep_rgb` を使う。
/// (2) `response.interact_pointer_pos()` はボタン種別を区別しないため、
///     右クリック・右ドラッグでもプライマリのアルファが変わってしまって
///     いた(パレット等の「右=セカンダリ」の慣習と衝突する)。プライマリ
///     ボタンが押されている間だけ入力を受け付ける。
fn alpha_slider(ui: &mut egui::Ui, primary: &mut Color32) {
    let (rect, response) = ui.allocate_exact_size(ALPHA_SLIDER_SIZE, Sense::click_and_drag());
    let primary_down = ui.input(|i| i.pointer.button_down(PointerButton::Primary));
    if primary_down {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            let [r, g, b, _a] = primary.to_srgba_unmultiplied();
            *primary = rgba_unmultiplied_keep_rgb(r, g, b, (t * 255.0).round() as u8);
        }
    }
    if ui.is_rect_visible(rect) {
        draw_checker(ui.painter(), rect);
        let [r, g, b, a] = primary.to_srgba_unmultiplied();
        // Mesh の頂点色は premultiplied(epaint::Vertex のドキュメント参照)。
        // `Color32::from_rgba_unmultiplied` に alpha=0 を渡すと RGB ごと
        // TRANSPARENT に潰れてしまう(共通ケース最適化)ため、勾配の両端は
        // 自前で premultiply する。
        let premultiplied_stop = |alpha: u8| -> Color32 {
            let af = alpha as f32 / 255.0;
            let pm = |c: u8| ((c as f32) * af).round() as u8;
            Color32::from_rgba_premultiplied(pm(r), pm(g), pm(b), alpha)
        };
        let (c0, c1) = (premultiplied_stop(0), premultiplied_stop(255));
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), c0);
        mesh.colored_vertex(rect.left_bottom(), c0);
        mesh.colored_vertex(rect.right_top(), c1);
        mesh.colored_vertex(rect.right_bottom(), c1);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, SWATCH_BORDER),
            StrokeKind::Middle,
        );
        let x = egui::lerp(rect.left()..=rect.right(), a as f32 / 255.0);
        ui.painter().line_segment(
            [
                egui::pos2(x, rect.top() - 1.0),
                egui::pos2(x, rect.bottom() + 1.0),
            ],
            Stroke::new(2.0, Color32::from_gray(20)),
        );
    }
}

/// SPEC §14 項目3: 「HEX 入力欄(#RRGGBB / #RRGGBBAA、確定で反映)」。
fn hex_input(ui: &mut egui::Ui, primary: &mut Color32, hex_buffer: &mut String) {
    let response = ui.add(
        egui::TextEdit::singleline(hex_buffer)
            .id(egui::Id::new("darask_color_hex"))
            .desired_width(84.0)
            .hint_text("#RRGGBB"),
    );
    // レイヤー名編集(`layers_panel.rs`)と同じパターン: `lost_focus` を
    // 「確定」の合図として使う(Enter・クリックで外れる、いずれも対応)。
    if response.lost_focus() {
        if let Some(color) = parse_hex_color(hex_buffer) {
            *primary = color;
        }
    }
    // フォーカスが無い間だけ現在の primary を表示する(編集中は上書きしない)。
    if !response.has_focus() {
        *hex_buffer = format_hex(*primary);
    }
}

/// SPEC §14 項目4: 定義済み 24 色 + ユーザーパレット。
fn palette_section(
    ui: &mut egui::Ui,
    primary: &mut Color32,
    secondary: &mut Color32,
    user_palette: &mut Vec<Color32>,
) {
    ui.label("パレット:");
    // SPEC §14: 「2 段 × 12」。既定の `item_spacing`(8pt)のままだと
    // 210px 幅の右パネルに 12 個が収まらず 3 行以上に折り返されるため、
    // 詰めて 1 段 12 個が収まるようにする(`horizontal_wrapped` 自体は
    // 幅超過時に自動折り返しするのでパニックはしないが、意図した
    // 見た目に近づける)。
    //
    // v2 レビューで発見・修正したバグ: `ui.spacing_mut()` は「この `Ui` と
    // それ以降の子」に効く(egui 0.35 のドキュメントどおり)ため、
    // `ui.scope()` で包まずに直接変更すると、呼び出し元(side_panel.rs の
    // `ScrollArea` コンテンツ Ui)へそのまま漏れ、この後に描かれる
    // `ui.separator()` や `layers_panel::show`(行チェックボックス・名前、
    // 新規/複製/削除等のボタン列、不透明度スライダー)まで水平間隔が
    // 8pt→2pt に詰まってしまっていた。`ui.scope()` で子スコープを作り、
    // 変更をこの関数の中だけに閉じ込める。
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        ui.horizontal_wrapped(|ui| {
            for color in PALETTE_ROW1 {
                fixed_palette_swatch(ui, color, primary, secondary);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for color in palette_row2() {
                fixed_palette_swatch(ui, color, primary, secondary);
            }
        });

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let mut remove_idx = None;
            for (idx, &color) in user_palette.iter().enumerate() {
                let response = color_swatch(ui, PALETTE_SWATCH_SIZE, color, Sense::click())
                    .on_hover_text("クリックでプライマリに設定、右クリックで削除メニュー");
                if response.clicked() {
                    *primary = color;
                }
                // SPEC §14: 「ユーザー色は右クリックメニューで削除」。定義済み
                // パレットと違い、右クリックはセカンダリ設定ではなく削除メニュー
                // に割り当てる(右クリックの意味がユーザー色だけ異なることの
                // 混同を避けるため、二重の意味を持たせない設計)。
                response.context_menu(|ui| {
                    if ui.button("削除").clicked() {
                        remove_idx = Some(idx);
                        ui.close();
                    }
                });
            }
            if ui
                .button("＋")
                .on_hover_text("プライマリ色をユーザーパレットに追加")
                .clicked()
            {
                user_palette.push(*primary);
            }
            if let Some(idx) = remove_idx {
                if idx < user_palette.len() {
                    user_palette.remove(idx);
                }
            }
        });
    });
}

fn fixed_palette_swatch(
    ui: &mut egui::Ui,
    color: Color32,
    primary: &mut Color32,
    secondary: &mut Color32,
) {
    let response = color_swatch(ui, PALETTE_SWATCH_SIZE, color, Sense::click())
        .on_hover_text("クリックでプライマリに、右クリックでセカンダリに設定");
    if response.clicked() {
        *primary = color;
    }
    if response.secondary_clicked() {
        *secondary = color;
    }
}

/// SPEC §14 項目5: 「最近使った色 8 個」(v1 のものをオプションバーから
/// ここへ移動)。
///
/// v2 レビューで発見・修正したバグ: `palette_section` と同じ item_spacing
/// 漏れ(`ui.scope()` 無し)があった。
fn recent_colors_row(ui: &mut egui::Ui, recent: &VecDeque<Color32>, primary: &mut Color32) {
    ui.label("最近使った色:");
    if recent.is_empty() {
        ui.weak("(まだありません)");
        return;
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        ui.horizontal_wrapped(|ui| {
            for &color in recent.iter() {
                let response = color_swatch(ui, RECENT_SWATCH_SIZE, color, Sense::click())
                    .on_hover_text("クリックでプライマリに設定");
                if response.clicked() {
                    *primary = color;
                }
            }
        });
    });
}

// ---------------------------------------------------------------------
// スウォッチ描画・市松模様の共通ヘルパ
// ---------------------------------------------------------------------

fn color_swatch(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    color: Color32,
    sense: Sense,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        draw_swatch_at(ui.painter(), rect, color, 1.0);
    }
    response
}

fn draw_swatch_at(painter: &egui::Painter, rect: Rect, color: Color32, border_width: f32) {
    draw_checker(painter, rect);
    painter.rect_filled(rect, 2.0, color);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(border_width, SWATCH_BORDER),
        StrokeKind::Middle,
    );
}

/// アルファのある色を示すための簡易市松模様(SPEC §14 のアルファスライダー
/// だけでなく、アルファ付き色を表示しうるスウォッチ全般に使う)。
fn draw_checker(painter: &egui::Painter, rect: Rect) {
    const LIGHT: Color32 = Color32::from_gray(205);
    const DARK: Color32 = Color32::from_gray(165);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    painter.rect_filled(rect, 0.0, LIGHT);
    let cell = (rect.width().min(rect.height()) / 2.0).max(2.0);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 != 0 {
                continue;
            }
            let cell_rect = Rect::from_min_size(
                rect.min + vec2(col as f32 * cell, row as f32 * cell),
                vec2(cell, cell),
            )
            .intersect(rect);
            if cell_rect.width() > 0.0 && cell_rect.height() > 0.0 {
                painter.rect_filled(cell_rect, 0.0, DARK);
            }
        }
    }
}

// ---------------------------------------------------------------------
// 純関数: パレット導出・HEX 変換(テスト対象)
// ---------------------------------------------------------------------

/// SPEC §14: 「2 段目は各色の明るいパステル調」。正確な HEX 値の指定が
/// 無いため、1 段目を白へ `PALETTE_PASTEL_WHITE_MIX` の割合で混色して
/// 決定的に導出する。
pub fn palette_row2() -> [Color32; 12] {
    let mut row = PALETTE_ROW1;
    for color in &mut row {
        *color = pastel(*color);
    }
    row
}

fn pastel(c: Color32) -> Color32 {
    let mix = |ch: u8| -> u8 {
        (ch as f32 * (1.0 - PALETTE_PASTEL_WHITE_MIX) + 255.0 * PALETTE_PASTEL_WHITE_MIX)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

/// `#RRGGBB` / `#RRGGBBAA`(先頭 `#` は省略可)をパースする。それ以外は
/// `None`(SPEC §14: 「確定で反映」、無効な入力は反映しない)。
pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let s = s.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            Some(Color32::from_rgba_unmultiplied(r, g, b, a))
        }
        _ => None,
    }
}

/// `parse_hex_color` の逆変換。アルファが不透明(255)なら 6 桁、そうで
/// なければ 8 桁で表示する。
pub fn format_hex(color: Color32) -> String {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    if a == 255 {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_row1_matches_spec_hex_values() {
        let expected: [(u8, u8, u8); 12] = [
            (0x00, 0x00, 0x00),
            (0x7F, 0x7F, 0x7F),
            (0xFF, 0xFF, 0xFF),
            (0xE5, 0x39, 0x35),
            (0xFB, 0x8C, 0x00),
            (0xFD, 0xD8, 0x35),
            (0x43, 0xA0, 0x47),
            (0x00, 0xAC, 0xC1),
            (0x1E, 0x88, 0xE5),
            (0x8E, 0x24, 0xAA),
            (0xEC, 0x40, 0x7A),
            (0x6D, 0x4C, 0x41),
        ];
        for (color, (r, g, b)) in PALETTE_ROW1.iter().zip(expected) {
            assert_eq!(color.to_srgba_unmultiplied(), [r, g, b, 255]);
        }
    }

    #[test]
    fn palette_row2_is_lighter_and_less_saturated() {
        let row2 = palette_row2();
        for (base, pastel_color) in PALETTE_ROW1.iter().zip(row2.iter()) {
            let [br, bg, bb, _] = base.to_srgba_unmultiplied();
            let [pr, pg, pb, pa] = pastel_color.to_srgba_unmultiplied();
            assert_eq!(pa, 255);
            // 白へ寄せた色なので、各チャンネルは元の色以上(黒以外は真に
            // 明るくなる)。
            assert!(pr >= br && pg >= bg && pb >= bb);
        }
    }

    #[test]
    fn parse_hex_color_accepts_rgb_and_rgba() {
        assert_eq!(
            parse_hex_color("#112233"),
            Some(Color32::from_rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(
            parse_hex_color("112233"),
            Some(Color32::from_rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(
            parse_hex_color("#11223380"),
            Some(Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0x80))
        );
    }

    #[test]
    fn parse_hex_color_rejects_invalid_input() {
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#12"), None);
        assert_eq!(parse_hex_color("#1234567"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
        assert_eq!(parse_hex_color("#123456789"), None);
    }

    #[test]
    fn format_hex_roundtrips_through_parse() {
        let opaque = Color32::from_rgb(0x11, 0x22, 0x33);
        assert_eq!(format_hex(opaque), "#112233");
        assert_eq!(parse_hex_color(&format_hex(opaque)), Some(opaque));

        // `Color32` は内部で premultiplied 保持するため(ecolor の
        // `to_srgba_unmultiplied` のドキュメント参照: 「for transparent
        // colors what you get back might be slightly different (rounding
        // errors)」)、非不透明色は 1 バイト分の丸め誤差が起こりうる。
        // 完全なバイト一致ではなく、丸め誤差の範囲内かつ 2 回目の
        // フォーマットで安定する(不動点に達する)ことを確認する。
        let translucent = Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0x80);
        let hex = format_hex(translucent);
        let parsed = parse_hex_color(&hex).expect("valid 8-digit hex must parse");
        let [pr, pg, pb, pa] = parsed.to_srgba_unmultiplied();
        assert!((pr as i32 - 0x11).abs() <= 1);
        assert!((pg as i32 - 0x22).abs() <= 1);
        assert!((pb as i32 - 0x33).abs() <= 1);
        assert_eq!(pa, 0x80);
        assert_eq!(
            format_hex(parsed),
            hex,
            "formatting must reach a fixed point"
        );
    }
}
