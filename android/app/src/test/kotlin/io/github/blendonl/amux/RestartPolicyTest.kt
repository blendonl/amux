package io.github.blendonl.amux

import io.github.blendonl.amux.RestartPolicy.Decision
import kotlin.time.Duration.Companion.minutes
import kotlin.time.Duration.Companion.seconds
import org.junit.Assert.assertEquals
import org.junit.Test

class RestartPolicyTest {
    private val policy = RestartPolicy(quickRun = 30.seconds, firstDelay = 1.seconds, quickFailuresBeforeGivingUp = 5)

    @Test
    fun `stops when the server exits cleanly`() {
        assertEquals(Decision.Stop, policy.afterExit(status = 0, ranFor = 2.seconds))
    }

    @Test
    fun `backs off exponentially on quick failures and then gives up`() {
        val decisions = List(5) { policy.afterExit(status = 1, ranFor = 1.seconds) }

        assertEquals(
            listOf(
                Decision.Restart(1.seconds),
                Decision.Restart(2.seconds),
                Decision.Restart(4.seconds),
                Decision.Restart(8.seconds),
                Decision.GiveUp,
            ),
            decisions,
        )
    }

    @Test
    fun `a long run resets the backoff`() {
        repeat(3) { policy.afterExit(status = 1, ranFor = 1.seconds) }

        assertEquals(Decision.Restart(1.seconds), policy.afterExit(status = 1, ranFor = 10.minutes))
        assertEquals(Decision.Restart(1.seconds), policy.afterExit(status = 1, ranFor = 1.seconds))
    }
}
