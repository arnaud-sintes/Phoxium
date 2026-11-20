//! Composite and S-Video encode and decode (T-31, optional module, off by default): the f64 oracle of
//! the `signal` slot (`shaders/composite.comp`). Voltage domain, before the video amplifier: the input
//! and the output are drive voltages R'G'B' in [0, 1] (gamma-encoded, as a composite encoder sees them).
//!
//! # Model (`CRT.md` "Composite, S-Video, component"; PLAN T-31)
//!
//! Everything happens on the horizontal scan line, at the source's pixel clock f_px (MHz, the
//! `amp_pixel_clock_mhz` parameter shared with the amplifier): one source pixel lasts 1 / f_px us. The
//! line is simulated as a discrete signal of [`SAMPLES_PER_PX`] samples per source pixel (sample k is
//! centred at x = (k + 1/2) / S px from the left edge of the active line); each source pixel holds its
//! value over its S samples (zero-order hold). Outside the active line the signal is blanking: 0, no
//! burst (the burst only sets the decoder's phase reference, which is assumed locked).
//!
//! 1. **Encode** per pixel: Y = 0.299 R' + 0.587 G' + 0.114 B' (BT.601 luma [K]),
//!    U = 0.492111 (B' - Y), V = 0.877283 (R' - Y) (BT.470 / SMPTE 170M scaled colour differences [K]).
//!    The subcarrier phase of sample k on line y of frame n, in cycles, is
//!    phi = c x_k + frac(y a_line + n a_frame + phi0), with c = f_sc / f_px subcarrier cycles per pixel,
//!    a_line = frac(f_sc / f_line) (the advance per line: NTSC 227.5 cycles per line gives 1/2, i.e. 180
//!    degrees), a_frame the advance per frame (`composite_frame_advance_deg` / 360: 180 degrees for
//!    broadcast NTSC, whose 525-line frame holds an odd number of half cycles; 0 for a source whose dot
//!    pattern stands still) and phi0 the phase at the left edge of line 0 of frame 0. The chroma is
//!    C = U sin(2 pi phi) + s V cos(2 pi phi), s = +1 in NTSC and the PAL V switch (-1)^y in PAL. NTSC
//!    I and Q are the same quadrature pair turned by 33 degrees [K]: C = I cos(2 pi phi + 33 deg) +
//!    Q sin(2 pi phi + 33 deg), with I = -U sin 33 + V cos 33, Q = U cos 33 + V sin 33. The encoder does
//!    not band-limit I and Q (8-bit consoles switch the chroma per pixel; a broadcast encoder's I/Q
//!    filters are assumed absorbed into the decoder's, see 4). Composite: c = Y + C. S-Video: Y and C on
//!    separate wires.
//! 2. **Channel** (composite only, optional): a Gaussian low-pass of -3 dB bandwidth B_ch
//!    (`composite_bandwidth_mhz`, 0 = none; time sigma 0.1325 / B_ch, as the amplifier,
//!    [`crate::beam::gaussian_amp_sigma_ns`]), discretised on the samples (truncated at 4 sigma,
//!    normalised; B_ch lifted so that sigma is at most [`CHANNEL_SIGMA_MAX_SAMPLES`]). The decoder's
//!    automatic colour control (ACC) measures the burst, which went through the same channel, so it scales
//!    the separated chroma by 1 / |H(f_sc)| = 2^((f_sc / B_ch)^2 / 2), at most [`ACC_GAIN_MAX`] (a narrow
//!    channel then desaturates the picture, as a real ACC at its limit).
//! 3. **Luma/chroma separation** (composite only):
//!    - `Notch`: luma = the mean of the signal over one subcarrier period (a box of L = S / c samples,
//!      fractional end weights), which nulls f_sc and its harmonics; chroma = signal - luma. Simple and
//!      soft: the box also halves the luma response at f_sc / 2 (sinc).
//!    - `Comb` (NTSC): the simple band-split 1-line comb. d = (c_y - c_{y-1}) / 2 holds the chroma when the
//!      phase advances 180 degrees per line and the luma of the two lines is equal; chroma = d - box(d)
//!      (only the band around the subcarrier is combed), luma = c_y - chroma. Line 0 (no previous line)
//!      uses the notch. Vertical colour or luma changes give hanging dots and colour errors, as in a real
//!      comb; a phase advance other than 180 degrees breaks it.
//!    - PAL: the luma is always the notch; `Comb` selects the PAL delay line (PAL-D): the demodulated U
//!      and V of the line and of the previous line (each with its own V switch) are averaged, which turns
//!      a hue error into a saturation loss cos(h). `Notch` is the simple PAL-S decoder.
//!    - S-Video: luma = Y and chroma = C as received (no crosstalk, no channel filter, no ACC).
//! 4. **Demodulation and chroma bandwidth**: with the demodulator angle t = 2 pi phi + a + h (a = 33
//!    degrees in NTSC, 0 in PAL; h = `composite_hue_deg`, a phase error of the decoder's reference),
//!    A = G_A * (2 chroma cos t), B = G_B * (2 chroma sin t), G a normalised Gaussian low-pass evaluated
//!    at the pixel centre (truncated at 4 sigma). NTSC: I = A (bandwidth `composite_i_bandwidth_mhz`,
//!    1.3 MHz [K]), Q = B (`composite_q_bandwidth_mhz`, 0.5 MHz [K]), then U, V by the inverse turn.
//!    PAL: V = s A, U = B, both at the I bandwidth (PAL U and V about 1.3 MHz [K]). The demodulator
//!    products at 2 f_sc fall far outside these bandwidths.
//! 5. **Output**: Y = the mean of the luma over the pixel's S samples; U and V times
//!    `composite_saturation`; R' = Y + V / 0.877283, B' = Y + U / 0.492111, G' = (Y - 0.299 R' -
//!    0.114 B') / 0.587, clamped to [0, 1] (the drive rails; an estimate: real decoders clip somewhat
//!    above white).
//!
//! What the model gives: a flat colour field returns unchanged (2 f_sc products filtered, ACC exact at
//! f_sc); a colour edge bleeds over the chroma low-pass (10-90 % width 0.3396 f_px / B pixels, so the Q
//! bleed is 2.6 times the I bleed in NTSC); luma detail near f_sc demodulates as false colour (cross-colour, the
//! "artifact colours" of 8-bit era games when f_px is close to 2 f_sc); chroma left in the luma by an
//! imperfect notch moves with the line and frame phase (dot crawl).
//!
//! Not modelled: the sync and burst themselves (the decoder is locked), the 7.5 IRE set-up, an encoder
//! I/Q filter, VSB/RF artefacts, adaptive (2D or 3D) comb filters, the PAL 25 Hz offset beyond its effect
//! on f_sc / f_line, the per-frame flip of the PAL V switch.
//!
//! Numbers: f_sc NTSC 3.579545 MHz and PAL 4.43361875 MHz, line rates 15.734 and 15.625 kHz [S, CRT.md];
//! the exact NTSC line rate 4.5 MHz / 286 = 15.734266 kHz [K]; the matrices and the 33 degree turn [K];
//! the I/Q bandwidths 1.3 / 0.5 MHz [K] (CRT.md: "chroma bandwidth about 0.5 to 1.3 MHz"); the NTSC
//! broadcast luma bandwidth 4.2 MHz [K]; the Gaussian shapes of every filter and the box notch are
//! modelling choices [E].

