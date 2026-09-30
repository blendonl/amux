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
        put("esc", press(KeyEvent.KEYCODE_ESCAPE, "Esc"))
        put("tab", press(KeyEvent.KEYCODE_TAB, "Tab"))
        put("enter", press(KeyEvent.KEYCODE_ENTER, "⏎"))
        put("bksp", press(KeyEvent.KEYCODE_DEL, "⌫", repeats = true))
        put("del", press(KeyEvent.KEYCODE_FORWARD_DEL, "Del", repeats = true))
        put("ins", press(KeyEvent.KEYCODE_INSERT, "Ins"))
        put("home", press(KeyEvent.KEYCODE_MOVE_HOME, "Home"))
        put("end", press(KeyEvent.KEYCODE_MOVE_END, "End"))
        put("pgup", press(KeyEvent.KEYCODE_PAGE_UP, "PgUp", repeats = true))
        put("pgdn", press(KeyEvent.KEYCODE_PAGE_DOWN, "PgDn", repeats = true))
        put("up", press(KeyEvent.KEYCODE_DPAD_UP, "↑", repeats = true))
        put("down", press(KeyEvent.KEYCODE_DPAD_DOWN, "↓", repeats = true))
        put("left", press(KeyEvent.KEYCODE_DPAD_LEFT, "←", repeats = true))
        put("right", press(KeyEvent.KEYCODE_DPAD_RIGHT, "→", repeats = true))
        for (number in 1..FUNCTION_KEYS) {
            put("f$number", press(KeyEvent.KEYCODE_F1 + number - 1, "F$number"))
        }
        put("space", Key(KeyAction.Type(" "), "Space"))
        put("ctrl", Key(KeyAction.Modify(Modifier.CTRL), "Ctrl"))
        put("alt", Key(KeyAction.Modify(Modifier.ALT), "Alt"))
        put("shift", Key(KeyAction.Modify(Modifier.SHIFT), "⇧"))
        put("prefix", Key(KeyAction.Send(AMUX_PREFIX), "Prefix"))
        put("paste", Key(KeyAction.Paste, "Paste"))
        put("hide", Key(KeyAction.Hide, "Hide"))
    }

    fun lookup(name: String): Key? = when {
        name.codePointCount(0, name.length) == 1 -> typing(name)
        name.startsWith(LAYER_PREFIX) && name.length > LAYER_PREFIX.length -> layer(name.removePrefix(LAYER_PREFIX))
        else -> NAMED[name.lowercase()]
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
