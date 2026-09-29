package io.github.blendonl.amux.keyboard

import android.os.FileObserver
import android.os.Handler
import android.os.Looper
import java.io.File

class ConfigFileWatcher(private val file: File, onChange: () -> Unit) {
    private val mainThread = Handler(Looper.getMainLooper())
    private val settled = Runnable(onChange)
    private var observer: FileObserver? = null

    fun start() {
        if (observer != null) return
        val directory = file.parentFile ?: return
        observer = object : FileObserver(directory, EVENTS) {
            override fun onEvent(event: Int, path: String?) {
                if (path != file.name) return
                mainThread.removeCallbacks(settled)
                mainThread.postDelayed(settled, SETTLE_MS)
            }
        }.also(FileObserver::startWatching)
    }

    fun stop() {
        observer?.stopWatching()
        observer = null
        mainThread.removeCallbacks(settled)
    }

    private companion object {
        const val EVENTS = FileObserver.CLOSE_WRITE or FileObserver.MOVED_TO or FileObserver.MOVED_FROM or FileObserver.DELETE
        const val SETTLE_MS = 150L
    }
}
