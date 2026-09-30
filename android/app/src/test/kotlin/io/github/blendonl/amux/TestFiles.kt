package io.github.blendonl.amux

import java.io.File
import java.net.URI
import java.nio.file.FileSystems
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermission

object NioPosix : Posix {
    override fun chmod(file: File, mode: Int) {
        Files.setPosixFilePermissions(file.toPath(), permissions(mode))
    }

    override fun symlink(target: String, link: File) {
        Files.createSymbolicLink(link.toPath(), Paths.get(target))
    }

    override fun rename(from: File, to: File) {
        Files.move(from.toPath(), to.toPath(), StandardCopyOption.ATOMIC_MOVE)
    }
}

private val PERMISSION_ORDER = PosixFilePermission.values().toList()

fun permissions(mode: Int): Set<PosixFilePermission> =
    PERMISSION_ORDER.filterIndexed { index, _ -> mode and (0b100_000_000 ushr index) != 0 }.toSet()

fun modeOf(file: File): Int =
    Files.getPosixFilePermissions(file.toPath(), LinkOption.NOFOLLOW_LINKS)
        .sumOf { 0b100_000_000 ushr PERMISSION_ORDER.indexOf(it) }

fun linkTarget(file: File): String = Files.readSymbolicLink(file.toPath()).toString()

fun isLink(file: File): Boolean = Files.isSymbolicLink(file.toPath())

sealed interface ArchiveNode {
    val path: String

    data class Regular(override val path: String, val text: String, val mode: Int = 0b110_000_000) : ArchiveNode

    data class Directory(override val path: String, val mode: Int = 0b111_000_000) : ArchiveNode
}

fun writeArchive(zip: File, version: String, symlinks: String, nodes: List<ArchiveNode>) {
    Files.deleteIfExists(zip.toPath())
    val uri = URI.create("jar:${zip.toURI()}")
    FileSystems.newFileSystem(uri, mapOf("create" to "true", "enablePosixFileAttributes" to "true")).use { archive ->
        val root = archive.getPath("/")
        write(root.resolve(UserlandArchive.SYMLINKS), symlinks, 0b110_000_000)
        write(root.resolve(UserlandArchive.VERSION), "$version\n", 0b110_000_000)
        nodes.forEach { node ->
            val path = root.resolve(node.path)
            when (node) {
                is ArchiveNode.Directory -> {
                    Files.createDirectories(path)
                    Files.setPosixFilePermissions(path, permissions(node.mode))
                }
                is ArchiveNode.Regular -> {
                    path.parent?.let { Files.createDirectories(it) }
                    write(path, node.text, node.mode)
                }
            }
        }
    }
}

private fun write(path: Path, text: String, mode: Int) {
    Files.write(path, text.toByteArray())
    Files.setPosixFilePermissions(path, permissions(mode))
}
