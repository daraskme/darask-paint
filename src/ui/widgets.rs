//! v13 §60 の共通部品(自前描画のアイコンボタン・スライダー・トグル類)。
//!
//! egui 標準の `Slider` はつまみの色がレールと同じ `widgets.inactive.bg_fill`
//! から決まり、モック(白いつまみ+アンバーの塗り)を再現できないため、
//! スライダーはここで自前描画する。どの部品も `Response` を返すだけで状態を
//! 持たない(毎フレームの再描画要求もしない — アイドル CPU 0%)。

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::ui::theme;

/// `ui/icons.rs` の描画関数の型。
pub type IconPaint = fn(&egui::Painter, Rect, Color32);

/// アイコンだけのボタン。通常は背景なし、ホバーで面を出し、`selected` は
/// アクセントの淡色面+縁+アクセント色のアイコン(選択中ツールと同じ意匠)。
/// 無効時はクリックを受けない。
pub fn icon_button(
    ui: &mut egui::Ui,
    size: Vec2,
    icon_size: f32,
    enabled: bool,
    selected: bool,
    paint: IconPaint,
    label: &str,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, stroke, icon) = button_colors(&response, enabled, selected);
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, 7.0, fill);
        }
        if let Some(stroke) = stroke {
            painter.rect_stroke(rect, 7.0, stroke, StrokeKind::Inside);
        }
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_size));
        paint(painter, icon_rect, icon);
    }
    response
}

/// アイコンボタン・ツールボタン共通の配色(背景, 縁, アイコン色)。
pub fn button_colors(
    response: &egui::Response,
    enabled: bool,
    selected: bool,
) -> (Color32, Option<Stroke>, Color32) {
    if !enabled {
        return (Color32::TRANSPARENT, None, theme::TEXT_DISABLED);
    }
    if selected {
        return (
            theme::ACCENT_SOFT,
            Some(Stroke::new(1.0, theme::ACCENT_LINE)),
            theme::ACCENT,
        );
    }
    if response.is_pointer_button_down_on() {
        (theme::BG_ACTIVE, None, Color32::WHITE)
    } else if response.hovered() {
        (theme::BG_HOVER, None, Color32::WHITE)
    } else {
        (Color32::TRANSPARENT, None, theme::ICON)
    }
}

/// 縦の区切り線(ツールバー・オプションバーのグループ境界)。
pub fn vertical_divider(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, theme::LINE_STRONG),
    );
}

/// ショートカット表記の小さなバッジ(「B」「Ctrl+J」など)。
pub fn shortcut_badge(ui: &mut egui::Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(theme::FONT_SMALL),
        theme::TEXT_DIM,
    );
    let size = galley.size() + vec2(10.0, 2.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 4.0, theme::BG_WIDGET);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, theme::TEXT_DIM);
    }
}

/// 個数などを出す丸いバッジ(パネル見出しの「4」など)。
pub fn count_badge(ui: &mut egui::Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(theme::FONT_SMALL),
        theme::TEXT_DIM,
    );
    let size = vec2((galley.size().x + 12.0).max(18.0), 16.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 8.0, theme::BG_WIDGET);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, theme::TEXT_DIM);
    }
}

/// 補足ラベル(オプションバー・プロパティ欄の項目名)。
pub fn field_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(egui::RichText::new(text).color(theme::TEXT_DIM))
}

/// スライダーのつまみの直径とレールの太さ。
const KNOB: f32 = 14.0;
const RAIL: f32 = 4.0;

/// 0..=1 の正規化値を編集する自前スライダー(白いつまみ+アンバーの塗り)。
/// 値を変えたフレームだけ `changed()` が立つ。
pub fn track(ui: &mut egui::Ui, t: &mut f32, width: f32, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, KNOB + 4.0), sense);
    let inner = rect.shrink2(vec2(KNOB / 2.0, 0.0));
    if enabled && (response.dragged() || response.clicked()) {
        if let Some(pos) = response.interact_pointer_pos() {
            let new_t = ((pos.x - inner.left()) / inner.width().max(1.0)).clamp(0.0, 1.0);
            if (new_t - *t).abs() > f32::EPSILON {
                *t = new_t;
                response.mark_changed();
            }
        }
    }
    if ui.is_rect_visible(rect) {
        paint_track(ui, inner, t.clamp(0.0, 1.0), enabled, &response);
    }
    response
}

