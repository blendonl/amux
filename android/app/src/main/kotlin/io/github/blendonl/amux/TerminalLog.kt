package io.github.blendonl.amux

import android.util.Log

object TerminalLog {
    private const val DEFAULT_TAG = "amux"

    fun error(tag: String?, message: String?, error: Throwable? = null) {
        Log.e(tag ?: DEFAULT_TAG, message.orEmpty(), error)
    }

    fun warn(tag: String?, message: String?) {
        Log.w(tag ?: DEFAULT_TAG, message.orEmpty())
    }

    fun info(tag: String?, message: String?) {
        Log.i(tag ?: DEFAULT_TAG, message.orEmpty())
    }

    fun debug(tag: String?, message: String?) {
        Log.d(tag ?: DEFAULT_TAG, message.orEmpty())
    }

    fun verbose(tag: String?, message: String?) {
        Log.v(tag ?: DEFAULT_TAG, message.orEmpty())
    }
}
