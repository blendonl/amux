package io.github.blendonl.amux

import java.io.File

class UserlandLayout(dirs: AppDirs) {
    val prefix = File(dirs.files, "usr")
    val staging = File(dirs.files, "usr-staging")
    val retired = File(dirs.files, "usr-old")
    val download = File(dirs.files, "usr-staging.zip")
    val applib = File(dirs.files, "applib")
    val legacyBin = File(dirs.files, "bin")
    val nativeLibraries = dirs.nativeLibraries
    val version = File(prefix, UserlandArchive.VERSION)
    val bin = File(prefix, "bin")
    val tmp = File(prefix, "tmp")
    val shell = File(bin, "zsh")
    val amux = File(bin, "amux")
    val execPreload = File(prefix, "lib/libtermux-exec-direct-ld-preload.so")

    companion object {
        const val AMUX_TARGET = "../../applib/libamux.so"
    }
}
