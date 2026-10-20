//! Chapter marks in MP4 containers (M4B, M4A), which symphonia does not
//! read.
//!
//! Two conventions exist and files often carry both:
//!
//! - A Nero `chpl` atom under `moov/udta`: a list of start times in 100 ns
//!   ticks with titles. Written by ffmpeg, m4b-tool, and most converters.
//! - A `QuickTime` chapter track: the audio track's `tref/chap` names a text
//!   track whose samples are the titles and whose sample times are the
//!   starts. Written by Apple's tools and by ffmpeg.
//!
//! `chpl` is tried first because it is cheaper and self-contained. The
//! chapter track needs the sample tables and a read per title.
//!
//! The whole `moov` atom is read into memory. It is small: kilobytes for
//! most books, a few megabytes for very long ones.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use crate::probe::Chapter;

/// Largest `moov` we will read. Beyond this the file is not a book.
const MOOV_LIMIT: u64 = 64 * 1024 * 1024;
/// Largest title sample we will read.
const TITLE_LIMIT: u64 = 64 * 1024;
/// More chapters than any book has; a guard against junk tables.
const CHAPTER_LIMIT: usize = 10_000;
#[allow(clippy::cast_possible_truncation)]
const CHAPTER_LIMIT_U32: u32 = CHAPTER_LIMIT as u32;

/// Chapter marks from an MP4 file, or an empty list when the file is not
/// MP4 or has none. Leaves the reader positioned anywhere.
///
/// # Errors
/// Only I/O errors; malformed atoms yield an empty list.
pub fn chapters<R: Read + Seek + ?Sized>(r: &mut R) -> io::Result<Vec<Chapter>> {
    let Some(moov) = read_moov(r)? else {
        return Ok(Vec::new());
    };
    if let Some(found) = from_chpl(&moov) {
        return Ok(found);
    }
    from_chapter_track(r, &moov)
}

/// Walk the top-level atoms and return the body of `moov`.
fn read_moov<R: Read + Seek + ?Sized>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let end = r.seek(SeekFrom::End(0))?;
    let mut pos = r.seek(SeekFrom::Start(0))?;
    let mut first = true;
    while pos + 8 <= end {
        let mut head = [0u8; 16];
        r.read_exact(&mut head[..8])?;
        let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let kind = [head[4], head[5], head[6], head[7]];
        let mut header = 8;
        if size == 1 {
            r.read_exact(&mut head[8..16])?;
            size = u64::from_be_bytes([
                head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15],
            ]);
            header = 16;
        } else if size == 0 {
            size = end - pos;
        }
        if first {
            // Only walk files that start like MP4; anything else is not ours.
            if !matches!(
                &kind,
                b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"uuid"
            ) {
                return Ok(None);
            }
            first = false;
        }
        if size < header || pos + size > end {
            return Ok(None);
        }
        if &kind == b"moov" {
            let len = size - header;
            if len > MOOV_LIMIT {
                return Ok(None);
            }
            #[allow(clippy::cast_possible_truncation)]
            let mut body = vec![0u8; len as usize];
            r.read_exact(&mut body)?;
            return Ok(Some(body));
        }
        pos = r.seek(SeekFrom::Start(pos + size))?;
    }
    Ok(None)
}

/// One atom inside a container body.
struct Atom<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// The atoms directly inside `body`.
fn children(body: &[u8]) -> impl Iterator<Item = Atom<'_>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let head = body.get(pos..pos + 8)?;
        let mut size =
            usize::try_from(u32::from_be_bytes([head[0], head[1], head[2], head[3]])).ok()?;
        let kind = [head[4], head[5], head[6], head[7]];
        let mut header = 8;
        if size == 1 {
            let large = body.get(pos + 8..pos + 16)?;
            size = usize::try_from(u64::from_be_bytes([
                large[0], large[1], large[2], large[3], large[4], large[5], large[6], large[7],
            ]))
            .ok()?;
            header = 16;
        } else if size == 0 {
            size = body.len() - pos;
        }
        if size < header {
            return None;
        }
        let atom_body = body.get(pos + header..pos + size)?;
        pos += size;
        Some(Atom {
            kind,
            body: atom_body,
        })
    })
}

