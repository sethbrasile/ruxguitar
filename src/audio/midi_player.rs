use crate::audio::midi_builder::MidiBuilder;
use crate::audio::midi_event::MidiEvent;
use crate::audio::midi_event::MidiEventType;
use crate::audio::midi_player_params::MidiPlayerParams;
use crate::audio::midi_sequencer::MidiSequencer;
use crate::parser::song_parser::Song;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::DefaultStreamConfigError;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::watch::Sender;

/// Default sound font file is embedded in the binary (6MB)
const TIMIDITY_SOUND_FONT: &[u8] = include_bytes!("../../resources/TimGM6mb.sf2");

pub struct AudioPlayer {
    is_playing: bool,
    song: Arc<Song>,                             // Song to play (shared with app)
    stream: Option<cpal::Stream>,                // Audio output stream (not Send/Sync)
    sequencer: Arc<Mutex<MidiSequencer>>,        // Need a handle to reset sequencer
    player_params: Arc<Mutex<MidiPlayerParams>>, // Use to communicate play changes to sequencer
    synthesizers: Arc<Mutex<Vec<Synthesizer>>>,  // One synth per track for TBT
    tbt_track_count: Option<usize>,
    sound_font: Arc<SoundFont>,    // Sound font for synthesizer
    beat_sender: Arc<Sender<u32>>, // Notify beat changes
}

impl AudioPlayer {
    pub fn new(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
        beat_sender: Arc<Sender<u32>>,
    ) -> Self {
        let events = MidiBuilder::new().build_for_song(&song);
        Self::new_with_events(
            song,
            song_tempo,
            tempo_percentage,
            sound_font_file,
            beat_sender,
            events,
            None,
        )
    }

    /// Initialize the native player with the exact trace from `build_for_tbt`.
    pub fn new_with_tbt_events(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
        beat_sender: Arc<Sender<u32>>,
        events: Vec<MidiEvent>,
        track_count: usize,
    ) -> Self {
        assert!(track_count > 0);
        Self::new_with_events(
            song,
            song_tempo,
            tempo_percentage,
            sound_font_file,
            beat_sender,
            events,
            Some(track_count),
        )
    }

    fn new_with_events(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
        beat_sender: Arc<Sender<u32>>,
        events: Vec<MidiEvent>,
        tbt_track_count: Option<usize>,
    ) -> Self {
        // default to no solo track
        let solo_track_id = None;

        // player params
        let player_params = Arc::new(Mutex::new(MidiPlayerParams::new(
            song_tempo,
            tempo_percentage,
            solo_track_id,
        )));

        // sound font setup
        let sound_font = if let Some(sound_font_file) = sound_font_file {
            let mut sf2 = File::open(sound_font_file).unwrap();
            SoundFont::new(&mut sf2).unwrap()
        } else {
            let mut sf2 = TIMIDITY_SOUND_FONT;
            SoundFont::new(&mut sf2).unwrap()
        };
        let sound_font = Arc::new(sound_font);

        let midi_sequencer = MidiSequencer::new(events);

        let synthesizers = Arc::new(Mutex::new(Vec::new()));
        let sequencer = Arc::new(Mutex::new(midi_sequencer));
        Self {
            is_playing: false,
            song,
            stream: None,
            sequencer,
            player_params,
            synthesizers,
            tbt_track_count,
            sound_font,
            beat_sender,
        }
    }

    fn make_synthesizer(sound_font: Arc<SoundFont>, sample_rate: u32) -> Synthesizer {
        let synthesizer_settings = SynthesizerSettings::new(sample_rate as i32);
        let synthesizer_settings = Arc::new(synthesizer_settings);
        assert_eq!(synthesizer_settings.sample_rate, sample_rate as i32);
        Synthesizer::new(&sound_font, &synthesizer_settings).unwrap()
    }

    pub const fn is_playing(&self) -> bool {
        self.is_playing
    }

    /// The ordered trace currently held by the production sequencer.
    pub fn event_trace(&self) -> Vec<MidiEvent> {
        self.sequencer.lock().unwrap().events().to_vec()
    }

    pub fn solo_track_id(&self) -> Option<usize> {
        self.player_params.lock().unwrap().solo_track_id()
    }

