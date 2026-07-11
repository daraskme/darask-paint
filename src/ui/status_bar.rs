//! ステータスバー(SPEC §3: 座標 | 画像サイズ | 選択サイズ | ズーム% | トースト)。
//!
//! M2 で画像サイズ・カーソル座標・ズーム%を実データ化した。M4 で選択サイズ
//! (選択または浮動片があるときの幅×高さ)とトースト(SPEC §8: I/O エラー等を
//! 約 4 秒表示)を実配線した。

use eframe::egui;

use crate::document::Document;

/// `cursor_img` はキャンバス上のカーソル位置(画像ピクセル座標)。
/// キャンバス外なら `None`。`zoom` は 1.0 = 100%。`selection_size` は
/// 選択または浮動片があるときの `(幅, 高さ)`。`toast` は表示中のトースト文言。
pub fn show(
    ui: &mut egui::Ui,
    doc: &Document,
    cursor_img: Option<egui::Pos2>,
    zoom: f32,
    selection_size: Option<(u32, u32)>,
    toast: Option<&str>,
) {
    egui::Panel::bottom("status_bar")
        .exact_size(24.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let coord_text = match cursor_img {
                    Some(p) => format!("座標: {}, {}", p.x.floor() as i64, p.y.floor() as i64),
                    None => "座標: -".to_owned(),
                };
                ui.label(coord_text);
                ui.separator();
                ui.label(format!("画像サイズ: {}×{}", doc.width, doc.height));
                ui.separator();
                let selection_text = match selection_size {
                    Some((w, h)) => format!("選択サイズ: {w}×{h}"),
                    None => "選択サイズ: -".to_owned(),
                };
                ui.label(selection_text);
                ui.separator();
                ui.label(format!("ズーム: {:.0}%", zoom * 100.0));
                if let Some(text) = toast {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), text);
                }
            });
        });
}
