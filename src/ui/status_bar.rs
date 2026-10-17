//! ステータスバー(SPEC §3: 画像サイズ | 座標 | 選択サイズ | トースト | ズーム)。
//!
//! M2 で画像サイズ・カーソル座標・ズーム%を実データ化した。M4 で選択サイズ
//! (選択または浮動片があるときの幅×高さ)とトースト(SPEC §8: I/O エラー等を
//! 約 4 秒表示)を実配線した。v13 §60 で右端のズーム表示を「− 59% + 全体」の
//! 操作部にした(押下は `StatusBarAction` を返すだけ。実行は `app.rs`)。

use eframe::egui::{self, vec2};

use crate::document::Document;
use crate::keymap::{self, Action};
use crate::ui::{icons, theme, widgets};

/// ステータスバーで押された操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBarAction {
    /// v12 §53: 実行中ジョブのキャンセル。
    CancelJob,
    ZoomIn,
    ZoomOut,
    /// ズーム率の表示をクリック = 100%。
    Zoom100,
    FitWindow,
}

const HEIGHT: f32 = 28.0;

/// `cursor_img` はキャンバス上のカーソル位置(画像ピクセル座標)。
/// キャンバス外なら `None`。`zoom` は 1.0 = 100%。`selection_size` は
/// 選択または浮動片があるときの `(幅, 高さ)`。`toast` は表示中のトースト文言。
/// v12 §53: `background_job` が `Some(ラベル)` のとき文言と「キャンセル」を
/// 出す(スピナーやアニメーションは出さない — アイドル CPU 0% のため)。
pub fn show(
    ui: &mut egui::Ui,
    doc: &Document,
    cursor_img: Option<egui::Pos2>,
    zoom: f32,
    selection_size: Option<(u32, u32)>,
    toast: Option<&str>,
    background_job: Option<&str>,
) -> Option<StatusBarAction> {
    let mut action = None;
    egui::Panel::bottom("status_bar")
        .exact_size(HEIGHT)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG_APP)
                .inner_margin(egui::Margin::symmetric(12, 0)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.painter().hline(
                full.x_range().expand(12.0),
                full.top() + 0.5,
                egui::Stroke::new(1.0, theme::LINE),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                let dim =
                    |text: String| egui::RichText::new(text).size(12.0).color(theme::TEXT_DIM);
                ui.label(dim(format!("{} × {} px", doc.width, doc.height)));
                ui.label(dim(match cursor_img {
                    Some(p) => format!("X {} · Y {}", p.x.floor() as i64, p.y.floor() as i64),
                    None => "X — · Y —".to_owned(),
                }));
                ui.label(dim(match selection_size {
                    Some((w, h)) => format!("選択 {w} × {h}"),
                    None => "選択 —".to_owned(),
                }));
                if let Some(text) = toast {
                    toast_pill(ui, text);
                }
                // v12 §53: 実行中ジョブ(修復)の表示 + キャンセル。
                if let Some(label) = background_job {
                    ui.label(egui::RichText::new(label).size(12.0).color(theme::ACCENT));
                    if ui.small_button("キャンセル").clicked() {
                        action = Some(StatusBarAction::CancelJob);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if zoom_button(
                        ui,
                        icons::paint_fit_window_icon,
                        "ウィンドウに合わせる",
                        Action::FitWindow,
                    ) {
                        action = Some(StatusBarAction::FitWindow);
                    }
                    ui.add_space(4.0);
                    if zoom_button(ui, icons::paint_plus_icon, "拡大", Action::ZoomIn) {
                        action = Some(StatusBarAction::ZoomIn);
                    }
                    let percent = ui
                        .add_sized(
                            [48.0, 22.0],
                            egui::Button::new(
                                egui::RichText::new(format!("{:.0}%", zoom * 100.0))
                                    .size(12.0)
                                    .color(theme::TEXT),
                            )
                            .frame(false),
                        )
                        .on_hover_text(keymap::menu_label("100% 表示", Action::Zoom100));
                    if percent.clicked() {
                        action = Some(StatusBarAction::Zoom100);
                    }
                    if zoom_button(ui, icons::paint_minus_icon, "縮小", Action::ZoomOut) {
                        action = Some(StatusBarAction::ZoomOut);
                    }
                });
            });
        });
    action
}

fn zoom_button(
    ui: &mut egui::Ui,
    paint: widgets::IconPaint,
    label: &str,
    shortcut: Action,
) -> bool {
    widgets::icon_button(ui, vec2(22.0, 22.0), 12.0, true, false, paint, label)
        .on_hover_text(keymap::menu_label(label, shortcut))
        .clicked()
}

/// トーストは控えめな角丸の帯(アクセントの点 + 文言)で出す。
fn toast_pill(ui: &mut egui::Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT,
    );
    let size = vec2(galley.size().x + 30.0, 20.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 10.0, theme::BG_WIDGET);
        painter.circle_filled(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            3.0,
            theme::ACCENT,
        );
        painter.galley(
            egui::pos2(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
            galley,
            theme::TEXT,
        );
    }
}
