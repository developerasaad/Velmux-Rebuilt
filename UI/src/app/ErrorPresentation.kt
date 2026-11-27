package com.velmux.ui.app

import uniffi.velmux_core.EngineException

/** Present the authoritative error detail, without generated binding field labels. */
internal fun Throwable.userMessage(fallback: String): String {
    val detail = when (this) {
        is EngineException.Configuration -> this.detail
        is EngineException.Decoding -> this.detail
        is EngineException.Encoding -> this.detail
        is EngineException.Filtering -> this.detail
        is EngineException.Internal -> this.detail
        is EngineException.InvalidMedia -> this.detail
        is EngineException.Muxing -> this.detail
        is EngineException.Platform -> this.detail
        is EngineException.Recovery -> this.detail
        is EngineException.SourceAccess -> this.detail
        is EngineException.Storage -> this.detail
        is EngineException.UnsupportedMedia -> this.detail
        is EngineException.InvalidStorageRoot -> "Velmux could not initialize application storage. Restart the app and try again."
        is EngineException.Cancelled -> "The request was canceled."
        else -> message
    }
    return detail?.take(1000)?.takeIf { it.isNotBlank() } ?: fallback
}
