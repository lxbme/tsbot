use std::collections::HashMap;

use serde::Deserialize;

/// 角色 + 单指令权限。按 uid 判定，与 TS3 服务器组解耦。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Permissions {
    #[serde(default = "default_role")]
    pub default_role: String,
    /// 角色名 → 允许的指令名列表（含 "*" 表示全部）。
    #[serde(default)]
    pub roles: HashMap<String, Vec<String>>,
    /// uid → 角色名。
    #[serde(default)]
    pub users: HashMap<String, String>,
}

fn default_role() -> String {
    "guest".to_string()
}

impl Permissions {
    /// 是否允许某 uid 执行某指令。
    /// roles 为空（未配置权限）→ 一律放行（opt-in）。
    pub fn allows(&self, uid: &str, command: &str) -> bool {
        if self.roles.is_empty() {
            return true;
        }
        let role = self.role_of(uid);
        match self.roles.get(role) {
            Some(cmds) => cmds.iter().any(|c| c == "*" || c == command),
            None => false,
        }
    }

    /// uid 当前归属的角色名（未映射则 default_role）。
    pub fn role_of(&self, uid: &str) -> &str {
        self.users.get(uid).map(String::as_str).unwrap_or(&self.default_role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms() -> Permissions {
        let mut roles = HashMap::new();
        roles.insert("admin".to_string(), vec!["*".to_string()]);
        roles.insert("guest".to_string(), vec!["play".to_string(), "queue".to_string()]);
        let mut users = HashMap::new();
        users.insert("admin-uid".to_string(), "admin".to_string());
        Permissions { default_role: "guest".to_string(), roles, users }
    }

    #[test]
    fn disabled_when_no_roles() {
        let p = Permissions::default();
        assert!(p.allows("anyone", "stop"));
    }

    #[test]
    fn admin_wildcard_allows_all() {
        let p = perms();
        assert!(p.allows("admin-uid", "stop"));
        assert!(p.allows("admin-uid", "playlist"));
    }

    #[test]
    fn guest_limited() {
        let p = perms();
        assert!(p.allows("stranger", "play"));
        assert!(p.allows("stranger", "queue"));
        assert!(!p.allows("stranger", "stop"));
    }

    #[test]
    fn unknown_role_denies() {
        let mut p = perms();
        p.default_role = "ghost".to_string();
        assert!(!p.allows("stranger", "play"));
    }

    #[test]
    fn role_of_maps() {
        let p = perms();
        assert_eq!(p.role_of("admin-uid"), "admin");
        assert_eq!(p.role_of("stranger"), "guest");
    }
}
