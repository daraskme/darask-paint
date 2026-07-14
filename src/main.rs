//! Darask Paint エントリポイント。
//!
//! 起動速度が最優先(SPEC §0: 300ms 以内)のため、ここでの処理は最小限に
//! 留める。実際の状態・レイアウトはすべて `app::DaraskApp` が持つ。
#![windows_subsystem = "windows"]

mod app;
mod canvas_view;
mod document;
mod history;
mod icon;
mod io;
mod keymap;
mod project;
mod raster;
mod settings;
mod text;
mod tools;
mod ui;

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

fn main() -> eframe::Result {
    // SPEC §11: ベンチマークモードは main() 冒頭で計測を開始する。
    let process_start = Instant::now();
    let bench_mode = std::env::var_os("DARASK_BENCH").is_some();

    // v4 §16.2(ARCHITECTURE.md): 日本語フォント読み込み(~10MB のファイル
    // I/O)を、これから始まる eframe のウィンドウ/GL コンテキスト作成と
    // 並行させるため、ここで別スレッドを起こしておく。`DaraskApp::new`
    // (`run_native` のアプリ生成クロージャの中、ウィンドウ作成が終わった
    // 後に呼ばれる)がこの `JoinHandle` を join する — ウィンドウ作成に
    // かかる時間ぶん、実質的な待ち時間が短縮される(効果は起動フェーズの
    // 計測値で確認する、SPEC §28)。`text::load_font_bytes` はパニックしない
    // 純粋な I/O 関数なので、スレッド境界を挟んでも安全側に倒せる
    // (`DaraskApp::new` 側は `JoinHandle::join()` の `Err`(パニック)も
    // `unwrap()` せず安全に処理する)。
    let font_handle = std::thread::spawn(text::load_font_bytes);

    // v4 §26(ARCHITECTURE.md §16.7): 設定の読み込みは起動時 1 回、ここで
    // 行う(ウィンドウ初期寸法・最大化状態に必要なため、`DaraskApp::new`
    // より前でなければならない)。読み込み自体は同期 I/O だが、対象は数百
    // バイト程度の小さなテキストファイル 1 つなので、フォント読込ほどの
    // 並行化の効果は見込めない(実測は §28 のベンチ内訳「settings」フェーズ
    // で確認する)。破損・欠損時は `settings::load` 内部で黙って既定値に
    // フォールバックする(SPEC §26、パニックしない)。
    let settings = settings::load();
    let settings_loaded_ms = bench_mode.then(|| process_start.elapsed().as_millis());

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

    // SPEC §3: 最小 640×480。初期寸法・最大化状態は v4 §26 で設定ファイルから
    // 復元する(既定値は従来どおり 1280×800、`settings::Settings::default`)。
    // 起動直後は無題の新規文書(CLI 引数が無ければ)。
    // v4 §29(ARCHITECTURE.md §16.8): exe への埋め込み(build.rs +
    // winresource)と同じ絵を、ウィンドウ/タスクバーアイコンにも使う
    // (`icon::generate_icon_rgba` を共有)。サイズは 4 の倍数を推奨する
    // egui の `IconData` の注記に従い 64px(正方形)を使う。
    const ICON_SIZE: u32 = 64;
    let icon_rgba = icon::generate_icon_rgba(ICON_SIZE);

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([settings.window_width as f32, settings.window_height as f32])
        .with_min_inner_size([
            settings::MIN_WINDOW_WIDTH as f32,
            settings::MIN_WINDOW_HEIGHT as f32,
        ])
        .with_maximized(settings.window_maximized)
        .with_title("無題 - Darask Paint")
        .with_icon(egui::IconData {
            rgba: icon_rgba,
            width: ICON_SIZE,
            height: ICON_SIZE,
        });

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
                font_handle,
                settings,
                settings_loaded_ms,
            )))
        }),
    )
}
