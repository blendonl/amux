package io.github.blendonl.amux

import java.io.File
import java.nio.file.Files
import java.nio.file.LinkOption
import java.util.zip.ZipFile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class BundledUserlandTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val archive = File("src/main/assets/userland/x86_64.zip")
    private val jniLibs = File("src/main/jniLibs/x86_64")
    private lateinit var layout: UserlandLayout

    @Before
    fun installBundledArchive() {
        assumeTrue("run android/build.sh package first", archive.isFile && jniLibs.isDirectory)
        val dirs = AppDirs(temporary.newFolder("files"), temporary.newFolder("cache"), temporary.root, jniLibs.absoluteFile)
        layout = UserlandLayout(dirs)
        val outcome = UserlandInstaller(layout, { archive.inputStream() }, NioPosix).install {}
        assertEquals(UserlandInstaller.Outcome.Ready, outcome)
    }

    @Test
    fun `installs the version the archive carries`() {
        assertEquals(archive.inputStream().use(UserlandArchive::versionOf), layout.version.readText().trim())
    }

    @Test
    fun `runs zsh and amux from the native library directory`() {
        assertEquals("../../applib/libu_bin_zsh.so", linkTarget(layout.shell))
        assertTrue(File(layout.applib, "libu_bin_zsh.so").isFile)
        assertEquals(File(layout.applib, "libamux.so").canonicalFile, layout.amux.canonicalFile)
    }

    @Test
    fun `has the termux-exec library the pane environment preloads`() {
        assertTrue(Files.isRegularFile(layout.execPreload.toPath(), LinkOption.NOFOLLOW_LINKS))
    }

    @Test
    fun `resolves every link into applib to a packaged executable`() {
        val missing = links().filter { "applib/" in linkTarget(it) && !it.exists() }

        assertEquals(emptyList<File>(), missing)
    }

    @Test
    fun `creates every symlink the archive lists`() {
        val listed = ZipFile(archive).use { zip ->
            zip.getInputStream(zip.getEntry(UserlandArchive.SYMLINKS)).use { SymlinkList.parse(it.readBytes().decodeToString()) }
        }

        assertEquals(listed.map { it.path }.sorted(), links().map { it.relativeTo(layout.prefix).path }.sorted())
    }

    @Test
    fun `keeps the modes the packager recorded`() {
        assertEquals(0b111_000_000, modeOf(File(layout.prefix, "bin/curl-config")))
        assertEquals(0b110_000_000, modeOf(File(layout.prefix, "share/zsh/functions/Completion/compinit")))
    }

    private fun links(): List<File> =
        Files.walk(layout.prefix.toPath()).use { paths ->
            paths.iterator().asSequence().filter { Files.isSymbolicLink(it) }.map { it.toFile() }.toList()
        }.filter { it != layout.amux }
}
