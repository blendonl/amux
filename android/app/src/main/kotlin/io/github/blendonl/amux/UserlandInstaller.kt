package io.github.blendonl.amux

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.FileSystemException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.attribute.BasicFileAttributes

class UserlandInstaller(
    private val layout: UserlandLayout,
    private val openArchive: () -> InputStream,
    private val posix: Posix,
) {
    sealed interface Outcome {
        data object Ready : Outcome

        data class Failed(val message: String) : Outcome
    }

    fun install(progress: (percent: Int) -> Unit): Outcome =
        try {
            clearLeftovers()
            replaceSymlink(layout.applib, layout.nativeLibraries.path)
            val bundled = openArchive().use(UserlandArchive::versionOf)
            if (!isCurrent(installedVersion(), bundled)) {
                stage(progress)
                swap()
            }
            linkPrefix()
            Outcome.Ready
        } catch (e: Exception) {
            runCatching { clearLeftovers() }
            Outcome.Failed(describe(e))
        }

    private fun clearLeftovers() {
        if (!exists(layout.prefix) && exists(layout.retired)) posix.rename(layout.retired, layout.prefix)
        deleteTree(layout.staging)
        deleteTree(layout.retired)
        deleteTree(layout.download)
    }

    private fun installedVersion(): String? = layout.version.takeIf { it.isFile }?.readText()

    private fun stage(progress: (percent: Int) -> Unit) {
        var reported = 0
        progress(reported)
        try {
            openArchive().use { archive -> Files.copy(archive, layout.download.toPath()) }
            UserlandArchive(layout.download).extractTo(layout.staging, posix) { done, total ->
                val percent = done * 100 / total
                if (percent != reported) {
                    reported = percent
                    progress(percent)
                }
            }
        } finally {
            deleteTree(layout.download)
        }
    }

    private fun swap() {
        if (exists(layout.prefix)) posix.rename(layout.prefix, layout.retired)
        posix.rename(layout.staging, layout.prefix)
        deleteTree(layout.retired)
    }

    private fun linkPrefix() {
        replaceSymlink(layout.amux, UserlandLayout.AMUX_TARGET)
        if (!layout.tmp.isDirectory) Files.createDirectory(layout.tmp.toPath())
        posix.chmod(layout.tmp, UserlandArchive.PRIVATE_DIRECTORY)
        deleteTree(layout.legacyBin)
    }

    private fun replaceSymlink(link: File, target: String) {
        if (Files.isDirectory(link.toPath(), LinkOption.NOFOLLOW_LINKS)) deleteTree(link)
        val fresh = File(link.parentFile, ".${link.name}.new")
        Files.deleteIfExists(fresh.toPath())
        posix.symlink(target, fresh)
        posix.rename(fresh, link)
    }

    private fun describe(e: Exception): String =
        when (e) {
            is FileSystemException -> listOfNotNull(e.file, e.reason ?: e.javaClass.simpleName).joinToString(": ")
            else -> e.message ?: e.javaClass.simpleName
        }

    companion object {
        fun isCurrent(installed: String?, bundled: String): Boolean = installed?.trim() == bundled.trim()

        fun abiFor(supported: List<String>, bundled: Collection<String>): String? =
            supported.firstOrNull { "$it.zip" in bundled }

        fun exists(file: File): Boolean = Files.exists(file.toPath(), LinkOption.NOFOLLOW_LINKS)

        fun deleteTree(root: File) {
            if (!exists(root)) return
            Files.walkFileTree(
                root.toPath(),
                object : SimpleFileVisitor<Path>() {
                    override fun visitFile(file: Path, attributes: BasicFileAttributes): FileVisitResult {
                        Files.delete(file)
                        return FileVisitResult.CONTINUE
                    }

                    override fun postVisitDirectory(dir: Path, failure: IOException?): FileVisitResult {
                        if (failure != null) throw failure
                        Files.delete(dir)
                        return FileVisitResult.CONTINUE
                    }
                },
            )
        }
    }
}