use std::f64::consts::PI;

use crate::beam::gaussian_amp_sigma_ns;

/// Samples per source pixel of the line simulation (the GPU uses the same).
pub const SAMPLES_PER_PX: usize = 8;
/// Gaussian kernels are truncated at this many sigmas (then normalised).
pub const REACH_SIGMAS: f64 = 4.0;
/// NTSC colour subcarrier, MHz [S, CRT.md].
pub const NTSC_SUBCARRIER_MHZ: f64 = 3.579545;
/// PAL colour subcarrier, MHz [S, CRT.md].
pub const PAL_SUBCARRIER_MHZ: f64 = 4.43361875;
/// NTSC line rate, kHz: 4.5 MHz / 286 [K] (CRT.md: 15.734 kHz).
pub const NTSC_LINE_RATE_KHZ: f64 = 4500.0 / 286.0;
/// PAL line rate, kHz [S, CRT.md].
pub const PAL_LINE_RATE_KHZ: f64 = 15.625;
/// BT.601 luma weights [K].
pub const LUMA: [f64; 3] = [0.299, 0.587, 0.114];
/// U = U_SCALE (B' - Y) [K].
pub const U_SCALE: f64 = 0.492111;
/// V = V_SCALE (R' - Y) [K].
pub const V_SCALE: f64 = 0.877283;
/// Turn of the NTSC I/Q axes against U/V, degrees [K].
pub const IQ_TURN_DEG: f64 = 33.0;
/// Largest gain of the automatic colour control: 8 (+18 dB). A real ACC loop saturates at a few times its
/// nominal gain; the value is an estimate [E]. It also keeps the gain finite: uncapped, 2^((f_sc / B)^2 / 2)
/// overflows f32 below B = f_sc / 16 and f64 below about f_sc / 38 (review D-03).
pub const ACC_GAIN_MAX: f64 = 8.0;
/// Largest sigma of the channel low-pass, samples (one source pixel): a narrower channel bandwidth is
/// lifted to the one of this sigma, 0.1325 f_px (about 0.71 MHz at 5.37 MHz). Its 4-sigma reach, 32
/// samples, is what the GPU's line tile holds for the channel (`shaders/composite.comp`, `CAP_CHANNEL`), so
/// the oracle and the shader apply the same kernel at every bandwidth. A modelling limit [E]: at such
/// bandwidths the chroma is attenuated far beyond [`ACC_GAIN_MAX`] anyway (colour killed).
pub const CHANNEL_SIGMA_MAX_SAMPLES: f64 = 8.0;

/// Colour television standard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standard {
    Ntsc,
    Pal,
}

/// How luma and chroma reach the decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// One wire: c = Y + C, separated by the decoder.
    Composite,
    /// Separate Y and C wires.
    SVideo,
}

/// Luma/chroma separation (composite) and, in PAL, the chroma delay line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Separation {
    /// Box over one subcarrier period (PAL: no delay line, PAL-S).
    Notch,
    /// NTSC: band-split 1-line comb. PAL: notch luma and the chroma delay line (PAL-D).
    Comb,
}

/// The composite chain's settings, physical units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Composite {
    pub standard: Standard,
    pub connection: Connection,
    pub separation: Separation,
    /// Source pixel clock, MHz.
    pub pixel_clock_mhz: f64,
    /// Colour subcarrier, MHz.
    pub subcarrier_mhz: f64,
    /// Line rate, kHz.
    pub line_rate_khz: f64,
    /// Subcarrier phase advance per frame, degrees.
    pub frame_advance_deg: f64,
    /// Subcarrier phase at the left edge of line 0, frame 0, degrees.
    pub phase_deg: f64,
    /// NTSC I (PAL U and V) bandwidth, MHz.
    pub i_bandwidth_mhz: f64,
    /// NTSC Q bandwidth, MHz.
    pub q_bandwidth_mhz: f64,
    /// Composite channel bandwidth, MHz; 0 (or any non-positive value) = no channel filter. See
    /// [`Composite::channel_bandwidth_effective_mhz`].
    pub channel_bandwidth_mhz: f64,
    /// Decoder phase error (tint), degrees.
    pub hue_deg: f64,
    /// Chroma gain (colour).
    pub saturation: f64,
}

impl Composite {
    /// The NTSC defaults of `params/composite.toml` at the pixel clock `f_px` MHz.
    pub fn ntsc(pixel_clock_mhz: f64) -> Self {
        Self {
            standard: Standard::Ntsc,
            connection: Connection::Composite,
            separation: Separation::Notch,
            pixel_clock_mhz,
            subcarrier_mhz: NTSC_SUBCARRIER_MHZ,
            line_rate_khz: NTSC_LINE_RATE_KHZ,
            frame_advance_deg: 180.0,
            phase_deg: 0.0,
            i_bandwidth_mhz: 1.3,
            q_bandwidth_mhz: 0.5,
            channel_bandwidth_mhz: 4.2,
            hue_deg: 0.0,
            saturation: 1.0,
        }
    }

    /// The PAL defaults at the pixel clock `f_px` MHz (U and V at 1.3 MHz).
    pub fn pal(pixel_clock_mhz: f64) -> Self {
        Self {
            standard: Standard::Pal,
            subcarrier_mhz: PAL_SUBCARRIER_MHZ,
            line_rate_khz: PAL_LINE_RATE_KHZ,
            channel_bandwidth_mhz: 5.0,
            ..Self::ntsc(pixel_clock_mhz)
        }
    }

