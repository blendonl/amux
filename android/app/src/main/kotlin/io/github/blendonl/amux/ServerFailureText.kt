package io.github.blendonl.amux

import android.content.Context

fun ServerFailure.describe(context: Context): String =
    when (this) {
        is ServerFailure.Exited ->
            if (lastLogLine == null) context.getString(R.string.failure_exit, status)
            else context.getString(R.string.failure_exit_with_log, status, lastLogLine)
        is ServerFailure.NoAnswer ->
            context.getString(R.string.failure_no_answer, socket.path, timeout.inWholeSeconds.toInt())
        is ServerFailure.Unlaunchable ->
            context.getString(R.string.failure_launch, binary.path, message.orEmpty())
        is ServerFailure.Unprepared ->
            context.getString(R.string.failure_prepare, directory.path, message.orEmpty())
    }

fun ServerState.Restarting.describe(context: Context): String =
    context.getString(R.string.server_restarting, failure.describe(context), delay.inWholeSeconds.toInt())

fun ServerState.Failed.describe(context: Context): String =
    context.getString(R.string.server_failed, failure.describe(context))
