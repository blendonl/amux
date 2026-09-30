package io.github.blendonl.amux

import android.Manifest
import android.app.Activity
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.view.View
import android.view.WindowInsets
import android.widget.Toast
import com.termux.terminal.TerminalEmulator
import com.termux.terminal.TerminalSession
import com.termux.view.TerminalView
import io.github.blendonl.amux.keyboard.ConfigFileWatcher
import io.github.blendonl.amux.keyboard.KeyboardConfigFile
import io.github.blendonl.amux.keyboard.KeyboardHalfView
import io.github.blendonl.amux.keyboard.KeyboardLayoutParser
import io.github.blendonl.amux.keyboard.Side
import io.github.blendonl.amux.keyboard.SplitKeyboard
import io.github.blendonl.amux.keyboard.TerminalKeySink
import java.io.File

class MainActivity : Activity() {
    private lateinit var terminalView: TerminalView
    private lateinit var extraKeys: ExtraKeys
    private lateinit var input: InputPanels
    private lateinit var splitKeyboard: SplitKeyboard
    private var keyboardConfig: KeyboardConfigFile? = null
    private var keyboardWatcher: ConfigFileWatcher? = null
    private lateinit var status: StatusPanel
    private lateinit var sessionCallbacks: TerminalSessionCallbacks
    private var session: TerminalSession? = null
    private var service: AmuxService? = null
    private var bound = false
    private var attachWhenReady = true
    private var userlandFailureShown = false