    /// Subcarrier cycles per source pixel, c = f_sc / f_px.
    ///
    /// # Panics
    /// When the pixel clock or the subcarrier is not positive and finite.
    pub fn cycles_per_px(&self) -> f64 {
        assert!(
            self.pixel_clock_mhz.is_finite() && self.pixel_clock_mhz > 0.0,
            "pixel clock"
        );
        assert!(
            self.subcarrier_mhz.is_finite() && self.subcarrier_mhz > 0.0,
            "subcarrier"
        );
        self.subcarrier_mhz / self.pixel_clock_mhz
    }

    /// Subcarrier phase advance per line, cycles in [0, 1): frac(f_sc / f_line).
    ///
    /// # Panics
    /// When the line rate is not positive and finite.
    pub fn line_advance_cycles(&self) -> f64 {
        assert!(self.line_rate_khz.is_finite() && self.line_rate_khz > 0.0, "line rate");
        (self.subcarrier_mhz * 1000.0 / self.line_rate_khz).rem_euclid(1.0)
    }

    /// Phase of frame `n` at the left edge of line 0, cycles in [0, 1).
    pub fn frame_phase_cycles(&self, frame: u32) -> f64 {
        (f64::from(frame) * self.frame_advance_deg / 360.0 + self.phase_deg / 360.0).rem_euclid(1.0)
    }

    /// Phase at the left edge of line `y` of frame `n`, cycles in [0, 1).
    pub fn line_phase_cycles(&self, y: u32, frame: u32) -> f64 {
        (f64::from(y) * self.line_advance_cycles() + self.frame_phase_cycles(frame)).rem_euclid(1.0)
    }

    /// The PAL V switch of line `y` (+1 even, -1 odd); +1 in NTSC.
    pub fn v_switch(&self, y: u32) -> f64 {
        if self.standard == Standard::Pal && y % 2 == 1 {
            -1.0
        } else {
            1.0
        }
    }

    /// Sigma, in samples, of the Gaussian low-pass of -3 dB bandwidth `bw` MHz; 0 when `bw` is 0.
    pub fn sigma_samples(&self, bw_mhz: f64) -> f64 {
        if bw_mhz <= 0.0 {
            return 0.0;
        }
        gaussian_amp_sigma_ns(bw_mhz) * 1e-3 * self.pixel_clock_mhz * SAMPLES_PER_PX as f64
    }

    /// Length of the notch box, samples: one subcarrier period, S / c.
    pub fn box_len_samples(&self) -> f64 {
        SAMPLES_PER_PX as f64 / self.cycles_per_px()
    }

    /// The channel bandwidth the model applies, MHz: 0 (no channel filter) with S-Video or when
    /// `channel_bandwidth_mhz` is not positive (0 = off); otherwise `channel_bandwidth_mhz` lifted to the
    /// narrowest bandwidth whose kernel sigma is [`CHANNEL_SIGMA_MAX_SAMPLES`] (0.1325 f_px; see there).
    pub fn channel_bandwidth_effective_mhz(&self) -> f64 {
        // Not positive, or NaN: no channel filter.
        if self.connection == Connection::SVideo
            || self.channel_bandwidth_mhz.is_nan()
            || self.channel_bandwidth_mhz <= 0.0
        {
            return 0.0;
        }
        // sigma_samples(B) = k / B, so sigma_samples(B) = CHANNEL_SIGMA_MAX_SAMPLES at B = k / max.
        let floor = self.sigma_samples(1.0) / CHANNEL_SIGMA_MAX_SAMPLES;
        self.channel_bandwidth_mhz.max(floor)
    }

    /// Sigma of the channel low-pass, samples: 0 without a channel filter, at most
    /// [`CHANNEL_SIGMA_MAX_SAMPLES`].
    pub fn channel_sigma_samples(&self) -> f64 {
        self.sigma_samples(self.channel_bandwidth_effective_mhz())
    }

    /// ACC gain of the composite chroma: 1 / |H_ch(f_sc)| = 2^((f_sc / B_ch)^2 / 2) at the effective channel
    /// bandwidth, capped at [`ACC_GAIN_MAX`]; 1 without a channel filter or with S-Video. Always finite.
    pub fn acc_gain(&self) -> f64 {
        let b = self.channel_bandwidth_effective_mhz();
        if b <= 0.0 {
            return 1.0;
        }
        let r = self.subcarrier_mhz / b;
        let exponent = 0.5 * r * r;
        // Compare the exponents: 2^exponent itself overflows f64 for a narrow channel.
        if exponent >= ACC_GAIN_MAX.log2() {
            ACC_GAIN_MAX
        } else {
            exponent.exp2()
        }
    }

    /// Demodulator axis turn a, radians: 33 degrees in NTSC (I/Q axes), 0 in PAL (U/V axes).
    pub fn axis_turn_rad(&self) -> f64 {
        match self.standard {
            Standard::Ntsc => IQ_TURN_DEG.to_radians(),
            Standard::Pal => 0.0,
        }
    }

    /// Sigmas (samples) of the two demodulator low-passes A and B.
    pub fn demod_sigmas(&self) -> (f64, f64) {
        let a = self.sigma_samples(self.i_bandwidth_mhz);
        let b = match self.standard {
            Standard::Ntsc => self.sigma_samples(self.q_bandwidth_mhz),
            Standard::Pal => a,
        };
        (a, b)
    }

    /// Whether line `y` uses the previous line (NTSC comb or PAL delay line).
    pub fn uses_previous_line(&self, y: u32) -> bool {
        y > 0 && self.separation == Separation::Comb
    }

    /// Samples of padding the line simulation needs on each side so that every output pixel of the
    /// active line sees all its inputs: channel reach + half the box + demodulator reach + 2.
    pub fn padding_samples(&self) -> usize {
        let reach = |s: f64| (REACH_SIGMAS * s).ceil();
        let (a, b) = self.demod_sigmas();
        let ch = reach(self.channel_sigma_samples());
        (ch + (0.5 * self.box_len_samples()).ceil() + reach(a.max(b)) + 2.0) as usize
    }

