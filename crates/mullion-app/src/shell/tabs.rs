//! 标签容器(F36 最小版):活动下标、关闭后的焦点转移、按世代号路由、切换语义。
//!
//! **内容是泛型参数**,这不是为了将来能装别的东西,是为了让本模块**结构上够不着**
//! 标签里装的是什么 —— 路由和焦点转移是纯下标算术,一旦这里能读到 `Workspace`,
//! 迟早有人在这里顺手判一句「这个标签连着没有」,判据就散了(S1/S2 的落点)。
//! 唯一需要从内容里取的东西是世代号,走 [`TabPayload`] 这一个方法,别的取不到。
//!
//! 零 egui/winit/wgpu/tokio,可纯单测。`app.rs` 只做接线。

use mullion_store::SessionId;

/// 标签身份。**不是下标** —— 下标会因为关闭左边的标签而位移,身份不会。
///
/// 全进程单调递增(由 [`Tabs::next_id`] 发),关掉的标签的 id 不回收:回收会让
/// 「关了 A 又开了个新的,新的拿到 A 的 id」,任何缓存了 id 的地方都会张冠李戴。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u32);

/// 标签内容必须能报出自己的世代号(S1)。
///
/// 迟到事件(`PaneOpened` / 自动化结论 / `ConnectErr`)携带的世代号是**路由键**:
/// `next_ws_generation` 全局单调递增,世代号天然全局唯一,拿它就能定位属主标签,
/// 不需要再往事件负载里塞一个 `TabId`。
pub trait TabPayload {
    fn generation(&self) -> u64;
}

/// 一个标签。
///
/// `title` / `session_id` 是**标签自己的事实**,不从内容里推 —— D1 的 SFTP 标签
/// 没有 `Workspace` 可推。终端标签建立时从 `HostConn` 播种一次,此后不变。
pub struct Tab<C> {
    pub id: TabId,
    /// 标签栏上显示的名字。会话名,快速连接时是 `user@host`。
    pub title: String,
    /// F61/F62 外观查表键(`AppearanceCache::get`)。`None` = 快速连接或 store 不可用,
    /// 此时标签不着色、不画图标。
    pub session_id: Option<SessionId>,
    pub content: C,
}

/// 一列标签 + 活动下标。
///
/// **空 = launcher 态**(取代原先的 `ws: None`)。`active` 在非空时恒为合法下标,
/// 这条不变量由 [`Tabs::open`] / [`Tabs::close`] 维持,外部拿不到 `active` 的可写引用。
pub struct Tabs<C> {
    tabs: Vec<Tab<C>>,
    active: usize,
    next_id: u32,
}

impl<C> Default for Tabs<C> {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
        }
    }
}

impl<C> Tabs<C> {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// 活动下标。空表时无意义,调用方应先判 [`Tabs::is_empty`]。
    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab<C>> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab<C>> {
        self.tabs.iter_mut()
    }

    pub fn active(&self) -> Option<&Tab<C>> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab<C>> {
        self.tabs.get_mut(self.active)
    }

    pub fn get(&self, ix: usize) -> Option<&Tab<C>> {
        self.tabs.get(ix)
    }

