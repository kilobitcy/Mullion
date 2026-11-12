package io.github.kilobitcy.mullion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) { Screen(vm) }
            }
        }
    }
}

@Composable
private fun Screen(vm: TerminalViewModel) {
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
