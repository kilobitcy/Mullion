package io.github.kilobitcy.mullion

import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * F348~F350 端到端:导入 `open.mullionpack`(节点 = 宿主机 `10.0.2.2:2223`,开 shell 先吐 `$ `
 * 的 echo sshd)→ 列表点开 → 首次见主机弹窗确认 → attach 计划回显上屏。
 * 资产生成见 `crates/mullion-ffi/examples/make_test_pack.rs`。
 */
@RunWith(AndroidJUnit4::class)
class OpenProjectTest {
    @get:Rule val rule = createEmptyComposeRule()

    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private var scenario: ActivityScenario<MainActivity>? = null

    @Before
    fun importPack() {
        File(ctx.filesDir, VaultKey.KEY_FILE).delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        VaultKey.cloudDir(ctx).deleteRecursively()
        val pack = InstrumentationRegistry.getInstrumentation().context.assets
            .open("open.mullionpack").bufferedReader().use { it.readText() }
        PackImport.run(ctx, pack, "demo-pack-password")
        // 导入完再起界面:列表在 ViewModel 构造时读盘
        scenario = ActivityScenario.launch(MainActivity::class.java)
    }

    @After
    fun close() {
        scenario?.close()
        // 别把项目留给后面的测试:手动表单会被挤到列表下面
        VaultKey.vaultDir(ctx).deleteRecursively()
    }

    @Test
    fun tapAProjectConfirmTheHostAndSeeTheAttachPlan() {
        rule.waitUntil(10_000) { rule.onAllNodes(hasText("演示项目")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithText("演示项目").performClick()

        rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("hostkey-trust")).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("hostkey-text").assertExists()
        rule.onNodeWithTag("hostkey-trust").performClick()

        rule.waitUntil(10_000) { rule.onAllNodes(hasText("已连接", substring = true)).fetchSemanticsNodes().isNotEmpty() }
        rule.onNodeWithTag("disconnect").assertExists()
        rule.waitUntil(10_000) {
            rule.onAllNodes(hasText("tmux has-session", substring = true)).fetchSemanticsNodes().isNotEmpty()
        }

        // 「断开」回列表
        rule.onNodeWithTag("disconnect").performClick()
        rule.waitUntil(10_000) { rule.onAllNodes(hasText("演示项目")).fetchSemanticsNodes().isNotEmpty() }
    }
}
