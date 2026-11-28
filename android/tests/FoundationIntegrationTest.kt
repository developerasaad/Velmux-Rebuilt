package com.velmux.mobile

import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.velmux.ui.app.FoundationSession
import com.velmux.ui.app.PlatformServices
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class FoundationIntegrationTest {
    @Test
    fun activityRecreationRetainsApplicationEngine() {
        val application = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as VelmuxApplication
        val expected = application.session
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            scenario.onActivity { assertSame(expected, (it.application as VelmuxApplication).session) }
            scenario.recreate()
            scenario.onActivity { assertSame(expected, (it.application as VelmuxApplication).session) }
        }
        // Composition disposal must not close the application-owned engine.
        assertTrue(expected.inspect().encoderCount > 0)
    }

    @Test
    fun generatedBindingsReachLinkedFfmpegAndNativeCallbacks() {
        val root = InstrumentationRegistry.getInstrumentation().targetContext.filesDir
        val services = AndroidServices(root)
        val session = FoundationSession(services)
        try {
            val first = session.inspect()
            val second = session.inspect()
            assertFalse(first.ffmpegVersion.isBlank())
            assertTrue(first.encoderCount > 0)
            assertTrue(first.decoderCount > 0)
            assertTrue(first.muxerCount > 0)
            assertTrue(first.demuxerCount > 0)
            assertTrue(first.filterCount > 0)
            assertEquals(root.absolutePath, first.storageRoot)
            assertEquals(first.sequence + 1, second.sequence)
            assertEquals(2L, services.callbackCount)
            assertTrue(session.verifyCancelledRequest())
            assertEquals(2L, services.callbackCount)
        } finally {
            session.close()
            session.close()
        }
    }

    @Test
    fun invalidNativeStorageIsTyped() {
        val session = FoundationSession(object : PlatformServices {
            override fun storageRoot() = ""
            override fun platformName() = "Android"
            override fun onDiagnostic(sequence: Long) = error("Invalid storage must fail before a callback")
        })
        try {
            assertTrue(session.verifyInvalidStorageRequest())
        } finally {
            session.close()
        }
    }
}
