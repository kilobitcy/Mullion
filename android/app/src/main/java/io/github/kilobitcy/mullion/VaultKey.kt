package io.github.kilobitcy.mullion

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import java.security.UnrecoverableKeyException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * F346-a 设计 D3:信封。AndroidKeyStore 里一把不可导出的 AES-256-GCM 包装钥,
 * 包住一把随机 32 字节数据密钥;Rust 侧拿数据密钥当 `InMemoryKey` 封 `secrets.enc`。
 *
 * `vault-key.bin` = 版本(1 字节) ‖ iv(12) ‖ 密文。版本字节给 F347 留路:Keystore 密钥
 * 生成后参数不可改,加认证要求只能换新 alias、解旧包、重包。
 *
 * 已知代价(F347 前):数据密钥以 ByteArray 过 JNA,Kotlin 堆与 JNA 缓冲各留一份,不零化。
 */
object VaultKey {
    const val ALIAS = "mullion-vault-wrap-v1"
    const val KEY_FILE = "vault-key.bin"
    const val VAULT_DIR = "vault"
    const val CLOUD_DIR = "cloud"
    private const val VERSION: Byte = 1
    private const val IV_LEN = 12
    private const val TAG_BITS = 128

    /** [reset] = 本机原有的密钥解不开,旧数据已清空,需要重新导入。 */
    class Loaded(val dataKey: ByteArray, val reset: Boolean)

    fun vaultDir(ctx: Context) = File(ctx.filesDir, VAULT_DIR)

    /** F346-b:云配置在 vault/ 之外(设计 D5),但 SK/口令用同一把数据密钥封,钥没了一起清。 */
    fun cloudDir(ctx: Context) = File(ctx.filesDir, CLOUD_DIR)

    /** 确认永久失效(密钥再也解不开这份数据):alias 不在、文件版本/长度不对、解出长度不对。 */
    class PermanentLoss(msg: String) : Exception(msg)

    /**
     * 只有确认永久失效的才算丢钥;其余(Keystore 暂时不可用、IO 故障等)一律不是,
     * 调用方须原样抛出,不得清数据、不得删 alias。按类型判断,不看 message。
     */
    fun isPermanentLoss(e: Throwable): Boolean = e is PermanentLoss ||
        e is UnrecoverableKeyException ||
        e is KeyPermanentlyInvalidatedException ||
        e is AEADBadTagException

    @Synchronized
    fun load(ctx: Context): Loaded {
        val file = File(ctx.filesDir, KEY_FILE)
        if (file.exists()) {
            try {
                return Loaded(unwrap(file.readBytes()), reset = false)
            } catch (e: Exception) {
                if (!isPermanentLoss(e)) throw e // 暂时性故障:保住旧数据,让用户稍后重试
            }
        }
        // 无钥,或确认永久失效(alias 丢了 / 系统还原 / 清除凭据存储):旧密文再也解不开,
        // 连同 vault/ 一起清掉,重导即恢复(设计 D4)。
        val reset = file.exists()
        file.delete()
        vaultDir(ctx).deleteRecursively()
        cloudDir(ctx).deleteRecursively() // 云配置的密文也解不开了,一起清
        if (reset) keyStore().deleteEntry(ALIAS)

        val key = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val tmp = File(ctx.filesDir, "$KEY_FILE.tmp")
        tmp.writeBytes(wrap(key))
        check(tmp.renameTo(file)) { "写 $KEY_FILE 失败" }
        return Loaded(key, reset)
    }

    private fun keyStore() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private fun wrappingKey(): SecretKey {
        (keyStore().getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    private fun wrap(dataKey: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, wrappingKey()) // IV 由 Keystore 随机生成
        val iv = c.iv
        check(iv.size == IV_LEN) { "意外的 IV 长度 ${iv.size}" }
        return byteArrayOf(VERSION) + iv + c.doFinal(dataKey)
    }

    private fun unwrap(blob: ByteArray): ByteArray {
        if (blob.size <= 1 + IV_LEN || blob[0] != VERSION) throw PermanentLoss("$KEY_FILE 版本或长度不对")
        val key = keyStore().getKey(ALIAS, null) as? SecretKey ?: throw PermanentLoss("包装钥不在了")
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, blob, 1, IV_LEN))
        val out = c.doFinal(blob, 1 + IV_LEN, blob.size - 1 - IV_LEN)
        if (out.size != 32) throw PermanentLoss("数据密钥长度不对")
        return out
    }
}
