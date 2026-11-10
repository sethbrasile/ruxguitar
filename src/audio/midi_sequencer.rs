use crate::audio::midi_event::MidiEvent;
const QUARTER_TICKS: u64 = 960;

pub struct MidiSequencer {
    current_tick: u32,             // current Midi tick
    last_tick: u32,                // last Midi tick
    tick_fraction: u64,            // fractional tick numerator at the output sample rate
    sorted_events: Vec<MidiEvent>, // sorted Midi events
    next_event: usize,             // first event not yet delivered
}

impl MidiSequencer {
    pub fn new(sorted_events: Vec<MidiEvent>) -> Self {
        // events are sorted by tick
        assert!(sorted_events
            .as_slice()
            .windows(2)
            .all(|w| w[0].tick <= w[1].tick));
        Self {
            current_tick: 0,
            last_tick: 0,
            tick_fraction: 0,
            sorted_events,
            next_event: 0,
        }
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn events(&self) -> &[MidiEvent] {
        &self.sorted_events
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn set_tick(&mut self, tick: u32) {
        self.last_tick = tick;
        self.current_tick = tick;
        self.tick_fraction = 0;
        self.next_event = self
            .sorted_events
            .partition_point(|event| event.tick < tick);
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn reset_ticks(&mut self) {
        self.current_tick = 0;
        self.last_tick = 0;
        self.tick_fraction = 0;
        self.next_event = 0;
    }

    pub const fn get_tick(&self) -> u32 {
        self.current_tick
    }

    pub const fn get_last_tick(&self) -> u32 {
        self.last_tick
    }

    pub fn get_next_events(&mut self) -> Option<&[MidiEvent]> {
        if self.next_event == self.sorted_events.len() {
            return None;
        }
        let end = self.next_event
            + self.sorted_events[self.next_event..]
                .partition_point(|event| event.tick <= self.current_tick);
        let start = self.next_event;
        self.next_event = end;
        Some(&self.sorted_events[start..end])
    }

    /// Advance by audio actually rendered, so an idle or delayed callback
    /// cannot rush a cluster of notes to catch up with wall-clock time.
    pub fn advance_frames(&mut self, tempo: u32, frames: usize, sample_rate: u32) {
        assert!(sample_rate > 0);
        let denominator = u64::from(sample_rate) * 60;
        let numerator = frames as u128 * u128::from(tempo) * u128::from(QUARTER_TICKS)
            + u128::from(self.tick_fraction);
        let delta = numerator / u128::from(denominator);
        self.last_tick = self.current_tick;
        self.current_tick = self
            .current_tick
            .saturating_add(delta.min(u128::from(u32::MAX)) as u32);
        self.tick_fraction = (numerator % u128::from(denominator)) as u64;
    }

    #[cfg(test)]
    #[allow(clippy::missing_const_for_fn)]
    pub fn advance_tick(&mut self, tick: u32) {
        self.last_tick = self.current_tick;
        self.current_tick += tick;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::midi_builder::MidiBuilder;
    use crate::parser::song_parser_tests::parse_gp_file;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn frame_clock_keeps_triplet_spacing_after_idle() {
        let mut sequencer = MidiSequencer::new(Vec::new());
        sequencer.set_tick(960);
        std::thread::sleep(Duration::from_millis(100));
        // At 120 BPM and 48 kHz, 4,000 stereo frames are exactly 160 ticks.
        for expected in [1120, 1280, 1440, 1600] {
            sequencer.advance_frames(120, 4_000, 48_000);
            assert_eq!(sequencer.get_tick(), expected);
        }
    }

    #[test]
    fn frame_clock_carries_subtick_fractions_between_callbacks() {
        let mut sequencer = MidiSequencer::new(Vec::new());
        for _ in 0..46 {
            sequencer.advance_frames(120, 256, 48_000);
        }
        sequencer.advance_frames(120, 224, 48_000);
        assert_eq!(sequencer.get_tick(), 480);
        sequencer.set_tick(960);
        sequencer.advance_frames(120, 4_000, 48_000);
        assert_eq!(sequencer.get_tick(), 1120);
    }

    #[test]
    fn delivers_tick_zero_simultaneous_events_and_exact_seek_once() {
        let events = vec![
            MidiEvent::new_note_on(0, 0, 45, 95, 0),
            MidiEvent::new_note_on(0, 0, 45, 95, 1),
            MidiEvent::new_note_off(160, 0, 45, 0),
            MidiEvent::new_note_on(160, 0, 46, 95, 0),
            MidiEvent::new_note_off(320, 0, 46, 0),
        ];
        let mut sequencer = MidiSequencer::new(events.clone());
        assert_eq!(sequencer.get_next_events().unwrap(), &events[..2]);
        assert!(sequencer.get_next_events().unwrap().is_empty());
        sequencer.advance_tick(160);
        assert_eq!(sequencer.get_next_events().unwrap(), &events[2..4]);
        assert!(sequencer.get_next_events().unwrap().is_empty());
        sequencer.set_tick(160);
        assert_eq!(sequencer.get_next_events().unwrap(), &events[2..4]);
        sequencer.advance_tick(160);
        assert_eq!(sequencer.get_next_events().unwrap(), &events[4..]);
        assert!(sequencer.get_next_events().is_none());
    }
    #[test]
    fn test_sequence_demo_song() {
        const FILE_PATH: &str = "test-files/Demo v5.gp5";
        let song = parse_gp_file(FILE_PATH).unwrap();
        let song = Arc::new(song);
        let builder = MidiBuilder::new();
        let events = builder.build_for_song(&song);
        let events_len = 4471;
        assert_eq!(events.len(), events_len);
        assert_eq!(events[0].tick, 1);
        assert_eq!(events.iter().last().unwrap().tick, 189_120);
        let mut sequencer = MidiSequencer::new(events.clone());

        // last_tick:0 current_tick:0
        let batch = sequencer.get_next_events().unwrap();
        assert_eq!(batch.len(), 0);

        // advance time by 1 tick
        sequencer.advance_tick(1);

        // last_tick:0 current_tick:1
        let batch = sequencer.get_next_events().unwrap();
        let count_1 = batch.len();
        assert_eq!(&events[0..count_1], batch);
        assert!(batch.iter().all(MidiEvent::is_midi_message));

        let mut pos = count_1;
        loop {
            let prev_tick = sequencer.get_tick();
            // advance time by 112 tick
            sequencer.advance_tick(112);
            let next_tick = sequencer.get_tick();
            assert_eq!(next_tick - prev_tick, 112);

            if let Some(batch) = sequencer.get_next_events() {
                let count = batch.len();
                assert_eq!(&events[pos..pos + count], batch);
                pos += count;
            } else {
                break;
            }
        }
        assert_eq!(pos, events.len());
    }
}
