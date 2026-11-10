use crate::audio::midi_builder::MidiBuilder;
use crate::audio::midi_event::MidiEvent;
use crate::audio::midi_event::MidiEventType;
use crate::audio::midi_player_params::MidiPlayerParams;
use crate::audio::midi_sequencer::MidiSequencer;
use crate::audio::FIRST_TICK;
use crate::parser::song_parser::Song;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::DefaultStreamConfigError;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::watch::Sender;

const DEFAULT_SAMPLE_RATE: u32 = 44100; // number of samples per second

/// Default sound font file is embedded in the binary (6MB)
const TIMIDITY_SOUND_FONT: &[u8] = include_bytes!("../../resources/TimGM6mb.sf2");

pub struct AudioPlayer {
    is_playing: bool,
    song: Arc<Song>,                             // Song to play (shared with app)
    stream: Option<cpal::Stream>,                // Audio output stream (not Send/Sync)
    sequencer: Arc<Mutex<MidiSequencer>>,        // Need a handle to reset sequencer
    player_params: Arc<Mutex<MidiPlayerParams>>, // Use to communicate play changes to sequencer
    synthesizers: Arc<Mutex<Vec<Synthesizer>>>,  // One synth per track for TBT
    track_isolated: bool,
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
            1,
            false,
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
            track_count,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_events(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
        beat_sender: Arc<Sender<u32>>,
        events: Vec<MidiEvent>,
        synth_count: usize,
        track_isolated: bool,
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

        // build new default synthesizer for the stream
        let synthesizers = (0..synth_count)
            .map(|_| Self::make_synthesizer(sound_font.clone(), DEFAULT_SAMPLE_RATE))
            .collect();
        let midi_sequencer = MidiSequencer::new(events);

        let synthesizers = Arc::new(Mutex::new(synthesizers));
        let sequencer = Arc::new(Mutex::new(midi_sequencer));
        Self {
            is_playing: false,
            song,
            stream: None,
            sequencer,
            player_params,
            synthesizers,
            track_isolated,
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
        // Pause stream
        if let Some(stream) = &self.stream {
            log::info!("Stopping audio stream");
            if let Err(e) = stream.pause() {
                log::error!("Failed to pause stream during stop: {e}");
            }
        }
        self.is_playing = false;

        // reset ticks
        let mut sequencer_guard = self.sequencer.lock().unwrap();
        sequencer_guard.reset_ticks();
        drop(sequencer_guard);

        // stop all sound in synthesizer
        let mut synthesizer_guard = self.synthesizers.lock().unwrap();
        for synthesizer in synthesizer_guard.iter_mut() {
            synthesizer.note_off_all(false);
        }
        drop(synthesizer_guard);

        // Drop stream
        self.stream.take();
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
                self.track_isolated,
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
    track_isolated: bool,
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
    if sample_rate != DEFAULT_SAMPLE_RATE {
        // audio output is not using the default sample rate - recreate synthesizer with proper sample rate
        for synth in synthesizer_guard.iter_mut() {
            *synth = AudioPlayer::make_synthesizer(sound_font.clone(), sample_rate);
        }
    }

    // Apply events at tick=FIRST_TICK to set up synthesizer state
    // otherwise clicking on a measure *before* playing does not produce the correct instrument sound
    sequencer
        .lock()
        .unwrap()
        .events()
        .iter()
        .take_while(|event| event.tick == FIRST_TICK)
        .filter(|event| event.is_midi_message())
        .for_each(|event| {
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
        });

    drop(synthesizer_guard);

    // Size left and right buffers according to sample rate.
    // The buffer accounts for 0.1 second of audio.
    // e.g. 4410 samples at 44100 Hz is 0.1 second
    let channel_sample_count = sample_rate / 10;

    // reuse buffer for left and right channels across all calls
    let mut left: Vec<f32> = vec![0_f32; channel_sample_count as usize];
    let mut right: Vec<f32> = vec![0_f32; channel_sample_count as usize];
    let mut track_left: Vec<f32> = vec![0_f32; channel_sample_count as usize];
    let mut track_right: Vec<f32> = vec![0_f32; channel_sample_count as usize];

    let channel_count = usize::from(stream_config.channels);
    let err_fn = |err| log::error!("an error occurred on stream: {err}");

    let stream = device.build_output_stream(
        &stream_config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut player_params_guard = player_params.lock().unwrap();
            let mut sequencer_guard = sequencer.lock().unwrap();
            sequencer_guard.advance_frames(
                player_params_guard.adjusted_tempo(),
                output.len() / channel_count,
                sample_rate,
            );
            let mut synthesizer_guard = synthesizers.lock().unwrap();
            let tick = sequencer_guard.get_tick();
            let last_tick = sequencer_guard.get_last_tick();
            // process midi events for current tick
            if let Some(events) = sequencer_guard.get_next_events() {
                if !events.is_empty() {
                    log::debug!(
                        "---> Increase {} ticks [{} -> {}] ({} events)",
                        tick - last_tick,
                        last_tick,
                        tick,
                        events.len()
                    );
                }
                if events
                    .iter()
                    .any(super::midi_event::MidiEvent::is_note_event)
                {
                    beat_notifier
                        .send(tick)
                        .expect("Failed to send beat notification");
                }
                for midi_event in events {
                    let target = if track_isolated {
                        midi_event.track.unwrap_or(0) as usize
                    } else {
                        0
                    };
                    match midi_event.event {
                        MidiEventType::NoteOn(channel, key, velocity) => {
                            // Check if track is audible (handles both solo and mute)
                            if let Some(track_id) = midi_event.track {
                                if !player_params_guard.is_track_audible(track_id as usize) {
                                    continue;
                                }
                            }
                            log::debug!(
                                "[{}] Note on: channel={}, key={}, velocity={}",
                                midi_event.tick,
                                channel,
                                key,
                                velocity
                            );
                            if let Some(synth) = synthesizer_guard.get_mut(target) {
                                synth.note_on(channel, key, i32::from(velocity));
                            }
                        }
                        MidiEventType::NoteOff(channel, key) => {
                            log::debug!(
                                "[{}] Note off: channel={}, key={}",
                                midi_event.tick,
                                channel,
                                key
                            );
                            if let Some(synth) = synthesizer_guard.get_mut(target) {
                                synth.note_off(channel, key);
                            }
                        }
                        MidiEventType::TempoChange(tempo) => {
                            log::info!("Tempo changed to {tempo}");
                            player_params_guard.set_tempo(tempo);
                        }
                        MidiEventType::MidiMessage(channel, command, data1, data2) => {
                            log::debug!(
                                "[{}] Midi message: channel={}, command={}, data1={}, data2={}",
                                midi_event.tick,
                                channel,
                                command,
                                data1,
                                data2
                            );
                            if let Some(synth) = synthesizer_guard.get_mut(target) {
                                synth.process_midi_message(channel, command, data1, data2);
                            }
                        }
                    }
                }
            }
            // Split buffer for this run between left and right
            let mut output_channel_len = output.len() / 2;

            if left.len() < output_channel_len || right.len() < output_channel_len {
                log::info!(
                    "Output buffer larger than expected channel size {} > {}",
                    output_channel_len,
                    left.len()
                );
                output_channel_len = left.len();
            }

            // Render the waveform.
            left[..output_channel_len].fill(0.0);
            right[..output_channel_len].fill(0.0);
            for synth in synthesizer_guard.iter_mut() {
                synth.render(
                    &mut track_left[..output_channel_len],
                    &mut track_right[..output_channel_len],
                );
                for i in 0..output_channel_len {
                    left[i] += track_left[i];
                    right[i] += track_right[i];
                }
            }

            // Drop locks
            drop(sequencer_guard);
            drop(synthesizer_guard);
            drop(player_params_guard);

            // Interleave the left and right channels into the output buffer.
            for i in 0..output_channel_len {
                output[i * 2] = left[i];
                output[i * 2 + 1] = right[i];
            }
        },
        err_fn,
        None, // blocking stream
    );
    let stream = stream.map_err(|e| AudioStreamError::StreamBuildError(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioStreamError::StreamPlayError(e.to_string()))?;
    Ok(stream)
}