    /// 开一个新标签,**它成为活动标签**,返回新发的身份。
    pub fn open(&mut self, title: String, session_id: Option<SessionId>, content: C) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            title,
            session_id,
            content,
        });
        self.active = self.tabs.len() - 1;
        id
    }

    /// 关掉第 `ix` 个标签,把它**交还给调用方收口**(Task 6:先 abort 自动化,
    /// 再 drop 内容关 channel)。这里绝不自己 drop —— 收口顺序是 app 的事,
    /// 在容器里 drop 等于把顺序钉死在够不着运行时的地方。
    ///
    /// 焦点转移:关的不是活动标签,活动的**还是同一个** `TabId`(只做下标位移修正);
    /// 关的是活动标签则取**右邻**,没有右邻取左邻;关空则回 launcher 态。
    pub fn close(&mut self, ix: usize) -> Option<Tab<C>> {
        if ix >= self.tabs.len() {
            return None;
        }
        let gone = self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if ix < self.active {
            // 活动标签整体左移了一格,身份不变。
            self.active -= 1;
        } else if ix == self.active && self.active >= self.tabs.len() {
            // 关掉的是最后一个,没有右邻可顶 —— 取左邻。
            self.active = self.tabs.len() - 1;
        }
        // ix == active 且还有右邻:下标不动,右邻自然顶上来。
        // ix > active:活动标签在它左边,不受影响。
        Some(gone)
    }

    /// F37:**就地**把 `id` 那个标签换成新内容,把旧的交还给调用方收口。
    /// `None` = 没有这个 id(它在拨号期间被关掉了)。
    ///
    /// 位置与活动下标都不动 —— 「重连」的语义是「这个标签活过来了」,
    /// 不是「关掉它再开一个新的」:后者会让标签跳到最右边,而用户可能
    /// 正在别的标签上工作,焦点被抢走。
    ///
    /// `id` 不变:任何缓存了这个 `TabId` 的地方(比如同一帧里的
    /// `UiActions::reconnect_tab`)仍然指得对。
    pub fn replace(&mut self, id: TabId, title: String, content: C) -> Option<Tab<C>> {
        let ix = self.tabs.iter().position(|t| t.id == id)?;
        let session_id = self.tabs[ix].session_id;
        let fresh = Tab {
            id,
            title,
            session_id,
            content,
        };
        Some(std::mem::replace(&mut self.tabs[ix], fresh))
    }

    pub fn close_active(&mut self) -> Option<Tab<C>> {
        if self.tabs.is_empty() {
            return None;
        }
        self.close(self.active)
    }

    /// 全部取走(退出时逐个收口用,Task 6)。取完即 launcher 态。
    pub fn drain(&mut self) -> Vec<Tab<C>> {
        self.active = 0;
        std::mem::take(&mut self.tabs)
    }

    /// `Ctrl+Tab`:环绕到下一个。
    pub fn switch_next(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
    }

    /// `Ctrl+Shift+Tab`:环绕到上一个。
    pub fn switch_prev(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
    }

    /// 按下标切换。越界即 no-op(不夹紧 —— 夹紧会让误触落到某个真实标签上)。
    pub fn switch_to_index(&mut self, ix: usize) {
        if ix < self.tabs.len() {
            self.active = ix;
        }
    }

    /// `Ctrl+1..9`。`n` 是 **1-based**;**`n == 9` 恒为最后一个标签**(与浏览器一致),
    /// 不是「第 9 个」—— 标签超过 9 个时「第 9 个」这个位置用户根本数不出来。
    /// `n` 为 0 或超出标签数即 no-op。
    pub fn switch_to_nth(&mut self, n: usize) {
        if self.tabs.is_empty() || n == 0 {
            return;
        }
        if n == 9 {
            self.active = self.tabs.len() - 1;
        } else if n <= self.tabs.len() {
            self.active = n - 1;
        }
    }
}

impl<C: TabPayload> Tabs<C> {
    /// S1:按世代号定位**属主标签**。
    ///
    /// 迟到事件一律走这里,**绝不用「活动标签」去接**:用户在标签 A 发起连接、
    /// 切到标签 B 的几百毫秒里 `PaneOpened` 抵达,拿活动标签接就会把 A 的 pane
    /// 挂到 B 上(adr-009 记的四条失效模式之一,多标签把它从「一个 ws 内」
    /// 放大到「跨标签」)。
    pub fn by_generation_mut(&mut self, generation: u64) -> Option<&mut Tab<C>> {
        self.tabs
            .iter_mut()
            .find(|t| t.content.generation() == generation)
    }

    pub fn by_generation(&self, generation: u64) -> Option<&Tab<C>> {
        self.tabs
            .iter()
            .find(|t| t.content.generation() == generation)
    }
}