    pub fn toggle_solo_mode(&self, new_track_id: usize) {
        let mut params_guard = self.player_params.lock().unwrap();
        if params_guard.solo_track_id() == Some(new_track_id) {
            log::info!("Disable solo mode on track {new_track_id}");
            params_guard.set_solo_track_id(None);
        } else {
            log::info!("Enable solo mode on track {new_track_id}");
            params_guard.set_solo_track_id(Some(new_track_id));
        }
    }

    pub fn set_tempo_percentage(&self, new_tempo_percentage: u32) {
        let mut params_guard = self.player_params.lock().unwrap();
        params_guard.set_tempo_percentage(new_tempo_percentage);
    }

    pub fn stop(&mut self) {
        // Join the stream callback before resetting state for the next Play.
        // Releasing notes alone leaves voices and a partially rendered synth
        // block that can bleed into the first attack on restart.
        drop(self.stream.take());
        self.is_playing = false;
        self.sequencer.lock().unwrap().reset_ticks();
        self.synthesizers.lock().unwrap().clear();
    }

    pub fn toggle_play(&mut self) {
        log::info!("Toggle audio stream");
        if let Some(ref stream) = self.stream {
            if self.is_playing {
                self.is_playing = false;
                if let Err(e) = stream.pause() {
                    log::error!("Failed to pause stream: {e}");
                }
            } else {
                self.is_playing = true;
                if let Err(e) = stream.play() {
                    log::error!("Failed to play stream: {e}");
                }
            }
        } else {
            self.is_playing = true;

            // Initialize audio output stream
            let stream = new_output_stream(
                self.sequencer.clone(),
                self.player_params.clone(),
                self.synthesizers.clone(),
                self.tbt_track_count,
                self.sound_font.clone(),
                self.beat_sender.clone(),
            );

            match stream {
                Ok(stream) => {
                    self.stream = Some(stream);
                }
                Err(err) => {
                    log::error!("Failed to create audio stream: {err}");
                    self.is_playing = false;
                    self.stream = None;
                }
            }
        }
    }

    pub fn focus_measure(&self, measure_id: usize) {
        log::debug!("Focus audio player on measure:{measure_id}");
        let measure = &self.song.measure_headers[measure_id];
        let measure_start_tick = measure.start;
        let tempo = measure.tempo.value;

        // move sequencer to measure start tick
        let mut sequencer_guard = self.sequencer.lock().unwrap();
        sequencer_guard.set_tick(measure_start_tick);
        drop(sequencer_guard);

        // stop current sound
        let mut synthesizer_guard = self.synthesizers.lock().unwrap();
        for synthesizer in synthesizer_guard.iter_mut() {
            synthesizer.note_off_all(false);
        }
        drop(synthesizer_guard);

        // set tempo for focuses measure
        let mut player_params_guard = self.player_params.lock().unwrap();
        player_params_guard.set_tempo(tempo);
    }

    /// Seek to a specific tick position.
    ///
    /// Pauses if playing, seeks to the tick, then resumes if was playing.
    pub fn seek_to_tick(&mut self, tick: u32) {
        let was_playing = self.is_playing;

        // Pause stream if playing
        if was_playing {
            if let Some(stream) = &self.stream {
                if let Err(e) = stream.pause() {
                    log::error!("Failed to pause stream during seek: {e}");
                }
            }
        }

        // Move sequencer to the requested tick
        let mut sequencer_guard = self.sequencer.lock().unwrap();
        sequencer_guard.set_tick(tick);
        drop(sequencer_guard);

        // Stop all currently playing notes
        let mut synthesizer_guard = self.synthesizers.lock().unwrap();
        for synthesizer in synthesizer_guard.iter_mut() {
            synthesizer.note_off_all(false);
        }
        drop(synthesizer_guard);

        // Resume stream if was playing
        if was_playing {
            if let Some(stream) = &self.stream {
                if let Err(e) = stream.play() {
                    log::error!("Failed to resume stream after seek: {e}");
                }
            }
        }
    }

    /// Get the current playback tick position.
    pub fn get_current_tick(&self) -> u32 {
        self.sequencer.lock().unwrap().get_tick()
    }

    /// Set mute state for a specific track.
    pub fn set_track_muted(&self, track_id: usize, muted: bool) {
        self.player_params
            .lock()
            .unwrap()
            .set_track_muted(track_id, muted);
    }

    /// Check if a track is muted.
    pub fn is_track_muted(&self, track_id: usize) -> bool {
        self.player_params.lock().unwrap().is_track_muted(track_id)
    }

