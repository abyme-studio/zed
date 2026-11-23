//! A `MotionEvent`'s samples as GPUI's touches and scrolls, apart from the platform so it is tested
//! on any machine (`android/input.rs` sends them).

use gpui::{PenTilt, Point, ScrollDelta, ScrollEvent, ScrollSource, TouchEvent, TouchId, TouchPhase, TouchSample, px};

fn position(x: f32, y: f32, scale: f32) -> Point<gpui::Pixels> {
    Point { x: px(x / scale), y: px(y / scale) }
}

/// One sample of a pointer as `MotionEvent` reads it: where, in device pixels, its pressure (0 when
/// not reported), and a stylus's `AXIS_TILT` and `AXIS_ORIENTATION` in radians (`None` for a finger
/// or mouse).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub tilt: Option<(f32, f32)>,
}

impl Sample {
    pub(crate) fn force(self) -> Option<f32> {
        (self.pressure > 0.0).then_some(self.pressure.clamp(0.0, 1.0))
    }

    /// The DOM's angles from Android's: `AXIS_TILT` is 0 upright to pi/2 flat, so the altitude is
    /// its complement; `AXIS_ORIENTATION` rises clockwise from 0, which Chromium's
    /// `MotionEventAndroid` reads as the pen leaning toward the person (`tiltY` positive), so the
    /// azimuth from +x is a quarter turn on.
    pub(crate) fn tilt(self) -> Option<PenTilt> {
        let (tilt, orientation) = self.tilt?;
        let turn = std::f32::consts::TAU;
        Some(PenTilt {
            altitude: (std::f32::consts::FRAC_PI_2 - tilt).clamp(0.0, std::f32::consts::FRAC_PI_2),
            azimuth: (orientation + std::f32::consts::FRAC_PI_2).rem_euclid(turn),
        })
    }
}

/// The touch GPUI hears for a pointer's `samples` (oldest first, the event's own last): the last is
/// the event, and all of them its coalesced samples when there is more than one.
pub fn touch_event(id: i32, phase: TouchPhase, samples: &[Sample], scale: f32) -> Option<TouchEvent> {
    let last = *samples.last()?;
    let coalesced = if samples.len() > 1 {
        samples
            .iter()
            .map(|s| TouchSample { position: position(s.x, s.y, scale), force: s.force(), tilt: s.tilt() })
            .collect()
    } else {
        Vec::new()
    };
    Some(TouchEvent {
        id: TouchId(id as u64),
        phase,
        position: position(last.x, last.y, scale),
        predicted_position: None,
        force: last.force(),
        tilt: last.tilt(),
        coalesced,
    })
}

/// GPUI's scroll for Android's: lines, vertical positive a notch away from the person as GPUI's
/// (content moves down), horizontal positive to the right, so GPUI's x is its negative (as a
/// browser's `-deltaX`).
pub fn scroll_event(x: f32, y: f32, h: f32, v: f32, modifiers: gpui::Modifiers, scale: f32) -> ScrollEvent {
    ScrollEvent {
        position: position(x, y, scale),
        delta: ScrollDelta::Lines(Point { x: -h, y: v }),
        modifiers,
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32) -> Sample {
        Sample { x, y, pressure: 0.5, tilt: None }
    }

    #[test]
    fn a_move_with_history_carries_every_sample_and_is_its_last() {
        let event = touch_event(3, TouchPhase::Moved, &[at(10.0, 20.0), at(12.0, 24.0), at(16.0, 30.0)], 2.0).unwrap();
        assert_eq!(event.position, Point { x: px(8.0), y: px(15.0) });
        let places: Vec<_> = event.coalesced.iter().map(|s| s.position).collect();
        assert_eq!(places, [Point { x: px(5.0), y: px(10.0) }, Point { x: px(6.0), y: px(12.0) }, Point { x: px(8.0), y: px(15.0) }]);
        assert_eq!(event.force, Some(0.5));
    }

    #[test]
    fn one_sample_has_no_coalesced_ones() {
        let event = touch_event(0, TouchPhase::Started, &[at(1.0, 1.0)], 1.0).unwrap();
        assert!(event.coalesced.is_empty());
        assert!(touch_event(0, TouchPhase::Moved, &[], 1.0).is_none());
    }

    #[test]
    fn a_stylus_tilt_is_the_doms() {
        // Upright: altitude pi/2. Orientation 0 leans toward the person as Chromium reads it: azimuth pi/2.
        let upright = Sample { tilt: Some((0.0, 0.0)), ..at(0.0, 0.0) }.tilt().unwrap();
        assert!((upright.altitude - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert!((upright.azimuth - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        let flat_left = Sample { tilt: Some((std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)), ..at(0.0, 0.0) }.tilt().unwrap();
        assert!(flat_left.altitude.abs() < 1e-6);
        assert!((flat_left.azimuth - std::f32::consts::PI).abs() < 1e-6);
        assert!(at(0.0, 0.0).tilt().is_none());
    }

    #[test]
    fn a_wheel_notch_away_scrolls_up_a_line() {
        let event = scroll_event(0.0, 0.0, 1.0, 1.0, gpui::Modifiers::default(), 1.0);
        assert!(matches!(event.delta, ScrollDelta::Lines(p) if p == Point { x: -1.0, y: 1.0 }));
    }
}
