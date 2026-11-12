package io.github.kilobitcy.mullion

import android.app.Application
import android.net.Uri
import android.provider.OpenableColumns
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.FfiException
import io.github.kilobitcy.mullion.ffi.inspectPack
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** F346-a:选文件 →(需要时)口令 → 摘要。项目列表界面是 F348。 */
class ImportViewModel(app: Application) : AndroidViewModel(app) {
    var message by mutableStateOf("")
        private set

    /** 非 null = 包里有密文,正等口令。口令错时保留,让用户再输。 */
    var pending by mutableStateOf<String?>(null)
        private set

    var busy by mutableStateOf(false)
        private set

    fun picked(uri: Uri) = launchBusy {
        val text = withContext(Dispatchers.IO) { readPicked(uri) }
        if (text == null) {
            pending = null // 口令框属于上一个文件,别留着
            message = TOO_BIG
            return@launchBusy
        }
        if (withContext(Dispatchers.IO) { inspectPack(text) }.hasSecrets) {
            pending = text
            message = "请输入包口令"
        } else {
            finish(text, "")
        }
    }

    /** 超限返回 null。先看 SIZE 列,未知时用有界读取兜底。 */
    private fun readPicked(uri: Uri): String? {
        val cr = getApplication<Application>().contentResolver
        val size = cr.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)?.use {
            if (it.moveToFirst() && !it.isNull(0)) it.getLong(0) else null
        }
        if (size != null && size > MAX_PACK_BYTES) return null
        return cr.openInputStream(uri)!!.use { readBounded(it, MAX_PACK_BYTES) }?.toString(Charsets.UTF_8)
    }

    fun submit(password: String) {
        val text = pending ?: return
        launchBusy { finish(text, password) }
    }

    private suspend fun finish(text: String, password: String) {
        message = withContext(Dispatchers.IO) { PackImport.run(getApplication(), text, password) }
        pending = null
    }

    private fun launchBusy(block: suspend () -> Unit) {
        if (busy) return
        busy = true
        viewModelScope.launch {
            try {
                block()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val wrongPw = (e as? PackImport.FailedAfterReset)?.cause ?: e
                // 口令错:保留口令框让用户重输;其余失败结束本次导入
                if (wrongPw !is FfiException.WrongPassword) pending = null
                message = PackImport.failureText(e)
            } finally {
                busy = false
            }
        }
    }

    private companion object {
        // 桌面包通常 KB 级;读入后 ByteArray → String → uniffi 拷贝 → 解析各占一份,放大数倍
        const val MAX_PACK_BYTES = 16 * 1024 * 1024
        const val TOO_BIG = "文件太大,不像 .mullionpack"
    }
}
