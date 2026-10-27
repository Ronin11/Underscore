# The map server and the worker

A listener opens a book and music plays under it. If no map exists yet,
one is made on the listener's own hardware, a chapter at a time, starting
where they are listening. This page is how that works and how to run it.

## The pieces

```
phone ──── map server ──── worker (GPU machine)
  │        (beside ABS)     │  whisper: speech to text
  │            │            │  local model (Ollama): scenes and moods
  │            │            │  CLAP: moods matched to music by sound
  │            └── Audiobookshelf (checks the phone's token)
  └── Audiobookshelf, Navidrome (books and music, as before)
```

- **The map server**, `crates/mapserver`, binary `underscore-maps`. Small,
  Rust, no audio. It keeps maps as files, each book's list of maps and its
  default, the queue of maps to make, and the transcripts the worker
  makes. Phones sign in with the Audiobookshelf token they already have:
  the server asks Audiobookshelf whose token it is. The worker uses a
  shared secret. Its routes are listed at the top of
  `crates/mapserver/src/api.rs`.
- **The worker**, `worker/`, Python. It asks the map server for a job,
  reads the book from Audiobookshelf, and for each chapter, starting with
  the listener's:
  1. Transcribes it with faster-whisper on the GPU, unless a transcript is
     already beside the book (`underscore.transcript.json`) or kept from
     before.
  2. Asks a local model, through Ollama, to split it into scenes. Each
     scene has a summary, the music it wants described as sound ("tense low
     strings, slow pulse"), an intensity, and whether it wants silence or a
     song the book names.
  3. Matches each mood to the music index by CLAP similarity. The index is
     an embedding of every song on the music server, heard at three
     windows. Scoring avoids short songs, songs just used, songs used a
     lot, and songs with singing. A song the book names plays where it is
     named, when the library has it.
  4. Writes the whole map so far and sends it. The map server makes it the
     book's default if it has none.

  Between jobs it adds new songs from the music server to the index.
- **The phone.** When a server book opens, the app asks the map server
  what it has. A default map, or a book's only map, loads without asking.
  With several maps and no default, the app asks which. With none, and no
  map beside the book, it asks for one from the chapter the listener is
  in, if "Make a map when a book has none" is on in Settings. While a map
  is being made, the app asks every minute how far it is. It takes the
  newer map only when the listener reaches a chapter its copy lacks, so
  the music playing now never starts over. If the listener jumps ahead,
  the worker is told and starts from there. The Music button has "Maps
  for this book…" and "Make a new map".

Why Python for the worker: the speech and music models it runs are built
for Python, and the worker never runs on a listener's device or touches
the mix. It reads files to analyse them. What it writes is a map, which
is data.

## The three rules

- **Nothing is mixed or written.** The worker decodes audio only to
  transcribe it and to embed songs. Songs it downloads to hear are
  deleted once embedded. It writes maps and transcripts.
- **No DRM.** It reads the files Audiobookshelf and Navidrome serve.
- **No streaming services.** Music comes from the listener's own server.

The book's text stays on the listener's network: the model runs in
Ollama on the worker's machine. Nothing is sent to a hosted model.

## Maps carry an identity

A map has an `id`, chosen once and kept through every edit, a `name`, and
`based_on` when it was copied from another map and changed. A map still
being made has `complete = false`; chapters it has not reached have no
cues and play without music. See `docs/music-map.md`.

A map made by hand can be added to a book with
`POST /api/books/{book}/maps`. One that already has an id replaces the
map with that id, so a better version of a shared map stays one map.

## Running it

### The map server

Beside Audiobookshelf, in the same compose file, so it reaches
Audiobookshelf by name. `deploy/map-server/compose.yml` is the service
to add; `deploy/map-server/Dockerfile` builds it from a checkout:

```
git clone https://github.com/Ronin11/Underscore /opt/underscore
# add the service from deploy/map-server/compose.yml, then:
echo "UNDERSCORE_WORKER_TOKEN=$(openssl rand -hex 24)" >> .env
docker compose up -d underscore-maps
```

Check `UNDERSCORE_MAPS_ABS` against the Audiobookshelf service's name and
port in that compose file. Phones away from home need it through the same
reverse proxy as Audiobookshelf, under its own name.

### The worker

On a machine with an NVIDIA GPU, Miniconda, and Ollama with a model
pulled. About 20 GB of GPU memory covers the default model, whisper, and
CLAP together; Ollama spills to the CPU if it runs short.

```
git clone https://github.com/Ronin11/Underscore ~/underscore
~/underscore/deploy/worker/install.sh
$EDITOR ~/.config/underscore/worker.env      # from worker.env.example
systemctl --user enable --now underscore-worker
sudo loginctl enable-linger $USER            # keep it running after logout
journalctl --user -u underscore-worker -f
```

The worker reads books and music fastest straight from the shared folders.
Set `UNDERSCORE_BOOK_PATHS` and `UNDERSCORE_MUSIC_ROOT` to where they are
mounted on the worker's machine; without them it downloads what it reads
through the servers. `UNDERSCORE_MUSIC_ONLY` keeps scoring to some
folders, such as a production-music library, while songs a book names are
still found anywhere.

The first job indexes the whole music library before it starts, which
takes a while once. After that a chapter takes about a minute: whisper
does a chapter in seconds on a GPU, and the model's reading is most of it.

### The phone

Connections, +, Map server, and its address. It signs in through
Audiobookshelf, so that needs to be added first.

## Tests

`cargo test -p underscore-mapserver` covers the store and the routes.
`python -m unittest discover worker/tests` covers chapter order, song
choice, and the map text; it writes `worker/tests/sample-map.toml`, which
the Rust test `generated_maps_parse_and_schedule` reads with the phone's
own parser.
