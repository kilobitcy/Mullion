package io.github.kilobitcy.mullion

import android.content.pm.ApplicationInfo
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.IOException
import java.security.KeyStore
import java.security.KeyStoreException
import java.security.ProviderException
import java.security.UnrecoverableKeyException
import android.security.keystore.KeyPermanentlyInvalidatedException
import javax.crypto.AEADBadTagException

/** F346-a 设计 §5:Keystore 信封。 */
@RunWith(AndroidJUnit4::class)
class VaultKeyTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val keyFile get() = File(ctx.filesDir, VaultKey.KEY_FILE)

    private fun dropAlias() =
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(VaultKey.ALIAS)

    @Before
    fun clean() {
        keyFile.delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        dropAlias()
    }

    @Test
    fun theSameDataKeyComesBackAcrossLoads() {
        val a = VaultKey.load(ctx)
        val b = VaultKey.load(ctx)
        assertFalse(a.reset)
        assertFalse(b.reset)
        assertEquals(32, a.dataKey.size)
        assertArrayEquals(a.dataKey, b.dataKey)
    }

    @Test
    fun theKeyFileIsVersionedAndDoesNotContainThePlainKey() {
        val a = VaultKey.load(ctx)
        val raw = keyFile.readBytes()
        assertEquals(1, raw[0].toInt())
        assertEquals(1 + 12 + 32 + 16, raw.size)
        assertFalse(raw.toList().windowed(32).any { it.toByteArray().contentEquals(a.dataKey) })
    }

    @Test
    fun aLostAliasRegeneratesTheKeyAndClearsTheVault() {
        val a = VaultKey.load(ctx)
        VaultKey.vaultDir(ctx).mkdirs()
        File(VaultKey.vaultDir(ctx), "sessions.toml").writeText("x")
        dropAlias()

        val b = VaultKey.load(ctx)
        assertTrue(b.reset)
        assertFalse(a.dataKey.contentEquals(b.dataKey))
        assertFalse("旧密文用旧钥封的,再也解不开,必须清掉", VaultKey.vaultDir(ctx).exists())
        assertArrayEquals("重新生成后应稳定", b.dataKey, VaultKey.load(ctx).dataKey)
    }

    @Test
    fun anUnknownVersionByteIsTreatedAsLost() {
        VaultKey.load(ctx)
        val raw = keyFile.readBytes().also { it[0] = 2 }
        keyFile.writeBytes(raw)
        assertTrue(VaultKey.load(ctx).reset)
    }

    @Test
    fun onlyConfirmedPermanentLossCountsAsLost() {
        for (e in listOf(KeyStoreException("x"), ProviderException("x"), IOException("x"), IllegalStateException("x"))) {
            assertFalse("${e::class.simpleName} 是暂时性/未知故障,不能清数据", VaultKey.isPermanentLoss(e))
        }
        for (e in listOf(
            UnrecoverableKeyException("x"),
            KeyPermanentlyInvalidatedException("x"),
            AEADBadTagException("x"),
            VaultKey.PermanentLoss("x"),
        )) {
            assertTrue("${e::class.simpleName} 应判永久失效", VaultKey.isPermanentLoss(e))
        }
    }

    @Test
    fun aTruncatedKeyFileIsTreatedAsLost() {
        VaultKey.load(ctx)
        keyFile.writeBytes(byteArrayOf(1, 2, 3))
        assertTrue(VaultKey.load(ctx).reset)
    }

    @Test
    fun backupIsDisabled() {
        // 系统备份会带走 vault-key.bin 而带不走 Keystore 钥。
        assertEquals(0, ctx.applicationInfo.flags and ApplicationInfo.FLAG_ALLOW_BACKUP)
    }
}
