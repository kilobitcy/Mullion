//! SFTP 书签与默认目录(F120,schema v8)。
//!
//! **挂在 `SessionRecord` 上而不是做全局书签表**(设计 D15):`/data/Mullion`
//! 这种路径换台机器没有意义;点全局书签还要先问「在哪台机器上打开」,多一步。
//!
//! 路径在这一层是 `String`:它是**用户在表单里敲进去的东西**,天然是文本。
//! 到了 `mullion-ssh` 才转成 `RemotePath`(字节真源,见那边的 D16 修订)。

use serde::{Deserialize, Serialize};

/// 一条远端书签。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// 显示名。空串是允许的 —— 那时界面回退显示路径本身。
    pub name: String,
    pub path: String,
}

/// 一条会话的 SFTP 偏好。
///
/// **不是「可继承分节」** —— 它没挂在 `GroupRecord` 上、也不在 `PrefsLayer`
/// 里,分组改默认远端目录不会落到组内会话。这跟 `terminal`/`network` 那几个
/// 分节是两回事,别照着它们的样子给这里加继承语义:`/srv/app` 这种路径本来
/// 就是**一台机器**上的东西,拿去继承给一组机器没有意义(设计 D15)。
///
/// 字段全 `Option` / 空集合:**留空即用缺省**,远端 `.`(登录后的 home)、
/// 本地 `%USERPROFILE%`。不记忆「上次打开的目录」——那会让每次打开的位置
/// 取决于上次干了什么,而不是取决于配置(spec F120)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_local: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
}
