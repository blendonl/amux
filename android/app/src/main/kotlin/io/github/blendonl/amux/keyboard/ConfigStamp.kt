package io.github.blendonl.amux.keyboard

import java.io.File

data class ConfigStamp(val files: List<FileStamp>) {
    data class FileStamp(val path: String, val size: Long, val modifiedMs: Long)

    companion object {
        private const val CONFIG_SUFFIX = ".lua"

        fun isConfig(name: String): Boolean = name.endsWith(CONFIG_SUFFIX)

        fun of(dir: File): ConfigStamp = ConfigStamp(
            dir.walkTopDown()
                .filter { it.isFile && isConfig(it.name) }
                .map { FileStamp(it.relativeTo(dir).path, it.length(), it.lastModified()) }
                .sortedBy(FileStamp::path)
                .toList(),
        )
    }
}
