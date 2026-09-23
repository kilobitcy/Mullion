//! F290:命令抽屉的纯判据。零 IO、零 egui。

/// 抽屉该 `cd` 到哪。按序回退:父 pane 上报的**绝对**路径 → 所属项目的
/// 目录 → 不 `cd`(`None`)。
///
/// 只认绝对路径:F123 的窗口标题那条腿会报 `~/x`,而抽屉是一条**新** shell,
/// `cd '~/x'` 单引号里的 `~` 不展开,会 `cd` 失败还多一行报错。
/// 项目目录是用户手填的,同样要求以 `/` 开头。
pub fn drawer_cwd(pane_cwd: Option<&[u8]>, project_dir: Option<&str>) -> Option<Vec<u8>> {
    if let Some(c) = pane_cwd.filter(|c| c.starts_with(b"/")) {
        return Some(c.to_vec());
    }
    project_dir
        .filter(|d| d.starts_with('/'))
        .map(|d| d.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自证会变红:把第一段的 `starts_with(b"/")` 过滤删掉(`~/x` 会被当目录)。
    #[test]
    fn the_pane_directory_wins_only_when_it_is_absolute() {
        assert_eq!(
            drawer_cwd(Some(b"/srv/app"), Some("/proj")),
            Some(b"/srv/app".to_vec())
        );
        assert_eq!(
            drawer_cwd(Some(b"~/app"), Some("/proj")),
            Some(b"/proj".to_vec())
        );
    }

    /// 自证会变红:把项目目录那段的 `starts_with('/')` 删掉。
    #[test]
    fn the_project_directory_is_the_fallback_and_relative_ones_are_refused() {
        assert_eq!(drawer_cwd(None, Some("/proj")), Some(b"/proj".to_vec()));
        assert_eq!(drawer_cwd(None, Some("proj")), None);
        assert_eq!(drawer_cwd(None, None), None);
    }
}
