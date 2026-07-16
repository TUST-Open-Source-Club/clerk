use std::path::{Path, PathBuf};

/// 将路径中的 `~` 展开为用户主目录。
/// 如果输入不是以 `~` 开头，或无法获取 HOME 环境变量，则返回原路径。
pub fn expand_tilde<P: AsRef<Path>>(path: P) -> PathBuf {
    let path = path.as_ref();
    let mut components = path.components();
    match components.next() {
        Some(std::path::Component::Normal(os_str)) if os_str == "~" => {
            if let Some(home) = std::env::var_os("HOME") {
                PathBuf::from(home).join(components.as_path())
            } else {
                path.to_path_buf()
            }
        }
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_for_home() {
        let expanded = expand_tilde("~/foo");
        assert!(!expanded.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn test_expand_tilde_no_change_for_relative() {
        assert_eq!(expand_tilde("foo/bar"), PathBuf::from("foo/bar"));
    }
}
