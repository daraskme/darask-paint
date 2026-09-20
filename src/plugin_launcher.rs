//! プラグインフォルダ(zip 配置)の探索・展開・起動(SPEC §55.3)。
//!
//! `darask-paint.exe` と同じ階層の `plugins\`(または設定 `plugin.dir` で
//! 指定したフォルダ)に置かれた IOpaint / AI Diffusion の配布 zip を、
//! AI メニュー実行時に**初めて**展開し、同梱の `darask-plugin.bat` を別
//! コンソールで起動する。起動時のディレクトリ走査や自動接続は一切しない
//! (SPEC §55.1 の「起動時にプラグインを探さない」を維持する)。
//!
//! 展開は Windows 10 以降に標準搭載の `%SystemRoot%\System32\tar.exe`
//! (bsdtar)に委ねる。zip パーサ依存を増やさずに済み、bsdtar は既定で
//! 絶対パス・`..` を含むエントリの書き出しを拒否する(パストラバーサル
//! 防御)。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use crate::plugin::json_string;

/// 既定のプラグインフォルダ名(実行ファイルと同じ階層)。
pub const DEFAULT_PLUGIN_DIR_NAME: &str = "plugins";
/// `darask-plugin.json` の `name`。
pub const IOPAINT_MANIFEST_NAME: &str = "iopaint";
pub const DIFFUSION_MANIFEST_NAME: &str = "ai-diffusion";

const MANIFEST_FILE: &str = "darask-plugin.json";
const STAMP_FILE: &str = ".darask-zip-stamp";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ZIP_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub enum LaunchError {
    /// プラグインフォルダ(または zip)が無い。
    NotFound,
    /// zip の展開に失敗した(`tar.exe` が無い・壊れた zip 等)。
    Extract(String),
    /// 展開先に `darask-plugin.json` はあるが内容・ランチャーが不正。
    InvalidManifest,
    /// ランチャーのプロセス生成に失敗した。
    Spawn(io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("plugin not found in plugin folder"),
            Self::Extract(detail) => write!(f, "zip extraction failed: {detail}"),
            Self::InvalidManifest => f.write_str("invalid darask-plugin.json or missing launcher"),
            Self::Spawn(error) => write!(f, "failed to start launcher: {error}"),
        }
    }
}

/// 展開済みプラグイン(`dir` に `darask-plugin.json` と `launcher` がある)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    pub dir: PathBuf,
    pub launcher: PathBuf,
}

/// 設定値 `configured`(空なら既定)からプラグインフォルダを決める。
/// 既定は実行ファイルと同じ階層の `plugins`。実行ファイルの場所が取れない
/// 場合だけ `None`。
pub fn resolve_plugin_dir(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        return Some(PathBuf::from(trimmed));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(DEFAULT_PLUGIN_DIR_NAME))
}

/// `root` 直下の zip を必要なら展開し、`darask-plugin.json` の `name` が
/// `manifest_name` に一致するプラグインを返す。
pub fn find_plugin(root: &Path, manifest_name: &str) -> Result<InstalledPlugin, LaunchError> {
    if !root.is_dir() {
        return Err(LaunchError::NotFound);
    }
    extract_pending_zips(root)?;
    let mut candidates: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|_| LaunchError::NotFound)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    candidates.sort();
    for dir in candidates {
        if let Some(found) = load_manifest(&dir, manifest_name) {
            return found;
        }
        // GitHub の "Download ZIP" は `<repo>-<ref>/` を 1 段挟むので、
        // 直下にマニフェストが無ければ 1 段だけ潜る。
        let Ok(children) = fs::read_dir(&dir) else {
            continue;
        };
        let mut nested: Vec<PathBuf> = children
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        nested.sort();
        for child in nested {
            if let Some(found) = load_manifest(&child, manifest_name) {
                return found;
            }
        }
    }
    Err(LaunchError::NotFound)
}

/// `dir/darask-plugin.json` を読み、`name` が一致すれば結果を返す
/// (無い/別プラグインなら `None`、一致するが壊れていれば `Some(Err)`)。
fn load_manifest(dir: &Path, manifest_name: &str) -> Option<Result<InstalledPlugin, LaunchError>> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = read_limited(&manifest_path, MAX_MANIFEST_BYTES).ok()?;
    let name = json_string(&text, "name").ok()?;
    if name != manifest_name {
        return None;
    }
    let Ok(launcher) = json_string(&text, "launcher") else {
        return Some(Err(LaunchError::InvalidManifest));
    };
    // ランチャーは同一ディレクトリ内の単一ファイル名に限定する。
    let is_plain_name = !launcher.is_empty()
        && !launcher.contains(['/', '\\', ':'])
        && launcher != "."
        && launcher != "..";
    if !is_plain_name {
        return Some(Err(LaunchError::InvalidManifest));
    }
    let launcher_path = dir.join(&launcher);
    if !launcher_path.is_file() {
        return Some(Err(LaunchError::InvalidManifest));
    }
    Some(Ok(InstalledPlugin {
        name,
        dir: dir.to_path_buf(),
        launcher: launcher_path,
    }))
}

