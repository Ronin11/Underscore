//! A Subsonic-compatible server (Navidrome, Gonic, Airsonic) as a [`Library`].
//!
//! Music comes from the listener's own server. The whole song list is
//! loaded once, in pages, and kept in memory; after that `search` and
//! `item` answer without touching the network, so a map can be resolved
//! under a lock. A track is fetched on demand as the file the server
//! holds, byte for byte, into the per-stream [`Cache`]. Nothing is
//! transcoded, decrypted, or sent anywhere: the only bytes that move are
//! the server's own files, downwards.
//!
//! The server never sees a password after sign-in. Subsonic logins are a
//! salt and `md5(password + salt)`; that pair is what [`SubsonicLogin`]
//! keeps and what goes on the wire, so it is also all there is to save.
//!
//! This is a music library. It lists no books and has no notion of how far
//! through a song the listener is.
//!
//! Every call here does network I/O except `search`, `item`, and `items`
//! once the song list is loaded. Run the rest off the audio thread.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::http::Response;

use crate::{Cache, Error, Item, ItemId, Library, Query, Role, normalize};

/// The API level this client speaks. Every server that speaks Subsonic accepts it.
const API_VERSION: &str = "1.16.1";

/// Songs asked for per request when loading the list.
const PAGE: usize = 500;

/// A stop in case a server never runs out of pages.
const MAX_PAGES: usize = 400;

/// What to keep to sign in again without a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsonicLogin {
    /// The account name.
    pub user: String,
    /// The salt the token was made with.
    pub salt: String,
    /// `md5(password + salt)`, in lowercase hex.
    pub token: String,
}

impl SubsonicLogin {
    /// Make a login from a password, with a fresh salt.
    #[must_use]
    pub fn from_password(user: &str, password: &str) -> Self {
        let salt = fresh_salt();
        let token = md5_hex(&format!("{password}{salt}"));
        Self {
            user: user.to_string(),
            salt,
            token,
        }
    }
}

/// One Subsonic-compatible server, signed in.
pub struct Subsonic {
    name: String,
    base: String,
    agent: Agent,
    login: SubsonicLogin,
    songs: RwLock<Catalog>,
}

/// Every song, and each title folded the way [`Query`] folds it, so a
/// title search can skip most of the list without allocating.
#[derive(Default)]
struct Catalog {
    items: Vec<Item>,
    titles: Vec<String>,
}

impl std::fmt::Debug for Subsonic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subsonic")
            .field("name", &self.name)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Subsonic {
    /// Sign in with a username and password. `base` is the server's
    /// address, such as `https://music.example.net`.
    ///
    /// # Errors
    /// The server could not be reached or refused the login.
    pub fn login(
        name: impl Into<String>,
        base: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, Error> {
        let this = Self::with_login(name, base, SubsonicLogin::from_password(username, password));
        this.ping()?;
        Ok(this)
    }

    /// Use a saved login. Nothing is sent until the first request.
    pub fn with_login(name: impl Into<String>, base: &str, login: SubsonicLogin) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .user_agent("underscore")
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self {
            name: name.into(),
            base: base.trim_end_matches('/').to_string(),
            agent,
            login,
            songs: RwLock::new(Catalog::default()),
        }
    }

    /// The server's address.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The login to save for next time.
    #[must_use]
    pub fn credentials(&self) -> SubsonicLogin {
        self.login.clone()
    }

    /// Check that the server answers and accepts the login.
    ///
    /// # Errors
    /// The server could not be reached or refused the login.
    pub fn ping(&self) -> Result<(), Error> {
        self.reply("ping", &[]).map(|_| ())
    }

    /// Load every song on the server. Returns how many there are.
    ///
    /// # Errors
    /// The server could not be reached or refused the login.
    pub fn refresh_items(&self) -> Result<usize, Error> {
        let mut items = Vec::new();
        for page in 0..MAX_PAGES {
            let offset = (page * PAGE).to_string();
            let size = PAGE.to_string();
            let reply = self.reply(
                "search3",
                &[
                    ("query", ""),
                    ("artistCount", "0"),
                    ("albumCount", "0"),
                    ("songCount", &size),
                    ("songOffset", &offset),
                ],
            )?;
            let songs = reply.search.map(|s| s.song).unwrap_or_default();
            let n = songs.len();
            items.extend(songs.into_iter().map(Song::into_item));
            if n < PAGE {
                break;
            }
        }
        let n = items.len();
        let titles = items.iter().map(|i| normalize(&i.title)).collect();
        *self
            .songs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Catalog { items, titles };
        Ok(n)
    }

    /// Load the song list if it has not been loaded yet.
    fn ensure_loaded(&self) -> Result<(), Error> {
        let empty = self
            .songs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .is_empty();
        if empty {
            self.refresh_items()?;
        }
        Ok(())
    }

    /// A GET to `/rest/<endpoint>` with the login and `params`.
    fn get(&self, endpoint: &str, params: &[(&str, &str)]) -> Result<Response<ureq::Body>, Error> {
        let mut req = self
            .agent
            .get(format!("{}/rest/{endpoint}", self.base))
            .query("u", &self.login.user)
            .query("t", &self.login.token)
            .query("s", &self.login.salt)
            .query("v", API_VERSION)
            .query("c", "underscore")
            .query("f", "json");
        for (k, v) in params {
            req = req.query(k, v);
        }
        req.call()
            .map_err(|e| Error::Backend(format!("music server: {e}")))
    }

    /// A call that answers in JSON. Subsonic reports failures in the body
    /// with a 200, so the body decides.
    fn reply(&self, endpoint: &str, params: &[(&str, &str)]) -> Result<Reply, Error> {
        let mut resp = self.get(endpoint, params)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(Error::Backend(format!(
                "music server {endpoint}: HTTP {status}"
            )));
        }
        let env: Envelope = resp
            .body_mut()
            .read_json()
            .map_err(|e| Error::Backend(format!("music server {endpoint}: {e}")))?;
        let reply = env.response;
        if reply.status == "ok" {
            return Ok(reply);
        }
        let err = reply.error.unwrap_or_default();
        Err(Error::Backend(match err.code {
            40 | 41 => "the music server refused the username or password".to_string(),
            _ => format!("music server {endpoint}: {}", err.message),
        }))
    }
}

