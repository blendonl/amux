package io.github.blendonl.amux.keyboard

import io.github.blendonl.amux.keyboard.Latch.State
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LatchTest {
    private val latch = Latch()

    private fun tap() {
        latch.press()
        latch.release()
    }

    @Test
    fun `taps cycle through one shot, locked and off`() {
        tap()
        assertEquals(State.ONE_SHOT, latch.state)
        tap()
        assertEquals(State.LOCKED, latch.state)
        tap()
        assertEquals(State.OFF, latch.state)
    }

    @Test
    fun `a one shot ends with the next key`() {
        tap()
        latch.use()

        assertEquals(State.OFF, latch.state)
        assertFalse(latch.active)
    }

    @Test
    fun `a lock survives keys`() {
        tap()
        tap()
        latch.use()
        latch.use()

        assertEquals(State.LOCKED, latch.state)
        assertTrue(latch.active)
    }

    @Test
    fun `holding acts only while held when another key is used`() {
        latch.press()
        assertTrue(latch.active)
        latch.use()
        latch.use()
        assertTrue(latch.active)
        latch.release()

        assertEquals(State.OFF, latch.state)
        assertFalse(latch.active)
    }

    @Test
    fun `holding a locked latch keeps it locked`() {
        tap()
        tap()
        latch.press()
        latch.use()
        latch.release()

        assertEquals(State.LOCKED, latch.state)
    }

    @Test
    fun `holding a one shot latch uses it up`() {
        tap()
        latch.press()
        latch.use()
        latch.release()

        assertEquals(State.OFF, latch.state)
    }

    @Test
    fun `a release without a press changes nothing`() {
        latch.release()

        assertEquals(State.OFF, latch.state)
    }

    @Test
    fun `reset clears every state`() {
        tap()
        latch.press()
        latch.reset()

        assertEquals(State.OFF, latch.state)
        assertFalse(latch.active)
    }
}
