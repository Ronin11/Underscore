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

use underscore_decode::Chapter;
use underscore_library::{Cache, Credentials, Download, ItemId, Library, Opened, SubsonicLogin};

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

/// Load a music map that plays from the music server. Returns null when
/// the map is loaded, otherwise a message saying what was wrong. Slow:
/// reads the server's song list before taking the lock. Call it from a
/// background thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeLoadMapFromServer(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    toml: JString,
    cache_dir: JString,
) -> jstring {
    let (Some(toml), Some(cache_dir)) = (string(&mut env, &toml), string(&mut env, &cache_dir))
    else {
        return jstr(&env, "map and cache folder are both needed");
    };
    let Some(server) = with(handle, None, |b| b.music_server()) else {
        return jstr(&env, "not signed in to a music server");
    };
    match Bridge::prepare_map_from_server(&toml, server, &PathBuf::from(cache_dir)) {
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

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetSkipMs(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    back_ms: jlong,
    forward_ms: jlong,
) {
    with(handle, (), |b| b.set_skips(ms(back_ms), ms(forward_ms)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeSetRewind(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    enabled: jboolean,
) {
    with(handle, (), |b| b.set_rewind(enabled != JNI_FALSE));
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

// Audiobookshelf. Every call that talks to the server does so with the
// lock released, and the Kotlin side calls them from background threads.

/// Sign in. Null on success, otherwise what went wrong.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerLogin(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    url: JString,
    username: JString,
    secret: JString,
) -> jstring {
    let args = (
        string(&mut env, &url),
        string(&mut env, &username),
        string(&mut env, &secret),
    );
    let (Some(url), Some(username), Some(secret)) = args else {
        return jstr(
            &env,
            "server address, username, and password are all needed",
        );
    };
    match Bridge::sign_in(&url, &username, &secret) {
        Ok(server) => {
            with(handle, (), |b| b.set_server(Some(server)));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

/// Come back with saved credentials. Null on success, otherwise why not.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerResume(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    url: JString,
    access: JString,
    refresh: JString,
) -> jstring {
    let (Some(url), Some(access)) = (string(&mut env, &url), string(&mut env, &access)) else {
        return jstr(&env, "server address and token are needed");
    };
    let creds = Credentials {
        access,
        refresh: string(&mut env, &refresh).filter(|r| !r.is_empty()),
    };
    match Bridge::resume_server(&url, creds) {
        Ok(server) => {
            with(handle, (), |b| b.set_server(Some(server)));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

/// The credentials to save: the access token, a newline, and the refresh
/// token (possibly empty). Null when no server is in use.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerCredentials(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    let Some(server) = with(handle, None, |b| b.server()) else {
        return std::ptr::null_mut();
    };
    let c = server.credentials();
    jstr(
        &env,
        &format!("{}\n{}", c.access, c.refresh.unwrap_or_default()),
    )
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerForget(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    with(handle, (), |b| b.set_server(None));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeHasServer(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, false, |b| b.server().is_some()))
}

/// Begin a single sign-on. Answers `{"url": "..."}`, the address to open
/// in the browser, or `{"error": "..."}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerOpenIdStart(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    url: JString,
) -> jstring {
    let answer = |env: &JNIEnv, v: serde_json::Value| jstr(env, &v.to_string());
    let Some(url) = string(&mut env, &url) else {
        return answer(
            &env,
            serde_json::json!({ "error": "server address is needed" }),
        );
    };
    match Bridge::openid_start(&url) {
        Ok(start) => {
            let authorize = start.authorize_url.clone();
            with(handle, (), |b| b.set_openid(Some(start)));
            answer(&env, serde_json::json!({ "url": authorize }))
        }
        Err(e) => answer(&env, serde_json::json!({ "error": e })),
    }
}

/// Finish a single sign-on with the `code` and `state` from the address
/// the browser came back to. Null on success, otherwise what went wrong.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerOpenIdFinish(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    code: JString,
    state: JString,
) -> jstring {
    let (Some(code), Some(state)) = (string(&mut env, &code), string(&mut env, &state)) else {
        return jstr(&env, "the sign-in came back without a code");
    };
    let Some(start) = with(handle, None, Bridge::take_openid) else {
        return jstr(&env, "no sign-in was waiting; start it again");
    };
    match Bridge::openid_finish(&start, &code, &state) {
        Ok(server) => {
            with(handle, (), |b| b.set_server(Some(server)));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

// The music server. Like Audiobookshelf, every call that talks to it does
// so with the lock released, from a Kotlin background thread.

/// Sign in to the music server. Null on success, otherwise what went wrong.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMusicLogin(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    url: JString,
    username: JString,
    secret: JString,
) -> jstring {
    let args = (
        string(&mut env, &url),
        string(&mut env, &username),
        string(&mut env, &secret),
    );
    let (Some(url), Some(username), Some(secret)) = args else {
        return jstr(
            &env,
            "server address, username, and password are all needed",
        );
    };
    match Bridge::sign_in_music(&url, &username, &secret) {
        Ok(server) => {
            with(handle, (), |b| b.set_music_server(Some(server)));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

/// Come back to the music server with a saved login. Null on success,
/// otherwise why not.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMusicResume(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    url: JString,
    username: JString,
    salt: JString,
    token: JString,
) -> jstring {
    let args = (
        string(&mut env, &url),
        string(&mut env, &username),
        string(&mut env, &salt),
        string(&mut env, &token),
    );
    let (Some(url), Some(user), Some(salt), Some(token)) = args else {
        return jstr(&env, "server address and saved login are needed");
    };
    match Bridge::resume_music(&url, SubsonicLogin { user, salt, token }) {
        Ok(server) => {
            with(handle, (), |b| b.set_music_server(Some(server)));
            std::ptr::null_mut()
        }
        Err(e) => jstr(&env, &e),
    }
}

/// The login to save: user, salt and token, one per line. Null when no
/// music server is in use.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMusicCredentials(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    let Some(server) = with(handle, None, |b| b.music_server()) else {
        return std::ptr::null_mut();
    };
    let c = server.credentials();
    jstr(&env, &format!("{}\n{}\n{}", c.user, c.salt, c.token))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeMusicForget(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    with(handle, (), |b| b.set_music_server(None));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeHasMusicServer(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    jbool(with(handle, false, |b| b.music_server().is_some()))
}

/// The server's books as JSON: `{"books":[{"id","title","author",
/// "durationMs"}]}`, or `{"error": "..."}`. Fetches the list every time.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerBooks(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    let answer = |v: serde_json::Value| jstr(&env, &v.to_string());
    let Some((server, open)) = with(handle, None, |b| b.server().map(|s| (s, b.server_book())))
    else {
        return answer(serde_json::json!({ "error": "not signed in" }));
    };
    match server.shelf() {
        Ok(books) => {
            let list: Vec<serde_json::Value> = books
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "id": b.item.id.0,
                        "title": b.item.title,
                        "author": b.item.artist.clone().unwrap_or_default(),
                        "series": b.item.album.clone().unwrap_or_default(),
                        "durationMs": b.item.duration.map_or(-1, to_ms),
                        "addedMs": b.added_ms.unwrap_or(0),
                        "files": b.files.unwrap_or(0),
                        "cover": b.has_cover,
                        "progress": b.progress.unwrap_or(-1.0),
                        "finished": b.finished,
                        "playedMs": b.played_ms.unwrap_or(0),
                        "open": open.as_ref() == Some(&b.item.id),
                    })
                })
                .collect();
            answer(serde_json::json!({ "books": list, "base": server.base() }))
        }
        Err(e) => answer(serde_json::json!({ "error": e.to_string() })),
    }
}

/// Fetch a server book into the cache under `cache_dir`, load it as the
/// book, and move to where the server says the listener was. Null on
/// success, otherwise what went wrong. Slow: the whole file comes down.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerOpen(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    id: JString,
    cache_dir: JString,
) -> jstring {
    let (Some(id), Some(cache_dir)) = (string(&mut env, &id), string(&mut env, &cache_dir)) else {
        return jstr(&env, "book id and cache folder are needed");
    };
    let Some(server) = with(handle, None, |b| b.server()) else {
        return jstr(&env, "not signed in");
    };
    let id = ItemId(id);
    let cache = match Cache::open(PathBuf::from(cache_dir)) {
        Ok(c) => c,
        Err(e) => return jstr(&env, &format!("cache: {e}")),
    };
    let item = match server.item(&id) {
        Ok(i) => i,
        Err(e) => return jstr(&env, &e.to_string()),
    };
    let opened = match server.open_streaming(&id, &cache) {
        Ok(o) => o,
        Err(e) => return jstr(&env, &e.to_string()),
    };
    let chapters: Vec<Chapter> = item
        .chapters
        .iter()
        .map(|c| Chapter {
            title: c.title.clone(),
            start: c.start,
        })
        .collect();
    // Where the listener was, by the server's account. Not worth failing
    // the load over.
    let resume = server.progress(&id).ok().flatten();
    let loaded = match opened {
        Opened::Cached(path) => {
            let Some(path) = path.to_str().map(ToOwned::to_owned) else {
                return jstr(&env, "cached file has a path that is not text");
            };
            with(handle, false, |b| {
                let ok = b.load_server_book(&path, id, chapters);
                if ok && let Some(at) = resume {
                    b.seek(at);
                }
                ok
            })
        }
        Opened::Streaming(download) => {
            // Opening the decoder and moving to the listener's place wait on
            // the network, so they happen here, outside the lock. By the time
            // the source is handed over, the bytes around `resume` are in.
            let (rate, channels) = with(handle, (0, 0), |b| (b.sample_rate(), b.channels()));
            let Some(prepared) = Bridge::prepare_stream(&download, rate, channels, resume) else {
                return jstr(
                    &env,
                    &download
                        .error()
                        .unwrap_or_else(|| format!("could not decode {}", item.title)),
                );
            };
            with(handle, false, |b| {
                let ok = b.load_server_stream(prepared, chapters, id);
                if ok && let Some(at) = resume {
                    b.seek(at);
                }
                ok
            })
        }
        Opened::Parts(parts) => {
            // As above, a part at a time: the first part plays while the
            // rest queue up behind it.
            let (rate, channels) = with(handle, (0, 0), |b| (b.sample_rate(), b.channels()));
            let Some(prepared) = Bridge::prepare_parts(parts, rate, channels, resume) else {
                return jstr(
                    &env,
                    &format!("could not decode the parts of {}", item.title),
                );
            };
            with(handle, false, |b| {
                let ok = b.load_server_stream(prepared, chapters, id);
                if ok && let Some(at) = resume {
                    b.seek(at);
                }
                ok
            })
        }
    };
    if loaded {
        std::ptr::null_mut()
    } else {
        jstr(&env, &format!("could not decode {}", item.title))
    }
}

/// The cached file behind the loaded server book, or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerBookFile(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    match with(handle, None, |b| b.server_book_file()) {
        Some(path) => jstr(&env, &path),
        None => std::ptr::null_mut(),
    }
}

/// How the loaded server book's download is going: "received total" in
/// bytes, or null when nothing is downloading. The two match once it is
/// done; [`nativeServerBookFile`] then has the file.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerDownload(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jstring {
    let downloads = with(handle, Vec::new(), |b| b.downloads());
    if downloads.is_empty() {
        return std::ptr::null_mut();
    }
    if let Some(e) = downloads.iter().find_map(Download::error) {
        return jstr(&env, &format!("error {e}"));
    }
    let got: u64 = downloads.iter().map(Download::received).sum();
    let total: u64 = downloads.iter().map(Download::len).sum();
    jstr(&env, &format!("{got} {total}"))
}

/// Load a server book straight from the cache, without asking the
/// server: for coming back to the last book when the server is out of
/// reach. Chapters come from the file only. Progress reports resume once
/// the server is signed in again.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerReopen(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    id: JString,
    path: JString,
) -> jboolean {
    let (Some(id), Some(path)) = (string(&mut env, &id), string(&mut env, &path)) else {
        return JNI_FALSE;
    };
    jbool(with(handle, false, |b| {
        b.load_server_book(&path, ItemId(id), Vec::new())
    }))
}

