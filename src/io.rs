//! ファイル I/O・クリップボード(ARCHITECTURE.md §8, SPEC §8)。
//!
//! - 読み込み: `image` crate で開き `to_rgba8()` に正規化する(PNG/JPEG/BMP/
//!   GIF 先頭フレーム/WebP、SPEC §8)。
//! - 保存: 拡張子で PNG/JPEG/BMP を判定(不明なら `.png` を付ける)。JPEG は
//!   アルファを白に合成してから `JpegEncoder::new_with_quality` で書く。
//! - ダイアログ: `rfd::FileDialog`(ブロッキング、フィルタ必須)。
//! - クリップボード: `arboard::Clipboard`(`get_image`/`set_image`、RGBA)。
//!
//! この 3 つはいずれも失敗しうる(壊れたファイル、権限、クリップボード未対応
//! 形式など)。CLAUDE.md 鉄則「I/O・ユーザー入力経路で unwrap() しない」を
//! 守るため、すべて `Result<_, String>` を返し、呼び出し側(app.rs)がトースト
//! で通知する。

use std::path::{Path, PathBuf};

use image::ImageEncoder;

use crate::document::Document;

/// 保存フォーマット(SPEC §8)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    Png,
    Jpeg { quality: u8 },
    Bmp,
}

/// 拡張子からフォーマットを推定する。対応拡張子は SPEC §8 の
/// 「PNG/JPEG/BMP」。不明な拡張子は `None`(呼び出し側が既定の PNG として
/// 拡張子を補う、`ensure_png_extension` 参照)。JPEG の品質は呼び出し側が
/// 別途 UI から与えるため、ここでは常に `quality: 90`(デフォルト値、
/// SPEC §8)を仮で入れておく。
pub fn format_for_path(path: &Path) -> Option<SaveFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(SaveFormat::Png),
        "jpg" | "jpeg" => Some(SaveFormat::Jpeg { quality: 90 }),
        "bmp" => Some(SaveFormat::Bmp),
        _ => None,
    }
}

/// 拡張子が無い、または対応外の拡張子なら `.png` を付ける
/// (SPEC §8: 「拡張子で判定、不明な拡張子なら .png を付ける」)。
pub fn ensure_extension(path: PathBuf) -> PathBuf {
    if format_for_path(&path).is_some() {
        return path;
    }
    let mut with_ext = path.clone();
    match path.extension() {
        Some(_) => {
            // 対応外の拡張子(例: .txt)が既についている場合は、それを
            // 上書きせず末尾に png を足す形にすると `foo.txt.png` のような
            // 見た目になり紛らわしいため、拡張子ごと png に置き換える。
            with_ext.set_extension("png");
        }
        None => {
            with_ext.set_extension("png");
        }
    }
    with_ext
}

/// 画像ファイルを読み込み、内部表現(RGBA8)の `Document` にする(SPEC §8)。
/// GIF は先頭フレームのみ(`image::open` の `DynamicImage` は単一フレーム)。
/// SPEC §13: 開いた直後は「背景」レイヤー 1 枚。
pub fn load_image(path: &Path) -> Result<Document, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Document::from_loaded(
        width,
        height,
        rgba.into_raw(),
        path.to_path_buf(),
    ))
}

/// `doc` を `path` に `format` で保存する(SPEC §8)。SPEC §13: 保存は常に
/// 可視レイヤーの合成(統合)結果を書き出すため、保存前に `composite` を
/// 全再計算する。
pub fn save_image(doc: &mut Document, path: &Path, format: SaveFormat) -> Result<(), String> {
    doc.recomposite_full();
    match format {
        SaveFormat::Png => save_rgba(doc, path, image::ImageFormat::Png),
        SaveFormat::Bmp => save_bmp(doc, path),
        SaveFormat::Jpeg { quality } => save_jpeg(doc, path, quality),
    }
}

fn save_rgba(doc: &Document, path: &Path, format: image::ImageFormat) -> Result<(), String> {
    image::save_buffer_with_format(
        path,
        &doc.composite,
        doc.width,
        doc.height,
        image::ColorType::Rgba8,
        format,
    )
    .map_err(|e| e.to_string())
}

