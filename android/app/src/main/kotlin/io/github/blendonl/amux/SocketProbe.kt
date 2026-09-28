package io.github.blendonl.amux

import android.net.LocalSocket
import android.net.LocalSocketAddress
import java.io.File
import java.io.IOException

class SocketProbe(private val socket: File) {
    fun answers(): Boolean =
        try {
            LocalSocket().use { it.connect(LocalSocketAddress(socket.path, LocalSocketAddress.Namespace.FILESYSTEM)) }
            true
        } catch (e: IOException) {
            false
        }
}
