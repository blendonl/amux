package io.github.blendonl.amux.keyboard

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class ConfigStampTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private lateinit var dir: File
    private lateinit var init: File

    @Before
    fun writeConfig() {
        dir = temporary.newFolder("amux")
        init = File(dir, "init.lua").apply { writeText("amux.opt.name = \"phone\"\n") }
    }

    private fun write(path: String, text: String) = File(dir, path).apply {
        parentFile?.mkdirs()
        writeText(text)
    }

    @Test
    fun `an untouched config keeps its stamp`() {
        assertEquals(ConfigStamp.of(dir), ConfigStamp.of(dir))
    }

    @Test
    fun `an edit that changes the size changes the stamp`() {
        val before = ConfigStamp.of(dir)
        init.appendText("amux.opt.android.keyboard = {}\n")

        assertNotEquals(before, ConfigStamp.of(dir))
    }

    @Test
    fun `an edit of the same size changes the stamp through the modification time`() {
        val before = ConfigStamp.of(dir)
        init.writeText("amux.opt.name = \"tablet\"\n".take(init.length().toInt()))
        init.setLastModified(init.lastModified() + 2_000)

        assertNotEquals(before, ConfigStamp.of(dir))
    }

    @Test
    fun `a required module counts`() {
        val module = write("lua/keys.lua", "return {}\n")
        val before = ConfigStamp.of(dir)
        module.writeText("return { ctrl = true }\n")

        assertNotEquals(before, ConfigStamp.of(dir))
    }

    @Test
    fun `adding or removing a module changes the stamp`() {
        val before = ConfigStamp.of(dir)
        val module = write("lua/keys/init.lua", "return {}\n")
        val added = ConfigStamp.of(dir)
        module.delete()

        assertNotEquals(before, added)
        assertEquals(before, ConfigStamp.of(dir))
    }

    @Test
    fun `files that are not lua do not count`() {
        val before = ConfigStamp.of(dir)
        write(".init.lua.swp", "swap")
        write("servers.lua.123.tmp", "partial")
        write("lua/notes.txt", "notes")

        assertEquals(before, ConfigStamp.of(dir))
    }

    @Test
    fun `paths are relative to the config dir and sorted`() {
        write("servers.lua", "return {}\n")
        write("lua/a.lua", "return {}\n")

        assertEquals(listOf("init.lua", "lua/a.lua", "servers.lua"), ConfigStamp.of(dir).files.map { it.path })
    }

    @Test
    fun `a missing config dir has an empty stamp`() {
        assertEquals(ConfigStamp(emptyList()), ConfigStamp.of(File(dir, "missing")))
    }
}
