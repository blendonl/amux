package io.github.blendonl.amux

import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Semaphore
import java.util.concurrent.TimeUnit
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource

class ServerSupervisor(
    private val amux: AmuxEnvironment,
    private val report: (ServerState) -> Unit,
) {
    private class Exit(val status: Int, val ranFor: Duration, val failure: ServerFailure)

    private enum class Answer { ANSWERED, EXITED, SILENT, STOPPING }

    private val commands = AmuxCommands(amux)
    private val probe = SocketProbe(amux.socket)
    private val policy = RestartPolicy()
    private val stopRequested = CountDownLatch(1)
    private val wakeups = Semaphore(0)
    private val worker = Thread(::supervise, "amux-supervisor")

    @Volatile
    private var server: Process? = null

    private val stopping: Boolean
        get() = stopRequested.count == 0L

    val isRunning: Boolean
        get() = worker.isAlive

    fun start() = worker.start()

    fun requestStop() {
        stopRequested.countDown()
        wakeups.release()
    }

    fun awaitStop(grace: Duration) {
        requestStop()
        server?.let { if (!it.waitFor(grace)) it.destroy() }
        worker.join(grace.inWholeMilliseconds)
    }

    private fun supervise() {
        while (!stopping) {
            val exit = if (probe.answers()) watchRunningServer() else runServer()
            if (stopping) return
            when (val decision = policy.afterExit(exit.status, exit.ranFor)) {
                RestartPolicy.Decision.Stop -> return report(ServerState.Stopped)
                RestartPolicy.Decision.GiveUp -> return report(ServerState.Failed(exit.failure))
                is RestartPolicy.Decision.Restart -> {
                    report(ServerState.Restarting(exit.failure, decision.delay))
                    if (stopRequested.await(decision.delay)) return
                }
            }
        }
    }

    private fun watchRunningServer(): Exit {
        report(ServerState.Ready)
        val started = TimeSource.Monotonic.markNow()
        val socketWatch = FileWatch(amux.socket) { wakeups.release() }.also(FileWatch::start)
        try {
            do {
                wakeups.tryAcquire(WATCH_INTERVAL.inWholeMilliseconds, TimeUnit.MILLISECONDS)
            } while (!stopping && probe.answers())
        } finally {
            socketWatch.stop()
        }
        return Exit(0, started.elapsedNow(), ServerFailure.Exited(0, null))
    }

    private fun runServer(): Exit {
        report(ServerState.Starting)
        val started = TimeSource.Monotonic.markNow()
        val process = try {
            commands.startServer()
        } catch (e: IOException) {
            return Exit(UNLAUNCHABLE, started.elapsedNow(), ServerFailure.Unlaunchable(amux.binary, e.message))
        }
        server = process
        try {
            return when (awaitAnswer(process)) {
                Answer.ANSWERED -> {
                    report(ServerState.Ready)
                    exitOf(process, started)
                }
                Answer.EXITED, Answer.STOPPING -> exitOf(process, started)
                Answer.SILENT -> {
                    process.destroy()
                    process.waitFor()
                    Exit(SILENT, started.elapsedNow(), ServerFailure.NoAnswer(amux.socket, READY_TIMEOUT))
                }
            }
        } finally {
            server = null
        }
    }

    private fun awaitAnswer(process: Process): Answer {
        val deadline = TimeSource.Monotonic.markNow() + READY_TIMEOUT
        while (deadline.hasNotPassedNow()) {
            if (probe.answers()) return Answer.ANSWERED
            if (!process.isAlive) return Answer.EXITED
            if (stopRequested.await(POLL_INTERVAL)) return Answer.STOPPING
        }
        return Answer.SILENT
    }

    private fun exitOf(process: Process, started: TimeSource.Monotonic.ValueTimeMark): Exit {
        val status = process.waitFor()
        return Exit(status, started.elapsedNow(), ServerFailure.Exited(status, commands.lastLogLine()))
    }

    private companion object {
        const val UNLAUNCHABLE = -1
        const val SILENT = -2
        val READY_TIMEOUT = 10.seconds
        val POLL_INTERVAL = 100.milliseconds
        val WATCH_INTERVAL = 30.seconds
    }
}
