use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub request: String,
    pub title: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    pub items: Vec<PlaylistItem>,
}

/// 歌单名校验：非空，且每字符为 字母数字(含 Unicode) / '-' / '_'。
/// 拒绝空、含 '/'、'.'、空格、'..' 等（防路径穿越，安全关键）。
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// 歌单存储：每个歌单一个 <dir>/<name>.toml。
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path_for(&self, name: &str) -> Result<PathBuf> {
        if !valid_name(name) {
            bail!("歌单名只能含字母数字、- 和 _");
        }
        Ok(self.dir.join(format!("{name}.toml")))
    }

    pub fn save(&self, p: &Playlist) -> Result<()> {
        let path = self.path_for(&p.name)?;
        std::fs::create_dir_all(&self.dir).context("创建歌单目录失败")?;
        let text = toml::to_string(p).context("序列化歌单失败")?;
        std::fs::write(&path, text).context("写入歌单失败")?;
        Ok(())
    }

    pub fn load(&self, name: &str) -> Result<Playlist> {
        let path = self.path_for(name)?;
        if !path.exists() {
            bail!("歌单 {name} 不存在");
        }
        let text = std::fs::read_to_string(&path).context("读取歌单失败")?;
        toml::from_str(&text).context("解析歌单失败")
    }

    pub fn list(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(names), // 目录不存在 → 空列表
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.path_for(name)?;
        if !path.exists() {
            bail!("歌单 {name} 不存在");
        }
        std::fs::remove_file(&path).context("删除歌单失败")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path().to_path_buf());
        (dir, s)
    }

    fn pl(name: &str, items: &[(&str, &str)]) -> Playlist {
        Playlist {
            name: name.to_string(),
            items: items
                .iter()
                .map(|(r, t)| PlaylistItem { request: r.to_string(), title: t.to_string() })
                .collect(),
        }
    }

    #[test]
    fn valid_name_rules() {
        assert!(valid_name("my_list-1"));
        assert!(valid_name("歌单"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".."));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a.toml"));
    }

    #[test]
    fn save_then_load_roundtrip() {
        let (_d, s) = store();
        let p = pl("mix", &[("https://x", "Song X"), ("/a.mp3", "Local A")]);
        s.save(&p).unwrap();
        let got = s.load("mix").unwrap();
        assert_eq!(got.name, "mix");
        assert_eq!(got.items.len(), 2);
        assert_eq!(got.items[0].request, "https://x");
        assert_eq!(got.items[0].title, "Song X");
        assert_eq!(got.items[1].title, "Local A");
    }

    #[test]
    fn list_sorted_and_delete() {
        let (_d, s) = store();
        s.save(&pl("beta", &[])).unwrap();
        s.save(&pl("alpha", &[])).unwrap();
        assert_eq!(s.list().unwrap(), vec!["alpha".to_string(), "beta".to_string()]);
        s.delete("alpha").unwrap();
        assert_eq!(s.list().unwrap(), vec!["beta".to_string()]);
    }

    #[test]
    fn load_missing_and_bad_name_error() {
        let (_d, s) = store();
        assert!(s.load("nope").is_err());
        assert!(s.save(&pl("a/b", &[])).is_err());
        assert!(s.delete("..").is_err());
    }

    #[test]
    fn list_empty_dir_ok() {
        let (_d, s) = store();
        assert!(s.list().unwrap().is_empty());
    }
}
