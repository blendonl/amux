package io.github.blendonl.amux.keyboard

import android.view.KeyEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SplitKeyboardTest {
    private class RecordingSink : KeySink {
        val events = mutableListOf<String>()

        override fun type(codePoint: Int, ctrl: Boolean, alt: Boolean) {
            events += modifiers(ctrl, alt, shift = false) + String(Character.toChars(codePoint))
        }

        override fun press(keyCode: Int, ctrl: Boolean, alt: Boolean, shift: Boolean) {
            events += modifiers(ctrl, alt, shift) + "#$keyCode"
        }

        override fun send(text: String) {
            events += "send:$text"
        }

        override fun paste() {
            events += "Paste"
        }

        override fun hide() {
            events += "Hide"
        }

        private fun modifiers(ctrl: Boolean, alt: Boolean, shift: Boolean) =
            (if (ctrl) "C-" else "") + (if (alt) "M-" else "") + (if (shift) "S-" else "")
    }

    private val sink = RecordingSink()
    private val tabCode = "#${KeyEvent.KEYCODE_TAB}"
    private val leftCode = "#${KeyEvent.KEYCODE_DPAD_LEFT}"
    private val homeCode = "#${KeyEvent.KEYCODE_MOVE_HOME}"
    private val ctrl = NamedKeys.lookup("Ctrl")!!
    private val alt = NamedKeys.lookup("Alt")!!
    private val shift = NamedKeys.lookup("Shift")!!
    private val sym = NamedKeys.layer("sym")
    private val nav = NamedKeys.layer("nav")

    private fun key(name: String) = NamedKeys.lookup(name)!!

    private val layout = KeyboardLayout(
        leftPercent = 25f,
        rightPercent = 30f,
        layers = mapOf(
            "base" to Layer("base", listOf(listOf(key("a"), key("1"), key("Tab"))), listOf(listOf(ctrl, alt, shift, sym, nav))),
            "sym" to Layer("sym", listOf(listOf(key("!"), key("{"), key("Tab"))), listOf(listOf(ctrl, alt, shift, sym, nav))),
            "nav" to Layer("nav", listOf(listOf(key("Left"), key("Home"), key("Tab"))), listOf(listOf(ctrl, alt, shift, sym, nav))),
        ),
    )
    private val timer = ManualTimer()
    private val keyboard = SplitKeyboard(layout, sink, timer)

    private fun tap(key: Key) {
        keyboard.press(key)
        keyboard.release(key)
    }

    private fun tapLeft(column: Int) = tap(keyboard.rows(Side.LEFT)[0][column])

    @Test
    fun `types the base layer`() {
        tapLeft(0)
        tapLeft(1)

        assertEquals(listOf("a", "1"), sink.events)
    }

    @Test
    fun `a tapped modifier applies to the next key only`() {
        tap(shift)
        tapLeft(0)
        tapLeft(0)
        tap(ctrl)
        tapLeft(0)
        tapLeft(0)

        assertEquals(listOf("A", "a", "C-a", "a"), sink.events)
    }

    @Test
    fun `shift types the shifted symbol and reaches special keys`() {
        tap(shift)
        tapLeft(1)
        tap(shift)
        tapLeft(2)

        assertEquals(listOf("!", "S-$tabCode"), sink.events)
    }

    @Test
    fun `a held modifier applies to every key until released`() {
        keyboard.press(ctrl)
        tapLeft(0)
        tapLeft(0)
        keyboard.release(ctrl)
        tapLeft(0)

        assertEquals(listOf("C-a", "C-a", "a"), sink.events)
    }

    @Test
    fun `a double tapped modifier stays locked until tapped again`() {
        tap(alt)
        tap(alt)
        tapLeft(0)
        tapLeft(0)
        tap(alt)
        tapLeft(0)

        assertEquals(listOf("M-a", "M-a", "a"), sink.events)
    }

    @Test
    fun `modifiers combine`() {
        tap(ctrl)
        tap(alt)
        tapLeft(2)

        assertEquals(listOf("C-M-$tabCode"), sink.events)
    }

    @Test
    fun `a tapped layer applies to the next key only`() {
        tap(sym)
        assertEquals("sym", keyboard.activeLayer.name)
        tapLeft(1)
        assertEquals("base", keyboard.activeLayer.name)
        tapLeft(1)

        assertEquals(listOf("{", "1"), sink.events)
    }

    @Test
    fun `a held layer stays while held`() {
        keyboard.press(nav)
        tapLeft(0)
        tapLeft(0)
        keyboard.release(nav)
        tapLeft(0)

        assertEquals(listOf(leftCode, leftCode, "a"), sink.events)
    }

    @Test
    fun `a double tapped layer stays locked`() {
        tap(nav)
        tap(nav)
        tapLeft(1)
        tapLeft(1)
        tap(nav)
        tapLeft(1)

        assertEquals(listOf(homeCode, homeCode, "1"), sink.events)
    }

    @Test
    fun `the most recently pressed layer wins`() {
        tap(nav)
        tap(nav)
        keyboard.press(sym)
        tapLeft(1)
        keyboard.release(sym)
        tapLeft(1)

        assertEquals(listOf("{", homeCode), sink.events)
    }

    @Test
    fun `modifiers apply inside a layer`() {
        tap(ctrl)
        tap(sym)
        tapLeft(1)

        assertEquals(listOf("C-{"), sink.events)
    }

    @Test
    fun `a layer that the layout lacks does nothing`() {
        tap(NamedKeys.layer("missing"))

        assertEquals("base", keyboard.activeLayer.name)
    }

    @Test
    fun `text keys type every character`() {
        tap(ctrl)
        tap(Key(KeyAction.Type("ls"), "ls"))

        assertEquals(listOf("C-l", "C-s"), sink.events)
    }

    @Test
    fun `send, paste and hide reach the sink`() {
        tap(key("Prefix"))
        tap(key("Paste"))
        tap(key("Hide"))

        assertEquals(listOf("send:\u0002", "Paste", "Hide"), sink.events)
    }

    @Test
    fun `a blank key keeps pending modifiers`() {
        tap(shift)
        tap(Key.BLANK)
        tapLeft(0)

        assertEquals(listOf("A"), sink.events)
    }

    @Test
    fun `repeats emit the key again without latching`() {
        val left = key("Left")
        keyboard.press(left)
        keyboard.repeat(left)
        keyboard.repeat(shift)
        keyboard.release(left)

        assertEquals(listOf(leftCode, leftCode), sink.events)
        assertFalse(keyboard.shifted)
    }

    @Test
    fun `a new layout clears latches`() {
        tap(shift)
        tap(sym)
        tap(sym)
        keyboard.layout = layout.copy(leftPercent = 20f)

        assertFalse(keyboard.shifted)
        assertEquals("base", keyboard.activeLayer.name)
    }

    @Test
    fun `listeners hear every change`() {
        var changes = 0
        keyboard.addListener { changes++ }
        tap(shift)
        tapLeft(0)

        assertEquals(3, changes)
    }

    @Test
    fun `reports latch state for drawing`() {
        tap(shift)

        assertTrue(keyboard.shifted)
        assertEquals(Latch.State.ONE_SHOT, keyboard.latchOf(shift)?.state)
        assertEquals(null, keyboard.latchOf(sym))
        assertEquals(null, keyboard.latchOf(key("a")))
    }

    private val f1Code = "#${KeyEvent.KEYCODE_F1}"
    private val escapeCode = "#${KeyEvent.KEYCODE_ESCAPE}"
    private val f = Key(KeyAction.Type("f", "F"), "f", "F", hold = Hold.Choices(listOf(key("F"), key("C-f"), key("M-f"))))
    private val one = Key(KeyAction.Type("1"), "1", hold = Hold.Alternate(key("F1")))
    private val j = Key(KeyAction.Type("j", "J"), "j", "J", taps = listOf(key("Escape")))
    private val amux = Key(KeyAction.Send("\u0002"), "Prefix", hold = Hold.Alternate(NamedKeys.layer("amux")))

    private val oneSided = KeyboardLayout(
        leftPercent = 21f,
        rightPercent = 21f,
        layers = mapOf(
            "base" to Layer("base", listOf(listOf(key("a"), key("b"))), listOf(listOf(key("y"), key("z")))),
            "sym" to Layer("sym", listOf(listOf(key("!"), key("@"))), null),
            "amux" to Layer("amux", null, listOf(listOf(Key(KeyAction.Send("\u0002c"), "+win"), key("Hide")))),
        ),
    )

    private fun topLabel(side: Side) = keyboard.rows(side)[0][0].label

    @Test
    fun `a layer changes only the halves it defines`() {
        keyboard.layout = oneSided
        keyboard.press(sym)

        assertEquals("!", topLabel(Side.LEFT))
        assertEquals("y", topLabel(Side.RIGHT))

        keyboard.press(amux)

        assertEquals("!", topLabel(Side.LEFT))
        assertEquals("+win", topLabel(Side.RIGHT))
    }

    @Test
    fun `a hold types the key's alternate and a quick tap types the key`() {
        keyboard.press(one)
        timer.advance(299)
        keyboard.release(one)
        keyboard.press(one)
        timer.advance(300)
        keyboard.release(one)

        assertEquals(listOf("1", f1Code), sink.events)
    }

    @Test
    fun `holding a key opens a popup whose choice types on release`() {
        keyboard.press(f)
        timer.advance(300)

        assertEquals(listOf("F", "^F", "M-f"), keyboard.popup?.choices?.map(Key::label))

        keyboard.select(1)
        keyboard.release(f)
        assertNull(keyboard.popup)
        keyboard.press(f)
        timer.advance(300)
        keyboard.release(f)

        assertEquals(listOf("C-f", "F"), sink.events)
    }

    @Test
    fun `pressing another key types a waiting key first`() {
        keyboard.press(f)
        tapLeft(0)
        keyboard.release(f)
        timer.advance(1000)

        assertEquals(listOf("f", "a"), sink.events)
        assertNull(keyboard.popup)
    }

    @Test
    fun `pressing another key while a popup is open types its choice first`() {
        keyboard.press(f)
        timer.advance(300)
        keyboard.select(2)
        tapLeft(0)
        keyboard.release(f)

        assertEquals(listOf("M-f", "a"), sink.events)
    }

    @Test
    fun `a layer on a hold shows at once and a quick tap sends the key`() {
        keyboard.layout = oneSided
        keyboard.press(amux)

        assertEquals("+win", topLabel(Side.RIGHT))

        keyboard.release(amux)

        assertEquals("y", topLabel(Side.RIGHT))
        assertEquals(listOf("send:\u0002"), sink.events)
    }

    @Test
    fun `a layer on a hold stays while the other half is used`() {
        keyboard.layout = oneSided
        keyboard.press(amux)
        tap(keyboard.rows(Side.RIGHT)[0][0])
        keyboard.release(amux)
        tap(keyboard.rows(Side.RIGHT)[0][1])

        assertEquals(listOf("send:\u0002c", "z"), sink.events)
    }

    @Test
    fun `a layer held past the hold time sends nothing`() {
        keyboard.layout = oneSided
        keyboard.press(amux)
        timer.advance(300)
        keyboard.release(amux)

        assertEquals(emptyList<String>(), sink.events)
        assertEquals("base", keyboard.activeLayer.name)
    }

    @Test
    fun `quick taps pick the key's taps`() {
        tap(j)
        timer.advance(249)
        assertEquals(emptyList<String>(), sink.events)
        tap(j)
        tap(j)
        timer.advance(250)

        assertEquals(listOf(escapeCode, "j"), sink.events)
    }

    @Test
    fun `another key ends a run of taps first`() {
        tap(j)
        tapLeft(0)

        assertEquals(listOf("j", "a"), sink.events)
    }

    @Test
    fun `labels show what ctrl and alt will send`() {
        val a = key("a")
        tap(ctrl)
        assertEquals("^A", keyboard.label(a))
        tapLeft(0)
        assertEquals("a", keyboard.label(a))
        tap(alt)
        assertEquals("M-a", keyboard.label(a))
        assertEquals("Tab", keyboard.label(key("Tab")))
    }

    @Test
    fun `chords send their own modifiers`() {
        tap(key("C-c"))
        tap(key("M-Left"))

        assertEquals(listOf("C-c", "M-$leftCode"), sink.events)
    }

    @Test
    fun `waiting keys never repeat`() {
        keyboard.press(f)
        keyboard.repeat(f)

        assertEquals(emptyList<String>(), sink.events)
    }

    @Test
    fun `a new layout drops waiting keys and popups`() {
        keyboard.press(f)
        timer.advance(300)
        keyboard.layout = layout.copy(leftPercent = 20f)
        keyboard.release(f)

        assertNull(keyboard.popup)
        assertEquals(emptyList<String>(), sink.events)
    }
}
