package io.github.blendonl.amux

import android.system.ErrnoException
import android.system.Os
import java.io.File
import java.io.IOException

interface Posix {
    fun chmod(file: File, mode: Int)

    fun symlink(target: String, link: File)

    fun rename(from: File, to: File)
}

object AndroidPosix : Posix {
    override fun chmod(file: File, mode: Int) = failingWithPath(file) { Os.chmod(file.path, mode) }

    override fun symlink(target: String, link: File) = failingWithPath(link) { Os.symlink(target, link.path) }

    override fun rename(from: File, to: File) = failingWithPath(from) { Os.rename(from.path, to.path) }

    private inline fun failingWithPath(file: File, call: () -> Unit) {
        try {
            call()
        } catch (e: ErrnoException) {
            throw IOException("${file.path}: ${e.message}", e)
        }
    }
}
