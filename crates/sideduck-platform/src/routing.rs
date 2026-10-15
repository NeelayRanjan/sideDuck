use std::collections::VecDeque;

use crate::{ContactPhase, TabletSample};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    TabletPoint,
    /// Primary mouse button with a tablet subtype (tip contact).
    MouseDown,
    MouseDragged,
    MouseUp,
    MouseMoved,
    Proximity {
        entering: bool,
        eraser: bool,
    },
}

/// Platform-neutral translation of an AppKit event, also the recorded trace format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawTabletEvent {
    pub kind: EventKind,
    /// Mouse events are accepted only when AppKit reports TabletPoint subtype.
    pub tablet_subtype: bool,
    pub position: [f64; 2],
    pub window_size: [f64; 2],
    pub window_id: i64,
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub timestamp_seconds: f64,
    pub device_id: u64,
    pub buttons: u64,
}

#[derive(Default)]
struct DeviceState {
    id: u64,
    eraser: bool,
    contacting: bool,
    last: Option<TabletSample>,
}

/// Bounded single-pen routing shared by AppKit and mocks.
/// A change of active device terminates the old contact, preventing joined strokes.
pub struct TabletRouter {
    queue: VecDeque<TabletSample>,
    capacity: usize,
    dropped: u64,
    device: DeviceState,
}