    /// Check if a track is audible (considering both solo and mute state).
    pub fn is_track_audible(&self, track_id: usize) -> bool {
        self.player_params
            .lock()
            .unwrap()
            .is_track_audible(track_id)
    }
}

#[derive(Debug, thiserror::Error)]
enum AudioStreamError {
    #[error("audio device not found")]
    CpalDeviceNotFount,
    #[error("no output configuration found: {0}")]
    CpalOutputConfigNotFound(DefaultStreamConfigError),
    #[error("failed to build output stream: {0}")]
    StreamBuildError(String),
    #[error("failed to start stream playback: {0}")]
    StreamPlayError(String),
}

/// Create a new output stream for audio playback.
fn new_output_stream(
    sequencer: Arc<Mutex<MidiSequencer>>,
    player_params: Arc<Mutex<MidiPlayerParams>>,
    synthesizers: Arc<Mutex<Vec<Synthesizer>>>,
    tbt_track_count: Option<usize>,
    sound_font: Arc<SoundFont>,
    beat_notifier: Arc<Sender<u32>>,
) -> Result<cpal::Stream, AudioStreamError> {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return Err(AudioStreamError::CpalDeviceNotFount);
    };

    let config = device
        .default_output_config()
        .map_err(AudioStreamError::CpalOutputConfigNotFound)?;

    assert!(
        config.sample_format().is_float(),
        "{}",
        format!("Unsupported sample format {}", config.sample_format())
    );
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;

    log::info!(
        "Audio output stream config: {:?} ({} channels, {} Hz)",
        stream_config,
        stream_config.channels,
        stream_config.sample_rate.0
    );

    let mut synthesizer_guard = synthesizers.lock().unwrap();
    let track_isolated = tbt_track_count.is_some();
    *synthesizer_guard = (0..tbt_track_count.unwrap_or(1))
        .map(|_| AudioPlayer::make_synthesizer(sound_font.clone(), sample_rate))
        .collect();

    // A new stream has fresh voices. Restore controller and instrument changes
    // before a start at the cursor. The renderer handles the cursor tick itself.
    {
        let seq = sequencer.lock().unwrap();
        for event in seq
            .events()
            .iter()
            .take_while(|event| event.tick < seq.get_tick())
        {
            if let MidiEventType::MidiMessage(channel, command, data1, data2) = event.event {
                let target = if track_isolated {
                    event.track.unwrap_or(0) as usize
                } else {
                    0
                };
                if let Some(synth) = synthesizer_guard.get_mut(target) {
                    synth.process_midi_message(channel, command, data1, data2);
                }
            }
        }
    }
    drop(synthesizer_guard);

    let mut render = output_renderer(
        sequencer,
        player_params,
        synthesizers,
        track_isolated,
        beat_notifier,
        sample_rate,
        usize::from(stream_config.channels),
    );
    let err_fn = |err| log::error!("an error occurred on stream: {err}");
    let stream = device.build_output_stream(
        &stream_config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| render(output),
        err_fn,
        None, // blocking stream
    );
    let stream = stream.map_err(|e| AudioStreamError::StreamBuildError(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioStreamError::StreamPlayError(e.to_string()))?;
    Ok(stream)
}

fn output_renderer(
    sequencer: Arc<Mutex<MidiSequencer>>,
    player_params: Arc<Mutex<MidiPlayerParams>>,
    synthesizers: Arc<Mutex<Vec<Synthesizer>>>,
    track_isolated: bool,
    beat_notifier: Arc<Sender<u32>>,
    sample_rate: u32,
    channel_count: usize,
) -> impl FnMut(&mut [f32]) {
    // Reuse scratch space across all segments of a device buffer.
    const MIX_FRAMES: usize = 512;
    let mut left = [0.0; MIX_FRAMES];
    let mut right = [0.0; MIX_FRAMES];
    move |output| {
        let mut params = player_params.lock().unwrap();
        let mut seq = sequencer.lock().unwrap();
        let mut synths = synthesizers.lock().unwrap();
        let mut remaining = output;
        while !remaining.is_empty() {
            let tick = seq.get_tick();
            if let Some(events) = seq.get_next_events() {
                if events.iter().any(MidiEvent::is_note_event) {
                    let _ = beat_notifier.send(tick);
                }
                for event in events {
                    apply_event(event, &mut params, &mut synths, track_isolated);
                }
            }

            // Events at the current frame have been applied. Render up to (but
            // excluding) the frame of the next event, at the tempo now in effect.
            let tempo = params.adjusted_tempo();
            let frames = (remaining.len() / channel_count)
                .min(MIX_FRAMES)
                .min(seq.frames_until_next_event(tempo, sample_rate));
            let (segment, rest) = remaining.split_at_mut(frames * channel_count);
            segment.fill(0.0);
            for synth in synths.iter_mut() {
                synth.render(&mut left[..frames], &mut right[..frames]);
                for (i, frame) in segment.chunks_exact_mut(channel_count).enumerate() {
                    if channel_count == 1 {
                        frame[0] += (left[i] + right[i]) * 0.5;
                    } else {
                        frame[0] += left[i];
                        frame[1] += right[i];
                    }
                }
            }
            seq.advance_frames(tempo, frames, sample_rate);
            remaining = rest;
        }
    }
}

