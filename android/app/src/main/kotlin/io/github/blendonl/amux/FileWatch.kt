package io.github.blendonl.amux

import android.os.FileObserver
import java.io.File

class FileWatch(private val file: File, private val onChange: () -> Unit) {
    private var observer: FileObserver? = null

    fun start() {
        if (observer != null) return
        val directory = file.parentFile ?: return
        observer = object : FileObserver(directory, EVENTS) {
            override fun onEvent(event: Int, path: String?) {
                if (path == file.name) onChange()
            }
        }.also(FileObserver::startWatching)
    }

    fun stop() {
        observer?.stopWatching()
        observer = null
    }

    private companion object {
        const val EVENTS = FileObserver.CREATE or FileObserver.CLOSE_WRITE or FileObserver.MOVED_TO or
            FileObserver.MOVED_FROM or FileObserver.DELETE
    }
}
