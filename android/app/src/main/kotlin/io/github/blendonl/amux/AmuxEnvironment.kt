package io.github.blendonl.amux

import android.annotation.SuppressLint
import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions

class AmuxEnvironment(
    dirs: AppDirs,
    uid: Int,
    sdk: Int,
    val withUserland: Boolean,
    inherited: Map<String, String> = System.getenv(),
) {
    val userland = UserlandLayout(dirs)
    val home = File(dirs.files, "home")
    val configHome = File(dirs.files, "config")
    val stateHome = File(dirs.files, "state")
    val binDir = userland.legacyBin
    val tmpDir = if (withUserland) userland.tmp else dirs.cache
    val binary = File(dirs.nativeLibraries, BINARY_NAME)
    val binaryLink = File(binDir, "amux")
    val configDir = File(configHome, "amux")
    val initFile = File(configDir, "init.lua")
    val zshrc = File(home, ".zshrc")
    val runtimeDir = File(tmpDir, "amux-$uid")
    val socket = File(runtimeDir, "default")
    val serverLog = File(runtimeDir, "default.log")

    val variables: Map<String, String> = buildMap {
        put("HOME", home.path)
        put("XDG_CONFIG_HOME", configHome.path)
        put("XDG_STATE_HOME", stateHome.path)
        if (withUserland) {
            put("PREFIX", userland.prefix.path)
            put("TERMUX__PREFIX", userland.prefix.path)
            put("TERMUX_APP__DATA_DIR", dirs.data.path)
            put("TERMUX_APP__LEGACY_DATA_DIR", LEGACY_DATA_DIR)
            put("LD_PRELOAD", userland.execPreload.path)
            put("TERMUX_EXEC__SYSTEM_LINKER_EXEC__MODE", "disable")
            put("ANDROID__BUILD_VERSION_SDK", sdk.toString())
            put("TMPDIR", tmpDir.path)
            put("PATH", "${userland.bin.path}:$SYSTEM_BIN")
            put("SHELL", userland.shell.path)
            put("LANG", "en_US.UTF-8")
        } else {
            put("TMPDIR", tmpDir.path)
            put("SHELL", SYSTEM_SHELL)
            put("PATH", "${binDir.path}:$SYSTEM_BIN")
            put("LANG", "C.UTF-8")
        }
        PASSED_THROUGH.forEach { name -> inherited[name]?.let { put(name, it) } }
    }

    val terminalVariables: Map<String, String> = variables + TERMINAL_VARIABLES

    fun prepare(serverName: String, zshrcTemplate: String) {
        listOf(home, configHome, stateHome).forEach { Files.createDirectories(it.toPath()) }
        if (!withUserland) refreshBinaryLink()
        createPrivateDirectory(runtimeDir)
        writeIfMissing(initFile, "amux.opt.name = \"$serverName\"\n")
        writeIfMissing(zshrc, zshrcTemplate.replace(HOST_PLACEHOLDER, serverName))
    }

    private fun createPrivateDirectory(dir: File) {
        val path = Files.createDirectories(dir.toPath())
        Files.setPosixFilePermissions(path, PRIVATE_PERMISSIONS)
    }

    private fun refreshBinaryLink() {
        Files.createDirectories(binDir.toPath())
        val link = binaryLink.toPath()
        Files.deleteIfExists(link)
        Files.createSymbolicLink(link, binary.toPath())
    }

    private fun writeIfMissing(file: File, text: String) {
        if (file.exists()) return
        Files.createDirectories(file.toPath().parent)
        file.writeText(text)
    }

    private companion object {
        const val BINARY_NAME = "libamux.so"
        const val SYSTEM_SHELL = "/system/bin/sh"
        const val SYSTEM_BIN = "/system/bin"
        @SuppressLint("SdCardPath")
        const val LEGACY_DATA_DIR = "/data/data/io.github.blendonl.amux"
        const val HOST_PLACEHOLDER = "{{host}}"
        val PASSED_THROUGH = listOf(
            "ANDROID_ART_ROOT",
            "ANDROID_ASSETS",
            "ANDROID_DATA",
            "ANDROID_I18N_ROOT",
            "ANDROID_ROOT",
            "ANDROID_RUNTIME_ROOT",
            "ANDROID_STORAGE",
            "ANDROID_TZDATA_ROOT",
            "ASEC_MOUNTPOINT",
            "BOOTCLASSPATH",
            "DEX2OATBOOTCLASSPATH",
            "EXTERNAL_STORAGE",
            "LOOP_MOUNTPOINT",
            "SYSTEMSERVERCLASSPATH",
        )
        val TERMINAL_VARIABLES = mapOf("TERM" to "xterm-256color", "COLORTERM" to "truecolor")
        val PRIVATE_PERMISSIONS: Set<PosixFilePermission> = PosixFilePermissions.fromString("rwx------")
    }
}