fn paint_track(ui: &egui::Ui, inner: Rect, t: f32, enabled: bool, response: &egui::Response) {
    let painter = ui.painter();
    let y = inner.center().y;
    let rail = Rect::from_min_max(
        pos2(inner.left(), y - RAIL / 2.0),
        pos2(inner.right(), y + RAIL / 2.0),
    );
    painter.rect_filled(rail, RAIL / 2.0, theme::LINE_STRONG);
    let x = egui::lerp(inner.left()..=inner.right(), t);
    let fill_color = if enabled {
        theme::ACCENT
    } else {
        theme::TEXT_DISABLED
    };
    painter.rect_filled(
        Rect::from_min_max(rail.min, pos2(x, rail.max.y)),
        RAIL / 2.0,
        fill_color,
    );
    let knob_color = if !enabled {
        theme::TEXT_DISABLED
    } else if response.hovered() || response.dragged() {
        Color32::WHITE
    } else {
        Color32::from_rgb(0xf2, 0xf2, 0xf4)
    };
    painter.circle_filled(pos2(x, y + 1.0), KNOB / 2.0, Color32::from_black_alpha(110));
    painter.circle_filled(pos2(x, y), KNOB / 2.0, knob_color);
}

/// 掘り込みの数値欄(`DragValue` を入力欄風にした見た目)。
pub fn number_field<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    suffix: &str,
    width: f32,
) -> egui::Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        for state in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            state.weak_bg_fill = theme::BG_SUNKEN;
            state.bg_fill = theme::BG_SUNKEN;
            state.expansion = 0.0;
        }
        widgets.inactive.bg_stroke = Stroke::new(1.0, theme::LINE_STRONG);
        let drag = egui::DragValue::new(value)
            .range(range)
            .suffix(suffix)
            .speed(0.5);
        // 整数は小数点を出さない。小数(ブラシサイズ等)も整数値なら「4 px」。
        let drag = if N::INTEGRAL {
            drag.fixed_decimals(0)
        } else {
            drag.custom_formatter(|v, _| {
                if v.fract().abs() < 1e-6 {
                    format!("{v:.0}")
                } else {
                    format!("{v:.1}")
                }
            })
        };
        ui.add_sized([width, 22.0], drag)
    })
    .inner
}

/// 「ラベル + スライダー + 数値欄」の 1 項目(整数)。値が変わったら `true`。
pub fn int_slider_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut i32,
    range: std::ops::RangeInclusive<i32>,
    suffix: &str,
    track_width: f32,
    enabled: bool,
) -> bool {
    let (min, max) = (*range.start(), *range.end());
    let mut changed = false;
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            field_label(ui, label);
            let span = (max - min).max(1) as f32;
            let mut t = (*value - min) as f32 / span;
            if track(ui, &mut t, track_width, enabled).changed() {
                *value = (min as f32 + t * span).round() as i32;
                changed = true;
            }
            let before = *value;
            number_field(ui, value, min..=max, suffix, 46.0);
            *value = (*value).clamp(min, max);
            changed |= *value != before;
        });
    });
    changed
}

/// 「ラベル + スライダー + 数値欄」の 1 項目(小数。ブラシサイズ等)。
pub fn float_slider_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    track_width: f32,
) -> bool {
    let (min, max) = (*range.start(), *range.end());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        field_label(ui, label);
        let span = (max - min).max(f32::EPSILON);
        let mut t = (*value - min) / span;
        if track(ui, &mut t, track_width, true).changed() {
            *value = (min + t * span).round().clamp(min, max);
            changed = true;
        }
        let before = *value;
        number_field(ui, value, min..=max, suffix, 52.0);
        *value = value.clamp(min, max);
        changed |= (*value - before).abs() > f32::EPSILON;
    });
    changed
}

