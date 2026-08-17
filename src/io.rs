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

use crate::document::{Document, MAX_DIMENSION};

/// v8 レビュー修正: 「開く」/クリップボード貼り付けの寸法上限エラー
/// (`MAX_DIMENSION`、SPEC §7/§36 と同じ 8192)。上限なしに巨大画像を
/// 読み込むと、レイヤー+合成バッファの確保だけで OOM 中断(release は
/// `panic = "abort"`)に至りうる(CLAUDE.md 鉄則: I/O 経路でパニックしない)。
fn dimension_error(width: u32, height: u32) -> String {
    format!("画像が大きすぎます({width}×{height}。対応上限 {MAX_DIMENSION}×{MAX_DIMENSION})")
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("画像サイズが不正です".to_owned());
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(dimension_error(width, height));
    }
    Ok(())
}

/// 保存フォーマット(SPEC §8)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    Project,
    Png,
    Jpeg { quality: u8 },
    Bmp,
}

/// 拡張子からフォーマットを推定する。対応拡張子は SPEC §8 の
/// 「PNG/JPEG/BMP」。不明な拡張子は `None`(呼び出し側が既定の PNG として
/// 拡張子を補う、`ensure_png_extension` 参照)。JPEG の品質は呼び出し側が
/// 別途 UI から与えるため、ここでは常に `quality: 90`(デフォルト値、
/// SPEC §8)を仮で入れておく。
/// ドラッグ&ドロップで「新規レイヤーとして追加」できる画像拡張子
/// (SPEC §8。GIF/WebP は読み込み専用なので `format_for_path` には含めない)。
pub fn is_raster_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp"
            )
        })
}

pub fn format_for_path(path: &Path) -> Option<SaveFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "dpaint" => Some(SaveFormat::Project),
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
    // v8 レビュー修正: 本体をデコードする前にヘッダだけ読んで寸法を検査し、
    // 上限超過のファイルは画素バッファを 1 バイトも確保せずに拒否する。
    // v8 R2: 検査とデコードを**同じファイルハンドル**で行う(検査後に開き
    // 直すと、その間の置換で上限検査をすり抜ける競合窓ができる)。
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = image::ImageDecoder::dimensions(&decoder);
    check_dimensions(width, height)?;
    let img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    // デコード結果がヘッダと食い違う奇妙なファイルへの防御(二重検査)。
    check_dimensions(width, height)?;
    Ok(Document::from_loaded(
        width,
        height,
        rgba.into_raw(),
        path.to_path_buf(),
    ))
}

/// `doc` を `path` に `format` で保存する(SPEC §8)。SPEC §13: 保存は常に
/// 可視レイヤーの合成(統合)結果を書き出す。未反映の dirty 領域だけを
/// 合成し、既に最新の `composite` は再計算しない。
pub fn save_image(doc: &mut Document, path: &Path, format: SaveFormat) -> Result<(), String> {
    doc.recompose_if_dirty();
    match format {
        SaveFormat::Project => {
            Err("プロジェクト形式は履歴を含む保存APIを使用してください".to_owned())
        }
        SaveFormat::Png => save_rgba(doc, path, image::ImageFormat::Png),
        SaveFormat::Bmp => save_bmp(doc, path),
        SaveFormat::Jpeg { quality } => save_jpeg(doc, path, quality),
    }
}

/// v8 レビュー修正: 画像書き出しも `.dpaint` と同じ「一時ファイル→単一
/// rename」で置換する。従来は保存先へ直接書いており(JPEG は `File::create`
/// が既存ファイルを先に切り詰める)、エンコードやディスク書込が途中で失敗
/// すると**既存の元ファイルが空・不完全なまま残る**危険があった(SPEC §8 の
/// トーストでは原本を回復できない)。エンコーダは一時ファイルへ直接
/// ストリームする(`project::atomic_write_with` — 最大 8192² でも完成バイト
/// 列をメモリへ持たない)。
fn save_rgba(doc: &Document, path: &Path, format: image::ImageFormat) -> Result<(), String> {
    crate::project::atomic_write_with(path, |writer| {
        image::write_buffer_with_format(
            writer,
            &doc.composite,
            doc.width,
            doc.height,
            image::ColorType::Rgba8,
            format,
        )
        .map_err(|e| e.to_string())
    })
}

