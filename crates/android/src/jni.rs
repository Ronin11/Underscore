//! JNI entry points for `io.github.ronin11.underscore.Engine`.
//!
//! Every function takes the engine handle the Kotlin side got from
//! `nativeCreate` and holds the bridge mutex for the duration of the call.
//! Times arrive as milliseconds on Android's monotonic `elapsedRealtime`
//! clock; the engine only needs them to be monotonic.
//!
//! `nativeLoadMap` is the one call that does real work: it scans a music
//! folder. It does that before taking the lock, and the Kotlin side calls
//! it from a background thread.

#![allow(
    clippy::missing_safety_doc,
    non_snake_case,
    clippy::not_unsafe_ptr_arg_deref
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jni::JNIEnv;
use jni::objects::{JFloatArray, JObject, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jfloat, jint, jlong, jstring};

use crate::{Bridge, hint_for_mime};

/// The Kotlin handle points at the mutex inside an `Arc`; the map worker
/// holds the `Arc` weakly.
type Handle = Mutex<Bridge>;

fn ms(v: jlong) -> Duration {
    Duration::from_millis(v.max(0).unsigned_abs())
}

#[allow(clippy::cast_possible_truncation)]
fn to_ms(d: Duration) -> jlong {
    d.as_millis().min(jlong::MAX as u128) as jlong
}

fn with<R>(handle: jlong, default: R, f: impl FnOnce(&mut Bridge) -> R) -> R {
    if handle == 0 {
        return default;
    }
    // SAFETY: `handle` came from `nativeCreate`, which leaked an `Arc`
    // pointing at this mutex, and has not been passed to `nativeDestroy`;
    // the Kotlin wrapper guarantees both.
    let m = unsafe { &*(handle as *const Handle) };
    match m.lock() {
        Ok(mut b) => f(&mut b),
        Err(_) => default,
    }
}

fn jbool(v: bool) -> jboolean {
    if v { JNI_TRUE } else { JNI_FALSE }
}

fn string(env: &mut JNIEnv, s: &JString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(Into::into)
}

fn hint(env: &mut JNIEnv, mime: &JString) -> Option<&'static str> {
    hint_for_mime(&string(env, mime)?)
}

