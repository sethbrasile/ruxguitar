//! Thread-safe audio engine for external integration (e.g., Tauri apps).
//!
//! This module provides `AudioEngine`, a wrapper around `AudioPlayer` that
//! is `Send + Sync` and can be safely used from any thread, including Tauri
//! command handlers.
//!
//! # Architecture
//!
//! The `AudioEngine` communicates with an `AudioPlayer` running on a dedicated
//! thread via channels. This design ensures that `cpal::Stream` (which is
//! `!Send + !Sync`) stays on a single thread while providing a thread-safe
//! API to external consumers.
//!
//! # Example
//!
//! ```no_run
//! use ruxguitar::{AudioEngine, parse_gp_data};
//! use std::sync::Arc;
//!
//! let file_data = std::fs::read("song.gp5").unwrap();
//! let song = parse_gp_data(&file_data).unwrap();
//! let engine = AudioEngine::new(Arc::new(song), 120, 100, None).unwrap();
//!
//! engine.play().unwrap();
//! // ... later
//! engine.pause().unwrap();
//! engine.stop().unwrap();
//! ```

use crate::audio::midi_player::AudioPlayer;
use crate::error::RuxError;
use crate::parser::song_parser::Song;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tokio::sync::watch;

/// Commands sent to the audio player thread.
#[derive(Debug)]
enum AudioCommand {
    Play,
    Pause,
    Stop,
    TogglePlay,
    SeekToTick(u32),
    FocusMeasure(usize),
    SetTempoPercentage(u32),
    ToggleSoloMode(usize),
    SetTrackMuted(usize, bool),
    GetState(Sender<AudioState>),
    Shutdown,
}

/// Current state of the audio engine.
#[derive(Debug, Clone)]
pub struct AudioState {
    /// Whether audio is currently playing.
    pub is_playing: bool,
    /// Current playback tick position.
    pub current_tick: u32,
    /// ID of the solo track, if any.
    pub solo_track_id: Option<usize>,
}

/// Thread-safe audio engine for playback control.
///
/// This struct is `Send + Sync` and can be safely shared across threads,
/// making it suitable for use in Tauri command handlers or other multi-threaded
/// contexts.
pub struct AudioEngine {
    command_tx: Sender<AudioCommand>,
    beat_receiver: watch::Receiver<u32>,
    worker_handle: Mutex<Option<JoinHandle<()>>>,
}

// Safety: AudioEngine only contains channel endpoints and a mutex-protected handle,
// all of which are Send + Sync.
unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

impl AudioEngine {
    /// Create a new audio engine for the given song.
    ///
    /// # Arguments
    ///
    /// * `song` - The parsed song to play
    /// * `song_tempo` - Initial tempo in BPM
    /// * `tempo_percentage` - Tempo percentage (100 = normal speed)
    /// * `sound_font_file` - Optional custom soundfont file path
    ///
    /// # Returns
    ///
    /// A new `AudioEngine` instance, or an error if initialization fails.
    pub fn new(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
    ) -> Result<Self, RuxError> {
        let (command_tx, command_rx) = mpsc::channel::<AudioCommand>();
        let (beat_tx, beat_rx) = watch::channel(0u32);
        let beat_sender = Arc::new(beat_tx);

        // Spawn the audio player on a dedicated thread
        let worker_handle = thread::Builder::new()
            .name("audio-engine".to_string())
            .spawn(move || {
                Self::audio_worker(
                    song,
                    song_tempo,
                    tempo_percentage,
                    sound_font_file,
                    beat_sender,
                    command_rx,
                );
            })
            .map_err(|e| RuxError::AudioError(format!("Failed to spawn audio thread: {e}")))?;

        Ok(Self {
            command_tx,
            beat_receiver: beat_rx,
            worker_handle: Mutex::new(Some(worker_handle)),
        })
    }

    /// Audio worker function that runs on a dedicated thread.
    fn audio_worker(
        song: Arc<Song>,
        song_tempo: u32,
        tempo_percentage: u32,
        sound_font_file: Option<PathBuf>,
        beat_sender: Arc<watch::Sender<u32>>,
        command_rx: Receiver<AudioCommand>,
    ) {
        let mut player = AudioPlayer::new(
            song,
            song_tempo,
            tempo_percentage,
            sound_font_file,
            beat_sender,
        );

        while let Ok(command) = command_rx.recv() {
            match command {
                AudioCommand::Play => {
                    if !player.is_playing() {
                        player.toggle_play();
                    }
                }
                AudioCommand::Pause => {
                    if player.is_playing() {
                        player.toggle_play();
                    }
                }
                AudioCommand::Stop => {
                    player.stop();
                }
                AudioCommand::TogglePlay => {
                    player.toggle_play();
                }
                AudioCommand::SeekToTick(tick) => {
                    player.seek_to_tick(tick);
                }
                AudioCommand::FocusMeasure(measure_id) => {
                    player.focus_measure(measure_id);
                }
                AudioCommand::SetTempoPercentage(percentage) => {
                    player.set_tempo_percentage(percentage);
                }
                AudioCommand::ToggleSoloMode(track_id) => {
                    player.toggle_solo_mode(track_id);
                }
                AudioCommand::SetTrackMuted(track_id, muted) => {
                    player.set_track_muted(track_id, muted);
                }
                AudioCommand::GetState(response_tx) => {
                    let state = AudioState {
                        is_playing: player.is_playing(),
                        current_tick: player.get_current_tick(),
                        solo_track_id: player.solo_track_id(),
                    };
                    let _ = response_tx.send(state);
                }
                AudioCommand::Shutdown => {
                    player.stop();
                    break;
                }
            }
        }
    }

