package io.github.kilobitcy.mullion

import android.content.Context
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.ImportSummary
import io.github.kilobitcy.mullion.ffi.importPack

/** F346-a:取数据密钥 → 导入 → 摘要文本。耗时(Argon2 + IO),调用方放 IO 线程。 */
object PackImport {
    fun run(ctx: Context, text: String, password: String): String {
        val key = VaultKey.load(ctx)
        val summary = try {
            importPack(VaultKey.vaultDir(ctx).path, text, password, key.dataKey)
        } catch (e: Exception) {
            // 旧数据在 load 里已清掉,失败也得让用户知道;口令错等语义由 cause 保留
            throw if (key.reset) FailedAfterReset(e) else e
        }
        val head = if (key.reset) RESET_NOTE else ""
        return head + summaryText(summary)
    }

    /** 导入与云拉取两条路径共用:VaultKey.load 重置会清 vault 与 cloud 两处。 */
    const val RESET_NOTE = "本机密钥已失效,旧数据与云配置已清空,请重新导入或重新填写云配置。\n"

    /** 密钥重建(旧数据已清)之后导入又失败。[cause] 是真正的失败原因。 */
    class FailedAfterReset(cause: Exception) : Exception(cause.message, cause)

    /** 失败提示:口令错保持原文案(口令框保留),重建过则前缀清空说明。 */
    fun failureText(e: Exception): String {
        val cause = (e as? FailedAfterReset)?.cause ?: e
        val body = if (cause is FfiException.WrongPassword) cause.message ?: "口令不对" else "导入失败:${cause.message}"
        return (if (e is FailedAfterReset) RESET_NOTE else "") + body
    }

    fun summaryText(s: ImportSummary): String = buildString {
        append("已导入 ${s.projects.size} 个项目")
        if (s.projects.isNotEmpty()) append(":" + s.projects.joinToString("、"))
        append(";保留 ${s.kept} 份凭据,丢弃 ${s.dropped} 份")
        if (s.brokenNodes > 0u) append("\n${s.brokenNodes} 个节点的跳板链解析失败,只导入了节点本身的凭据")
        if (s.unrecognized > 0u) append("\n跳过 ${s.unrecognized} 条无法识别的条目")
    }
}

/** 读到 [max] 字节为止;超过 [max] 返回 null(多读 1 字节判断,不会把大文件读进内存)。 */
fun readBounded(input: java.io.InputStream, max: Int): ByteArray? {
    val out = java.io.ByteArrayOutputStream()
    val buf = ByteArray(8192)
    var total = 0
    while (true) {
        val n = input.read(buf)
        if (n < 0) return out.toByteArray()
        total += n
        if (total > max) return null
        out.write(buf, 0, n)
    }
}
