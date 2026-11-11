/// Output emitted by the touch scrubbing state machine on motion frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrubUpdate {
    /// Normalized value (e.g. 0.01..=1.0 for brightness, 0.0..=1.0 for volume).
    pub value: f64,
    /// True if the finger moved upward relative to the previous frame/anchor.
    pub swiping_up: bool,
}

#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    PendingDeadzone {
        touch_id: i32,
        start_y: f64,
    },
    Scrubbing {
        touch_id: i32,
        anchor_y: f64,
        anchor_val: f64,
        last_y: f64,
        last_val: f64,
    },
}

/// Per-edge vertical touch gesture recognizer implementing:
/// 1. Single-slot tracking (ignores secondary fingers on the same strip).
/// 2. Initial vertical deadzone to reject accidental bezel taps / palm grips.
/// 3. VLC-style relative delta scaling with instant boundary re-anchoring.
#[derive(Debug, Clone)]
pub struct EdgeScrubber {
    deadzone_px: f64,
    min_val: f64,
    max_val: f64,
    state: State,
}

impl EdgeScrubber {
    pub fn new(deadzone_px: f64, min_val: f64, max_val: f64) -> Self {
        let min_v = min_val.min(max_val);
        let max_v = max_val.max(min_val);
        Self {
            deadzone_px: deadzone_px.max(0.0),
            min_val: min_v,
            max_val: max_v,
            state: State::Idle,
        }
    }

    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    pub fn on_touch_down(&mut self, touch_id: i32, y: f64) {
        if matches!(self.state, State::Idle) {
            self.state = State::PendingDeadzone {
                touch_id,
                start_y: y,
            };
        }
    }

    pub fn on_touch_motion<F>(
        &mut self,
        touch_id: i32,
        y: f64,
        sweep_px: f64,
        read_initial: F,
    ) -> Option<ScrubUpdate>
    where
        F: FnOnce() -> f64,
    {
        let sweep = sweep_px.max(20.0);

        match self.state {
            State::Idle => None,
            State::PendingDeadzone {
                touch_id: active_id,
                start_y,
            } => {
                if touch_id != active_id {
                    return None;
                }
                let dy = y - start_y;
                if dy.abs() < self.deadzone_px {
                    return None;
                }

                // Anchor right at the deadzone boundary so crossing the threshold
                // transitions seamlessly without an abrupt jump.
                let excess_anchor_y = start_y + dy.signum() * self.deadzone_px;
                let init = read_initial().clamp(self.min_val, self.max_val);

                let raw = init + (excess_anchor_y - y) / sweep;
                let clamped = raw.clamp(self.min_val, self.max_val);
                let swiping_up = y < excess_anchor_y;

                let (anchor_y, anchor_val) = if (raw - clamped).abs() > f64::EPSILON {
                    (y, clamped)
                } else {
                    (excess_anchor_y, init)
                };

                self.state = State::Scrubbing {
                    touch_id,
                    anchor_y,
                    anchor_val,
                    last_y: y,
                    last_val: clamped,
                };

                Some(ScrubUpdate {
                    value: clamped,
                    swiping_up,
                })
            }
            State::Scrubbing {
                touch_id: active_id,
                mut anchor_y,
                mut anchor_val,
                last_y,
                last_val,
            } => {
                if touch_id != active_id {
                    return None;
                }

                let raw = anchor_val + (anchor_y - y) / sweep;
                let clamped = raw.clamp(self.min_val, self.max_val);
                let swiping_up = if (y - last_y).abs() > 0.01 {
                    y < last_y
                } else {
                    clamped > last_val
                };

                // Boundary re-anchoring: if the finger drags past min or max,
                // keep the anchor pinned to the finger so reversing direction
                // responds on the very first pixel of return motion.
                if raw > self.max_val || raw < self.min_val {
                    anchor_y = y;
                    anchor_val = clamped;
                }

                self.state = State::Scrubbing {
                    touch_id,
                    anchor_y,
                    anchor_val,
                    last_y: y,
                    last_val: clamped,
                };

                Some(ScrubUpdate {
                    value: clamped,
                    swiping_up,
                })
            }
        }
    }

    pub fn on_touch_up(&mut self, touch_id: i32) -> bool {
        let matched = match self.state {
            State::Idle => false,
            State::PendingDeadzone {
                touch_id: active_id,
                ..
            }
            | State::Scrubbing {
                touch_id: active_id,
                ..
            } => active_id == touch_id,
        };
        if matched {
            self.state = State::Idle;
        }
        matched
    }
}