    /// Send a command to the audio worker.
    fn send_command(&self, command: AudioCommand) -> Result<(), RuxError> {
        self.command_tx
            .send(command)
            .map_err(|_| RuxError::AudioError("Audio worker thread disconnected".to_string()))
    }

    /// Start playback.
    pub fn play(&self) -> Result<(), RuxError> {
        self.send_command(AudioCommand::Play)
    }

    /// Pause playback.
    pub fn pause(&self) -> Result<(), RuxError> {
        self.send_command(AudioCommand::Pause)
    }

    /// Stop playback and reset to the beginning.
    pub fn stop(&self) -> Result<(), RuxError> {
        self.send_command(AudioCommand::Stop)
    }

    /// Toggle between playing and paused states.
    pub fn toggle_play(&self) -> Result<(), RuxError> {
        self.send_command(AudioCommand::TogglePlay)
    }

    /// Seek to a specific tick position.
    pub fn seek_to_tick(&self, tick: u32) -> Result<(), RuxError> {
        self.send_command(AudioCommand::SeekToTick(tick))
    }

    /// Focus on a specific measure (seeks to measure start).
    pub fn focus_measure(&self, measure_id: usize) -> Result<(), RuxError> {
        self.send_command(AudioCommand::FocusMeasure(measure_id))
    }

    /// Set the tempo percentage (100 = normal speed).
    pub fn set_tempo_percentage(&self, percentage: u32) -> Result<(), RuxError> {
        self.send_command(AudioCommand::SetTempoPercentage(percentage))
    }

    /// Toggle solo mode for a specific track.
    pub fn toggle_solo_mode(&self, track_id: usize) -> Result<(), RuxError> {
        self.send_command(AudioCommand::ToggleSoloMode(track_id))
    }

    /// Set mute state for a specific track.
    pub fn set_track_muted(&self, track_id: usize, muted: bool) -> Result<(), RuxError> {
        self.send_command(AudioCommand::SetTrackMuted(track_id, muted))
    }

    /// Get the current state of the audio engine.
    ///
    /// This performs a synchronous query to the audio worker thread.
    pub fn get_state(&self) -> Result<AudioState, RuxError> {
        let (response_tx, response_rx) = mpsc::channel();
        self.send_command(AudioCommand::GetState(response_tx))?;
        response_rx.recv().map_err(|_| {
            RuxError::AudioError("Failed to receive state from audio worker".to_string())
        })
    }

    /// Check if audio is currently playing.
    pub fn is_playing(&self) -> Result<bool, RuxError> {
        Ok(self.get_state()?.is_playing)
    }

    /// Get the current playback tick position.
    pub fn get_current_tick(&self) -> Result<u32, RuxError> {
        Ok(self.get_state()?.current_tick)
    }

    /// Get a receiver for beat change notifications.
    ///
    /// This can be used to synchronize UI updates with the playback position.
    pub fn beat_receiver(&self) -> watch::Receiver<u32> {
        self.beat_receiver.clone()
    }

    /// Shutdown the audio engine and release resources.
    ///
    /// This is called automatically when the engine is dropped,
    /// but can be called explicitly for more controlled cleanup.
    pub fn shutdown(&self) -> Result<(), RuxError> {
        self.send_command(AudioCommand::Shutdown)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Send shutdown command (ignore errors if channel is already closed)
        let _ = self.command_tx.send(AudioCommand::Shutdown);

        // Wait for the worker thread to finish
        if let Ok(mut handle_guard) = self.worker_handle.lock() {
            if let Some(handle) = handle_guard.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Full audio tests require actual audio hardware.
    // These tests verify the API compiles and basic operations work.

    #[test]
    fn test_audio_state_debug() {
        let state = AudioState {
            is_playing: true,
            current_tick: 100,
            solo_track_id: Some(1),
        };
        let debug_str = format!("{state:?}");
        assert!(debug_str.contains("is_playing: true"));
    }

    #[test]
    fn test_audio_state_clone() {
        let state = AudioState {
            is_playing: true,
            current_tick: 100,
            solo_track_id: None,
        };
        let cloned = state.clone();
        assert_eq!(cloned.is_playing, state.is_playing);
        assert_eq!(cloned.current_tick, state.current_tick);
    }
}
