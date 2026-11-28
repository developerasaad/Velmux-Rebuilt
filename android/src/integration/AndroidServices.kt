package com.velmux.mobile

import com.velmux.ui.app.PlatformServices
import java.io.File
import java.util.concurrent.atomic.AtomicLong

class AndroidServices(private val root: File) : PlatformServices {
    private val diagnosticCount = AtomicLong(0)
    val callbackCount: Long get() = diagnosticCount.get()

    override fun storageRoot(): String {
        return if (root.isDirectory || root.mkdirs()) root.absolutePath else ""
    }
    override fun platformName() = "Android"
    override fun onDiagnostic(sequence: Long) {
        diagnosticCount.incrementAndGet()
    }
}
