//! Audiobookshelf as a [`Library`].
//!
//! Books come from the listener's own server. A book is fetched once, as
//! the file the server holds, into the per-stream [`Cache`], and played
//! from there; progress goes back to the server so other clients pick up
//! where this one left off. Nothing is decrypted and nothing mixed is
//! sent anywhere: the only bytes that move are the server's own files,
//! downwards.
//!
//! Only books with a single audio file play for now. Books split across
//! many files are listed, and `fetch` says why they cannot be played yet.
//! Podcasts are not listed yet.
//!
//! Signing in works two ways. A local account takes a username and a
//! password. An account that lives in the server's single sign-on
//! (`OpenID`, such as Authentik) goes through the browser: [`OpenIdStart`]
//! holds the half-done sign-in while the browser is out, and
//! [`Audiobookshelf::openid_finish`] trades what the browser brings back
//! for tokens. That is the server's own mobile flow, with PKCE, so the
//! password never passes through this app at all.
//!
//! Every call here does network I/O. Run it off the audio thread.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::http::Response;

use crate::stream::Download;
use crate::{Cache, Chapter, Error, Item, ItemId, Library, Query, Role};

/// What to keep to sign in again without a password. The access token
/// expires after an hour or so and is renewed with the refresh token; an
/// API key from the server's settings is a long-lived access token with
/// no refresh token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// Sent as a bearer token on every request.
    pub access: String,
    /// Used to get a new access token when the old one is refused.
    #[serde(default)]
    pub refresh: Option<String>,
}

/// A single sign-on in progress: the browser has been sent to
/// [`OpenIdStart::authorize_url`] and will come back to
/// [`OpenIdStart::redirect_uri`] with a `code` and a `state`.
#[derive(Debug, Clone)]
pub struct OpenIdStart {
    /// Open this in the browser.
    pub authorize_url: String,
    /// The address the browser returns to. The app must answer it.
    pub redirect_uri: String,
    base: String,
    state: String,
    verifier: String,
    /// The server's session for this sign-in. It ties the browser's
    /// trip to the exchange that finishes it.
    cookie: String,
}

/// One Audiobookshelf server, signed in.
pub struct Audiobookshelf {
    name: String,
    base: String,
    agent: Agent,
    auth: Mutex<Credentials>,
    /// The last listing, so `item` and `search` need no round trip.
    books: RwLock<Vec<Item>>,
    /// What the last listing said beyond each [`Item`], for [`Self::shelf`].
    listed: RwLock<HashMap<ItemId, Listed>>,
    /// The audio file behind each book, learned when the book is opened.
    files: RwLock<HashMap<ItemId, AudioFile>>,
}

impl std::fmt::Debug for Audiobookshelf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Audiobookshelf")
            .field("name", &self.name)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// One book as a book list shows it: the [`Item`], plus what the
/// listing and the listener's own progress say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfBook {
    /// The book. `album` holds the series, with its number when known.
    pub item: Item,
    /// When the book reached the server, in milliseconds since 1970.
    pub added_ms: Option<u64>,
    /// How many audio files hold the book, when the server says. Only
    /// single-file books play for now.
    pub files: Option<usize>,
    /// Whether the server has a cover for it.
    pub has_cover: bool,
    /// How far the listener is, from 0 to 1. `None` when never started.
    pub progress: Option<f64>,
    /// The listener marked it finished, or reached the end.
    pub finished: bool,
    /// When the listener last played it, in milliseconds since 1970.
    pub played_ms: Option<u64>,
}

/// A book opened for playing: already whole in the cache, or coming down.
#[derive(Debug)]
pub enum Opened {
    /// Every byte is in the cache.
    Cached(PathBuf),
    /// Downloading; playable from [`Download::reader`] now.
    Streaming(Download),
}

/// What a listing says about a book beyond its [`Item`].
#[derive(Debug, Clone, Copy, Default)]
struct Listed {
    added_ms: Option<u64>,
    files: Option<usize>,
    has_cover: bool,
}

/// Where a book's audio is on the server.
#[derive(Debug, Clone)]
struct AudioFile {
    ino: String,
    ext: String,
}

