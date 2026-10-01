package io.github.blendonl.amux.keyboard

import android.view.KeyEvent

object NamedKeys {
    private const val LAYER_PREFIX = "layer:"
    private const val FUNCTION_KEYS = 12
    private const val AMUX_PREFIX = "\u0002"

    private val US_SHIFTED: Map<Int, Int> =
        "`1234567890-=[]\\;',./".codePoints().toArray()
            .zip("~!@#$%^&*()_+{}|:\"<>?".codePoints().toArray())
            .toMap()

    private val NAMED: Map<String, Key> = buildMap {
        put("Escape", press(KeyEvent.KEYCODE_ESCAPE, "Esc"))
        put("Tab", press(KeyEvent.KEYCODE_TAB, "Tab"))
        put("Enter", press(KeyEvent.KEYCODE_ENTER, "⏎"))
        put("Backspace", press(KeyEvent.KEYCODE_DEL, "⌫", repeats = true))
        put("Delete", press(KeyEvent.KEYCODE_FORWARD_DEL, "Del", repeats = true))
        put("Insert", press(KeyEvent.KEYCODE_INSERT, "Ins"))
        put("Home", press(KeyEvent.KEYCODE_MOVE_HOME, "Home"))
        put("End", press(KeyEvent.KEYCODE_MOVE_END, "End"))
        put("PageUp", press(KeyEvent.KEYCODE_PAGE_UP, "PgUp", repeats = true))
        put("PageDown", press(KeyEvent.KEYCODE_PAGE_DOWN, "PgDn", repeats = true))
        put("Up", press(KeyEvent.KEYCODE_DPAD_UP, "↑", repeats = true))
        put("Down", press(KeyEvent.KEYCODE_DPAD_DOWN, "↓", repeats = true))
        put("Left", press(KeyEvent.KEYCODE_DPAD_LEFT, "←", repeats = true))
        put("Right", press(KeyEvent.KEYCODE_DPAD_RIGHT, "→", repeats = true))
        for (number in 1..FUNCTION_KEYS) {
            put("F$number", press(KeyEvent.KEYCODE_F1 + number - 1, "F$number"))
        }
        put("Space", Key(KeyAction.Type(" "), "Space"))
        put("Ctrl", Key(KeyAction.Modify(Modifier.CTRL), "Ctrl"))
        put("Alt", Key(KeyAction.Modify(Modifier.ALT), "Alt"))
        put("Shift", Key(KeyAction.Modify(Modifier.SHIFT), "⇧"))
        put("Prefix", Key(KeyAction.Send(AMUX_PREFIX), "Prefix"))
        put("Paste", Key(KeyAction.Paste, "Paste"))
        put("Hide", Key(KeyAction.Hide, "Hide"))
    }

    private val MODIFIER_PREFIXES = listOf("C-" to Modifier.CTRL, "M-" to Modifier.ALT, "S-" to Modifier.SHIFT)

    fun lookup(name: String): Key? = when {
        name.codePointCount(0, name.length) == 1 -> typing(name)
        name.startsWith(LAYER_PREFIX) && name.length > LAYER_PREFIX.length -> layer(name.removePrefix(LAYER_PREFIX))
        else -> NAMED[name] ?: chord(name)
    }

    private fun chord(name: String): Key? {
        val modifiers = linkedSetOf<Modifier>()
        var rest = name
        while (true) {
            val (prefix, modifier) = MODIFIER_PREFIXES
                .firstOrNull { (prefix, _) -> rest.startsWith(prefix) && rest.length > prefix.length }
                ?: break
            modifiers += modifier
            rest = rest.removePrefix(prefix)
        }
        if (modifiers.isEmpty()) return null
        val key = lookup(rest) ?: return null
        if (key.action !is KeyAction.Type && key.action !is KeyAction.Press) return null
        return Key(KeyAction.Chord(key.action, modifiers), chordLabel(modifiers, key.label))
    }

    private fun chordLabel(modifiers: Set<Modifier>, label: String): String {
        if (modifiers == setOf(Modifier.CTRL) && label.codePointCount(0, label.length) == 1) {
            return "^" + label.uppercase()
        }
        return modifiers.joinToString("") { modifier -> MODIFIER_PREFIXES.first { it.second == modifier }.first } + label
    }

    fun typing(text: String): Key {
        val shifted = shifted(text)
        return Key(KeyAction.Type(text, shifted), text, shifted)
    }

    fun layer(name: String): Key = Key(KeyAction.UseLayer(name), name)

    fun shifted(text: String): String {
        if (text.codePointCount(0, text.length) != 1) return text
        val codePoint = text.codePointAt(0)
        val shifted = US_SHIFTED[codePoint] ?: Character.toUpperCase(codePoint)
        return String(Character.toChars(shifted))
    }

    private fun press(keyCode: Int, label: String, repeats: Boolean = false) =
        Key(KeyAction.Press(keyCode), label, repeats = repeats)
}
