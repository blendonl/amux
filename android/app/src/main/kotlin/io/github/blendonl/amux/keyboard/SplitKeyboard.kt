package io.github.blendonl.amux.keyboard

class SplitKeyboard(layout: KeyboardLayout, private val sink: KeySink) {
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

    val shifted: Boolean
        get() = modifiers.getValue(Modifier.SHIFT).active

    val activeLayer: Layer
        get() {
            val name = layerLatches
                .filterValues(Latch::active)
                .keys
                .maxByOrNull { layerPressedAt[it] ?: 0L }
            return name?.let(layout.layers::get) ?: layout.base
        }

    fun rows(side: Side): KeyRows = activeLayer.rows(side)

    fun addListener(listener: () -> Unit) {
        listeners += listener
    }

    fun latchOf(key: Key): Latch? = when (val action = key.action) {
        is KeyAction.Modify -> modifiers.getValue(action.modifier)
        is KeyAction.UseLayer -> layerLatches[action.name]
        else -> null
    }

    fun press(key: Key) {
        when (val action = key.action) {
            is KeyAction.Modify -> modifiers.getValue(action.modifier).press()
            is KeyAction.UseLayer -> pressLayer(action.name)
            else -> emit(action)
        }
        changed()
    }

    fun release(key: Key) {
        val latch = latchOf(key) ?: return
        latch.release()
        changed()
    }

    fun repeat(key: Key) {
        if (key.latches) return
        emit(key.action)
        changed()
    }

    fun reset() {
        modifiers.values.forEach(Latch::reset)
        layerLatches.clear()
        layerPressedAt.clear()
        changed()
    }

    private fun pressLayer(name: String) {
        if (name !in layout.layers) return
        layerLatches.getOrPut(name, ::Latch).press()
        layerPressedAt[name] = ++presses
    }

    private fun emit(action: KeyAction) {
        val ctrl = modifiers.getValue(Modifier.CTRL).active
        val alt = modifiers.getValue(Modifier.ALT).active
        val shift = shifted
        when (action) {
            is KeyAction.Type -> (if (shift) action.shifted else action.text)
                .codePoints()
                .forEach { sink.type(it, ctrl, alt) }
            is KeyAction.Press -> sink.press(action.keyCode, ctrl, alt, shift)
            is KeyAction.Send -> sink.send(action.text)
            KeyAction.Paste -> sink.paste()
            KeyAction.Hide -> sink.hide()
            KeyAction.Blank, KeyAction.Transparent, is KeyAction.Modify, is KeyAction.UseLayer -> return
        }
        modifiers.values.forEach(Latch::use)
        layerLatches.values.forEach(Latch::use)
    }

    private fun changed() {
        listeners.forEach { it() }
    }
}
