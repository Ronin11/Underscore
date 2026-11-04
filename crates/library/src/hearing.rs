//! Song scores made on this device: where a song sits on the scene axes,
//! worked out by listening to three short stretches of it.
//!
//! A few plain measurements of the sound go through a small linear model
//! (`song_model.rs`, written by `tools/song-model/train.py`), fitted to what
//! CLAP says on a GPU. It is rougher than CLAP and needs no server, so a map
//! that asks for music by energy and valence works for any music the phone
//! can read. It cannot tell singing from instruments yet, so `voice` stays 0.
//!
//! The samples live on the stack of [`hear`] and are gone when it returns.
//! Only the numbers are kept, in a [`SongScores`] file per library.

// Frame, bin, and sample counts here are tiny next to f64's 52 bits.
#![allow(clippy::cast_precision_loss)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use underscore_core::Source;
use underscore_decode::FileSource;

use crate::song_model as model;
use crate::{Axes, Error};

/// The sample rate the measurements are made at.
const RATE: usize = 16_000;
/// The rate songs are decoded at, three times [`RATE`] so a filter can
/// remove what would otherwise fold down into the measured range.
const DECODE_RATE: u32 = 48_000;
const WINDOW_SECS: f64 = 10.0;
const NFFT: usize = 1024;
const HOP: usize = 160;
const BINS: usize = NFFT / 2 + 1;
const MELS: usize = 32;
/// Measurements from one stretch: band means, band spreads, and the rest.
const PER_WINDOW: usize = MELS * 2 + 17;
/// Measurements for one song: their mean and spread over its stretches.
pub const FEATURES: usize = PER_WINDOW * 2;

/// Krumhansl-Kessler key profiles.
const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Listen to the song at `path`: the mean and spread of the measurements
/// from stretches a quarter, half, and three quarters of the way through.
///
/// # Errors
/// The file cannot be decoded or holds under a second of sound.
pub fn hear(path: &Path) -> Result<Vec<f32>, Error> {
    let mut source = FileSource::open_path(path, DECODE_RATE, 1)?;
    let total = source.duration().map(|d| d.as_secs_f64());
    let spectra = Spectra::new();
    let mut rows = Vec::new();
    for at in [0.25, 0.5, 0.75] {
        let start = total.map_or(0.0, |t| (t * at).min(t - WINDOW_SECS).max(0.0));
        if !source.seek(Duration::from_secs_f64(start)) && !rows.is_empty() {
            break;
        }
        let pcm = decimate(&read_window(&mut source));
        if let Some(row) = spectra.measure(&pcm) {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return Err(Error::Backend(format!(
            "{}: too little sound to score",
            path.display()
        )));
    }
    Ok(summarize(&rows))
}

/// Where measurements from [`hear`] put a song.
#[must_use]
pub fn score(features: &[f32]) -> Axes {
    let dot = |weights: &[f32]| -> f32 {
        features
            .iter()
            .zip(model::MEAN.iter().zip(model::SPREAD.iter()))
            .zip(weights)
            .map(|((x, (m, s)), w)| {
                let z = (x - m) / s;
                // A broken measurement counts as average, not as a verdict.
                if z.is_finite() { z * w } else { 0.0 }
            })
            .sum()
    };
    Axes {
        energy: (model::ENERGY_MEAN + dot(&model::ENERGY)).clamp(1.0, 5.0),
        valence: (model::VALENCE_MEAN + dot(&model::VALENCE)).clamp(-1.0, 1.0),
        voice: 0.0,
    }
}

/// Listen to a song and say where it sits.
///
/// # Errors
/// As for [`hear`].
pub fn score_file(path: &Path) -> Result<Axes, Error> {
    Ok(score(&hear(path)?))
}

fn read_window(source: &mut FileSource) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let want = (WINDOW_SECS * f64::from(DECODE_RATE)) as usize;
    let mut pcm = vec![0.0; want];
    let mut got = 0;
    while got < want {
        let n = source.read(&mut pcm[got..]);
        if n == 0 {
            break;
        }
        got += n;
    }
    pcm.truncate(got);
    pcm
}