/// The first child of `body` with this kind.
fn child(body: &[u8], kind: [u8; 4]) -> Option<&[u8]> {
    children(body).find(|a| a.kind == kind).map(|a| a.body)
}

/// Follow a path of kinds down from `body`.
fn descend<'a>(body: &'a [u8], path: &[[u8; 4]]) -> Option<&'a [u8]> {
    path.iter().try_fold(body, |b, k| child(b, *k))
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be_u64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// Nero chapters: `moov/udta/chpl`.
fn from_chpl(moov: &[u8]) -> Option<Vec<Chapter>> {
    let chpl = descend(moov, &[*b"udta", *b"chpl"])?;
    let version = *chpl.first()?;
    let mut at = 4;
    if version != 0 {
        at += 4;
    }
    let count = usize::from(*chpl.get(at)?);
    at += 1;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let ticks = be_u64(chpl, at)?;
        let len = usize::from(*chpl.get(at + 8)?);
        let title = chpl.get(at + 9..at + 9 + len)?;
        at += 9 + len;
        out.push(Chapter {
            title: text(title),
            // 100 ns ticks.
            start: Duration::from_nanos(ticks.saturating_mul(100)),
        });
    }
    out.sort_by_key(|c| c.start);
    (!out.is_empty()).then_some(out)
}

/// A title, trimmed, or `None` when it is blank.
fn text(bytes: &[u8]) -> Option<String> {
    let s = match bytes {
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!s.is_empty()).then(|| s.to_string())
}

