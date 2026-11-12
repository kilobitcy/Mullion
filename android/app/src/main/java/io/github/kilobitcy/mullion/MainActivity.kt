package io.github.kilobitcy.mullion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    private val vm: TerminalViewModel by viewModels()
    private val importVm: ImportViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) { Screen(vm, importVm) }
            }
        }
    }
}

@Composable
private fun Screen(vm: TerminalViewModel, importVm: ImportViewModel) {
    var host by remember { mutableStateOf("") }
    var port by remember { mutableStateOf("22") }
    var user by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var input by remember { mutableStateOf("") }

    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding().padding(8.dp)) {
        if (vm.status != "已连接") {
            OutlinedTextField(host, { host = it }, Modifier.fillMaxWidth().testTag("host"), label = { Text("主机") })
            OutlinedTextField(port, { port = it }, Modifier.fillMaxWidth().testTag("port"), label = { Text("端口") })
            OutlinedTextField(user, { user = it }, Modifier.fillMaxWidth().testTag("user"), label = { Text("用户") })
            OutlinedTextField(
                password, { password = it }, Modifier.fillMaxWidth().testTag("password"),
                label = { Text("密码") }, visualTransformation = PasswordVisualTransformation(),
            )
            Button({ vm.connect(host, port, user, password) }, Modifier.testTag("connect")) { Text("连接") }
            ImportSection(importVm)
        }
        Text(vm.status, Modifier.testTag("status"))
        Column(Modifier.weight(1f).horizontalScroll(rememberScrollState())) {
            vm.rows.forEachIndexed { i, line ->
                Text(line, Modifier.testTag("row-$i"), fontFamily = FontFamily.Monospace, fontSize = 11.sp, softWrap = false)
            }
        }
        Row {
            OutlinedTextField(input, { input = it }, Modifier.weight(1f).testTag("input"))
            Button({ vm.send(input); input = "" }, Modifier.testTag("send")) { Text("发送") }
        }
    }
}

@Composable
private fun ImportSection(vm: ImportViewModel) {
    var packPassword by remember { mutableStateOf("") }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let(vm::picked)
    }
    // .mullionpack 没有登记的 MIME,只能放开到 */*;选错了由 inspect_pack 报「包已损坏」。
    Button({ picker.launch(arrayOf("*/*")) }, Modifier.testTag("import"), enabled = !vm.busy) {
        Text("导入 .mullionpack")
    }
    // 包里只有凭据段受口令保护,主机名/known_hosts 是明文、可被篡改(F362 之前)。
    Text("只导入自己经可信途径传来的包:包内主机地址不受口令保护,被改过的包会把凭据发给别人")
    if (vm.pending != null) {
        OutlinedTextField(
            packPassword, { packPassword = it }, Modifier.fillMaxWidth().testTag("pack-password"),
            label = { Text("包口令") }, visualTransformation = PasswordVisualTransformation(),
        )
        Button({ vm.submit(packPassword); packPassword = "" }, Modifier.testTag("pack-submit"), enabled = !vm.busy) {
            Text("导入")
        }
    }
    if (vm.message.isNotEmpty()) Text(vm.message, Modifier.testTag("import-status"))
}
