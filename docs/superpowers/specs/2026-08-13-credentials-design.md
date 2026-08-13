# 切片 H:凭据实体(F74)设计

> 状态:定案。实现计划见 `docs/superpowers/plans/2026-08-13-slice-h-credentials.md`。
> 相关:`spec.md` F74 / F75、ADR-002(TOML)、`docs/adr-010`(隧道作为一等对象的先例)。

## 1. 要解决的问题

今天每条会话都自带一份认证(`user` + `AuthKind` + 侧车里的密码/私钥正文/口令)。
一把私钥同时给 8 台机器用,就在库里躺了 8 份正文;换钥匙要改 8 处,漏一处的表现是
「有的机器连得上有的连不上」,而且**旧私钥仍留在磁盘上**。

F74 把凭据抽成可被多条会话引用的一等对象:改一处,全部生效。

## 2. 范围

**做**:凭据的数据模型 + 存储 + 引用完整性 + 连接链路解析 + 管理 UI + 会话编辑器里的选择器。

**不做**:
- **F75 显式去重提取**(扫出重复的 `(用户名, 私钥)` 提示合并)—— 另一片。本片
  **绝不在迁移里静默合并**,迁移后凭据表恒为空。
- 「引用 + 局部覆盖」(引用一份凭据但改掉用户名)。spec F74 定死了严格二选一:
  任何时刻只有一个真值。有覆盖就有「这台机器到底用的哪个用户名」的追查成本,
  而这正是本功能想消灭的东西。
- 凭据分组、凭据标签、凭据继承。凭据是叶子对象,不进继承体系。

## 3. 决策

### D1 `Auth` 改为枚举,严格二选一

```rust
pub enum Auth {
    Inline(InlineAuth),     // 今天的形态:{ user, kind }
    Ref(CredentialId),
}
```

不选「`Auth` 保持结构体 + 加一个 `Option<CredentialId>`」:那样两个真值可以同时存在,
「引用了凭据但 user 字段还留着老值」这种状态编译器管不住,只能靠约定,而约定会烂。

### D2 TOML 编码:`source` 标签 + 中间 repr 手工转换

```toml
# Inline(与 v8 逐字节一致 —— 不写 source)
[session.auth]
user = "ops"
kind = "public_key"
has_passphrase = true

# Ref
[session.auth]
source = "ref"
credential_id = 3
```

**不用 `#[serde(tag = "source")]` 加在枚举上**,两条硬理由:

1. `AuthKind` 本身就是内部标签枚举(`tag = "kind"`)且被 `flatten` 进 `[session.auth]`。
   再套一层内部标签就是「内部标签枚举里嵌 flatten 的内部标签枚举」——
   spec F74 验收 ① 点名过 toml 对这个组合的已知限制。
2. 内部标签枚举**没有默认变体**。v8 文件的 `[session.auth]` 里没有 `source` 键,
   加了标签就一律解析失败,「新版本能直接读旧版本」当场断掉。

实现:一个私有 `AuthRepr { source, user, kind, has_passphrase, credential_id }`
(全是标量,没有嵌套 flatten),`TryFrom` 双向转换。Inline 序列化时 `source` 与
`credential_id` 都 skip,所以 auth 分节的字节与 v8 完全相同。

**两个真值同时出现 = 解析失败**,不是「取其中一个」:
`source = "ref"` 却带着 `user`/`kind`,或者没有 `source` 却带着 `credential_id`,
都说明这份文件被手改坏了。静默取一个的后果是用户以为在用 A 身份、实际在用 B。

### D3 密文键空间:`cred:<id>`

会话密文的键今天是 `"1"`、`"2"`(`SessionId` 的十进制)。凭据用 `"cred:1"`。
带前缀而不是另开一张表:`secrets.enc` 是一个 `BTreeMap<String, SecretEntry>`,
另开表要改文件格式(又一次不兼容);前缀是纯加法,旧文件天然没有这类键。

`cred:` 前缀不可能与 `SessionId` 的十进制表示撞车,这一条有测试钉着。

**`Vault::open` 的孤儿裁剪(`secrets.retain`)必须同步扩集合**(spec F74 验收 ⑤):
不扩的话,每次打开都把凭据口令当孤儿静默删掉 —— 用户看到的是「昨天还能连,
今天要我重新输密码」。守护测试:存入凭据口令 → save → 重开 → 口令还在。

### D4 代理口令永远属于会话,不属于凭据

`SecretEntry` 有四个字段,凭据只接管三个(`password` / `passphrase` / `private_key`)。
`proxy_password` 留在会话自己的密文里:代理是**网络路径**,凭据是**身份**,
两台机器共用一把 SSH 私钥却各走各的代理是完全正常的配置。有守护测试。

### D5 解析产物是 owned 的 `ResolvedAuth`,解析点在 `Vault`

```rust
pub struct ResolvedAuth { pub user: String, pub kind: AuthKind, pub secret: Option<SecretEntry> }

impl Vault {
    pub fn resolve_auth(&self, rec: &SessionRecord) -> Result<ResolvedAuth, StoreError>;
    pub fn resolve_auth_of(&self, auth: &Auth, inline_secret: Option<&SecretEntry>)
        -> Result<ResolvedAuth, StoreError>;
}
```

