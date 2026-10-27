//! The HTTP routes, apart from the socket, so they can be tested.
//!
//! A phone signs in with the Audiobookshelf token it already has; the
//! server asks Audiobookshelf who it belongs to. A worker sends the token
//! set in the server's environment.
//!
//! ```text
//! GET  /api/health                     no sign-in: is it up, when was the worker seen
//! GET  /api/me                         who the token belongs to
//! GET  /api/books/{book}/maps          the book's maps, default, and job
//! POST /api/books/{book}/maps          a map made by hand, as TOML
//! PUT  /api/books/{book}/default       {"map": id}
//! POST /api/books/{book}/build         {"chapter": n}: make a map, from there
//! GET  /api/books/{book}/transcript    the worker's transcript of the book
//! GET  /api/books/{book}/voices        who says what: spans of the transcript by speaker
//! POST /api/books/{book}/voices/build  {"chapter": n}: work out who says what, from there
//! GET  /api/maps/{id}                  the map, as TOML
//! POST /api/worker/claim               the next job, or 204
//! POST /api/worker/jobs/{job}          progress, the map so far, or the end
//! PUT  /api/worker/books/{book}/transcript  the transcript, as the phone imports it
//! PUT  /api/worker/books/{book}/voices      who says what
//! GET  /api/worker/books/{book}/transcript  what the server keeps, so a worker never
//! GET  /api/worker/books/{book}/voices      transcribes or listens to a book twice
//! ```

use serde::Deserialize;

use crate::store::{Error, Progress, Store};

/// Who may ask.
pub trait Auth: Send + Sync {
    /// The user an Audiobookshelf token belongs to, or None when it is
    /// refused.
    fn user(&self, token: &str) -> Option<String>;
    /// Whether this is the worker's token.
    fn worker(&self, token: &str) -> bool;
}

/// What to send back.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    fn json(status: u16, v: &serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: v.to_string(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }

    /// A stored file as it is, or why there is none.
    fn file(r: Result<String, Error>, content_type: &'static str) -> Self {
        match r {
            Ok(body) => Self {
                status: 200,
                content_type,
                body,
            },
            Err(e) => Self::of(&e),
        }
    }

    fn of(e: &Error) -> Self {
        match e {
            Error::NotFound(m) => Self::error(404, m),
            Error::Invalid(m) => Self::error(400, m),
            Error::Io(m) => Self::error(500, m),
        }
    }
}

#[derive(Deserialize)]
struct Default {
    map: String,
}

#[derive(Deserialize)]
struct Build {
    #[serde(default)]
    chapter: u32,
}

