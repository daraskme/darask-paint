//! ページ管理パネル(SPEC §54)。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use eframe::egui;

use crate::pages::PageSet;

const THUMBNAIL_SIZE: u32 = 48;
pub const THUMBNAIL_QUEUE_CAPACITY: usize = 8;

struct ThumbnailJob {
    path: PathBuf,
    repaint: egui::Context,
}

struct ThumbnailResult {
    path: PathBuf,
    image: Result<egui::ColorImage, String>,
}

enum CachedThumbnail {
    Ready(egui::TextureHandle),
    Failed,
}

pub struct PageThumbnailCache {
    sender: SyncSender<ThumbnailJob>,
    receiver: Receiver<ThumbnailResult>,
    pending: HashSet<PathBuf>,
    cached: HashMap<PathBuf, CachedThumbnail>,
}

impl Default for PageThumbnailCache {
    fn default() -> Self {
        let (sender, jobs) = mpsc::sync_channel::<ThumbnailJob>(THUMBNAIL_QUEUE_CAPACITY);
        let (results, receiver) = mpsc::channel::<ThumbnailResult>();
        let _ = thread::Builder::new()
            .name("page-thumbnail".to_owned())
            .spawn(move || thumbnail_worker(jobs, results));
        Self {
            sender,
            receiver,
            pending: HashSet::new(),
            cached: HashMap::new(),
        }
    }
}

impl PageThumbnailCache {
    fn collect(&mut self, ctx: &egui::Context) -> Vec<String> {
        let mut errors = Vec::new();
        while let Ok(result) = self.receiver.try_recv() {
            self.pending.remove(&result.path);
            match result.image {
                Ok(image) => {
                    let texture = ctx.load_texture(
                        format!("page-thumbnail:{:?}", result.path),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.cached
                        .insert(result.path, CachedThumbnail::Ready(texture));
                }
                Err(error) => {
                    self.cached
                        .insert(result.path.clone(), CachedThumbnail::Failed);
                    errors.push(format!("サムネイルを読み込めませんでした: {error}"));
                }
            }
        }
        errors
    }

    fn request(&mut self, path: &Path, ctx: &egui::Context) {
        if is_project(path) || self.cached.contains_key(path) || self.pending.contains(path) {
            return;
        }
        let job = ThumbnailJob {
            path: path.to_path_buf(),
            repaint: ctx.clone(),
        };
        match self.sender.try_send(job) {
            Ok(()) => {
                self.pending.insert(path.to_path_buf());
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn get(&self, path: &Path) -> Option<&CachedThumbnail> {
        self.cached.get(path)
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
        Some(CachedThumbnail::Ready(texture)) => {
            ui.add(egui::Image::new(texture).fit_to_exact_size(size));
        }
        Some(CachedThumbnail::Failed) | None => {
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
            image: decode_thumbnail(&job.path),
        };
        if results.send(result).is_err() {
            break;
        }
        job.repaint.request_repaint();
    }
}

fn decode_thumbnail(path: &Path) -> Result<egui::ColorImage, String> {
    let image = image::open(path).map_err(|error| error.to_string())?;
    let image = image.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        image.as_raw(),
    ))
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
}