/// Tell the server where the listener is in the loaded server book.
/// Does nothing, and returns false, when the book is not from the server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeServerSync(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    let Some((server, book, at)) = with(handle, None, |b| {
        Some((b.server()?, b.server_book()?, b.position()))
    }) else {
        return JNI_FALSE;
    };
    jbool(server.report_progress(&book, at).is_ok())
}

/// Transcribe the book on the phone while it plays. `path` is the book's
/// file, or empty when `fd` is an open descriptor for it, which this
/// takes over. `dir` holds the models and the transcripts; `key` names
/// this book's transcript.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)] // the JNI env, the object and the handle come first
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeTranscribeStart(
    mut env: JNIEnv,
    _this: JObject,
    _handle: jlong,
    dir: JString,
    model: JString,
    key: JString,
    path: JString,
    fd: jint,
    length_ms: jlong,
) {
    let (Some(dir), Some(model), Some(key)) = (
        string(&mut env, &dir),
        string(&mut env, &model),
        string(&mut env, &key),
    ) else {
        return;
    };
    let path = string(&mut env, &path).unwrap_or_default();
    let audio = if path.is_empty() {
        crate::transcript::Audio::Fd(fd)
    } else {
        crate::transcript::Audio::Path(PathBuf::from(path))
    };
    crate::transcript::start(crate::transcript::Job {
        key,
        audio,
        length: Duration::from_millis(u64::try_from(length_ms).unwrap_or(0)),
        dir: PathBuf::from(dir),
        model,
    });
}

/// Where the listener is, so the transcriber keeps its lead.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeTranscribePosition(
    _env: JNIEnv,
    _this: JObject,
    _handle: jlong,
    at_ms: jlong,
) {
    crate::transcript::set_position(Duration::from_millis(u64::try_from(at_ms).unwrap_or(0)));
}

/// What the transcriber is doing, as JSON.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeTranscribeStatus(
    env: JNIEnv,
    _this: JObject,
    _handle: jlong,
) -> jstring {
    jstr(&env, &crate::transcript::status().to_string())
}

/// Stop transcribing. What is done stays.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_ronin11_underscore_Engine_nativeTranscribeStop(
    _env: JNIEnv,
    _this: JObject,
    _handle: jlong,
) {
    crate::transcript::stop();
}
