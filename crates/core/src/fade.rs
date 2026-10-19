//! Gain ramps and crossfades.

/// The shape of a ramp from one gain to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Curve {
    /// Straight line. Audible as a slight dip in the middle of a crossfade.
    Linear,
    /// Sine quarter-wave. Paired with its cosine, the two sum to constant
    /// power, which is what a crossfade between unrelated material wants.
    #[default]
    EqualPower,
    /// Smoothstep: gentle at both ends.
    Smooth,
    /// Slow start, fast finish. Reads as natural for fade-outs.
    Exponential,
}

impl Curve {
    /// Map progress `t` in `0..=1` to a gain in `0..=1`.
    #[must_use]
    pub fn shape(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            Self::Smooth => t * t * (3.0 - 2.0 * t),
            Self::Exponential => t * t,
        }
    }

    /// The complementary gain for the outgoing side of a crossfade, so that
    /// `shape(t)` and `complement(t)` make a sensible pair.
    #[must_use]
    pub fn complement(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::EqualPower => (t * std::f32::consts::FRAC_PI_2).cos(),
            other => 1.0 - other.shape(t),
        }
    }
}

/// A gain that moves toward a target over a fixed number of frames.
#[derive(Debug, Clone)]
pub struct Fader {
    start: f32,
    target: f32,
    curve: Curve,
    total: u32,
    elapsed: u32,
}

impl Fader {
    /// A fader sitting at `gain`.
    #[must_use]
    pub fn at(gain: f32) -> Self {
        Self {
            start: gain,
            target: gain,
            curve: Curve::Linear,
            total: 0,
            elapsed: 0,
        }
    }

    /// Start ramping to `target` over `frames`. Zero frames jumps at once.
    pub fn ramp_to(&mut self, target: f32, frames: u32, curve: Curve) {
        self.start = self.gain();
        self.target = target;
        self.curve = curve;
        self.total = frames;
        self.elapsed = 0;
    }

    /// Jump to `gain` immediately.
    pub fn set(&mut self, gain: f32) {
        self.ramp_to(gain, 0, Curve::Linear);
    }

    /// The gain at the current frame without advancing.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn gain(&self) -> f32 {
        if self.elapsed >= self.total {
            return self.target;
        }
        let t = self.elapsed as f32 / self.total as f32;
        let shaped = if self.target >= self.start {
            self.curve.shape(t)
        } else {
            1.0 - self.curve.complement(t)
        };
        self.start + (self.target - self.start) * shaped
    }

    /// The gain for this frame, then advance one frame.
    pub fn advance(&mut self) -> f32 {
        let g = self.gain();
        if self.elapsed < self.total {
            self.elapsed += 1;
        }
        g
    }

    /// Where the ramp is headed.
    #[must_use]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// True once the ramp has finished.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.elapsed >= self.total
    }
}

/// Progress through a crossfade between an outgoing and an incoming stream.
#[derive(Debug, Clone)]
pub struct Crossfade {
    curve: Curve,
    total: u32,
    elapsed: u32,
}

impl Crossfade {
    /// A crossfade lasting `frames`.
    #[must_use]
    pub fn new(frames: u32, curve: Curve) -> Self {
        Self {
            curve,
            total: frames.max(1),
            elapsed: 0,
        }
    }

    /// `(outgoing, incoming)` gains for this frame, then advance.
    #[allow(clippy::cast_precision_loss)]
    pub fn advance(&mut self) -> (f32, f32) {
        let t = self.elapsed as f32 / self.total as f32;
        if self.elapsed < self.total {
            self.elapsed += 1;
        }
        (self.curve.complement(t), self.curve.shape(t))
    }

    /// True once the incoming stream is at full gain.
    #[must_use]
    pub fn done(&self) -> bool {
        self.elapsed >= self.total
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn curves_span_zero_to_one() {
        for c in [
            Curve::Linear,
            Curve::EqualPower,
            Curve::Smooth,
            Curve::Exponential,
        ] {
            assert!(close(c.shape(0.0), 0.0), "{c:?} at 0");
            assert!(close(c.shape(1.0), 1.0), "{c:?} at 1");
            assert!(close(c.complement(0.0), 1.0), "{c:?} comp at 0");
            assert!(close(c.complement(1.0), 0.0), "{c:?} comp at 1");
        }
    }

    #[test]
    fn equal_power_sums_to_constant_power() {
        let c = Curve::EqualPower;
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let p = c.shape(t).powi(2) + c.complement(t).powi(2);
            assert!(close(p, 1.0), "power {p} at {t}");
        }
    }

    #[test]
    fn fader_reaches_target_after_frames() {
        let mut f = Fader::at(1.0);
        f.ramp_to(0.0, 10, Curve::Linear);
        let mut last = 1.0;
        for _ in 0..10 {
            last = f.advance();
        }
        assert!(last > 0.0 && last < 0.2, "last frame of ramp {last}");
        assert!(close(f.advance(), 0.0));
        assert!(f.settled());
    }

    #[test]
    fn fader_retargets_from_current_gain() {
        let mut f = Fader::at(0.0);
        f.ramp_to(1.0, 10, Curve::Linear);
        for _ in 0..5 {
            f.advance();
        }
        let mid = f.gain();
        f.ramp_to(0.0, 5, Curve::Linear);
        assert!(close(f.gain(), mid));
    }

    #[test]
    fn crossfade_ends_with_incoming_at_unity() {
        let mut x = Crossfade::new(4, Curve::Linear);
        let mut pair = (1.0, 0.0);
        for _ in 0..5 {
            pair = x.advance();
        }
        assert!(close(pair.0, 0.0) && close(pair.1, 1.0));
        assert!(x.done());
    }
}
