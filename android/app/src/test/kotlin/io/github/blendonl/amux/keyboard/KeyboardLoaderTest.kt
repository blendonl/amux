package io.github.blendonl.amux.keyboard

import java.io.File
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class KeyboardLoaderTest {
    private val defaults = File("src/test/resources/keyboard/default.json").readText()
    private val configured = defaults.replace("\"left\": 21.0", "\"left\": 30.0")
    private val broken = "init.lua: amux.opt.android.keyboard.width.left must be between 5 and 45"
    private val asked = mutableListOf<Boolean>()

    private fun loader(configuredOutput: Result<String>) = KeyboardLoader { wantsDefaults ->
        asked += wantsDefaults
        if (wantsDefaults) Result.success(defaults) else configuredOutput
    }

    @Test
    fun `loads the configured keyboard`() {
        val loaded = loader(Result.success(configured)).load(keepCurrentOnFailure = false)

        assertEquals(30f, loaded.layout?.leftPercent)
        assertNull(loaded.problem)
        assertEquals(listOf(false), asked)
    }

    @Test
    fun `keeps the current keyboard when the config breaks`() {
        val loaded = loader(Result.failure(IOException(broken))).load(keepCurrentOnFailure = true)

        assertNull(loaded.layout)
        assertEquals(broken, loaded.problem)
        assertEquals(listOf(false), asked)
    }

    @Test
    fun `falls back to the defaults when there is no keyboard yet`() {
        val loaded = loader(Result.failure(IOException(broken))).load(keepCurrentOnFailure = false)

        assertEquals(21f, loaded.layout?.leftPercent)
        assertEquals(broken, loaded.problem)
        assertEquals(listOf(false, true), asked)
    }

    @Test
    fun `reports output it can't read`() {
        val loaded = loader(Result.success("{}")).load(keepCurrentOnFailure = true)

        assertEquals("width should be an object", loaded.problem)
    }
}
