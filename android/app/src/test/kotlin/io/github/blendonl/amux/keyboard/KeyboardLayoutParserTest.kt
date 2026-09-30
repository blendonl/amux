package io.github.blendonl.amux.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyboardLayoutParserTest {
    private val defaults = """
        {
          "width": { "left": 25, "right": 30 },
          "layers": {
            "base": {
              "left": [["q", "w"], ["ctrl", "layer:sym"]],
              "right": [["o", "p"], ["enter", "bksp"]]
            },
            "sym": {
              "left": [["!", null], null],
              "right": [[null, "{", null], [null, null]]
            }
          }
        }
    """.trimIndent()

    private fun parse(overrides: String? = null) = KeyboardLayoutParser.parse(defaults, overrides)

    private fun failure(overrides: String): String =
        assertThrows(KeyboardLayoutException::class.java) { parse(overrides) }.message!!

    @Test
    fun `reads widths and layers`() {
        val layout = parse()

        assertEquals(25f, layout.leftPercent)
        assertEquals(30f, layout.rightPercent)
        assertEquals(45f, layout.terminalPercent)
        assertEquals(setOf("base", "sym"), layout.layers.keys)
        assertEquals(KeyAction.Type("q", "Q"), layout.base.left[0][0].action)
        assertEquals(KeyAction.UseLayer("sym"), layout.base.left[1][1].action)
    }

    @Test
    fun `null keys and rows fall through to the base layer`() {
        val layout = parse()
        val sym = layout.layers.getValue("sym")

        assertEquals(KeyAction.Type("!", "!"), sym.left[0][0].action)
        assertEquals(layout.base.left[0][1], sym.left[0][1])
        assertEquals(layout.base.left[1], sym.left[1])
        assertEquals(layout.base.right[0][0], sym.right[0][0])
        assertEquals(KeyAction.Type("{"), sym.right[0][1].action)
        assertEquals(Key.BLANK, sym.right[0][2])
        assertEquals(layout.base.right[1], sym.right[1])
    }

    @Test
    fun `overrides replace single widths`() {
        val layout = parse("""{ "width": { "right": 20.5 } }""")

        assertEquals(25f, layout.leftPercent)
        assertEquals(20.5f, layout.rightPercent)
    }

    @Test
    fun `overrides change, add and remove layers by name`() {
        val layout = parse(
            """
            {
              "layers": {
                "base": { "left": [["a", "layer:nav"]], "right": [["b"]] },
                "sym": null,
                "nav": { "left": [[null, "up"]], "right": [["down"]] }
              }
            }
            """,
        )

        assertEquals(setOf("base", "nav"), layout.layers.keys)
        assertEquals(KeyAction.Type("a", "A"), layout.base.left[0][0].action)
        assertEquals(layout.base.left[0][0], layout.layers.getValue("nav").left[0][0])
    }

    @Test
    fun `a layer override keeps the sides it leaves out`() {
        val layout = parse("""{ "layers": { "sym": { "right": [["x"]] } } }""")
        val sym = layout.layers.getValue("sym")

        assertEquals(KeyAction.Type("x", "X"), sym.right[0][0].action)
        assertEquals(KeyAction.Type("!", "!"), sym.left[0][0].action)
    }

    @Test
    fun `a new layer needs both sides`() {
        assertEquals(
            "layers.nav.right is missing",
            failure("""{ "layers": { "nav": { "left": [["up"]] } } }"""),
        )
    }

    @Test
    fun `object keys set text, shift, label, width and repeat`() {
        val layout = parse(
            """
            {
              "layers": {
                "base": {
                  "left": [[
                    { "text": "git ", "label": "git" },
                    { "key": ";", "shift": ":" },
                    { "key": "space", "width": 2.5, "repeat": true },
                    { "send": "\u001b:w\r", "label": "save" },
                    { "send": "\u0002c" },
                    { "key": "", "width": 0.5 }
                  ]],
                  "right": [["x"]]
                },
                "sym": null
              }
            }
            """,
        )
        val row = layout.base.left[0]

        assertEquals(Key(KeyAction.Type("git "), "git"), row[0])
        assertEquals(Key(KeyAction.Type(";", ":"), ";", ":"), row[1])
        assertEquals(Key(KeyAction.Type(" "), "Space", width = 2.5f, repeats = true), row[2])
        assertEquals(Key(KeyAction.Send("\u001b:w\r"), "save"), row[3])
        assertEquals(Key(KeyAction.Send("\u0002c"), "^Bc"), row[4])
        assertEquals(Key.BLANK.copy(width = 0.5f), row[5])
    }

    @Test
    fun `an empty string is a blank key`() {
        val layout = parse("""{ "layers": { "base": { "left": [[""]], "right": [["x"]] }, "sym": null } }""")

        assertEquals(Key.BLANK, layout.base.left[0][0])
    }

    @Test
    fun `errors name where the problem is`() {
        assertEquals(
            "layers.base.left[0][1] is \"escape\", which isn't a key name",
            failure("""{ "layers": { "base": { "left": [["q", "escape"]], "right": [["x"]] } } }"""),
        )
        assertEquals(
            "lsplit is not a setting, expected one of width, layers",
            failure("""{ "lsplit": { "left": 30 } }"""),
        )
        assertEquals(
            "width.left should be between 5 and 45 percent of the screen",
            failure("""{ "width": { "left": 60 } }"""),
        )
        assertEquals("width.right should be a number", failure("""{ "width": { "right": "30" } }"""))
        assertEquals("width.right is missing", failure("""{ "width": { "right": null } }"""))
        assertEquals("width should be an object", failure("""{ "width": 30 }"""))
    }

    @Test
    fun `a layer key must name a layer`() {
        assertEquals(
            "layers.base.left[1][1] uses the unknown layer \"sym\"",
            failure("""{ "layers": { "sym": null } }"""),
        )
    }

    @Test
    fun `the base layer is required and can't fall through`() {
        assertEquals("layers needs a base layer", failure("""{ "layers": { "base": null } }"""))
        assertTrue(
            failure("""{ "layers": { "base": { "left": [[null]], "right": [["x"]] }, "sym": null } }""")
                .startsWith("layers.base.left[0][0] is null"),
        )
    }

    @Test
    fun `key objects are checked`() {
        assertEquals(
            "layers.base.left[0][0] needs exactly one of key, text, send",
            failure("""{ "layers": { "base": { "left": [[{ "key": "a", "text": "b" }]], "right": [["x"]] }, "sym": null } }"""),
        )
        assertEquals(
            "layers.base.left[0][0].shift only applies to keys that type text",
            failure("""{ "layers": { "base": { "left": [[{ "key": "esc", "shift": "x" }]], "right": [["x"]] }, "sym": null } }"""),
        )
        assertEquals(
            "layers.base.left[0][0].width should be a number above 0",
            failure("""{ "layers": { "base": { "left": [[{ "key": "a", "width": 0 }]], "right": [["x"]] }, "sym": null } }"""),
        )
        assertEquals(
            "layers.base.left[0][0].colour is not a setting, expected one of key, text, send, label, shift, width, repeat",
            failure("""{ "layers": { "base": { "left": [[{ "key": "a", "colour": "red" }]], "right": [["x"]] }, "sym": null } }"""),
        )
    }

    @Test
    fun `rows must hold keys`() {
        assertEquals(
            "layers.base.right needs at least one row",
            failure("""{ "layers": { "base": { "left": [["a"]], "right": [] }, "sym": null } }"""),
        )
        assertEquals(
            "layers.base.right[0] needs at least one key",
            failure("""{ "layers": { "base": { "left": [["a"]], "right": [[]] }, "sym": null } }"""),
        )
        assertEquals(
            "layers.base.right[0][0] should be a key name or an object",
            failure("""{ "layers": { "base": { "left": [["a"]], "right": [[1]] }, "sym": null } }"""),
        )
    }

    @Test
    fun `text that isn't JSON is reported`() {
        assertTrue(failure("{ width: ").startsWith("not valid JSON"))
        assertFalse(failure("[]").isEmpty())
    }
}