/// SPEC §13: 「JPEG/BMP は白に合成」。アルファチャンネルを持てない形式向けに
/// straight-alpha の合成結果を白背景へ source-over 合成した RGB バッファを作る
/// (`save_jpeg`/`save_bmp` で共有)。
fn composite_over_white_rgb(doc: &Document) -> Vec<u8> {
    let mut rgb = vec![0u8; doc.width as usize * doc.height as usize * 3];
    for (src, dst) in doc.composite.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
        let a = src[3] as f32 / 255.0;
        for c in 0..3 {
            let v = src[c] as f32 * a + 255.0 * (1.0 - a);
            dst[c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    rgb
}

/// JPEG はアルファチャンネルを持てないため、白背景に source-over 合成してから
/// 保存する(SPEC §13: 「JPEG 保存時は…アルファは白に合成してから保存」)。
fn save_jpeg(doc: &Document, path: &Path, quality: u8) -> Result<(), String> {
    let rgb = composite_over_white_rgb(doc);
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let writer = std::io::BufWriter::new(file);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(writer, quality);
    encoder
        .write_image(&rgb, doc.width, doc.height, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())
}

/// v2 で SPEC §13 に追加された規則(「JPEG/BMP は白に合成」)。
///
/// v2 レビューで発見・修正したバグ: 以前は `save_rgba` に流していたため
/// RGBA のまま(32bpp)書き出しており、アルファ非対応のビューア/アプリでは
/// 透明部が黒や不定色に見えていた。JPEG と同じ白合成を経てから、アルファ
/// チャンネルを持たない RGB8(24bpp)として書き出す。
fn save_bmp(doc: &Document, path: &Path) -> Result<(), String> {
    let rgb = composite_over_white_rgb(doc);
    image::save_buffer_with_format(
        path,
        &rgb,
        doc.width,
        doc.height,
        image::ColorType::Rgb8,
        image::ImageFormat::Bmp,
    )
    .map_err(|e| e.to_string())
}

/// 「開く」ダイアログ(SPEC §8、フィルタ必須、ARCHITECTURE.md §8)。
/// ユーザーがキャンセルしたら `None`。
pub fn open_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("画像を開く")
        .add_filter(
            "画像ファイル",
            &["png", "jpg", "jpeg", "bmp", "gif", "webp"],
        )
        .add_filter("すべてのファイル", &["*"])
        .pick_file()
}

/// 「名前を付けて保存」ダイアログ(SPEC §8)。`default_name` は初期ファイル名。
pub fn save_dialog(default_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("名前を付けて保存")
        .set_file_name(default_name)
        .add_filter("PNG", &["png"])
        .add_filter("JPEG", &["jpg", "jpeg"])
        .add_filter("BMP", &["bmp"])
        .save_file()
}

/// クリップボードへ RGBA 画像をコピーする(SPEC §6: Ctrl+C/Ctrl+X)。
/// `width`/`height` が 0 の場合は arboard に渡さず早期にエラーを返す
/// (ARCHITECTURE.md §12-8: 「arboard の ImageData は所有バイト。寸法 0
/// チェック」)。
pub fn copy_image_to_clipboard(width: u32, height: u32, pixels: &[u8]) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("コピーする範囲がありません".to_owned());
    }
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let image = arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Borrowed(pixels),
    };
    clipboard.set_image(image).map_err(|e| e.to_string())
}