fn utf16(bytes: &[u8], from: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| from(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// `QuickTime` chapters: the text track the audio track refers to.
fn from_chapter_track<R: Read + Seek + ?Sized>(r: &mut R, moov: &[u8]) -> io::Result<Vec<Chapter>> {
    let traks: Vec<&[u8]> = children(moov)
        .filter(|a| &a.kind == b"trak")
        .map(|a| a.body)
        .collect();

    // Which track holds the chapters: whatever a `chap` reference names,
    // else any text track.
    let referenced: Vec<u32> = traks
        .iter()
        .filter_map(|t| descend(t, &[*b"tref", *b"chap"]))
        .flat_map(|chap| {
            chap.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_be_bytes(*c))
        })
        .collect();
    let text_track = traks
        .iter()
        .find(|t| track_id(t).is_some_and(|id| referenced.contains(&id)))
        .or_else(|| traks.iter().find(|t| handler(t) == Some(*b"text")))
        .copied();
    let Some(trak) = text_track else {
        return Ok(Vec::new());
    };
    let Some(samples) = samples(trak) else {
        return Ok(Vec::new());
    };
    let Some(timescale) = timescale(trak).filter(|t| *t > 0) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(samples.len());
    for s in samples {
        if s.size > TITLE_LIMIT {
            continue;
        }
        r.seek(SeekFrom::Start(s.offset))?;
        #[allow(clippy::cast_possible_truncation)]
        let mut buf = vec![0u8; s.size as usize];
        if r.read_exact(&mut buf).is_err() {
            continue;
        }
        // A QuickTime text sample: big-endian length, then the text.
        let title = be_u16(&buf, 0)
            .and_then(|len| buf.get(2..2 + usize::from(len)))
            .and_then(text);
        #[allow(clippy::cast_precision_loss)]
        let start = Duration::from_secs_f64(s.start as f64 / f64::from(timescale));
        out.push(Chapter { title, start });
    }
    out.sort_by_key(|c| c.start);
    Ok(out)
}

fn track_id(trak: &[u8]) -> Option<u32> {
    let tkhd = child(trak, *b"tkhd")?;
    let at = if *tkhd.first()? == 0 { 12 } else { 20 };
    be_u32(tkhd, at)
}

fn handler(trak: &[u8]) -> Option<[u8; 4]> {
    let hdlr = descend(trak, &[*b"mdia", *b"hdlr"])?;
    hdlr.get(8..12).map(|s| [s[0], s[1], s[2], s[3]])
}

fn timescale(trak: &[u8]) -> Option<u32> {
    let mdhd = descend(trak, &[*b"mdia", *b"mdhd"])?;
    let at = if *mdhd.first()? == 0 { 12 } else { 20 };
    be_u32(mdhd, at)
}

struct Sample {
    offset: u64,
    size: u64,
    /// In the track's timescale.
    start: u64,
}

/// Every sample of a track, from its sample tables.
fn samples(trak: &[u8]) -> Option<Vec<Sample>> {
    let stbl = descend(trak, &[*b"mdia", *b"minf", *b"stbl"])?;

    // Durations: (count, delta) runs.
    let stts = child(stbl, *b"stts")?;
    let runs = usize::try_from(be_u32(stts, 4)?).ok()?;
    let mut starts = Vec::new();
    let mut clock = 0u64;
    for i in 0..runs.min(CHAPTER_LIMIT) {
        let count = be_u32(stts, 8 + i * 8)?;
        let delta = be_u32(stts, 12 + i * 8)?;
        for _ in 0..count.min(CHAPTER_LIMIT_U32) {
            starts.push(clock);
            clock = clock.saturating_add(u64::from(delta));
        }
    }

    // Sizes: one for all, or a table.
    let size_table = child(stbl, *b"stsz")?;
    let fixed = be_u32(size_table, 4)?;
    let count = usize::try_from(be_u32(size_table, 8)?)
        .ok()?
        .min(CHAPTER_LIMIT);
    let sizes: Vec<u64> = if fixed != 0 {
        vec![u64::from(fixed); count]
    } else {
        (0..count)
            .map(|i| be_u32(size_table, 12 + i * 4).map(u64::from))
            .collect::<Option<_>>()?
    };

    // Chunk offsets, 32 or 64 bit.
    let chunks: Vec<u64> = if let Some(stco) = child(stbl, *b"stco") {
        let n = usize::try_from(be_u32(stco, 4)?).ok()?.min(CHAPTER_LIMIT);
        (0..n)
            .map(|i| be_u32(stco, 8 + i * 4).map(u64::from))
            .collect::<Option<_>>()?
    } else {
        let co64 = child(stbl, *b"co64")?;
        let n = usize::try_from(be_u32(co64, 4)?).ok()?.min(CHAPTER_LIMIT);
        (0..n)
            .map(|i| be_u64(co64, 8 + i * 8))
            .collect::<Option<_>>()?
    };

    // Samples per chunk: (first_chunk, samples_per_chunk) runs, 1-based.
    let chunk_map = child(stbl, *b"stsc")?;
    let n = usize::try_from(be_u32(chunk_map, 4)?)
        .ok()?
        .min(CHAPTER_LIMIT);
    let mut layout = Vec::with_capacity(n);
    for i in 0..n {
        let first = be_u32(chunk_map, 8 + i * 12)?;
        let per = be_u32(chunk_map, 12 + i * 12)?;
        layout.push((first, per));
    }

    let mut out = Vec::with_capacity(count);
    let mut sample = 0usize;
    for (ci, chunk) in chunks.iter().enumerate() {
        let chunk_no = u32::try_from(ci + 1).ok()?;
        let per = layout
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_no)
            .map_or(0, |(_, per)| *per);
        let mut offset = *chunk;
        for _ in 0..per {
            if sample >= count {
                break;
            }
            out.push(Sample {
                offset,
                size: sizes[sample],
                start: starts.get(sample).copied().unwrap_or(clock),
            });
            offset = offset.saturating_add(sizes[sample]);
            sample += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn atom(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + body.len());
        #[allow(clippy::cast_possible_truncation)]
        v.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        v.extend_from_slice(&kind);
        v.extend_from_slice(body);
        v
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn chpl_body(entries: &[(u64, &str)]) -> Vec<u8> {
        let mut b = vec![1, 0, 0, 0, 0, 0, 0, 0];
        #[allow(clippy::cast_possible_truncation)]
        b.push(entries.len() as u8);
        for (ticks, title) in entries {
            b.extend_from_slice(&ticks.to_be_bytes());
            #[allow(clippy::cast_possible_truncation)]
            b.push(title.len() as u8);
            b.extend_from_slice(title.as_bytes());
        }
        b
    }

    fn ftyp() -> Vec<u8> {
        atom(*b"ftyp", b"M4B \0\0\0\0M4B mp42isom")
    }

    #[test]
    fn reads_nero_chapters() {
        let moov = atom(
            *b"moov",
            &atom(
                *b"udta",
                &atom(
                    *b"chpl",
                    &chpl_body(&[(0, "One"), (30_000_000, "Two"), (75_000_000, "Three")]),
                ),
            ),
        );
        let file = cat(&[ftyp(), atom(*b"mdat", &[0; 100]), moov]);
        let got = chapters(&mut Cursor::new(file)).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].title.as_deref(), Some("Two"));
        assert_eq!(got[1].start, Duration::from_secs(3));
        assert_eq!(got[2].start, Duration::from_millis(7500));
    }

    /// A chapter text track with `n` titles, each one second apart,
    /// stored in an `mdat` that comes first in the file.
    fn chapter_track_file(titles: &[&[u8]], co64: bool, with_tref: bool) -> Vec<u8> {
        let head = ftyp();
        let mut mdat_body = Vec::new();
        let mut sizes = Vec::new();
        for t in titles {
            #[allow(clippy::cast_possible_truncation)]
            let sample = cat(&[(t.len() as u16).to_be_bytes().to_vec(), t.to_vec()]);
            sizes.push(sample.len());
            mdat_body.extend_from_slice(&sample);
        }
        let mdat_start = head.len() as u64 + 8;

        let tkhd = |id: u32| {
            let mut b = vec![0u8; 84];
            b[12..16].copy_from_slice(&id.to_be_bytes());
            atom(*b"tkhd", &b)
        };
        let mdhd = |scale: u32| {
            let mut b = vec![0u8; 24];
            b[12..16].copy_from_slice(&scale.to_be_bytes());
            atom(*b"mdhd", &b)
        };
        let hdlr = |kind: &[u8; 4]| atom(*b"hdlr", &cat(&[vec![0; 8], kind.to_vec(), vec![0; 13]]));

        let mut stts = vec![0, 0, 0, 0];
        stts.extend_from_slice(&1u32.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        stts.extend_from_slice(&(titles.len() as u32).to_be_bytes());
        stts.extend_from_slice(&1000u32.to_be_bytes());

        let mut size_table = vec![0, 0, 0, 0, 0, 0, 0, 0];
        #[allow(clippy::cast_possible_truncation)]
        size_table.extend_from_slice(&(titles.len() as u32).to_be_bytes());
        for s in &sizes {
            #[allow(clippy::cast_possible_truncation)]
            size_table.extend_from_slice(&(*s as u32).to_be_bytes());
        }

        // One chunk holding every sample.
        let mut chunk_map = vec![0, 0, 0, 0];
        chunk_map.extend_from_slice(&1u32.to_be_bytes());
        chunk_map.extend_from_slice(&1u32.to_be_bytes());
        #[allow(clippy::cast_possible_truncation)]
        chunk_map.extend_from_slice(&(titles.len() as u32).to_be_bytes());
        chunk_map.extend_from_slice(&1u32.to_be_bytes());

        let offsets = if co64 {
            let mut b = vec![0, 0, 0, 0];
            b.extend_from_slice(&1u32.to_be_bytes());
            b.extend_from_slice(&mdat_start.to_be_bytes());
            atom(*b"co64", &b)
        } else {
            let mut b = vec![0, 0, 0, 0];
            b.extend_from_slice(&1u32.to_be_bytes());
            #[allow(clippy::cast_possible_truncation)]
            b.extend_from_slice(&(mdat_start as u32).to_be_bytes());
            atom(*b"stco", &b)
        };

        let stbl = atom(
            *b"stbl",
            &cat(&[
                atom(*b"stts", &stts),
                atom(*b"stsz", &size_table),
                atom(*b"stsc", &chunk_map),
                offsets,
            ]),
        );
        let text_trak = atom(
            *b"trak",
            &cat(&[
                tkhd(2),
                atom(
                    *b"mdia",
                    &cat(&[mdhd(1000), hdlr(b"text"), atom(*b"minf", &stbl)]),
                ),
            ]),
        );
        let mut audio_parts = vec![
            tkhd(1),
            atom(*b"mdia", &cat(&[mdhd(44_100), hdlr(b"soun")])),
        ];
        if with_tref {
            audio_parts.push(atom(*b"tref", &atom(*b"chap", &2u32.to_be_bytes())));
        }
        let audio_trak = atom(*b"trak", &cat(&audio_parts));
        let moov = atom(*b"moov", &cat(&[audio_trak, text_trak]));
        cat(&[head, atom(*b"mdat", &mdat_body), moov])
    }

    #[test]
    fn reads_quicktime_chapter_track() {
        let file = chapter_track_file(&[b"Intro", b"Part One", b"The End"], false, true);
        let got = chapters(&mut Cursor::new(file)).unwrap();
        let titles: Vec<_> = got.iter().map(|c| c.title.clone().unwrap()).collect();
        assert_eq!(titles, ["Intro", "Part One", "The End"]);
        assert_eq!(got[2].start, Duration::from_secs(2));
    }

    #[test]
    fn chapter_track_with_64_bit_offsets_and_no_reference() {
        let file = chapter_track_file(&[b"A", b"B"], true, false);
        let got = chapters(&mut Cursor::new(file)).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].title.as_deref(), Some("B"));
    }

    #[test]
    fn decodes_utf16_titles_and_drops_blank_ones() {
        let utf16: Vec<u8> = cat(&[
            vec![0xFE, 0xFF],
            "Zwei".encode_utf16().flat_map(u16::to_be_bytes).collect(),
        ]);
        let file = chapter_track_file(&[b"Eins", &utf16, b"  "], false, true);
        let got = chapters(&mut Cursor::new(file)).unwrap();
        assert_eq!(got[1].title.as_deref(), Some("Zwei"));
        assert_eq!(got[2].title, None);
    }

    #[test]
    fn nero_wins_when_both_are_present() {
        let mut file = chapter_track_file(&[b"from track"], false, true);
        // Append a second moov? No: real files have one. Put chpl inside.
        let udta = atom(*b"udta", &atom(*b"chpl", &chpl_body(&[(0, "from chpl")])));
        // Rebuild: the moov is the last atom; graft udta into it.
        let moov_pos = {
            let mut pos = 0;
            let mut found = 0;
            while pos + 8 <= file.len() {
                let size =
                    u32::from_be_bytes([file[pos], file[pos + 1], file[pos + 2], file[pos + 3]])
                        as usize;
                if &file[pos + 4..pos + 8] == b"moov" {
                    found = pos;
                }
                pos += size;
            }
            found
        };
        let moov_body = file.split_off(moov_pos + 8);
        file.truncate(moov_pos);
        file.extend_from_slice(&atom(*b"moov", &cat(&[moov_body, udta])));
        let got = chapters(&mut Cursor::new(file)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title.as_deref(), Some("from chpl"));
    }

    #[test]
    fn non_mp4_and_chapterless_mp4_give_nothing() {
        assert!(
            chapters(&mut Cursor::new(b"RIFF....WAVEfmt ".to_vec()))
                .unwrap()
                .is_empty()
        );
        let file = cat(&[ftyp(), atom(*b"moov", &atom(*b"udta", b""))]);
        assert!(chapters(&mut Cursor::new(file)).unwrap().is_empty());
        assert!(chapters(&mut Cursor::new(Vec::new())).unwrap().is_empty());
    }
}
