package io.github.kilobitcy.mullion

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.ImportSummary
import io.github.kilobitcy.mullion.ffi.inspectPack
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayInputStream
import java.io.File
import java.security.KeyStore

/**
 * F346-a:端到端导入 Rust example 生成的演示包(`crates/mullion-ffi/tests/common/demo_pack.rs`)。
 * 预期:项目「演示项目」,保留 3 份凭据,丢弃 1 份。
 */
@RunWith(AndroidJUnit4::class)
class PackImportTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val pack = InstrumentationRegistry.getInstrumentation().context.assets
        .open("test.mullionpack").bufferedReader().use { it.readText() }

    @Before
    fun clean() {
        File(ctx.filesDir, VaultKey.KEY_FILE).delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        VaultKey.cloudDir(ctx).deleteRecursively()
    }

    @Test
    fun inspectSeesThatThePackNeedsAPassword() {
        assertTrue(inspectPack(pack).hasSecrets)
    }

    @Test
    fun importKeepsOnlyProjectReachableSecrets() {
        val text = PackImport.run(ctx, pack, "demo-pack-password")
        assertEquals("已导入 1 个项目:演示项目;保留 3 份凭据,丢弃 1 份", text)
        assertEquals(
            setOf("sessions.toml", "known_hosts.toml", "secrets.enc"),
            VaultKey.vaultDir(ctx).list()!!.toSet(),
        )
    }

    @Test
    fun aWrongPasswordIsTyped() {
        assertThrows(FfiException.WrongPassword::class.java) {
            PackImport.run(ctx, pack, "nope")
        }
    }

    @Test
    fun theSummaryMentionsBrokenNodesAndSkippedEntries() {
        val t = PackImport.summaryText(ImportSummary(listOf("甲"), 1u, 0u, 2u, 3u))
        assertTrue(t, t.contains("2 个节点"))
        assertTrue(t, t.contains("跳过 3 条"))
    }

    @Test
    fun aWrongPasswordAfterAResetStillTellsTheUserTheOldDataIsGone() {
        PackImport.run(ctx, pack, "demo-pack-password")
        VaultKey.cloudDir(ctx).mkdirs()
        File(VaultKey.cloudDir(ctx), "marker").writeText("x")
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(VaultKey.ALIAS)

        val e = assertThrows(PackImport.FailedAfterReset::class.java) {
            PackImport.run(ctx, pack, "nope")
        }
        assertTrue(e.cause is FfiException.WrongPassword)
        val t = PackImport.failureText(e)
        assertTrue(t, t.contains("旧数据与云配置已清空"))
        // 重置同时清掉云配置目录(R5:VaultKey.load 的行为)
        assertTrue(!VaultKey.cloudDir(ctx).exists())
        // 没 reset 的口令错:提示不带清空说明
        assertTrue(!PackImport.failureText(FfiException.WrongPassword("口令不对,再输一次")).contains("已清空"))
    }

    @Test
    fun boundedReadStopsAtTheLimit() {
        val data = ByteArray(10) { it.toByte() }
        assertArrayEquals(data, readBounded(ByteArrayInputStream(data), 10))
        assertNull(readBounded(ByteArrayInputStream(data), 9))
        assertArrayEquals(ByteArray(0), readBounded(ByteArrayInputStream(ByteArray(0)), 0))
    }
}