    /// Decodes a frame of drive voltages (`rows[y][x]` = R', G', B'), frame index `frame`: the decoded
    /// R'G'B' per pixel, clamped to [0, 1].
    pub fn decode_frame(&self, rows: &[Vec<[f64; 3]>], frame: u32) -> Vec<Vec<[f64; 3]>> {
        self.decode_frame_yuv(rows, frame)
            .iter()
            .map(|row| row.iter().map(|&yuv| yuv_to_rgb(yuv)).collect())
            .collect()
    }

    /// As [`Composite::decode_frame`], but returns the decoded (Y, U, V) per pixel, saturation applied,
    /// before the conversion to R'G'B' and the clamp.
    pub fn decode_frame_yuv(&self, rows: &[Vec<[f64; 3]>], frame: u32) -> Vec<Vec<[f64; 3]>> {
        (0..rows.len())
            .map(|y| {
                let (luma, uv) = self.decode_line(rows, y as u32, frame);
                let prev = if self.standard == Standard::Pal && self.uses_previous_line(y as u32) {
                    Some(self.decode_line(rows, y as u32 - 1, frame).1)
                } else {
                    None
                };
                luma.iter()
                    .zip(&uv)
                    .enumerate()
                    .map(|(j, (&l, &(u, v)))| {
                        let (u, v) = match &prev {
                            Some(p) => ((u + p[j].0) / 2.0, (v + p[j].1) / 2.0),
                            None => (u, v),
                        };
                        [l, u * self.saturation, v * self.saturation]
                    })
                    .collect()
            })
            .collect()
    }

    /// The encoded signal of line `y` on the padded sample grid (index i is sample i - pad of the active
    /// line): (composite or S-Video C, S-Video Y; empty for composite).
    fn encode_line(&self, row: &[[f64; 3]], y: u32, frame: u32, pad: usize) -> (Vec<f64>, Vec<f64>) {
        let s = SAMPLES_PER_PX as i64;
        let n = row.len() * SAMPLES_PER_PX + 2 * pad;
        let c = self.cycles_per_px();
        let line = self.line_phase_cycles(y, frame);
        let sw = self.v_switch(y);
        let svideo = self.connection == Connection::SVideo;
        let mut sig = vec![0.0; n];
        let mut luma = if svideo { vec![0.0; n] } else { Vec::new() };
        for (i, out) in sig.iter_mut().enumerate() {
            let g = i as i64 - pad as i64;
            let j = g.div_euclid(s);
            if j < 0 || j as usize >= row.len() {
                continue;
            }
            let [yy, u, v] = rgb_to_yuv(row[j as usize]);
            let phi = c * (g as f64 + 0.5) / s as f64 + line;
            let t = 2.0 * PI * phi;
            let chroma = u * t.sin() + sw * v * t.cos();
            if svideo {
                *out = chroma;
                luma[i] = yy;
            } else {
                *out = yy + chroma;
            }
        }
        (sig, luma)
    }

    /// Decodes line `y` (no PAL delay line): per pixel the luma mean and (U, V) before saturation.
    fn decode_line(&self, rows: &[Vec<[f64; 3]>], y: u32, frame: u32) -> (Vec<f64>, Vec<(f64, f64)>) {
        let width = rows[y as usize].len();
        let pad = self.padding_samples();
        let n = width * SAMPLES_PER_PX + 2 * pad;
        let (sig, raw_luma) = self.encode_line(&rows[y as usize], y, frame, pad);
        let svideo = self.connection == Connection::SVideo;
        let (luma, chroma): (Vec<f64>, Vec<f64>) = if svideo {
            (raw_luma, sig)
        } else {
            let sigma_ch = self.channel_sigma_samples();
            let f0 = gaussian_filter(&sig, sigma_ch);
            let len = self.box_len_samples();
            let acc = self.acc_gain();
            let comb = self.standard == Standard::Ntsc && self.uses_previous_line(y);
            if comb {
                let (sig1, _) = self.encode_line(&rows[y as usize - 1], y - 1, frame, pad);
                let f1 = gaussian_filter(&sig1, sigma_ch);
                let b0 = box_filter(&f0, len);
                let b1 = box_filter(&f1, len);
                let mut l = vec![0.0; n];
                let mut ch = vec![0.0; n];
                for i in 0..n {
                    let d = (f0[i] - f1[i]) / 2.0;
                    let c = d - (b0[i] - b1[i]) / 2.0;
                    l[i] = f0[i] - c;
                    ch[i] = acc * c;
                }
                (l, ch)
            } else {
                let b0 = box_filter(&f0, len);
                let ch = f0.iter().zip(&b0).map(|(f, b)| acc * (f - b)).collect();
                (b0, ch)
            }
        };
        // Demodulation.
        let c = self.cycles_per_px();
        let line = self.line_phase_cycles(y, frame);
        let angle = self.axis_turn_rad() + self.hue_deg.to_radians();
        let s = SAMPLES_PER_PX as f64;
        let (ma, mb): (Vec<f64>, Vec<f64>) = (0..n)
            .map(|i| {
                let g = i as f64 - pad as f64;
                let t = 2.0 * PI * (c * (g + 0.5) / s + line) + angle;
                (2.0 * chroma[i] * t.cos(), 2.0 * chroma[i] * t.sin())
            })
            .unzip();
        let (sa, sb) = self.demod_sigmas();
        let turn = self.axis_turn_rad();
        let sw = self.v_switch(y);
        let mut lum = Vec::with_capacity(width);
        let mut uv = Vec::with_capacity(width);
        for j in 0..width {
            let first = pad + j * SAMPLES_PER_PX;
            lum.push(luma[first..first + SAMPLES_PER_PX].iter().sum::<f64>() / s);
            let centre = first as f64 + (s - 1.0) / 2.0;
            let a = gaussian_at(&ma, centre, sa);
            let b = gaussian_at(&mb, centre, sb);
            uv.push(match self.standard {
                // I = a, Q = b; U = -I sin 33 + Q cos 33, V = I cos 33 + Q sin 33.
                Standard::Ntsc => (-a * turn.sin() + b * turn.cos(), a * turn.cos() + b * turn.sin()),
                Standard::Pal => (b, sw * a),
            });
        }
        (lum, uv)
    }
}

/// (Y, U, V) of a drive-voltage triple.
pub fn rgb_to_yuv([r, g, b]: [f64; 3]) -> [f64; 3] {
    let y = LUMA[0] * r + LUMA[1] * g + LUMA[2] * b;
    [y, U_SCALE * (b - y), V_SCALE * (r - y)]
}

