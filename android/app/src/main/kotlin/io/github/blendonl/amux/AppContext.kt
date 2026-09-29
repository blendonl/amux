package io.github.blendonl.amux

import android.content.Context
import android.os.Build
import android.os.Process
import android.provider.Settings
import java.io.File
import java.io.IOException
import java.io.InputStream

private const val USERLAND_ASSETS = "userland"
private const val ZSHRC_ASSET = "dotfiles/zshrc"

fun Context.appDirs(): AppDirs =
    AppDirs(
        files = filesDir,
        cache = cacheDir,
        data = File(applicationInfo.dataDir),
        nativeLibraries = File(applicationInfo.nativeLibraryDir),
    )

fun Context.amuxEnvironment(withUserland: Boolean): AmuxEnvironment =
    AmuxEnvironment(
        dirs = appDirs(),
        uid = Process.myUid(),
        sdk = Build.VERSION.SDK_INT,
        withUserland = withUserland,
    )

fun Context.userlandInstaller(): UserlandInstaller =
    UserlandInstaller(UserlandLayout(appDirs()), { openUserlandArchive() }, AndroidPosix)

fun Context.zshrcTemplate(): String = assets.open(ZSHRC_ASSET).use { it.readBytes().decodeToString() }

fun Context.serverName(): String =
    ServerName.choose(Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME), Build.MODEL)

private fun Context.openUserlandArchive(): InputStream {
    val supported = Build.SUPPORTED_ABIS.toList()
    val abi = UserlandInstaller.abiFor(supported, assets.list(USERLAND_ASSETS).orEmpty().toList())
        ?: throw IOException("this APK has no userland for ${supported.joinToString()}")
    return assets.open("$USERLAND_ASSETS/$abi.zip")
}
