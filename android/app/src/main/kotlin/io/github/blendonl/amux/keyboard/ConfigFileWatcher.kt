package io.github.blendonl.amux.keyboard

import android.os.FileObserver
import android.os.Handler
import android.os.Looper
import java.io.File

class ConfigFileWatcher(private val dir: File, onChange: () -> Unit) {
    private val mainThread = Handler(Looper.getMainLooper())
    private val settled = Runnable(onChange)
    private val observers = mutableMapOf<File, FileObserver>()
    private var watching = false

    fun start() {
        if (watching) return
        watching = true
        watchDirectories()
    }

    fun stop() {
        watching = false
        observers.values.forEach(FileObserver::stopWatching)
        observers.clear()
        mainThread.removeCallbacks(settled)
    }

    private fun changed(path: String?) {
        if (!watching) return
        watchDirectories()
        if (path == null || !ConfigStamp.isConfig(path)) return
        mainThread.removeCallbacks(settled)
        mainThread.postDelayed(settled, SETTLE_MS)
    }

    private fun watchDirectories() {
        val directories = dir.walkTopDown().filter(File::isDirectory).toSet()
        (observers.keys - directories).forEach { observers.remove(it)?.stopWatching() }
        (directories - observers.keys).forEach { observers[it] = observe(it) }
    }

    private fun observe(directory: File): FileObserver =
        object : FileObserver(directory, EVENTS) {
            override fun onEvent(event: Int, path: String?) {
                mainThread.post { changed(path) }
            }
        }.also(FileObserver::startWatching)

    private companion object {
        const val EVENTS = FileObserver.CLOSE_WRITE or FileObserver.MOVED_TO or FileObserver.MOVED_FROM or
            FileObserver.DELETE or FileObserver.CREATE or FileObserver.DELETE_SELF
        const val SETTLE_MS = 150L
    }
}
