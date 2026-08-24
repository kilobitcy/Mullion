# ADR-011：差分整形的判脏用行内容指纹，不用 `Term::damage()`

- 日期：2026-08-24
- 状态：已采纳
- 关联：F12（P0）、N1/N2、[adr-001](adr-001-glyph-rendering.md)、领域陷阱 T3
- 设计文档：`docs/superpowers/specs/2026-08-24-f12-row-fingerprint-diff-shaping-design.md`

## 背景

`TextLayer::prepare_panes` 原来每帧对所有 pane 的每一行无条件重新整形。
作者 2026-08-21 的量化脚手架实测 8 pane ≈ 18~23ms/帧；2026-08-24 的 Windows
实机对照实验显示，每加一个挂 tmux + Claude Code 的 pane，进程 CPU 跳 0.6~1.6
个点，而加两个不接 tmux 的 pane 只多 0.3%——差别不在内容量（都空闲），
而在 tmux 空闲时仍周期性吐极小的字节，每一次都触发一轮**全窗口**整形。
同机 xshell 连同样的节点跑同样的 tmux + Claude Code，常驻 CPU 0.2%。

`spec.md` 的 F12 原文点名了 `Term::damage()`，alacritty_terminal 0.26.0 也确实
提供 `Term::damage() -> TermDamage` + `reset_damage()`。

## 决策

**不用 `Term::damage()`。** 在 `Emulator::snapshot()` 里给每行算一个覆盖
`SnapCell` 全部字段的 FNV-1a 指纹，渲染层拿它跟上一帧比。

## 理由

`Term::damage()` 只知道 alacritty 自己改过的格子。而能改变「一行最终长什么样」
的来源至少有七个：

1. `Term` 内容变化 —— damage 知道
2. **选区反色** —— `text::row_to_spans` 把选中格的文字色从 `fg` 换成 `bg`；
   alacritty 的 `term/mod.rs:450-452` **明说** selection 不在 damage 里
3. IME preedit 让路（`hidden_span_for_row`）
4. **主题换色** —— `Emulator::set_default_colors` 改的是 `palette::resolve`
   解析出的 fg/bg，alacritty 完全不知道
5. 字体族 / 字号变化（`TextLayer::set_font` 改 metrics）
6. DPI 缩放变化
7. pane 像素宽度变化（进 `Buffer::set_size` 的 `avail`）

以 damage 为基础就必须**逐个枚举**这七个来源去求并集。漏掉任何一个，症状是
**屏幕上留着一行陈旧的字**——编译不报错、测试不报错、日志不报错，只有人眼
能发现，正落在 `CLAUDE.md` §「你无法验证的东西」那一类。而「列举式门控在加档
时必然漏」在本项目已经踩中过三次。

行指纹把判据从「列举所有会变的原因」翻转成「直接看结果变没变」：1/2/4 已经烘进
快照字段，自动覆盖；5/6 由 `set_font` 里一次整体 `clear` 覆盖；7 由缓存条目
自带的 `term_w` 比对覆盖；3 单独处理（只有一行，且**绝不写缓存**）。

**失败方向也反过来了**：指纹方案的最坏情况是多整形一次（画面永远正确），
damage 方案的最坏情况是少画（静默陈旧）。

## 缓存键的实际语义：内容寻址，`PaneId` 只是分桶

缓存键写的是 `(PaneId, row)`，但 **`PaneId` 并不全局唯一**：它只在单个
`Workspace` 内单调递增，每个新标签页、每次断线重连产生的新 `Workspace`
都从头计数（首个 pane 恒 `PaneId(1)`）。所以切标签、重连都会让不同内容
先后落在同一个键位上。

这不构成 bug，理由不是「概率小」而是**构造上就安全**：命中的最终判据是
`hash == hash && term_w == term_w`，而整形结果只取决于（内容、`term_w`、字体），
与它出自哪块 pane 无关。两个标签的第 5 行若指纹相同，那它们本来就该整形成
同一份东西，复用是**正确的**，不是侥幸。`PaneId` 在这里的作用只是分桶，
降低不同内容争用同一槽位的频率。

键里那个「稳定身份」的要求仍然成立，但它约束的是**同一帧内**不能用
`panes.iter().enumerate()` 的下标（关掉中间一块会让其后所有 pane 挪位，
当帧就张冠李戴）——守护测试
`shaped_cache::tests::the_cache_is_keyed_by_pane_id_not_by_frame_index`
钉的正是这一条。

## 代价

- 每帧多算约 0.1~0.3ms 的哈希（对照要省掉的 18~23ms）。
- 需要修订 `spec.md` 里 F12 的措辞。
- 指纹的字段覆盖面成了新的关键不变量，由两层机械守护看着：存量字段靠
  `snapshot.rs` 里六条逐字段测试，增量字段靠 `hash_row` 函数体内的穷尽解构
  （给 `SnapCell` 加字段即编译报错）。

## 被否的备选

- **A. damage 驱动**（`spec.md` 字面）：风险如上。
- **C. 两者都做**（damage 决定重建哪些**快照行** + 指纹决定重整形）：收益能叠加
  （还能省掉每帧那份 `cols×rows` 的 `Vec` 分配），但复杂度翻倍，且 A 的静默
  风险原样保留。留作后续——真要做的时候，指纹仍是最终判据，damage 只作为
  「少建几行快照」的优化，不改变失效判定的构造式性质。

## 重新考虑的触发条件

若剖面显示 `Emulator::snapshot()` 每帧重建整份 `cols×rows` 成为新的头号开销
（`pump` 阶段的 p95 压过 `text_prepare`），就该重开备选 C。