/// Answer one request. `token` is the bearer token, if any.
pub fn handle(
    store: &Store,
    auth: &dyn Auth,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> Reply {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').skip(1).collect();
    if parts.first() != Some(&"api") {
        return Reply::error(404, "not here");
    }
    let rest = &parts[1..];
    if rest == ["health"] {
        let seen = store.worker_seen();
        return Reply::json(200, &serde_json::json!({ "ok": true, "worker_seen": seen }));
    }
    let Some(token) = token else {
        return Reply::error(401, "sign in with your Audiobookshelf token");
    };

    if rest.first() == Some(&"worker") {
        if !auth.worker(token) {
            return Reply::error(403, "not the worker's token");
        }
        return match (method, &rest[1..]) {
            ("POST", ["claim"]) => match store.claim() {
                Ok(Some(job)) => Reply::json(200, &serde_json::json!(job)),
                Ok(None) => Reply {
                    status: 204,
                    content_type: "application/json",
                    body: String::new(),
                },
                Err(e) => Reply::of(&e),
            },
            ("GET", ["books", book, "transcript"]) => {
                Reply::file(store.transcript(book), "application/json")
            }
            ("GET", ["books", book, "voices"]) => {
                Reply::file(store.voices(book), "application/json")
            }
            ("PUT", ["books", book, "voices"]) => match store.set_voices(book, body) {
                Ok(()) => Reply::json(200, &serde_json::json!({ "ok": true })),
                Err(e) => Reply::of(&e),
            },
            ("PUT", ["books", book, "transcript"]) => match store.set_transcript(book, body) {
                Ok(()) => Reply::json(200, &serde_json::json!({ "ok": true })),
                Err(e) => Reply::of(&e),
            },
            ("POST", ["jobs", job]) => match serde_json::from_str::<Progress>(body) {
                Ok(news) => match store.progress(job, &news) {
                    Ok(job) => Reply::json(200, &serde_json::json!(job)),
                    Err(e) => Reply::of(&e),
                },
                Err(e) => Reply::error(400, &e.to_string()),
            },
            _ => Reply::error(404, "no such worker route"),
        };
    }

    let Some(user) = auth.user(token) else {
        return Reply::error(401, "Audiobookshelf did not accept the token");
    };
    match (method, rest) {
        ("GET", ["me"]) => Reply::json(200, &serde_json::json!({ "user": user })),
        ("GET", ["books", book, "maps"]) => Reply::json(200, &store.book(book)),
        ("POST", ["books", book, "maps"]) => match store.add_map(book, body) {
            Ok(info) => Reply::json(200, &serde_json::json!(info)),
            Err(e) => Reply::of(&e),
        },
        ("PUT", ["books", book, "default"]) => match serde_json::from_str::<Default>(body) {
            Ok(d) => match store.set_default(book, &d.map) {
                Ok(()) => Reply::json(200, &store.book(book)),
                Err(e) => Reply::of(&e),
            },
            Err(e) => Reply::error(400, &e.to_string()),
        },
        ("POST", ["books", book, "build"]) => {
            let chapter = serde_json::from_str::<Build>(body).map_or(1, |b| b.chapter);
            match store.build(book, chapter) {
                Ok(job) => Reply::json(200, &serde_json::json!(job)),
                Err(e) => Reply::of(&e),
            }
        }
        ("GET", ["books", book, "voices"]) => Reply::file(store.voices(book), "application/json"),
        ("POST", ["books", book, "voices", "build"]) => {
            let chapter = serde_json::from_str::<Build>(body).map_or(1, |b| b.chapter);
            match store.build_voices(book, chapter) {
                Ok(job) => Reply::json(200, &serde_json::json!(job)),
                Err(e) => Reply::of(&e),
            }
        }
        ("GET", ["books", book, "transcript"]) => {
            Reply::file(store.transcript(book), "application/json")
        }
        ("GET", ["maps", id]) => Reply::file(store.map(id), "application/toml"),
        _ => Reply::error(404, "no such route"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Auth for Fake {
        fn user(&self, token: &str) -> Option<String> {
            (token == "listener").then(|| "nate".into())
        }
        fn worker(&self, token: &str) -> bool {
            token == "worker"
        }
    }

    fn store() -> Store {
        let tag = crate::store::new_id().unwrap();
        Store::open(&std::env::temp_dir().join(format!("underscore-api-{tag}"))).unwrap()
    }

    #[test]
    fn health_needs_no_token_and_the_rest_do() {
        let s = store();
        assert_eq!(
            handle(&s, &Fake, "GET", "/api/health", None, "").status,
            200
        );
        assert_eq!(handle(&s, &Fake, "GET", "/api/me", None, "").status, 401);
        assert_eq!(
            handle(&s, &Fake, "GET", "/api/me", Some("stranger"), "").status,
            401
        );
        let me = handle(&s, &Fake, "GET", "/api/me", Some("listener"), "");
        assert!(me.body.contains("nate"));
    }

    #[test]
    fn listeners_cannot_use_the_worker_routes() {
        let s = store();
        assert_eq!(
            handle(&s, &Fake, "POST", "/api/worker/claim", Some("listener"), "").status,
            403
        );
        assert_eq!(
            handle(&s, &Fake, "POST", "/api/worker/claim", Some("worker"), "").status,
            204
        );
    }

    #[test]
    fn a_build_reaches_the_worker() {
        let s = store();
        let r = handle(
            &s,
            &Fake,
            "POST",
            "/api/books/li1/build",
            Some("listener"),
            r#"{"chapter": 4}"#,
        );
        assert_eq!(r.status, 200);
        let claim = handle(&s, &Fake, "POST", "/api/worker/claim", Some("worker"), "");
        let job: serde_json::Value = serde_json::from_str(&claim.body).unwrap();
        assert_eq!(job["book"], "li1");
        assert_eq!(job["chapter"], 4);
        let list = handle(
            &s,
            &Fake,
            "GET",
            "/api/books/li1/maps",
            Some("listener"),
            "",
        );
        assert!(list.body.contains("running"));
    }

    #[test]
    fn the_worker_can_read_back_what_it_sent() {
        let s = store();
        let path = "/api/worker/books/li1/transcript";
        assert_eq!(
            handle(&s, &Fake, "GET", path, Some("worker"), "").status,
            404
        );
        assert_eq!(
            handle(
                &s,
                &Fake,
                "PUT",
                path,
                Some("worker"),
                r#"{"chapters": []}"#
            )
            .status,
            200
        );
        let back = handle(&s, &Fake, "GET", path, Some("worker"), "");
        assert_eq!(back.status, 200);
        assert!(back.body.contains("chapters"));
        assert_eq!(
            handle(&s, &Fake, "GET", path, Some("listener"), "").status,
            403
        );
    }

    #[test]
    fn bad_maps_are_refused_with_the_reason() {
        let s = store();
        let r = handle(
            &s,
            &Fake,
            "POST",
            "/api/books/li1/maps",
            Some("listener"),
            "version = 9",
        );
        assert_eq!(r.status, 400);
        assert!(r.body.contains("version 9"));
    }
}
