package io.github.blendonl.amux

import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import kotlin.time.Duration

class AmuxCommands(private val amux: AmuxEnvironment) {
    fun startServer(): Process {
        truncateLargeLog()
        return command("server").start()
    }

    fun killServer(timeout: Duration): Boolean {
        val process = try {
            command("kill-server").start()
        } catch (e: IOException) {
            return false
        }
        if (process.waitFor(timeout)) return process.exitValue() == 0
        process.destroy()
        return false
    }

    fun lastLogLine(): String? {
        val log = amux.serverLog
        if (!log.isFile) return null
        return RandomAccessFile(log, "r").use { file ->
            val start = (file.length() - LOG_TAIL_BYTES).coerceAtLeast(0)
            val tail = ByteArray((file.length() - start).toInt())
            file.seek(start)
            file.readFully(tail)
            tail.decodeToString().lineSequence().lastOrNull { it.isNotBlank() }?.trim()
        }
    }

    private fun command(argument: String): ProcessBuilder =
        ProcessBuilder(amux.binary.path, argument)
            .directory(amux.home)
            .redirectInput(ProcessBuilder.Redirect.from(DEV_NULL))
            .redirectOutput(ProcessBuilder.Redirect.appendTo(amux.serverLog))
            .redirectErrorStream(true)
            .also { builder ->
                builder.environment().clear()
                builder.environment().putAll(amux.variables)
            }

    private fun truncateLargeLog() {
        if (amux.serverLog.length() > MAX_LOG_BYTES) amux.serverLog.writeText("")
    }

    private companion object {
        val DEV_NULL = File("/dev/null")
        const val LOG_TAIL_BYTES = 4096L
        const val MAX_LOG_BYTES = 1L shl 20
    }
}
