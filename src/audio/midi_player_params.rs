use std::collections::HashSet;

/// Hold values changed during playback of a MIDI events.
pub struct MidiPlayerParams {
    tempo: u32,
    tempo_percentage: u32,
    solo_track_id: Option<usize>,
    muted_tracks: HashSet<usize>,
}

impl MidiPlayerParams {
    pub fn new(tempo: u32, tempo_percentage: u32, solo_track_id: Option<usize>) -> Self {
        Self {
            tempo,
            tempo_percentage,
            solo_track_id,
            muted_tracks: HashSet::new(),
        }
    }

    pub const fn solo_track_id(&self) -> Option<usize> {
        self.solo_track_id
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn set_solo_track_id(&mut self, solo_track_id: Option<usize>) {
        self.solo_track_id = solo_track_id;
    }

    pub fn adjusted_tempo(&self) -> u32 {
        (self.tempo as f32 * self.tempo_percentage as f32 / 100.0) as u32
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn set_tempo(&mut self, tempo: u32) {
        self.tempo = tempo;
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn set_tempo_percentage(&mut self, tempo_percentage: u32) {
        self.tempo_percentage = tempo_percentage;
    }

    /// Set mute state for a specific track.
    pub fn set_track_muted(&mut self, track_id: usize, muted: bool) {
        if muted {
            self.muted_tracks.insert(track_id);
        } else {
            self.muted_tracks.remove(&track_id);
        }
    }

    /// Check if a track is muted.
    pub fn is_track_muted(&self, track_id: usize) -> bool {
        self.muted_tracks.contains(&track_id)
    }

    /// Check if a track should be audible based on solo and mute state.
    ///
    /// Solo takes precedence over mute:
    /// - If a track is soloed, only that track is audible
    /// - Otherwise, muted tracks are inaudible
    pub fn is_track_audible(&self, track_id: usize) -> bool {
        if let Some(solo_id) = self.solo_track_id {
            return track_id == solo_id;
        }
        !self.muted_tracks.contains(&track_id)
    }

    /// Clear all muted tracks.
    pub fn clear_mutes(&mut self) {
        self.muted_tracks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_default_state() {
        let params = MidiPlayerParams::new(120, 100, None);
        assert_eq!(params.solo_track_id(), None);
        assert!(!params.is_track_muted(0));
        assert!(!params.is_track_muted(1));
    }

    #[test]
    fn test_new_with_solo_track() {
        let params = MidiPlayerParams::new(120, 100, Some(2));
        assert_eq!(params.solo_track_id(), Some(2));
    }

    #[test]
    fn test_set_solo_track_id() {
        let mut params = MidiPlayerParams::new(120, 100, None);
        params.set_solo_track_id(Some(1));
        assert_eq!(params.solo_track_id(), Some(1));

        params.set_solo_track_id(None);
        assert_eq!(params.solo_track_id(), None);
    }

    #[test]
    fn test_adjusted_tempo_at_100_percent() {
        let params = MidiPlayerParams::new(120, 100, None);
        assert_eq!(params.adjusted_tempo(), 120);
    }

    #[test]
    fn test_adjusted_tempo_at_50_percent() {
        let params = MidiPlayerParams::new(120, 50, None);
        assert_eq!(params.adjusted_tempo(), 60);
    }

    #[test]
    fn test_adjusted_tempo_at_150_percent() {
        let params = MidiPlayerParams::new(120, 150, None);
        assert_eq!(params.adjusted_tempo(), 180);
    }

    #[test]
    fn test_set_tempo() {
        let mut params = MidiPlayerParams::new(120, 100, None);
        params.set_tempo(140);
        assert_eq!(params.adjusted_tempo(), 140);
    }

    #[test]
    fn test_set_tempo_percentage() {
        let mut params = MidiPlayerParams::new(120, 100, None);
        params.set_tempo_percentage(75);
        assert_eq!(params.adjusted_tempo(), 90);
    }

    #[test]
    fn test_track_mute_unmute() {
        let mut params = MidiPlayerParams::new(120, 100, None);

        // Initially not muted
        assert!(!params.is_track_muted(0));

        // Mute track 0
        params.set_track_muted(0, true);
        assert!(params.is_track_muted(0));
        assert!(!params.is_track_muted(1)); // Other tracks unaffected

        // Unmute track 0
        params.set_track_muted(0, false);
        assert!(!params.is_track_muted(0));
    }

    #[test]
    fn test_multiple_tracks_muted() {
        let mut params = MidiPlayerParams::new(120, 100, None);

        params.set_track_muted(0, true);
        params.set_track_muted(2, true);

        assert!(params.is_track_muted(0));
        assert!(!params.is_track_muted(1));
        assert!(params.is_track_muted(2));
    }

    #[test]
    fn test_clear_mutes() {
        let mut params = MidiPlayerParams::new(120, 100, None);

        params.set_track_muted(0, true);
        params.set_track_muted(1, true);
        params.set_track_muted(2, true);

        params.clear_mutes();

        assert!(!params.is_track_muted(0));
        assert!(!params.is_track_muted(1));
        assert!(!params.is_track_muted(2));
    }

    #[test]
    fn test_is_track_audible_no_solo_no_mute() {
        let params = MidiPlayerParams::new(120, 100, None);

        // All tracks audible by default
        assert!(params.is_track_audible(0));
        assert!(params.is_track_audible(1));
        assert!(params.is_track_audible(2));
    }

    #[test]
    fn test_is_track_audible_with_mute() {
        let mut params = MidiPlayerParams::new(120, 100, None);

        params.set_track_muted(1, true);

        assert!(params.is_track_audible(0)); // Not muted
        assert!(!params.is_track_audible(1)); // Muted
        assert!(params.is_track_audible(2)); // Not muted
    }

    #[test]
    fn test_is_track_audible_with_solo() {
        let params = MidiPlayerParams::new(120, 100, Some(1));

        assert!(!params.is_track_audible(0)); // Not soloed
        assert!(params.is_track_audible(1)); // Soloed
        assert!(!params.is_track_audible(2)); // Not soloed
    }

    #[test]
    fn test_solo_overrides_mute() {
        let mut params = MidiPlayerParams::new(120, 100, Some(1));

        // Mute the soloed track
        params.set_track_muted(1, true);

        // Solo takes precedence - the soloed track is still audible
        assert!(params.is_track_audible(1));
        // Other tracks are inaudible because of solo (regardless of mute state)
        assert!(!params.is_track_audible(0));
    }

    #[test]
    fn test_mute_ignored_when_solo_active() {
        let mut params = MidiPlayerParams::new(120, 100, Some(2));

        // Mute a non-soloed track
        params.set_track_muted(0, true);

        // Track 0 is inaudible because track 2 is soloed (mute is irrelevant)
        assert!(!params.is_track_audible(0));
        // Track 2 is audible because it's soloed
        assert!(params.is_track_audible(2));
    }
}
