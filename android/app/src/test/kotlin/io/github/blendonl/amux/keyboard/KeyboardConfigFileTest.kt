package io.github.blendonl.amux.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class KeyboardConfigFileTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val defaults = """
        {
          "width": { "left": 25, "right": 25 },
          "layers": { "base": { "left": [["a"]], "right": [["b"]] } }
        }
    """.trimIndent()

    private val file by lazy { temporary.root.resolve("amux/keyboard.json") }
    private val config by lazy { KeyboardConfigFile(defaults, file) }

    private fun write(text: String, modified: Long) {
        file.parentFile!!.mkdirs()
        file.writeText(text)
        file.setLastModified(modified)
    }

    @Test
    fun `uses the defaults while there is no file`() {
        val loaded = config.loadIfChanged()!!

        assertEquals(25f, loaded.layout.leftPercent)
        assertNull(loaded.problem)
    }

    @Test
    fun `applies the file over the defaults`() {
        write("""{ "width": { "left": 30 } }""", modified = 1_000)

        val loaded = config.loadIfChanged()!!

        assertEquals(30f, loaded.layout.leftPercent)
        assertEquals(25f, loaded.layout.rightPercent)
    }

    @Test
    fun `loads again only after the file changes`() {
        write("""{ "width": { "left": 30 } }""", modified = 1_000)
        config.loadIfChanged()

        assertNull(config.loadIfChanged())

        write("""{ "width": { "left": 35 } }""", modified = 2_000)
        assertEquals(35f, config.loadIfChanged()!!.layout.leftPercent)

        file.delete()
        assertEquals(25f, config.loadIfChanged()!!.layout.leftPercent)
    }

    @Test
    fun `falls back to the defaults and reports a broken file`() {
        write("""{ "width": { "left": 80 } }""", modified = 1_000)

        val loaded = config.loadIfChanged()!!

        val problem = loaded.problem.orEmpty()
        assertEquals(25f, loaded.layout.leftPercent)
        assertTrue(problem, problem.startsWith("width.left should be between"))
    }
}