/// SPEC §13: 「JPEG/BMP は白に合成」。アルファチャンネルを持てない形式向けに
/// straight-alpha の合成結果を白背景へ source-over 合成した RGB バッファを作る
/// (`save_jpeg`/`save_bmp` で共有)。
fn composite_over_white_rgb(doc: &Document) -> Result<Vec<u8>, String> {
    // v8 レビュー修正: 8192² では約 192MiB になるバッファなので、失敗を
    // トーストへ返せる fallible な確保にする(CLAUDE.md 鉄則: I/O 経路で
    // パニックしない)。
    let len = (doc.width as usize)
        .checked_mul(doc.height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| "画像が大きすぎます".to_owned())?;
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(len)
        .map_err(|_| "保存用メモリを確保できません".to_owned())?;
    rgb.resize(len, 0);
    for (src, dst) in doc.composite.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
        let a = src[3] as f32 / 255.0;
        for c in 0..3 {
            let v = src[c] as f32 * a + 255.0 * (1.0 - a);
            dst[c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(rgb)
}

/// JPEG はアルファチャンネルを持てないため、白背景に source-over 合成してから
/// 保存する(SPEC §13: 「JPEG 保存時は…アルファは白に合成してから保存」)。
fn save_jpeg(doc: &Document, path: &Path, quality: u8) -> Result<(), String> {
    let rgb = composite_over_white_rgb(doc)?;
    // v8 レビュー修正: `File::create`(既存ファイルの即時切り詰め)をやめ、
    // 一時ファイルへ直接エンコードしてから原子的に設置する(`save_rgba` と
    // 同じ理由)。
    crate::project::atomic_write_with(path, |writer| {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut *writer, quality);
        encoder
            .write_image(&rgb, doc.width, doc.height, image::ExtendedColorType::Rgb8)
            .map_err(|e| e.to_string())
    })
}

/// v2 で SPEC §13 に追加された規則(「JPEG/BMP は白に合成」)。
///
/// v2 レビューで発見・修正したバグ: 以前は `save_rgba` に流していたため
/// RGBA のまま(32bpp)書き出しており、アルファ非対応のビューア/アプリでは
/// 透明部が黒や不定色に見えていた。JPEG と同じ白合成を経てから、アルファ
/// チャンネルを持たない RGB8(24bpp)として書き出す。
fn save_bmp(doc: &Document, path: &Path) -> Result<(), String> {
    let rgb = composite_over_white_rgb(doc)?;
    // v8 レビュー修正: `save_rgba` と同じく一時ファイルへ直接エンコード→
    // 原子的設置。
    crate::project::atomic_write_with(path, |writer| {
        image::write_buffer_with_format(
            writer,
            &rgb,
            doc.width,
            doc.height,
            image::ColorType::Rgb8,
            image::ImageFormat::Bmp,
        )
        .map_err(|e| e.to_string())
    })
}

/// 「開く」ダイアログ(SPEC §8、フィルタ必須、ARCHITECTURE.md §8)。
/// ユーザーがキャンセルしたら `None`。
pub fn open_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("画像を開く")
        .add_filter("Darask Paint プロジェクト", &["dpaint"])
        .add_filter(
            "画像ファイル",
            &["png", "jpg", "jpeg", "bmp", "gif", "webp"],
        )
        .add_filter("すべてのファイル", &["*"])
        .pick_file()
}

pub fn open_pages_folder_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("フォルダをページとして開く")
        .pick_folder()
}

/// v9 §43: 「ファイルから貼り付け」ダイアログ(画像形式のみ — `.dpaint` は
/// レイヤー構成を持つため貼り付け元にしない。開きたい場合は「開く」で
/// 別タブに開く)。
pub fn paste_file_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("ファイルから貼り付け")
        .add_filter(
            "画像ファイル",
            &["png", "jpg", "jpeg", "bmp", "gif", "webp"],
        )
        .pick_file()
}

