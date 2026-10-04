//! Writable per-user locations, including when the executable is in /nix/store.

use std::path::PathBuf;

#[cfg(not(windows))]
fn xdg_dir(value: Option<PathBuf>, home: Option<PathBuf>, fallback: &str) -> Option<PathBuf> {
    value
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(fallback))
        })
        .map(|path| path.join("darask-paint"))
}

pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(|path| PathBuf::from(path).join("darask-paint"))
    }
    #[cfg(not(windows))]
    {
        xdg_dir(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
            ".config",
        )
    }
}

#[cfg(not(windows))]
pub fn data_dir() -> Option<PathBuf> {
    xdg_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        ".local/share",
    )
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_require_absolute_values_and_fall_back_to_home() {
        let home = Some(PathBuf::from("/home/test user"));
        assert_eq!(
            xdg_dir(Some("/data".into()), home.clone(), ".local/share"),
            Some("/data/darask-paint".into())
        );
        for value in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(
                xdg_dir(value, home.clone(), ".config"),
                Some("/home/test user/.config/darask-paint".into())
            );
        }
        assert_eq!(xdg_dir(None, None, ".config"), None);
    }
}
