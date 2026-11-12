package io.github.kilobitcy.mullion

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.kilobitcy.mullion.ffi.ConnectParams
import io.github.kilobitcy.mullion.ffi.ScreenListener
import io.github.kilobitcy.mullion.ffi.Terminal
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * F345:在模拟器里经 JNA → libmullion_ffi.so → 真 SSH 连宿主机的 echo sshd
 * (`android/scripts/e2e.sh` 负责拉起)。地址可用 `-e host/port` 覆盖。
 */
@RunWith(AndroidJUnit4::class)
class FfiRoundTripTest {
    private val args = InstrumentationRegistry.getArguments()
    private val host = args.getString("host") ?: "10.0.2.2"
    private val port = (args.getString("port") ?: "2222").toUShort()

    @Test
    fun typedTextComesBackOnScreen() {
        val dirty = LinkedBlockingQueue<Unit>()
        val term = Terminal.connect(
            ConnectParams(host, port, "testuser", "test-password", 40u, 5u),
            object : ScreenListener {
                override fun onDirty() { dirty.offer(Unit) }
                override fun onClosed() {}
            },
        )
        try {
            term.write("hello".toByteArray())
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
            while (term.screen().rows[0] != "hello" && System.nanoTime() < deadline) {
                dirty.poll(100, TimeUnit.MILLISECONDS)
            }
            assertEquals("hello", term.screen().rows[0])
        } finally {
            term.close()
        }
    }
}
