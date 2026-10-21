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
//! Every call here does network I/O. Run it off the audio thread.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::http::Response;

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

/// One Audiobookshelf server, signed in.
pub struct Audiobookshelf {
    name: String,
    base: String,
    agent: Agent,
    auth: Mutex<Credentials>,
    /// The last listing, so `item` and `search` need no round trip.
    books: RwLock<Vec<Item>>,
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
        for lib in libs
            .libraries
            .into_iter()
            .filter(|l| l.media_type == "book")
        {
            let path = format!("/api/libraries/{}/items?limit=0&page=0", lib.id);
            let mut resp = self.get(&path)?;
            let page: ItemsPage = read_json(&mut resp, "GET /api/libraries/{id}/items")?;
            books.extend(page.results.into_iter().filter_map(LibraryItem::into_item));
        }
        books.sort_by(|a, b| a.title.cmp(&b.title));
        let n = books.len();
        *self
            .books
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = books;
        Ok(n)
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
        let known = self
            .files
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned();
        let file = if let Some(f) = known {
            f
        } else {
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
            file
        };
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
struct Me {
    username: String,
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
}
