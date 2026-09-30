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
        assertEquals(KeyAction.Type("q", "Q"), parsed.base.left[0][0].action)
        assertEquals(KeyAction.Modify(Modifier.CTRL), parsed.base.left[1][0].action)
        assertEquals(KeyAction.UseLayer("sym"), parsed.base.left[1][1].action)
        assertEquals(KeyAction.Type("{", "{"), parsed.layers.getValue("sym").right[0][0].action)
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
        val row = parsed.base.left[0]

        assertEquals(Key(KeyAction.Type("git "), "git"), row[0])
        assertEquals(Key(KeyAction.Type(";", ":"), ";", ":"), row[1])
        assertEquals(Key(KeyAction.Type(" "), "Space", width = 2f, repeats = true), row[2])
        assertEquals(Key(KeyAction.Send("\u001b:w\r"), "save"), row[3])
        assertEquals(Key(KeyAction.Send("\u0002c"), "^Bc"), row[4])
        assertEquals(Key.BLANK, row[5])
    }

    @Test
    fun `unexpected output names where it is, counting from one`() {
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
