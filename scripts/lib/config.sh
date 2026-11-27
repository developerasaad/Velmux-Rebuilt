#!/usr/bin/env bash
# Shared native dependency policy. Application configuration lives in its
# corresponding Rust, Gradle and Xcode build definitions.
VELMUX_FFMPEG_BUILDER_DIR="scripts/ffmpeg-builder"
VELMUX_FFMPEG_OUT_DIR="${VELMUX_FFMPEG_OUT_DIR:-scripts/ffmpeg-builder/out}"
VELMUX_ANDROID_MIN_API="29"
export VELMUX_ANDROID_API="${VELMUX_ANDROID_API:-$VELMUX_ANDROID_MIN_API}"
VELMUX_ANDROID_DEFAULT_ABI="arm64-v8a"
VELMUX_ANDROID_ALL_ABIS="arm64-v8a x86_64"
VELMUX_ANDROID_NDK_VERSION="27.2.12479018"
export VELMUX_IOS_DEPLOYMENT_TARGET="${VELMUX_IOS_DEPLOYMENT_TARGET:-15.0}"
