//! `underscore-maps`: keeps music maps for the books on an Audiobookshelf
//! server, and the queue of maps for a worker to build.
//!
//! It holds data only: maps, which name tracks and never carry audio, and
//! the jobs. It reads no audio and plays none. See `docs/map-server.md`.
//!
//! Settings come from the environment:
//!
//! ```text
//! UNDERSCORE_MAPS_DATA          where maps and state live      (/data)
//! UNDERSCORE_MAPS_LISTEN        address to listen on           (0.0.0.0:8790)
//! UNDERSCORE_MAPS_ABS           Audiobookshelf, for checking tokens  (required)
//! UNDERSCORE_MAPS_WORKER_TOKEN  the worker's shared secret     (required)
//! ```

mod api;
mod store;

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use api::Auth;
use store::Store;

/// How long a token Audiobookshelf accepted is taken without asking again.
const TOKEN_TTL: Duration = Duration::from_mins(10);
/// The largest request body: a long book's map is a few hundred kilobytes.
const MAX_BODY: u64 = 8 << 20;

struct AbsAuth {
    base: String,
    worker_token: String,
    agent: ureq::Agent,
    known: Mutex<HashMap<String, (String, Instant)>>,
}

#[derive(serde::Deserialize)]
struct Me {
    username: String,
}

impl Auth for AbsAuth {
    fn user(&self, token: &str) -> Option<String> {
        if let Some((user, at)) = self.known.lock().ok()?.get(token)
            && at.elapsed() < TOKEN_TTL
        {
            return Some(user.clone());
        }
        let resp = self
            .agent
            .get(format!("{}/api/me", self.base))
            .header("Authorization", format!("Bearer {token}"))
            .call();
        let user = match resp {
            Ok(mut r) => r.body_mut().read_json::<Me>().ok()?.username,
            Err(e) => {
                eprintln!("underscore-maps: checking a token with Audiobookshelf: {e}");
                return None;
            }
        };
        let mut known = self.known.lock().ok()?;
        known.retain(|_, (_, at)| at.elapsed() < TOKEN_TTL);
        known.insert(token.to_string(), (user.clone(), Instant::now()));
        Some(user)
    }

    fn worker(&self, token: &str) -> bool {
        // Compared in full every time, so the time taken says nothing.
        let (a, b) = (token.as_bytes(), self.worker_token.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn main() {
    let data = PathBuf::from(env("UNDERSCORE_MAPS_DATA").unwrap_or_else(|| "/data".into()));
    let listen = env("UNDERSCORE_MAPS_LISTEN").unwrap_or_else(|| "0.0.0.0:8790".into());
    let (Some(abs), Some(worker_token)) = (
        env("UNDERSCORE_MAPS_ABS"),
        env("UNDERSCORE_MAPS_WORKER_TOKEN"),
    ) else {
        eprintln!("underscore-maps: set UNDERSCORE_MAPS_ABS and UNDERSCORE_MAPS_WORKER_TOKEN");
        std::process::exit(2);
    };
    let store = match Store::open(&data) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("underscore-maps: {}: {e}", data.display());
            std::process::exit(1);
        }
    };
    let auth = Arc::new(AbsAuth {
        base: abs.trim_end_matches('/').to_string(),
        worker_token,
        agent: ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into(),
        known: Mutex::new(HashMap::new()),
    });
    let server = match tiny_http::Server::http(&listen) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("underscore-maps: listening on {listen}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "underscore-maps: listening on {listen}, maps in {}",
        data.display()
    );
    for mut request in server.incoming_requests() {
        let (store, auth) = (Arc::clone(&store), Arc::clone(&auth));
        std::thread::spawn(move || {
            let token = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
                .map(str::to_string);
            let mut body = String::new();
            if request
                .as_reader()
                .take(MAX_BODY)
                .read_to_string(&mut body)
                .is_err()
            {
                let _ = request
                    .respond(tiny_http::Response::from_string("not UTF-8").with_status_code(400));
                return;
            }
            let method = request.method().as_str().to_string();
            let url = request.url().to_string();
            let reply = api::handle(
                &store,
                auth.as_ref(),
                &method,
                &url,
                token.as_deref(),
                &body,
            );
            if reply.status >= 400 {
                eprintln!(
                    "underscore-maps: {method} {url}: {} {}",
                    reply.status, reply.body
                );
            }
            let header = tiny_http::Header::from_bytes("Content-Type", reply.content_type)
                .expect("a valid header");
            let response = tiny_http::Response::from_string(reply.body)
                .with_status_code(reply.status)
                .with_header(header);
            let _ = request.respond(response);
        });
    }
}
