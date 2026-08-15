//! 漫画のような複数ページ閲覧用のページ集合(SPEC §54)。

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSet {
    pub dir: PathBuf,
    pub entries: Vec<PageEntry>,
    pub current: usize,
    pub autosave: bool,
}

impl PageSet {
    pub fn enumerate(dir: &Path) -> Result<Self, String> {
        let read_dir = std::fs::read_dir(dir).map_err(|error| error.to_string())?;
        let mut entries = Vec::new();
        for item in read_dir {
            let item = item.map_err(|error| error.to_string())?;
            let file_type = item.file_type().map_err(|error| error.to_string())?;
            if file_type.is_file() && is_supported_page_path(&item.path()) {
                entries.push(PageEntry { path: item.path() });
            }
        }
        entries.sort_by(|left, right| {
            natural_cmp(
                left.path.file_name().unwrap_or(left.path.as_os_str()),
                right.path.file_name().unwrap_or(right.path.as_os_str()),
            )
        });
        Ok(Self {
            dir: dir.to_path_buf(),
            entries,
            current: 0,
            autosave: false,
        })
    }
}

pub fn is_supported_page_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "dpaint"
            )
        })
}

pub fn natural_cmp(left: &OsStr, right: &OsStr) -> Ordering {
    let left = left.as_encoded_bytes();
    let right = right.as_encoded_bytes();
    let mut left_at = 0;
    let mut right_at = 0;
    while left_at < left.len() && right_at < right.len() {
        if left[left_at].is_ascii_digit() && right[right_at].is_ascii_digit() {
            let left_end = digit_end(left, left_at);
            let right_end = digit_end(right, right_at);
            let left_digits = &left[left_at..left_end];
            let right_digits = &right[right_at..right_end];
            let left_significant = trim_leading_zeroes(left_digits);
            let right_significant = trim_leading_zeroes(right_digits);
            let order = left_significant
                .len()
                .cmp(&right_significant.len())
                .then_with(|| left_significant.cmp(right_significant))
                .then_with(|| left_digits.len().cmp(&right_digits.len()));
            if order != Ordering::Equal {
                return order;
            }
            left_at = left_end;
            right_at = right_end;
            continue;
        }
        let order = left[left_at]
            .to_ascii_lowercase()
            .cmp(&right[right_at].to_ascii_lowercase())
            .then_with(|| left[left_at].cmp(&right[right_at]));
        if order != Ordering::Equal {
            return order;
        }
        left_at += 1;
        right_at += 1;
    }
    left.len().cmp(&right.len())
}

fn digit_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn trim_leading_zeroes(digits: &[u8]) -> &[u8] {
    let first_nonzero = digits
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(digits.len().saturating_sub(1));
    &digits[first_nonzero..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn natural_cmp_orders_mixed_numeric_runs_by_value() {
        let mut names = ["p10.png", "p2.png", "p1.png", "p2a10.png", "p2a2.png"];
        names.sort_by(|left, right| natural_cmp(OsStr::new(left), OsStr::new(right)));
        assert_eq!(
            names,
            ["p1.png", "p2.png", "p2a2.png", "p2a10.png", "p10.png"]
        );
    }

    #[test]
    fn natural_cmp_uses_padding_length_as_numeric_tiebreaker() {
        assert_eq!(
            natural_cmp(OsStr::new("2"), OsStr::new("02")),
            Ordering::Less
        );
        assert_eq!(
            natural_cmp(OsStr::new("02"), OsStr::new("002")),
            Ordering::Less
        );
    }

    #[cfg(windows)]
    #[test]
    fn natural_cmp_accepts_non_unicode_names_without_lossy_conversion() {
        use std::os::windows::ffi::OsStringExt;
        let left = OsString::from_wide(&[b'p' as u16, 0xd800, b'2' as u16]);
        let right = OsString::from_wide(&[b'p' as u16, 0xd800, b'1' as u16, b'0' as u16]);
        assert_eq!(natural_cmp(&left, &right), Ordering::Less);
    }

    #[test]
    fn enumerate_filters_direct_children_and_sorts_naturally() {
        let dir = std::env::temp_dir().join(format!("darask_pages_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).expect("temporary directory should exist");
        for name in ["page10.PNG", "page2.jpg", "page1.dpaint", "ignore.txt"] {
            std::fs::write(dir.join(name), b"x").expect("seed file should be written");
        }
        std::fs::write(dir.join("nested/page0.png"), b"x").expect("nested file should exist");
        let set = PageSet::enumerate(&dir).expect("directory should enumerate");
        let names: Vec<&OsStr> = set
            .entries
            .iter()
            .filter_map(|entry| entry.path.file_name())
            .collect();
        assert_eq!(
            names,
            [
                OsStr::new("page1.dpaint"),
                OsStr::new("page2.jpg"),
                OsStr::new("page10.PNG")
            ]
        );
        assert_eq!(set.dir, dir);
        assert_eq!(set.current, 0);
        assert!(!set.autosave);
        let _ = std::fs::remove_dir_all(&set.dir);
    }
}
