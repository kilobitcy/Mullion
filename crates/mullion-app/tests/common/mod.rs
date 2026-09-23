//! 源码切片守护共用的两把剪刀。
//!
//! 这两件事每个「扫源码找模式」的守护都要做,而两件都**做错了不报错**:
//! 剥少了假红(注释里提一句就被当成真代码),剥多了假绿(真代码被当成测试
//! 丢掉)。放一份共享实现,省得每个守护各自踩一遍。

/// 去掉 `#[cfg(test)]` 标注的那些块,**只去掉块本身**。
///
/// 朴素写法 `src.split("#[cfg(test)]").next()` 是错的:它把第一个测试模块
/// **之后的全部内容**一起丢掉。本仓库的测试模块惯例放文件末尾,所以那样写
/// 平时看着没事 —— 直到有人把新代码加在测试模块后面,扫描器从此看不见它,
/// 守护静默失效(F287 变异实测:在 `group_manager.rs` 末尾加一个裸搜索框,
/// 两条对账全绿)。
///
/// 这里按花括号配对数到块结束,块之后的代码照常保留。字符串字面量里的花括号
/// 会让计数偏掉,但那只影响到「块在哪结束」,而本仓库的测试模块一律到文件末尾
/// 或紧跟另一个 item,偏掉的后果是多剥一点(偏保守),不会造出假绿。
pub fn strip_cfg_test(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        // 从标注往后找第一个 `{`,再数到配对的 `}`。找不到 `{` 的(例如
        // `#[cfg(test)] use ...;`)就只吃掉这一行。
        let Some(open) = after.find('{') else {
            let line_end = after.find('\n').map_or(after.len(), |n| n + 1);
            rest = &after[line_end..];
            continue;
        };
        let mut depth = 0usize;
        let mut end = after.len();
        for (j, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// 去掉行注释。
///
/// **这一步不能省**:本仓库的注释密度很高,判据关键词在注释里出现几十次 ——
/// 不剥掉的话扫描器一边假绿(注释顶替了真调用)一边假红(注释里的关键词被
/// 当成违规)。
pub fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// 生产代码的每一行(已剥注释与 `#[cfg(test)]` 块)。
pub fn prod_lines(src: &str) -> Vec<String> {
    strip_cfg_test(src)
        .lines()
        .map(|l| strip_comment(l).to_owned())
        .collect()
}
