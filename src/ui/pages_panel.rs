//! ページ管理パネル(SPEC §54)。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::SystemTime;

use eframe::egui;
use image::ImageDecoder;

use crate::document::MAX_DIMENSION;
use crate::pages::PageSet;

const THUMBNAIL_SIZE: u32 = 48;
const MAX_THUMBNAIL_PIXELS: u32 = MAX_DIMENSION * MAX_DIMENSION;
const MAX_THUMBNAIL_DECODE_BYTES: u64 = (MAX_THUMBNAIL_PIXELS as u64) * 8;
pub const THUMBNAIL_QUEUE_CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileSignature {
    len: u64,
    modified: Option<SystemTime>,
}

struct ThumbnailJob {
    path: PathBuf,
    signature: Option<FileSignature>,
    version: u64,
    repaint: egui::Context,
}

struct ThumbnailResult {
    path: PathBuf,
    signature: Option<FileSignature>,
    version: u64,
    image: Result<egui::ColorImage, String>,
}

enum CachedThumbnail {
    Ready {
        texture: egui::TextureHandle,
        signature: Option<FileSignature>,
    },
    Failed {
        signature: Option<FileSignature>,
    },
}

pub struct PageThumbnailCache {
    sender: SyncSender<ThumbnailJob>,
    receiver: Receiver<ThumbnailResult>,
    pending: HashMap<PathBuf, u64>,
    cached: HashMap<PathBuf, CachedThumbnail>,
    versions: HashMap<PathBuf, u64>,
    live_paths: Option<HashSet<PathBuf>>,
    init_error: Option<String>,
}

impl Default for PageThumbnailCache {
    fn default() -> Self {
        let (sender, jobs) = mpsc::sync_channel::<ThumbnailJob>(THUMBNAIL_QUEUE_CAPACITY);
        let (results, receiver) = mpsc::channel::<ThumbnailResult>();
        let init_error = thread::Builder::new()
            .name("page-thumbnail".to_owned())
            .spawn(move || thumbnail_worker(jobs, results))
            .err()
            .map(|error| format!("サムネイルワーカーを開始できませんでした: {error}"));
        Self {
            sender,
            receiver,
            pending: HashMap::new(),
            cached: HashMap::new(),
            versions: HashMap::new(),
            live_paths: None,
            init_error,
        }
    }
}

impl PageThumbnailCache {
    fn collect(&mut self, ctx: &egui::Context) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(error) = self.init_error.take() {
            errors.push(error);
        }
        while let Ok(result) = self.receiver.try_recv() {
            if self.pending.get(&result.path) == Some(&result.version) {
                self.pending.remove(&result.path);
            }
            if self.versions.get(&result.path).copied().unwrap_or(0) != result.version {
                continue;
            }
            if self
                .live_paths
                .as_ref()
                .is_some_and(|paths| !paths.contains(&result.path))
            {
                continue;
            }
            if file_signature(&result.path) != result.signature {
                self.invalidate(&result.path);
                continue;
            }
            match result.image {
                Ok(image) => {
                    let texture = ctx.load_texture(
                        format!("page-thumbnail:{:?}", result.path),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.cached.insert(
                        result.path,
                        CachedThumbnail::Ready {
                            texture,
                            signature: result.signature,
                        },
                    );
                }
                Err(error) => {
                    self.cached.insert(
                        result.path.clone(),
                        CachedThumbnail::Failed {
                            signature: result.signature,
                        },
                    );
                    errors.push(format!("サムネイルを読み込めませんでした: {error}"));
                }
            }
        }
        errors
    }

