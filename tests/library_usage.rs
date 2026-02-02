//! Integration tests for ruxguitar library usage.
//!
//! These tests verify that the library can be used as a dependency
//! from external projects.

use ruxguitar::{
    is_tbt_file, parse_gp_data, parse_tbt_data, tbt_to_song, AudioEngine, AudioState, MidiBuilder,
    MidiEventType, RuxError, Song, TbtSong, FIRST_TICK, QUARTER_TIME,
};
use std::io::Read;
use std::sync::Arc;

/// Test that all major types are accessible from the library.
#[test]
fn test_types_accessible() {
    // This test verifies that the public API types compile and are usable.
    // If any re-export is missing, this test will fail to compile.

    fn _assert_types() {
        let _: fn(&[u8]) -> Result<Song, RuxError> = parse_gp_data;
        let _: u32 = FIRST_TICK;
        let _: u32 = QUARTER_TIME;
    }
}

/// Test parsing a GP5 file from the test-files directory.
#[test]
fn test_parse_gp5_file() {
    let mut file = std::fs::File::open("test-files/Demo v5.gp5").expect("Failed to open test file");
    let mut file_data: Vec<u8> = vec![];
    file.read_to_end(&mut file_data)
        .expect("Failed to read test file");

    let song = parse_gp_data(&file_data).expect("Failed to parse GP5 file");

    // Verify basic song structure
    assert!(
        !song.tracks.is_empty(),
        "Song should have at least one track"
    );
    assert!(
        !song.measure_headers.is_empty(),
        "Song should have at least one measure"
    );
}

/// Test generating MIDI events from a parsed song.
#[test]
fn test_midi_generation() {
    let mut file = std::fs::File::open("test-files/Demo v5.gp5").expect("Failed to open test file");
    let mut file_data: Vec<u8> = vec![];
    file.read_to_end(&mut file_data)
        .expect("Failed to read test file");

    let song = parse_gp_data(&file_data).expect("Failed to parse GP5 file");
    let song_arc = Arc::new(song);
    let midi_events = MidiBuilder::new().build_for_song(&song_arc);

    // Verify MIDI events were generated
    assert!(!midi_events.is_empty(), "Should generate MIDI events");

    // Verify we have expected event types
    let has_note_on = midi_events
        .iter()
        .any(|e| matches!(e.event, MidiEventType::NoteOn { .. }));
    assert!(has_note_on, "Should have NoteOn events");
}

/// Test error handling for invalid data.
#[test]
fn test_parse_error() {
    let invalid_data = vec![0u8; 10]; // Not a valid GP file
    let result = parse_gp_data(&invalid_data);

    assert!(result.is_err(), "Should return error for invalid data");
    let err = result.unwrap_err();
    assert!(
        matches!(err, RuxError::ParsingError(_)),
        "Should be a ParsingError"
    );
}

// =============================================================================
// TBT (TabIt) file format tests
// =============================================================================

/// Test that TBT exports are accessible from the library.
#[test]
fn test_tbt_types_accessible() {
    // This test verifies that the TBT public API types compile and are usable.
    fn _assert_tbt_types() {
        let _: fn(&[u8]) -> bool = is_tbt_file;
        let _: fn(&[u8]) -> Result<TbtSong, RuxError> = parse_tbt_data;
    }
}

/// Test TBT file format detection.
#[test]
fn test_is_tbt_file() {
    // Read a real TBT file
    let tbt_data =
        std::fs::read("test-files/Aguado - Study 1.tbt").expect("Failed to read TBT file");
    assert!(is_tbt_file(&tbt_data), "Should detect TBT file format");

    // Test with non-TBT data
    let gp_data = std::fs::read("test-files/Demo v5.gp5").expect("Failed to read GP file");
    assert!(!is_tbt_file(&gp_data), "Should not detect GP file as TBT");

    // Test with invalid data
    let invalid_data = vec![0u8; 10];
    assert!(
        !is_tbt_file(&invalid_data),
        "Should not detect invalid data as TBT"
    );
}

/// Test parsing a TBT file.
#[test]
fn test_parse_tbt_file() {
    let tbt_data =
        std::fs::read("test-files/Aguado - Study 1.tbt").expect("Failed to read TBT file");

    let tbt_song: TbtSong = parse_tbt_data(&tbt_data).expect("Failed to parse TBT file");

    // Verify we got a song structure with tracks
    assert!(
        !tbt_song.metadata.tracks.is_empty(),
        "TBT song should have at least one track"
    );
}

/// Test converting TBT to common Song format.
#[test]
fn test_tbt_to_song_conversion() {
    let tbt_data =
        std::fs::read("test-files/Aguado - Study 1.tbt").expect("Failed to read TBT file");
    let tbt_song = parse_tbt_data(&tbt_data).expect("Failed to parse TBT file");

    let song = tbt_to_song(&tbt_song).expect("Failed to convert TBT to Song");

    // Verify basic song structure
    assert!(
        !song.tracks.is_empty(),
        "Converted song should have at least one track"
    );
    assert!(
        !song.measure_headers.is_empty(),
        "Converted song should have at least one measure"
    );
}

/// Test generating MIDI events from a TBT file.
#[test]
fn test_tbt_midi_generation() {
    let tbt_data =
        std::fs::read("test-files/Aguado - Study 1.tbt").expect("Failed to read TBT file");
    let tbt_song = parse_tbt_data(&tbt_data).expect("Failed to parse TBT file");
    let song = tbt_to_song(&tbt_song).expect("Failed to convert TBT to Song");

    let song_arc = Arc::new(song);
    let midi_events = MidiBuilder::new().build_for_song(&song_arc);

    // Verify MIDI events were generated
    assert!(
        !midi_events.is_empty(),
        "Should generate MIDI events from TBT"
    );

    // Verify we have note events
    let has_note_on = midi_events
        .iter()
        .any(|e| matches!(e.event, MidiEventType::NoteOn { .. }));
    assert!(has_note_on, "Should have NoteOn events from TBT");
}

// =============================================================================
// Audio Engine tests
// =============================================================================

/// Test that AudioEngine and AudioState types are accessible.
#[test]
fn test_audio_engine_types_accessible() {
    // This test verifies that the audio engine public API types compile.

    // Verify AudioState can be constructed
    let state = AudioState {
        is_playing: false,
        current_tick: 0,
        solo_track_id: None,
    };
    assert!(!state.is_playing);
    assert_eq!(state.current_tick, 0);
    assert_eq!(state.solo_track_id, None);

    // Verify AudioEngine type is accessible (can't call new without audio hardware)
    fn _takes_engine(_engine: AudioEngine) {}
}