/// From [`DECODE_RATE`] down to [`RATE`], through a windowed sinc low-pass.
fn decimate(pcm: &[f32]) -> Vec<f32> {
    const TAPS: usize = 47;
    const FACTOR: usize = 3;
    let centre = (TAPS - 1) as f64 / 2.0;
    let filter: Vec<f32> = (0..TAPS)
        .map(|i| {
            let t = i as f64 - centre;
            let sinc = if t == 0.0 {
                1.0
            } else {
                let x = std::f64::consts::PI * t / FACTOR as f64;
                x.sin() / x
            };
            let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (TAPS - 1) as f64).cos();
            #[allow(clippy::cast_possible_truncation)]
            let tap = (sinc * hann / FACTOR as f64) as f32;
            tap
        })
        .collect();
    if pcm.len() < TAPS {
        return Vec::new();
    }
    pcm.windows(TAPS)
        .step_by(FACTOR)
        .map(|w| w.iter().zip(&filter).map(|(a, b)| a * b).sum())
        .collect()
}

/// The mean and spread of each measurement over a song's stretches.
fn summarize(rows: &[Vec<f64>]) -> Vec<f32> {
    let n = rows.len() as f64;
    let mut out = vec![0.0; FEATURES];
    for i in 0..PER_WINDOW {
        let mean = rows.iter().map(|r| r[i]).sum::<f64>() / n;
        let var = rows.iter().map(|r| (r[i] - mean).powi(2)).sum::<f64>() / n;
        #[allow(clippy::cast_possible_truncation)]
        {
            out[i] = mean as f32;
            out[PER_WINDOW + i] = var.sqrt() as f32;
        }
    }
    out
}

/// What stays the same from frame to frame.
struct Spectra {
    window: Vec<f64>,
    freqs: Vec<f64>,
    /// For each mel band, the first bin and its weights.
    mel: Vec<(usize, Vec<f64>)>,
    /// The pitch class of each bin at or above 60 Hz.
    pitch: Vec<Option<usize>>,
    twiddle: Vec<(f64, f64)>,
}

impl Spectra {
    fn new() -> Self {
        use std::f64::consts::TAU;
        let window = (0..NFFT)
            .map(|i| 0.5 - 0.5 * (TAU * i as f64 / (NFFT - 1) as f64).cos())
            .collect();
        let freqs: Vec<f64> = (0..BINS)
            .map(|k| k as f64 * RATE as f64 / NFFT as f64)
            .collect();
        let hz2mel = |f: f64| 2595.0 * (1.0 + f / 700.0).log10();
        let mel2hz = |m: f64| 700.0 * (10f64.powf(m / 2595.0) - 1.0);
        let (lo, hi) = (hz2mel(60.0), hz2mel(7600.0));
        let points: Vec<f64> = (0..MELS + 2)
            .map(|i| mel2hz(lo + (hi - lo) * i as f64 / (MELS + 1) as f64))
            .collect();
        let mel = (0..MELS)
            .map(|b| {
                let (a, c, d) = (points[b], points[b + 1], points[b + 2]);
                let weight = |f: f64| ((f - a) / (c - a)).min((d - f) / (d - c)).max(0.0);
                let first = freqs.iter().position(|&f| weight(f) > 0.0).unwrap_or(0);
                let weights: Vec<f64> = freqs[first..]
                    .iter()
                    .map(|&f| weight(f))
                    .take_while(|&w| w > 0.0)
                    .collect();
                (first, weights)
            })
            .collect();
        let pitch = freqs
            .iter()
            .map(|&f| {
                (f >= 60.0).then(|| {
                    #[allow(clippy::cast_possible_truncation)]
                    let note = (69.0 + 12.0 * (f / 440.0).log2()).round() as i64;
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let class = note.rem_euclid(12) as usize;
                    class
                })
            })
            .collect();
        let twiddle = (0..NFFT / 2)
            .map(|k| {
                let a = -TAU * k as f64 / NFFT as f64;
                (a.cos(), a.sin())
            })
            .collect();
        Self {
            window,
            freqs,
            mel,
            pitch,
            twiddle,
        }
    }

