package io.github.kilobitcy.mullion

import android.content.Context
import io.github.kilobitcy.mullion.ffi.CloudForm
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.loadCloudForm
import io.github.kilobitcy.mullion.ffi.pullLatest
import io.github.kilobitcy.mullion.ffi.saveCloudForm
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** F346-b:云配置存取与拉取。阻塞(网络 + Argon2 + IO),调用方放 IO 线程。 */
object CloudPull {
    private val AMZ = DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss'Z'").withZone(ZoneOffset.UTC)

    /** SigV4 的 x-amz-date。手机时间偏差由服务端 RequestTimeTooSkewed 报出(设计 D7)。 */
    fun amzDate(now: Instant = Instant.now()): String = AMZ.format(now)

    const val RESET_NOTE = PackImport.RESET_NOTE

    fun load(ctx: Context): CloudForm = loadCloudForm(VaultKey.cloudDir(ctx).path)

    /** 返回 true = 本次取钥时发生了重置(旧云配置已清,刚存的是重置后的第一份)。 */
    fun save(ctx: Context, form: CloudForm): Boolean {
        val key = VaultKey.load(ctx)
        saveCloudForm(VaultKey.cloudDir(ctx).path, key.dataKey, form)
        return key.reset
    }

    fun pull(ctx: Context, now: Instant = Instant.now()): String {
        val key = VaultKey.load(ctx)
        val s = try {
            pullLatest(VaultKey.vaultDir(ctx).path, VaultKey.cloudDir(ctx).path, key.dataKey, amzDate(now))
        } catch (e: Exception) {
            throw if (key.reset) PackImport.FailedAfterReset(e) else e
        }
        val head = if (key.reset) RESET_NOTE else ""
        return head + "已从云端 ${s.key.substringAfterLast('/')} 更新。" + PackImport.summaryText(s.import)
    }

    fun failureText(e: Exception): String {
        val cause = (e as? PackImport.FailedAfterReset)?.cause ?: e
        val body = when (cause) {
            is FfiException.WrongPassword -> "备份口令不对,请重填后再试"
            // Rust 文案是「包已损坏,请换一份:…」,对云端对象没法「换一份」:只留细节
            is FfiException.BadPack -> "云端备份无法解析:" + (cause.message ?: "").substringAfter("请换一份:")
            is FfiException.CloudAuth -> (cause.message ?: cause.toString()) + "(也检查 Region 与 path-style)"
            is FfiException -> cause.message ?: cause.toString()
            else -> "拉取失败(本机错误)"
        }
        return (if (e is PackImport.FailedAfterReset) RESET_NOTE else "") + body
    }
}
