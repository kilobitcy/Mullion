package io.github.kilobitcy.mullion

import kotlinx.coroutines.sync.Mutex

/**
 * F346-b:vault/ 与 cloud/ 的写入串行化。导入(ImportViewModel)与云端拉取(CloudViewModel)
 * 各有自己的 busy 闸,互相看不见;Keystore 重置时 VaultKey.load 还会整目录删除,
 * 所以两边的阻塞段都得在这把锁里跑。
 */
object VaultLock {
    val mutex = Mutex()
}
