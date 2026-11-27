package com.velmux.mobile.testing

import android.content.Context
import android.content.pm.PackageManager
import android.os.Binder
import android.os.Process
import java.io.File
import java.io.FileNotFoundException

/** The helper accepts only whitelisted generated media and the signed target app. */
internal object FixtureFiles {
    val names = listOf("video.mp4", "stereo.wav", "stereo.flac", "video-large.mp4", "positive-origin.mkv", "vfr.mkv", "rotated-sar.mp4", "multi-track.mkv", "hdr.mkv", "truncated.mp4", "invalid.bin")

    fun requireControlCaller(context: Context) {
        val uid = Binder.getCallingUid()
        if (uid == Process.myUid()) return
        val packages = context.packageManager.getPackagesForUid(uid).orEmpty()
        if ("com.velmux.mobile" !in packages ||
            context.packageManager.checkSignatures(Process.myUid(), uid) != PackageManager.SIGNATURE_MATCH) {
            throw SecurityException("Only the signed verification target may control manufactured fixtures")
        }
    }

    @Synchronized
    fun open(context: Context, id: String): File {
        if (id !in names) throw FileNotFoundException("Unknown verification source")
        val root = File(context.filesDir, "conversion-fixtures")
        if (!root.isDirectory && !root.mkdirs()) throw FileNotFoundException("Fixture storage is unavailable")
        val file = File(root, id)
        if (!file.isFile) {
            context.assets.open(id).use { input -> file.outputStream().use { input.copyTo(it) } }
        }
        return file
    }
}