/// クリップボードから RGBA 画像を読み込む(SPEC §6: Ctrl+V)。
/// 寸法 0 は失敗として扱う(ARCHITECTURE.md §12-8)。
pub fn read_clipboard_image() -> Result<(u32, u32, Vec<u8>), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let image = clipboard.get_image().map_err(|e| e.to_string())?;
    if image.width == 0 || image.height == 0 {
        return Err("クリップボードの画像サイズが不正です".to_owned());
    }
    Ok((
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;

    #[test]
    fn format_for_path_recognizes_known_extensions() {
        assert_eq!(format_for_path(Path::new("a.png")), Some(SaveFormat::Png));
        assert_eq!(format_for_path(Path::new("a.PNG")), Some(SaveFormat::Png));
        assert!(matches!(
            format_for_path(Path::new("a.jpg")),
            Some(SaveFormat::Jpeg { .. })
        ));
        assert!(matches!(
            format_for_path(Path::new("a.jpeg")),
            Some(SaveFormat::Jpeg { .. })
        ));
        assert_eq!(format_for_path(Path::new("a.bmp")), Some(SaveFormat::Bmp));
    }

    #[test]
    fn format_for_path_unknown_extension_is_none() {
        assert_eq!(format_for_path(Path::new("a.txt")), None);
        assert_eq!(format_for_path(Path::new("a")), None);
    }

    #[test]
    fn ensure_extension_appends_png_when_missing() {
        assert_eq!(ensure_extension(PathBuf::from("a")), PathBuf::from("a.png"));
    }

    #[test]
    fn ensure_extension_replaces_unknown_extension_with_png() {
        assert_eq!(
            ensure_extension(PathBuf::from("a.txt")),
            PathBuf::from("a.png")
        );
    }

    #[test]
    fn ensure_extension_keeps_known_extension() {
        assert_eq!(
            ensure_extension(PathBuf::from("a.jpg")),
            PathBuf::from("a.jpg")
        );
        assert_eq!(
            ensure_extension(PathBuf::from("a.BMP")),
            PathBuf::from("a.BMP")
        );
    }

    #[test]
    fn png_save_load_round_trip() {
        // ARCHITECTURE.md §13: 「io: PNG 保存→読込ラウンドトリップ
        // (temp dir 使用)」。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("round_trip.png");

        let mut doc = Document::new(4, 3, Background::Transparent);
        doc.set_pixel(1, 1, [10, 20, 30, 200]);
        doc.set_pixel(3, 2, [255, 0, 0, 255]);

        save_image(&mut doc, &path, SaveFormat::Png).expect("save should succeed");
        let loaded = load_image(&path).expect("load should succeed");

        assert_eq!(loaded.width, doc.width);
        assert_eq!(loaded.height, doc.height);
        assert_eq!(loaded.active_pixels(), doc.composite.as_slice());
        assert_eq!(loaded.path, Some(path.clone()));
        assert!(!loaded.modified);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bmp_save_load_round_trip_opaque() {
        // BMP は一部エンコーダでアルファの扱いが異なりうるため、不透明画像で
        // 色のラウンドトリップのみ確認する。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_bmp_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("round_trip.bmp");

        let mut doc = Document::new(3, 3, Background::White);
        doc.set_pixel(0, 0, [10, 20, 30, 255]);

        save_image(&mut doc, &path, SaveFormat::Bmp).expect("save should succeed");
        let loaded = load_image(&path).expect("load should succeed");
        assert_eq!(loaded.width, 3);
        assert_eq!(loaded.height, 3);
        assert_eq!(loaded.get_pixel(0, 0), Some([10, 20, 30, 255]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bmp_save_composites_alpha_over_white() {
        // v2 レビューで発見・修正したバグ: SPEC §13 は BMP も JPEG と同様に
        // アルファを白へ合成してから保存すると定めているが、以前は RGBA の
        // まま(32bpp)書き出しており、完全に透明な画素は R=G=B=0,A=0(黒
        // 相当)になっていた。BMP は非可逆圧縮を経ないため、JPEG のテスト
        // (`jpeg_save_composites_alpha_over_white`)と違い厳密な一致を
        // 検証できる。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_bmp_alpha_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("round_trip_alpha.bmp");

        let mut doc = Document::new(2, 2, Background::Transparent);
        save_image(&mut doc, &path, SaveFormat::Bmp).expect("save should succeed");
        let loaded = load_image(&path).expect("load should succeed");
        assert_eq!(
            loaded.get_pixel(0, 0),
            Some([255, 255, 255, 255]),
            "fully transparent pixels must be composited over white, not left as RGBA"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jpeg_save_composites_alpha_over_white() {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_jpeg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("round_trip.jpg");

        // 完全に透明な画素は白になるはず。
        let mut doc = Document::new(2, 2, Background::Transparent);
        save_image(&mut doc, &path, SaveFormat::Jpeg { quality: 90 }).expect("save should succeed");
        let loaded = load_image(&path).expect("load should succeed");
        let px = loaded.get_pixel(0, 0).unwrap();
        // JPEG の非可逆圧縮を考慮し、白に近いことだけ確認する。
        assert!(px[0] > 240 && px[1] > 240 && px[2] > 240);
        assert_eq!(px[3], 255);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_error_not_panic() {
        let result = load_image(Path::new("__darask_paint_definitely_missing__.png"));
        assert!(result.is_err());
    }

    #[test]
    fn copy_zero_size_is_error() {
        let result = copy_image_to_clipboard(0, 0, &[]);
        assert!(result.is_err());
    }
}