impl Audiobookshelf {
    /// Sign in with a username and password. `base` is the server's
    /// address, such as `https://books.example.net`.
    ///
    /// # Errors
    /// The server could not be reached or refused the login.
    pub fn login(
        name: impl Into<String>,
        base: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, Error> {
        let this = Self::with_credentials(
            name,
            base,
            Credentials {
                access: String::new(),
                refresh: None,
            },
        );
        let mut resp = this
            .agent
            .post(this.url("/login"))
            .header("x-return-tokens", "true")
            .send_json(serde_json::json!({ "username": username, "password": password }))?;
        if resp.status().as_u16() == 401 {
            return Err(Error::Backend(
                "Audiobookshelf refused the username or password".into(),
            ));
        }
        let login: LoginResponse = read_json(&mut resp, "POST /login")?;
        *this.lock_auth() = login.user.credentials()?;
        Ok(this)
    }

    /// Begin a single sign-on. Tries each of `redirect_uris` in turn and
    /// keeps the first the server allows; servers allow
    /// `audiobookshelf://oauth` unless told otherwise.
    ///
    /// # Errors
    /// The server could not be reached, has no single sign-on, or allows
    /// none of the addresses.
    pub fn openid_start(base: &str, redirect_uris: &[&str]) -> Result<OpenIdStart, Error> {
        let base = base.trim_end_matches('/').to_string();
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent("underscore")
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();
        let mut refused = Vec::new();
        for redirect in redirect_uris {
            let verifier = random_token(32)?;
            let state = random_token(16)?;
            let mut resp = agent
                .get(format!("{base}/auth/openid"))
                .query("code_challenge", challenge(&verifier))
                .query("code_challenge_method", "S256")
                .query("redirect_uri", redirect)
                .query("client_id", "Audiobookshelf-App")
                .query("response_type", "code")
                .query("state", &state)
                .call()?;
            let status = resp.status().as_u16();
            if status == 400 {
                let why = resp.body_mut().read_to_string().unwrap_or_default();
                if why.contains("redirect_uri") {
                    refused.push(*redirect);
                    continue;
                }
                return Err(Error::Backend(format!(
                    "Audiobookshelf single sign-on: {why}"
                )));
            }
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .filter(|_| (300..400).contains(&status))
                .ok_or_else(|| {
                    Error::Backend(format!(
                        "Audiobookshelf single sign-on: HTTP {status}; is OpenID turned on?"
                    ))
                })?
                .to_string();
            let cookie = resp
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .filter_map(|v| v.split(';').next())
                .collect::<Vec<_>>()
                .join("; ");
            return Ok(OpenIdStart {
                authorize_url: location,
                redirect_uri: (*redirect).to_string(),
                base,
                state,
                verifier,
                cookie,
            });
        }
        Err(Error::Backend(format!(
            "Audiobookshelf allows none of {}; add one under Settings, Authentication, \
             allowed mobile redirect URIs",
            refused.join(", ")
        )))
    }

    /// Finish a single sign-on with the `code` and `state` the browser
    /// came back with.
    ///
    /// # Errors
    /// The answer is not for this sign-in, or the server refused it.
    pub fn openid_finish(
        name: impl Into<String>,
        start: &OpenIdStart,
        code: &str,
        state: &str,
    ) -> Result<Self, Error> {
        if state != start.state {
            return Err(Error::Backend(
                "the sign-in that came back is not the one this app started".into(),
            ));
        }
        let this = Self::with_credentials(
            name,
            &start.base,
            Credentials {
                access: String::new(),
                refresh: None,
            },
        );
        let mut resp = this
            .agent
            .get(this.url("/auth/openid/callback"))
            .query("state", state)
            .query("code", code)
            .query("code_verifier", &start.verifier)
            .header("cookie", &start.cookie)
            .header("x-return-tokens", "true")
            .call()?;
        let status = resp.status().as_u16();
        if status != 200 {
            let why = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(Error::Backend(format!(
                "Audiobookshelf refused the sign-in: HTTP {status} {}",
                why.chars().take(200).collect::<String>()
            )));
        }
        let login: LoginResponse = read_json(&mut resp, "GET /auth/openid/callback")?;
        *this.lock_auth() = login.user.credentials()?;
        Ok(this)
    }

    /// Use saved credentials or an API key. Nothing is sent until the
    /// first request.
    pub fn with_credentials(name: impl Into<String>, base: &str, creds: Credentials) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .user_agent("underscore")
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();
        Self {
            name: name.into(),
            base: base.trim_end_matches('/').to_string(),
            agent,
            auth: Mutex::new(creds),
            books: RwLock::new(Vec::new()),
            listed: RwLock::new(HashMap::new()),
            files: RwLock::new(HashMap::new()),
        }
    }

    /// The server's address.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The credentials to save for next time. They change when the
    /// access token is renewed, so read them after use, not before.
    #[must_use]
    pub fn credentials(&self) -> Credentials {
        self.lock_auth().clone()
    }

    /// The signed-in user's name. The cheapest way to check that saved
    /// credentials still work.
    ///
    /// # Errors
    /// The server could not be reached or the credentials are refused.
    pub fn whoami(&self) -> Result<String, Error> {
        let mut resp = self.get("/api/me")?;
        let me: Me = read_json(&mut resp, "GET /api/me")?;
        Ok(me.username)
    }

    /// Fetch the list of books from every book library the user can see.
    /// Returns how many there are.
    ///
    /// # Errors
    /// The server could not be reached or the credentials are refused.
    pub fn refresh_items(&self) -> Result<usize, Error> {
        let mut resp = self.get("/api/libraries")?;
        let libs: Libraries = read_json(&mut resp, "GET /api/libraries")?;
        let mut books = Vec::new();
        let mut listed = HashMap::new();
        for lib in libs
            .libraries
            .into_iter()
            .filter(|l| l.media_type == "book")
        {
            let path = format!("/api/libraries/{}/items?limit=0&page=0", lib.id);
            let mut resp = self.get(&path)?;
            let page: ItemsPage = read_json(&mut resp, "GET /api/libraries/{id}/items")?;
            for li in page.results.into_iter().filter(|li| !li.is_missing) {
                let extra = Listed {
                    added_ms: li.added_at,
                    files: li.media.num_audio_files,
                    has_cover: li
                        .media
                        .cover_path
                        .as_deref()
                        .is_some_and(|c| !c.is_empty()),
                };
                if let Some(item) = li.into_item() {
                    listed.insert(item.id.clone(), extra);
                    books.push(item);
                }
            }
        }
        books.sort_by(|a, b| a.title.cmp(&b.title));
        let n = books.len();
        *self
            .listed
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = listed;
        *self
            .books
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = books;
        Ok(n)
    }