    /// Radix-2 FFT in place. `re` and `im` are [`NFFT`] long.
    fn fft(&self, re: &mut [f64], im: &mut [f64]) {
        let bits = NFFT.trailing_zeros();
        for i in 0..NFFT {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= NFFT {
            let step = NFFT / size;
            for start in (0..NFFT).step_by(size) {
                for k in 0..size / 2 {
                    let (wr, wi) = self.twiddle[k * step];
                    let (a, b) = (start + k, start + k + size / 2);
                    let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }

    /// The measurements of one stretch at [`RATE`], or `None` under a second.
    #[allow(clippy::too_many_lines)]
    fn measure(&self, x: &[f32]) -> Option<Vec<f64>> {
        if x.len() < RATE {
            return None;
        }
        let frames = (x.len() - NFFT) / HOP;
        let (mut re, mut im) = (vec![0.0; NFFT], vec![0.0; NFFT]);
        let mut previous = vec![0.0; BINS];
        let mut rms_db = Vec::with_capacity(frames);
        let mut flux = Vec::with_capacity(frames);
        let mut sum = Totals::default();
        let mut chroma = [0.0; 12];
        let mut mel_sum = [0.0; MELS];
        let mut mel_sq = [0.0; MELS];
        let mut spec = vec![0.0; BINS];
        for t in 0..frames {
            let frame = &x[t * HOP..t * HOP + NFFT];
            for (i, sample) in frame.iter().enumerate() {
                re[i] = f64::from(*sample) * self.window[i];
                im[i] = 0.0;
            }
            // Signs change where the windowed frame crosses zero.
            let crossings = re.windows(2).filter(|w| sign(w[0]) != sign(w[1])).count();
            sum.zcr += crossings as f64 / (NFFT - 1) as f64;
            self.fft(&mut re, &mut im);
            let (mut power, mut weighted, mut logs, mut mags) = (0.0, 0.0, 0.0, 0.0);
            let (mut low, mut high) = (0.0, 0.0);
            for k in 0..BINS {
                let p = re[k] * re[k] + im[k] * im[k];
                let m = p.sqrt();
                spec[k] = m;
                power += p;
                weighted += p * self.freqs[k];
                logs += (m + 1e-9).ln();
                mags += m;
                if self.freqs[k] < 250.0 {
                    low += p;
                } else if self.freqs[k] > 4000.0 {
                    high += p;
                }
                if let Some(class) = self.pitch[k] {
                    chroma[class] += p;
                }
            }
            let tot = power + 1e-12;
            rms_db.push(20.0 * ((power / NFFT as f64 + 1e-12).sqrt() + 1e-9).log10());
            sum.centroid += weighted / tot;
            let mut running = 0.0;
            let mut bin = 0;
            for (k, m) in spec.iter().enumerate() {
                running += m * m;
                if running / tot >= 0.85 {
                    bin = k;
                    break;
                }
            }
            sum.rolloff += self.freqs[bin];
            sum.flatness += (logs / BINS as f64).exp() / (mags / BINS as f64 + 1e-9);
            sum.low += low / tot;
            sum.high += high / tot;
            for (b, (first, weights)) in self.mel.iter().enumerate() {
                let band: f64 = weights
                    .iter()
                    .zip(&spec[*first..])
                    .map(|(w, m)| w * m * m)
                    .sum();
                let l = (band + 1e-6).ln();
                mel_sum[b] += l;
                mel_sq[b] += l * l;
            }
            let mut rise = 0.0;
            for k in 0..BINS {
                let l = spec[k].ln_1p();
                if t > 0 {
                    rise += (l - previous[k]).max(0.0);
                }
                previous[k] = l;
            }
            if t > 0 {
                flux.push(rise);
            }
        }
        let n = frames as f64;
        let mel_mean: Vec<f64> = mel_sum.iter().map(|s| s / n).collect();
        let level = mel_mean.iter().sum::<f64>() / MELS as f64;
        let mut row: Vec<f64> = mel_mean.iter().map(|m| m - level).collect();
        for b in 0..MELS {
            row.push((mel_sq[b] / n - mel_mean[b] * mel_mean[b]).max(0.0).sqrt());
        }
        let (rms_mean, rms_sd) = mean_sd(&rms_db);
        let centroid = sum.centroid / n;
        let onsets = Onsets::of(&flux);
        let (major, minor) = key_fit(&chroma);
        row.extend([
            rms_mean,
            rms_sd,
            percentile(&rms_db, 95.0) - percentile(&rms_db, 20.0),
            centroid,
            (centroid + 1.0).ln(),
            sum.rolloff / n,
            sum.flatness / n,
            sum.low / n,
            sum.high / n,
            sum.zcr / n,
            onsets.flux,
            onsets.rate,
            onsets.bpm,
            onsets.tempo_strength,
            major,
            minor,
            major - minor,
        ]);
        debug_assert_eq!(row.len(), PER_WINDOW);
        Some(row)
    }
}

#[derive(Default)]
struct Totals {
    centroid: f64,
    rolloff: f64,
    flatness: f64,
    low: f64,
    high: f64,
    zcr: f64,
}

fn sign(x: f64) -> i8 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

fn mean_sd(v: &[f64]) -> (f64, f64) {
    let n = v.len().max(1) as f64;
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

fn percentile(values: &[f64], percent: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let last = sorted.len().saturating_sub(1);
    let at = last as f64 * percent / 100.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let below = at.floor() as usize;
    let above = (below + 1).min(last);
    sorted[below] + (sorted[above] - sorted[below]) * (at - below as f64)
}

/// How well the sound's pitch classes fit the best major and minor keys.
fn key_fit(chroma: &[f64; 12]) -> (f64, f64) {
    let total = chroma.iter().sum::<f64>() + 1e-9;
    let c: Vec<f64> = chroma.iter().map(|x| x / total).collect();
    let best = |profile: &[f64; 12]| {
        (0..12)
            .map(|shift| {
                let rolled: Vec<f64> = (0..12).map(|i| profile[(i + 12 - shift) % 12]).collect();
                correlation(&c, &rolled)
            })
            .fold(f64::NEG_INFINITY, f64::max)
    };
    (best(&MAJOR), best(&MINOR))
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let (ma, sa) = mean_sd(a);
    let (mb, sb) = mean_sd(b);
    if sa == 0.0 || sb == 0.0 {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - ma) * (y - mb))
        .sum::<f64>()
        / (a.len() as f64 * sa * sb)
}

/// Note starts and pulse, from how the spectrum rises frame to frame.
struct Onsets {
    flux: f64,
    rate: f64,
    bpm: f64,
    tempo_strength: f64,
}

impl Onsets {
    fn of(raw: &[f64]) -> Self {
        const SMOOTH: usize = 21;
        let n = raw.len();
        // The local mean, taken with zeros beyond the ends.
        let mut flux = Vec::with_capacity(n);
        for i in 0..n {
            let from = i.saturating_sub(SMOOTH / 2);
            let to = (i + SMOOTH / 2 + 1).min(n);
            flux.push(raw[i] - raw[from..to].iter().sum::<f64>() / SMOOTH as f64);
        }
        let (mean, sd) = mean_sd(&flux);
        let threshold = mean + sd;
        let peaks = flux
            .windows(3)
            .filter(|w| w[1] > threshold && w[1] >= w[0] && w[1] > w[2])
            .count();
        let hop_secs = HOP as f64 / RATE as f64;
        let positive: Vec<f64> = flux.iter().map(|f| f.max(0.0)).collect();
        let (env_mean, _) = mean_sd(&positive);
        let env: Vec<f64> = positive.iter().map(|f| f - env_mean).collect();
        let lag_at = |lag: usize| -> f64 {
            env.iter()
                .zip(env.iter().skip(lag))
                .map(|(a, b)| a * b)
                .sum()
        };
        let zero = lag_at(0) + 1e-9;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (lo, hi) = ((0.3 / hop_secs) as usize, (1.0 / hop_secs) as usize);
        let (mut lag, mut strength) = (lo, f64::NEG_INFINITY);
        for l in lo..hi {
            let v = lag_at(l) / zero;
            if v > strength {
                (lag, strength) = (l, v);
            }
        }
        Self {
            flux: mean,
            rate: peaks as f64 / (n.max(1) as f64 * hop_secs),
            bpm: 60.0 / (lag as f64 * hop_secs),
            tempo_strength: strength,
        }
    }
}

/// The scores made on this device for one library, in a small JSON file.
/// Kept away from the download cache, which is trimmed to make room: losing
/// this file would mean hearing the whole library again.
#[derive(Debug)]
pub struct SongScores {
    path: PathBuf,
    file: ScoresFile,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScoresFile {
    /// The model these came from. Another model's scores are thrown away.
    model: u32,
    songs: HashMap<String, Axes>,
    /// Songs that could not be decoded, so they are not fetched again.
    #[serde(default)]
    failed: HashSet<String>,
}

impl SongScores {
    /// The scores for library `name` kept in `dir`, or none yet.
    #[must_use]
    pub fn open(dir: &Path, name: &str) -> Self {
        let path = dir.join(format!("{name}.json"));
        let file = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ScoresFile>(&bytes).ok())
            .filter(|f| f.model == model::VERSION)
            .unwrap_or(ScoresFile {
                model: model::VERSION,
                ..ScoresFile::default()
            });
        Self { path, file }
    }

    /// Every song scored so far, by item id.
    #[must_use]
    pub fn songs(&self) -> &HashMap<String, Axes> {
        &self.file.songs
    }

    /// Whether this song needs no more attention.
    #[must_use]
    pub fn settled(&self, id: &str) -> bool {
        self.file.songs.contains_key(id) || self.file.failed.contains(id)
    }

    /// How many songs could not be decoded.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.file.failed.len()
    }

    /// Note where a song sits.
    pub fn set(&mut self, id: &str, axes: Axes) {
        self.file.songs.insert(id.to_string(), axes);
    }

    /// Note that a song cannot be decoded.
    pub fn set_failed(&mut self, id: &str) {
        self.file.failed.insert(id.to_string());
    }

    /// Write the file, whole or not at all.
    ///
    /// # Errors
    /// The folder cannot be written.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&self.file)?)?;
        fs::rename(tmp, &self.path)
    }
}

#[cfg(test)]
// Test signals: small counts, and samples that fit f32.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    fn tone(hz: f64, secs: f64) -> Vec<f32> {
        let n = (secs * RATE as f64) as usize;
        (0..n)
            .map(|i| (0.5 * (std::f64::consts::TAU * hz * i as f64 / RATE as f64).sin()) as f32)
            .collect()
    }

