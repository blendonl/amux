package io.github.blendonl.amux.keyboard

class ManualTimer : KeyTimer {
    private class Scheduled(val at: Long, val action: () -> Unit) {
        var cancelled = false
    }

    private val scheduled = mutableListOf<Scheduled>()
    private var now = 0L

    override fun after(delayMs: Long, action: () -> Unit): () -> Unit {
        val entry = Scheduled(now + delayMs, action)
        scheduled += entry
        return { entry.cancelled = true }
    }

    fun advance(ms: Long) {
        val until = now + ms
        while (true) {
            val next = scheduled.filter { !it.cancelled && it.at <= until }.minByOrNull { it.at } ?: break
            scheduled -= next
            now = next.at
            next.action()
        }
        now = until
    }
}
