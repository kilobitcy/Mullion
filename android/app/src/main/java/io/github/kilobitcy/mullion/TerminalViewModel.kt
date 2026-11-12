package io.github.kilobitcy.mullion

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.ConnectParams
import io.github.kilobitcy.mullion.ffi.ScreenListener
import io.github.kilobitcy.mullion.ffi.Terminal
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * F345 最小终端的状态。**临时形态**:F351 换成 Canvas 绘制 + Choreographer 节流,
 * F352 换成本地编辑框 + 辅助键栏;这里只证明「连得上、看得见、发得出」。
 */
class TerminalViewModel : ViewModel() {
    var status by mutableStateOf("未连接")
        private set
    var rows by mutableStateOf(emptyList<String>())
        private set

    private var term: Terminal? = null

    fun connect(host: String, port: String, user: String, password: String) {
        // 连接中或已连上再点一次:不再开第二条,否则旧的那条没人关。
        if (term != null || status == "连接中…") return
        val p = port.toUShortOrNull() ?: run { status = "端口无效"; return }
        status = "连接中…"
        viewModelScope.launch {
            var made: Terminal? = null
            try {
                withContext(Dispatchers.IO) {
                    made = Terminal.connect(ConnectParams(host, p, user, password, 80u, 24u), listener)
                }
                term = made
                refresh()
                status = "已连接"
            } catch (e: CancellationException) {
                // 连上的同时 ViewModel 被清:刚建好的这条还没交给 term,得在这里关。
                made?.close()
                throw e
            } catch (e: Exception) {
                status = "连接失败:${e.message}"
            }
        }
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

    // 回调在 Rust 工作线程上:只把刷新切回主线程,不在这里做重活。
    private val listener = object : ScreenListener {
        override fun onDirty() {
            viewModelScope.launch(Dispatchers.Main) { refresh() }
        }

        override fun onClosed() {
            viewModelScope.launch(Dispatchers.Main) { status = "连接已断开" }
        }
    }

    override fun onCleared() {
        term?.close()
        term = null
    }
}
