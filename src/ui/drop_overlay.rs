//! ファイルをウィンドウ上へドラッグしている間の案内(SPEC §8 / §60)。
//!
//! ドロップしたときに何が起きるか(画像 = 新規レイヤー、`.dpaint` = 新規タブ、
//! フォルダ = ページ集)を、離す前にキャンバス上へ表示する。判定は
//! `app.rs::handle_dropped_files` と同じ規則(`classify`)で、表示は
//! `HoveredFile` が届いている間だけ(ホバーの開始・終了はウィンドウイベント
//! なので、ここから再描画を要求する必要はない — アイドル CPU 0%)。

use std::path::Path;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Stroke, StrokeKind};

use crate::io::{self, SaveFormat};
use crate::ui::{icons, theme};

/// ドロップしたときの処理の種類(`handle_dropped_files` の分岐と 1:1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Layer,
    Tab,
    Pages,
    Unsupported,
}

pub fn classify(path: &Path) -> DropKind {
    if path.is_dir() {
        DropKind::Pages
    } else if matches!(io::format_for_path(path), Some(SaveFormat::Project)) {
        DropKind::Tab
    } else if io::is_raster_image_path(path) {
        DropKind::Layer
    } else {
        DropKind::Unsupported
    }
}

/// 案内の見出しと補足。パスが分からない(`HoveredFile::path` が `None`)
/// 項目しか無いときは `None`(何も表示しない)。`blocked` はモーダル表示中
/// (ドロップしても受け付けない)。
pub fn hint(paths: &[&Path], blocked: bool) -> Option<(String, String)> {
    let first = paths.first()?;
    if blocked {
        return Some((
            "ダイアログを閉じてからドロップしてください".to_owned(),
            "開いているダイアログがあります".to_owned(),
        ));
    }
    let kinds: Vec<DropKind> = paths.iter().map(|p| classify(p)).collect();
    let name = first
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let all = |kind| kinds.iter().all(|k| *k == kind);
    let count = paths.len();
    let detail_for = |single: &str, many: &str| {
        if count == 1 {
            format!("{name} · {single}")
        } else {
            format!("{count} 個のファイル · {many}")
        }
    };
    Some(if all(DropKind::Layer) {
        (
            "ドロップして新規レイヤーとして追加".to_owned(),
            detail_for("キャンバス中央に配置", "それぞれ新規レイヤーに"),
        )
    } else if all(DropKind::Tab) {
        (
            "ドロップして新規タブで開く".to_owned(),
            detail_for("Darask Paint プロジェクト", "それぞれ新規タブに"),
        )
    } else if all(DropKind::Pages) {
        (
            "ドロップしてページ集として開く".to_owned(),
            detail_for("フォルダ内の画像をページとして", "フォルダごとにページ集"),
        )
    } else if all(DropKind::Unsupported) {
        (
            "対応していないファイルです".to_owned(),
            "PNG / JPEG / BMP / GIF / WebP / .dpaint / フォルダ".to_owned(),
        )
    } else {
        (
            "ドロップして開く".to_owned(),
            format!("{count} 個のファイル · 種類ごとにレイヤー / タブ / ページ集"),
        )
    })
}

/// キャンバス領域 `area` に案内を重ねて描く。
pub fn paint(ui: &egui::Ui, area: Rect, title: &str, detail: &str, blocked: bool) {
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_black_alpha(55));
    let frame = area.shrink(16.0);
    let accent = if blocked {
        theme::TEXT_FAINT
    } else {
        theme::ACCENT
    };
    painter.rect_stroke(frame, 14.0, Stroke::new(2.0, accent), StrokeKind::Inside);

    let title_galley = painter.layout_no_wrap(
        title.to_owned(),
        egui::FontId::proportional(14.0),
        theme::TEXT,
    );
    let detail_galley = painter.layout_no_wrap(
        detail.to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT_DIM,
    );
    let text_w = title_galley.size().x.max(detail_galley.size().x);
    let card_size = vec2(14.0 + 40.0 + 14.0 + text_w + 18.0, 64.0);
    let card = Rect::from_center_size(
        pos2(frame.center().x, frame.bottom() - 32.0 - card_size.y / 2.0),
        card_size,
    );
    painter.rect_filled(
        card.translate(vec2(0.0, 4.0)).expand(2.0),
        14.0,
        Color32::from_black_alpha(45),
    );
    painter.rect_filled(card, 12.0, Color32::from_rgb(0x25, 0x25, 0x29));
    painter.rect_stroke(
        card,
        12.0,
        Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3a, 0x40)),
        StrokeKind::Inside,
    );
    let icon_box = Rect::from_min_size(
        pos2(card.left() + 14.0, card.center().y - 20.0),
        vec2(40.0, 40.0),
    );
    painter.rect_filled(
        icon_box,
        10.0,
        if blocked {
            theme::BG_WIDGET
        } else {
            theme::ACCENT_SOFT
        },
    );
    icons::paint_paste_as_layer_icon(&painter, icon_box.shrink(9.0), accent);
    let text_x = icon_box.right() + 14.0;
    let title_h = title_galley.size().y;
    let total_h = title_h + 2.0 + detail_galley.size().y;
    let top = card.center().y - total_h / 2.0;
    painter.galley(pos2(text_x, top), title_galley, theme::TEXT);
    painter.galley(
        pos2(text_x, top + title_h + 2.0),
        detail_galley,
        theme::TEXT_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_mirrors_the_drop_handler() {
        assert_eq!(classify(Path::new("a/photo.PNG")), DropKind::Layer);
        assert_eq!(classify(Path::new("x.webp")), DropKind::Layer);
        assert_eq!(classify(Path::new("work.dpaint")), DropKind::Tab);
        assert_eq!(classify(Path::new("notes.txt")), DropKind::Unsupported);
        let dir = std::env::temp_dir();
        assert_eq!(classify(&dir), DropKind::Pages);
    }

    #[test]
    fn hint_describes_single_and_multiple_images() {
        let one = [Path::new("/tmp/写真.png")];
        let (title, detail) = hint(&one, false).expect("hint");
        assert_eq!(title, "ドロップして新規レイヤーとして追加");
        assert!(detail.starts_with("写真.png"), "{detail}");

        let two = [Path::new("a.png"), Path::new("b.jpg")];
        let (_, detail) = hint(&two, false).expect("hint");
        assert!(detail.starts_with("2 個のファイル"), "{detail}");

        let mixed = [Path::new("a.png"), Path::new("b.dpaint")];
        assert_eq!(hint(&mixed, false).expect("hint").0, "ドロップして開く");
    }

    #[test]
    fn hint_is_absent_without_paths_and_warns_while_a_modal_is_open() {
        assert!(hint(&[], false).is_none());
        let one = [Path::new("a.png")];
        let (title, _) = hint(&one, true).expect("hint");
        assert!(title.contains("ダイアログを閉じて"));
    }
}