/// 「名前を付けて保存」ダイアログ(SPEC §8)。`default_name` は初期ファイル名。
pub fn save_dialog(default_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("プロジェクト保存 / 画像を書き出し")
        .set_file_name(default_name)
        .add_filter("Darask Paint プロジェクト", &["dpaint"])
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
    // v8 レビュー修正: 貼り付けも「開く」と同じ寸法上限(8192)を通す。
    // ここで拒否すればレイヤー/浮動片/履歴のバッファ群を確保せずに済む。
    let width = u32::try_from(image.width).map_err(|_| "クリップボードの画像サイズが不正です")?;
    let height = u32::try_from(image.height).map_err(|_| "クリップボードの画像サイズが不正です")?;
    check_dimensions(width, height)?;
    let bytes = image.bytes.into_owned();
    // arboard は RGBA8 を保証するが、長さの食い違いには防御的に対処する
    // (以降の経路は `len == w*h*4` を前提に組み立てるため)。
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if expected != Some(bytes.len()) {
        return Err("クリップボードの画像データが不正です".to_owned());
    }
    Ok((width, height, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Background;

    #[test]
    fn format_for_path_recognizes_known_extensions() {
        assert_eq!(
            format_for_path(Path::new("a.dpaint")),
            Some(SaveFormat::Project)
        );
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
    fn is_raster_image_path_accepts_openable_image_extensions() {
        for name in ["a.png", "a.JPG", "a.jpeg", "a.bmp", "a.gif", "a.webp"] {
            assert!(
                is_raster_image_path(Path::new(name)),
                "{name} should be a droppable raster image"
            );
        }
        assert!(!is_raster_image_path(Path::new("a.dpaint")));
        assert!(!is_raster_image_path(Path::new("a.txt")));
        assert!(!is_raster_image_path(Path::new("a")));
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
            ensure_extension(PathBuf::from("a.dpaint")),
            PathBuf::from("a.dpaint")
        );
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
        // `save_image` は dirty 領域だけを合成するため、実編集経路と同じく
        // 低レベルのテスト書き込み後に変更範囲を通知する。
        doc.mark_dirty(crate::document::IRect {
            x0: 1,
            y0: 1,
            x1: 4,
            y1: 3,
        });

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
    fn save_image_does_not_recompose_when_document_is_not_dirty() {
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_save_clean_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("clean.png");
        let mut doc = Document::new(1, 1, Background::White);
        doc.dirty.clear();
        doc.layers[0].pixels.copy_from_slice(&[255, 0, 0, 255]);
        doc.composite.copy_from_slice(&[0, 0, 255, 255]);

        save_image(&mut doc, &path, SaveFormat::Png).expect("save should succeed");
        let loaded = load_image(&path).expect("load should succeed");
        assert_eq!(loaded.get_pixel(0, 0), Some([0, 0, 255, 255]));

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
        doc.mark_dirty(crate::document::IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        });

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
    fn load_image_rejects_dimensions_over_the_limit_before_decoding() {
        // v8 レビュー修正: 上限(8192)超の画像はヘッダ検査で拒否する
        // (`check_dimensions` のコメント参照)。9000×1 なら生成コストは
        // 36KB 程度で、テストとして安全に作れる。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_too_big_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("too_wide.png");
        let pixels = vec![0u8; 9000 * 4];
        image::save_buffer_with_format(
            &path,
            &pixels,
            9000,
            1,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .expect("seed oversized png");

        let Err(message) = load_image(&path) else {
            panic!("oversized image must be rejected");
        };
        assert!(
            message.contains("大きすぎます"),
            "上限超過の明確なエラーメッセージを返す: {message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_image_atomically_replaces_an_existing_file() {
        // v8 レビュー修正: 画像書き出しは一時ファイル→rename 置換
        // (`save_rgba` のコメント参照)。上書き保存後に (1) 内容が新しい
        // 画像になり、(2) 一時ファイルが残らないことを確認する。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_atomic_img_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("overwrite.png");

        let mut first = Document::new(2, 2, Background::White);
        save_image(&mut first, &path, SaveFormat::Png).expect("first save");
        let mut second = Document::new(2, 2, Background::Transparent);
        second.set_pixel(0, 0, [1, 2, 3, 255]);
        second.mark_dirty(crate::document::IRect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        });
        save_image(&mut second, &path, SaveFormat::Png).expect("overwrite save");

        let loaded = load_image(&path).expect("load overwritten file");
        assert_eq!(loaded.get_pixel(0, 0), Some([1, 2, 3, 255]));
        let stray: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "一時ファイルが残ってはならない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_image_refuses_a_directory_target_and_keeps_it() {
        // 保存先がディレクトリ(`install_temp` の防御)でもパニックせず、
        // ディレクトリを壊さない。
        let dir = std::env::temp_dir().join(format!(
            "darask_paint_test_dir_target_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = dir.join("i_am_a_directory.png");
        std::fs::create_dir_all(&target).expect("create dir target");
        let mut doc = Document::new(2, 2, Background::White);
        assert!(save_image(&mut doc, &target, SaveFormat::Png).is_err());
        assert!(target.is_dir(), "既存ディレクトリは壊れない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_zero_size_is_error() {
        let result = copy_image_to_clipboard(0, 0, &[]);
        assert!(result.is_err());
    }
}