    /// Every book, fetched fresh, with where the listener is in each.
    /// Two round trips plus one per book library: the listing and the
    /// listener's progress.
    ///
    /// # Errors
    /// The server could not be reached or the credentials are refused.
    pub fn shelf(&self) -> Result<Vec<ShelfBook>, Error> {
        self.refresh_items()?;
        let mut resp = self.get("/api/me")?;
        let me: Me = read_json(&mut resp, "GET /api/me")?;
        let progress: HashMap<String, ServerProgress> = me
            .media_progress
            .into_iter()
            .filter(|p| p.episode_id.is_none())
            .filter_map(|p| Some((p.library_item_id.clone()?, p)))
            .collect();
        let books = self
            .books
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let listed = self
            .listed
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(books
            .into_iter()
            .map(|item| {
                let extra = listed.get(&item.id).copied().unwrap_or_default();
                let p = progress.get(&item.id.0);
                ShelfBook {
                    added_ms: extra.added_ms,
                    files: extra.files,
                    has_cover: extra.has_cover,
                    progress: p
                        .filter(|p| p.progress > 0.0 || p.current_time > 0.0)
                        .map(|p| p.progress.clamp(0.0, 1.0)),
                    finished: p.is_some_and(|p| p.is_finished),
                    played_ms: p.and_then(|p| p.last_update),
                    item,
                }
            })
            .collect())
    }