    private val serverObserver: (ServerState) -> Unit = { onServerState(it) }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            val connected = (binder as AmuxService.LocalBinder).service
            service = connected
            connected.observe(serverObserver)
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
        }

        override fun onBindingDied(name: ComponentName) {
            service = null
            if (!isFinishing) rebind()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        fitContentToInsets(findViewById(R.id.root))
        terminalView = findViewById(R.id.terminal)
        extraKeys = ExtraKeys(findViewById(R.id.extra_keys), terminalView)
        setUpInput()
        val fontSize = FontSize(this)
        terminalView.setTextSize(fontSize.current)
        terminalView.setTerminalViewClient(TerminalViewCallbacks(terminalView, extraKeys, fontSize, input::onTerminalTap))
        sessionCallbacks = TerminalSessionCallbacks(this, terminalView, ::onClientExited)
        status = StatusPanel(findViewById(R.id.status), onReattach = ::reattach, onStop = ::stopAmux)

        requestNotificationPermission()
        showProgress(getString(R.string.status_starting), null)
        AmuxService.start(this)
        bind()
    }

    override fun onResume() {
        super.onResume()
        reloadKeyboard()
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        reloadKeyboard()
        input.configure(newConfig)
    }

    override fun onDestroy() {
        keyboardWatcher?.stop()
        service?.stopObserving(serverObserver)
        if (bound) unbindService(connection)
        session?.finishIfRunning()
        session = null
        super.onDestroy()
    }

    private fun setUpInput() {
        val leftHalf = findViewById<KeyboardHalfView>(R.id.keyboard_left)
        val rightHalf = findViewById<KeyboardHalfView>(R.id.keyboard_right)
        input = InputPanels(window, terminalView, findViewById(R.id.terminal_pane), extraKeys, leftHalf, rightHalf)
        val sink = TerminalKeySink(terminalView, getSystemService(ClipboardManager::class.java), input::hideSplitKeyboard)
        splitKeyboard = SplitKeyboard(KeyboardLayoutParser.parse(defaultKeyboardLayout()), sink)
        leftHalf.attach(splitKeyboard, Side.LEFT)
        rightHalf.attach(splitKeyboard, Side.RIGHT)
        input.resize(splitKeyboard.layout)
        input.configure(resources.configuration)
    }

    private fun watchKeyboardConfig(file: File) {
        if (keyboardConfig != null) return
        keyboardConfig = KeyboardConfigFile(defaultKeyboardLayout(), file)
        keyboardWatcher = ConfigFileWatcher(file, ::reloadKeyboard).also(ConfigFileWatcher::start)
        reloadKeyboard()
    }

    private fun reloadKeyboard() {
        val config = keyboardConfig ?: return
        val loaded = config.loadIfChanged() ?: return
        splitKeyboard.layout = loaded.layout
        input.resize(loaded.layout)
        loaded.problem?.let { problem ->
            Toast.makeText(this, getString(R.string.keyboard_config_failed, config.file.name, problem), Toast.LENGTH_LONG).show()
        }
    }

    private fun bind() {
        bound = bindService(Intent(this, AmuxService::class.java), connection, 0)
    }

    private fun rebind() {
        if (bound) unbindService(connection)
        bind()
    }

    private fun onServerState(state: ServerState) {
        if (!attachWhenReady) return
        when (state) {
            ServerState.Ready -> attachOrWarn()
            is ServerState.Installing ->
                showProgress(getString(R.string.status_installing), getString(R.string.status_installing_progress, state.percent))
            ServerState.Starting -> showProgress(getString(R.string.status_starting), null)
            is ServerState.Restarting -> showProgress(getString(R.string.status_starting), state.describe(this))
            ServerState.Stopping -> showProgress(getString(R.string.status_stopping), null)
            is ServerState.Failed -> showDetached(getString(R.string.status_server_failed), state.describe(this))
            ServerState.Stopped -> showDetached(getString(R.string.status_stopped), null)
        }
    }

    private fun attachOrWarn() {
        val connected = service ?: return
        val failure = connected.userlandFailure
        if (failure != null && !userlandFailureShown) {
            userlandFailureShown = true
            attachWhenReady = false
            showDetached(
                getString(R.string.status_userland_failed),
                getString(R.string.userland_fallback, failure),
                getString(R.string.action_continue),
            )
            return
        }
        connected.environment?.let(::attach)
    }

    private fun attach(amux: AmuxEnvironment) {
        attachWhenReady = false
        watchKeyboardConfig(amux.keyboardFile)
        val client = TerminalSession(
            amux.binary.path,
            amux.home.path,
            arrayOf(CLIENT_ARGV0),
            amux.terminalVariables.map { (name, value) -> "$name=$value" }.toTypedArray(),
            TerminalEmulator.DEFAULT_TERMINAL_TRANSCRIPT_ROWS,
            sessionCallbacks,
        )
        session = client
        terminalView.attachSession(client)
        showTerminal()
    }

    private fun onClientExited(finished: TerminalSession) {
        if (finished !== session) return
        session = null
        val exitStatus = finished.exitStatus
        val detail = if (exitStatus == 0) null else getString(R.string.status_client_exited, exitStatus)
        showDetached(getString(R.string.status_detached), detail)
    }

    private fun reattach() {
        attachWhenReady = true
        showProgress(getString(R.string.status_starting), null)
        AmuxService.start(this)
        if (service?.state == ServerState.Ready) attachOrWarn()
    }

    private fun stopAmux() {
        AmuxService.stop(this)
        finish()
    }

    private fun showTerminal() {
        status.hide()
        terminalView.visibility = View.VISIBLE
        terminalView.keepScreenOn = true
        terminalView.requestFocus()
        input.showTerminal()
    }

    private fun showProgress(title: String, detail: String?) {
        hideTerminal()
        status.show(title, detail)
    }

    private fun showDetached(title: String, detail: String?, reattachLabel: String = getString(R.string.action_reattach)) {
        hideTerminal()
        status.show(title, detail, reattachLabel)
    }

    private fun hideTerminal() {
        terminalView.visibility = View.INVISIBLE
        terminalView.keepScreenOn = false
        input.hideTerminal()
    }

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return
        requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), NOTIFICATION_PERMISSION_REQUEST)
    }

    private fun fitContentToInsets(root: View) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) return
        root.setOnApplyWindowInsetsListener { view, insets ->
            val bars = insets.getInsets(
                WindowInsets.Type.systemBars() or WindowInsets.Type.ime() or WindowInsets.Type.displayCutout(),
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsets.CONSUMED
        }
    }

    private companion object {
        const val CLIENT_ARGV0 = "amux"
        const val NOTIFICATION_PERMISSION_REQUEST = 1
    }
}