/// A Java string, or null if it could not be made.
fn jstr(env: &JNIEnv, s: &str) -> jstring {
    env.new_string(s)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeCreate(
    _env: JNIEnv,
    _this: JObject,
    sample_rate: jint,
    channels: jint,
) -> jlong {
    let rate = u32::try_from(sample_rate).unwrap_or(48_000).max(8_000);
    let ch = usize::try_from(channels).unwrap_or(2).clamp(1, 8);
    Arc::into_raw(Bridge::shared(rate, ch)) as jlong
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeDestroy(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: created by `nativeCreate`, destroyed exactly once here.
        // The map worker may hold the bridge a moment longer while it
        // finishes a job; it exits once the bridge is gone.
        drop(unsafe { Arc::from_raw(handle as *const Handle) });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeLoadSpeech(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    fd: jint,
    mime: JString,
) -> jboolean {
    let h = hint(&mut env, &mime);
    jbool(with(handle, false, |b| b.load_speech_fd(fd, h)))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeLoadMusic(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    fd: jint,
    mime: JString,
) -> jboolean {
    let h = hint(&mut env, &mime);
    jbool(with(handle, false, |b| b.load_music_fd(fd, h)))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativePlay(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.play(ms(now_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativePause(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.pause(ms(now_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeToggle(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.toggle(ms(now_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeIsPlaying(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, false, |b| b.is_playing()))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetSpeed(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    speed: jfloat,
) -> jfloat {
    with(handle, 1.0, |b| b.set_speed(speed))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSpeed(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jfloat {
    with(handle, 1.0, |b| b.speed())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetMusicEnabled(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    enabled: jboolean,
) {
    with(handle, (), |b| b.set_music_enabled(enabled == JNI_TRUE));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMusicEnabled(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, true, |b| b.music_enabled()))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetMusicLevel(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    level: jfloat,
) {
    with(handle, (), |b| b.set_music_level(level));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSkipBack(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    with(handle, (), Bridge::skip_back);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSkipForward(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    with(handle, (), Bridge::skip_forward);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSeekMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    to_ms: jlong,
) {
    with(handle, (), |b| b.seek(ms(to_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetSleepMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    after_ms: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.set_sleep(ms(after_ms), ms(now_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeExtendSleepMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    by_ms: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.extend_sleep(ms(by_ms), ms(now_ms)));
}

/// Milliseconds left on the sleep timer, or -1 when none is set.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSleepRemainingMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    now_ms: jlong,
) -> jlong {
    with(handle, -1, |b| {
        b.sleep_remaining(ms(now_ms)).map_or(-1, to_ms)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetNoiseDb(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    db: jfloat,
) {
    with(handle, (), |b| b.set_noise_db(db));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativePositionMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jlong {
    with(handle, 0, |b| to_ms(b.position()))
}

/// Book length in milliseconds, or -1 when unknown.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeDurationMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jlong {
    with(handle, -1, |b| b.duration().map_or(-1, to_ms))
}

/// How many chapter marks the book has.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeChapterCount(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jint {
    with(handle, 0, |b| {
        jint::try_from(b.chapters().len()).unwrap_or(jint::MAX)
    })
}

/// A chapter's title, or null when it has none or `index` is out of range.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeChapterTitle(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    index: jint,
) -> jstring {
    let title = with(handle, None, |b| {
        usize::try_from(index)
            .ok()
            .and_then(|i| b.chapters().get(i))
            .and_then(|c| c.title.clone())
    });
    match title {
        Some(t) => jstr(&env, &t),
        None => std::ptr::null_mut(),
    }
}

/// Load a music map and the folder it plays from. Returns null when the
/// map is loaded, otherwise a message saying what was wrong. Slow: scans
/// the folder before taking the lock. Call it from a background thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeLoadMap(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    toml: JString,
    music_dir: JString,
    cache_dir: JString,
) -> jstring {
    let args = (
        string(&mut env, &toml),
        string(&mut env, &music_dir),
        string(&mut env, &cache_dir),
    );
    let (Some(toml), Some(music_dir), Some(cache_dir)) = args else {
        return jstr(&env, "map, music folder, and cache folder are all needed");
    };
    if handle == 0 {
        return jstr(&env, "engine closed");
    }
    match Bridge::prepare_map(&toml, &PathBuf::from(music_dir), &PathBuf::from(cache_dir)) {
        Ok(prepared) => {
            with(handle, false, |b| b.install_map(prepared));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeClearMap(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    with(handle, (), Bridge::clear_map);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeHasMap(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, false, |b| b.has_map()))
}

/// One line on the map for the UI. Never null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMapStatus(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    let status = with(handle, "No map".to_string(), |b| b.map_status());
    jstr(&env, &status)
}

/// Where a chapter starts, in milliseconds, or -1 when out of range.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeChapterStartMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    index: jint,
) -> jlong {
    with(handle, -1, |b| {
        usize::try_from(index)
            .ok()
            .and_then(|i| b.chapters().get(i))
            .map_or(-1, |c| to_ms(c.start))
    })
}

/// The chapter the book is in, or -1 when it has no chapters.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeCurrentChapter(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jint {
    with(handle, -1, |b| {
        b.current_chapter()
            .and_then(|i| jint::try_from(i).ok())
            .unwrap_or(-1)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSeekChapter(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    index: jint,
) -> jboolean {
    jbool(with(handle, false, |b| {
        usize::try_from(index).is_ok_and(|i| b.seek_chapter(i))
    }))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetSleepEndOfChapter(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    now_ms: jlong,
) {
    with(handle, (), |b| b.set_sleep_end_of_chapter(ms(now_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSleepAtChapterEnd(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, false, |b| b.sleep_at_chapter_end()))
}

/// Fill `out` with interleaved samples. Returns frames written.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeRender(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    out: JFloatArray,
    now_ms: jlong,
) -> jint {
    let Ok(len) = env.get_array_length(&out) else {
        return 0;
    };
    let Ok(len) = usize::try_from(len) else {
        return 0;
    };
    let mut buf = vec![0.0f32; len];
    let frames = with(handle, 0, |b| b.render(&mut buf, ms(now_ms)));
    if env.set_float_array_region(&out, 0, &buf).is_err() {
        return 0;
    }
    jint::try_from(frames).unwrap_or(jint::MAX)
}
