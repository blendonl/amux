package io.github.blendonl.amux

import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class UserlandInstallerTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val filesDir by lazy { temporary.newFolder("files") }
    private val archive by lazy { File(temporary.root, "x86_64.zip") }
    private val nativeLibraries = File("/data/app/amux-1/lib/x86_64")

    private fun layout(libraries: File = nativeLibraries) =
        UserlandLayout(AppDirs(filesDir, File(temporary.root, "cache"), temporary.root, libraries))

    private fun installer(libraries: File = nativeLibraries) =
        UserlandInstaller(layout(libraries), { archive.inputStream() }, NioPosix)

    private fun install(libraries: File = nativeLibraries, progress: MutableList<Int> = mutableListOf()) =
        installer(libraries).install { progress += it }

    private fun writeVersion(version: String, extra: List<ArchiveNode> = emptyList()) =
        writeArchive(
            archive,
            version,
            symlinks = "../../applib/libu_bin_zsh.so←./bin/zsh\ndash←./bin/sh\n../etc/profile←./share/profile\n",
            nodes = listOf(
                ArchiveNode.Regular("bin/zcat", "#!/bin/sh\nexec gzip -cd \"\$@\"\n", 0b111_000_000),
                ArchiveNode.Regular("etc/profile", "export PATH\n"),
                ArchiveNode.Regular("share/doc/zsh/README", "zsh $version\n"),
                ArchiveNode.Directory("var/empty"),
            ) + extra,
        )

    @Test
    fun `installs the archive into usr with its modes and symlinks`() {
        writeVersion("one")

        assertEquals(UserlandInstaller.Outcome.Ready, install())

        val usr = File(filesDir, "usr")
        assertEquals("one\n", File(usr, "USERLAND_VERSION").readText())
        assertEquals(0b111_000_000, modeOf(File(usr, "bin/zcat")))
        assertEquals(0b110_000_000, modeOf(File(usr, "etc/profile")))
        assertEquals(0b111_000_000, modeOf(File(usr, "var/empty")))
        assertEquals("../../applib/libu_bin_zsh.so", linkTarget(File(usr, "bin/zsh")))
        assertEquals("dash", linkTarget(File(usr, "bin/sh")))
        assertEquals("export PATH\n", File(usr, "share/profile").readText())
        assertFalse(File(usr, UserlandArchive.SYMLINKS).exists())
    }

    @Test
    fun `links applib to the native library directory and amux through it`() {
        writeVersion("one")

        install()

        assertEquals(nativeLibraries.path, linkTarget(File(filesDir, "applib")))
        assertEquals("../../applib/libamux.so", linkTarget(File(filesDir, "usr/bin/amux")))
    }

    @Test
    fun `creates a private tmp directory in the prefix`() {
        writeVersion("one")

        install()

        assertEquals(0b111_000_000, modeOf(File(filesDir, "usr/tmp")))
    }

    @Test
    fun `leaves no staging, old prefix or downloaded archive behind`() {
        writeVersion("one")
        install()
        writeVersion("two")

        install()

        assertEquals(listOf("applib", "usr"), filesDir.list()!!.sorted())
    }

    @Test
    fun `reports progress up to one hundred percent`() {
        writeVersion("one")
        val progress = mutableListOf<Int>()

        install(progress = progress)

        assertEquals(0, progress.first())
        assertEquals(100, progress.last())
        assertEquals(progress.distinct().sorted(), progress)
    }

    @Test
    fun `skips the install when the installed version matches`() {
        writeVersion("one")
        install()
        File(filesDir, "usr/tmp/session").writeText("kept")
        val progress = mutableListOf<Int>()

        assertEquals(UserlandInstaller.Outcome.Ready, install(progress = progress))

        assertEquals(emptyList<Int>(), progress)
        assertEquals("kept", File(filesDir, "usr/tmp/session").readText())
    }

    @Test
    fun `replaces the prefix when the bundled version changes`() {
        writeVersion("one", extra = listOf(ArchiveNode.Regular("share/only-in-one", "gone\n")))
        install()

        writeVersion("two")
        install()

        assertEquals("two\n", File(filesDir, "usr/USERLAND_VERSION").readText())
        assertEquals("zsh two\n", File(filesDir, "usr/share/doc/zsh/README").readText())
        assertFalse(File(filesDir, "usr/share/only-in-one").exists())
    }

    @Test
    fun `never touches home`() {
        val home = File(filesDir, "home").apply { mkdirs() }
        File(home, ".zshrc").writeText("mine")
        writeVersion("one")
        install()
        writeVersion("two")

        install()

        assertEquals("mine", File(home, ".zshrc").readText())
    }

    @Test
    fun `refreshes applib when the native library directory moves`() {
        writeVersion("one")
        install(libraries = File("/data/app/amux-1/lib/x86_64"))

        install(libraries = File("/data/app/amux-2/lib/x86_64"))

        assertEquals("/data/app/amux-2/lib/x86_64", linkTarget(File(filesDir, "applib")))
    }

    @Test
    fun `drops the bin directory of the app without a userland`() {
        val legacyBin = File(filesDir, "bin").apply { mkdirs() }
        Files.createSymbolicLink(File(legacyBin, "amux").toPath(), File(nativeLibraries, "libamux.so").toPath())
        writeVersion("one")

        install()

        assertFalse(UserlandInstaller.exists(legacyBin))
    }

    @Test
    fun `clears a staging directory and an old prefix left by an interrupted install`() {
        writeVersion("one")
        install()
        File(filesDir, "usr-staging/bin").mkdirs()
        File(filesDir, "usr-old/share").mkdirs()
        File(filesDir, "usr-staging.zip").writeText("partial")

        install()

        assertEquals(listOf("applib", "usr"), filesDir.list()!!.sorted())
    }

    @Test
    fun `puts the old prefix back when a swap stopped halfway`() {
        writeVersion("one")
        install()
        File(filesDir, "usr").renameTo(File(filesDir, "usr-old"))
        File(filesDir, "usr-staging/bin").mkdirs()
        val progress = mutableListOf<Int>()

        install(progress = progress)

        assertEquals(emptyList<Int>(), progress)
        assertEquals("one\n", File(filesDir, "usr/USERLAND_VERSION").readText())
        assertEquals(listOf("applib", "usr"), filesDir.list()!!.sorted())
    }

    @Test
    fun `deletes leftovers without following their symlinks`() {
        val outside = temporary.newFolder("outside")
        File(outside, "keep").writeText("keep")
        File(filesDir, "usr-old").mkdirs()
        Files.createSymbolicLink(File(filesDir, "usr-old/link").toPath(), outside.toPath())
        writeVersion("one")

        install()

        assertEquals("keep", File(outside, "keep").readText())
    }

    @Test
    fun `keeps the previous prefix when the new archive is damaged`() {
        writeVersion("one")
        install()
        archive.writeText("not a zip")

        val outcome = install()

        assertTrue(outcome is UserlandInstaller.Outcome.Failed)
        assertEquals("one\n", File(filesDir, "usr/USERLAND_VERSION").readText())
        assertEquals(listOf("applib", "usr"), filesDir.list()!!.sorted())
    }

    @Test
    fun `fails without a prefix when the archive has a path outside the prefix`() {
        writeArchive(archive, "one", symlinks = "x←./../escape\n", nodes = emptyList())

        val outcome = install()

        assertTrue(outcome is UserlandInstaller.Outcome.Failed)
        assertFalse(File(filesDir, "usr").exists())
        assertFalse(File(temporary.root, "escape").exists())
    }

    @Test
    fun `fails when the archive has no version`() {
        archive.outputStream().use { java.util.zip.ZipOutputStream(it).close() }

        val outcome = install()

        assertEquals(UserlandInstaller.Outcome.Failed("the userland archive has no USERLAND_VERSION"), outcome)
    }

    @Test
    fun `compares versions without surrounding whitespace`() {
        assertTrue(UserlandInstaller.isCurrent("abc123\n", "abc123"))
        assertFalse(UserlandInstaller.isCurrent("abc123\n", "def456"))
        assertFalse(UserlandInstaller.isCurrent(null, "abc123"))
    }

    @Test
    fun `picks the first supported abi the apk has a userland for`() {
        val bundled = listOf("arm64-v8a.zip", "x86_64.zip")

        assertEquals("x86_64", UserlandInstaller.abiFor(listOf("x86_64", "x86", "arm64-v8a"), bundled))
        assertEquals("arm64-v8a", UserlandInstaller.abiFor(listOf("arm64-v8a", "armeabi-v7a"), bundled))
        assertNull(UserlandInstaller.abiFor(listOf("armeabi-v7a", "armeabi"), bundled))
    }
}
