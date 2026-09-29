package io.github.blendonl.amux.keyboard

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DefaultLayoutTest {
    private val layout = KeyboardLayoutParser.parse(File("src/main/res/raw/keyboard.json").readText())

    private fun widths(rows: KeyRows) = rows.map { row -> row.sumOf { it.width.toDouble() } }

    @Test
    fun `splits the screen into quarters around the terminal`() {
        assertEquals(25f, layout.leftPercent)
        assertEquals(25f, layout.rightPercent)
    }

    @Test
    fun `every layer keeps the base layer's shape`() {
        for (layer in layout.layers.values) {
            for (side in Side.entries) {
                assertEquals("${layer.name} $side", widths(layout.base.rows(side)), widths(layer.rows(side)))
            }
        }
    }

    @Test
    fun `every layer can reach every other layer and every modifier`() {
        val wanted = layout.layers.keys.map(KeyAction::UseLayer) + Modifier.entries.map(KeyAction::Modify)
        for (layer in layout.layers.values) {
            val actions = Side.entries.flatMap { side -> layer.rows(side).flatten().map(Key::action) }
            assertTrue(layer.name, actions.containsAll(wanted.filter { it != KeyAction.UseLayer(KeyboardLayout.BASE) }))
        }
    }

    @Test
    fun `the base and sym layers type every printable ASCII character`() {
        val base = layout.base
        val sym = layout.layers.getValue("sym")
        val typed = listOf(base, sym)
            .flatMap { layer -> Side.entries.flatMap { layer.rows(it).flatten() } }
            .map(Key::action)
            .filterIsInstance<KeyAction.Type>()
            .flatMap { listOf(it.text, it.shifted) }
            .toSet()

        val missing = (' '..'~').map(Char::toString).filterNot(typed::contains)
        assertEquals(emptyList<String>(), missing)
    }
}
