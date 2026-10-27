//! A map server: maps for the books on an Audiobookshelf server, the
//! default map for each, and maps built on request. See
//! `docs/map-server.md` and `crates/mapserver`.
//!
//! It signs in with the Audiobookshelf token the app already has, so
//! adding one needs only its address. Maps are data; nothing here moves
//! audio.

use std::io::Read;
use std::time::Duration;

use ureq::Agent;

use crate::{Audiobookshelf, Error};

/// The largest answer read: a long book's map or transcript.
const MAX_BODY: u64 = 32 << 20;

pub struct MapServer {
    base: String,
    agent: Agent,
}

impl std::fmt::Debug for MapServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapServer")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl MapServer {
    /// The map server at `base`, such as `https://maps.example.net`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .user_agent("underscore")
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();
        Self {
            base: base.trim().trim_end_matches('/').to_string(),
            agent,
        }
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// One request with the book server's token. A refused token is
    /// renewed through the book server, once, and the request sent again.
    /// Returns the status and the body.
    fn call(
        &self,
        books: &Audiobookshelf,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, Vec<u8>), Error> {
        let mut renewed = false;
        loop {
            let token = books.credentials().access;
            let url = format!("{}{path}", self.base);
            let auth = format!("Bearer {token}");
            let resp = match (method, body) {
                ("GET", _) => self.agent.get(&url).header("Authorization", &auth).call(),
                ("PUT", b) => self
                    .agent
                    .put(&url)
                    .header("Authorization", &auth)
                    .header("Content-Type", "application/json")
                    .send(b.unwrap_or("")),
                (_, b) => self
                    .agent
                    .post(&url)
                    .header("Authorization", &auth)
                    .header("Content-Type", "application/json")
                    .send(b.unwrap_or("")),
            };
            let mut resp = resp.map_err(|e| Error::Backend(format!("map server: {e}")))?;
            let status = resp.status().as_u16();
            if status == 401 && !renewed {
                // Asking the book server who we are renews a stale token.
                books.whoami()?;
                renewed = true;
                continue;
            }
            let mut out = Vec::new();
            resp.body_mut()
                .as_reader()
                .take(MAX_BODY)
                .read_to_end(&mut out)
                .map_err(|e| Error::Backend(format!("map server: {e}")))?;
            return Ok((status, out));
        }
    }

    /// The body as text when the status is a success, otherwise the
    /// server's reason.
    fn ok(
        &self,
        books: &Audiobookshelf,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, Error> {
        let (status, bytes) = self.call(books, method, path, body)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if (200..300).contains(&status) {
            return Ok(text);
        }
        let reason = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(text);
        Err(Error::Backend(format!(
            "map server: HTTP {status}: {reason}"
        )))
    }

    /// Who the map server takes us for: a check that it is there and
    /// accepts the book server's sign-in.
    ///
    /// # Errors
    /// Not reachable, or the token is refused.
    pub fn check(&self, books: &Audiobookshelf) -> Result<String, Error> {
        let text = self.ok(books, "GET", "/api/me", None)?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| Error::Backend(format!("map server: {e}")))?;
        Ok(v["user"].as_str().unwrap_or_default().to_string())
    }

    /// Book `id`'s maps, default, and job, as the server's JSON.
    ///
    /// # Errors
    /// Not reachable, or refused.
    pub fn book(&self, books: &Audiobookshelf, id: &str) -> Result<String, Error> {
        self.ok(books, "GET", &format!("/api/books/{id}/maps"), None)
    }

    /// One map, as TOML.
    ///
    /// # Errors
    /// Not reachable, refused, or no such map.
    pub fn map(&self, books: &Audiobookshelf, id: &str) -> Result<String, Error> {
        self.ok(books, "GET", &format!("/api/maps/{id}"), None)
    }

    /// Ask for a map for book `id`, built from `chapter` on. Returns the job.
    ///
    /// # Errors
    /// Not reachable, or refused.
    pub fn build(&self, books: &Audiobookshelf, id: &str, chapter: u32) -> Result<String, Error> {
        let body = serde_json::json!({ "chapter": chapter }).to_string();
        self.ok(
            books,
            "POST",
            &format!("/api/books/{id}/build"),
            Some(&body),
        )
    }

    /// Make `map` the map book `id` opens with. Returns the book's maps.
    ///
    /// # Errors
    /// Not reachable, refused, or not one of the book's maps.
    pub fn set_default(
        &self,
        books: &Audiobookshelf,
        id: &str,
        map: &str,
    ) -> Result<String, Error> {
        let body = serde_json::json!({ "map": map }).to_string();
        self.ok(
            books,
            "PUT",
            &format!("/api/books/{id}/default"),
            Some(&body),
        )
    }

    /// The transcript the server made of book `id`, or None.
    ///
    /// # Errors
    /// Not reachable, or refused.
    pub fn transcript(&self, books: &Audiobookshelf, id: &str) -> Result<Option<Vec<u8>>, Error> {
        let (status, bytes) =
            self.call(books, "GET", &format!("/api/books/{id}/transcript"), None)?;
        match status {
            200..300 => Ok(Some(bytes)),
            404 => Ok(None),
            s => Err(Error::Backend(format!("map server: HTTP {s}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;
    use crate::Credentials;

    /// Answer each request in turn with the next of `replies`, and send
    /// back the requests seen, as `METHOD path auth`.
    fn serve(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut auth = String::new();
                let mut length = 0usize;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("authorization:") {
                        auth = v.trim().to_string();
                    }
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut rest = vec![0; length];
                reader.read_exact(&mut rest).unwrap();
                let parts: Vec<&str> = line.split_whitespace().collect();
                seen.push(format!("{} {} {auth}", parts[0], parts[1]));
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            seen
        });
        (base, handle)
    }

    fn books(base: &str) -> Audiobookshelf {
        Audiobookshelf::with_credentials(
            "abs",
            base,
            Credentials {
                access: "tok".into(),
                refresh: None,
            },
        )
    }

    #[test]
    fn asks_with_the_book_servers_token() {
        let (base, seen) = serve(vec![(200, r#"{"default":null,"maps":[],"job":null}"#)]);
        let maps = MapServer::new(&format!("{base}/"));
        let text = maps.book(&books("http://127.0.0.1:1"), "li1").unwrap();
        assert!(text.contains("\"maps\""));
        assert_eq!(
            seen.join().unwrap(),
            vec!["GET /api/books/li1/maps bearer tok"]
        );
    }

    #[test]
    fn the_servers_reason_comes_through() {
        let (base, seen) = serve(vec![(404, r#"{"error":"no map x"}"#)]);
        let err = MapServer::new(&base)
            .map(&books("http://127.0.0.1:1"), "x")
            .unwrap_err();
        assert!(err.to_string().contains("no map x"), "{err}");
        seen.join().unwrap();
    }

    #[test]
    fn no_transcript_is_none_not_an_error() {
        let (base, seen) = serve(vec![(404, r#"{"error":"none"}"#)]);
        let t = MapServer::new(&base)
            .transcript(&books("http://127.0.0.1:1"), "li1")
            .unwrap();
        assert!(t.is_none());
        seen.join().unwrap();
    }
}
