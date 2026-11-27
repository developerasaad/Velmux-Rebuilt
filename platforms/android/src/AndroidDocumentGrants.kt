package com.velmux.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import com.velmux.ui.app.PlatformFailure

/** Records only OS permission ownership; Rust supplies every retention decision. */
internal object AndroidDocumentGrants {
    private const val PREFERENCES = "velmux-document-grant-receipts"
    private const val OWNED = "owned-read-uris"
    private val gate = Any()
    private var nextSelection = 0L
    private var selection: Long? = null
    private var deliveredLocator: String? = null
    private val deferredReleases = mutableSetOf<String>()

    fun beginSelection(context: Context): Long = synchronized(gate) {
        check(selection == null) { "A document selection is already open." }
        deliveredLocator = null
        drainReleases(context)
        (++nextSelection).also { selection = it }
    }

    fun finishSelection(context: Context, token: Long, locator: String?) = synchronized(gate) {
        if (selection != token) return@synchronized
        selection = null
        deliveredLocator = locator
        drainReleases(context)
    }

    fun retain(context: Context, locator: String): Unit = synchronized(gate) {
        try {
            retainPermission(context, locator)
            deferredReleases.remove(locator)
            Unit
        } finally {
            if (deliveredLocator == locator) deliveredLocator = null
            drainReleases(context)
        }
    }

    private fun retainPermission(context: Context, locator: String) {
        val uri = Uri.parse(locator)
        if (uri.scheme != "content") throw PlatformFailure("The source does not have a document URI.")
        val resolver = context.contentResolver
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val owned = preferences.getStringSet(OWNED, emptySet()).orEmpty().toMutableSet()
        val permission = resolver.persistedUriPermissions.firstOrNull { it.uri == uri && it.isReadPermission }
        val existing = permission != null
        val receiptKey = receiptKey(locator)
        val ownsCurrentGrant = locator in owned && permission?.persistedTime == preferences.getLong(receiptKey, Long.MIN_VALUE)
        // A different application facility may already own this grant. Do not
        // acquire a receipt that would later let media cleanup release it.
        if (existing && !ownsCurrentGrant) {
            // A revoked/reacquired permission is a different OS grant, even if
            // a previous receipt could not be removed during storage failure.
            if (owned.remove(locator)) preferences.edit().putStringSet(OWNED, owned).remove(receiptKey).commit()
            return
        }
        try {
            if (!existing) resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            if (!ownsCurrentGrant) {
                val acquired = resolver.persistedUriPermissions.firstOrNull { it.uri == uri && it.isReadPermission }
                    ?: throw PlatformFailure("The operating system did not retain document access.")
                owned.add(locator)
                if (!preferences.edit().putStringSet(OWNED, owned).putLong(receiptKey, acquired.persistedTime).commit()) {
                    if (!existing) runCatching { resolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
                    throw PlatformFailure("The document access receipt could not be saved. Check private storage and try again.")
                }
            }
        } catch (error: PlatformFailure) {
            throw error
        } catch (_: SecurityException) {
            throw PlatformFailure("Lasting access to this document is unavailable. Select it again using the system document picker.")
        }
    }

    fun release(context: Context, locator: String) {
        synchronized(gate) {
            if (selection != null || deliveredLocator == locator) {
                // Releasing persisted access also destroys a newly delivered
                // persistable offer on Android. Hold that OS handoff until
                // Rust claims it or the next selection expires it.
                deferredReleases.add(locator)
            } else runCatching { releaseOwned(context, locator) }
        }
    }

    fun reconcile(context: Context, retainedLocators: List<String>) {
        synchronized(gate) {
            val retained = retainedLocators.toSet()
            selection = null
            deliveredLocator = null
            deferredReleases.removeAll(retained)
            drainReleases(context)
            val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            val owned = preferences.getStringSet(OWNED, emptySet()).orEmpty().toSet()
            owned.filterNot { it in retained }.forEach { locator -> runCatching { releaseOwned(context, locator) } }
        }
    }

    private fun drainReleases(context: Context) {
        if (selection != null) return
        deferredReleases.filterNot { it == deliveredLocator }.toList().forEach { locator ->
            runCatching { releaseOwned(context, locator) }
            deferredReleases.remove(locator)
        }
    }

    private fun releaseOwned(context: Context, locator: String) {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val owned = preferences.getStringSet(OWNED, emptySet()).orEmpty().toMutableSet()
        if (locator !in owned) return
        val uri = Uri.parse(locator)
        val resolver = context.contentResolver
        val permission = resolver.persistedUriPermissions.firstOrNull { it.uri == uri && it.isReadPermission }
        val key = receiptKey(locator)
        val ownsCurrentGrant = permission?.persistedTime == preferences.getLong(key, Long.MIN_VALUE)
        if (permission != null && ownsCurrentGrant) {
            resolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        // Keep the receipt if the OS did not relinquish it, so reconciliation
        // can retry without ever expanding cleanup to unknown permissions.
        if (!ownsCurrentGrant || resolver.persistedUriPermissions.none { it.uri == uri && it.isReadPermission }) {
            owned.remove(locator)
            preferences.edit().putStringSet(OWNED, owned).remove(key).commit()
        }
    }

    private fun receiptKey(locator: String): String = "grant-time-" +
        java.security.MessageDigest.getInstance("SHA-256").digest(locator.toByteArray(Charsets.UTF_8))
            .joinToString("") { "%02x".format(it) }
}