impl TabletRouter {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "Tablet queue capacity must be positive");
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            dropped: 0,
            device: DeviceState::default(),
        }
    }

    pub fn push(&mut self, event: RawTabletEvent) {
        let proximity = matches!(event.kind, EventKind::Proximity { .. });
        if event.kind != EventKind::TabletPoint && !proximity && !event.tablet_subtype {
            return;
        }
        if !event.timestamp_seconds.is_finite() || event.timestamp_seconds < 0.0 {
            self.dropped += 1;
            let last_timestamp = self.device.last.map_or(0.0, |s| s.timestamp_seconds);
            self.end_contact(last_timestamp);
            return;
        }
        if self.device.id != event.device_id {
            self.end_contact(event.timestamp_seconds);
            self.device = DeviceState {
                id: event.device_id,
                ..DeviceState::default()
            };
        }
        if let EventKind::Proximity { entering, eraser } = event.kind {
            if entering {
                if eraser != self.device.eraser {
                    self.end_contact(event.timestamp_seconds);
                }
                self.device.eraser = eraser;
            } else {
                self.end_contact(event.timestamp_seconds);
                self.device = DeviceState::default();
            }
            return;
        }
        if !event.position.into_iter().all(f64::is_finite)
            || !event
                .window_size
                .into_iter()
                .all(|v| v.is_finite() && v > 0.0)
            || !event.pressure.is_finite()
            || !event.tilt.into_iter().all(f32::is_finite)
        {
            self.dropped += 1;
            self.end_contact(event.timestamp_seconds);
            return;
        }
        // The UI filters by destination window. Send a release to the previous
        // window before switching, so filtering cannot leave a stale contact.
        if self
            .device
            .last
            .is_some_and(|last| last.window_id != event.window_id)
        {
            self.end_contact(event.timestamp_seconds);
        }
        let pressure = event.pressure.clamp(0.0, 1.0);
        let tilt = event.tilt.map(|v| v.clamp(-1.0, 1.0));
        let contacting = match event.kind {
            EventKind::MouseDown | EventKind::MouseDragged => true,
            EventKind::MouseUp | EventKind::MouseMoved => false,
            _ => pressure > 0.0 || (event.buttons & 1) != 0,
        };
        let phase = match (self.device.contacting, contacting) {
            (false, true) => ContactPhase::Down,
            (true, true) => ContactPhase::Move,
            (true, false) => ContactPhase::Up,
            (false, false) => ContactPhase::Hover,
        };
        let sample = TabletSample {
            position: event.position,
            window_size: event.window_size,
            window_id: event.window_id,
            pressure,
            tilt,
            azimuth: tilt[1].atan2(tilt[0]),
            timestamp_seconds: event.timestamp_seconds,
            device_id: event.device_id,
            phase,
            eraser: self.device.eraser,
            buttons: event.buttons,
        };
        // AppKit can expose a point as both a tablet event and a mouse subtype.
        // Suppress only identical physical samples; never coalesce real samples.
        if let Some(last) = self.device.last {
            let mut same_phase = sample;
            same_phase.phase = last.phase;
            if same_phase == last && self.device.contacting == contacting {
                return;
            }
        }
        self.device.contacting = contacting;
        self.device.last = Some(sample);
        self.enqueue(sample);
    }

    fn end_contact(&mut self, timestamp: f64) {
        if self.device.contacting {
            if let Some(mut last) = self.device.last {
                last.phase = ContactPhase::Up;
                last.pressure = 0.0;
                last.buttons = 0;
                last.timestamp_seconds = timestamp;
                self.device.last = Some(last);
                self.enqueue(last);
            }
            self.device.contacting = false;
        }
    }

    fn enqueue(&mut self, sample: TabletSample) {
        if self.queue.len() == self.capacity {
            self.queue.pop_front();
            self.dropped += 1;
        }
        self.queue.push_back(sample);
    }

    pub fn drain(&mut self) -> Vec<TabletSample> {
        self.queue.drain(..).collect()
    }

    pub fn dropped_samples(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MockTabletSource, TabletSource};

    fn point(timestamp: f64, pressure: f32) -> RawTabletEvent {
        RawTabletEvent {
            kind: EventKind::TabletPoint,
            tablet_subtype: false,
            position: [12.0, 30.0],
            window_size: [400.0, 300.0],
            window_id: 42,
            pressure,
            tilt: [0.25, 0.5],
            timestamp_seconds: timestamp,
            device_id: 7,
            buttons: u64::from(pressure > 0.0),
        }
    }

    #[test]
    fn stylus_only_canvas_rejects_mouse_and_accepts_tablet_mouse_subtype() {
        let mut source = MockTabletSource::new(8);
        let mut event = point(1.0, 0.4);
        event.kind = EventKind::MouseDown;
        source.push_event(event);
        assert!(source.drain_samples().is_empty());
        event.tablet_subtype = true;
        source.push_event(event);
        let samples = source.drain_samples();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].phase, ContactPhase::Down);
        assert_eq!(samples[0].pressure, 0.4);
    }

    #[test]
    fn recorded_sample_streams_replay_identically() {
        let events = [point(1.0, 0.2), point(2.0, 0.8), point(3.0, 0.0)];
        let mut a = MockTabletSource::new(8);
        let mut b = MockTabletSource::new(8);
        a.replay(events);
        b.replay(events);
        let first = a.drain_samples();
        assert_eq!(first, b.drain_samples());
        assert_eq!(
            first.iter().map(|s| s.phase).collect::<Vec<_>>(),
            [ContactPhase::Down, ContactPhase::Move, ContactPhase::Up]
        );
        assert_eq!(first[0].position_top_left(), [12.0, 270.0]);
        assert_eq!(first[0].tilt, [0.25, 0.5]);
        assert_eq!(first[0].azimuth, 0.5_f32.atan2(0.25));
    }

    #[test]
    fn duplicate_tablet_and_mouse_subtype_do_not_duplicate_dabs() {
        let mut router = TabletRouter::new(8);
        let mut event = point(1.0, 0.6);
        router.push(event);
        event.kind = EventKind::MouseDown;
        event.tablet_subtype = true;
        router.push(event);
        router.push(point(2.0, 0.0));
        let mut up = point(2.0, 0.0);
        up.kind = EventKind::MouseUp;
        up.tablet_subtype = true;
        router.push(up);
        assert_eq!(router.drain().len(), 2);
    }

    #[test]
    fn tablet_queue_residency_ceiling_held_under_adversarial_access() {
        let mut router = TabletRouter::new(4);
        for n in 0..10_000 {
            router.push(point(n as f64, 0.5));
            assert!(router.queue.len() <= 4);
            assert_eq!(router.queue.capacity(), 4);
        }
        assert_eq!(router.dropped_samples(), 9996);
        assert_eq!(router.drain().len(), 4);
    }

    #[test]
    fn eraser_proximity_and_device_change_terminate_contact() {
        let mut router = TabletRouter::new(8);
        let mut event = point(0.0, 0.0);
        event.kind = EventKind::Proximity {
            entering: true,
            eraser: true,
        };
        router.push(event);
        router.push(point(1.0, 0.5));
        event = point(2.0, 0.4);
        event.device_id = 8;
        router.push(event);
        let samples = router.drain();
        assert_eq!(samples.len(), 3);
        assert!(samples[0].eraser);
        assert_eq!(samples[1].phase, ContactPhase::Up);
        assert_eq!(samples[2].phase, ContactPhase::Down);
        assert!(!samples[2].eraser);
    }

    #[test]
    fn malformed_samples_rejected_pressure_and_tilt_clamped() {
        let mut router = TabletRouter::new(4);
        router.push(point(f64::NAN, 0.5));
        router.push(point(1.0, f32::INFINITY));
        let mut event = point(2.0, 3.0);
        event.tilt = [-5.0, 8.0];
        router.push(event);
        assert_eq!(router.dropped_samples(), 2);
        let sample = router.drain()[0];
        assert_eq!(sample.pressure, 1.0);
        assert_eq!(sample.tilt, [-1.0, 1.0]);
    }

    #[test]
    fn barrel_button_while_hovering_does_not_begin_a_stroke() {
        let mut router = TabletRouter::new(4);
        let mut event = point(1.0, 0.0);
        event.buttons = 2;
        router.push(event);
        let sample = router.drain()[0];
        assert_eq!(sample.phase, ContactPhase::Hover);
        assert_eq!(sample.buttons, 2);
    }

    #[test]
    fn switching_windows_releases_previous_window_before_starting_new_contact() {
        let mut router = TabletRouter::new(8);
        router.push(point(1.0, 0.5));
        let mut controls = point(2.0, 0.5);
        controls.window_id = 43;
        router.push(controls);
        router.push(point(3.0, 0.5));
        let canvas_samples = router
            .drain()
            .into_iter()
            .filter(|s| s.window_id == 42)
            .map(|s| s.phase)
            .collect::<Vec<_>>();
        assert_eq!(
            canvas_samples,
            [ContactPhase::Down, ContactPhase::Up, ContactPhase::Down]
        );
    }

    #[test]
    fn malformed_samples_in_same_batch_do_not_join_contacts_across_gap() {
        let missing_window = RawTabletEvent {
            window_size: [0.0; 2],
            ..point(2.0, 0.5)
        };
        for invalid in [point(f64::NAN, 0.5), point(2.0, f32::NAN), missing_window] {
            let mut router = TabletRouter::new(8);
            router.push(point(1.0, 0.5));
            router.push(invalid);
            router.push(point(3.0, 0.5));
            let phases = router
                .drain()
                .into_iter()
                .map(|s| s.phase)
                .collect::<Vec<_>>();
            assert_eq!(
                phases,
                [ContactPhase::Down, ContactPhase::Up, ContactPhase::Down]
            );
            assert_eq!(router.dropped_samples(), 1);
        }
    }
}