    /// Open a book to play while it downloads. A book already in the cache
    /// comes back as its path. Otherwise the download starts, and the
    /// returned [`Download`] can be read at once; it fetches from wherever
    /// the reader is, and the file lands in the cache when it is whole.
    ///
    /// # Errors
    /// The server could not be reached, or the book is many files, which
    /// do not play yet.
    pub fn open_streaming(self: &Arc<Self>, id: &ItemId, cache: &Cache) -> Result<Opened, Error> {
        let file = self.audio_file(id)?;
        if let Some(path) = cache.get(&self.name, id, &file.ext) {
            return Ok(Opened::Cached(path));
        }
        let path = format!("/api/items/{id}/file/{}", file.ino);
        // The length, from a one-byte range.
        let resp = self.get_range(&path, Some((0, 0)))?;
        let status = resp.status().as_u16();
        let header = |k: &str| {
            resp.headers()
                .get(k)
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned)
        };
        let len = match status {
            206 => header("content-range")
                .and_then(|r| r.rsplit('/').next().and_then(|n| n.trim().parse().ok())),
            200 => header("content-length").and_then(|n| n.parse().ok()),
            _ => None,
        }
        .ok_or_else(|| Error::Backend(format!("GET {path}: HTTP {status}, no length")))?;
        drop(resp);
        let this = Arc::clone(self);
        let fetch = move |from: u64| -> io::Result<Box<dyn io::Read + Send>> {
            let resp = this
                .get_range(&path, Some((from, u64::MAX)))
                .map_err(io::Error::other)?;
            match resp.status().as_u16() {
                206 => {}
                // A server that ignores ranges sends it all; fine from the start.
                200 if from == 0 => {}
                s => {
                    return Err(io::Error::other(format!(
                        "GET {path} from {from}: HTTP {s}"
                    )));
                }
            }
            Ok(Box::new(
                resp.into_body().into_with_config().limit(u64::MAX).reader(),
            ))
        };
        let target = cache.path_for(&self.name, id, &file.ext);
        Download::start(target, len, Box::new(fetch))
            .map(Opened::Streaming)
            .map_err(Error::Io)
    }

    /// The one audio file behind a book, learned once and remembered.
    fn audio_file(&self, id: &ItemId) -> Result<AudioFile, Error> {
        let known = self
            .files
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned();
        if let Some(f) = known {
            return Ok(f);
        }
        let (item, files) = self.expanded(id)?;
        let n = files.len();
        let [file] = <[AudioFile; 1]>::try_from(files).map_err(|_| {
            Error::Unsupported(format!(
                "\"{}\" is {n} audio files; only single-file books play for now",
                item.title
            ))
        })?;
        self.files
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), file.clone());
        Ok(file)
    }

    /// A GET for bytes `from..=to` of `path`; `u64::MAX` for to the end.
    fn get_range(
        &self,
        path: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Response<ureq::Body>, Error> {
        let range = range.map(|(a, b)| {
            if b == u64::MAX {
                format!("bytes={a}-")
            } else {
                format!("bytes={a}-{b}")
            }
        });
        self.authed(|token| {
            let mut req = self
                .agent
                .get(self.url(path))
                .header("Authorization", format!("Bearer {token}"));
            if let Some(r) = &range {
                req = req.header("Range", r);
            }
            req.call()
        })
    }

    /// Ask the server for everything about one book, and remember which
    /// file holds its audio.
    fn expanded(&self, id: &ItemId) -> Result<(Item, Vec<AudioFile>), Error> {
        let path = format!("/api/items/{id}?expanded=1");
        let mut resp = self.get(&path)?;
        if resp.status().as_u16() == 404 {
            return Err(Error::NotFound(id.clone()));
        }
        let li: LibraryItem = read_json(&mut resp, "GET /api/items/{id}")?;
        let files: Vec<AudioFile> = li
            .media
            .audio_files
            .iter()
            .map(|f| AudioFile {
                ino: f.ino.clone(),
                ext: f.metadata.ext.trim_start_matches('.').to_ascii_lowercase(),
            })
            .collect();
        let item = li.into_item().ok_or_else(|| Error::NotFound(id.clone()))?;
        Ok((item, files))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn lock_auth(&self) -> std::sync::MutexGuard<'_, Credentials> {
        self.auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A GET with the bearer token. A refused token is renewed once and
    /// the request repeated.
    fn get(&self, path: &str) -> Result<Response<ureq::Body>, Error> {
        self.authed(|token| {
            self.agent
                .get(self.url(path))
                .header("Authorization", format!("Bearer {token}"))
                .call()
        })
    }

    fn patch_json(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<Response<ureq::Body>, Error> {
        self.authed(|token| {
            self.agent
                .patch(self.url(path))
                .header("Authorization", format!("Bearer {token}"))
                .send_json(body)
        })
    }

    fn authed(
        &self,
        send: impl Fn(&str) -> Result<Response<ureq::Body>, ureq::Error>,
    ) -> Result<Response<ureq::Body>, Error> {
        let token = self.lock_auth().access.clone();
        let resp = send(&token)?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        self.renew()?;
        let token = self.lock_auth().access.clone();
        let resp = send(&token)?;
        if resp.status().as_u16() == 401 {
            return Err(Error::Backend(
                "Audiobookshelf no longer accepts this login; sign in again".into(),
            ));
        }
        Ok(resp)
    }

    /// Trade the refresh token for a new access token.
    fn renew(&self) -> Result<(), Error> {
        let refresh = self.lock_auth().refresh.clone().ok_or_else(|| {
            Error::Backend("Audiobookshelf no longer accepts this login; sign in again".into())
        })?;
        let mut resp = self
            .agent
            .post(self.url("/auth/refresh"))
            .header("x-refresh-token", refresh)
            .send_empty()?;
        if resp.status().as_u16() == 401 {
            return Err(Error::Backend(
                "Audiobookshelf no longer accepts this login; sign in again".into(),
            ));
        }
        let login: LoginResponse = read_json(&mut resp, "POST /auth/refresh")?;
        *self.lock_auth() = login.user.credentials()?;
        Ok(())
    }
}

impl Library for Audiobookshelf {
    fn name(&self) -> &str {
        &self.name
    }

    fn items(&self, role: Role) -> Result<Vec<Item>, Error> {
        if role != Role::Speech {
            return Ok(Vec::new());
        }
        if self
            .books
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
        {
            self.refresh_items()?;
        }
        Ok(self
            .books
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    fn item(&self, id: &ItemId) -> Result<Item, Error> {
        let known = self
            .books
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|i| &i.id == id)
            .cloned();
        match known {
            // A listing carries no chapters; the full record does.
            Some(item) if !item.chapters.is_empty() => Ok(item),
            _ => self.expanded(id).map(|(item, _)| item),
        }
    }

    fn search(&self, query: &Query) -> Result<Vec<Item>, Error> {
        let items = self.items(Role::Speech)?;
        Ok(query.rank(items.iter()))
    }

    fn fetch(&self, id: &ItemId, cache: &Cache) -> Result<PathBuf, Error> {
        let file = self.audio_file(id)?;
        let path = format!("/api/items/{id}/file/{}", file.ino);
        let cached = cache.get_or_fill(&self.name, id, &file.ext, |out| {
            let mut resp = self.get(&path).map_err(io::Error::other)?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(io::Error::other(format!("GET {path}: HTTP {status}")));
            }
            let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
            io::copy(&mut body, out)?;
            Ok(())
        })?;
        Ok(cached)
    }

    fn progress(&self, id: &ItemId) -> Result<Option<Duration>, Error> {
        let path = format!("/api/me/progress/{id}");
        let mut resp = self.get(&path)?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let p: MediaProgress = read_json(&mut resp, "GET /api/me/progress/{id}")?;
        Ok(Some(Duration::from_secs_f64(p.current_time.max(0.0))))
    }

    fn report_progress(&self, id: &ItemId, at: Duration) -> Result<(), Error> {
        let duration = self
            .books
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|i| &i.id == id)
            .and_then(|i| i.duration);
        let mut body = serde_json::json!({ "currentTime": at.as_secs_f64() });
        if let Some(d) = duration.filter(|d| !d.is_zero()) {
            body["duration"] = d.as_secs_f64().into();
            body["progress"] = (at.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0).into();
        }
        let path = format!("/api/me/progress/{id}");
        let resp = self.patch_json(&path, &body)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(Error::Backend(format!("PATCH {path}: HTTP {status}")))
        }
    }
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Self::Backend(format!("Audiobookshelf: {e}"))
    }
}

