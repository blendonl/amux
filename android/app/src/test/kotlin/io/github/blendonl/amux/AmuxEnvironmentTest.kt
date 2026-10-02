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
    private val dataDir = File("/data/user/0/io.github.blendonl.amux")
    private val zshrcTemplate = "PROMPT='%n@{{host}} %~ %# '\n"

    private fun environment(
        withUserland: Boolean,
        nativeLibraryDir: File = File("/data/app/amux-1/lib/arm64"),
        inherited: Map<String, String> = emptyMap(),
    ) = AmuxEnvironment(
        AppDirs(filesDir, cacheDir, dataDir, nativeLibraryDir),
        uid = 10123,
        sdk = 35,
        withUserland = withUserland,
        inherited = inherited,
    )

    private val inherited = mapOf(
        "ANDROID_ROOT" to "/system",
        "ANDROID_DATA" to "/data",
        "BOOTCLASSPATH" to "/apex/com.android.art/javalib/core-oj.jar",
        "EXTERNAL_STORAGE" to "/sdcard",
        "LD_PRELOAD" to "/data/local/tmp/hook.so",
        "LD_LIBRARY_PATH" to "/data/local/tmp",
        "PATH" to "/sbin:/system/bin",
    )

    @Test
    fun `gives panes the userland once it is installed`() {
        val prefix = "$filesDir/usr"

        assertEquals(
            mapOf(
                "HOME" to "$filesDir/home",
                "XDG_CONFIG_HOME" to "$filesDir/config",
                "XDG_STATE_HOME" to "$filesDir/state",
                "PREFIX" to prefix,
                "TERMUX__PREFIX" to prefix,
                "TERMUX_APP__DATA_DIR" to "/data/user/0/io.github.blendonl.amux",
                "TERMUX_APP__LEGACY_DATA_DIR" to "/data/data/io.github.blendonl.amux",
                "LD_PRELOAD" to "$prefix/lib/libtermux-exec-direct-ld-preload.so",
                "TERMUX_EXEC__SYSTEM_LINKER_EXEC__MODE" to "disable",
                "ANDROID__BUILD_VERSION_SDK" to "35",
                "TMPDIR" to "$prefix/tmp",
                "PATH" to "$prefix/bin:/system/bin",
                "SHELL" to "$prefix/bin/zsh",
                "LANG" to "en_US.UTF-8",
                "ANDROID_ROOT" to "/system",
                "ANDROID_DATA" to "/data",
                "BOOTCLASSPATH" to "/apex/com.android.art/javalib/core-oj.jar",
                "EXTERNAL_STORAGE" to "/sdcard",
            ),
            environment(withUserland = true, inherited = inherited).variables,
        )
    }

    @Test
    fun `falls back to the system shell without the userland`() {
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
                "BOOTCLASSPATH" to "/apex/com.android.art/javalib/core-oj.jar",
                "EXTERNAL_STORAGE" to "/sdcard",
            ),
            environment(withUserland = false, inherited = inherited).variables,
        )
    }

    @Test
    fun `leaves out android variables the process does not have`() {
        listOf(true, false).forEach { withUserland ->
            val variables = environment(withUserland, inherited = emptyMap()).variables

            assertFalse("ANDROID_ROOT" in variables)
            assertFalse("ANDROID_DATA" in variables)
        }
    }

    @Test
    fun `gives the terminal client a terminal type on top of the app environment`() {
        listOf(true, false).forEach { withUserland ->
            val env = environment(withUserland)

            assertEquals(
                env.variables + mapOf("TERM" to "xterm-256color", "COLORTERM" to "truecolor"),
                env.terminalVariables,
            )
        }
    }

    @Test
    fun `puts the socket and the server log where the binary looks for them`() {
        val installed = environment(withUserland = true)
        val fallback = environment(withUserland = false)

        assertEquals(File(filesDir, "usr/tmp/amux-10123/default"), installed.socket)
        assertEquals(File(filesDir, "usr/tmp/amux-10123/default.log"), installed.serverLog)
        assertEquals(File(cacheDir, "amux-10123/default"), fallback.socket)
        assertEquals(File(cacheDir, "amux-10123/default.log"), fallback.serverLog)
    }

    @Test
    fun `watches the lan port file where the server writes it`() {
        listOf(true, false).forEach { withUserland ->
            val env = environment(withUserland)

            assertEquals(File(filesDir, "state/amux/default/lan-port"), env.lanPortFile)
        }
    }

    @Test
    fun `keeps the runtime directory inside the tmp directory of its environment`() {
        listOf(true, false).forEach { withUserland ->
            val env = environment(withUserland)

            assertEquals(File(env.variables.getValue("TMPDIR"), "amux-10123"), env.runtimeDir)
        }
    }

    @Test
    fun `runs the binary from the native library directory`() {
        assertEquals(File("/data/app/amux-1/lib/arm64/libamux.so"), environment(withUserland = true).binary)
    }

    @Test
    fun `creates the directories and private runtime and server state directories`() {
        listOf(true, false).forEach { withUserland ->
            val env = environment(withUserland)

            env.prepare("pixel-8", zshrcTemplate)

            listOf(env.home, env.configHome, env.stateHome).forEach { assertTrue(it.isDirectory) }
            listOf(env.runtimeDir, env.serverStateDir).forEach { dir ->
                assertEquals(PosixFilePermissions.fromString("rwx------"), Files.getPosixFilePermissions(dir.toPath()))
            }
        }
    }

    @Test
    fun `writes the server name into init lua on first run`() {
        val env = environment(withUserland = true)

        env.prepare("pixel-8-pro", zshrcTemplate)

        assertEquals("amux.opt.name = \"pixel-8-pro\"\n", env.initFile.readText())
    }

    @Test
    fun `keeps an existing init lua`() {
        val env = environment(withUserland = true)
        Files.createDirectories(env.initFile.toPath().parent)
        env.initFile.writeText("amux.opt.name = \"mine\"\n")

        env.prepare("pixel-8-pro", zshrcTemplate)

        assertEquals("amux.opt.name = \"mine\"\n", env.initFile.readText())
    }

    @Test
    fun `does not rewrite init lua on later starts`() {
        environment(withUserland = true).prepare("first-name", zshrcTemplate)

        environment(withUserland = true).prepare("second-name", zshrcTemplate)

        assertEquals("amux.opt.name = \"first-name\"\n", environment(withUserland = true).initFile.readText())
    }

    @Test
    fun `writes a zshrc with the server name as the host on first run`() {
        val env = environment(withUserland = true)

        env.prepare("pixel-8", zshrcTemplate)

        assertEquals("PROMPT='%n@pixel-8 %~ %# '\n", File(filesDir, "home/.zshrc").readText())
    }

    @Test
    fun `keeps an existing zshrc`() {
        val env = environment(withUserland = true)
        Files.createDirectories(env.home.toPath())
        env.zshrc.writeText("PROMPT='mine '\n")

        env.prepare("pixel-8", zshrcTemplate)
        environment(withUserland = true).prepare("pixel-9", zshrcTemplate)

        assertEquals("PROMPT='mine '\n", env.zshrc.readText())
    }

    @Test
    fun `ships a zshrc with completion, history in home, emacs keys and a host placeholder`() {
        val shipped = File("src/main/assets/dotfiles/zshrc").readText()

        listOf("compinit", "HISTFILE=\$HOME/.zsh_history", "bindkey -e", "%n@{{host}} %~").forEach {
            assertTrue(it, it in shipped)
        }
    }

    @Test
    fun `links amux on the path to the binary without the userland`() {
        val env = environment(withUserland = false)

        env.prepare("pixel-8", zshrcTemplate)

        assertEquals(env.binary.toPath(), Files.readSymbolicLink(env.binaryLink.toPath()))
    }

    @Test
    fun `leaves the bin directory to the installer with the userland`() {
        val env = environment(withUserland = true)

        env.prepare("pixel-8", zshrcTemplate)

        assertFalse(env.binDir.exists())
    }

    @Test
    fun `refreshes the amux link when the native library directory moves`() {
        environment(withUserland = false, nativeLibraryDir = File("/data/app/amux-1/lib/arm64"))
            .prepare("pixel-8", zshrcTemplate)

        val updated = environment(withUserland = false, nativeLibraryDir = File("/data/app/amux-2/lib/arm64"))
        updated.prepare("pixel-8", zshrcTemplate)

        assertEquals(
            File("/data/app/amux-2/lib/arm64/libamux.so").toPath(),
            Files.readSymbolicLink(updated.binaryLink.toPath()),
        )
    }
}
