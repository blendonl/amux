package io.github.blendonl.amux

import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class AmuxEnvironmentTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val filesDir by lazy { temporary.newFolder("files") }
    private val cacheDir by lazy { temporary.newFolder("cache") }

    private fun environment(
        nativeLibraryDir: File = File("/data/app/amux-1/lib/arm64"),
        inherited: Map<String, String> = emptyMap(),
    ) = AmuxEnvironment(filesDir, cacheDir, nativeLibraryDir, uid = 10123, inherited = inherited)

    @Test
    fun `builds the app environment inside private storage`() {
        val inherited = mapOf(
            "ANDROID_ROOT" to "/system",
            "ANDROID_DATA" to "/data",
            "LD_PRELOAD" to "/data/local/tmp/hook.so",
            "PATH" to "/sbin:/system/bin",
        )

        assertEquals(
            mapOf(
                "HOME" to "$filesDir/home",
                "XDG_CONFIG_HOME" to "$filesDir/config",
                "XDG_STATE_HOME" to "$filesDir/state",
                "TMPDIR" to "$cacheDir",
                "SHELL" to "/system/bin/sh",
                "PATH" to "$filesDir/bin:/system/bin",
                "LANG" to "C.UTF-8",
                "ANDROID_ROOT" to "/system",
                "ANDROID_DATA" to "/data",
            ),
            environment(inherited = inherited).variables,
        )
    }

    @Test
    fun `leaves out android variables the process does not have`() {
        val variables = environment(inherited = emptyMap()).variables

        assertFalse("ANDROID_ROOT" in variables)
        assertFalse("ANDROID_DATA" in variables)
    }

    @Test
    fun `gives the terminal client a terminal type on top of the app environment`() {
        val env = environment()

        assertEquals(
            env.variables + mapOf("TERM" to "xterm-256color", "COLORTERM" to "truecolor"),
            env.terminalVariables,
        )
    }

    @Test
    fun `uses the socket the binary computes from TMPDIR and the uid`() {
        assertEquals(File(cacheDir, "amux-10123/default"), environment().socket)
    }

    @Test
    fun `runs the binary from the native library directory`() {
        assertEquals(File("/data/app/amux-1/lib/arm64/libamux.so"), environment().binary)
    }

    @Test
    fun `creates the directories and a private runtime directory`() {
        val env = environment()

        env.prepare("pixel-8")

        listOf(env.home, env.configHome, env.stateHome, env.binDir).forEach { assertTrue(it.isDirectory) }
        assertEquals(
            PosixFilePermissions.fromString("rwx------"),
            Files.getPosixFilePermissions(env.runtimeDir.toPath()),
        )
    }

    @Test
    fun `writes the server name into init lua on first run`() {
        val env = environment()

        env.prepare("pixel-8-pro")

        assertEquals("amux.opt.name = \"pixel-8-pro\"\n", env.initFile.readText())
    }

    @Test
    fun `keeps an existing init lua`() {
        val env = environment()
        Files.createDirectories(env.initFile.toPath().parent)
        env.initFile.writeText("amux.opt.name = \"mine\"\n")

        env.prepare("pixel-8-pro")

        assertEquals("amux.opt.name = \"mine\"\n", env.initFile.readText())
    }

    @Test
    fun `does not rewrite init lua on later starts`() {
        environment().prepare("first-name")

        environment().prepare("second-name")

        assertEquals("amux.opt.name = \"first-name\"\n", environment().initFile.readText())
    }

    @Test
    fun `links amux on the path to the binary`() {
        val env = environment()

        env.prepare("pixel-8")

        assertEquals(env.binary.toPath(), Files.readSymbolicLink(env.binaryLink.toPath()))
    }

    @Test
    fun `refreshes the amux link when the native library directory moves`() {
        environment(nativeLibraryDir = File("/data/app/amux-1/lib/arm64")).prepare("pixel-8")

        val updated = environment(nativeLibraryDir = File("/data/app/amux-2/lib/arm64"))
        updated.prepare("pixel-8")

        assertEquals(
            File("/data/app/amux-2/lib/arm64/libamux.so").toPath(),
            Files.readSymbolicLink(updated.binaryLink.toPath()),
        )
    }
}
