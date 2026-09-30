package io.github.blendonl.amux.keyboard

class Latch {
    enum class State { OFF, ONE_SHOT, LOCKED }

    var state = State.OFF
        private set
    var held = false
        private set
    private var usedWhileHeld = false

    val active: Boolean
        get() = held || state != State.OFF

    fun press() {
        held = true
        usedWhileHeld = false
    }

    fun release() {
        if (!held) return
        held = false
        if (!usedWhileHeld) state = afterTap(state)
    }

    fun use() {
        if (held) usedWhileHeld = true
        if (state == State.ONE_SHOT) state = State.OFF
    }

    fun reset() {
        state = State.OFF
        held = false
        usedWhileHeld = false
    }

    private fun afterTap(state: State) = when (state) {
        State.OFF -> State.ONE_SHOT
        State.ONE_SHOT -> State.LOCKED
        State.LOCKED -> State.OFF
    }
}
