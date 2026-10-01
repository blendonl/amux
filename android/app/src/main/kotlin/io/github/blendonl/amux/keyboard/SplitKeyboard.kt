package io.github.blendonl.amux.keyboard

class SplitKeyboard(layout: KeyboardLayout, private val sink: KeySink, private val timer: KeyTimer) {
    data class Popup(val key: Key, val choices: List<Key>, val selected: Int = 0)

    private enum class Resolution { WAITING, TAPPED, HELD }

    private class Down(val key: Key) {
        var resolution = Resolution.WAITING
        var cancel: () -> Unit = {}
    }

    private class Run(val key: Key) {
        var taps = 0
        var cancel: () -> Unit = {}
    }

    var layout: KeyboardLayout = layout
        set(value) {
            field = value
            reset()
        }

    private val modifiers = Modifier.entries.associateWith { Latch() }
    private val layerLatches = mutableMapOf<String, Latch>()
    private val layerPressedAt = mutableMapOf<String, Long>()
    private var presses = 0L
    private val listeners = mutableListOf<() -> Unit>()
    private val down = mutableListOf<Down>()
    private var run: Run? = null

    var popup: Popup? = null
        private set

    val shifted: Boolean
        get() = modifiers.getValue(Modifier.SHIFT).active

    val activeLayer: Layer
        get() = activeLayers().firstOrNull() ?: layout.base

    fun rows(side: Side): KeyRows = activeLayers().firstNotNullOfOrNull { it.rows(side) } ?: layout.baseRows(side)

    fun label(key: Key): String {
        val text = key.label(shifted)
        if (key.action !is KeyAction.Type || text.codePointCount(0, text.length) != 1) return text
        val ctrl = modifiers.getValue(Modifier.CTRL).active
        val alt = modifiers.getValue(Modifier.ALT).active
        return when {
            ctrl && alt -> "C-M-$text"
            ctrl -> "^" + text.uppercase()
            alt -> "M-$text"
            else -> text
        }
    }

    fun addListener(listener: () -> Unit) {
        listeners += listener
    }

    fun latchOf(key: Key): Latch? = when (val action = key.action) {
        is KeyAction.Modify -> modifiers.getValue(action.modifier)
        is KeyAction.UseLayer -> layerLatches[action.name]
        else -> null
    }

    fun press(key: Key) {
        if (key.action == KeyAction.Blank) return
        settle(key)
        when {
            key.latches -> pressLatch(key)
            key.waits -> wait(key)
            else -> emit(key.action)
        }
        changed()
    }

    fun release(key: Key) {
        val held = down.firstOrNull { it.key === key }
        if (held == null) {
            val latch = latchOf(key) ?: return
            latch.release()
            changed()
            return
        }
        down -= held
        held.cancel()
        key.heldLatch?.let { latchOf(it)?.lift() }
        when {
            popup?.key === key -> commitPopup()
            held.resolution == Resolution.WAITING -> tapped(key)
            else -> {}
        }
        changed()
    }

    fun select(choice: Int) {
        val open = popup ?: return
        if (choice !in open.choices.indices || choice == open.selected) return
        popup = open.copy(selected = choice)
        changed()
    }

    fun repeat(key: Key) {
        if (key.latches || key.waits) return
        emit(key.action)
        changed()
    }

    fun reset() {
        down.forEach { it.cancel() }
        down.clear()
        run?.cancel?.invoke()
        run = null
        popup = null
        modifiers.values.forEach(Latch::reset)
        layerLatches.clear()
        layerPressedAt.clear()
        changed()
    }

    private fun activeLayers(): List<Layer> = layerLatches
        .filterValues(Latch::active)
        .keys
        .sortedByDescending { layerPressedAt[it] ?: 0L }
        .mapNotNull(layout.layers::get)

    private fun settle(pressed: Key) {
        for (held in down) {
            when {
                held.resolution != Resolution.WAITING -> if (popup?.key === held.key) commitPopup()
                held.key.heldLatch != null -> {
                    held.cancel()
                    held.resolution = Resolution.HELD
                }
                else -> {
                    held.cancel()
                    held.resolution = Resolution.TAPPED
                    tapped(held.key)
                }
            }
        }
        if (run != null && run?.key !== pressed) finishRun()
    }

    private fun pressLatch(key: Key) {
        when (val action = key.action) {
            is KeyAction.Modify -> modifiers.getValue(action.modifier).press()
            is KeyAction.UseLayer -> pressLayer(action.name)
            else -> {}
        }
    }

    private fun pressLayer(name: String) {
        if (name !in layout.layers) return
        layerLatches.getOrPut(name, ::Latch).press()
        layerPressedAt[name] = ++presses
    }

    private fun wait(key: Key) {
        val held = Down(key)
        down += held
        key.heldLatch?.let(::pressLatch)
        if (key.hold != null && run?.key !== key) {
            held.cancel = timer.after(layout.holdMs) { holdFired(held) }
        }
    }

    private fun holdFired(held: Down) {
        if (held !in down || held.resolution != Resolution.WAITING) return
        held.resolution = Resolution.HELD
        when (val hold = held.key.hold) {
            is Hold.Alternate -> if (!hold.key.latches) emit(hold.key.action)
            is Hold.Choices -> popup = Popup(held.key, hold.keys)
            null -> {}
        }
        changed()
    }

    private fun tapped(key: Key) {
        if (key.taps.isEmpty()) {
            emit(key.action)
            return
        }
        val current = run?.takeIf { it.key === key } ?: Run(key).also {
            finishRun()
            run = it
        }
        current.cancel()
        current.taps++
        if (current.taps > key.taps.size) {
            finishRun()
        } else {
            current.cancel = timer.after(layout.tapsMs) {
                finishRun()
                changed()
            }
        }
    }

    private fun finishRun() {
        val finished = run ?: return
        run = null
        finished.cancel()
        val key = if (finished.taps <= 1) finished.key else finished.key.taps[finished.taps - 2]
        emit(key.action)
    }

    private fun commitPopup() {
        val open = popup ?: return
        popup = null
        emit(open.choices[open.selected].action)
    }

    private fun emit(action: KeyAction) {
        val chord = action as? KeyAction.Chord
        val forced = chord?.modifiers.orEmpty()
        val ctrl = modifiers.getValue(Modifier.CTRL).active || Modifier.CTRL in forced
        val alt = modifiers.getValue(Modifier.ALT).active || Modifier.ALT in forced
        val shift = shifted || Modifier.SHIFT in forced
        when (val plain = chord?.action ?: action) {
            is KeyAction.Type -> (if (shift) plain.shifted else plain.text)
                .codePoints()
                .forEach { sink.type(it, ctrl, alt) }
            is KeyAction.Press -> sink.press(plain.keyCode, ctrl, alt, shift)
            is KeyAction.Send -> sink.send(plain.text)
            KeyAction.Paste -> sink.paste()
            KeyAction.Hide -> sink.hide()
            KeyAction.Blank, KeyAction.Transparent, is KeyAction.Chord, is KeyAction.Modify, is KeyAction.UseLayer -> return
        }
        modifiers.values.forEach(Latch::use)
        layerLatches.values.forEach(Latch::use)
    }

    private fun changed() {
        listeners.forEach { it() }
    }
}
