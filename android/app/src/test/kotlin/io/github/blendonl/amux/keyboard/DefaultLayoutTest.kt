package io.github.blendonl.amux.keyboard

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DefaultLayoutTest {
    private val layout = KeyboardLayoutParser.parse(File("src/test/resources/keyboard/default.json").readText())

    private fun widths(rows: KeyRows) = rows.map { row -> row.sumOf { it.width.toDouble() } }

    private fun Key.reaches(layer: String) =
        action == KeyAction.UseLayer(layer) || heldLatch?.action == KeyAction.UseLayer(layer)

    @Test
    fun `gives each half a fifth of the screen`() {
        assertEquals(21f, layout.leftPercent)
        assertEquals(21f, layout.rightPercent)
        assertEquals(300L, layout.holdMs)
        assertEquals(250L, layout.tapsMs)
    }

    @Test
    fun `each half has five columns under a top row and above a thumb row`() {
        for (side in Side.entries) {
            val rows = layout.baseRows(side)
            assertEquals(5, rows.size)
            rows.dropLast(1).forEach { assertEquals("$side", 5, it.size) }
        }
    }

    @Test
    fun `every layer keeps the base layer's shape on the halves it changes`() {
        for (layer in layout.layers.values) {
            for (side in Side.entries) {
                val rows = layer.rows(side) ?: continue
                assertEquals("${layer.name} $side", widths(layout.baseRows(side)), widths(rows))
            }
        }
    }

    @Test
    fun `a layer is held from the half it leaves alone`() {
        for (layer in layout.layers.values.filter { it.name != KeyboardLayout.BASE }) {
            val changed = Side.entries.filter { layer.rows(it) != null }
            val triggers = Side.entries.filter { side -> layout.baseRows(side).flatten().any { it.reaches(layer.name) } }

            assertEquals(layer.name, Side.entries - changed.toSet(), triggers)
        }
    }

    @Test
    fun `the base, sym and num layers type every printable ASCII character`() {
        val typed = listOf("base", "sym", "num")
            .map(layout.layers::getValue)
            .flatMap { layer -> Side.entries.flatMap { layer.rows(it).orEmpty().flatten() } }
            .map(Key::action)
            .filterIsInstance<KeyAction.Type>()
            .flatMap { listOf(it.text, it.shifted) }
            .toSet()

        val missing = (' '..'~').map(Char::toString).filterNot(typed::contains)
        assertEquals(emptyList<String>(), missing)
    }

    @Test
    fun `letters offer their capital, ctrl and alt forms on a hold`() {
        val f = layout.baseRows(Side.LEFT)[2][3]

        assertEquals("f", f.label)
        assertEquals(listOf("F", "^F", "M-f"), (f.hold as Hold.Choices).keys.map(Key::label))
    }

    @Test
    fun `the amux key sends the prefix and holds the amux layer`() {
        val amux = layout.baseRows(Side.LEFT)[4][2]

        assertEquals(KeyAction.Send("\u0002"), amux.action)
        assertEquals(KeyAction.UseLayer("amux"), amux.heldLatch?.action)
        assertTrue(layout.layers.getValue("amux").right!!.flatten().any { it.action == KeyAction.Send("\u0002c") })
    }
}
