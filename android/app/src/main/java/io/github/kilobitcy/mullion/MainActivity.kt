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
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    private val vm: TerminalViewModel by viewModels()
    private val importVm: ImportViewModel by viewModels()
    private val cloudVm: CloudViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) { Screen(vm, importVm, cloudVm) }
            }
        }
    }
}

@Composable
private fun Screen(vm: TerminalViewModel, importVm: ImportViewModel, cloudVm: CloudViewModel) {
    // host/port/user 旋转不丢;密码类故意只用 remember,不进 saved-state Bundle
    var host by rememberSaveable { mutableStateOf("") }
    var port by rememberSaveable { mutableStateOf("22") }
    var user by rememberSaveable { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var input by remember { mutableStateOf("") }
    // 导入完成后(尤其重置了本机密钥)云表单要重读磁盘,否则仍显示已被清掉的旧值
    LaunchedEffect(importVm.finishedCount) { if (importVm.finishedCount > 0) cloudVm.refresh() }

    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding().padding(8.dp)) {
        val connected = vm.status == "已连接"
        if (!connected) {
            // 表单与两个分节同处一个滚动区并占满剩余高度:软键盘/横屏下任何控件都能滚到;
            // 状态行留在滚动区外(单行,不会被挤没),结果提示另由各分节自行滚入视野
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).testTag("form-scroll")) {
                OutlinedTextField(host, { host = it }, Modifier.fillMaxWidth().testTag("host"), label = { Text("主机") })
                OutlinedTextField(port, { port = it }, Modifier.fillMaxWidth().testTag("port"), label = { Text("端口") })
                OutlinedTextField(user, { user = it }, Modifier.fillMaxWidth().testTag("user"), label = { Text("用户") })
                OutlinedTextField(
                    password, { password = it }, Modifier.fillMaxWidth().testTag("password"),
                    label = { Text("密码") }, visualTransformation = PasswordVisualTransformation(),
                )
                Button({ vm.connect(host, port, user, password) }, Modifier.testTag("connect")) { Text("连接") }
                ImportSection(importVm, otherBusy = cloudVm.busy)
                CloudSection(cloudVm, otherBusy = importVm.busy)
            }
        }
        Text(vm.status, Modifier.testTag("status"))
        // 终端区与发送框只在已连接时存在:断线后 rows 仍有内容,若未连接也渲染会把滚动区挤没
        if (connected) {
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
}

@Composable
private fun ImportSection(vm: ImportViewModel, otherBusy: Boolean) {
    var packPassword by remember { mutableStateOf("") }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let(vm::picked)
    }
    // .mullionpack 没有登记的 MIME,只能放开到 */*;选错了由 inspect_pack 报「包已损坏」。
    Button({ picker.launch(arrayOf("*/*")) }, Modifier.testTag("import"), enabled = !vm.busy && !otherBusy) {
        Text("导入 .mullionpack")
    }
    // 包里只有凭据段受口令保护,主机名/known_hosts 是明文、可被篡改(F362 之前);云端对象同理。
    Text("只导入自己经可信途径传来的包:包内主机地址不受口令保护,被改过的包(含云端对象被改)会把凭据发给别人")
    if (vm.pending != null) {
        OutlinedTextField(
            packPassword, { packPassword = it }, Modifier.fillMaxWidth().testTag("pack-password"),
            label = { Text("包口令") }, visualTransformation = PasswordVisualTransformation(),
        )
        Button({ vm.submit(packPassword); packPassword = "" }, Modifier.testTag("pack-submit"), enabled = !vm.busy && !otherBusy) {
            Text("导入")
        }
    }
    if (vm.message.isNotEmpty()) {
        val req = remember { BringIntoViewRequester() }
        Text(vm.message, Modifier.bringIntoViewRequester(req).testTag("import-status"))
        // 结果出现(或同文案再次出现:结果世代号递增)时滚入视野;不能用 busy 做 key,无关的 busy 翻转(如 refresh)会把视口拉走
        LaunchedEffect(vm.message, vm.finishedCount) { req.bringIntoView() }
    }
}

@Composable
private fun CloudField(
    value: String, tag: String, label: String, enabled: Boolean, secret: Boolean = false, onChange: (String) -> Unit,
) {
    OutlinedTextField(
        value, onChange, Modifier.fillMaxWidth().testTag(tag), label = { Text(label) },
        visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None,
        singleLine = true, enabled = enabled,
        // 密钥/口令框:关掉输入法联想与自动更正,免得明文进词库
        keyboardOptions = if (secret) KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false) else KeyboardOptions.Default,
    )
}

@Composable
private fun CloudSection(vm: CloudViewModel, otherBusy: Boolean) {
    val f = vm.form
    val idle = !vm.busy && !otherBusy
    Text("云端备份(只读拉取)")
    CloudField(f.endpoint, "cloud-endpoint", "Endpoint", !vm.busy) { vm.edit(f.copy(endpoint = it)) }
    if (f.endpoint.trim().startsWith("http://", ignoreCase = true)) {
        Text("明文 http:AK、签名与对象名在链路上可见,建议用 https", Modifier.testTag("cloud-http-warning"))
    }
    CloudField(f.region, "cloud-region", "Region", !vm.busy) { vm.edit(f.copy(region = it)) }
    CloudField(f.bucket, "cloud-bucket", "Bucket", !vm.busy) { vm.edit(f.copy(bucket = it)) }
    CloudField(f.prefix, "cloud-prefix", "前缀", !vm.busy) { vm.edit(f.copy(prefix = it)) }
    Row {
        Text("path-style")
        Switch(f.pathStyle, { vm.edit(f.copy(pathStyle = it)) }, Modifier.testTag("cloud-path-style"), enabled = !vm.busy)
    }
    CloudField(f.socks5, "cloud-socks5", "SOCKS5(host:port,可空,不要带用户名口令)", !vm.busy) { vm.edit(f.copy(socks5 = it)) }
    CloudField(f.accessKeyId, "cloud-ak", "AK", !vm.busy) { vm.edit(f.copy(accessKeyId = it)) }
    CloudField(
        f.secretKey, "cloud-sk", if (f.hasSecret) "SK(已设置,留空沿用;换 AK 须重填)" else "SK(未设置)", !vm.busy, secret = true,
    ) { vm.edit(f.copy(secretKey = it)) }
    CloudField(
        f.passphrase, "cloud-pass", if (f.hasPassphrase) "备份口令(已设置,留空沿用)" else "备份口令(未设置)", !vm.busy, secret = true,
    ) { vm.edit(f.copy(passphrase = it)) }
    Row {
        Button(vm::save, Modifier.testTag("cloud-save"), enabled = idle && !f.isCorrupt) { Text("保存") }
        Button(vm::pull, Modifier.testTag("cloud-pull"), enabled = idle && !f.isCorrupt) { Text("从云端更新") }
        if (f.isCorrupt) Button(vm::reset, Modifier.testTag("cloud-reset"), enabled = idle) { Text("重置云配置") }
    }
    if (vm.message.isNotEmpty()) {
        val req = remember { BringIntoViewRequester() }
        Text(vm.message, Modifier.bringIntoViewRequester(req).testTag("cloud-status"))
        LaunchedEffect(vm.message, vm.resultCount) { req.bringIntoView() }
    }
}
