package io.github.kilobitcy.mullion

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.CloudForm
import io.github.kilobitcy.mullion.ffi.resetCloudConfig
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** F346-b:云配置表单 + 「从云端更新」。手动拉,不自动(设计 D2)。 */
class CloudViewModel(app: Application) : AndroidViewModel(app) {
    // 构造时同步读一个 KB 级 TOML:异步加载会与用户/测试的首次输入竞态,被迟到的初值覆盖
    var form by mutableStateOf(CloudPull.load(app))
        private set
    var message by mutableStateOf(if (form.isCorrupt) "云配置损坏,请点「重置云配置」" else "")
        private set
    var busy by mutableStateOf(false)
        private set

    /** 结果世代号:只在 save/pull/reset 结束时递增,refresh 不动;UI 据此决定是否把结果滚入视野。 */
    var resultCount by mutableStateOf(0)
        private set

    // busy 时忽略:persist 之后的 reload 会把期间敲的字盖掉
    fun edit(f: CloudForm) { if (!busy) form = f }

    fun save() = launchBusy { persist(); if (message.isEmpty()) message = "已保存" }

    /** 先保存再拉:用户填完直接点更新,不该拿旧配置去拉。 */
    fun pull() = launchBusy {
        val reset = persist()
        try {
            message = withContext(Dispatchers.IO) {
                VaultLock.mutex.withLock { CloudPull.pull(getApplication()) }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // persist 时重置过:拉取失败也要带上「已清空」说明
            throw if (reset && e !is PackImport.FailedAfterReset) PackImport.FailedAfterReset(e) else e
        }
        if (reset) message = CloudPull.RESET_NOTE + message
    }

    /** 导入(尤其导入时发生了 Keystore 重置)之后重读磁盘:保留当前 message。 */
    fun refresh() = launchBusy(announces = false) {
        val keep = message
        reload(keep)
    }

    fun reset() = launchBusy {
        withContext(Dispatchers.IO) {
            VaultLock.mutex.withLock { resetCloudConfig(VaultKey.cloudDir(getApplication()).path) }
        }
        reload("云配置已重置")
    }

    /** 返回 true = 保存时发生了 Keystore 重置。 */
    private suspend fun persist(): Boolean {
        val reset = withContext(Dispatchers.IO) { VaultLock.mutex.withLock { CloudPull.save(getApplication(), form) } }
        reload(if (reset) CloudPull.RESET_NOTE else "")
        return reset
    }

    /** 存完重读:SK/口令框清空,只显示「已设置」。 */
    private suspend fun reload(msg: String) {
        form = withContext(Dispatchers.IO) { VaultLock.mutex.withLock { CloudPull.load(getApplication()) } }
        message = msg
    }

    private fun launchBusy(announces: Boolean = true, block: suspend () -> Unit) {
        if (busy) return
        busy = true
        viewModelScope.launch {
            try {
                block()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                message = CloudPull.failureText(e)
            } finally {
                if (announces) resultCount++
                busy = false
            }
        }
    }
}