/// R'G'B' of (Y, U, V), clamped to [0, 1].
pub fn yuv_to_rgb([y, u, v]: [f64; 3]) -> [f64; 3] {
    let r = y + v / V_SCALE;
    let b = y + u / U_SCALE;
    let g = (y - LUMA[0] * r - LUMA[2] * b) / LUMA[1];
    [r, g, b].map(|x| x.clamp(0.0, 1.0))
}

/// Gaussian of sigma `sigma` samples at integer offsets, truncated at [`REACH_SIGMAS`] sigma, normalised;
/// the identity when sigma is below 1e-3 samples. Samples outside `x` are 0.
pub fn gaussian_filter(x: &[f64], sigma: f64) -> Vec<f64> {
    if sigma < 1e-3 {
        return x.to_vec();
    }
    (0..x.len()).map(|i| gaussian_at(x, i as f64, sigma)).collect()
}

/// The normalised Gaussian mean of `x` around the (fractional) sample position `centre`: weights
/// exp(-(k - centre)^2 / (2 sigma^2)) over |k - centre| <= 4 sigma; `x` at the nearest sample when sigma is
/// below 1e-3. Samples outside `x` are 0 but keep their weight.
pub fn gaussian_at(x: &[f64], centre: f64, sigma: f64) -> f64 {
    let at = |k: i64| {
        if k < 0 || k as usize >= x.len() {
            0.0
        } else {
            x[k as usize]
        }
    };
    if sigma < 1e-3 {
        return at(centre.round() as i64);
    }
    let r = REACH_SIGMAS * sigma;
    let (lo, hi) = ((centre - r).ceil() as i64, (centre + r).floor() as i64);
    let (mut sum, mut wsum) = (0.0, 0.0);
    for k in lo..=hi {
        let d = (k as f64 - centre) / sigma;
        let w = (-0.5 * d * d).exp();
        sum += w * at(k);
        wsum += w;
    }
    sum / wsum
}

/// Box mean of length `len` samples centred on each sample (sample m covers [m - 1/2, m + 1/2], weight =
/// its overlap with [i - len / 2, i + len / 2], divided by `len`). Samples outside `x` are 0.
pub fn box_filter(x: &[f64], len: f64) -> Vec<f64> {
    (0..x.len()).map(|i| box_at(x, i as f64, len)).collect()
}

fn box_at(x: &[f64], centre: f64, len: f64) -> f64 {
    let (lo, hi) = (centre - len / 2.0, centre + len / 2.0);
    let mut sum = 0.0;
    for m in (lo - 0.5).ceil() as i64..=(hi + 0.5).floor() as i64 {
        let w = (m as f64 + 0.5).min(hi) - (m as f64 - 0.5).max(lo);
        if w > 0.0 && m >= 0 && (m as usize) < x.len() {
            sum += w * x[m as usize];
        }
    }
    sum / len
}

#[cfg(test)]
mod tests {
    use super::*;

    const NES_CLOCK_MHZ: f64 = 5.369318; // 21.477272 / 4 [K]; CRT.md: NES/SNES 5.369 MHz.
    const TWO_FSC_MHZ: f64 = 2.0 * NTSC_SUBCARRIER_MHZ; // Apple II / CGA 320 class: 7.15909 MHz [K].

    fn flat(w: usize, h: usize, c: [f64; 3]) -> Vec<Vec<[f64; 3]>> {
        vec![vec![c; w]; h]
    }

    fn max_err(a: &[Vec<[f64; 3]>], b: &[Vec<[f64; 3]>], cols: std::ops::Range<usize>) -> f64 {
        let mut m: f64 = 0.0;
        for (ra, rb) in a.iter().zip(b) {
            for j in cols.clone() {
                for c in 0..3 {
                    m = m.max((ra[j][c] - rb[j][c]).abs());
                }
            }
        }
        m
    }

    fn all_configs(f_px: f64) -> Vec<Composite> {
        let mut v = Vec::new();
        for base in [Composite::ntsc(f_px), Composite::pal(f_px)] {
            for conn in [Connection::Composite, Connection::SVideo] {
                for sep in [Separation::Notch, Separation::Comb] {
                    v.push(Composite {
                        connection: conn,
                        separation: sep,
                        ..base
                    });
                }
            }
        }
        v
    }

    /// The matrices round-trip, and U, V have the BT.470 scale (pure blue U = 0.492111 x 0.886).
    #[test]
    fn yuv_matrices_round_trip() {
        for c in [[1.0, 0.0, 0.0], [0.2, 0.7, 0.4], [0.5, 0.5, 0.5], [0.0, 0.0, 1.0]] {
            let back = yuv_to_rgb(rgb_to_yuv(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 1e-12, "{c:?} -> {back:?}");
            }
        }
        let [y, u, v] = rgb_to_yuv([0.0, 0.0, 1.0]);
        assert!(
            (y - 0.114).abs() < 1e-12 && (u - 0.492111 * 0.886).abs() < 1e-12 && (v + 0.877283 * 0.114).abs() < 1e-12
        );
    }

    /// PLAN acceptance: phase advance per line = 360 x frac(f_sc / f_line); NTSC 525-line timing gives
    /// 180 degrees (227.5 cycles per line), PAL 283.7516 cycles gives 270.6 degrees, the NES (341 dots at
    /// 5.369318 MHz: f_line = 15.7458 kHz, 227.333 cycles) 120 degrees.
    #[test]
    fn phase_advance_per_line_and_frame() {
        let n = Composite::ntsc(NES_CLOCK_MHZ);
        assert!((n.subcarrier_mhz * 1000.0 / n.line_rate_khz - 227.5).abs() < 1e-4);
        assert!((n.line_advance_cycles() * 360.0 - 180.0).abs() < 1.0);
        let p = Composite::pal(NES_CLOCK_MHZ);
        assert!(
            (p.line_advance_cycles() * 360.0 - 0.7516 * 360.0).abs() < 1.0,
            "{}",
            p.line_advance_cycles()
        );
        let nes = Composite {
            line_rate_khz: NES_CLOCK_MHZ * 1000.0 / 341.0,
            ..n
        };
        assert!(
            (nes.line_advance_cycles() * 360.0 - 120.0).abs() < 1.0,
            "{}",
            nes.line_advance_cycles() * 360.0
        );
        // Line phases accumulate; the frame alternation adds 180 degrees (or nothing).
        assert!((n.line_phase_cycles(3, 0) - 0.5).abs() < 1e-3);
        assert!((n.line_phase_cycles(0, 1) - 0.5).abs() < 1e-12);
        assert!((n.line_phase_cycles(0, 2)).abs() < 1e-12);
        let still = Composite {
            frame_advance_deg: 0.0,
            ..n
        };
        assert_eq!(still.line_phase_cycles(5, 7), still.line_phase_cycles(5, 0));
        // NES timing: cycles per pixel 2/3 (f_sc = 6 / 4 x ... : the master clock 21.477272 MHz is 6 f_sc and
        // 4 f_px), so the notch box is exactly 12 samples.
        assert!((nes.cycles_per_px() - 2.0 / 3.0).abs() < 1e-5);
        assert!((nes.box_len_samples() - 12.0).abs() < 1e-3);
    }