impl Library for Subsonic {
    fn name(&self) -> &str {
        &self.name
    }

    fn items(&self, role: Role) -> Result<Vec<Item>, Error> {
        if role != Role::Music {
            return Ok(Vec::new());
        }
        self.ensure_loaded()?;
        Ok(self
            .songs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .clone())
    }

    fn item(&self, id: &ItemId) -> Result<Item, Error> {
        self.ensure_loaded()?;
        self.songs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .iter()
            .find(|i| &i.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.clone()))
    }

    fn search(&self, query: &Query) -> Result<Vec<Item>, Error> {
        self.ensure_loaded()?;
        let catalog = self
            .songs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A title match needs one title to contain the other, so most of a
        // large library can be skipped before scoring anything.
        match query
            .title
            .as_deref()
            .map(normalize)
            .filter(|t| !t.is_empty())
        {
            Some(want) => {
                let near = catalog
                    .items
                    .iter()
                    .zip(&catalog.titles)
                    .filter(|(_, have)| {
                        !have.is_empty() && (have.contains(&want) || want.contains(have.as_str()))
                    })
                    .map(|(item, _)| item);
                Ok(query.rank(near))
            }
            None => Ok(query.rank(catalog.items.iter())),
        }
    }

    fn fetch(&self, id: &ItemId, cache: &Cache) -> Result<PathBuf, Error> {
        self.ensure_loaded()?;
        let ext = self
            .songs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .iter()
            .find(|i| &i.id == id)
            .ok_or_else(|| Error::NotFound(id.clone()))?
            .format
            .clone()
            .unwrap_or_else(|| "mp3".to_string());
        let cached = cache.get_or_fill(&self.name, id, &ext, |out| {
            let mut resp = self
                .get("download", &[("id", id.0.as_str())])
                .map_err(io::Error::other)?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(io::Error::other(format!("download {id}: HTTP {status}")));
            }
            // A failed download is a JSON or XML body with a 200.
            let kind = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if kind.contains("json") || kind.contains("xml") || kind.starts_with("text/") {
                return Err(io::Error::other(format!(
                    "download {id}: the server sent an error instead of a file"
                )));
            }
            let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
            io::copy(&mut body, out)?;
            Ok(())
        })?;
        Ok(cached)
    }

    fn progress(&self, _id: &ItemId) -> Result<Option<Duration>, Error> {
        Ok(None)
    }

    fn report_progress(&self, _id: &ItemId, _at: Duration) -> Result<(), Error> {
        Ok(())
    }
}

