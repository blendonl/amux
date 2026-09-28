package io.github.blendonl.amux

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.time.Duration

fun Process.waitFor(timeout: Duration): Boolean =
    waitFor(timeout.inWholeMilliseconds, TimeUnit.MILLISECONDS)

fun CountDownLatch.await(timeout: Duration): Boolean =
    await(timeout.inWholeMilliseconds, TimeUnit.MILLISECONDS)