/// `bytes` of randomness as unpadded base64url: a PKCE verifier or a state.
fn random_token(bytes: usize) -> Result<String, Error> {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| Error::Backend("no randomness for the sign-in".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// The PKCE challenge for a verifier: base64url of its SHA-256.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// Parse a JSON body, or say which call failed and how.
fn read_json<T: serde::de::DeserializeOwned>(
    resp: &mut Response<ureq::Body>,
    what: &str,
) -> Result<T, Error> {
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Backend(format!(
            "Audiobookshelf {what}: HTTP {status}"
        )));
    }
    resp.body_mut()
        .read_json()
        .map_err(|e| Error::Backend(format!("Audiobookshelf {what}: {e}")))
}

// The parts of the server's JSON this client reads. Everything else is
// ignored, and every field the server might leave out has a default.

#[derive(Deserialize)]
struct LoginResponse {
    user: User,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    /// The old never-expiring token, from servers before access tokens.
    #[serde(default)]
    token: Option<String>,
}

impl User {
    fn credentials(self) -> Result<Credentials, Error> {
        let access = self
            .access_token
            .or(self.token)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| Error::Backend("Audiobookshelf sent no token".into()))?;
        Ok(Credentials {
            access,
            refresh: self.refresh_token.filter(|t| !t.is_empty()),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Me {
    username: String,
    #[serde(default)]
    media_progress: Vec<ServerProgress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerProgress {
    #[serde(default)]
    library_item_id: Option<String>,
    #[serde(default)]
    episode_id: Option<String>,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    current_time: f64,
    #[serde(default)]
    is_finished: bool,
    #[serde(default)]
    last_update: Option<u64>,
}

#[derive(Deserialize)]
struct Libraries {
    libraries: Vec<LibrarySummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibrarySummary {
    id: String,
    #[serde(default)]
    media_type: String,
}

#[derive(Deserialize)]
struct ItemsPage {
    results: Vec<LibraryItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryItem {
    id: String,
    #[serde(default)]
    media_type: String,
    #[serde(default)]
    media: Media,
    #[serde(default)]
    added_at: Option<u64>,
    #[serde(default)]
    is_missing: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Media {
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    chapters: Vec<ServerChapter>,
    #[serde(default)]
    audio_files: Vec<ServerAudioFile>,
    #[serde(default)]
    num_audio_files: Option<usize>,
    #[serde(default)]
    cover_path: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author_name: Option<String>,
    #[serde(default)]
    authors: Vec<Named>,
    #[serde(default)]
    series_name: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
}

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ServerChapter {
    #[serde(default)]
    start: f64,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerAudioFile {
    #[serde(default)]
    ino: String,
    #[serde(default)]
    metadata: FileMetadata,
}

#[derive(Deserialize, Default)]
struct FileMetadata {
    #[serde(default)]
    ext: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaProgress {
    #[serde(default)]
    current_time: f64,
}

impl LibraryItem {
    /// As a library item, or `None` for anything that is not a book.
    fn into_item(self) -> Option<Item> {
        if self.media_type != "book" {
            return None;
        }
        let m = self.media;
        let meta = m.metadata;
        let authors: Vec<String> = meta.authors.into_iter().map(|a| a.name).collect();
        let artist = meta
            .author_name
            .filter(|a| !a.is_empty())
            .or_else(|| (!authors.is_empty()).then(|| authors.join(", ")));
        let mut tags = meta.genres;
        tags.extend(m.tags);
        let chapters = m
            .chapters
            .into_iter()
            .map(|c| Chapter {
                title: c.title.filter(|t| !t.is_empty()),
                start: Duration::from_secs_f64(c.start.max(0.0)),
            })
            .collect();
        let format = m
            .audio_files
            .first()
            .map(|f| f.metadata.ext.trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        Some(Item {
            id: ItemId(self.id),
            role: Role::Speech,
            title: meta.title.unwrap_or_else(|| "Untitled".into()),
            artist,
            album: meta.series_name.filter(|s| !s.is_empty()),
            duration: m.duration.filter(|d| *d > 0.0).map(Duration::from_secs_f64),
            tags,
            chapters,
            format,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One request as the fake server saw it.
    struct Req {
        method: String,
        path: String,
        auth: Option<String>,
        range: Option<String>,
        body: String,
    }

    /// A reply: status, content type, body.
    type Reply = (u16, &'static str, Vec<u8>);

    /// A fake Audiobookshelf: a closure answers each request. Returns the
    /// base URL and a counter of requests served.
    fn serve(
        handler: impl Fn(&Req) -> Reply + Send + Sync + 'static,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&count);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut auth = None;
                let mut range = None;
                let mut len = 0usize;
                let mut chunked = false;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    match k.to_ascii_lowercase().as_str() {
                        "authorization" => auth = Some(v.trim().to_string()),
                        "x-refresh-token" => auth = Some(format!("refresh {}", v.trim())),
                        "content-length" => len = v.trim().parse().unwrap(),
                        "range" => range = Some(v.trim().to_string()),
                        "transfer-encoding" => chunked = v.trim() == "chunked",
                        _ => {}
                    }
                }
                let mut body = vec![0u8; len];
                if chunked {
                    body.clear();
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let n = usize::from_str_radix(size.trim(), 16).unwrap();
                        let mut chunk = vec![0u8; n + 2];
                        reader.read_exact(&mut chunk).unwrap();
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                } else {
                    reader.read_exact(&mut body).unwrap();
                }
                let req = Req {
                    method,
                    path,
                    auth,
                    range,
                    body: String::from_utf8_lossy(&body).into_owned(),
                };
                let (status, ctype, out) = handler(&req);
                served.fetch_add(1, Ordering::SeqCst);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&out);
                let _ = stream.flush();
            }
        });
        (base, count)
    }

    #[allow(clippy::needless_pass_by_value)] // call sites pass literals
    fn json(status: u16, v: serde_json::Value) -> Reply {
        (status, "application/json", v.to_string().into_bytes())
    }

    fn book(id: &str, title: &str, files: usize) -> serde_json::Value {
        let audio: Vec<_> = (0..files)
            .map(|i| serde_json::json!({ "ino": format!("ino{i}"), "metadata": { "ext": ".m4b" } }))
            .collect();
        serde_json::json!({
            "id": id,
            "mediaType": "book",
            "media": {
                "metadata": { "title": title, "authorName": "Someone", "genres": ["Fantasy"] },
                "tags": ["long"],
                "duration": 3600.5,
                "chapters": [
                    { "id": 0, "start": 0.0, "end": 100.0, "title": "One" },
                    { "id": 1, "start": 100.0, "end": 3600.5, "title": "Two" }
                ],
                "audioFiles": audio
            }
        })
    }

    /// The whole happy path against a fake server: sign in, list, open a
    /// book into the cache, read and write progress.
    #[test]
    fn signs_in_lists_fetches_and_syncs_progress() {
        let audio = b"not really an m4b".to_vec();
        let audio_served = audio.clone();
        let (base, _) = serve(move |r| match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/login") => {
                let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                assert_eq!(v["username"], "nate");
                assert_eq!(v["password"], "pw");
                json(
                    200,
                    serde_json::json!({ "user": {
                    "accessToken": "acc1", "refreshToken": "ref1", "token": "legacy" } }),
                )
            }
            (_, _) if r.auth.as_deref() != Some("Bearer acc1") => json(401, serde_json::json!({})),
            ("GET", "/api/me") => json(200, serde_json::json!({ "username": "nate" })),
            ("GET", "/api/libraries") => json(
                200,
                serde_json::json!({ "libraries": [
                { "id": "lib1", "mediaType": "book" },
                { "id": "lib2", "mediaType": "podcast" } ] }),
            ),
            ("GET", "/api/libraries/lib1/items?limit=0&page=0") => {
                // The listing carries no chapters and no files.
                let mut b = book("li1", "Wizard's First Rule", 1);
                b["media"].as_object_mut().unwrap().remove("chapters");
                b["media"].as_object_mut().unwrap().remove("audioFiles");
                let mut c = book("li2", "A Split Book", 3);
                c["media"].as_object_mut().unwrap().remove("audioFiles");
                json(200, serde_json::json!({ "results": [c, b] }))
            }
            ("GET", "/api/items/li1?expanded=1") => {
                json(200, book("li1", "Wizard's First Rule", 1))
            }
            ("GET", "/api/items/li2?expanded=1") => json(200, book("li2", "A Split Book", 3)),
            ("GET", "/api/items/li1/file/ino0") => (200, "audio/mp4", audio_served.clone()),
            ("GET", "/api/me/progress/li1") => {
                json(200, serde_json::json!({ "currentTime": 12.5 }))
            }
            ("GET", "/api/me/progress/li2") => json(404, serde_json::json!({})),
            ("PATCH", "/api/me/progress/li1") => {
                let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                assert_eq!(v["currentTime"], 30.0);
                assert_eq!(v["duration"], 3600.5);
                (200, "text/plain", b"OK".to_vec())
            }
            _ => json(404, serde_json::json!({ "error": r.path.clone() })),
        });

        let abs = Audiobookshelf::login("abs", &base, "nate", "pw").unwrap();
        assert_eq!(abs.whoami().unwrap(), "nate");
        assert_eq!(
            abs.credentials(),
            Credentials {
                access: "acc1".into(),
                refresh: Some("ref1".into())
            }
        );

        let books = abs.items(Role::Speech).unwrap();
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].title, "A Split Book");
        assert_eq!(books[1].artist.as_deref(), Some("Someone"));
        assert_eq!(books[1].duration, Some(Duration::from_secs_f64(3600.5)));
        assert_eq!(books[1].tags, vec!["Fantasy", "long"]);
        assert!(abs.items(Role::Music).unwrap().is_empty());

        // The full record fills in the chapters.
        let item = abs.item(&"li1".into()).unwrap();
        assert_eq!(item.chapters.len(), 2);
        assert_eq!(item.chapters[1].title.as_deref(), Some("Two"));
        assert_eq!(item.format.as_deref(), Some("m4b"));

        let dir = std::env::temp_dir().join(format!("underscore-abs-{}", std::process::id()));
        let cache = Cache::open(&dir).unwrap();
        let path = abs.fetch(&"li1".into(), &cache).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), audio);
        assert_eq!(path.extension().unwrap(), "m4b");
        // Already cached: the same path, no download.
        assert_eq!(abs.fetch(&"li1".into(), &cache).unwrap(), path);

        let err = abs.fetch(&"li2".into(), &cache).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(ref m) if m.contains("3 audio files")),
            "{err}"
        );

        assert_eq!(
            abs.progress(&"li1".into()).unwrap(),
            Some(Duration::from_secs_f64(12.5))
        );
        assert_eq!(abs.progress(&"li2".into()).unwrap(), None);
        abs.report_progress(&"li1".into(), Duration::from_secs(30))
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The shelf joins the listing with the listener's progress, and
    /// leaves out books the server has lost.
    #[test]
    fn shelf_has_progress_covers_and_file_counts() {
        let (base, _) = serve(|r| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/libraries") => json(
                200,
                serde_json::json!({ "libraries": [{ "id": "lib1", "mediaType": "book" }] }),
            ),
            ("GET", "/api/libraries/lib1/items?limit=0&page=0") => {
                let mut a = book("li1", "Started", 0);
                a["addedAt"] = 1_700_000_000_000u64.into();
                a["media"]["numAudioFiles"] = 1.into();
                a["media"]["coverPath"] = "/covers/li1.jpg".into();
                let mut b = book("li2", "Untouched", 0);
                b["media"]["numAudioFiles"] = 4.into();
                let mut c = book("li3", "Gone", 0);
                c["isMissing"] = true.into();
                let d = book("li4", "Done", 0);
                json(200, serde_json::json!({ "results": [a, b, c, d] }))
            }
            ("GET", "/api/me") => json(
                200,
                serde_json::json!({ "username": "nate", "mediaProgress": [
                    { "libraryItemId": "li1", "progress": 0.25, "currentTime": 900.0,
                      "isFinished": false, "lastUpdate": 1_710_000_000_000u64 },
                    { "libraryItemId": "li4", "progress": 1.0, "currentTime": 3600.0,
                      "isFinished": true },
                    { "libraryItemId": "li2", "episodeId": "ep1", "progress": 0.5 }
                ] }),
            ),
            _ => json(404, serde_json::json!({})),
        });
        let abs = Audiobookshelf::with_credentials(
            "abs",
            &base,
            Credentials {
                access: "acc".into(),
                refresh: None,
            },
        );
        let shelf = abs.shelf().unwrap();
        let titles: Vec<_> = shelf.iter().map(|b| b.item.title.as_str()).collect();
        assert_eq!(titles, ["Done", "Started", "Untouched"]);
        let done = &shelf[0];
        assert!(done.finished);
        assert_eq!(done.progress, Some(1.0));
        let started = &shelf[1];
        assert_eq!(started.progress, Some(0.25));
        assert_eq!(started.played_ms, Some(1_710_000_000_000));
        assert_eq!(started.added_ms, Some(1_700_000_000_000));
        assert_eq!(started.files, Some(1));
        assert!(started.has_cover);
        // Podcast episode progress is not the book's.
        let untouched = &shelf[2];
        assert_eq!(untouched.progress, None);
        assert_eq!(untouched.files, Some(4));
        assert!(!untouched.has_cover);
    }