/// 一次标签快捷键要做的事(S4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Next,
    Prev,
    CloseActive,
    /// `Ctrl+1..9`,1-based。语义同 [`Tabs::switch_to_nth`](9 = 最后一个)。
    Nth(usize),
}

/// 把一次按键翻译成标签意图。**纯函数** —— 与 `annotate::hotkey` 同一个套路:
/// 「哪些组合键属于标签栏」这件事必须能脱离 winit/egui 单测,否则只能靠人按。
///
/// 返回 `Some` 就意味着调用方要把这个键**吞掉**:既不喂 egui(T8),也不编码
/// 进 PTY —— 否则 `Ctrl+W` 会在切标签的同时把远端 shell 的前一个词删掉。
///
/// 只认 `ctrl` 这一路:`Alt+数字` 在 tmux 里有约定用法,`Super` 归 Windows。
/// **`Ctrl+Tab` 必须显式排除 alt/sup**,否则 `Ctrl+Alt+Tab`(Windows 的系统
/// 切换器)会被我们吃掉一半。
///
/// `modal_open` 为真时整段不生效:此时键盘归 egui(T8),表单里按 `Ctrl+W`
/// 该是「删前一个词」,不是「把背后的标签关了」。**这个闸门放在纯函数里而不是
/// 调用点**,是为了让它能被真正测出来 —— `App` 要 `EventLoopProxy` 才能构造,
/// 留在 `app.rs` 里就只剩源码结构守护那种弱断言。
pub fn hotkey(
    key: mullion_term::keymap::Key,
    mods: mullion_term::keymap::Mods,
    modal_open: bool,
) -> Option<Intent> {
    use mullion_term::keymap::Key;
    if modal_open || !mods.ctrl || mods.alt || mods.sup {
        return None;
    }
    match key {
        Key::Tab if mods.shift => Some(Intent::Prev),
        Key::Tab => Some(Intent::Next),
        // `Ctrl+W` 抢的是 bash 的 `^W`(删前一个词)。这是刻意的取舍:关标签
        // 是所有标签式客户端的通用肌肉记忆,而 `^W` 在 Claude Code 的 TUI 里
        // 本就不常用。**留待实机验收**——若确实碍事就改 `Ctrl+Shift+W`。
        Key::Char('w' | 'W') if !mods.shift => Some(Intent::CloseActive),
        // 数字键**不接受 shift**:`Ctrl+Shift+2` 之类在很多终端里是别的东西,
        // 而且用户按出它多半是想打字符不是切标签。
        Key::Char(c) if !mods.shift && c.is_ascii_digit() && c != '0' => {
            Some(Intent::Nth(c as usize - '0' as usize))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假内容:只有一个世代号。用它而不是真 `Workspace` 是本模块泛型化的全部目的
    /// —— 焦点转移是下标算术,拿真 `Workspace` 来测等于让 20 行 pane 构造代码
    /// 挡在断言前面。
    struct Fake(u64);

    impl TabPayload for Fake {
        fn generation(&self) -> u64 {
            self.0
        }
    }

    /// F37:`replace` 就地换内容 —— 位置、身份、活动下标全不动。
    ///
    /// 故意**不在活动标签上**做替换(活动是 2,替换的是 0):
    /// 「换掉的正好是活动那个」会让一份「其实是关掉再新开」的错误实现
    /// 也看起来正常(新开的会成为活动标签,恰好还是它)。
    ///
    /// 自证会变红:把 `replace` 改成 `close(ix)` + `open(..)`。
    #[test]
    fn replace_swaps_the_content_in_place_without_moving_anything() {
        let mut t = tabs_of(3);
        t.switch_to_index(2);
        let target = t.get(0).expect("有三个标签").id;
        let old = t
            .replace(target, "重连后".into(), Fake(999))
            .expect("这个 id 在表里");
        assert_eq!(
            old.content.generation(),
            100,
            "交还的应该是被换掉的那个旧内容"
        );
        assert_eq!(t.len(), 3, "标签数不该变");
        assert_eq!(t.active_index(), 2, "活动标签被抢走了 —— 用户正在看的那个");
        let now = t.get(0).expect("还在原位");
        assert_eq!(
            now.id, target,
            "身份变了 —— 缓存了这个 TabId 的地方全会指错"
        );
        assert_eq!(now.title, "重连后");
        assert_eq!(now.content.generation(), 999);
    }

    /// 拨号期间用户把这个标签关掉了 —— `replace` 必须报 `None`,不能顺手
    /// 开一个新的:用户明确关掉了它,连上之后又自己冒出来是最糟的表现。
    #[test]
    fn replacing_a_tab_that_is_already_gone_creates_nothing() {
        let mut t = tabs_of(2);
        let gone = t.get(1).expect("有两个").id;
        t.close(1);
        assert!(t.replace(gone, "x".into(), Fake(999)).is_none());
        assert_eq!(t.len(), 1, "replace 不该凭空造出标签");
    }

    /// 开 `n` 个标签,世代号依次为 `100, 101, …`(与下标错开,免得测试里
    /// 「下标 1」和「世代 1」互相掩盖)。
    fn tabs_of(n: usize) -> Tabs<Fake> {
        let mut t = Tabs::default();
        for i in 0..n {
            t.open(format!("t{i}"), None, Fake(100 + i as u64));
        }
        t
    }

    #[test]
    fn empty_is_launcher_state() {
        let t: Tabs<Fake> = Tabs::default();
        assert!(t.is_empty());
        assert!(t.active().is_none());
    }

    #[test]
    fn open_makes_the_new_tab_active() {
        let t = tabs_of(3);
        assert_eq!(t.active_index(), 2);
        assert_eq!(t.active().unwrap().title, "t2");
    }

    /// 假内容 2:揣着一份「连接」(`Arc<()>` 站位)。用它测的是**所有权**——
    /// `Arc::strong_count` 是「这份东西还在不在、被谁拿着」唯一能在无头环境里
    /// 观测到的量。
    struct Conn {
        gen_: u64,
        conn: std::sync::Arc<()>,
    }

    impl TabPayload for Conn {
        fn generation(&self) -> u64 {
            self.gen_
        }
    }

    /// **spec F36 的验收标准**:切换标签绝不碰任何 SSH 连接。
    ///
    /// 这是 F36 存在的全部理由 —— 在高延迟代理链路上重连一次要好几秒,而
    /// 「切回上一个标签」是每分钟都会发生的动作。切换实现里但凡混进一句
    /// 「顺手重建一下连接」,产品价值就没了,而且现象只有在真机高延迟下才明显。
    ///
    /// **验证边界(必须说清)**:这里断言的是「切换只动下标、连接的所有权分毫
    /// 不动」。真跑两条 SSH 连接的端到端版本在无头环境里做不出来 —— `App` 要
    /// `EventLoopProxy` 才能构造,拿不到窗口就没有事件循环。剩下那半(app.rs
    /// 的切换分支里没有任何建连调用)由
    /// `app::tests::tab_switching_never_reconnects` 从源码结构上守。
    ///
    /// 自证会变红:把 `switch_next` 改成「取出标签、重建 payload(新 `Arc`)、
    /// 放回去」,即模拟「切换 = 重连」。
    #[test]
    fn switching_tabs_does_not_touch_the_ssh_connections() {
        let a = std::sync::Arc::new(());
        let b = std::sync::Arc::new(());
        let mut t: Tabs<Conn> = Tabs::default();
        t.open(
            "A".into(),
            None,
            Conn {
                gen_: 1,
                conn: a.clone(),
            },
        );
        t.open(
            "B".into(),
            None,
            Conn {
                gen_: 2,
                conn: b.clone(),
            },
        );
        // 每个标签各持一份 + 测试自己手上那份 = 2。
        assert_eq!(std::sync::Arc::strong_count(&a), 2);
        assert_eq!(std::sync::Arc::strong_count(&b), 2);

        for i in 0..10 {
            match i % 4 {
                0 => t.switch_next(),
                1 => t.switch_prev(),
                2 => t.switch_to_nth(1),
                _ => t.switch_to_index(1),
            }
            assert_eq!(
                std::sync::Arc::strong_count(&a),
                2,
                "第 {i} 次切换动了 A 的连接 —— 切换只该改活动下标"
            );
            assert_eq!(
                std::sync::Arc::strong_count(&b),
                2,
                "第 {i} 次切换动了 B 的连接"
            );
        }
        // 连接**还是原来那两份**,不是「计数碰巧也是 2 的新对象」。
        assert!(std::sync::Arc::ptr_eq(&t.get(0).unwrap().content.conn, &a));
        assert!(std::sync::Arc::ptr_eq(&t.get(1).unwrap().content.conn, &b));
    }

    /// F36(Task 5):新连接**开新标签**,已有标签原样留着、连接一根不动。
    ///
    /// Task 2 的过渡实现是「连接替换活动标签」(单标签下与迁移前逐帧等价),
    /// 这条钉死它已经改掉了。
    #[test]
    fn opening_a_tab_keeps_the_previous_ones_live() {
        let a = std::sync::Arc::new(());
        let mut t: Tabs<Conn> = Tabs::default();
        t.open(
            "A".into(),
            None,
            Conn {
                gen_: 1,
                conn: a.clone(),
            },
        );
        t.open(
            "B".into(),
            None,
            Conn {
                gen_: 2,
                conn: std::sync::Arc::new(()),
            },
        );
        assert_eq!(t.len(), 2, "开新标签不该顶掉旧的");
        assert_eq!(t.active_index(), 1, "新标签成为活动标签");
        assert_eq!(
            std::sync::Arc::strong_count(&a),
            2,
            "开第二个标签动了第一个标签的连接"
        );
    }

    /// Task 6:`close` 把标签**交还给调用方**,调用方 drop 掉它才释放资源。
    /// 容器自己不 drop —— 收口顺序(先 abort 自动化再 drop pane)是 app 的事,
    /// 在容器里 drop 等于把顺序钉死在够不着运行时的地方。
    ///
    /// 同时钉住「只释放它自己那一份」:关 A 不该动 B 的连接。
    #[test]
    fn closing_a_tab_releases_only_its_own_connection() {
        let a = std::sync::Arc::new(());
        let b = std::sync::Arc::new(());
        let mut t: Tabs<Conn> = Tabs::default();
        t.open(
            "A".into(),
            None,
            Conn {
                gen_: 1,
                conn: a.clone(),
            },
        );
        t.open(
            "B".into(),
            None,
            Conn {
                gen_: 2,
                conn: b.clone(),
            },
        );
        let gone = t.close(0).expect("关得掉");
        assert_eq!(
            std::sync::Arc::strong_count(&a),
            2,
            "close 不该自己 drop 内容 —— 收口顺序归调用方"
        );
        drop(gone);
        assert_eq!(std::sync::Arc::strong_count(&a), 1, "调用方 drop 后才释放");
        assert_eq!(std::sync::Arc::strong_count(&b), 2, "关 A 动了 B 的连接");
    }

    /// Task 6:`drain`(退出时逐个收口)同样把全部标签交还调用方。
    #[test]
    fn drain_hands_every_tab_back_to_the_caller() {
        let a = std::sync::Arc::new(());
        let mut t: Tabs<Conn> = Tabs::default();
        t.open(
            "A".into(),
            None,
            Conn {
                gen_: 1,
                conn: a.clone(),
            },
        );
        let all = t.drain();
        assert_eq!(all.len(), 1);
        assert!(t.is_empty(), "drain 完即 launcher 态");
        assert_eq!(std::sync::Arc::strong_count(&a), 2, "drain 不自己 drop");
        drop(all);
        assert_eq!(std::sync::Arc::strong_count(&a), 1);
    }

    #[test]
    fn ids_are_never_reused_after_close() {
        let mut t = tabs_of(2);
        let first = t.get(0).unwrap().id;
        t.close(0);
        let fresh = t.open("new".into(), None, Fake(200));
        assert_ne!(fresh, first, "回收 id 会让缓存了 id 的地方张冠李戴");
    }

    #[test]
    fn close_active_moves_focus_to_the_right_neighbour() {
        let mut t = tabs_of(3);
        t.switch_to_index(1);
        t.close(1);
        assert_eq!(t.active().unwrap().title, "t2", "应该顶上来的是右邻");
    }

    #[test]
    fn close_active_falls_back_to_left_when_it_was_last() {
        let mut t = tabs_of(3);
        assert_eq!(t.active_index(), 2);
        t.close(2);
        assert_eq!(t.active().unwrap().title, "t1");
    }

    #[test]
    fn close_non_active_keeps_the_same_tab_focused() {
        let mut t = tabs_of(3);
        t.switch_to_index(2);
        let before = t.active().unwrap().id;
        t.close(0);
        // 下标从 2 变成了 1,但焦点还在同一个标签上 —— 这条扎的是
        // 「只删了元素、忘了修正活动下标」那类 bug。
        assert_eq!(t.active().unwrap().id, before);
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn close_returns_the_tab_for_the_caller_to_wind_down() {
        let mut t = tabs_of(2);
        let gone = t.close(0).expect("关掉的标签必须交还给调用方收口");
        assert_eq!(gone.title, "t0");
        assert!(t.close(9).is_none(), "越界应返回 None,不 panic");
    }

    #[test]
    fn close_last_returns_to_launcher() {
        let mut t = tabs_of(1);
        t.close(0);
        assert!(t.is_empty());
        assert!(t.active().is_none());
        assert!(t.close_active().is_none());
    }

    #[test]
    fn switch_wraps_around_in_both_directions() {
        let mut t = tabs_of(3);
        t.switch_next(); // 2 → 0
        assert_eq!(t.active_index(), 0);
        t.switch_prev(); // 0 → 2
        assert_eq!(t.active_index(), 2);
    }

    #[test]
    fn switch_to_9_means_last_tab() {
        let mut t = tabs_of(3);
        t.switch_to_index(0);
        t.switch_to_nth(9);
        assert_eq!(t.active_index(), 2, "Ctrl+9 是「最后一个」,不是「第 9 个」");

        let mut many = tabs_of(12);
        many.switch_to_index(0);
        many.switch_to_nth(9);
        assert_eq!(many.active_index(), 11);
        many.switch_to_nth(3);
        assert_eq!(many.active_index(), 2, "Ctrl+3 仍是第 3 个");
    }

    #[test]
    fn switch_to_nth_out_of_range_is_inert() {
        let mut t = tabs_of(3);
        t.switch_to_index(1);
        t.switch_to_nth(5); // 只有 3 个标签
        assert_eq!(t.active_index(), 1, "越界不该夹紧到某个真实标签上");
        t.switch_to_nth(0);
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn by_generation_finds_the_owner_tab_not_the_active_one() {
        let mut t = tabs_of(2);
        // 活动的是 t1(世代 101);拿 t0 的世代号来查,必须查到 t0。
        assert_eq!(t.active().unwrap().title, "t1");
        let owner = t.by_generation_mut(100).expect("世代 100 的属主是 t0");
        assert_eq!(owner.title, "t0");
        assert!(t.by_generation(999).is_none(), "过期世代必须查不到");
    }

    fn mods(ctrl: bool, shift: bool) -> mullion_term::keymap::Mods {
        mullion_term::keymap::Mods {
            ctrl,
            shift,
            alt: false,
            sup: false,
        }
    }

    #[test]
    fn ctrl_tab_cycles_and_shift_reverses() {
        use mullion_term::keymap::Key;
        assert_eq!(
            hotkey(Key::Tab, mods(true, false), false),
            Some(Intent::Next)
        );
        assert_eq!(
            hotkey(Key::Tab, mods(true, true), false),
            Some(Intent::Prev)
        );
    }

    #[test]
    fn ctrl_w_closes_and_ctrl_digits_jump() {
        use mullion_term::keymap::Key;
        assert_eq!(
            hotkey(Key::Char('w'), mods(true, false), false),
            Some(Intent::CloseActive)
        );
        assert_eq!(
            hotkey(Key::Char('W'), mods(true, false), false),
            Some(Intent::CloseActive),
            "大小写都要认 —— 用户按 Ctrl+Shift 之外的方式也可能送上大写"
        );
        assert_eq!(
            hotkey(Key::Char('1'), mods(true, false), false),
            Some(Intent::Nth(1))
        );
        assert_eq!(
            hotkey(Key::Char('9'), mods(true, false), false),
            Some(Intent::Nth(9))
        );
    }

    /// **裸键必须放行**。这是这组快捷键唯一的致命失效模式:`hotkey` 判宽了就会
    /// 吞掉本该进 PTY 的键,而现象是「远端某些键莫名其妙没反应」,极难定位。
    /// 裸 Tab 尤其要紧 —— 它是 Claude Code 里的补全键(T8 的邻居)。
    #[test]
    fn plain_keys_and_other_modifiers_are_left_alone() {
        use mullion_term::keymap::Key;
        assert_eq!(
            hotkey(Key::Tab, mods(false, false), false),
            None,
            "裸 Tab 是补全键"
        );
        assert_eq!(hotkey(Key::Tab, mods(false, true), false), None);
        assert_eq!(hotkey(Key::Char('w'), mods(false, false), false), None);
        assert_eq!(hotkey(Key::Char('1'), mods(false, false), false), None);
        assert_eq!(
            hotkey(Key::Char('0'), mods(true, false), false),
            None,
            "Ctrl+0 在浏览器里是「重置缩放」,不是切标签"
        );
        assert_eq!(
            hotkey(Key::Char('w'), mods(true, true), false),
            None,
            "Ctrl+Shift+W 留白 —— 若实机验收判定 Ctrl+W 抢了 bash 的 ^W 太碍事,\
             这就是备用位"
        );
        assert_eq!(hotkey(Key::Enter, mods(true, false), false), None);
    }

    /// `Ctrl+Alt+Tab` 是 Windows 的系统切换器,`Super` 归系统。吃掉它们一半,
    /// 现象是系统快捷键时灵时不灵。
    #[test]
    fn alt_and_super_combinations_are_left_to_the_system() {
        use mullion_term::keymap::{Key, Mods};
        let ctrl_alt = Mods {
            ctrl: true,
            shift: false,
            alt: true,
            sup: false,
        };
        let ctrl_sup = Mods {
            ctrl: true,
            shift: false,
            alt: false,
            sup: true,
        };
        assert_eq!(hotkey(Key::Tab, ctrl_alt, false), None);
        assert_eq!(hotkey(Key::Char('1'), ctrl_sup, false), None);
    }

    /// 模态开着(会话管理器 / 关于 / 主机密钥 / 粘贴确认)时整组失效。
    ///
    /// 此时键盘归 egui(T8)。会话管理器的表单里按 `Ctrl+W` 该是「删前一个词」,
    /// 按 `Ctrl+1` 该是输入或什么都不做 —— 绝不该把背后的标签关了 / 切了,
    /// 而用户在弹窗里根本看不见背后发生了什么。
    #[test]
    fn tab_shortcuts_are_inert_while_a_modal_is_open() {
        use mullion_term::keymap::Key;
        for (key, m) in [
            (Key::Tab, mods(true, false)),
            (Key::Tab, mods(true, true)),
            (Key::Char('w'), mods(true, false)),
            (Key::Char('3'), mods(true, false)),
        ] {
            assert_eq!(
                hotkey(key, m, true),
                None,
                "模态开着时 {key:?} 不该被标签栏吃掉"
            );
            assert!(
                hotkey(key, m, false).is_some(),
                "{key:?} 在无模态时本该命中,否则上面那条断言是空跑"
            );
        }
    }
}
