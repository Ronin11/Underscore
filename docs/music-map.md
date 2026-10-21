# Music maps

A map is a plan for one book: for each stretch of it, what should play
underneath and how. The player reads maps. A separate tool writes them.
This page is the format, the reasoning, and where the pieces live.

## What a map is, and is not

A map is data. It names tracks by artist and title, it says which part of
the track to use and whether to repeat it, and it anchors each cue to a
chapter and an offset. It carries no audio, no book text beyond a few words
per anchor, and nothing from the music beyond its tags.

That keeps it on the right side of all three rules in `README.md`:

- Nothing is mixed or written. The map says what to play; the engine
  plays it in memory as it does everything else.
- Nothing is decrypted. Maps do not care where the book or the music came
  from, only what they are.
- No streaming service is involved. A cue resolves against the listener's
  own library, local or self-hosted.

Because it is only metadata, a map for a popular book can be shared the
way a playlist is, and resolved against whatever each listener owns. A
cue that names a track most people lack falls back to its vibe tags. That
is deliberate: books call out songs, and the song sets the tone even when
the listener does not have it.

## The format

TOML, one file per book. The example is `crates/map/src/lib.rs`'s test
fixture, abridged:

```toml
version = 1

[book]
title = "The Name of the Wind"
author = "Patrick Rothfuss"
duration = "27:55:00"

[defaults]
fade_in = "8"
fade_out = "6"
crossfade = "4"
level_db = -6

[[cue]]
at = { chapter = 7, offset = "4:10" }
until = { chapter = 7, offset = "11:30" }
anchor = "the lute was warm in his hands"
why = "first time at the Eolian"
track = { artist = "Yo-Yo Ma", title = "Prelude", album = "Bach: Cello Suites" }
vibe = ["solo strings", "warm", "no vocals"]
region = { start = "0:40", end = "2:15", repeat = true, crossfade = "3" }
level_db = -9

[[cue]]
at = { chapter = 7, offset = "11:30" }
silence = true
why = "Denna speaks"

[[cue]]
at = { chapter = 8, offset = "0:00" }
vibe = ["sparse", "slow", "piano"]
fade_in = "15"
```

Times are written as people write them: `4:10`, `1:02:03.5`, or a bare
number of seconds. Chapters count from one.

Per cue:

| Field      | Meaning                                                         |
|------------|-----------------------------------------------------------------|
| `at`       | Where the cue starts. A table with `chapter` and `offset`, or a bare time when the recording has no chapters. |
| `until`    | Where it ends. Defaults to the next cue's `at`, or the end of the book. A gap between `until` and the next `at` is silence. |
| `anchor`   | A few words from the book at `at`. A tool can use a transcript to re-anchor the cue to a different recording. |
| `why`      | A note for whoever reads the map later.                         |
| `track`    | The track to play: `artist`, `title`, optional `album` and `mbid`. |
| `vibe`     | Mood tags. Used to pick a track when `track` is absent or not in the library. |
| `silence`  | Play nothing. Excludes `track` and `vibe`.                      |
| `region`   | `start`, optional `end`, `repeat`, optional `crossfade`. Absent means the whole track, once. |
| `level_db` | Offset from the listener's music level. Overrides the default.  |
| `fade_in`, `fade_out` | Override the defaults.                               |

A cue must name a track, a vibe, or silence. The parser rejects a map that
contradicts itself and says which cue. Order against a recording is
checked when the map is resolved, since it depends on the chapter times.

## How it plays

Three pieces, all in:

- **`Region`** in `crates/core`. A `Source` that plays a window of another
  source and can repeat it with a crossfade at the seam. The head of the
  window is read once when the region is opened, so the seam can blend the
  tail into it without the source being in two places at once. The render
  path allocates nothing. Done.
- **`Schedule`** in `crates/map`. Resolves every cue against a `Timeline`
  of chapter start times and answers one question each tick: did the
  current cue change? Seeks are just a bigger step; nothing is assumed
  about direction. Done.
