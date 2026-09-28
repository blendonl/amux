package io.github.blendonl.amux

import kotlin.time.Duration

sealed interface ServerState {
    data object Starting : ServerState

    data object Ready : ServerState

    data class Restarting(val failure: ServerFailure, val delay: Duration) : ServerState

    data class Failed(val failure: ServerFailure) : ServerState

    data object Stopping : ServerState

    data object Stopped : ServerState
}
