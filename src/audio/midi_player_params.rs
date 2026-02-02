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