    fn noise(secs: f64) -> Vec<f32> {
        let mut state = 1u32;
        (0..(secs * RATE as f64) as usize)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(state) / f64::from(u32::MAX)) as f32 - 0.5) * 0.5
            })
            .collect()
    }

    /// The column of a measurement in a stretch's row.
    fn at(name: &str) -> usize {
        let names = [
            "rms",
            "rms_sd",
            "range",
            "centroid",
            "log_centroid",
            "rolloff",
            "flatness",
            "low",
            "high",
            "zcr",
            "flux",
            "onsets",
            "bpm",
            "strength",
            "major",
            "minor",
            "mode",
        ];
        MELS * 2 + names.iter().position(|n| *n == name).unwrap()
    }

    #[test]
    fn the_fft_finds_a_tone() {
        let s = Spectra::new();
        let (mut re, mut im) = (vec![0.0; NFFT], vec![0.0; NFFT]);
        for (i, r) in re.iter_mut().enumerate() {
            *r = (std::f64::consts::TAU * 32.0 * i as f64 / NFFT as f64).cos();
        }
        s.fft(&mut re, &mut im);
        let peak = (0..BINS)
            .max_by(|&a, &b| {
                (re[a].powi(2) + im[a].powi(2)).total_cmp(&(re[b].powi(2) + im[b].powi(2)))
            })
            .unwrap();
        assert_eq!(peak, 32);
        assert!((re[32] - NFFT as f64 / 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_tone_and_noise_measure_as_they_sound() {
        let s = Spectra::new();
        let low_tone = s.measure(&tone(200.0, 10.0)).unwrap();
        let high_tone = s.measure(&tone(5000.0, 10.0)).unwrap();
        let hiss = s.measure(&noise(10.0)).unwrap();
        assert_eq!(low_tone.len(), PER_WINDOW);
        assert!(low_tone[at("low")] > 0.9, "{}", low_tone[at("low")]);
        assert!(high_tone[at("high")] > 0.9, "{}", high_tone[at("high")]);
        assert!(low_tone[at("centroid")] < high_tone[at("centroid")]);
        assert!(hiss[at("flatness")] > low_tone[at("flatness")] * 5.0);
        // A 200 Hz tone crosses zero 400 times a second, 1 in 40 samples.
        assert!(
            (low_tone[at("zcr")] - 0.025).abs() < 0.002,
            "{}",
            low_tone[at("zcr")]
        );
    }

    #[test]
    fn a_steady_pulse_measures_as_its_tempo() {
        // A click every half second, 120 beats a minute.
        let mut x = vec![0.0; RATE * 10];
        let mut state = 1u32;
        for beat in 0..20 {
            for s in &mut x[beat * RATE / 2..beat * RATE / 2 + 400] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *s = (f64::from(state) / f64::from(u32::MAX)) as f32 - 0.5;
            }
        }
        let row = Spectra::new().measure(&x).unwrap();
        assert!((row[at("bpm")] - 120.0).abs() < 3.0, "{}", row[at("bpm")]);
        assert!(
            (row[at("onsets")] - 2.0).abs() < 0.5,
            "{}",
            row[at("onsets")]
        );
    }

    #[test]
    fn a_short_stretch_measures_nothing() {
        assert!(Spectra::new().measure(&tone(440.0, 0.5)).is_none());
    }

    #[test]
    fn decimating_removes_what_would_fold_down() {
        let high: Vec<f32> = (0..48_000)
            .map(|i| (std::f64::consts::TAU * 15_000.0 * f64::from(i) / 48_000.0).sin() as f32)
            .collect();
        let low: Vec<f32> = (0..48_000)
            .map(|i| (std::f64::consts::TAU * 1_000.0 * f64::from(i) / 48_000.0).sin() as f32)
            .collect();
        let power = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32;
        assert!(
            power(&decimate(&high)) < 0.001,
            "{}",
            power(&decimate(&high))
        );
        assert!((power(&decimate(&low)) - 0.5).abs() < 0.02);
    }

    #[test]
    fn scores_stay_on_the_axes_whatever_comes_in() {
        for x in [1e9f32, -1e9, f32::NAN, f32::INFINITY] {
            let a = score(&vec![x; FEATURES]);
            assert!((1.0..=5.0).contains(&a.energy), "{a:?}");
            assert!((-1.0..=1.0).contains(&a.valence), "{a:?}");
            assert!(a.voice.abs() < f32::EPSILON);
        }
    }

    #[test]
    fn a_model_of_the_right_shape() {
        assert_eq!(model::MEAN.len(), FEATURES);
        assert_eq!(model::SPREAD.len(), FEATURES);
        assert_eq!(model::ENERGY.len(), FEATURES);
        assert_eq!(model::VALENCE.len(), FEATURES);
    }

    #[test]
    fn scores_outlast_a_reopen_but_not_a_new_model() {
        let dir = std::env::temp_dir().join(format!("underscore-hearing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut s = SongScores::open(&dir, "music");
        let axes = Axes {
            energy: 3.0,
            valence: 0.5,
            voice: 0.0,
        };
        s.set("a", axes);
        s.set_failed("b");
        s.save().unwrap();

        let again = SongScores::open(&dir, "music");
        assert_eq!(again.songs().get("a"), Some(&axes));
        assert!(again.settled("a") && again.settled("b") && !again.settled("c"));
        assert_eq!(again.failed(), 1);

        fs::write(
            dir.join("music.json"),
            br#"{"model":0,"songs":{"a":{"energy":1,"valence":0}}}"#,
        )
        .unwrap();
        assert!(SongScores::open(&dir, "music").songs().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
