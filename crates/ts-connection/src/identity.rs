use std::path::Path;

use anyhow::Result;
use tsclientlib::Identity;

/// 若 `path` 存在则读取复用，否则生成新 identity 并写入。
pub fn load_or_create(path: &Path) -> Result<Identity> {
    if path.exists() {
        let s = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&s)?)
    } else {
        let id = Identity::create();
        std::fs::write(path, toml::to_string(&id)?)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reuses_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.toml");

        // 第一次：文件不存在 -> 生成并落盘
        let first = load_or_create(&path).unwrap();
        assert!(path.exists());

        // 第二次：从文件加载 -> 与第一次内容一致
        let second = load_or_create(&path).unwrap();
        assert_eq!(
            toml::to_string(&first).unwrap(),
            toml::to_string(&second).unwrap()
        );
    }
}
