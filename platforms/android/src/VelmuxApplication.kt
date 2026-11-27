package com.velmux.mobile

import android.app.Application
import com.velmux.ui.app.FoundationSession

class VelmuxApplication : Application() {
    lateinit var session: FoundationSession
        private set

    override fun onCreate() {
        super.onCreate()
        session = FoundationSession(AndroidServices(filesDir))
    }
}
