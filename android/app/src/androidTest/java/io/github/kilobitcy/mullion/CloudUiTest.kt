package io.github.kilobitcy.mullion

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class CloudUiTest {
    @get:Rule val rule = createAndroidComposeRule<MainActivity>()
    private val endpoint = InstrumentationRegistry.getArguments().getString("s3") ?: "http://10.0.2.2:9000"

    @Before
    fun clean() {
        val ctx = InstrumentationRegistry.getInstrumentation().targetContext
        File(ctx.filesDir, VaultKey.KEY_FILE).delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        VaultKey.cloudDir(ctx).deleteRecursively()
    }

    private fun type(tag: String, text: String) =
        rule.onNodeWithTag(tag).performScrollTo().performTextReplacement(text)

    @Test
    fun fillTheFormPullAndSeeTheSummary() {
        type("cloud-endpoint", endpoint)
        // 默认 endpoint 是 http://:明文警告必须出现
        rule.onNodeWithTag("cloud-http-warning").assertExists()
        type("cloud-endpoint", "https://s3.example.com")
        rule.onNodeWithTag("cloud-http-warning").assertDoesNotExist()
        type("cloud-endpoint", endpoint)
        type("cloud-region", "us-east-1")
        type("cloud-bucket", "b")
        type("cloud-prefix", "mullion/")
        val sw = rule.onNodeWithTag("cloud-path-style").performScrollTo()
        val on = sw.fetchSemanticsNode().config[SemanticsProperties.ToggleableState] == ToggleableState.On
        if (!on) sw.performClick()
        type("cloud-socks5", "") // 残留配置不带进来
        type("cloud-ak", "AK")
        type("cloud-sk", "SK")
        type("cloud-pass", "demo-pack-password")
        rule.onNodeWithTag("cloud-pull").performScrollTo().performClick()
        rule.waitUntil(30_000) {
            rule.onAllNodes(hasText("演示项目", substring = true) and androidx.compose.ui.test.hasTestTag("cloud-status"))
                .fetchSemanticsNodes().isNotEmpty()
        }
    }
}
