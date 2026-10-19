# Why the rules are the rules

This records the legal reasoning behind the three rules in the README, so
the reasoning travels with the code. It is an engineering team's working
analysis of United States law and the published terms of the services
involved, as of October 2026. It is not legal advice.

## The core argument holds

A listener playing two lawfully obtained audio sources at the same time,
privately, with nothing recorded, triggers none of the exclusive rights in
17 U.S.C. § 106. There is no public performance, no reproduction, and no
fixed derivative work. The closest precedent is *Lewis Galoob Toys v.
Nintendo* (9th Cir. 1992), where altering a game's output in real time with
the Game Genie created no derivative work because nothing was fixed.
Real-time ducking and layering of audio is the same act. Synchronization
licenses govern fixed audiovisual works, which this is not.

Because the listener is not infringing, there is no direct infringement
for the software's author to be secondarily liable for. A player with
substantial non-infringing uses is further protected under *Sony v.
Universal* (1984).

That argument is only as strong as the absence of a fixed copy. Hence rule
one: **the mix exists only in memory.** The moment the combined signal is
written to disk, cached, or exported, it becomes a reproduction and a
derivative work, and the argument is gone.

## Streaming services forbid this by contract

Copyright is not the obstacle with Spotify, Apple Music, or YouTube Music.
Their developer agreements are.

- **Spotify.** The Developer Policy's prohibited-applications list says not
  to "permit any device or system to segue, mix, re-mix, or overlap any
  Spotify Content with any other audio content." Spotify also cut off every
  third-party DJ application in July 2020 and has tightened API access since
  2024.
- **Apple Music.** Real-time mixing of Apple Music streams is available to
  approved partners (djay, Serato, rekordbox) and not through public
  MusicKit, which exposes a single managed player.
- **YouTube Music.** The API terms prohibit background and audio-only
  playback.

The listener's own terms of service are not the issue; nothing in them
forbids having a podcast on at the same time. The restriction attaches to
the application that integrates. Hence rule three: **no streaming-service
SDKs.** Reading a service's encrypted offline cache from disk instead is
circumvention under 17 U.S.C. § 1201 and is not an option either.

## DRM is the problem on the book side

Podcasts are open RSS and plain files. Audible, Libby, and OverDrive
audiobooks are DRM-protected with no public playback API. Circumventing
that protection is a § 1201(a)(1) violation even for personal use; there is
no fair-use defense to circumvention and no Copyright Office exemption
covers format-shifting audiobooks.

A listener doing that at home carries little practical risk. Software that
does it for them is in a different position: distributing or embedding a
circumvention tool is § 1201(a)(2) trafficking, which is what actually gets
litigated (*Universal v. Corley*, 2d Cir. 2001, which also upheld an
injunction against *linking* to such a tool). Hence rule two: **no DRM,
anywhere.** underscore accepts plain M4B and MP3 files and says nothing
about where they came from. This is exactly how Audiobookshelf, BookPlayer,
Prologue, and Plappa operate. Documentation points at DRM-free sellers such
as Libro.fm and Downpour.

## DRM-free music is not a legal problem

It is a market one. Legal sources of DRM-free music include the iTunes
Store (DRM-free since 2009), Bandcamp, Qobuz, 7digital, HDtracks, ripped
CDs (no effective access control, so no § 1201 issue), and iTunes Match
uploads. Most listeners under forty own none of it, which is why the
self-hosted mode matters: the people who run Audiobookshelf also tend to run
Navidrome or Jellyfin, and they already have DRM-free files on both sides.

## Sources

- Spotify Developer Policy: https://developer.spotify.com/policy/
- Spotify compliance tips: https://developer.spotify.com/compliance-tips
- Spotify forum, staff reading of the no-mixing rule:
  https://community.spotify.com/t5/Spotify-for-Developers/Question-regarding-no-mixing-policy/td-p/5282821
- Apple developer forum on real-time DJ mixing beyond MusicKit:
  https://developer.apple.com/forums/thread/843627
- Algoriddim on DRM-protected songs: https://help.algoriddim.com/topic/music-streaming/drm-protected-songs
- Spotify dropping third-party DJ apps:
  https://musictech.com/news/gear/spotify-stopping-3rd-party-dj-app-support/
