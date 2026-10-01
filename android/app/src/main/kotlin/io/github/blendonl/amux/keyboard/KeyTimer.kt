package io.github.blendonl.amux.keyboard

import android.os.Handler

fun interface KeyTimer {
    fun after(delayMs: Long, action: () -> Unit): () -> Unit
}

class HandlerKeyTimer(private val handler: Handler) : KeyTimer {
    override fun after(delayMs: Long, action: () -> Unit): () -> Unit {
        val runnable = Runnable(action)
        handler.postDelayed(runnable, delayMs)
        return { handler.removeCallbacks(runnable) }
    }
}
