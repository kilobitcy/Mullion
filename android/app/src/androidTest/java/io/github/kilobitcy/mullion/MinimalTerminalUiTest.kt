package io.github.kilobitcy.mullion

import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextReplacement
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MinimalTerminalUiTest {
    @get:Rule val rule = createAndroidComposeRule<MainActivity>()

    private val args = InstrumentationRegistry.getArguments()

    @Test
    fun connectTypeAndSeeTheEcho() {
        rule.onNodeWithTag("host").performTextReplacement(args.getString("host") ?: "10.0.2.2")
        rule.onNodeWithTag("port").performTextReplacement(args.getString("port") ?: "2222")
        rule.onNodeWithTag("user").performTextReplacement("testuser")
        rule.onNodeWithTag("password").performTextReplacement("test-password")
        rule.onNodeWithTag("connect").performClick()

        rule.waitUntil(10_000) {
            rule.onAllNodes(hasText("已连接")).fetchSemanticsNodes().isNotEmpty()
        }
        rule.onNodeWithTag("input").performTextInput("hi-from-ui")
        rule.onNodeWithTag("send").performClick()
        rule.waitUntil(10_000) {
            rule.onAllNodes(hasText("hi-from-ui", substring = true) and hasTestTagRow())
                .fetchSemanticsNodes().isNotEmpty()
        }
    }
}

private fun hasTestTagRow() =
    androidx.compose.ui.test.SemanticsMatcher("终端行") {
        it.config.getOrNull(androidx.compose.ui.semantics.SemanticsProperties.TestTag)
            ?.startsWith("row-") == true
    }