fn read_limited(path: &Path, max_bytes: u64) -> io::Result<String> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut text = String::new();
    file.take(max_bytes + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "manifest too large",
        ));
    }
    Ok(text)
}

/// zip の同一性スタンプ(サイズ+更新時刻)。zip が差し替えられたら再展開する。
fn zip_stamp(zip: &Path) -> io::Result<String> {
    let meta = fs::metadata(zip)?;
    if meta.len() > MAX_ZIP_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "zip too large"));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    Ok(format!("{}:{}", meta.len(), mtime))
}

/// `root` 直下の `*.zip` を、同名(拡張子なし)のフォルダへ展開する。
/// スタンプが一致する(既に展開済みで zip も変わっていない)ものは飛ばす。
fn extract_pending_zips(root: &Path) -> Result<(), LaunchError> {
    let entries = fs::read_dir(root).map_err(|_| LaunchError::NotFound)?;
    let mut zips: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        })
        .collect();
    zips.sort();
    for zip in zips {
        let Some(stem) = zip.file_stem() else {
            continue;
        };
        let target = root.join(stem);
        let stamp = zip_stamp(&zip).map_err(|error| LaunchError::Extract(error.to_string()))?;
        let stamp_path = target.join(STAMP_FILE);
        if fs::read_to_string(&stamp_path).is_ok_and(|previous| previous == stamp) {
            continue;
        }
        // 自分が展開したフォルダ(スタンプがある)だけは作り直す。
        // ユーザーが手で作ったフォルダは消さず、上書き展開に留める。
        if stamp_path.is_file() {
            fs::remove_dir_all(&target).map_err(|error| LaunchError::Extract(error.to_string()))?;
        }
        fs::create_dir_all(&target).map_err(|error| LaunchError::Extract(error.to_string()))?;
        extract_zip(&zip, &target)?;
        fs::write(&stamp_path, stamp).map_err(|error| LaunchError::Extract(error.to_string()))?;
    }
    Ok(())
}

fn system_tar() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("tar.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("tar"))
}

