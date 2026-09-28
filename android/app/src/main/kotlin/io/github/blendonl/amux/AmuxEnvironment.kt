package io.github.blendonl.amux

import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions

class AmuxEnvironment(
    filesDir: File,
    cacheDir: File,
    nativeLibraryDir: File,
    uid: Int,
    inherited: Map<String, String> = System.getenv(),
) {
    val home = File(filesDir, "home")
    val configHome = File(filesDir, "config")
    val stateHome = File(filesDir, "state")
    val binDir = File(filesDir, "bin")
    val tmpDir = cacheDir
    val binary = File(nativeLibraryDir, BINARY_NAME)
    val binaryLink = File(binDir, "amux")
    val initFile = File(configHome, "amux/init.lua")
    val runtimeDir = File(cacheDir, "amux-$uid")
    val socket = File(runtimeDir, "default")
    val serverLog = File(runtimeDir, "default.log")

    val variables: Map<String, String> = buildMap {
        put("HOME", home.path)
        put("XDG_CONFIG_HOME", configHome.path)
        put("XDG_STATE_HOME", stateHome.path)
        put("TMPDIR", tmpDir.path)
        put("SHELL", SYSTEM_SHELL)
        put("PATH", "${binDir.path}:$SYSTEM_BIN")
        put("LANG", "C.UTF-8")
        PASSED_THROUGH.forEach { name -> inherited[name]?.let { put(name, it) } }
    }

    val terminalVariables: Map<String, String> = variables + TERMINAL_VARIABLES

    fun prepare(serverName: String) {
        listOf(home, configHome, stateHome, binDir).forEach { Files.createDirectories(it.toPath()) }
        createPrivateDirectory(runtimeDir)
        refreshBinaryLink()
        writeInitFileIfMissing(serverName)
    }

    private fun createPrivateDirectory(dir: File) {
        val path = Files.createDirectories(dir.toPath())
        Files.setPosixFilePermissions(path, PRIVATE_PERMISSIONS)
    }

    private fun refreshBinaryLink() {
        val link = binaryLink.toPath()
        Files.deleteIfExists(link)
        Files.createSymbolicLink(link, binary.toPath())
    }

    private fun writeInitFileIfMissing(serverName: String) {
        if (initFile.exists()) return
        Files.createDirectories(initFile.toPath().parent)
        initFile.writeText("amux.opt.name = \"$serverName\"\n")
    }

    private companion object {
        const val BINARY_NAME = "libamux.so"
        const val SYSTEM_SHELL = "/system/bin/sh"
        const val SYSTEM_BIN = "/system/bin"
        val PASSED_THROUGH = listOf("ANDROID_ROOT", "ANDROID_DATA")
        val TERMINAL_VARIABLES = mapOf("TERM" to "xterm-256color", "COLORTERM" to "truecolor")
        val PRIVATE_PERMISSIONS: Set<PosixFilePermission> = PosixFilePermissions.fromString("rwx------")
    }
}
