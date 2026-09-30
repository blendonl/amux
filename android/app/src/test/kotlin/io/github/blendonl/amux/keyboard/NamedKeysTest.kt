package io.github.blendonl.amux.keyboard

import android.view.KeyEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NamedKeysTest {
    @Test
    fun `a single character types itself and its US shifted form`() {
        assertEquals(KeyAction.Type("q", "Q"), NamedKeys.lookup("q")?.action)
        assertEquals(KeyAction.Type("1", "!"), NamedKeys.lookup("1")?.action)
        assertEquals(KeyAction.Type("'", "\""), NamedKeys.lookup("'")?.action)
        assertEquals(KeyAction.Type("\\", "|"), NamedKeys.lookup("\\")?.action)
        assertEquals(KeyAction.Type("é", "É"), NamedKeys.lookup("é")?.action)
        assertEquals(KeyAction.Type("{", "{"), NamedKeys.lookup("{")?.action)
    }

    @Test
    fun `labels show the shifted form while shift is on`() {
        val key = NamedKeys.lookup("2")!!

        assertEquals("2", key.label(shifted = false))
        assertEquals("@", key.label(shifted = true))
    }

    @Test
    fun `names map to special keys, modifiers and actions`() {
        assertEquals(KeyAction.Press(KeyEvent.KEYCODE_ESCAPE), NamedKeys.lookup("esc")?.action)
        assertEquals(KeyAction.Press(KeyEvent.KEYCODE_F12), NamedKeys.lookup("f12")?.action)
        assertEquals(KeyAction.Press(KeyEvent.KEYCODE_PAGE_DOWN), NamedKeys.lookup("PgDn")?.action)
        assertEquals(KeyAction.Modify(Modifier.CTRL), NamedKeys.lookup("ctrl")?.action)
        assertEquals(KeyAction.Send("\u0002"), NamedKeys.lookup("prefix")?.action)
        assertEquals(KeyAction.Type(" "), NamedKeys.lookup("space")?.action)
        assertEquals(KeyAction.UseLayer("sym"), NamedKeys.lookup("layer:sym")?.action)
    }

    @Test
    fun `movement and deletion keys repeat while held`() {
        listOf("bksp", "del", "up", "down", "left", "right", "pgup", "pgdn").forEach { name ->
            assertTrue(name, NamedKeys.lookup(name)!!.repeats)
        }
    }

    @Test
    fun `unknown names are not keys`() {
        assertNull(NamedKeys.lookup("escape"))
        assertNull(NamedKeys.lookup("layer:"))
        assertNull(NamedKeys.lookup("f13"))
    }
}
