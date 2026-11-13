package io.github.kilobitcy.mullion

import android.content.pm.ActivityInfo
import android.content.res.Configuration
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.height
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** F346-c:未连接页布局(滚动区占满剩余高度、发送框仅连接后出现、结果提示滚入视野且不被无关刷新拉走)。 */
@RunWith(AndroidJUnit4::class)
class LayoutTest {
    @get:Rule val rule = createAndroidComposeRule<MainActivity>()

    /** 云表单可能被先跑的测试落盘过:先把 endpoint 清空,pull 才稳定得到「配置不完整」提示。 */
    private fun clearEndpointAndPull() {
        rule.onNodeWithTag("cloud-endpoint").performScrollTo().performTextReplacement("")
        rule.onNodeWithTag("cloud-pull").performScrollTo().performClick()
        rule.waitUntil(10_000) { rule.onAllNodes(hasTestTag("cloud-status")).fetchSemanticsNodes().isNotEmpty() }
    }

    /** 只是横屏可达性冒烟,不守护布局(旧布局在横屏也能滚到);布局回归由 formScrollFillsTheRemainingHeight 判。 */
    @Test
    fun cloudPullIsReachableInLandscape() {
        rule.activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        rule.waitUntil(10_000) {
            rule.activity.resources.configuration.orientation == Configuration.ORIENTATION_LANDSCAPE
        }
        rule.waitForIdle()
        rule.onNodeWithTag("cloud-pull").performScrollTo().assertIsDisplayed()
    }

    /** 竖屏:滚动区之下只有一行状态,所以状态行底边必须贴近根底边;平分高度回归时它会落在屏幕中部。 */
    @Test
    fun formScrollFillsTheRemainingHeight() {
        rule.waitForIdle()
        val root = rule.onRoot().getUnclippedBoundsInRoot()
        val scroll = rule.onNodeWithTag("form-scroll").getUnclippedBoundsInRoot()
        val status = rule.onNodeWithTag("status").getUnclippedBoundsInRoot()
        // 容差:8dp 外边距 + 系统栏/手势条
        assertTrue("状态行底边 ${status.bottom} 离根底边 ${root.bottom} 太远", status.bottom > root.bottom - 100.dp)
        assertTrue("滚动区只有 ${scroll.height},应占满剩余高度", scroll.height > root.height * 0.6f)
    }

    @Test
    fun noSendBoxBeforeConnecting() {
        rule.onNodeWithTag("input").assertDoesNotExist()
        rule.onNodeWithTag("send").assertDoesNotExist()
    }

    @Test
    fun cloudResultScrollsIntoViewByItself() {
        clearEndpointAndPull()
        // 不手动滚动:提示必须自己滚进视野
        rule.onNodeWithTag("cloud-status").assertIsDisplayed()
    }

    /** refresh(导入结束后调)只翻 busy、不改 message;视口不得被拉到旧的 cloud-status。 */
    @Test
    fun anUnrelatedRefreshDoesNotYankTheViewport() {
        clearEndpointAndPull()
        rule.onNodeWithTag("host").performScrollTo().assertIsDisplayed()
        val vm = ViewModelProvider(rule.activity)[CloudViewModel::class.java]
        rule.runOnUiThread { vm.refresh() }
        rule.waitUntil(10_000) { !vm.busy }
        rule.waitForIdle()
        rule.onNodeWithTag("host").assertIsDisplayed()
    }
}
