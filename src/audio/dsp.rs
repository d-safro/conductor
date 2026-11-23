//! What a song sounds like, as a few numbers: worked out from its samples (mono, `RATE` Hz) with a short-time Fourier
//! transform (2048-sample frames, 512 apart: 93 ms windows every 23 ms).
//!
//! - **tempo** (BPM) from the autocorrelation of the onset envelope (how much louder each mel band got from one frame
//!   to the next), with a preference for 120 BPM against doubling and halving, refined on a peak a few beats later;
//!   **pulse** (how clear that beat is: the autocorrelation there against at no lag) and **onset** (how much the sound
//!   jumps: percussive or smooth);
//! - the spectrum's **centroid** (bright or dark), **bandwidth**, **rolloff** (85% of the energy below) and
//!   **flatness** (noise or tones, in dB), and the **zero-crossing rate**, each as its mean and spread over the frames;
//! - **level** and its spread (dB of the frames' RMS: loud and squashed, or quiet with dynamics);
//! - a 12-bin **chroma** profile (how much of each pitch class, C to B: the key, roughly);
//! - 13 **MFCC**s (the timbre: a 40-band mel spectrum in dB, then a DCT), their means and spreads.
//!
//! Frames quieter than −60 dB (silence, fades) count only for the onset envelope. Everything is in plain Rust with
//! `realfft` for the transform; a minute of audio takes some tens of milliseconds.

use realfft::RealFftPlanner;

/// The sample rate the songs are decoded at: enough for everything here (up to 11 kHz), and cheap.
pub const RATE: u32 = 22_050;
const N_FFT: usize = 2048;
const HOP: usize = 512;
const BINS: usize = N_FFT / 2 + 1;
const N_MELS: usize = 40;
const MEL_TOP_HZ: f32 = 8000.0;
pub const N_MFCC: usize = 13;
/// Quieter frames are silence (or a fade): not counted.
const SILENT_DB: f32 = -60.0;
const CHROMA_LOW_HZ: f32 = 100.0;
const CHROMA_HIGH_HZ: f32 = 5000.0;
const SLOWEST_BPM: f32 = 30.0;
const FASTEST_BPM: f32 = 250.0;
/// The tempo most music is near: of two that fit (one twice the other), the one closer to this.
const USUAL_BPM: f32 = 120.0;
/// Less than this isn't worth analysing.
pub const SHORTEST_SECS: f32 = 2.0;

/// How many numbers a song is described by (see `Features::to_vec`).
pub const DIMS: usize = 3 + 2 * 6 + 12 + 2 * N_MFCC;

#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    /// BPM (0: no beat found)
    pub tempo: f32,
    /// how clear the beat is, 0…1
    pub pulse: f32,
    /// the mean onset strength (dB per frame, over the mel bands)
    pub onset: f32,
    /// [mean, spread] over the frames: Hz, Hz, Hz, dB, crossings per sample, dB
    pub centroid: [f32; 2],
    pub bandwidth: [f32; 2],
    pub rolloff: [f32; 2],
    pub flatness: [f32; 2],
    pub zcr: [f32; 2],
    pub level: [f32; 2],
    /// C, C♯, D … B; adds up to 1
    pub chroma: [f32; 12],
    pub mfcc_mean: [f32; N_MFCC],
    pub mfcc_std: [f32; N_MFCC],
}

impl Features {
    /// The numbers in a fixed order (what's stored; `index.rs` weighs them by this order).
    pub fn to_vec(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(DIMS);
        v.extend([self.tempo, self.pulse, self.onset]);
        for p in [self.centroid, self.bandwidth, self.rolloff, self.flatness, self.zcr, self.level] {
            v.extend(p);
        }
        v.extend(self.chroma);
        v.extend(self.mfcc_mean);
        v.extend(self.mfcc_std);
        debug_assert_eq!(v.len(), DIMS);
        v
    }
}