/// 角丸のトグル(「鉛筆モード」「縦書き」など)。押されたら値を反転して
/// `true` を返す。ON はアクセントの淡色面+チェック。
pub fn toggle_chip(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT,
    );
    let size = vec2(galley.size().x + 36.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
    });
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let on = *value;
        let enabled = ui.is_enabled();
        let (fill, border, text) = if !enabled {
            (
                Color32::TRANSPARENT,
                theme::LINE_STRONG,
                theme::TEXT_DISABLED,
            )
        } else if on {
            (theme::ACCENT_SOFT, theme::ACCENT_LINE, theme::TEXT)
        } else if response.hovered() {
            (theme::BG_HOVER, theme::LINE_STRONG, Color32::WHITE)
        } else {
            (Color32::TRANSPARENT, theme::LINE_STRONG, theme::TEXT_DIM)
        };
        painter.rect_filled(rect, 12.0, fill);
        painter.rect_stroke(rect, 12.0, Stroke::new(1.0, border), StrokeKind::Inside);
        let box_rect =
            Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), Vec2::splat(10.0));
        if on && enabled {
            painter.rect_filled(box_rect, 2.5, theme::ACCENT);
            let s = Stroke::new(1.6, theme::ON_ACCENT);
            painter.line_segment(
                [
                    box_rect.lerp_inside(vec2(0.2, 0.55)),
                    box_rect.lerp_inside(vec2(0.42, 0.78)),
                ],
                s,
            );
            painter.line_segment(
                [
                    box_rect.lerp_inside(vec2(0.42, 0.78)),
                    box_rect.lerp_inside(vec2(0.82, 0.25)),
                ],
                s,
            );
        } else {
            painter.rect_stroke(
                box_rect,
                2.5,
                Stroke::new(
                    1.4,
                    if enabled {
                        theme::TEXT_FAINT
                    } else {
                        theme::TEXT_DISABLED
                    },
                ),
                StrokeKind::Inside,
            );
        }
        painter.galley(
            pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
            galley,
            text,
        );
    }
    response
}

/// 択一の切り替え(線形/円形、枠線/塗り/両方 など)。選ばれた項目を
/// `value` に書き込み、変わったら `true`。
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let font = egui::FontId::proportional(12.0);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), theme::TEXT)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 20.0).collect();
    let total = widths.iter().sum::<f32>() + 4.0;
    let (outer, _) = ui.allocate_exact_size(vec2(total, 24.0), Sense::hover());
    if ui.is_rect_visible(outer) {
        ui.painter().rect_filled(outer, 7.0, theme::BG_SUNKEN);
        ui.painter().rect_stroke(
            outer,
            7.0,
            Stroke::new(1.0, theme::LINE_STRONG),
            StrokeKind::Inside,
        );
    }
    let mut x = outer.left() + 2.0;
    for (((option, label), galley), w) in options.iter().zip(galleys).zip(widths) {
        let rect = Rect::from_min_size(pos2(x, outer.top() + 2.0), vec2(w, outer.height() - 4.0));
        x += w;
        let id = ui.id().with(("darask_segment", *label));
        let response = ui.interact(rect, id, Sense::click());
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, *label)
        });
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
        if ui.is_rect_visible(rect) {
            let text = if selected {
                ui.painter().rect_filled(rect, 5.0, theme::BG_ACTIVE);
                Color32::WHITE
            } else if response.hovered() {
                ui.painter().rect_filled(rect, 5.0, theme::BG_WIDGET);
                theme::TEXT
            } else {
                theme::TEXT_DIM
            };
            ui.painter()
                .galley(rect.center() - galley.size() / 2.0, galley, text);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: Vec<egui::Event>, t: &mut f32) -> (Rect, bool) {
        let ctx = egui::Context::default();
        let mut out = (Rect::NOTHING, false);
        let input = |events| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 100.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input(Vec::new()), |ui| {
            out.0 = track(ui, t, 200.0, true).rect;
        });
        let _ = ctx.run_ui(input(events), |ui| {
            let r = track(ui, t, 200.0, true);
            out = (r.rect, r.changed());
        });
        out
    }

    #[test]
    fn clicking_the_track_sets_the_value_from_the_pointer_x() {
        let mut t = 0.0;
        let ctx = egui::Context::default();
        let input = |events| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 100.0))),
            events,
            ..Default::default()
        };
        let mut rect = Rect::NOTHING;
        let _ = ctx.run_ui(input(Vec::new()), |ui| {
            rect = track(ui, &mut t, 200.0, true).rect;
        });
        let inner = rect.shrink2(vec2(KNOB / 2.0, 0.0));
        let target = pos2(
            egui::lerp(inner.left()..=inner.right(), 0.75),
            rect.center().y,
        );
        let press = |pressed| egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut changed = false;
        for events in [
            vec![egui::Event::PointerMoved(target)],
            vec![press(true)],
            vec![press(false)],
        ] {
            let _ = ctx.run_ui(input(events), |ui| {
                changed |= track(ui, &mut t, 200.0, true).changed();
            });
        }
        assert!(changed);
        assert!((t - 0.75).abs() < 0.02, "t = {t}");
    }

    #[test]
    fn an_idle_track_reports_no_change() {
        let mut t = 0.4;
        let (_, changed) = run(Vec::new(), &mut t);
        assert!(!changed);
        assert_eq!(t, 0.4);
    }
}