/// A salt that differs between logins. It only has to be fresh, not
/// secret: it travels beside the token on every request.
fn fresh_salt() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut salt = md5_hex(&format!("{nanos}:{}:{n}", std::process::id()));
    salt.truncate(16);
    salt
}

fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes())
        .iter()
        .fold(String::with_capacity(32), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

// The parts of the server's JSON this client reads. Everything else is
// ignored, and every field the server might leave out has a default.

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Reply,
}

#[derive(Deserialize)]
struct Reply {
    status: String,
    #[serde(default)]
    error: Option<ApiError>,
    #[serde(default, rename = "searchResult3")]
    search: Option<SearchResult>,
}

#[derive(Deserialize, Default)]
struct ApiError {
    #[serde(default)]
    code: u32,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    song: Vec<Song>,
}

#[derive(Deserialize)]
struct Song {
    id: String,
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    genre: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
}

impl Song {
    fn into_item(self) -> Item {
        let tags = self
            .genre
            .iter()
            .flat_map(|g| g.split([';', ',', '/']))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        Item {
            id: ItemId(self.id),
            role: Role::Music,
            title: self.title,
            artist: self.artist.filter(|a| !a.is_empty()),
            album: self.album.filter(|a| !a.is_empty()),
            duration: self.duration.map(Duration::from_secs),
            tags,
            chapters: Vec::new(),
            format: self
                .suffix
                .map(|s| s.to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// A reply: content type and body. The status is always 200, as with
    /// a real Subsonic server.
    type Answer = (&'static str, Vec<u8>);

    /// A fake server: a closure answers each request path and query.
    /// Returns the base URL and a counter of requests served.
    fn serve(
        handler: impl Fn(&str) -> Answer + Send + Sync + 'static,
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
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim_end().is_empty() {
                        break;
                    }
                }
                served.fetch_add(1, Ordering::SeqCst);
                let (kind, body) = handler(&target);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (base, count)
    }

    fn json(v: &serde_json::Value) -> Answer {
        (
            "application/json",
            serde_json::to_vec(&serde_json::json!({ "subsonic-response": v })).unwrap(),
        )
    }

    fn ok() -> serde_json::Value {
        serde_json::json!({ "status": "ok", "version": "1.16.1" })
    }

    fn song(n: usize, title: &str, artist: &str, album: &str) -> serde_json::Value {
        serde_json::json!({
            "id": format!("s{n}"), "title": title, "artist": artist, "album": album,
            "duration": 200, "genre": "Soundtrack", "suffix": "MP3",
        })
    }

    fn files_under(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .map(|e| e.unwrap().path())
                .map(|p| if p.is_dir() { files_under(&p) } else { 1 })
                .sum()
        })
    }

    fn param<'a>(target: &'a str, key: &str) -> Option<&'a str> {
        target
            .split_once('?')?
            .1
            .split('&')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }

    #[test]
    fn token_is_md5_of_password_and_salt() {
        // The worked example from the Subsonic API documentation.
        assert_eq!(md5_hex("sesamec19b2d"), "26719a1196d2a940705a59634eb18eab");
        let a = SubsonicLogin::from_password("nate", "sesame");
        let b = SubsonicLogin::from_password("nate", "sesame");
        assert_eq!(a.token, md5_hex(&format!("sesame{}", a.salt)));
        assert_ne!(a.salt, b.salt, "every login gets its own salt");
    }

    #[test]
    fn login_sends_a_token_and_never_the_password() {
        let (base, _) = serve(|target| {
            assert!(target.starts_with("/rest/ping?"), "{target}");
            assert_eq!(param(target, "u"), Some("nate"));
            assert!(
                !target.contains("hunter2"),
                "the password went out: {target}"
            );
            let (s, t) = (param(target, "s").unwrap(), param(target, "t").unwrap());
            assert_eq!(t, md5_hex(&format!("hunter2{s}")));
            json(&ok())
        });
        let lib = Subsonic::login("music", &base, "nate", "hunter2").unwrap();
        assert_eq!(lib.credentials().user, "nate");
    }

    #[test]
    fn a_refused_login_says_so() {
        let (base, _) = serve(|_| {
            json(&serde_json::json!({
                "status": "failed",
                "error": { "code": 40, "message": "Wrong username or password" }
            }))
        });
        let err = Subsonic::login("music", &base, "nate", "nope").unwrap_err();
        assert_eq!(
            err.to_string(),
            "the music server refused the username or password"
        );
    }

    #[test]
    fn the_song_list_loads_in_pages_and_searches_from_memory() {
        let (base, count) = serve(|target| {
            if !target.starts_with("/rest/search3?") {
                return json(&ok());
            }
            let offset: usize = param(target, "songOffset").unwrap().parse().unwrap();
            // 501 songs: a full first page, then one more.
            let songs: Vec<_> = (offset..(offset + PAGE).min(PAGE + 1))
                .map(|n| {
                    if n == 7 {
                        song(n, "Heartbeat", "SideTrax", "Pack 02 - Ambient music")
                    } else {
                        song(n, &format!("Filler {n}"), "SideTrax", "Pack 99")
                    }
                })
                .collect();
            json(&serde_json::json!({ "status": "ok", "searchResult3": { "song": songs } }))
        });
        let lib = Subsonic::with_login("music", &base, SubsonicLogin::from_password("n", "p"));
        assert_eq!(lib.refresh_items().unwrap(), PAGE + 1);
        let loaded = count.load(Ordering::SeqCst);
        assert_eq!(loaded, 2, "one request per page");

        let hit = lib
            .search(&Query::track(
                "SideTrax",
                "heartbeat",
                Some("Pack 02 - Ambient music"),
            ))
            .unwrap();
        assert_eq!(hit[0].id, ItemId::from("s7"));
        assert_eq!(
            hit[0].format.as_deref(),
            Some("mp3"),
            "suffix folds to lowercase"
        );
        assert_eq!(hit[0].tags, ["Soundtrack"]);
        assert!(
            lib.search(&Query::track("SideTrax", "No Such Song", None))
                .unwrap()
                .is_empty()
        );
        assert_eq!(lib.item(&ItemId::from("s7")).unwrap().title, "Heartbeat");
        assert!(matches!(
            lib.item(&ItemId::from("nope")),
            Err(Error::NotFound(_))
        ));
        assert_eq!(
            count.load(Ordering::SeqCst),
            loaded,
            "searching made no requests"
        );
    }

    #[test]
    fn a_song_is_downloaded_once_into_the_cache() {
        let bytes = b"ID3 not really audio".to_vec();
        let body = bytes.clone();
        let (base, count) = serve(move |target| {
            if target.starts_with("/rest/download?") {
                assert_eq!(param(target, "id"), Some("s1"));
                return ("audio/mpeg", body.clone());
            }
            json(&serde_json::json!({
                "status": "ok",
                "searchResult3": { "song": [song(1, "Evolve", "SideTrax", "Pack 83")] }
            }))
        });
        let lib = Subsonic::with_login("music", &base, SubsonicLogin::from_password("n", "p"));
        let dir = std::env::temp_dir().join(format!("underscore-subsonic-{}", std::process::id()));
        let cache = Cache::open(&dir).unwrap();
        let id = ItemId::from("s1");
        let path = lib.fetch(&id, &cache).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp3"));
        let served = count.load(Ordering::SeqCst);
        assert_eq!(lib.fetch(&id, &cache).unwrap(), path);
        assert_eq!(
            count.load(Ordering::SeqCst),
            served,
            "second fetch is from the cache"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_error_body_is_not_cached_as_a_song() {
        let (base, _) = serve(|target| {
            if target.starts_with("/rest/download?") {
                return json(&serde_json::json!({
                    "status": "failed", "error": { "code": 70, "message": "not found" }
                }));
            }
            json(&serde_json::json!({
                "status": "ok",
                "searchResult3": { "song": [song(1, "Evolve", "SideTrax", "Pack 83")] }
            }))
        });
        let lib = Subsonic::with_login("music", &base, SubsonicLogin::from_password("n", "p"));
        let dir =
            std::env::temp_dir().join(format!("underscore-subsonic-err-{}", std::process::id()));
        let cache = Cache::open(&dir).unwrap();
        assert!(lib.fetch(&ItemId::from("s1"), &cache).is_err());
        assert_eq!(
            files_under(&dir),
            0,
            "a failed download leaves nothing behind"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn it_lists_no_books() {
        let (base, count) = serve(|_| json(&ok()));
        let lib = Subsonic::with_login("music", &base, SubsonicLogin::from_password("n", "p"));
        assert!(lib.items(Role::Speech).unwrap().is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
