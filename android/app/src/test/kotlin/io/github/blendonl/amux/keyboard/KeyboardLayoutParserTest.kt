package io.github.blendonl.amux.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyboardLayoutParserTest {
    private fun layout(left: String, right: String = """["x"]""", extraLayers: String = "") = """
        {
          "width": { "left": 25.0, "right": 30.5 },
          "layers": {
            "base": { "left": [$left], "right": [$right] }$extraLayers
          }
        }
    """.trimIndent()

    private fun failure(json: String): String =
        assertThrows(KeyboardLayoutException::class.java) { KeyboardLayoutParser.parse(json) }.message!!

    @Test
    fun `reads widths and layers`() {
        val parsed = KeyboardLayoutParser.parse(
            layout(
                left = """["q", "w"], ["Ctrl", "layer:sym"]""",
                extraLayers = """, "sym": { "left": [["!"]], "right": [["{"]] }""",
            ),
        )

        assertEquals(25f, parsed.leftPercent)
        assertEquals(30.5f, parsed.rightPercent)
        assertEquals(44.5f, parsed.terminalPercent)
        assertEquals(setOf("base", "sym"), parsed.layers.keys)
        assertEquals(KeyAction.Type("q", "Q"), parsed.base.left!![0][0].action)
        assertEquals(KeyAction.Modify(Modifier.CTRL), parsed.base.left!![1][0].action)
        assertEquals(KeyAction.UseLayer("sym"), parsed.base.left!![1][1].action)
        assertEquals(KeyAction.Type("{", "{"), parsed.layers.getValue("sym").right!![0][0].action)
    }

    @Test
    fun `key tables set text, shift, label, width and repeat`() {
        val parsed = KeyboardLayoutParser.parse(
            layout(
                left = """[
                    { "text": "git ", "label": "git" },
                    { "key": ";", "shift": ":" },
                    { "key": "Space", "width": 2.0, "repeats": true },
                    { "send": "\u001b:w\r", "label": "save" },
                    { "send": "\u0002c" },
                    ""
                ]""",
            ),
        )
        val row = parsed.base.left!![0]

        assertEquals(Key(KeyAction.Type("git "), "git"), row[0])
        assertEquals(Key(KeyAction.Type(";", ":"), ";", ":"), row[1])
        assertEquals(Key(KeyAction.Type(" "), "Space", width = 2f, repeats = true), row[2])
        assertEquals(Key(KeyAction.Send("\u001b:w\r"), "save"), row[3])
        assertEquals(Key(KeyAction.Send("\u0002c"), "^Bc"), row[4])
        assertEquals(Key.BLANK, row[5])
    }

    @Test
    fun `key tables set holds and taps`() {
        val parsed = KeyboardLayoutParser.parse(
            layout(
                left = """[
                    { "key": "f", "hold": ["F", "C-f", { "text": "ff", "label": "2f" }] },
                    { "key": "1", "shift": "1", "hold": "F1" },
                    { "send": "\u0002", "label": "Prefix", "hold": "layer:amux", "taps": ["Escape"] }
                ]""",
            ),
        )
        val row = parsed.base.left!![0]

        assertEquals(
            Hold.Choices(listOf(NamedKeys.lookup("F")!!, NamedKeys.lookup("C-f")!!, Key(KeyAction.Type("ff"), "2f"))),
            row[0].hold,
        )
        assertEquals(Hold.Alternate(NamedKeys.lookup("F1")!!), row[1].hold)
        assertEquals("F1", row[1].hint)
        assertEquals(KeyAction.UseLayer("amux"), row[2].heldLatch?.action)
        assertEquals(listOf(NamedKeys.lookup("Escape")!!), row[2].taps)
        assertTrue(row.all(Key::waits))
    }

    @Test
    fun `a layer may change one half and timings have defaults`() {
        val parsed = KeyboardLayoutParser.parse(
            layout(left = """["a"]""", extraLayers = """, "sym": { "left": [["!"]] }"""),
        )
        val sym = parsed.layers.getValue("sym")

        assertEquals(null, sym.right)
        assertEquals(KeyAction.Type("!", "!"), sym.left!![0][0].action)
        assertEquals(KeyboardLayout.DEFAULT_HOLD_MS, parsed.holdMs)
        assertEquals(KeyboardLayout.DEFAULT_TAPS_MS, parsed.tapsMs)
    }

    @Test
    fun `unexpected output names where it is, counting from one`() {
        assertEquals(
            "layers.sym needs left, right or both",
            failure(layout(left = """["a"]""", extraLayers = """, "sym": {}""")),
        )
        assertEquals(
            "layers.base needs both left and right",
            failure("""{ "width": { "left": 25, "right": 25 }, "layers": { "base": { "left": [["a"]] } } }"""),
        )
        assertEquals(
            "layers.base.left[1][1].hold[2] is \"Bogus\", which isn't a key name",
            failure(layout(left = """[{ "key": "a", "hold": ["A", "Bogus"] }]""")),
        )
        assertEquals(
            "layers.base.left[1][1].taps needs at least one key",
            failure(layout(left = """[{ "key": "a", "taps": [] }]""")),
        )
        assertEquals(
            "layers.base.left[1][2] is \"escape\", which isn't a key name",
            failure(layout(left = """["q", "escape"]""")),
        )
        assertEquals(
            "layers.base.left[2] should be a list of keys",
            failure(layout(left = """["q"], false""")),
        )
        assertEquals(
            "layers.base.left[1][1] should be a key name or a key table",
            failure(layout(left = "[1]")),
        )
        assertEquals(
            "layers.base.left[1][1] needs one of key, text and send",
            failure(layout(left = """[{ "label": "x" }]""")),
        )
        assertEquals(
            "layers has no base layer",
            failure("""{ "width": { "left": 25, "right": 25 }, "layers": {} }"""),
        )
        assertEquals(
            "width.left should be a number",
            failure("""{ "width": {}, "layers": { "base": { "left": [["a"]], "right": [["b"]] } } }"""),
        )
        assertTrue(failure("{ width: ").startsWith("not valid JSON"))
    }
}
