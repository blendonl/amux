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
import android.util.Log
import java.io.IOException
import kotlin.concurrent.thread
import kotlin.time.Duration.Companion.seconds

class AmuxService : Service() {
    inner class LocalBinder : Binder() {
        val service: AmuxService
            get() = this@AmuxService
    }

    private sealed interface Prepared {
        data class Ready(val environment: AmuxEnvironment) : Prepared

        data class Failed(val failure: ServerFailure) : Prepared
    }

    private val binder = LocalBinder()
    private val mainThread = Handler(Looper.getMainLooper())
    private val observers = mutableSetOf<(ServerState) -> Unit>()
    private lateinit var installer: UserlandInstaller
    private lateinit var notification: ServerNotification
    private lateinit var locks: ServiceLocks
    private lateinit var multicast: MulticastNeed
    private var lanPortWatch: FileWatch? = null
    private var supervisor: ServerSupervisor? = null
    private var preparer: Thread? = null
    private var preparing = false
    private var destroyed = false

    var state: ServerState = ServerState.Starting
        private set

    @Volatile
    var environment: AmuxEnvironment? = null
        private set

    @Volatile
    var userlandFailure: String? = null
        private set

    var appVisible: Boolean
        get() = multicast.appVisible
        set(value) {
            multicast.appVisible = value
        }

    override fun onCreate() {
        super.onCreate()
        installer = userlandInstaller()
        notification = ServerNotification(this)
        locks = ServiceLocks(this)
        multicast = MulticastNeed { hold -> if (!destroyed) locks.holdMulticast(hold) }
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
        lanPortWatch?.stop()
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
        if (state == ServerState.Stopping || preparing || supervisor?.isRunning == true) return
        preparing = true
        preparer = thread(name = "amux-prepare") {
            val prepared = prepare()
            mainThread.post { onPrepared(prepared) }
        }
    }

    private fun prepare(): Prepared {
        val installed = installer.install { percent -> mainThread.post { update(ServerState.Installing(percent)) } }
        val failure = (installed as? UserlandInstaller.Outcome.Failed)?.message
        if (failure != null) Log.w(TAG, "running panes in /system/bin/sh, installing the userland failed: $failure")
        userlandFailure = failure
        val amux = amuxEnvironment(withUserland = failure == null)
        environment = amux
        return try {
            amux.prepare(serverName(), zshrcTemplate())
            Prepared.Ready(amux)
        } catch (e: IOException) {
            Prepared.Failed(ServerFailure.Unprepared(filesDir, e.message))
        }
    }

    private fun onPrepared(prepared: Prepared) {
        preparing = false
        if (destroyed || state == ServerState.Stopping) return
        when (prepared) {
            is Prepared.Failed -> update(ServerState.Failed(prepared.failure))
            is Prepared.Ready -> {
                followLanPort(prepared.environment)
                update(ServerState.Starting)
                supervisor = ServerSupervisor(prepared.environment) { next -> mainThread.post { update(next) } }
                    .also { it.start() }
            }
        }
    }

    private fun followLanPort(amux: AmuxEnvironment) {
        if (lanPortWatch != null) return
        lanPortWatch = FileWatch(amux.lanPortFile) { mainThread.post(multicast::check) }.also(FileWatch::start)
        multicast.follow(amux.lanPortFile)
    }

    private fun stopAmux() {
        if (state == ServerState.Stopping) return
        val running = supervisor
        val installing = preparer
        running?.requestStop()
        update(ServerState.Stopping)
        thread(name = "amux-stop") {
            installing?.join()
            environment?.let { AmuxCommands(it).killServer(STOP_GRACE) }
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
        val renotify = !(state is ServerState.Installing && next is ServerState.Installing)
        state = next
        if (next == ServerState.Stopped) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        } else if (renotify) {
            notification.show(notification.build(next, locks.keepingAwake))
        }
        observers.toList().forEach { it(next) }
    }

    companion object {
        const val ACTION_STOP = "io.github.blendonl.amux.action.STOP"
        const val ACTION_TOGGLE_KEEP_AWAKE = "io.github.blendonl.amux.action.TOGGLE_KEEP_AWAKE"
        private const val TAG = "amux"
        private val STOP_GRACE = 5.seconds

        fun start(context: Context) {
            context.startForegroundService(Intent(context, AmuxService::class.java))
        }

        fun stop(context: Context) {
            context.startService(Intent(context, AmuxService::class.java).setAction(ACTION_STOP))
        }
    }
}