    /// PLAN acceptance: a flat colour field returns unchanged away from the line ends (only chroma
    /// within the low-pass reach of the blanking edges differs), for every standard, connection and
    /// separation, on several frames and lines.
    #[test]
    fn flat_colour_round_trips() {
        for f_px in [NES_CLOCK_MHZ, TWO_FSC_MHZ, 6.711647] {
            for cfg in all_configs(f_px) {
                for colour in [[0.8, 0.2, 0.1], [0.1, 0.6, 0.9], [0.5, 0.5, 0.5]] {
                    let rows = flat(64, 4, colour);
                    for frame in [0, 1] {
                        let out = cfg.decode_frame(&rows, frame);
                        let e = max_err(&out, &rows, 24..40);
                        assert!(e < 2e-3, "{cfg:?} {colour:?} f {frame}: max error {e}");
                    }
                }
            }
        }
    }

    /// PLAN acceptance: flat grey keeps its luma within 0.5 % and leaves under 0.5 % residual chroma.
    #[test]
    fn flat_grey_keeps_luma_and_no_chroma() {
        for cfg in all_configs(NES_CLOCK_MHZ) {
            for g in [0.1, 0.5, 0.9] {
                let yuv = cfg.decode_frame_yuv(&flat(64, 3, [g; 3]), 0);
                for row in &yuv {
                    for p in &row[16..48] {
                        assert!((p[0] / g - 1.0).abs() < 0.005, "{cfg:?} grey {g}: Y {}", p[0]);
                        assert!(p[1].hypot(p[2]) < 0.005, "{cfg:?} grey {g}: chroma {:?}", p);
                    }
                }
            }
        }
    }

    /// 10-90 % width of a monotone transition sampled per pixel (linear interpolation of the crossings).
    fn width_10_90(v: &[f64]) -> f64 {
        let (lo, hi) = (v[0], v[v.len() - 1]);
        let at = |level: f64| {
            let t = lo + level * (hi - lo);
            for j in 1..v.len() {
                let (a, b) = (v[j - 1], v[j]);
                if (a - t) * (b - t) <= 0.0 && a != b {
                    return (j - 1) as f64 + (t - a) / (b - a);
                }
            }
            panic!("no crossing");
        };
        (at(0.9) - at(0.1)).abs()
    }

    /// PLAN: a vertical colour edge bleeds over the chroma low-pass. A step of +I to -I (same Y, Q = 0)
    /// and one of +Q to -Q, S-Video NTSC at 14.318 MHz (CGA 640 class [K]): the 10-90 % width is
    /// that of the Gaussian, 0.3396 f_px / B (the amp derivation D1 in tests/stage_amp.rs): 3.740 px for
    /// I at 1.3 MHz, 9.725 px for Q at 0.5 MHz, within 5 % (measured 3.865 px for I: the 2 f_sc ripple
    /// below widens it slightly). The luma stays a sharp step.
    #[test]
    fn colour_edge_bleeds_over_the_chroma_bandwidth() {
        let f_px = 4.0 * NTSC_SUBCARRIER_MHZ;
        let cfg = Composite {
            connection: Connection::SVideo,
            ..Composite::ntsc(f_px)
        };
        let t = IQ_TURN_DEG.to_radians();
        // (U, V) of a unit I vector and of a unit Q vector.
        let i_axis = (-t.sin(), t.cos());
        let q_axis = (t.cos(), t.sin());
        for (axis, bw) in [(i_axis, 1.3), (q_axis, 0.5)] {
            let amp = 0.1;
            let yv = 0.5;
            let colour = |sgn: f64| yuv_to_rgb_unclamped([yv, sgn * amp * axis.0, sgn * amp * axis.1]);
            let w = 160;
            let row: Vec<[f64; 3]> = (0..w).map(|j| colour(if j < w / 2 { 1.0 } else { -1.0 })).collect();
            let yuv = cfg.decode_frame_yuv(&[row], 0);
            let proj: Vec<f64> = yuv[0][40..120].iter().map(|p| p[1] * axis.0 + p[2] * axis.1).collect();
            let width = width_10_90(&proj);
            let hand = 0.339624 * f_px / bw;
            assert!(
                (width / hand - 1.0).abs() < 0.05,
                "bw {bw}: width {width} px, hand {hand}"
            );
            // Pixel by pixel: the Gaussian step a (1 - 2 Phi((x - 80) / sigma)) at the pixel centres x = j + 1/2,
            // sigma = 0.132505 f_px / B px (D1 of tests/stage_amp.rs), plus the demodulator's 2 f_sc product of
            // the step: a step times cos(2 omega x) keeps low-frequency sidebands, which the low-pass leaves
            // as a ripple of at most about 2a phi(0) / (2 omega sigma) at the edge (omega = 2 pi c rad/px):
            // 0.087 x 2a for I, 0.034 x 2a for Q. A real synchronous detector shows it too.
            let sigma = 0.132505 * f_px / bw;
            let omega = 2.0 * PI * cfg.cycles_per_px();
            let ripple = 2.0 * amp * 0.398942 / (2.0 * omega * sigma);
            for (i, v) in proj.iter().enumerate() {
                let x = (40 + i) as f64 + 0.5;
                let phi = 0.5 * (1.0 + crate::beam::erf((x - 80.0) / (sigma * 2.0_f64.sqrt())));
                let want = amp * (1.0 - 2.0 * phi);
                assert!(
                    (v - want).abs() < ripple,
                    "bw {bw} x {x}: {v} vs {want} (ripple {ripple})"
                );
            }
            for p in &yuv[0][40..120] {
                assert!((p[0] - yv).abs() < 1e-9, "luma {p:?}");
            }
        }
    }

