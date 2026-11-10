//! Native TBT event trace built from the serializable exact-time ruler.

use crate::audio::midi_event::MidiEvent;
use crate::parser::tbt_timeline::TbtTimeline;
use crate::parser::tbt_types::{TbtEffectChangeType, TbtSong};
use crate::RuxError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const ENGINE_START: u32 = 960;
const STANDARD_TUNING: [i32; 6] = [40, 45, 50, 55, 59, 64];

fn invalid(message: impl Into<String>) -> RuxError {
    RuxError::ParsingError(format!("Invalid TBT playback: {}", message.into()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtAttack {
    pub cell: u32,
    /// Source string index, low string first.
    pub string: u8,
    pub fret: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtPlaybackTrack {
    pub tuning: Vec<i32>,
    pub program: u8,
    pub muted_program: u8,
    pub bank: u8,
    pub volume: u8,
    pub pan: u8,
    pub chorus: u8,
    pub reverb: u8,
    pub drum: bool,
    pub transpose: i8,
    pub attacks: Vec<TbtAttack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtPlayback {
    pub timeline: TbtTimeline,
    pub tempo: u32,
    pub tracks: Vec<TbtPlaybackTrack>,
}

impl TbtPlayback {
    pub fn from_parsed(song: &TbtSong) -> Result<Self, RuxError> {
        let timeline = TbtTimeline::from_parsed(song)?;
        let mut tracks = Vec::with_capacity(song.metadata.tracks.len());
        for (index, source) in song.metadata.tracks.iter().enumerate() {
            if source.string_count == 0 || source.string_count > 8 {
                return Err(invalid(format!("track {index} string count")));
            }
            let tuning = (0..source.string_count as usize)
                .map(|string| {
                    if let Some(base) = STANDARD_TUNING.get(string) {
                        base + i32::from(source.tuning[string] as i8)
                    } else {
                        i32::from(source.tuning[string])
                    }
                })
                .collect();
            let attacks = song.track_notes[index]
                .iter()
                .filter(|note| !note.is_stop)
                .map(|note| TbtAttack {
                    cell: note.vsq_position / 20,
                    string: note.string,
                    fret: note.fret,
                    muted: note.is_muted,
                })
                .collect();
            tracks.push(TbtPlaybackTrack {
                tuning,
                program: source.clean_guitar,
                muted_program: source.muted_guitar,
                bank: source.midi_bank,
                volume: source.volume,
                pan: source.pan,
                chorus: source.chorus,
                reverb: source.reverb,
                drum: source.is_drum,
                transpose: source.transpose_half_steps,
                attacks,
            });
        }
        let playback = Self {
            timeline,
            tempo: u32::from(song.header.tempo2),
            tracks,
        };
        playback.validate()?;
        Ok(playback)
    }

    pub fn validate(&self) -> Result<(), RuxError> {
        self.timeline.validate()?;
        if self.tempo == 0 || self.tracks.len() != self.timeline.tracks.len() {
            return Err(invalid("tempo or track count"));
        }
        for (index, track) in self.tracks.iter().enumerate() {
            if track.tuning.is_empty() || track.tuning.len() > 8 {
                return Err(invalid(format!("track {index} string count")));
            }
            let cells = self.timeline.tracks[index].cell_count;
            for attack in &track.attacks {
                if attack.cell >= cells || usize::from(attack.string) >= track.tuning.len() {
                    return Err(invalid(format!("track {index} attack address")));
                }
                let pitch = track.tuning[attack.string as usize]
                    + i32::from(attack.fret)
                    + i32::from(track.transpose);
                if !(0..=127).contains(&pitch) {
                    return Err(invalid(format!("track {index} attack pitch")));
                }
            }
            if self.timeline.tracks[index]
                .stops
                .iter()
                .any(|stop| usize::from(stop.string) >= track.tuning.len())
            {
                return Err(invalid(format!("track {index} stop string")));
            }
        }
        Ok(())
    }
}

fn control_message(
    tick: u32,
    track: usize,
    channel: i32,
    controller: i32,
    value: i32,
) -> MidiEvent {
    MidiEvent::new_midi_message(tick, track, channel, 0xB0, controller, value)
}

fn program_message(tick: u32, track: usize, channel: i32, program: i32) -> MidiEvent {
    MidiEvent::new_midi_message(tick, track, channel, 0xC0, program & 0x7f, 0)
}

/// The returned trace is the precise trace handed to `AudioPlayer`.
/// Track-local channels isolate equal pitches on different strings; the
/// player isolates tracks into separate synthesizers.
pub fn build_tbt_events(playback: &TbtPlayback) -> Result<Vec<MidiEvent>, RuxError> {
    playback.validate()?;
    let mut ordered: Vec<(u32, u8, usize, u32, MidiEvent)> = Vec::new();
    let mut sequence = 0u32;
    let mut emit = |priority: u8, track: usize, event: MidiEvent| {
        ordered.push((event.tick, priority, track, sequence, event));
        sequence += 1;
    };
    for (track_idx, track) in playback.tracks.iter().enumerate() {
        let timeline = &playback.timeline.tracks[track_idx];
        let bank = if track.drum {
            128
        } else {
            i32::from(track.bank)
        };
        for channel in 0..track.tuning.len() as i32 {
            emit(
                0,
                track_idx,
                control_message(1, track_idx, channel, 0, bank),
            );
            emit(
                0,
                track_idx,
                program_message(1, track_idx, channel, i32::from(track.program)),
            );
            for (controller, value) in [
                (7, track.volume),
                (10, track.pan),
                (93, track.chorus),
                (91, track.reverb),
            ] {
                emit(
                    0,
                    track_idx,
                    control_message(1, track_idx, channel, controller, i32::from(value)),
                );
            }
        }
        let mut by_cell: BTreeMap<u32, (Vec<u8>, Vec<usize>, Vec<&TbtAttack>)> = BTreeMap::new();
        for stop in &timeline.stops {
            by_cell.entry(stop.cell).or_default().0.push(stop.string);
        }
        for (index, control) in timeline.controls.iter().enumerate() {
            by_cell.entry(control.space).or_default().1.push(index);
        }
        for attack in &track.attacks {
            by_cell.entry(attack.cell).or_default().2.push(attack);
        }
        let mut string_events: [Vec<u32>; 8] = std::array::from_fn(|_| Vec::new());
        for (&cell, (stops, _, attacks)) in &by_cell {
            for string in stops {
                string_events[*string as usize].push(cell);
            }
            for attack in attacks {
                string_events[attack.string as usize].push(cell);
            }
        }
        for cells in &mut string_events {
            cells.sort_unstable();
            cells.dedup();
        }
        let mut active = [None::<i32>; 8];
        let mut lane_program = [track.program & 0x7f; 8];
        let mut current_program = track.program & 0x7f;
        let mut dont_ring = track.program & 0x80 != 0;
        let mut current_tempo = playback.tempo;
        for (cell, (stops, controls, attacks)) in by_cell {
            let tick = ENGINE_START
                .checked_add(playback.timeline.time_at(track_idx, cell)?.rounded_tick()?)
                .ok_or_else(|| invalid("event tick overflow"))?;
            for string in stops {
                if let Some(pitch) = active[string as usize].take() {
                    emit(
                        1,
                        track_idx,
                        MidiEvent::new_note_off(tick, track_idx, pitch, i32::from(string)),
                    );
                }
            }
            let mut controls = controls;
            controls.sort_by_key(|index| timeline.controls[*index].source_order);
            for index in controls {
                let control = &timeline.controls[index];
                let value = i32::from(control.value);
                match control.effect_type {
                    TbtEffectChangeType::Tempo if value > 0 => {
                        current_tempo = value as u32;
                        emit(
                            2,
                            track_idx,
                            MidiEvent::new_tempo_change(tick, value as u32),
                        );
                    }
                    TbtEffectChangeType::Instrument => {
                        current_program = (value & 0x7f) as u8;
                        dont_ring = value & 0x80 != 0;
                        for channel in 0..track.tuning.len() as i32 {
                            if value & 0x8000 != 0 {
                                emit(
                                    2,
                                    track_idx,
                                    control_message(
                                        tick,
                                        track_idx,
                                        channel,
                                        0,
                                        (value >> 8) & 0x7f,
                                    ),
                                );
                            }
                            emit(
                                2,
                                track_idx,
                                program_message(tick, track_idx, channel, value),
                            );
                            lane_program[channel as usize] = (value & 0x7f) as u8;
                        }
                    }
                    TbtEffectChangeType::Volume
                    | TbtEffectChangeType::Pan
                    | TbtEffectChangeType::Chorus
                    | TbtEffectChangeType::Reverb
                    | TbtEffectChangeType::Modulation => {
                        let controller = match control.effect_type {
                            TbtEffectChangeType::Volume => 7,
                            TbtEffectChangeType::Pan => 10,
                            TbtEffectChangeType::Chorus => 93,
                            TbtEffectChangeType::Reverb => 91,
                            _ => 1,
                        };
                        for channel in 0..track.tuning.len() as i32 {
                            emit(
                                2,
                                track_idx,
                                control_message(
                                    tick,
                                    track_idx,
                                    channel,
                                    controller,
                                    value.min(127),
                                ),
                            );
                        }
                    }
                    TbtEffectChangeType::PitchBend => {
                        for channel in 0..track.tuning.len() as i32 {
                            emit(
                                2,
                                track_idx,
                                MidiEvent::new_midi_message(
                                    tick,
                                    track_idx,
                                    channel,
                                    0xE0,
                                    value & 0x7f,
                                    (value >> 7) & 0x7f,
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
            let mut attacks = attacks;
            attacks.sort_by_key(|attack| attack.string);
            if dont_ring && !attacks.is_empty() {
                for (string, sounding) in active.iter_mut().enumerate() {
                    if let Some(pitch) = sounding.take() {
                        emit(
                            1,
                            track_idx,
                            MidiEvent::new_note_off(tick, track_idx, pitch, string as i32),
                        );
                    }
                }
            }
            for attack in attacks {
                let string = attack.string as usize;
                if let Some(pitch) = active[string].take() {
                    emit(
                        1,
                        track_idx,
                        MidiEvent::new_note_off(tick, track_idx, pitch, string as i32),
                    );
                }
                let desired_program = if attack.muted {
                    track.muted_program & 0x7f
                } else {
                    current_program
                };
                if lane_program[string] != desired_program {
                    emit(
                        2,
                        track_idx,
                        program_message(tick, track_idx, string as i32, i32::from(desired_program)),
                    );
                    lane_program[string] = desired_program;
                }
                let pitch =
                    track.tuning[string] + i32::from(attack.fret) + i32::from(track.transpose);
                emit(
                    3,
                    track_idx,
                    MidiEvent::new_note_on(
                        tick,
                        track_idx,
                        pitch,
                        if attack.muted { 70 } else { 95 },
                        string as i32,
                    ),
                );
                if attack.muted {
                    // TabIt mutes last 1/64 second, unless the next event on
                    // this string arrives earlier. Convert at the current BPM.
                    let duration = ((u64::from(current_tempo) + 2) / 4).max(1) as u32;
                    let off_tick = tick
                        .checked_add(duration)
                        .ok_or_else(|| invalid("muted note tick overflow"))?;
                    let next_index = string_events[string].partition_point(|next| *next <= cell);
                    let off_tick = if let Some(next_cell) = string_events[string].get(next_index) {
                        let next_tick = ENGINE_START
                            .checked_add(
                                playback
                                    .timeline
                                    .time_at(track_idx, *next_cell)?
                                    .rounded_tick()?,
                            )
                            .ok_or_else(|| invalid("muted release tick overflow"))?;
                        off_tick.min(next_tick)
                    } else {
                        off_tick
                    };
                    emit(
                        1,
                        track_idx,
                        MidiEvent::new_note_off(off_tick, track_idx, pitch, string as i32),
                    );
                } else {
                    active[string] = Some(pitch);
                }
            }
        }
        let end_tick = ENGINE_START
            .checked_add(
                playback
                    .timeline
                    .time_at(track_idx, timeline.cell_count)?
                    .rounded_tick()?,
            )
            .ok_or_else(|| invalid("end tick overflow"))?;
        for (string, pitch) in active.iter().enumerate() {
            if let Some(pitch) = pitch {
                emit(
                    1,
                    track_idx,
                    MidiEvent::new_note_off(end_tick, track_idx, *pitch, string as i32),
                );
            }
        }
    }
    ordered
        .sort_by_key(|(tick, priority, track, sequence, _)| (*tick, *priority, *track, *sequence));
    Ok(ordered
        .into_iter()
        .map(|(_, _, _, _, event)| event)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::midi_event::MidiEventType;
    use crate::parser::tbt_timeline::{TbtRatioRun, TbtStop, TbtTrackTimeline};
    use crate::parser::tbt_types::{TbtBarLine, TbtEffectChange};

    fn fixture() -> TbtPlayback {
        TbtPlayback {
            timeline: TbtTimeline {
                boundaries: vec![
                    TbtBarLine::default(),
                    TbtBarLine {
                        space: 4,
                        ..Default::default()
                    },
                ],
                tracks: vec![TbtTrackTimeline {
                    cell_count: 5,
                    runs: vec![
                        TbtRatioRun {
                            start_cell: 0,
                            count: 3,
                            numerator: 3,
                            denominator: 2,
                        },
                        TbtRatioRun {
                            start_cell: 3,
                            count: 2,
                            numerator: 1,
                            denominator: 1,
                        },
                    ],
                    stops: vec![TbtStop { cell: 4, string: 0 }],
                    controls: vec![],
                }],
            },
            tempo: 120,
            tracks: vec![TbtPlaybackTrack {
                tuning: vec![40, 45],
                program: 27,
                muted_program: 28,
                bank: 0,
                volume: 100,
                pan: 64,
                chorus: 0,
                reverb: 0,
                drum: false,
                transpose: 0,
                attacks: vec![
                    TbtAttack {
                        cell: 0,
                        string: 0,
                        fret: 5,
                        muted: false,
                    },
                    TbtAttack {
                        cell: 0,
                        string: 1,
                        fret: 0,
                        muted: false,
                    },
                    TbtAttack {
                        cell: 1,
                        string: 0,
                        fret: 6,
                        muted: false,
                    },
                    TbtAttack {
                        cell: 2,
                        string: 0,
                        fret: 7,
                        muted: false,
                    },
                    TbtAttack {
                        cell: 3,
                        string: 0,
                        fret: 9,
                        muted: false,
                    },
                ],
            }],
        }
    }

    #[test]
    fn exact_attack_stop_and_same_pitch_string_isolation() {
        let playback = fixture();
        let events = build_tbt_events(&playback).unwrap();
        let ons: Vec<_> = events
            .iter()
            .filter_map(|event| match event.event {
                MidiEventType::NoteOn(channel, pitch, _) => {
                    Some((event.tick - ENGINE_START, channel, pitch))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            ons,
            [
                (0, 0, 45),
                (0, 1, 45),
                (160, 0, 46),
                (320, 0, 47),
                (480, 0, 49)
            ]
        );
        assert!(events.iter().any(|event| event.tick == ENGINE_START + 720
            && event.event == MidiEventType::NoteOff(0, 49)));
        assert!(!events.iter().any(|event| event.tick == ENGINE_START + 720
            && event.event == MidiEventType::NoteOff(1, 45)));
        assert!(!ons.iter().any(|(tick, _, _)| *tick == 720));
        let restored: TbtPlayback =
            serde_json::from_str(&serde_json::to_string(&playback).unwrap()).unwrap();
        assert_eq!(events, build_tbt_events(&restored).unwrap());
    }

    #[test]
    fn refret_changes_one_pitch_and_keeps_stop() {
        let mut playback = fixture();
        let original = build_tbt_events(&playback).unwrap();
        playback.tracks[0].attacks[4].fret = 10;
        let changed = build_tbt_events(&playback).unwrap();
        assert!(changed.iter().any(|event| event.tick == ENGINE_START + 720
            && event.event == MidiEventType::NoteOff(0, 50)));
        assert!(changed.iter().any(|event| event.tick == ENGINE_START + 480
            && matches!(event.event, MidiEventType::NoteOn(0, 50, _))));
        playback.tracks[0].attacks[4].fret = 9;
        assert_eq!(original, build_tbt_events(&playback).unwrap());
    }

    #[test]
    fn dont_ring_bank_change_and_same_cell_control_order() {
        let mut playback = fixture();
        playback.tracks[0].program |= 0x80;
        playback.timeline.tracks[0].controls = vec![
            TbtEffectChange {
                space: 2,
                raw_id: 4,
                inline: false,
                effect_type: TbtEffectChangeType::Instrument,
                value: 0x85a2,
                source_order: 0,
            },
            TbtEffectChange {
                space: 2,
                raw_id: 5,
                inline: false,
                effect_type: TbtEffectChangeType::Volume,
                value: 20,
                source_order: 1,
            },
            TbtEffectChange {
                space: 2,
                raw_id: 5,
                inline: false,
                effect_type: TbtEffectChangeType::Volume,
                value: 30,
                source_order: 2,
            },
        ];
        let events = build_tbt_events(&playback).unwrap();
        assert!(events.iter().any(|event| event.tick == ENGINE_START + 160
            && event.event == MidiEventType::NoteOff(1, 45)));
        let at_cell: Vec<_> = events
            .iter()
            .filter(|event| event.tick == ENGINE_START + 320)
            .filter_map(|event| match event.event {
                MidiEventType::MidiMessage(0, command, data1, data2) => {
                    Some((command, data1, data2))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            at_cell,
            [(0xB0, 0, 5), (0xC0, 34, 0), (0xB0, 7, 20), (0xB0, 7, 30)]
        );
    }
}
