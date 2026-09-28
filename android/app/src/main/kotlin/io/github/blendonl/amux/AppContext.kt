package io.github.blendonl.amux

import android.content.Context
import android.os.Build
import android.os.Process
import android.provider.Settings
import java.io.File

fun Context.amuxEnvironment(): AmuxEnvironment =
    AmuxEnvironment(
        filesDir = filesDir,
        cacheDir = cacheDir,
        nativeLibraryDir = File(applicationInfo.nativeLibraryDir),
        uid = Process.myUid(),
    )

fun Context.serverName(): String =
    ServerName.choose(Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME), Build.MODEL)