fn extract_zip(zip: &Path, target: &Path) -> Result<(), LaunchError> {
    let mut command = Command::new(system_tar());
    command
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .map_err(|error| LaunchError::Extract(format!("tar: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LaunchError::Extract(stderr.trim().to_owned()));
    }
    Ok(())
}

/// 自分が起動したランチャー(`cmd.exe`)。ウィンドウが閉じられていない限り
/// 同じプラグインを二重起動しない。
static RUNNING: Mutex<Vec<(String, Child)>> = Mutex::new(Vec::new());

/// `name` のランチャーがまだ動いているか(`try_wait` で確認し、終了済みは
/// 一覧から外す)。
pub fn launcher_is_running(name: &str) -> bool {
    let Ok(mut running) = RUNNING.lock() else {
        return false;
    };
    running.retain_mut(|(_, child)| matches!(child.try_wait(), Ok(None)));
    running.iter().any(|(running_name, _)| running_name == name)
}

/// ランチャーを別コンソールで起動する(初回セットアップの進捗・質問が
/// ユーザーに見えるように、**非表示にはしない**)。
///
/// 標準入出力は指定しない: GUI サブシステムの本体には標準ハンドルが無いので、
/// 子は新しいコンソールの入出力を使う(`Stdio::null()` を渡すと bat の表示が
/// 消え、`pause` / `set /p` が即座に抜けてしまう)。
pub fn launch(plugin: &InstalledPlugin) -> Result<(), LaunchError> {
    if launcher_is_running(&plugin.name) {
        return Ok(());
    }
    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut command = Command::new(comspec);
    command
        .arg("/C")
        .arg(&plugin.launcher)
        .current_dir(&plugin.dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    let child = command.spawn().map_err(LaunchError::Spawn)?;
    if let Ok(mut running) = RUNNING.lock() {
        running.push((plugin.name.clone(), child));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "darask-plugin-test-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    fn write_manifest(dir: &Path, name: &str, launcher: &str) {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(
            dir.join(MANIFEST_FILE),
            format!("{{\n  \"name\": \"{name}\",\n  \"displayName\": \"x\",\n  \"launcher\": \"{launcher}\"\n}}\n"),
        )
        .expect("write manifest");
    }

    #[test]
    fn resolve_uses_configured_path_when_present() {
        assert_eq!(
            resolve_plugin_dir("  D:\\plugins  "),
            Some(PathBuf::from("D:\\plugins"))
        );
    }

    #[test]
    fn resolve_defaults_to_plugins_beside_the_executable() {
        let dir = resolve_plugin_dir("").expect("exe dir");
        assert_eq!(dir.file_name().and_then(|n| n.to_str()), Some("plugins"));
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        assert_eq!(dir.parent().map(Path::to_path_buf), exe_dir);
    }

    #[test]
    fn missing_root_is_not_found() {
        let root = temp_root("missing").join("nope");
        assert!(matches!(
            find_plugin(&root, IOPAINT_MANIFEST_NAME),
            Err(LaunchError::NotFound)
        ));
    }

    #[test]
    fn finds_manifest_directly_under_root() {
        let root = temp_root("direct");
        let dir = root.join("darask-paint-iopaint");
        write_manifest(&dir, IOPAINT_MANIFEST_NAME, "darask-plugin.bat");
        fs::write(dir.join("darask-plugin.bat"), "@echo off\n").expect("bat");
        let found = find_plugin(&root, IOPAINT_MANIFEST_NAME).expect("found");
        assert_eq!(found.dir, dir);
        assert_eq!(found.launcher, dir.join("darask-plugin.bat"));
        assert!(matches!(
            find_plugin(&root, DIFFUSION_MANIFEST_NAME),
            Err(LaunchError::NotFound)
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_manifest_one_level_deeper_for_github_style_zips() {
        let root = temp_root("nested");
        let dir = root
            .join("darask-paint-ai-diffusion-main")
            .join("darask-paint-ai-diffusion-main");
        write_manifest(&dir, DIFFUSION_MANIFEST_NAME, "darask-plugin.bat");
        fs::write(dir.join("darask-plugin.bat"), "@echo off\n").expect("bat");
        let found = find_plugin(&root, DIFFUSION_MANIFEST_NAME).expect("found");
        assert_eq!(found.dir, dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn launcher_outside_plugin_dir_is_rejected() {
        let root = temp_root("badlauncher");
        let dir = root.join("evil");
        write_manifest(&dir, IOPAINT_MANIFEST_NAME, "..\\\\evil.bat");
        assert!(matches!(
            find_plugin(&root, IOPAINT_MANIFEST_NAME),
            Err(LaunchError::InvalidManifest)
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_launcher_file_is_rejected() {
        let root = temp_root("nolauncher");
        write_manifest(&root.join("p"), IOPAINT_MANIFEST_NAME, "darask-plugin.bat");
        assert!(matches!(
            find_plugin(&root, IOPAINT_MANIFEST_NAME),
            Err(LaunchError::InvalidManifest)
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    fn make_zip(root: &Path, zip_name: &str, source_dir: &Path) {
        let status = Command::new(system_tar())
            .arg("-a")
            .arg("-cf")
            .arg(root.join(zip_name))
            .arg("-C")
            .arg(source_dir.parent().expect("parent"))
            .arg(source_dir.file_name().expect("name"))
            .status()
            .expect("tar available");
        assert!(status.success(), "tar -a -cf should succeed");
    }

    #[cfg(windows)]
    #[test]
    fn zip_in_root_is_extracted_once_and_reextracted_when_replaced() {
        let root = temp_root("zip");
        let staging = temp_root("zip-staging");
        let src = staging.join("darask-paint-iopaint");
        write_manifest(&src, IOPAINT_MANIFEST_NAME, "darask-plugin.bat");
        fs::write(src.join("darask-plugin.bat"), "@echo off\n").expect("bat");
        make_zip(&root, "darask-paint-iopaint.zip", &src);

        let found = find_plugin(&root, IOPAINT_MANIFEST_NAME).expect("extracted");
        let extracted_root = root.join("darask-paint-iopaint");
        assert_eq!(found.dir, extracted_root.join("darask-paint-iopaint"));
        assert!(extracted_root.join(STAMP_FILE).is_file());

        // 2 回目は再展開しない(展開先へ置いた印が残る)。
        let marker = extracted_root.join("marker.txt");
        fs::write(&marker, "keep").expect("marker");
        find_plugin(&root, IOPAINT_MANIFEST_NAME).expect("still found");
        assert!(marker.is_file(), "unchanged zip must not be re-extracted");

        // zip を差し替える(サイズが変わる)と作り直される。
        fs::write(src.join("extra.txt"), "changed").expect("extra");
        make_zip(&root, "darask-paint-iopaint.zip", &src);
        find_plugin(&root, IOPAINT_MANIFEST_NAME).expect("re-extracted");
        assert!(
            !marker.is_file(),
            "replaced zip must be re-extracted from scratch"
        );
        assert!(extracted_root
            .join("darask-paint-iopaint")
            .join("extra.txt")
            .is_file());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&staging);
    }

    #[cfg(windows)]
    #[test]
    fn corrupt_zip_reports_extract_error() {
        let root = temp_root("corrupt");
        fs::write(root.join("broken.zip"), b"not a zip").expect("write");
        assert!(matches!(
            find_plugin(&root, IOPAINT_MANIFEST_NAME),
            Err(LaunchError::Extract(_))
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn launcher_is_not_running_for_unknown_plugin() {
        assert!(!launcher_is_running("never-launched"));
    }
}