/// Maps perceptual brightness `p` in `[min_norm, 1.0]` onto raw hardware units `[raw_min, max_raw]`
/// using a power-law gamma curve (`gamma = 2.2` by default).
pub fn perceptual_to_raw(p: f64, max_raw: u32, min_percent: f64, gamma: f64) -> u32 {
    if max_raw == 0 {
        return 0;
    }
    if min_percent <= 0.0 && p <= 0.0 {
        return 0;
    }

    let p_min = (min_percent / 100.0).clamp(0.0, 0.5);
    let p_clamped = p.clamp(p_min, 1.0);

    // Hardware safety floor (~0.25% PWM at min_percent = 1%) keeps eDP LED drivers lit.
    let raw_min = if min_percent > 0.0 {
        ((max_raw as f64) * p_min * 0.25).round().max(1.0) as u32
    } else {
        0
    }
    .min(max_raw);

    if (1.0 - p_min).abs() < f64::EPSILON || raw_min >= max_raw {
        return max_raw;
    }

    let t = ((p_clamped - p_min) / (1.0 - p_min)).clamp(0.0, 1.0);
    let curved = t.powf(gamma);
    let raw = (raw_min as f64) + ((max_raw - raw_min) as f64) * curved;
    (raw.round() as u32).clamp(raw_min, max_raw)
}

/// Inverse of [`perceptual_to_raw`]: converts a raw sysfs brightness reading back into
/// perceptual brightness `[min_norm, 1.0]`.
pub fn raw_to_perceptual(raw: u32, max_raw: u32, min_percent: f64, gamma: f64) -> f64 {
    if max_raw == 0 {
        return 1.0;
    }
    let p_min = (min_percent / 100.0).clamp(0.0, 0.5);
    let raw_min = if min_percent > 0.0 {
        ((max_raw as f64) * p_min * 0.25).round().max(1.0) as u32
    } else {
        0
    }
    .min(max_raw);

    if raw <= raw_min {
        return p_min;
    }
    if raw >= max_raw || raw_min >= max_raw {
        return 1.0;
    }

    let frac = ((raw - raw_min) as f64) / ((max_raw - raw_min) as f64);
    (p_min + (1.0 - p_min) * frac.powf(1.0 / gamma)).clamp(p_min, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_motion_inside_deadzone() {
        let mut s = EdgeScrubber::new(10.0, 0.01, 1.0);
        s.on_touch_down(0, 300.0);
        assert!(s.on_touch_motion(0, 292.0, 400.0, || 0.5).is_none());
        assert!(s.on_touch_motion(0, 309.5, 400.0, || 0.5).is_none());
        assert!(s.on_touch_up(0));
        assert!(!s.is_active());
    }

    #[test]
    fn scrubs_smoothly_past_deadzone_and_ignores_second_finger() {
        let mut s = EdgeScrubber::new(10.0, 0.0, 1.0);
        s.on_touch_down(1, 300.0);
        // Move 50px up (10px deadzone + 40px effective scrub over 400px = +0.10)
        let u = s
            .on_touch_motion(1, 250.0, 400.0, || 0.40)
            .expect("should enter scrubbing");
        assert!((u.value - 0.50).abs() < 1e-6);
        assert!(u.swiping_up);

        // Second finger on same strip is ignored
        assert!(s.on_touch_motion(2, 100.0, 400.0, || 0.0).is_none());
        assert!(!s.on_touch_up(2));
        assert!(s.is_active());

        assert!(s.on_touch_up(1));
        assert!(!s.is_active());
    }

    #[test]
    fn reanchors_immediately_at_upper_and_lower_bounds() {
        let mut s = EdgeScrubber::new(10.0, 0.01, 1.0);
        s.on_touch_down(0, 500.0);
        // Drag far past 100% (initial 0.90, 210px up -> 200px effective / 400px = +0.50 -> clamped 1.0)
        let u1 = s.on_touch_motion(0, 290.0, 400.0, || 0.90).unwrap();
        assert!((u1.value - 1.0).abs() < 1e-6);

        // Dragging down 40px from y=290 to y=330 must immediately drop from 1.0 to 0.90 (no dead travel!)
        let u2 = s.on_touch_motion(0, 330.0, 400.0, || 0.90).unwrap();
        assert!((u2.value - 0.90).abs() < 1e-6);
        assert!(!u2.swiping_up);
    }

    #[test]
    fn perceptual_curve_roundtrip_on_framework_intel_backlight() {
        let max_raw = 96_000;
        for pct in [1, 2, 5, 10, 25, 50, 75, 100] {
            let p = (pct as f64) / 100.0;
            let raw = perceptual_to_raw(p, max_raw, 1.0, 2.2);
            assert!(raw >= 240 && raw <= max_raw, "raw={raw} for pct={pct}");
            let back = raw_to_perceptual(raw, max_raw, 1.0, 2.2);
            assert!(
                (back - p).abs() < 0.005,
                "roundtrip mismatch: p={p}, raw={raw}, back={back}"
            );
        }
    }
}
