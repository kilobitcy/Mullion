//! F346-a 设计 D5:手机端只读,不回写。主判据是结构性的(`dependency_direction.rs`
//! 里「ffi 不依赖 mullion-cloud」);这里是辅助的源码守护。
//!
//! 注释只剥**整行**的:剥行尾注释会连同一行的代码一起剥掉(剥多 → 假绿)。
//! 测试模块形如 `#[cfg(test)]\nmod tests { ... }`:只按花括号配平切掉这一块,
//! 其后的生产代码仍参与检查(切到文件末尾会切多 → 假绿);形状不对或找不到
//! 配平的 `}` 就 panic,而不是静默放过。

const FORBIDDEN: &[&str] = &["write_pack", "seal_secrets", "put_no_overwrite", ".save("];

/// 切掉 `#[cfg(test)]\nmod tests { ... }` 这一块,保留前后的内容。
fn strip_tests_mod(text: &str, path: &std::path::Path) -> String {
    let Some(i) = text.find("#[cfg(test)]") else {
        return text.to_string();
    };
    assert!(
        text[i..].starts_with("#[cfg(test)]\nmod tests"),
        "{}:`#[cfg(test)]` 后面不是 `mod tests`,守护无法安全切片",
        path.display()
    );
    let open = text[i..]
        .find('{')
        .map(|o| i + o)
        .unwrap_or_else(|| panic!("{}:`mod tests` 后找不到 `{{`", path.display()));
    let mut depth = 0usize;
    for (off, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = open + off + 1;
                    return format!("{}{}", &text[..i], &text[end..]);
                }
            }
            _ => {}
        }
    }
    panic!("{}:`mod tests` 的花括号没有配平", path.display());
}

fn production_source() -> String {
    let mut out = String::new();
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let body = strip_tests_mod(&text, &path);
            for line in body.lines().filter(|l| !l.trim_start().starts_with("//")) {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

#[test]
fn ffi_production_code_never_calls_a_write_back_api() {
    let src = production_source();
    // 防恒绿:确认真扫到了导入入口(而不是扫了个空目录)。
    assert!(
        src.contains("install_projects_only"),
        "解析失效:没扫到导入入口"
    );
    for f in FORBIDDEN {
        assert!(!src.contains(f), "ffi 生产代码出现了回写调用 `{f}`");
    }
}
