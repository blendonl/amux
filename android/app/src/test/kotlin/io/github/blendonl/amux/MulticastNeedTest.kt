package io.github.blendonl.amux

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class MulticastNeedTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val holds = mutableListOf<Boolean>()
    private val need = MulticastNeed { holds += it }
    private lateinit var lanPort: File

    @Before
    fun followLanPort() {
        lanPort = File(temporary.newFolder("default"), "lan-port")
        need.follow(lanPort)
    }

    @Test
    fun `holds nothing while the server does not listen on the lan`() {
        need.check()

        assertEquals(emptyList<Boolean>(), holds)
    }

    @Test
    fun `holds the lock while the lan port file exists`() {
        lanPort.writeText("40123\n")
        need.check()
        need.check()
        lanPort.delete()
        need.check()

        assertEquals(listOf(true, false), holds)
    }

    @Test
    fun `a port file being written does not count until it is renamed into place`() {
        File(lanPort.parentFile, "lan-port.tmp").writeText("40123\n")
        need.check()

        assertEquals(emptyList<Boolean>(), holds)
    }

    @Test
    fun `holds the lock while the app is visible`() {
        need.appVisible = true
        need.appVisible = true
        need.appVisible = false

        assertEquals(listOf(true, false), holds)
    }

    @Test
    fun `keeps the lock while either reason remains`() {
        need.appVisible = true
        lanPort.writeText("40123\n")
        need.check()
        need.appVisible = false
        lanPort.delete()
        need.check()

        assertEquals(listOf(true, false), holds)
    }

    @Test
    fun `an existing port file is held as soon as it is followed`() {
        val other = MulticastNeed { holds += it }
        lanPort.writeText("40123\n")

        other.follow(lanPort)

        assertEquals(listOf(true), holds)
    }
}
