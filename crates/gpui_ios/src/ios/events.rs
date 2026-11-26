//! Conversion from UIKit events to GPUI input events.

use gpui::{
    BackGestureEvent, BackGesturePhase, Modifiers, MouseExitEvent, MouseMoveEvent, Pixels,
    PlatformInput, Point, ScrollDelta, ScrollEvent, ScrollSource, TouchId, TouchPhase, px,
};
use objc2_core_foundation::CGPoint;
use objc2_foundation::NSObjectProtocol;
use objc2_ui_kit::{
    UIGestureRecognizerState, UIKeyModifierFlags, UIPanGestureRecognizer, UIRectEdge,
    UIEvent, UIHoverGestureRecognizer, UIScreenEdgePanGestureRecognizer, UITouch, UITouchPhase, UITouchType, UIView,
};

/// A release faster than this, in points per second toward going back, commits
/// a back gesture regardless of how far it travelled.
const BACK_COMMIT_VELOCITY: f64 = 300.;

/// Converts a step of the screen-edge back gesture, whose `Started` phase was
/// already delivered when the recognizer asked to begin.
///
/// Like `UINavigationController`, a release commits when the gesture is past
/// halfway without flicking back, or when it flicks forward quickly. Returns
/// `None` for states that carry no step.
pub fn back_event(
    recognizer: &UIScreenEdgePanGestureRecognizer,
    view: &UIView,
    start_position: Point<Pixels>,
) -> Option<BackGestureEvent> {
    // Gestures from the right edge move content leftward toward going back.
    let direction = if recognizer.edges() == UIRectEdge::Right {
        -1.
    } else {
        1.
    };
    let width = view.bounds().size.width.max(1.);
    let travel = recognizer.translationInView(Some(view)).x * direction;
    let progress = (travel / width).clamp(0., 1.);
    let phase = match recognizer.state() {
        UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed => {
            BackGesturePhase::Progressed
        }
        UIGestureRecognizerState::Ended => {
            let velocity = recognizer.velocityInView(Some(view)).x * direction;
            if velocity > BACK_COMMIT_VELOCITY
                || (progress > 0.5 && velocity > -BACK_COMMIT_VELOCITY)
            {
                BackGesturePhase::Committed
            } else {
                BackGesturePhase::Cancelled
            }
        }
        UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
            BackGesturePhase::Cancelled
        }
        _ => return None,
    };
    Some(BackGestureEvent {
        phase,
        start_position,
        progress: progress as f32,
    })
}

pub fn touch_location_in_view(touch: &UITouch, view: &UIView) -> Point<Pixels> {
    let location = touch.locationInView(Some(view));
    Point::new(px(location.x as f32), px(location.y as f32))
}

pub fn touch_phase(touch: &UITouch) -> TouchPhase {
    match touch.phase() {
        UITouchPhase::Began => TouchPhase::Started,
        UITouchPhase::Moved | UITouchPhase::Stationary => TouchPhase::Moved,
        UITouchPhase::Ended => TouchPhase::Ended,
        _ => TouchPhase::Cancelled,
    }
}

pub fn touch_id(touch: &UITouch) -> TouchId {
    TouchId(touch as *const UITouch as usize as u64)
}

/// Converts one step of a trackpad or mouse-wheel scroll gesture.
///
/// The recognizer's translation is reset after each step, so every event carries
/// only the movement since the previous one. UIKit reports the translation in
/// the direction the content should move, which matches GPUI's pixel deltas.
/// Returns `None` for states that carry no scroll.
pub fn scroll_event(recognizer: &UIPanGestureRecognizer, view: &UIView) -> Option<ScrollEvent> {
    let touch_phase = match recognizer.state() {
        UIGestureRecognizerState::Began => TouchPhase::Started,
        UIGestureRecognizerState::Changed => TouchPhase::Moved,
        UIGestureRecognizerState::Ended => TouchPhase::Ended,
        UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
            TouchPhase::Cancelled
        }
        _ => return None,
    };
    let translation = recognizer.translationInView(Some(view));
    recognizer.setTranslation_inView(CGPoint::ZERO, Some(view));
    let location = recognizer.locationInView(Some(view));
    Some(ScrollEvent {
        position: Point::new(px(location.x as f32), px(location.y as f32)),
        delta: ScrollDelta::Pixels(Point::new(
            px(translation.x as f32),
            px(translation.y as f32),
        )),
        modifiers: modifiers(recognizer.modifierFlags()),
        touch_phase,
        source: ScrollSource::Pointer,
    })
}

fn modifiers(flags: UIKeyModifierFlags) -> Modifiers {
    Modifiers {
        control: flags.contains(UIKeyModifierFlags::Control),
        alt: flags.contains(UIKeyModifierFlags::Alternate),
        shift: flags.contains(UIKeyModifierFlags::Shift),
        platform: flags.contains(UIKeyModifierFlags::Command),
        function: false,
    }
}

