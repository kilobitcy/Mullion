package io.github.kilobitcy.mullion

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.ProjectRow
import io.github.kilobitcy.mullion.ffi.listProjects
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.util.TimeZone

/** F348:项目列表 + 搜索。排序/筛选判据全在 Rust(`project_view::rows`)。 */
class ProjectsViewModel(app: Application) : AndroidViewModel(app) {
    var query by mutableStateOf("")
        private set
    var rows by mutableStateOf(emptyList<ProjectRow>())
        private set
    var message by mutableStateOf("")
        private set

    init { refresh() }

    fun search(q: String) { query = q; refresh() }

    /** 导入 / 云端拉取结束后由界面调用。 */
    fun refresh() {
        val q = query
        viewModelScope.launch {
            try {
                val (list, reset) = withContext(Dispatchers.IO) {
                    VaultLock.mutex.withLock { load(q) }
                }
                // 搜索词在途中变了:这份结果已过期,等后一次
                if (q != query) return@launch
                rows = list
                message = if (reset) PackImport.RESET_NOTE.trim() else ""
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                rows = emptyList()
                message = "读取项目失败:${e.message}"
            }
        }
    }

    private fun load(q: String): Pair<List<ProjectRow>, Boolean> {
        val ctx = getApplication<Application>()
        // 还没导入过:别为了列一个空表去生成数据密钥
        if (!VaultKey.vaultDir(ctx).exists()) return emptyList<ProjectRow>() to false
        val key = VaultKey.load(ctx)
        if (key.reset) return emptyList<ProjectRow>() to true
        val offset = TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 1000
        return listProjects(VaultKey.vaultDir(ctx).path, key.dataKey, q, offset) to false
    }
}