- **`Conductor`** in `crates/library`. Resolves every cue against the
  music library once, named track first and vibe tags as the fallback,
  then turns each position update into at most one `Direction`: play this
  region of this item at this gain with this fade, or fade to silence.
  Silence is asked for once, not again for every silent stretch. `open`
  fetches and decodes the item and wraps it in a `Region` and a `Gain`;
  `apply` is one crossfade on the music layer. A shell runs `open` off
  the audio thread. Done.
- **The Android app.** Pick a music folder and a map; the bridge asks the
  conductor on every render block and a worker thread opens each track
  while the lock is free. `docs/android.md` has the details. Done. The
  CLI does not play maps until desktop playback arrives.

`underscore map check <map> --music <folder>` resolves a map against a
folder and prints how each cue landed, which is the quickest way to see
whether a map and a library agree.

Chapter times come from the book itself (M4B chapters) or from the server
(Audiobookshelf's chapter list). A map with chapter positions cannot be
resolved against a recording with no chapter data, and the shell should
say so rather than guess.

## Where the context comes from

A map needs to know what happens in the book. In order of how much work it
is:

1. **Chapter titles and the publisher's synopsis.** Audiobookshelf has
   them today. Enough for a chapter-level map.
2. **Speech to text of the audiobook the listener owns.** A private
   transcript of a file you own, for your own listening, is fine. The
   timestamps come for free, which is exactly what cues need. Where it
   runs is an open decision; see below.
3. **A DRM-free ebook aligned to the audio.** More accurate for quoted
   songs, more work for alignment, and most ebooks are locked. Rule two
   means locked ones are never touched, so this stays optional.

## Where the transcript is made: an open decision

The first draft said the transcript runs on a desktop or the server, not
the phone, because Whisper on a laptop does about ten hours of audio in
an hour and a phone cannot keep that up. Measured against the phone that
is still true: whisper.cpp with the tiny or base model runs a few times
faster than real time on a flagship and throttles long before a book is
done, and those small models mangle names, which are what a map wants.

The phone has a better engine already installed. Since Android 13 the
system speech recognizer takes audio from a file descriptor instead of
the microphone, and since Android 14 it returns word timestamps. On a
Pixel that runs on the Tensor chip many times faster than real time,
with the full-size model and no bundled weights. It is not on every
phone: reliable on Pixels and most phones with Google's speech services,
absent on some others.

That makes the transcript cheap where the listening happens, and the
language-model step is a network call from anywhere. So there are three
places the map tool could live, and the choice has to be made before
the tool is written:

- **In the app.** Transcribe on the device, call the model, write the
  map next to the book. The best experience on a Pixel. Needs a model
  key in the app, and a fallback for phones without the recognizer.
- **In the CLI, on a desktop.** Whisper or the same recognizer's desktop
  cousin, then the model. Works for everyone with a computer, needs the
  book copied over, and the map copied back.
- **On the listener's server.** Audiobookshelf already holds the book;
  a job there could transcribe once for every device. Only helps the
  self-hosting users, and that backend is not written yet.

The likely answer is the first two sharing one Rust core: the transcript
format, the segmenter prompt, and the resolver live in a crate, and the
app and the CLI each supply a transcript source and a model client.
Nothing in this choice touches the three rules: the transcript is a
private copy of a file the listener owns, the map is still only data, and
the mix is never involved.

## The sidecar tool

`underscore map` in the CLI crate, planned. It takes a book and a library
and writes a map. Two stages, so the output stays checkable:

1. The transcript, in chapter-sized chunks, goes to a language model and
   scene segments come back: start, end, mood tags, intensity, explicit
   song or artist mentions, and places that want silence.
2. A plain resolver matches those against the library's inventory. The
   model is only asked to pick among a short list of candidates for each
   cue, never to browse the whole catalog.

A hand-written map must work with no model at all. That is also how the
format is tested.
