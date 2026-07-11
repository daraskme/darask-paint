//! Darask Paint エントリポイント。
//!
//! 起動速度が最優先(SPEC §0: 300ms 以内)のため、ここでの処理は最小限に
//! 留める。実際の状態・レイアウトはすべて `app::DaraskApp` が持つ。
#![windows_subsystem = "windows"]

mod app;
mod canvas_view;
mod document;
mod history;
mod io;
mod raster;
mod tools;
mod ui;

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

// TEMP DIAGNOSTIC (v2 white-screen regression): eframe/egui_glow/winit report
// real errors (e.g. GL/swap-buffer/window failures) exclusively through the
// `log` facade. Since this app installs no logger, those messages are
// silently dropped by `log`'s no-op default logger, so any real failure is
// invisible even though the process stays alive. Install a trivial logger
// that writes straight to stderr so nothing is swallowed while we
// investigate. Remove this together with the `log` dependency once the root
// cause is confirmed (unless the eventual fix needs it).
struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!(
            "[DIAG log {} {}] {}",
            record.level(),
            record.target(),
            record.args()
        );
    }
    fn flush(&self) {}
}

fn main() -> eframe::Result {
    // TEMP DIAGNOSTIC: see StderrLogger above.
    log::set_logger(&StderrLogger).expect("logger install");
    log::set_max_level(log::LevelFilter::Trace);
    eprintln!("[DIAG] main() start");

    // SPEC §11: ベンチマークモードは main() 冒頭で計測を開始する。
    let process_start = Instant::now();
    let bench_mode = std::env::var_os("DARASK_BENCH").is_some();

    // SPEC §3: コマンドライン引数 `darask-paint.exe 画像.png` でそのファイルを
    // 開く(「プログラムから開く」対応)。第 1 引数(プロセス名を除く)を
    // そのままパスとして扱う。
    //
    // M4 で発見・修正したバグ: `std::env::args()` は不正な Unicode(Windows
    // では非対の UTF-16 サロゲートを含むファイル名など)を含む引数がある場合
    // panic する(SPEC §12: I/O・ユーザー入力経路でパニックしない、に違反)。
    // 「プログラムから開く」はエクスプローラーがファイルパスをそのまま
    // argv[1] に渡す経路であり、非 Unicode パスも実際に作成しうるため、
    // 損失なく `OsString` のまま扱える `args_os()` を使う。
    let cli_path: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);

    // SPEC §3: 初期 1280×800、最小 640×480。起動直後は無題の新規文書。
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([640.0, 480.0])
        .with_title("無題 - Darask Paint");

    let native_options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "Darask Paint",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::DaraskApp::new(
                cc,
                process_start,
                bench_mode,
                cli_path,
            )))
        }),
    )
}