    fn yuv_to_rgb_unclamped([y, u, v]: [f64; 3]) -> [f64; 3] {
        let r = y + v / V_SCALE;
        let b = y + u / U_SCALE;
        [r, (y - LUMA[0] * r - LUMA[2] * b) / LUMA[1], b]
    }

    /// Fundamental amplitude of the sampled 1-on-1-off square wave (period 2 px = 2 S samples, on for
    /// the first S samples): 2 |(1 / 2S) sum_{k<S} exp(-i pi k / S)| = 1 / (S sin(pi / 2S)), by hand
    /// (geometric series: |sum| = |1 - e^{-i pi}| / |1 - e^{-i pi / S}| = 2 / (2 sin(pi / 2S))). S = 8:
    /// 0.640729 (the continuous square wave: 2 / pi = 0.636620).
    fn square_fundamental() -> f64 {
        let s = SAMPLES_PER_PX as f64;
        1.0 / (s * (PI / (2.0 * s)).sin())
    }

    /// PLAN acceptance: a 1-px black/white alternation at the subcarrier frequency (f_px = 2 f_sc)
    /// decodes as false colour in composite (magnitude > 10 %) and not in S-Video (< 1 %).
    /// Expected hue, by hand: the chroma left after the notch is the square wave minus its mean; its
    /// fundamental A1 cos(pi x - pi / 2) (peak at the centre of the white pixel 0, x = 1/2) demodulated with
    /// t = pi x + 2 pi phi_l + a gives I = A1 cos(pi / 2 + 2 pi phi_l + a), Q = A1 sin(...), so (U, V) =
    /// A1 (sin(pi / 2 + 2 pi phi_l), cos(pi / 2 + 2 pi phi_l)) = A1 (cos 2 pi phi_l, -sin 2 pi phi_l): on line 0
    /// of frame 0 pure +U (a blue-purple), on line 1 (phi_l = 1/2) pure -U (yellow-green), and frame 1
    /// swaps them (dot crawl at 30 Hz). The decoded luma is the flat mean 0.5: the stripes are gone.
    /// The comb filter cancels it (the luma of adjacent lines is equal).
    #[test]
    fn luma_stripes_at_the_subcarrier_give_cross_colour_at_the_expected_hue() {
        let cfg = Composite::ntsc(TWO_FSC_MHZ);
        assert!((cfg.cycles_per_px() - 0.5).abs() < 1e-12);
        let w = 64;
        let row: Vec<[f64; 3]> = (0..w).map(|j| [if j % 2 == 0 { 1.0 } else { 0.0 }; 3]).collect();
        let rows = vec![row; 4];
        let a1 = square_fundamental();
        assert!((a1 - 0.640729).abs() < 1e-6);
        for frame in [0u32, 1] {
            let yuv = cfg.decode_frame_yuv(&rows, frame);
            for (y, r) in yuv.iter().enumerate() {
                let phl = cfg.line_phase_cycles(y as u32, frame);
                let (eu, ev) = (a1 * (2.0 * PI * phl).cos(), -a1 * (2.0 * PI * phl).sin());
                for p in &r[24..40] {
                    assert!((p[0] - 0.5).abs() < 1e-3, "Y {p:?}");
                    assert!(
                        (p[1] - eu).abs() < 0.01 && (p[2] - ev).abs() < 0.01,
                        "y {y} f {frame}: {p:?} vs ({eu}, {ev})"
                    );
                    assert!(p[1].hypot(p[2]) > 0.1);
                }
            }
            // Line 0 is +U on frame 0 and -U on frame 1.
            let u0 = yuv[0][32][1];
            assert!(if frame == 0 { u0 > 0.6 } else { u0 < -0.6 }, "frame {frame}: U {u0}");
        }
        let sv = Composite {
            connection: Connection::SVideo,
            ..cfg
        };
        let comb = Composite {
            separation: Separation::Comb,
            ..cfg
        };
        for (c, what) in [(sv, "S-Video"), (comb, "comb")] {
            let yuv = c.decode_frame_yuv(&rows, 0);
            for r in &yuv[1..] {
                for p in &r[24..40] {
                    assert!(p[1].hypot(p[2]) < 0.01, "{what}: {p:?}");
                }
            }
        }
    }

    /// Dot crawl: a flat colour through a notch leaves chroma in the luma only where the box is not an
    /// exact period; at the Genesis H40 clock (6.711647 MHz [K], c = 0.5333, L = 15.0 samples) it is
    /// exact; with a non-period box (pixel clock 6.0 MHz, L = 13.41 samples) a residual dot pattern
    /// appears in the luma, moves with the line phase, and stays under 1 % of the chroma amplitude.
    #[test]
    fn notch_residual_dots_follow_the_phase() {
        let cfg = Composite {
            channel_bandwidth_mhz: 0.0,
            ..Composite::ntsc(6.0)
        };
        let rows = flat(64, 2, [0.9, 0.1, 0.1]);
        let y0 = cfg.decode_frame_yuv(&rows, 0);
        let y1 = cfg.decode_frame_yuv(&rows, 1);
        let mean = rgb_to_yuv([0.9, 0.1, 0.1])[0];
        let dev: f64 = y0[0][16..48].iter().map(|p| (p[0] - mean).abs()).fold(0.0, f64::max);
        assert!(dev > 1e-5 && dev < 0.01, "residual {dev}");
        // Frame 1 shifts the subcarrier by half a cycle: the residual flips sign.
        for j in 16..48 {
            assert!(((y0[0][j][0] - mean) + (y1[0][j][0] - mean)).abs() < 1e-9);
        }
    }