参数化内核 `resolve_auth_of` 与 `resolve_layer` / `expand_jump_chain_of` 是同一个模式:
F92「测试连接」拨的是**尚未入库的草稿**,草稿没有 id,查不到自己的密文,
必须能把手上那份 inline secret 直接喂进来。两条路径共用一个内核,
否则迟早出现「拨测通过、保存后连不上」。

owned 而非借用:跳板链本来就是 `Vec<SessionRecord>`(clone 过一遍),
借用会让调用点被生命周期绑死,收益为零。

### D6 悬空引用硬失败,绝不回落

凭据被删(或文件被手改)后 `Ref(id)` 指不到东西 → `StoreError::DanglingCredential(id)`。
**不回落到 agent、不回落到空口令、不回落到任何别的身份** —— 与 `JumpDangling`
(F5)、`TunnelDangling`(F110)是同一条铁律:静默换一个身份去登录是安全事故。

这条落在 `resolve_auth`,而 `resolve_auth` 的结果在**组拨号参数之前**就要 `?`。
今天 `dial_plan::jump_auth` 在「跳板没存密码」时会退回 agent —— 那是**缺凭据**
的降级,允许;而**指错凭据**不允许。两者必须在不同的层解决:先解析(可失败),
再物化(不可失败)。所以 `build_hops_*` 的 `secret_of` 闭包换成
`auth_of: &dyn Fn(SessionId) -> ResolvedAuth`(非 Option),调用方在这之前
已经把每一跳都解析成功了。

### D7 被引用的凭据不可删,UI 列出引用者

`delete_credential` 返回 `StoreError::CredentialInUse(Vec<SessionId>)`。
对齐的是**跳板/隧道**的做法(硬失败 + 列出引用者),不是分组的做法
(删组 → 会话 `group_id` 置 `None`)。理由:分组是组织手段,丢了不影响能不能连;
凭据是身份,悄悄解绑等于把一堆会话变成连不上的废配置。

### D8 凭据管理进 `ManagerMode` 第四档

会话管理器已有「会话 / SFTP / 隧道」模式条(F116)。凭据加第四档「凭据」,
左栏列表 + 右栏表单,与隧道档同构。不新开窗口 —— F90 的结论就是不要第二个窗口。

排序:`会话 · SFTP · 凭据 · 隧道`。凭据紧挨着会话,因为它是会话的一部分被抽出来的;
隧道仍留在最后,它是另一类东西。

### D9 会话编辑器「认证」页顶部加「凭据来源」二选一

- `本会话独有`(默认):今天的表单原样。
- `共享凭据`:一个下拉选凭据,下面是**只读摘要**(用户名 + 认证方式 + 有没有口令),
  外加一句「在「凭据」页修改」。身份/凭据两个分节整体让位 —— 严格二选一在 UI 上
  的样子就是「另一半根本不在屏幕上」,不是「灰着但还看得见」。

凭据库为空时下拉禁用并说明「还没有共享凭据 —— 去「凭据」页新建」,
而不是给一个点了没反应的下拉(与 F93 私钥候选下拉同一条规矩)。

### D10 必填校验(F91)随来源切换

`本会话独有`:今天的规则不变(用户名不能空)。
`共享凭据`:用户名不再由会话提供,校验改为**必须选中一个凭据**;
缺项仍映射到「认证」Tab、仍打红点。判定是纯函数,可无窗口单测。

### D11 schema v8 → v9,零迁移代码

v8 的 `[session.auth]` 没有 `source` 键 → 按 D2 直接解析成 `Inline`;
`[[credential]]` 数组缺失 → `serde(default)` 补空。所以**没有一行迁移转换代码**,
但版本号仍要升:v9 文件里可能有 `source = "ref"` 的会话,旧客户端读了会把
`credential_id` 当未知字段丢掉、把 `user`/`kind` 当缺失 —— 拒绝比装作能用好。

迁移单测仍要写(spec F74 验收 ②):v8 文件逐字段等价映射成 `Inline`、
**凭据表为空**(证明没有静默合并,守 F75 的边界)、`.bak` 存在。

## 4. 失效模式清单(每条配一个测试)

| # | 失效模式 | 症状 | 守护 |
|---|---|---|---|
| 1 | 孤儿裁剪没扩集合 | 凭据口令每次打开都被静默删掉 | `credential_secrets_survive_reopen` |
| 2 | 悬空引用降级 | 用一个**别的**身份登上了机器 | `a_dangling_credential_is_rejected_never_degraded` |
| 3 | 删掉被引用的凭据 | 一堆会话变成连不上的废配置 | `deleting_a_referenced_credential_is_refused_and_names_the_referents` |
| 4 | 代理口令被凭据接管 | 换一把共享私钥,顺手把三台机器的代理口令换没了 | `the_proxy_password_always_comes_from_the_session_not_the_credential` |
| 5 | auth 两个真值并存 | 「以为在用 A 身份、实际在用 B」 | `an_auth_section_with_both_shapes_is_rejected` |
| 6 | 草稿走了第二条解析路径 | 拨测通过、保存后连不上 | `a_draft_referencing_a_credential_dials_with_it` |
| 7 | 迁移静默合并 | 用户没点头就被改了数据结构(F75 的边界) | `migrating_v8_leaves_the_credential_table_empty` |
| 8 | 密文键撞车 | 凭据密文覆盖掉会话密文 | `credential_secret_keys_cannot_collide_with_session_ids` |
