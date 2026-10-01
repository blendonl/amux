package io.github.blendonl.amux.keyboard

enum class Modifier { CTRL, ALT, SHIFT }

sealed interface KeyAction {
    data class Type(val text: String, val shifted: String = text) : KeyAction

    data class Press(val keyCode: Int) : KeyAction

    data class Chord(val action: KeyAction, val modifiers: Set<Modifier>) : KeyAction

    data class Modify(val modifier: Modifier) : KeyAction

    data class UseLayer(val name: String) : KeyAction

    data class Send(val text: String) : KeyAction

    data object Paste : KeyAction

    data object Hide : KeyAction

    data object Blank : KeyAction

    data object Transparent : KeyAction
}

sealed interface Hold {
    data class Alternate(val key: Key) : Hold

    data class Choices(val keys: List<Key>) : Hold
}

data class Key(
    val action: KeyAction,
    val label: String,
    val shiftedLabel: String = label,
    val width: Float = 1f,
    val repeats: Boolean = false,
    val hold: Hold? = null,
    val taps: List<Key> = emptyList(),
) {
    val latches: Boolean
        get() = action is KeyAction.Modify || action is KeyAction.UseLayer

    val heldLatch: Key?
        get() = (hold as? Hold.Alternate)?.key?.takeIf(Key::latches)

    val waits: Boolean
        get() = hold != null || taps.isNotEmpty()

    val hint: String?
        get() = (hold as? Hold.Alternate)?.key?.label

    fun label(shifted: Boolean): String = if (shifted) shiftedLabel else label

    companion object {
        val BLANK = Key(KeyAction.Blank, "")
        val TRANSPARENT = Key(KeyAction.Transparent, "")
    }
}
