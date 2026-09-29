package io.github.blendonl.amux

import java.io.File

data class AppDirs(
    val files: File,
    val cache: File,
    val data: File,
    val nativeLibraries: File,
)
