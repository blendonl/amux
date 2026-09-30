package io.github.blendonl.amux

import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import kotlin.time.Duration

class AmuxCommands(private val amux: AmuxEnvironment) {
    fun startServer(): Process {
        truncateLargeLog()
        return logged("server").start()
    }

    fun killServer(timeout: Duration): Boolean {
        val process = try {
            logged("kill-server").start()
        } catch (e: IOException) {
            return false
        }
        if (process.waitFor(timeout)) return process.exitValue() == 0
        process.destroy()
        return false
    }

    fun printKeyboard(defaults: Boolean, timeout: Duration): Result<String> = runCatching {
        val output = File.createTempFile(KEYBOARD_PREFIX, ".json", amux.tmpDir)
        val errors = File.createTempFile(KEYBOARD_PREFIX, ".err", amux.tmpDir)
        try {
            val builder = command("config", "keyboard")
                .redirectOutput(output)
                .redirectError(errors)
            if (defaults) builder.environment()[CONFIG_VARIABLE] = DEV_NULL.path
            val process = builder.start()
            if (!process.waitFor(timeout)) {
                process.destroy()
                throw IOException("amux config keyboard took longer than $timeout")
            }
            if (process.exitValue() != 0) {
                val message = errors.readText().trim().removePrefix(ERROR_PREFIX)
                throw IOException(message.replace(amux.initFile.path, amux.initFile.name))
            }
            output.readText()
        } finally {
            output.delete()
            errors.delete()
        }
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

    private fun logged(argument: String): ProcessBuilder =
        command(argument)
            .redirectOutput(ProcessBuilder.Redirect.appendTo(amux.serverLog))
            .redirectErrorStream(true)

    private fun command(vararg arguments: String): ProcessBuilder =
        ProcessBuilder(amux.binary.path, *arguments)
            .directory(amux.home)
            .redirectInput(ProcessBuilder.Redirect.from(DEV_NULL))
            .also { builder ->
                builder.environment().clear()
                builder.environment().putAll(amux.variables)
            }

    private fun truncateLargeLog() {
        if (amux.serverLog.length() > MAX_LOG_BYTES) amux.serverLog.writeText("")
    }

    private companion object {
        val DEV_NULL = File("/dev/null")
        const val CONFIG_VARIABLE = "AMUX_CONFIG"
        const val KEYBOARD_PREFIX = "keyboard"
        const val ERROR_PREFIX = "Error: "
        const val LOG_TAIL_BYTES = 4096L
        const val MAX_LOG_BYTES = 1L shl 20
    }
}
