package io.github.blendonl.amux

import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

class RestartPolicy(
    private val quickRun: Duration = 30.seconds,
    private val firstDelay: Duration = 1.seconds,
    private val quickFailuresBeforeGivingUp: Int = 5,
) {
    sealed interface Decision {
        data object Stop : Decision

        data class Restart(val delay: Duration) : Decision

        data object GiveUp : Decision
    }

    private var quickFailures = 0

    fun afterExit(status: Int, ranFor: Duration): Decision {
        if (status == 0) return Decision.Stop
        if (ranFor >= quickRun) {
            quickFailures = 0
            return Decision.Restart(firstDelay)
        }
        quickFailures++
        if (quickFailures >= quickFailuresBeforeGivingUp) return Decision.GiveUp
        return Decision.Restart(firstDelay * (1 shl (quickFailures - 1)))
    }
}