    fn request(&mut self, path: &Path, ctx: &egui::Context) {
        if is_project(path) || self.pending.contains_key(path) {
            return;
        }
        let signature = file_signature(path);
        if self
            .cached
            .get(path)
            .is_some_and(|cached| cached.signature() == signature)
        {
            return;
        }
        self.cached.remove(path);
        let version = self.versions.get(path).copied().unwrap_or(0);
        let job = ThumbnailJob {
            path: path.to_path_buf(),
            signature,
            version,
            repaint: ctx.clone(),
        };
        match self.sender.try_send(job) {
            Ok(()) => {
                self.pending.insert(path.to_path_buf(), version);
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn get(&self, path: &Path) -> Option<&CachedThumbnail> {
        self.cached.get(path)
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.cached.remove(path);
        let version = self.versions.entry(path.to_path_buf()).or_default();
        *version = version.checked_add(1).unwrap_or(0);
    }

    pub fn prune<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let live_paths: HashSet<PathBuf> = paths.into_iter().collect();
        self.cached.retain(|path, _| live_paths.contains(path));
        self.versions
            .retain(|path, _| live_paths.contains(path) || self.pending.contains_key(path));
        self.live_paths = Some(live_paths);
    }
}

impl CachedThumbnail {
    fn signature(&self) -> Option<FileSignature> {
        match self {
            Self::Ready { signature, .. } | Self::Failed { signature } => *signature,
        }
    }
}

#[derive(Default)]
pub struct PagesPanelOutput {
    pub switch_to: Option<usize>,
    pub errors: Vec<String>,
}

pub fn show(
    ui: &mut egui::Ui,
    pages: Option<&mut PageSet>,
    thumbnails: &mut PageThumbnailCache,
) -> PagesPanelOutput {
    let mut output = PagesPanelOutput {
        errors: thumbnails.collect(ui.ctx()),
        ..PagesPanelOutput::default()
    };
    let Some(pages) = pages else {
        ui.weak("フォルダをページとして開いてください");
        return output;
    };

    ui.checkbox(&mut pages.autosave, "ページ切替時に自動保存");
    ui.weak("前/次ページ: PageUp / PageDown");
    ui.separator();
    for (index, entry) in pages.entries.iter().enumerate() {
        let rect = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 56.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(format!("{}", index + 1));
                    show_thumbnail(ui, &entry.path, thumbnails);
                    ui.label(
                        entry
                            .path
                            .file_name()
                            .unwrap_or(entry.path.as_os_str())
                            .to_string_lossy(),
                    );
                },
            )
            .response
            .rect;
        let response = ui.interact(
            rect,
            ui.id().with(("page-row", index)),
            egui::Sense::click(),
        );
        if index == pages.current {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                3.0,
                egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        if ui.is_rect_visible(rect) {
            thumbnails.request(&entry.path, ui.ctx());
        }
        if response.clicked() && index != pages.current {
            output.switch_to = Some(index);
        }
    }
    output
}

fn show_thumbnail(ui: &mut egui::Ui, path: &Path, cache: &PageThumbnailCache) {
    let size = egui::vec2(48.0, 48.0);
    if is_project(path) {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "DP",
            egui::FontId::proportional(15.0),
            ui.visuals().text_color(),
        );
        return;
    }
    match cache.get(path) {
        Some(CachedThumbnail::Ready { texture, .. }) => {
            ui.add(egui::Image::new(texture).fit_to_exact_size(size));
        }
        Some(CachedThumbnail::Failed { .. }) | None => {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "—",
                egui::FontId::proportional(18.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

fn thumbnail_worker(jobs: Receiver<ThumbnailJob>, results: mpsc::Sender<ThumbnailResult>) {
    while let Ok(job) = jobs.recv() {
        let result = ThumbnailResult {
            path: job.path.clone(),
            signature: job.signature,
            version: job.version,
            image: decode_thumbnail(&job.path),
        };
        if results.send(result).is_err() {
            break;
        }
        job.repaint.request_repaint();
    }
}

fn decode_thumbnail(path: &Path) -> Result<egui::ColorImage, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut reader = image::ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_THUMBNAIL_DECODE_BYTES);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    check_thumbnail_dimensions(width, height)?;
    let image = image::DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    let image = image.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        image.as_raw(),
    ))
}

fn check_thumbnail_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| format!("画像の画素数が大きすぎます: {width}×{height}"))?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || pixels > MAX_THUMBNAIL_PIXELS
    {
        return Err(format!(
            "画像が大きすぎます({width}×{height}。対応上限 {MAX_DIMENSION}×{MAX_DIMENSION})"
        ));
    }
    Ok(())
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileSignature {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn is_project(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dpaint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn thumbnail_queue_is_bounded() {
        let (sender, _receiver) = mpsc::sync_channel::<usize>(THUMBNAIL_QUEUE_CAPACITY);
        for value in 0..THUMBNAIL_QUEUE_CAPACITY {
            assert!(sender.try_send(value).is_ok());
        }
        assert!(matches!(sender.try_send(99), Err(TrySendError::Full(99))));
    }

    #[test]
    fn collecting_completed_results_stops_when_queue_is_empty() {
        let mut cache = PageThumbnailCache::default();
        assert!(cache.collect(&egui::Context::default()).is_empty());
        assert!(cache.collect(&egui::Context::default()).is_empty());
        assert!(cache.pending.is_empty());
    }

    #[test]
    fn worker_repaints_once_then_failed_thumbnail_is_not_retried() {
        let path =
            std::env::temp_dir().join(format!("darask_bad_thumb_{}.png", std::process::id()));
        std::fs::write(&path, b"not an image").expect("bad image should be written");
        let repaint_count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&repaint_count);
        let ctx = egui::Context::default();
        ctx.set_request_repaint_callback(move |_| {
            callback_count.fetch_add(1, Ordering::Relaxed);
        });
        let mut cache = PageThumbnailCache::default();
        cache.request(&path, &ctx);
        let deadline = Instant::now() + Duration::from_secs(2);
        while repaint_count.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(repaint_count.load(Ordering::Relaxed), 1);
        assert_eq!(cache.collect(&ctx).len(), 1);
        cache.request(&path, &ctx);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(repaint_count.load(Ordering::Relaxed), 1);
        assert!(cache.collect(&ctx).is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// SPEC §54 のサムネイルキャッシュ解放。`prune` に渡さなかったパスの
    /// 項目は捨てられ、渡したパスは残る(ページ集の張り替え・タブを閉じる
    /// 操作でメモリと GPU テクスチャが単調増加しないことの回帰テスト。
    /// 呼び出し側の配線は `app.rs::prune_page_thumbnails`)。
    #[test]
    fn prune_drops_thumbnails_that_left_every_page_set() {
        let mut cache = PageThumbnailCache::default();
        let kept = PathBuf::from("kept.png");
        let dropped = PathBuf::from("dropped.png");
        cache
            .cached
            .insert(kept.clone(), CachedThumbnail::Failed { signature: None });
        cache
            .cached
            .insert(dropped.clone(), CachedThumbnail::Failed { signature: None });

        cache.prune(vec![kept.clone()]);

        assert!(cache.get(&kept).is_some(), "生存パスの項目は残る");
        assert!(
            cache.get(&dropped).is_none(),
            "どのページ集にも属さなくなった項目は捨てる"
        );
    }
}
