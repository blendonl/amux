package io.github.blendonl.amux

import org.junit.Assert.assertEquals
import org.junit.Test

class ServerNameTest {
    @Test
    fun `lowercases a model name and joins its words with dashes`() {
        assertEquals("pixel-8-pro", ServerName.choose("Pixel 8 Pro"))
    }

    @Test
    fun `falls back to android when nothing is left`() {
        assertEquals("android", ServerName.choose("  "))
    }

    @Test
    fun `turns punctuation into single dashes`() {
        assertEquals("blendon-s-s23", ServerName.choose("Blendon's S23!"))
    }

    @Test
    fun `collapses runs and trims the ends`() {
        assertEquals("galaxy-tab-s9", ServerName.choose("--Galaxy__Tab  S9--"))
    }

    @Test
    fun `drops accents instead of the letters under them`() {
        assertEquals("cafe-phone", ServerName.choose("Café Phone"))
    }

    @Test
    fun `falls back to the next candidate when one sanitizes to nothing`() {
        assertEquals("sm-s911b", ServerName.choose("日本語", "SM-S911B"))
    }

    @Test
    fun `falls back to android without a candidate`() {
        assertEquals("android", ServerName.choose(null, null))
    }
}
