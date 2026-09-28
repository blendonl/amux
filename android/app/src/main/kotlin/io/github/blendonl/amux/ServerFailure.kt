package io.github.blendonl.amux

import java.io.File
import kotlin.time.Duration

sealed interface ServerFailure {
    data class Exited(val status: Int, val lastLogLine: String?) : ServerFailure

    data class NoAnswer(val socket: File, val timeout: Duration) : ServerFailure

    data class Unlaunchable(val binary: File, val message: String?) : ServerFailure

    data class Unprepared(val directory: File, val message: String?) : ServerFailure
}