fn apply_event(
    event: &MidiEvent,
    params: &mut MidiPlayerParams,
    synths: &mut [Synthesizer],
    track_isolated: bool,
) {
    let track = event.track.map(usize::from);
    let target = if track_isolated {
        track.unwrap_or(0)
    } else {
        0
    };
    match event.event {
        MidiEventType::TempoChange(tempo) => params.set_tempo(tempo),
        MidiEventType::NoteOn(channel, key, velocity) => {
            if track.is_none_or(|id| params.is_track_audible(id)) {
                if let Some(synth) = synths.get_mut(target) {
                    synth.note_on(channel, key, i32::from(velocity));
                }
            }
        }
        MidiEventType::NoteOff(channel, key) => {
            if let Some(synth) = synths.get_mut(target) {
                synth.note_off(channel, key);
            }
        }
        MidiEventType::MidiMessage(channel, command, data1, data2) => {
            if let Some(synth) = synths.get_mut(target) {
                synth.process_midi_message(channel, command, data1, data2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_four_triplet_attacks_at_their_frame_offsets() {
        const RATE: u32 = 48_000;
        let font = Arc::new(SoundFont::new(&mut TIMIDITY_SOUND_FONT.as_ref()).unwrap());
        let notes = [(0, 45), (160, 46), (320, 47), (480, 49)];
        let mut events = Vec::new();
        for (i, &(tick, key)) in notes.iter().enumerate() {
            events.push(MidiEvent::new_note_on(960 + tick, 0, key, 95, 0));
            let end = notes.get(i + 1).map_or(720, |&(next, _)| next);
            events.push(MidiEvent::new_note_off(960 + end, 0, key, 0));
        }
        for speed in [100, 70, 140] {
            let frame_at = |ticks: u32| {
                (u64::from(ticks) * u64::from(RATE) * 60)
                    .div_ceil(960 * 120 * u64::from(speed) / 100) as usize
            };
            let frames = frame_at(800);
            let mut left = vec![0.; frames];
            let mut right = vec![0.; frames];
            let mut reference = AudioPlayer::make_synthesizer(font.clone(), RATE);
            for (i, &(tick, key)) in notes.iter().enumerate() {
                let end = notes.get(i + 1).map_or(720, |&(next, _)| next);
                let range = frame_at(tick)..frame_at(end);
                reference.note_on(0, key, 95);
                reference.render(&mut left[range.clone()], &mut right[range]);
                reference.note_off(0, key);
            }
            reference.render(&mut left[frame_at(720)..], &mut right[frame_at(720)..]);
            for buffer_frames in [256, 4800, 14400] {
                let mut seq = MidiSequencer::new(events.clone());
                seq.set_tick(960);
                let (sender, _rx) = tokio::sync::watch::channel(0);
                let mut render = output_renderer(
                    Arc::new(Mutex::new(seq)),
                    Arc::new(Mutex::new(MidiPlayerParams::new(120, speed, None))),
                    Arc::new(Mutex::new(vec![AudioPlayer::make_synthesizer(
                        font.clone(),
                        RATE,
                    )])),
                    true,
                    Arc::new(sender),
                    RATE,
                    2,
                );
                let mut output = vec![f32::NAN; 2 * frames];
                for buffer in output.chunks_mut(2 * buffer_frames) {
                    render(buffer);
                }
                let mismatch = output
                    .chunks_exact(2)
                    .enumerate()
                    .position(|(i, sample)| sample != [left[i], right[i]]);
                assert_eq!(mismatch, None, "{speed}% in {buffer_frames}-frame buffers");
            }
        }
    }
}
