package io.github.kilobitcy.mullion

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.ConnectParams
import io.github.kilobitcy.mullion.ffi.OpenOutcome
import io.github.kilobitcy.mullion.ffi.PendingHostKey
import io.github.kilobitcy.mullion.ffi.ScreenListener
import io.github.kilobitcy.mullion.ffi.Terminal
import io.github.kilobitcy.mullion.ffi.trustHostKey
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * F345 最小终端的状态。**临时形态**:F351 换成 Canvas 绘制 + Choreographer 节流,
 * F352 换成本地编辑框 + 辅助键栏;这里只证明「连得上、看得见、发得出」。
 */
class TerminalViewModel(app: Application) : AndroidViewModel(app) {
    var status by mutableStateOf("未连接")
        private set
    var rows by mutableStateOf(emptyList<String>())
        private set

    /** 终端区与发送框只在此为真时出现。断线 / 「断开」即回列表。 */
    var connected by mutableStateOf(false)
        private set

    /** F349:当前项目连到了哪个节点(手动连接时为空)。 */
    var node by mutableStateOf("")
        private set

    /** F350:等用户确认的主机密钥;非 null 时弹窗。 */
    var pendingKey by mutableStateOf<PendingHostKey?>(null)
        private set
    private var pendingProject: ULong? = null

    private var term: Terminal? = null
    private var busy = false

    /** 界面按可用宽高算出的网格;打开时用它,变了就 resize(T4)。 */
    private var cols: UShort = 80u
    private var lines: UShort = 24u

    fun connect(host: String, port: String, user: String, password: String) {
        val p = port.toUShortOrNull() ?: run { status = "端口无效"; return }
        dial { l -> Terminal.connect(ConnectParams(host, p, user, password, cols, lines), l) }
    }

    /** F349。遇到没见过的主机先弹窗,确认后由 [trustPending] 重开。 */
    fun openProject(id: ULong) {
        val ctx = getApplication<Application>()
        dial { l ->
            val outcome = VaultLock.mutex.withLock {
                val key = VaultKey.load(ctx)
                if (key.reset) throw IllegalStateException(PackImport.RESET_NOTE.trim())
                io.github.kilobitcy.mullion.ffi.openProject(VaultKey.vaultDir(ctx).path, key.dataKey, id, cols, lines, l)
            }
            when (outcome) {
                is OpenOutcome.Opened -> { node = outcome.node; outcome.terminal }
                is OpenOutcome.UnknownHostKey -> {
                    pendingProject = id
                    pendingKey = outcome.key
                    null
                }
            }
        }
    }

    fun trustPending() {
        val key = pendingKey ?: return
        val id = pendingProject ?: return
        pendingKey = null
        pendingProject = null
        val dir = VaultKey.vaultDir(getApplication()).path
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) { trustHostKey(dir, key) }
                openProject(id)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                status = "记录主机密钥失败:${e.message}"
            }
        }
    }

    fun rejectPending() {
        pendingKey = null
        pendingProject = null
        status = "已取消:未信任该主机"
    }

    /**
     * 连接公共流程。[make] 在 IO 线程上跑,返回 null = 没连上但也不是错误(等用户确认指纹)。
     * 连接中或已连上再点一次:不再开第二条,否则旧的那条没人关。
     */
    private fun dial(make: suspend (ScreenListener) -> Terminal?) {
        if (term != null || busy) return
        busy = true
        status = "连接中…"
        node = ""
        val l = Listener(++generation)
        viewModelScope.launch {
            var made: Terminal? = null
            try {
                made = withContext(Dispatchers.IO) { make(l) }
                if (made == null) {
                    status = "请确认主机密钥"
                    return@launch
                }
                term = made
                refresh()
                connected = true
                status = "已连接"
            } catch (e: CancellationException) {
                // 连上的同时 ViewModel 被清:刚建好的这条还没交给 term,得在这里关。
                made?.close()
                throw e
            } catch (e: Exception) {
                status = "连接失败:${e.message}"
            } finally {
                busy = false
            }
        }
    }

    /** 界面量出新网格。连着就同步给远端(T4);没连着只记下,下次打开用。 */
    fun gridChanged(c: Int, r: Int) {
        if (c <= 0 || r <= 0) return
        val nc = c.coerceAtMost(500).toUShort()
        val nr = r.coerceAtMost(300).toUShort()
        if (nc == cols && nr == lines) return
        cols = nc
        lines = nr
        val t = term ?: return
        try {
            t.resize(nc, nr)
            refresh()
        } catch (e: Exception) {
            status = "调整尺寸失败:${e.message}"
        }
    }

    /** 快捷键栏:原样发字节,不补回车。 */
    fun sendRaw(bytes: ByteArray) {
        val t = term ?: return
        try {
            t.write(bytes)
        } catch (e: Exception) {
            status = "发送失败:${e.message}"
        }
    }

    /** 「断开」:关连接、回列表。 */
    fun disconnect() {
        generation++ // 这条连接还在路上的回调一律作废
        term?.close()
        term = null
        connected = false
        rows = emptyList()
        status = "未连接"
    }

    fun send(text: String) {
        val t = term ?: return
        try {
            t.write((text + "\r").toByteArray())
        } catch (e: Exception) {
            status = "发送失败:${e.message}"
        }
    }

    private fun refresh() {
        term?.let { rows = it.screen().rows }
    }

    /** 每次拨号加一。旧连接已投递、尚未执行的回调凭它认出自己过期了。 */
    private var generation = 0

    // 回调在 Rust 工作线程上:只把刷新切回主线程,不在这里做重活。
    // 每条连接一个实例:共享一个的话,旧连接迟到的 onClosed 会把刚开的新连接断掉。
    private inner class Listener(private val gen: Int) : ScreenListener {
        override fun onDirty() {
            viewModelScope.launch(Dispatchers.Main) { if (gen == generation) refresh() }
        }

        override fun onClosed() {
            viewModelScope.launch(Dispatchers.Main) {
                // 过期连接,或用户自己点了断开
                if (gen != generation || term == null) return@launch
                disconnect()
                status = "连接已断开"
            }
        }
    }

    override fun onCleared() {
        term?.close()
        term = null
    }
}