    /// Hue and saturation controls. NTSC: a hue error h turns the decoded (U, V) by -h; saturation scales
    /// it. PAL with the delay line: the hue error becomes a saturation loss cos h, the hue is kept.
    #[test]
    fn hue_and_saturation_controls() {
        let colour = [0.7, 0.3, 0.2];
        let [_, u, v] = rgb_to_yuv(colour);
        let rows = flat(64, 3, colour);
        let ntsc = Composite {
            hue_deg: 30.0,
            saturation: 0.5,
            ..Composite::ntsc(NES_CLOCK_MHZ)
        };
        let p = ntsc.decode_frame_yuv(&rows, 0)[1][32];
        let h = 30.0_f64.to_radians();
        // Demodulating at t + h turns the decoded vector by h (sign by convention): check the magnitude and
        // the size of the turn.
        assert!((p[1].hypot(p[2]) - 0.5 * u.hypot(v)).abs() < 1e-3);
        let turn = (p[2].atan2(p[1]) - v.atan2(u) + 3.0 * PI).rem_euclid(2.0 * PI) - PI;
        assert!((turn.abs() - h).abs() < 1e-3, "turn {turn}");
        let pal = Composite {
            hue_deg: 30.0,
            separation: Separation::Comb,
            ..Composite::pal(NES_CLOCK_MHZ)
        };
        let q = pal.decode_frame_yuv(&rows, 0)[1][32];
        assert!(
            (q[1] - u * h.cos()).abs() < 2e-3 && (q[2] - v * h.cos()).abs() < 2e-3,
            "{q:?}"
        );
        // PAL-S keeps the error, alternating per line (Hanover bars).
        let pal_s = Composite {
            separation: Separation::Notch,
            ..pal
        };
        let f = pal_s.decode_frame_yuv(&rows, 0);
        let a0 = f[0][32][2].atan2(f[0][32][1]);
        let a1 = f[1][32][2].atan2(f[1][32][1]);
        assert!(((a0 - a1).abs() - 2.0 * h).abs() < 2e-3, "{a0} {a1}");
        // Saturation 0: grey of the same luma.
        let grey = Composite {
            saturation: 0.0,
            ..Composite::ntsc(NES_CLOCK_MHZ)
        };
        let g = grey.decode_frame(&rows, 0)[1][32];
        let yy = rgb_to_yuv(colour)[0];
        assert!(g.iter().all(|c| (c - yy).abs() < 1e-3), "{g:?}");
    }

    /// ACC: the channel low-pass attenuates the subcarrier by 2^(-(f_sc / B)^2 / 2) (0.7774 at 4.2 MHz);
    /// the gain is its inverse, so a flat colour keeps its saturation (also covered by the round trip).
    #[test]
    fn acc_compensates_the_channel_at_the_subcarrier() {
        let cfg = Composite::ntsc(NES_CLOCK_MHZ);
        let r: f64 = NTSC_SUBCARRIER_MHZ / 4.2;
        assert!((cfg.acc_gain() - 2.0_f64.powf(0.5 * r * r)).abs() < 1e-12);
        assert!((1.0 / cfg.acc_gain() - 0.7774).abs() < 1e-3, "{}", 1.0 / cfg.acc_gain());
        assert_eq!(
            Composite {
                connection: Connection::SVideo,
                ..cfg
            }
            .acc_gain(),
            1.0
        );
    }

    /// Review D-03: a narrow channel never overflows. The ACC gain is finite, capped at ACC_GAIN_MAX and
    /// non-decreasing as the bandwidth narrows; the channel sigma never exceeds its cap; 0 (and any
    /// non-positive or NaN value) means no channel filter on every path; a narrow channel decodes a flat
    /// colour to a finite, desaturated colour of the same luma.
    #[test]
    fn narrow_channel_keeps_the_acc_gain_finite_and_capped() {
        let base = Composite::ntsc(NES_CLOCK_MHZ);
        let floor = base.sigma_samples(1.0) / CHANNEL_SIGMA_MAX_SAMPLES;
        assert!((floor - 0.1325 * NES_CLOCK_MHZ).abs() < 1e-3 * floor, "floor {floor}");
        let mut last = 1.0;
        for bw in [50.0, 8.0, 4.2, 2.0, 1.0, 0.5, 0.22, 0.2, 0.1, 0.05, 1e-6, 1e-300] {
            let cfg = Composite {
                channel_bandwidth_mhz: bw,
                ..base
            };
            let g = cfg.acc_gain();
            assert!(g.is_finite() && (1.0..=ACC_GAIN_MAX).contains(&g), "{bw} MHz: gain {g}");
            assert!(g >= last, "{bw} MHz: gain {g} < {last}");
            last = g;
            let s = cfg.channel_sigma_samples();
            assert!(
                s > 0.0 && s <= CHANNEL_SIGMA_MAX_SAMPLES * (1.0 + 1e-12),
                "{bw} MHz: sigma {s}"
            );
            assert!(
                cfg.padding_samples() < 200,
                "{bw} MHz: padding {}",
                cfg.padding_samples()
            );
        }
        assert_eq!(last, ACC_GAIN_MAX);
        for off in [0.0, -1.0, f64::NAN] {
            let cfg = Composite {
                channel_bandwidth_mhz: off,
                ..base
            };
            assert_eq!(
                (
                    cfg.channel_bandwidth_effective_mhz(),
                    cfg.channel_sigma_samples(),
                    cfg.acc_gain()
                ),
                (0.0, 0.0, 1.0),
                "{off}"
            );
        }
        let colour = [0.8, 0.3, 0.2];
        let rows = flat(64, 2, colour);
        let luma = rgb_to_yuv(colour)[0];
        for bw in [0.5, 0.22, 0.2, 0.1, 0.05] {
            let cfg = Composite {
                channel_bandwidth_mhz: bw,
                ..base
            };
            let out = cfg.decode_frame_yuv(&rows, 0);
            for p in &out[1][16..48] {
                assert!(p.iter().all(|c| c.is_finite()), "{bw} MHz: {p:?}");
                assert!((p[0] - luma).abs() < 0.02, "{bw} MHz: luma {p:?}");
                assert!(p[1].hypot(p[2]) < 0.05, "{bw} MHz: chroma {p:?}");
            }
        }
    }

    /// The box filter: a length of one period nulls a sampled sinusoid; constant fields keep their level.
    #[test]
    fn box_filter_nulls_one_period() {
        let x: Vec<f64> = (0..200)
            .map(|k| (2.0 * PI * (k as f64 + 0.5) / 12.0).sin() + 0.3)
            .collect();
        let b = box_filter(&x, 12.0);
        for v in &b[20..180] {
            assert!((v - 0.3).abs() < 1e-12, "{v}");
        }
        let g = gaussian_filter(&vec![0.7; 50], 2.0);
        assert!((g[25] - 0.7).abs() < 1e-12);
    }
}
