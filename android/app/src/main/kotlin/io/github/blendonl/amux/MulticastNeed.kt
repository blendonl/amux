package io.github.blendonl.amux

import java.io.File

class MulticastNeed(private val hold: (Boolean) -> Unit) {
    private var lanPortFile: File? = null
    private var held = false

    var appVisible = false
        set(value) {
            field = value
            check()
        }

    fun follow(lanPortFile: File) {
        this.lanPortFile = lanPortFile
        check()
    }

    fun check() {
        val wanted = appVisible || lanPortFile?.isFile == true
        if (wanted == held) return
        held = wanted
        hold(wanted)
    }
}