/// Little-endian f32s, as stored in the database.
pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The numbers back from the database (None when they aren't `DIMS` of them).
pub fn from_bytes(b: &[u8]) -> Option<Vec<f32>> {
    (b.len() == DIMS * 4).then(|| b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// A mean and a spread, added to one value at a time.
#[derive(Default, Clone, Copy)]
struct Acc {
    n: f64,
    sum: f64,
    sq: f64,
}

impl Acc {
    fn add(&mut self, x: f32) {
        if x.is_finite() {
            self.n += 1.0;
            self.sum += x as f64;
            self.sq += (x as f64) * (x as f64);
        }
    }
    fn get(&self) -> [f32; 2] {
        if self.n == 0.0 {
            return [0.0, 0.0];
        }
        let m = self.sum / self.n;
        [m as f32, (self.sq / self.n - m * m).max(0.0).sqrt() as f32]
    }
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Triangular mel bands over the FFT's bins: (first bin, weights).
fn mel_bank() -> Vec<(usize, Vec<f32>)> {
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(MEL_TOP_HZ));
    let edges: Vec<f32> = (0..N_MELS + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f32 / (N_MELS + 1) as f32)).collect();
    let hz = |k: usize| k as f32 * RATE as f32 / N_FFT as f32;
    (0..N_MELS)
        .map(|b| {
            let (l, c, r) = (edges[b], edges[b + 1], edges[b + 2]);
            let first = (0..BINS).find(|&k| hz(k) > l).unwrap_or(BINS);
            let w: Vec<f32> = (first..BINS)
                .take_while(|&k| hz(k) < r)
                .map(|k| {
                    let f = hz(k);
                    if f <= c { (f - l) / (c - l) } else { (r - f) / (r - c) }
                })
                .collect();
            (first, w)
        })
        .collect()
}

/// DCT-II (orthonormal), the first `N_MFCC` rows.
fn dct() -> Vec<[f32; N_MELS]> {
    (0..N_MFCC)
        .map(|k| {
            let s = if k == 0 { (1.0 / N_MELS as f32).sqrt() } else { (2.0 / N_MELS as f32).sqrt() };
            let mut row = [0.0; N_MELS];
            for (n, x) in row.iter_mut().enumerate() {
                *x = s * (std::f32::consts::PI * k as f32 * (2 * n + 1) as f32 / (2 * N_MELS) as f32).cos();
            }
            row
        })
        .collect()
}

/// The pitch class of a frequency: 0 = C … 9 = A (440 Hz) … 11 = B.
pub fn pitch_class(f: f32) -> usize {
    ((12.0 * (f / 440.0).log2()).round() as i32 + 9).rem_euclid(12) as usize
}

/// Describe a song from its samples (mono, `RATE` Hz, −1…1). None when it's too short or silent.
pub fn analyse(x: &[f32]) -> Option<Features> {
    if x.len() < N_FFT || (x.len() as f32) < SHORTEST_SECS * RATE as f32 {
        return None;
    }
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let (mut input, mut spec, mut scratch) = (fft.make_input_vec(), fft.make_output_vec(), fft.make_scratch_vec());
    let window: Vec<f32> =
        (0..N_FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos()).collect();
    let bank = mel_bank();
    let dct = dct();
    let freq: Vec<f32> = (0..BINS).map(|k| k as f32 * RATE as f32 / N_FFT as f32).collect();
    let pc: Vec<Option<usize>> =
        freq.iter().map(|&f| (CHROMA_LOW_HZ..=CHROMA_HIGH_HZ).contains(&f).then(|| pitch_class(f))).collect();

    let frames = (x.len() - N_FFT) / HOP + 1;
    let mut onset = Vec::with_capacity(frames);
    let mut prev: Option<[f32; N_MELS]> = None;
    let (mut centroid, mut bandwidth, mut rolloff, mut flatness, mut zcr, mut level) =
        (Acc::default(), Acc::default(), Acc::default(), Acc::default(), Acc::default(), Acc::default());
    let mut mfcc = [Acc::default(); N_MFCC];
    let mut chroma = [0f64; 12];
    let mut power = vec![0f32; BINS];
    for f in 0..frames {
        let frame = &x[f * HOP..f * HOP + N_FFT];
        for ((i, w), s) in input.iter_mut().zip(&window).zip(frame) {
            *i = s * w;
        }
        fft.process_with_scratch(&mut input, &mut spec, &mut scratch).ok()?;
        for (p, c) in power.iter_mut().zip(&spec) {
            *p = c.norm_sqr();
        }
        // the mel spectrum in dB: the onset envelope (every frame), and the MFCCs
        let mut mel = [0f32; N_MELS];
        for (m, (first, w)) in mel.iter_mut().zip(&bank) {
            let e: f32 = w.iter().zip(&power[*first..]).map(|(a, b)| a * b).sum();
            *m = 10.0 * e.max(1e-10).log10();
        }
        let flux = prev.map_or(0.0, |p| mel.iter().zip(&p).map(|(a, b)| (a - b).max(0.0)).sum::<f32>() / N_MELS as f32);
        onset.push(flux);
        prev = Some(mel);

        let ms = frame.iter().map(|s| s * s).sum::<f32>() / N_FFT as f32;
        let db = 10.0 * ms.max(1e-12).log10();
        if db < SILENT_DB {
            continue;
        }
        level.add(db);
        zcr.add(frame.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count() as f32 / (N_FFT - 1) as f32);
        // the spectrum's shape
        let (mut sum_m, mut sum_fm, mut total) = (0f32, 0f32, 0f32);
        let mut logs = 0f64;
        for (k, &p) in power.iter().enumerate() {
            let m = p.sqrt();
            sum_m += m;
            sum_fm += freq[k] * m;
            total += p;
            logs += ((p + 1e-12) as f64).ln();
        }
        if sum_m <= 0.0 {
            continue;
        }
        let c = sum_fm / sum_m;
        centroid.add(c);
        bandwidth.add((power.iter().zip(&freq).map(|(p, f)| p.sqrt() * (f - c) * (f - c)).sum::<f32>() / sum_m).sqrt());
        let mut cum = 0.0;
        let roll = power.iter().position(|p| {
            cum += p;
            cum >= 0.85 * total
        });
        rolloff.add(freq[roll.unwrap_or(BINS - 1)]);
        let geo = (logs / BINS as f64).exp();
        let arith = (total as f64 + 1e-12 * BINS as f64) / BINS as f64;
        flatness.add((10.0 * (geo / arith).log10()) as f32);
        // chroma: each frame's profile against its strongest pitch class
        let mut ch = [0f64; 12];
        for (k, p) in pc.iter().enumerate() {
            if let Some(p) = p {
                ch[*p] += power[k].sqrt() as f64;
            }
        }
        let top = ch.iter().cloned().fold(0.0, f64::max);
        if top > 0.0 {
            for (a, b) in chroma.iter_mut().zip(ch) {
                *a += b / top;
            }
        }
        for (acc, row) in mfcc.iter_mut().zip(&dct) {
            acc.add(row.iter().zip(&mel).map(|(a, b)| a * b).sum());
        }
    }
    if level.n == 0.0 {
        return None; // silence
    }
    let fps = RATE as f32 / HOP as f32;
    let (tempo, pulse) = tempo(&onset, fps);
    let sum: f64 = chroma.iter().sum();
    let chroma = chroma.map(|c| if sum > 0.0 { (c / sum) as f32 } else { 0.0 });
    let mfcc_mean: [f32; N_MFCC] = std::array::from_fn(|i| mfcc[i].get()[0]);
    let mfcc_std: [f32; N_MFCC] = std::array::from_fn(|i| mfcc[i].get()[1]);
    Some(Features {
        tempo,
        pulse,
        onset: onset.iter().sum::<f32>() / onset.len() as f32,
        centroid: centroid.get(),
        bandwidth: bandwidth.get(),
        rolloff: rolloff.get(),
        flatness: flatness.get(),
        zcr: zcr.get(),
        level: level.get(),
        chroma,
        mfcc_mean,
        mfcc_std,
    })
}

/// Where the top of a peak of `ac` at `l` really is, between the lags (a parabola through it and its neighbours).
fn peak_at(ac: &[f32], l: usize) -> f32 {
    let (a, b, c) = (ac[l - 1], ac[l], ac[l + 1]);
    let d = a - 2.0 * b + c;
    if d.abs() < 1e-12 { l as f32 } else { l as f32 + (0.5 * (a - c) / d).clamp(-0.5, 0.5) }
}

/// The tempo (BPM) and how clear the beat is (0…1) from an onset envelope with `fps` values a second. (0, 0) when
/// there's no beat.
pub fn tempo(onset: &[f32], fps: f32) -> (f32, f32) {
    let n = onset.len();
    if n < 8 {
        return (0.0, 0.0);
    }
    // smoothed a little (a beat that falls between frames isn't split over two lags), less its mean
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let at = |j: isize| onset[(i as isize + j).clamp(0, n as isize - 1) as usize];
            (at(-2) + 4.0 * at(-1) + 6.0 * at(0) + 4.0 * at(1) + at(2)) / 16.0
        })
        .collect();
    let mean = smooth.iter().sum::<f32>() / n as f32;
    let o: Vec<f32> = smooth.iter().map(|v| v - mean).collect();
    let min_lag = ((60.0 * fps / FASTEST_BPM).floor() as usize).max(2);
    let max_lag = (60.0 * fps / SLOWEST_BPM).ceil() as usize;
    // as far as four of the slowest beats, for the refinement (and never past half the envelope)
    let far = (max_lag * 4).min(n / 2);
    if far < min_lag + 2 {
        return (0.0, 0.0);
    }
    let ac: Vec<f32> = (0..=far).map(|l| o[..n - l].iter().zip(&o[l..]).map(|(a, b)| a * b).sum::<f32>() / (n - l) as f32).collect();
    if ac[0] <= 0.0 {
        return (0.0, 0.0);
    }
    // the best lag: a peak, weighed by how near its tempo is to the usual one (an octave away counts half as much)
    let mut best: Option<(usize, f32)> = None;
    for l in min_lag..=max_lag.min(far - 1) {
        if ac[l] > 0.0 && ac[l] >= ac[l - 1] && ac[l] >= ac[l + 1] {
            let bpm = 60.0 * fps / l as f32;
            let score = ac[l] * (-0.5 * (bpm / USUAL_BPM).log2().powi(2)).exp();
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((l, score));
            }
        }
    }
    let Some((l, _)) = best else { return (0.0, 0.0) };
    let pulse = (ac[l] / ac[0]).clamp(0.0, 1.0);
    // more precise on a peak a few beats later: k beats apart, k times the precision
    let mut lag = peak_at(&ac, l);
    for k in [4usize, 3, 2] {
        let c = (lag * k as f32).round() as usize;
        if c + 3 <= far {
            let m = (c - 2..=c + 2).max_by(|a, b| ac[*a].total_cmp(&ac[*b])).unwrap_or(c);
            if m > c - 2 && m < c + 2 {
                lag = peak_at(&ac, m) / k as f32;
            }
            break;
        }
    }
    (60.0 * fps / lag, pulse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32) -> usize {
        (s * RATE as f32) as usize
    }

    /// Noise that's the same every time.
    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    /// Short clicks (decaying noise, 10 ms) at `bpm`.
    fn clicks(bpm: f32, len: f32) -> Vec<f32> {
        let mut seed = 7;
        let every = 60.0 / bpm;
        (0..secs(len))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let since = t % every;
                if since < 0.01 { 0.8 * noise(&mut seed) * (-since * 400.0).exp() } else { 0.0 }
            })
            .collect()
    }

    fn sine(hz: f32, len: f32, amp: f32) -> Vec<f32> {
        (0..secs(len)).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin()).collect()
    }

    #[test]
    fn tempo_of_a_click_track() {
        for bpm in [65.0, 72.0, 90.0, 100.0, 120.0, 128.0, 140.0, 150.0, 174.0] {
            let f = analyse(&clicks(bpm, 30.0)).unwrap();
            assert!((f.tempo - bpm).abs() <= 3.0, "{bpm} BPM: found {}", f.tempo);
            assert!(f.pulse > 0.3, "{bpm} BPM: a clear beat ({})", f.pulse);
        }
    }

    #[test]
    fn chroma_of_a_pure_a_is_a() {
        assert_eq!(pitch_class(440.0), 9);
        assert_eq!(pitch_class(261.63), 0);
        assert_eq!(pitch_class(466.16), 10);
        assert_eq!(pitch_class(110.0), 9);
        let f = analyse(&sine(440.0, 5.0, 0.5)).unwrap();
        let top = (0..12).max_by(|a, b| f.chroma[*a].total_cmp(&f.chroma[*b])).unwrap();
        assert_eq!(top, 9, "{:?}", f.chroma);
        assert!(f.chroma[9] > 0.5, "mostly A: {:?}", f.chroma);
        // and a C is a C
        let f = analyse(&sine(523.25, 5.0, 0.5)).unwrap();
        let top = (0..12).max_by(|a, b| f.chroma[*a].total_cmp(&f.chroma[*b])).unwrap();
        assert_eq!(top, 0, "{:?}", f.chroma);
        assert!((f.chroma.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn noise_is_bright_and_flat_a_tone_is_neither() {
        let mut seed = 1;
        let hiss: Vec<f32> = (0..secs(5.0)).map(|_| 0.3 * noise(&mut seed)).collect();
        let (n, t) = (analyse(&hiss).unwrap(), analyse(&sine(220.0, 5.0, 0.3)).unwrap());
        assert!(n.centroid[0] > 3000.0 && t.centroid[0] < 500.0, "{} {}", n.centroid[0], t.centroid[0]);
        assert!(n.flatness[0] > -10.0 && t.flatness[0] < -30.0, "{} {}", n.flatness[0], t.flatness[0]);
        assert!(n.zcr[0] > 5.0 * t.zcr[0], "{} {}", n.zcr[0], t.zcr[0]);
        assert!(n.rolloff[0] > t.rolloff[0]);
        // the level: a sine at 0.3 has an RMS of 0.3/√2, −13.5 dB
        assert!((t.level[0] + 13.5).abs() < 0.5, "{}", t.level[0]);
        assert!(t.level[1] < 0.5, "steady: {}", t.level[1]);
    }

    #[test]
    fn silence_and_tiny_files_have_nothing() {
        assert!(analyse(&vec![0.0; secs(10.0)]).is_none());
        assert!(analyse(&sine(440.0, 1.0, 0.5)).is_none());
        assert!(analyse(&[]).is_none());
        // a beat needs a beat: a steady tone has no clear one
        let f = analyse(&sine(330.0, 10.0, 0.5)).unwrap();
        assert!(f.pulse < 0.3, "{}", f.pulse);
    }

    #[test]
    fn stored_as_bytes_and_back() {
        let f = analyse(&clicks(100.0, 6.0)).unwrap();
        let v = f.to_vec();
        assert_eq!(v.len(), DIMS);
        assert_eq!(from_bytes(&to_bytes(&v)).unwrap(), v);
        assert!(from_bytes(&[0; 12]).is_none());
        assert!(v.iter().all(|x| x.is_finite()), "{v:?}");
    }
}
