package io.github.blendonl.amux

import java.io.IOException

data class Symlink(val target: String, val path: String)

object SymlinkList {
    private const val ARROW = "←"
    private const val CURRENT_DIRECTORY = "./"

    fun parse(text: String): List<Symlink> =
        text.lineSequence()
            .withIndex()
            .filter { (_, line) -> line.isNotEmpty() }
            .map { (index, line) -> parseLine(index + 1, line) }
            .toList()

    private fun parseLine(number: Int, line: String): Symlink {
        val fields = line.split(ARROW)
        if (fields.size != 2 || fields[0].isEmpty()) {
            throw IOException("${UserlandArchive.SYMLINKS} line $number is not target${ARROW}path: $line")
        }
        return Symlink(fields[0], relativePath(fields[1].removePrefix(CURRENT_DIRECTORY)))
    }
}

fun relativePath(name: String): String {
    val path = name.removeSuffix("/")
    val segments = path.split('/')
    if (path.isEmpty() || path.startsWith('/') || segments.any { it.isEmpty() || it == "." || it == ".." }) {
        throw IOException("$name is not a path inside the userland")
    }
    return path
}