    /// A book plays while it downloads: the length comes from a one-byte
    /// range, the bytes from ranged requests, and the whole file lands in
    /// the cache. Opened again, it comes straight from the cache.
    #[test]
    fn streams_a_book_into_the_cache() {
        let audio: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        let served = audio.clone();
        let (base, _) = serve(move |r| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/items/li1?expanded=1") => {
                json(200, book("li1", "Wizard's First Rule", 1))
            }
            ("GET", "/api/items/li1/file/ino0") => {
                assert_eq!(r.auth.as_deref(), Some("Bearer acc"));
                let range = r.range.as_deref().unwrap().trim_start_matches("bytes=");
                let (a, b) = range.split_once('-').unwrap();
                let a: usize = a.parse().unwrap();
                let b: usize = b
                    .parse()
                    .map_or(served.len() - 1, |b: usize| b.min(served.len() - 1));
                // The header rides in the content type slot of this fake.
                let head = Box::leak(
                    format!("audio/mp4\r\nContent-Range: bytes {a}-{b}/{}", served.len())
                        .into_boxed_str(),
                );
                (206, &*head, served[a..=b].to_vec())
            }
            _ => json(404, serde_json::json!({})),
        });
        let abs = Arc::new(Audiobookshelf::with_credentials(
            "abs",
            &base,
            Credentials {
                access: "acc".into(),
                refresh: None,
            },
        ));
        let dir =
            std::env::temp_dir().join(format!("underscore-abs-stream-{}", std::process::id()));
        let cache = Cache::open(&dir).unwrap();
        let Opened::Streaming(dl) = abs.open_streaming(&"li1".into(), &cache).unwrap() else {
            panic!("should stream the first time");
        };
        assert_eq!(dl.len(), audio.len() as u64);
        let mut got = Vec::new();
        dl.reader().unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, audio);
        for _ in 0..500 {
            if dl.finished().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let path = dl.finished().expect("finished");
        assert_eq!(std::fs::read(&path).unwrap(), audio);
        let Opened::Cached(again) = abs.open_streaming(&"li1".into(), &cache).unwrap() else {
            panic!("should come from the cache the second time");
        };
        assert_eq!(again, path);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An expired access token is renewed with the refresh token, once,
    /// and the request goes through.
    #[test]
    fn renews_an_expired_token_and_retries() {
        let (base, count) = serve(|r| match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/auth/refresh") => {
                assert_eq!(r.auth.as_deref(), Some("refresh ref1"));
                json(
                    200,
                    serde_json::json!({ "user": { "accessToken": "acc2", "refreshToken": "ref2" } }),
                )
            }
            ("GET", "/api/me") if r.auth.as_deref() == Some("Bearer acc2") => {
                json(200, serde_json::json!({ "username": "nate" }))
            }
            _ => json(401, serde_json::json!({})),
        });
        let abs = Audiobookshelf::with_credentials(
            "abs",
            &base,
            Credentials {
                access: "stale".into(),
                refresh: Some("ref1".into()),
            },
        );
        assert_eq!(abs.whoami().unwrap(), "nate");
        assert_eq!(abs.credentials().access, "acc2");
        assert_eq!(abs.credentials().refresh.as_deref(), Some("ref2"));
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    /// Without a refresh token, a refused access token is a clear error,
    /// not a loop.
    #[test]
    fn a_refused_api_key_says_to_sign_in_again() {
        let (base, count) = serve(|_| json(401, serde_json::json!({})));
        let abs = Audiobookshelf::with_credentials(
            "abs",
            &base,
            Credentials {
                access: "revoked".into(),
                refresh: None,
            },
        );
        let err = abs.whoami().unwrap_err();
        assert!(err.to_string().contains("sign in again"), "{err}");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_wrong_password_is_reported_as_such() {
        let (base, _) = serve(|_| json(401, serde_json::json!({ "error": "Invalid" })));
        let err = Audiobookshelf::login("abs", &base, "nate", "wrong").unwrap_err();
        assert!(err.to_string().contains("username or password"), "{err}");
    }

    /// A fake server that writes whole raw responses, for the sign-in
    /// tests that need redirects and cookies. The handler sees the request
    /// target and the headers, lowercased.
    fn serve_raw(handler: impl Fn(&str, &str) -> String + Send + Sync + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut headers = String::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim_end().is_empty() {
                        break;
                    }
                    headers.push_str(&h.to_ascii_lowercase());
                }
                stream
                    .write_all(handler(&target, &headers).as_bytes())
                    .unwrap();
            }
        });
        base
    }

    fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
        target
            .split_once('?')?
            .1
            .split('&')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }

    fn raw(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// The server's side of the mobile flow: refuse our own address,
    /// allow the stock one, remember the challenge, and only hand out
    /// tokens to the verifier that matches it, with the session cookie.
    #[test]
    fn single_sign_on_falls_back_to_the_stock_address_and_checks_pkce() {
        let challenge_seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen = Arc::clone(&challenge_seen);
        let base = serve_raw(move |target, headers| {
            if target.starts_with("/auth/openid?") {
                if query(target, "redirect_uri") == Some("underscore%3A%2F%2Foauth") {
                    return raw("400 Bad Request", "", "Invalid redirect_uri");
                }
                assert_eq!(query(target, "code_challenge_method"), Some("S256"));
                *seen.lock().unwrap() = query(target, "code_challenge").unwrap().to_string();
                return raw(
                    "302 Found",
                    "Location: https://sso.example/authorize?x=1\r\n\
                     Set-Cookie: auth_method=openid-mobile; Path=/; HttpOnly\r\n\
                     Set-Cookie: connect.sid=abc123; Path=/; HttpOnly\r\n",
                    "",
                );
            }
            if target.starts_with("/auth/openid/callback?") {
                assert!(
                    headers.contains("cookie: auth_method=openid-mobile; connect.sid=abc123"),
                    "{headers}"
                );
                let verifier = query(target, "code_verifier").unwrap();
                if challenge(verifier) != *seen.lock().unwrap()
                    || query(target, "code") != Some("c0de")
                {
                    return raw("401 Unauthorized", "", "Unauthorized");
                }
                let body = r#"{"user":{"accessToken":"acc","refreshToken":"ref"}}"#;
                return raw("200 OK", "Content-Type: application/json\r\n", body);
            }
            raw("404 Not Found", "", "")
        });
        let start =
            Audiobookshelf::openid_start(&base, &["underscore://oauth", "audiobookshelf://oauth"])
                .unwrap();
        assert_eq!(start.redirect_uri, "audiobookshelf://oauth");
        assert_eq!(start.authorize_url, "https://sso.example/authorize?x=1");

        let wrong = Audiobookshelf::openid_finish("abs", &start, "c0de", "someone else's state");
        assert!(
            wrong.is_err(),
            "a state from another sign-in is refused before any request"
        );

        let state = start.state.clone();
        let abs = Audiobookshelf::openid_finish("abs", &start, "c0de", &state).unwrap();
        assert_eq!(
            abs.credentials(),
            Credentials {
                access: "acc".into(),
                refresh: Some("ref".into())
            }
        );
    }

    #[test]
    fn single_sign_on_says_which_addresses_were_refused() {
        let base = serve_raw(|_, _| raw("400 Bad Request", "", "Invalid redirect_uri"));
        let err = Audiobookshelf::openid_start(&base, &["underscore://oauth"]).unwrap_err();
        assert!(err.to_string().contains("underscore://oauth"), "{err}");
    }

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
