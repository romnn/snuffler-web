//! The easing primitives behind every map motion.

use std::ops::RangeInclusive;

/// Longest spring step, in seconds.
///
/// A backgrounded tab hands back one enormous frame delta on return, and stepping the
/// springs by it would teleport the camera.
const MAX_STEP_SECONDS: f64 = 1.0 / 15.0;

/// A critically damped spring.
///
/// Critically damped is the whole point: it eases *in* from rest and *out* into
/// its target without ever overshooting, and re-aiming it mid-flight carries the
/// current velocity over instead of restarting, which is what a wheel does when
/// it fires three notches in as many frames.
///
/// The step is the closed-form solution, so it is exact and stable at any frame
/// rate rather than an Euler integration that blows up on a long frame.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    value: f64,
    velocity: f64,
    target: f64,
    omega: f64,
}

impl Spring {
    /// A spring parked at `value`, converging at `omega` rad/s.
    #[must_use]
    pub fn new(value: f64, omega: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            omega,
        }
    }

    /// Where the spring is right now — what a frame draws.
    #[must_use]
    pub fn value(self) -> f64 {
        self.value
    }

    /// Where it is heading — what the next input should be applied to.
    #[must_use]
    pub fn target(self) -> f64 {
        self.target
    }

    /// Re-aims the spring without disturbing its current value or velocity.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
    }

    /// Teleports the spring: value, target, and velocity all agree at `value`.
    pub fn snap_to(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Confines the current value to `limits`, killing the velocity that was
    /// pushing past the wall so the spring rests against it instead of shoving.
    pub fn clamp_value(&mut self, limits: RangeInclusive<f64>) {
        if !limits.contains(&self.value) {
            self.value = self.value.clamp(*limits.start(), *limits.end());
            self.velocity = 0.0;
        }
    }

    /// Confines the target to `limits`, leaving the value and velocity alone.
    ///
    /// Separate from [`Spring::clamp_value`] because the two are not always
    /// bounded by the same thing: where a camera may *be* now and where it may
    /// *arrive* differ whenever the scale is still moving under it.
    pub fn clamp_target(&mut self, limits: RangeInclusive<f64>) {
        self.target = self.target.clamp(*limits.start(), *limits.end());
    }

    /// Confines value and target alike, for a limit that does not depend on the
    /// spring's own motion.
    pub fn clamp_to(&mut self, limits: RangeInclusive<f64>) {
        self.clamp_value(limits.clone());
        self.clamp_target(limits);
    }

    /// Whether the spring has come to rest within `epsilon` of its target.
    #[must_use]
    pub fn is_settled(self, epsilon: f64) -> bool {
        (self.value - self.target).abs() <= epsilon && self.velocity.abs() <= epsilon * self.omega
    }

    /// Advances by `dt` seconds, returning whether the spring is still moving (and
    /// the frame therefore needs another repaint).
    pub fn advance(&mut self, dt: f64, epsilon: f64) -> bool {
        if self.is_settled(epsilon) {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let dt = dt.clamp(0.0, MAX_STEP_SECONDS);
        let offset = self.value - self.target;
        let decay = (-self.omega * dt).exp();
        // x(t) = (x0 + (v0 + omega*x0)*t) * e^-omega*t, for x measured from target.
        let slope = self.omega.mul_add(offset, self.velocity);
        self.value = self.target + slope.mul_add(dt, offset) * decay;
        self.velocity = self.omega.mul_add(-slope * dt, self.velocity) * decay;
        true
    }
}

/// Cubic ease-out over `0..=1`, saturating outside it.
///
/// The spring above is for anything chasing a target that can move; this is for
/// the fixed-length fades — a tile arriving, a window revealing itself — where
/// the duration is known up front and there is nothing to re-aim at.
#[must_use]
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, bail, ensure};

    use super::Spring;

    const FRAME: f64 = 1.0 / 60.0;

    /// Ten seconds of frames: far longer than any glide here, so running out of
    /// them means the spring is not converging rather than merely being slow.
    const GIVE_UP_AFTER: usize = 600;

    #[test]
    fn a_spring_eases_in_then_out_without_overshooting() -> eyre::Result<()> {
        let mut spring = Spring::new(0.0, 16.0);
        spring.set_target(1.0);

        let mut steps = Vec::new();
        let mut previous = 0.0;
        let mut settled_after = None;
        for frame in 0..GIVE_UP_AFTER {
            let moving = spring.advance(FRAME, 1e-4);
            ensure!(spring.value() <= 1.0, "overshot to {}", spring.value());
            ensure!(spring.value() >= previous, "reversed at frame {frame}");
            steps.push(spring.value() - previous);
            previous = spring.value();
            if !moving {
                settled_after = Some(frame);
                break;
            }
        }
        let Some(frames) = settled_after else {
            bail!("the spring never settled")
        };
        ensure!(
            (20..90).contains(&frames),
            "settled after {frames} frames, wanted a glide of roughly a third of a second"
        );

        // Ease in, then out: starting from rest the step has to grow for a few
        // frames before it decays, which is exactly what a linear tween does not do.
        let peak = steps
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map_or(0, |(frame, _)| frame);
        ensure!(
            (1..8).contains(&peak),
            "the fastest frame was {peak}, so the glide did not accelerate out of rest"
        );
        // The final frame is the sub-epsilon snap onto the target, not a step.
        let glide = steps.get(..steps.len() - 1).unwrap_or_default();
        ensure!(
            glide.iter().skip(peak).is_sorted_by(|a, b| a >= b),
            "the glide sped up again after its peak instead of easing out"
        );
        Ok(())
    }
}
