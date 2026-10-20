//! Read what a file says about itself without decoding it: length, tags,
//! and chapter marks. A library scan does this to every file once.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::Hint;

use crate::{DecodeError, FromU64, track_duration};

/// The tags a library cares about. Everything is best effort: formats
/// disagree on names, and files are tagged by people.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tags {
    /// Track or book title.
    pub title: Option<String>,
    /// Artist, or author for a book.
    pub artist: Option<String>,
    /// Album, or series.
    pub album: Option<String>,
    /// Album artist, when it differs.
    pub album_artist: Option<String>,
    /// Genre.
    pub genre: Option<String>,
    /// Mood, where a tagger set one.
    pub mood: Option<String>,
    /// The recording ID at musicbrainz.org.
    pub mbid: Option<String>,
    /// Track number within the album.
    pub track_number: Option<u32>,
}

/// A chapter mark the container carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// Title, when the mark has one.
    pub title: Option<String>,
    /// Where it starts.
    pub start: Duration,
}

/// What a file says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    /// Length, when the container says.
    pub duration: Option<Duration>,
    /// Tags, merged from every layer of metadata in the file.
    pub tags: Tags,
    /// Chapter marks, in order. Empty when the format has none or the
    /// reader does not parse them.
    pub chapters: Vec<Chapter>,
}

/// Probe a file by path.
///
/// # Errors
/// If the file cannot be read or is not a supported audio file.
pub fn probe_path(path: impl AsRef<Path>) -> Result<Probe, DecodeError> {
    let path = path.as_ref();
    let ext = path.extension().and_then(|e| e.to_str());
    let file = File::open(path)?;
    probe(Box::new(file), ext)
}

/// Probe any seekable byte source. `hint` is a file extension.
///
/// # Errors
/// If the bytes are not a supported audio file.
pub fn probe(mut source: Box<dyn MediaSource>, hint: Option<&str>) -> Result<Probe, DecodeError> {
    // MP4 chapter atoms first, on the raw bytes: symphonia does not read
    // them. Harmless on other formats.
    let mut mp4_chapters = Vec::new();
    if source.is_seekable() {
        mp4_chapters = crate::mp4::chapters(&mut *source)?;
        source.seek(SeekFrom::Start(0))?;
    }
    let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
    let mut probe_hint = Hint::new();
    if let Some(ext) = hint {
        probe_hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe().format(
        &probe_hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut tags = Tags::default();
    if let Some(rev) = probed
        .metadata
        .get()
        .as_ref()
        .and_then(|m| m.current().cloned())
    {
        apply_tags(&mut tags, rev.tags());
    }
    let mut reader = probed.format;
    if let Some(rev) = reader.metadata().skip_to_latest() {
        apply_tags(&mut tags, rev.tags());
    }

    let track = reader
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(DecodeError::NoAudioTrack)?;
    let params = &track.codec_params;
    let duration = track_duration(params);
    let time_base = params.time_base;
    let rate = params.sample_rate;

    let mut chapters: Vec<Chapter> = reader
        .cues()
        .iter()
        .map(|cue| {
            let start = match (time_base, rate) {
                (Some(tb), _) => {
                    let t = tb.calc_time(cue.start_ts);
                    Duration::from_secs_f64(f64::from_u64(t.seconds) + t.frac)
                }
                (None, Some(rate)) => {
                    Duration::from_secs_f64(f64::from_u64(cue.start_ts) / f64::from(rate))
                }
                (None, None) => Duration::ZERO,
            };
            let title = cue
                .tags
                .iter()
                .find(|t| {
                    t.std_key == Some(StandardTagKey::TrackTitle)
                        || t.key.eq_ignore_ascii_case("title")
                })
                .map(|t| t.value.to_string());
            Chapter { title, start }
        })
        .collect();
    chapters.sort_by_key(|c| c.start);
    if chapters.is_empty() {
        chapters = mp4_chapters;
    }

    Ok(Probe {
        duration,
        tags,
        chapters,
    })
}

/// Fill in `tags` from `found`, later layers overriding earlier ones.
fn apply_tags(tags: &mut Tags, found: &[Tag]) {
    for tag in found {
        let value = tag.value.to_string();
        // RIFF INFO strings keep their NUL terminator.
        let value = value.trim().trim_matches('\0').trim();
        if value.is_empty() {
            continue;
        }
        let slot = match tag.std_key {
            Some(StandardTagKey::TrackTitle) => &mut tags.title,
            Some(StandardTagKey::Artist) => &mut tags.artist,
            Some(StandardTagKey::Album) => &mut tags.album,
            Some(StandardTagKey::AlbumArtist) => &mut tags.album_artist,
            Some(StandardTagKey::Genre) => &mut tags.genre,
            Some(StandardTagKey::Mood) => &mut tags.mood,
            Some(StandardTagKey::MusicBrainzRecordingId) => &mut tags.mbid,
            Some(StandardTagKey::TrackNumber) => {
                let number = value.split('/').next().unwrap_or(value).trim();
                if let Ok(n) = number.parse() {
                    tags.track_number = Some(n);
                }
                continue;
            }
            _ => continue,
        };
        *slot = Some(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV with a one-second mono tone and a RIFF INFO list of tags.
    fn tagged_wav(rate: u32, tags: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let frames = rate as usize;
        let mut data = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            #[allow(clippy::cast_precision_loss)]
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin();
            #[allow(clippy::cast_possible_truncation)]
            data.extend_from_slice(&((v * 32000.0) as i16).to_le_bytes());
        }
        let mut info = b"INFO".to_vec();
        for (key, value) in tags {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            if bytes.len() % 2 == 1 {
                bytes.push(0);
            }
            info.extend_from_slice(*key);
            #[allow(clippy::cast_possible_truncation)]
            info.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            info.extend_from_slice(&bytes);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&((4 + 24 + 8 + info.len() + 8 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"LIST");
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(info.len() as u32).to_le_bytes());
        out.extend_from_slice(&info);
        out.extend_from_slice(b"data");
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_duration_and_riff_tags() {
        let bytes = tagged_wav(
            8_000,
            &[
                (b"INAM", "Prelude"),
                (b"IART", "Yo-Yo Ma"),
                (b"IPRD", "Cello Suites"),
                (b"IGNR", "Classical"),
            ],
        );
        let p = probe(Box::new(std::io::Cursor::new(bytes)), Some("wav")).unwrap();
        assert_eq!(p.duration, Some(Duration::from_secs(1)));
        assert_eq!(p.tags.title.as_deref(), Some("Prelude"));
        assert_eq!(p.tags.artist.as_deref(), Some("Yo-Yo Ma"));
        assert_eq!(p.tags.album.as_deref(), Some("Cello Suites"));
        assert_eq!(p.tags.genre.as_deref(), Some("Classical"));
        assert!(p.chapters.is_empty());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(probe(Box::new(std::io::Cursor::new(vec![0u8; 64])), None).is_err());
    }
}
