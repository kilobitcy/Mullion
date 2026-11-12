package io.github.kilobitcy.mullion

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.kilobitcy.mullion.ffi.CloudForm
import io.github.kilobitcy.mullion.ffi.FfiException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.security.KeyStore
import java.time.Instant

/**
 * F346-b:端到端拉取宿主机假 S3(`examples/fake_s3_pack.rs`,`android/scripts/e2e.sh` 负责拉起)
 * 回放的演示包。地址可用 `-e s3 http://...` 覆盖。
 */
@RunWith(AndroidJUnit4::class)
class CloudPullTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val endpoint = InstrumentationRegistry.getArguments().getString("s3") ?: "http://10.0.2.2:9000"

    private fun form(pass: String) = CloudForm(
        endpoint = endpoint, region = "us-east-1", bucket = "b", prefix = "mullion/",
        pathStyle = true, socks5 = "", accessKeyId = "AK", secretKey = "SK", passphrase = pass,
        hasSecret = false, hasPassphrase = false, isCorrupt = false,
    )

    @Before
    fun clean() {
        File(ctx.filesDir, VaultKey.KEY_FILE).delete()
        VaultKey.vaultDir(ctx).deleteRecursively()
        VaultKey.cloudDir(ctx).deleteRecursively()
    }

    @Test
    fun amzDateIsUtcBasicFormat() {
        assertEquals("20261112T010203Z", CloudPull.amzDate(Instant.parse("2026-11-12T01:02:03Z")))
    }

    @Test
    fun pullInstallsTheDemoProject() {
        CloudPull.save(ctx, form("demo-pack-password"))
        val text = CloudPull.pull(ctx)
        assertTrue(text, text.contains("000001-20261112T000000Z.mpk"))
        assertTrue(text, text.contains("演示项目"))
        assertEquals(
            setOf("sessions.toml", "known_hosts.toml", "secrets.enc"),
            VaultKey.vaultDir(ctx).list()!!.toSet(),
        )
    }

    @Test
    fun aWrongPassphraseIsTypedAndToldAsTheBackupPassphrase() {
        CloudPull.save(ctx, form("nope"))
        val e = assertThrows(FfiException.WrongPassword::class.java) { CloudPull.pull(ctx) }
        assertTrue(CloudPull.failureText(e).contains("备份口令不对"))
    }

    @Test
    fun savedSecretsAreNotEchoedBack() {
        CloudPull.save(ctx, form("demo-pack-password"))
        val back = CloudPull.load(ctx)
        assertTrue(back.hasSecret && back.hasPassphrase)
        assertEquals("", back.secretKey)
        assertEquals("", back.passphrase)
    }

    @Test
    fun aKeystoreResetAlsoClearsTheCloudConfig() {
        CloudPull.save(ctx, form("demo-pack-password"))
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(VaultKey.ALIAS)
        val e = assertThrows(PackImport.FailedAfterReset::class.java) { CloudPull.pull(ctx) }
        assertTrue(e.cause is FfiException.CloudIncomplete)
        assertTrue(CloudPull.failureText(e).contains("云配置"))
        assertFalse(CloudPull.load(ctx).hasSecret)
    }
}
