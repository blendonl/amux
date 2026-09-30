package io.github.blendonl.amux.keyboard

import java.io.File
import java.io.IOException

class KeyboardConfigFile(private val defaults: String, val file: File) {
    class Loaded(val layout: KeyboardLayout, val problem: String?)

    private data class Stamp(val modified: Long, val length: Long)

    private var loaded: Stamp? = null

    fun loadIfChanged(): Loaded? {
        val stamp = Stamp(file.lastModified(), file.length())
        if (stamp == loaded) return null
        loaded = stamp
        return try {
            Loaded(KeyboardLayoutParser.parse(defaults, if (file.isFile) file.readText() else null), null)
        } catch (e: KeyboardLayoutException) {
            Loaded(KeyboardLayoutParser.parse(defaults), e.message)
        } catch (e: IOException) {
            Loaded(KeyboardLayoutParser.parse(defaults), e.message ?: e.javaClass.simpleName)
        }
    }
}
