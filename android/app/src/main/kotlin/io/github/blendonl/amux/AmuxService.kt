package io.github.blendonl.amux

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import java.io.IOException
import kotlin.concurrent.thread
import kotlin.time.Duration.Companion.seconds

class AmuxService : Service() {
    inner class LocalBinder : Binder() {
        val service: AmuxService
            get() = this@AmuxService
    }

    private val binder = LocalBinder()
    private val mainThread = Handler(Looper.getMainLooper())
    private val observers = mutableSetOf<(ServerState) -> Unit>()
    private lateinit var amux: AmuxEnvironment
    private lateinit var notification: ServerNotification
    private lateinit var locks: ServiceLocks
    private var supervisor: ServerSupervisor? = null
    private var destroyed = false

    var state: ServerState = ServerState.Starting
        private set

    override fun onCreate() {
        super.onCreate()
        amux = amuxEnvironment()
        notification = ServerNotification(this)
        locks = ServiceLocks(this)
        locks.holdMulticast()
    }

    override fun onBind(intent: Intent): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!enterForeground()) {
            stopSelf()
            return START_NOT_STICKY
        }
        when (intent?.action) {
            ACTION_STOP -> stopAmux()
            ACTION_TOGGLE_KEEP_AWAKE -> {
                toggleKeepAwake()
                superviseIfIdle()
            }
            else -> superviseIfIdle()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        destroyed = true
        supervisor?.requestStop()
        locks.releaseAll()
        observers.clear()
        super.onDestroy()
    }

    fun observe(observer: (ServerState) -> Unit) {
        observers += observer
        observer(state)
    }

    fun stopObserving(observer: (ServerState) -> Unit) {
        observers -= observer
    }

    private fun enterForeground(): Boolean =
        try {
            val shown = notification.build(state, locks.keepingAwake)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                startForeground(ServerNotification.ID, shown, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
            } else {
                startForeground(ServerNotification.ID, shown)
            }
            true
        } catch (e: IllegalStateException) {
            false
        }

    private fun superviseIfIdle() {
        if (state == ServerState.Stopping || supervisor?.isRunning == true) return
        try {
            amux.prepare(serverName())
        } catch (e: IOException) {
            update(ServerState.Failed(ServerFailure.Unprepared(filesDir, e.message)))
            return
        }
        state = ServerState.Starting
        supervisor = ServerSupervisor(amux) { next -> mainThread.post { update(next) } }.also { it.start() }
    }

    private fun stopAmux() {
        if (state == ServerState.Stopping) return
        val running = supervisor
        running?.requestStop()
        update(ServerState.Stopping)
        thread(name = "amux-stop") {
            AmuxCommands(amux).killServer(STOP_GRACE)
            running?.awaitStop(STOP_GRACE)
            mainThread.post { update(ServerState.Stopped) }
        }
    }

    private fun toggleKeepAwake() {
        locks.toggleKeepAwake()
        notification.show(notification.build(state, locks.keepingAwake))
    }

    private fun update(next: ServerState) {
        if (destroyed) return
        if (state == ServerState.Stopping && next != ServerState.Stopped) return
        state = next
        if (next == ServerState.Stopped) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        } else {
            notification.show(notification.build(next, locks.keepingAwake))
        }
        observers.toList().forEach { it(next) }
    }

    companion object {
        const val ACTION_STOP = "io.github.blendonl.amux.action.STOP"
        const val ACTION_TOGGLE_KEEP_AWAKE = "io.github.blendonl.amux.action.TOGGLE_KEEP_AWAKE"
        private val STOP_GRACE = 5.seconds

        fun start(context: Context) {
            context.startForegroundService(Intent(context, AmuxService::class.java))
        }

        fun stop(context: Context) {
            context.startService(Intent(context, AmuxService::class.java).setAction(ACTION_STOP))
        }
    }
}
