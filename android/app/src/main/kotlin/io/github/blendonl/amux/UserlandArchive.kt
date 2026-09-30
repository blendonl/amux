package io.github.blendonl.amux

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import java.util.zip.ZipEntry
import java.util.zip.ZipFile
import java.util.zip.ZipInputStream

class UserlandArchive(private val file: File) {
    fun extractTo(destination: File, posix: Posix, progress: (done: Int, total: Int) -> Unit) {
        val modes = ZipModes.read(file)
        ZipFile(file).use { zip ->
            val links = zip.getEntry(SYMLINKS)?.let { SymlinkList.parse(zip.readText(it)) }
                ?: throw IOException("${file.path} has no $SYMLINKS")
            val entries = zip.entries().asSequence().filter { it.name != SYMLINKS }.toList()
            val total = entries.size + links.size
            var done = 0
            Files.createDirectory(destination.toPath())
            posix.chmod(destination, PRIVATE_DIRECTORY)
            entries.forEach { entry ->
                extract(zip, entry, File(destination, relativePath(entry.name)), modes[entry.name], posix)
                progress(++done, total)
            }
            links.forEach { link ->
                val path = File(destination, link.path)
                Files.createDirectories(path.parentFile.toPath())
                posix.symlink(link.target, path)
                progress(++done, total)
            }
        }
    }

    private fun extract(zip: ZipFile, entry: ZipEntry, target: File, mode: Int?, posix: Posix) {
        if (entry.isDirectory) {
            Files.createDirectories(target.toPath())
            posix.chmod(target, mode ?: PRIVATE_DIRECTORY)
        } else {
            Files.createDirectories(target.parentFile.toPath())
            zip.getInputStream(entry).use { Files.copy(it, target.toPath()) }
            posix.chmod(target, mode ?: PRIVATE_FILE)
        }
    }

    private fun ZipFile.readText(entry: ZipEntry): String = getInputStream(entry).use { it.readBytes().decodeToString() }

    companion object {
        const val SYMLINKS = "SYMLINKS.txt"
        const val VERSION = "USERLAND_VERSION"
        const val PRIVATE_DIRECTORY = 0b111_000_000
        const val PRIVATE_FILE = 0b110_000_000

        fun versionOf(archive: InputStream): String =
            ZipInputStream(archive).use { zip ->
                generateSequence { zip.nextEntry }
                    .firstOrNull { it.name == VERSION }
                    ?.let { zip.readBytes().decodeToString().trim() }
                    ?: throw IOException("the userland archive has no $VERSION")
            }
    }
}
