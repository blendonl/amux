package io.github.blendonl.amux

import android.Manifest
import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.view.View
import android.view.WindowInsets
import android.view.inputmethod.InputMethodManager
import com.termux.terminal.TerminalEmulator
import com.termux.terminal.TerminalSession
import com.termux.view.TerminalView

class MainActivity : Activity() {
    private lateinit var amux: AmuxEnvironment
    private lateinit var terminalView: TerminalView
    private lateinit var extraKeys: ExtraKeys
    private lateinit var status: StatusPanel
    private lateinit var sessionCallbacks: TerminalSessionCallbacks
    private var session: TerminalSession? = null
    private var service: AmuxService? = null
    private var bound = false
    private var attachWhenReady = true

    private val serverObserver: (ServerState) -> Unit = { onServerState(it) }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = (binder as AmuxService.LocalBinder).service.also { it.observe(serverObserver) }
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        fitContentToInsets(findViewById(R.id.root))
        amux = amuxEnvironment()
        terminalView = findViewById(R.id.terminal)
        extraKeys = ExtraKeys(findViewById(R.id.extra_keys), terminalView)
        val fontSize = FontSize(this)
        terminalView.setTextSize(fontSize.current)
        terminalView.setTerminalViewClient(TerminalViewCallbacks(terminalView, extraKeys, fontSize, ::showKeyboard))
        sessionCallbacks = TerminalSessionCallbacks(this, terminalView, ::onClientExited)
        status = StatusPanel(findViewById(R.id.status), onReattach = ::reattach, onStop = ::stopAmux)

        requestNotificationPermission()
        showProgress(getString(R.string.status_starting), null)
        AmuxService.start(this)
        bound = bindService(Intent(this, AmuxService::class.java), connection, 0)
    }

    override fun onDestroy() {
        service?.stopObserving(serverObserver)
        if (bound) unbindService(connection)
        session?.finishIfRunning()
        session = null
        super.onDestroy()
    }

    private fun onServerState(state: ServerState) {
        if (!attachWhenReady) return
        when (state) {
            ServerState.Ready -> attach()
            ServerState.Starting -> showProgress(getString(R.string.status_starting), null)
            is ServerState.Restarting -> showProgress(getString(R.string.status_starting), state.describe(this))
            ServerState.Stopping -> showProgress(getString(R.string.status_stopping), null)
            is ServerState.Failed -> showDetached(getString(R.string.status_server_failed), state.describe(this))
            ServerState.Stopped -> showDetached(getString(R.string.status_stopped), null)
        }
    }

    private fun attach() {
        attachWhenReady = false
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
        if (service?.state == ServerState.Ready) attach()
    }

    private fun stopAmux() {
        AmuxService.stop(this)
        finish()
    }

    private fun showTerminal() {
        status.hide()
        terminalView.visibility = View.VISIBLE
        terminalView.keepScreenOn = true
        extraKeys.visible = true
        terminalView.requestFocus()
        showKeyboard()
    }

    private fun showProgress(title: String, detail: String?) {
        hideTerminal()
        status.show(title, detail, withActions = false)
    }

    private fun showDetached(title: String, detail: String?) {
        hideTerminal()
        hideKeyboard()
        status.show(title, detail, withActions = true)
    }

    private fun hideTerminal() {
        terminalView.visibility = View.INVISIBLE
        terminalView.keepScreenOn = false
        extraKeys.visible = false
    }

    private fun showKeyboard() {
        terminalView.post {
            getSystemService(InputMethodManager::class.java).showSoftInput(terminalView, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    private fun hideKeyboard() {
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(terminalView.windowToken, 0)
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