/// A touch's pointer: Apple Pencil is a pen, a pointer device a mouse, else a finger; the contact a
/// circle of UIKit's major radius (none for a pen or pointer, which report none).
pub fn touch_pointer(touch: &UITouch, id: TouchId, view: &UIView) -> gpui::PointerInfo {
    let kind = match touch.r#type() {
        UITouchType::Pencil => gpui::PointerKind::Pen,
        UITouchType::IndirectPointer => gpui::PointerKind::Mouse,
        _ => gpui::PointerKind::Touch,
    };
    let size = if kind == gpui::PointerKind::Touch { (touch.majorRadius() * 2.0) as f32 } else { 0.0 };
    gpui::PointerInfo {
        kind,
        id: id.0,
        width: size,
        height: size,
        pressure: touch_force(touch),
        tilt: touch_tilt(touch, view),
    }
}

/// The pointer behind a hover: a Pencil above the glass (iPadOS 16.1, on an
/// iPad that senses it), with its tilt from iPadOS 16.4, else the trackpad's or
/// mouse's pointer. A hover's moves are GPUI's mouse moves with no button, so a
/// pen's hover is a move whose pointer is a pen.
pub fn hover_pointer(recognizer: &UIHoverGestureRecognizer, view: &UIView) -> gpui::PointerInfo {
    // UIKit gives a pointer's hover no height: only a Pencil's is above 0.
    let pencil = recognizer.respondsToSelector(objc2::sel!(zOffset)) && recognizer.zOffset() > 0.0;
    if !pencil {
        return gpui::PointerInfo { kind: gpui::PointerKind::Mouse, ..Default::default() };
    }
    let tilt = recognizer.respondsToSelector(objc2::sel!(altitudeAngle)).then(|| gpui::PenTilt {
        altitude: recognizer.altitudeAngle() as f32,
        azimuth: recognizer.azimuthAngleInView(Some(view)) as f32,
    });
    gpui::PointerInfo { kind: gpui::PointerKind::Pen, tilt, ..Default::default() }
}

/// A pencil's tilt; none for a finger. UIKit's angles are the DOM's: altitude
/// 0 flat to pi/2 upright, azimuth from +x clockwise in the view.
pub fn touch_tilt(touch: &UITouch, view: &UIView) -> Option<gpui::PenTilt> {
    (touch.r#type() == UITouchType::Pencil).then(|| gpui::PenTilt {
        altitude: touch.altitudeAngle() as f32,
        azimuth: touch.azimuthAngleInView(Some(view)) as f32,
    })
}

/// The samples UIKit read for `touch` since its last delivery, oldest first and
/// `touch` itself last: for a pencil, up to 240 a second against the screen's
/// 60 or 120. Empty when UIKit gives none.
pub fn touch_samples(touch: &UITouch, event: Option<&UIEvent>, view: &UIView) -> Vec<gpui::TouchSample> {
    let Some(touches) = event.and_then(|event| event.coalescedTouchesForTouch(touch)) else {
        return Vec::new();
    };
    touches
        .iter()
        .map(|sample| gpui::TouchSample {
            position: touch_location_in_view(&sample, view),
            force: touch_force(&sample),
            tilt: touch_tilt(&sample, view),
        })
        .collect()
}

pub fn touch_force(touch: &UITouch) -> Option<f32> {
    let maximum_force = touch.maximumPossibleForce();
    if maximum_force <= 0.0 {
        return None;
    }
    Some((touch.force() / maximum_force).clamp(0.0, 1.0) as f32)
}

/// A hover's step for GPUI: while the pointer or a Pencil hovers, a mouse move
/// with no button; once it leaves the view, a mouse exit. None for states that
/// carry no hover.
pub fn hover_input(
    state: UIGestureRecognizerState,
    position: Point<Pixels>,
    modifiers: Modifiers,
) -> Option<PlatformInput> {
    match state {
        UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed => {
            Some(PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }))
        }
        UIGestureRecognizerState::Ended
        | UIGestureRecognizerState::Cancelled
        | UIGestureRecognizerState::Failed => Some(PlatformInput::MouseExited(MouseExitEvent {
            position,
            pressed_button: None,
            modifiers,
        })),
        _ => None,
    }
}

/// The keys held while a hover moves.
pub fn hover_modifiers(recognizer: &UIHoverGestureRecognizer) -> Modifiers {
    modifiers(recognizer.modifierFlags())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hover_moves_with_no_button_and_exits_when_it_ends() {
        let at = Point::new(px(40.), px(120.));
        let shift = Modifiers { shift: true, ..Default::default() };
        for state in [UIGestureRecognizerState::Began, UIGestureRecognizerState::Changed] {
            let Some(PlatformInput::MouseMove(event)) = hover_input(state, at, shift) else {
                panic!("{state:?} is a move");
            };
            assert_eq!(event.position, at);
            assert_eq!(event.pressed_button, None);
            assert!(event.modifiers.shift);
        }
        for state in [
            UIGestureRecognizerState::Ended,
            UIGestureRecognizerState::Cancelled,
            UIGestureRecognizerState::Failed,
        ] {
            let Some(PlatformInput::MouseExited(event)) = hover_input(state, at, shift) else {
                panic!("{state:?} is an exit");
            };
            assert_eq!(event.position, at);
            assert_eq!(event.pressed_button, None);
        }
        assert!(hover_input(UIGestureRecognizerState::Possible, at, shift).is_none());
    }
}
