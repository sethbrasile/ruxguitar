//! Exact TabIt score time. Shared boundaries use ordinary sixteenth spaces;
//! track cells use their own ruler and must never be treated as spaces.

use crate::parser::tbt_types::{TbtBarLine, TbtEffectChange, TbtSong};
use crate::RuxError;
use serde::{Deserialize, Serialize};

const TICKS_PER_SPACE: u128 = 240;
const MAX_CELLS: u32 = 1_000_000;

fn invalid(message: impl Into<String>) -> RuxError {
    RuxError::ParsingError(format!("Invalid TBT timeline: {}", message.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactTick {
    pub numerator: u128,
    pub denominator: u128,
}

impl ExactTick {
    pub const fn zero() -> Self {
        Self {
            numerator: 0,
            denominator: 1,
        }
    }

    fn new(numerator: u128, denominator: u128) -> Result<Self, RuxError> {
        if denominator == 0 {
            return Err(invalid("zero denominator"));
        }
        let divisor = gcd(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    fn add(self, other: Self) -> Result<Self, RuxError> {
        let left = self
            .numerator
            .checked_mul(other.denominator)
            .ok_or_else(|| invalid("time overflow"))?;
        let right = other
            .numerator
            .checked_mul(self.denominator)
            .ok_or_else(|| invalid("time overflow"))?;
        let numerator = left
            .checked_add(right)
            .ok_or_else(|| invalid("time overflow"))?;
        let denominator = self
            .denominator
            .checked_mul(other.denominator)
            .ok_or_else(|| invalid("time overflow"))?;
        Self::new(numerator, denominator)
    }

    /// Round one absolute position to the nearest MIDI tick, halves upward.
    pub fn rounded_tick(self) -> Result<u32, RuxError> {
        let rounded = self
            .numerator
            .checked_add(self.denominator / 2)
            .ok_or_else(|| invalid("tick overflow"))?
            / self.denominator;
        u32::try_from(rounded).map_err(|_| invalid("tick exceeds u32"))
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let rem = a % b;
        a = b;
        b = rem;
    }
    a
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtRatioRun {
    pub start_cell: u32,
    pub count: u32,
    /// `numerator` track cells occupy `denominator` ordinary spaces.
    pub numerator: u8,
    pub denominator: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtStop {
    pub cell: u32,
    /// Source string index, low string first.
    pub string: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtTrackTimeline {
    pub cell_count: u32,
    pub runs: Vec<TbtRatioRun>,
    pub stops: Vec<TbtStop>,
    pub controls: Vec<TbtEffectChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TbtTimeline {
    /// Ordered shared boundaries, including zero and the end sentinel.
    pub boundaries: Vec<TbtBarLine>,
    pub tracks: Vec<TbtTrackTimeline>,
}

impl TbtTimeline {
    pub fn from_parsed(song: &TbtSong) -> Result<Self, RuxError> {
        let mut boundaries = song.bar_lines.clone();
        if boundaries
            .first()
            .is_none_or(|boundary| boundary.space != 0)
        {
            boundaries.insert(0, TbtBarLine::default());
        }
        if boundaries.len() < 2 {
            return Err(invalid("missing end boundary"));
        }

        let mut tracks = Vec::with_capacity(song.metadata.tracks.len());
        for (track_idx, source) in song.metadata.tracks.iter().enumerate() {
            let cell_count = source
                .space_count
                .unwrap_or(u32::from(song.header.space_count));
            if cell_count == 0 || cell_count > MAX_CELLS {
                return Err(invalid(format!(
                    "track {track_idx} cell count out of range"
                )));
            }
            let alternate = song
                .alternate_times
                .get(track_idx)
                .ok_or_else(|| invalid("missing alternate-time track"))?;
            let mut regions = Vec::<(u32, u8, u8)>::new();
            regions.push((0, 1, 1));
            for change in alternate {
                if change.dsq_position % 2 != 0 {
                    return Err(invalid(format!(
                        "track {track_idx} has half-cell ratio change"
                    )));
                }
                let cell = change.dsq_position / 2;
                if cell >= cell_count || change.numerator == 0 || change.denominator == 0 {
                    return Err(invalid(format!(
                        "track {track_idx} has invalid ratio change"
                    )));
                }
                if cell < regions.last().unwrap().0 {
                    return Err(invalid(format!("track {track_idx} ratios are unsorted")));
                }
                if cell == regions.last().unwrap().0 {
                    regions.pop();
                }
                regions.push((cell, change.numerator, change.denominator));
            }
            let mut runs: Vec<TbtRatioRun> = Vec::with_capacity(regions.len());
            for (index, &(start_cell, numerator, denominator)) in regions.iter().enumerate() {
                let end_cell = regions.get(index + 1).map_or(cell_count, |next| next.0);
                if end_cell == start_cell {
                    continue;
                }
                if let Some(last) = runs.last_mut() {
                    if last.numerator == numerator && last.denominator == denominator {
                        last.count += end_cell - start_cell;
                        continue;
                    }
                }
                runs.push(TbtRatioRun {
                    start_cell,
                    count: end_cell - start_cell,
                    numerator,
                    denominator,
                });
            }
            let stops = song
                .track_notes
                .get(track_idx)
                .ok_or_else(|| invalid("missing note track"))?
                .iter()
                .filter(|note| note.is_stop)
                .map(|note| TbtStop {
                    cell: note.vsq_position / 20,
                    string: note.string,
                })
                .collect();
            let controls = song
                .track_effect_changes
                .get(track_idx)
                .ok_or_else(|| invalid("missing effect track"))?
                .clone();
            tracks.push(TbtTrackTimeline {
                cell_count,
                runs,
                stops,
                controls,
            });
        }
        let timeline = Self { boundaries, tracks };
        timeline.validate()?;
        Ok(timeline)
    }

    pub fn validate(&self) -> Result<(), RuxError> {
        if self.boundaries.len() < 2 || self.boundaries[0].space != 0 {
            return Err(invalid("boundaries must begin at zero and include an end"));
        }
        if self
            .boundaries
            .windows(2)
            .any(|pair| pair[0].space >= pair[1].space)
        {
            return Err(invalid("shared boundaries must increase"));
        }
        let end = self.boundaries.last().unwrap().space;
        for (track_idx, track) in self.tracks.iter().enumerate() {
            if track.cell_count == 0 || track.cell_count > MAX_CELLS {
                return Err(invalid(format!(
                    "track {track_idx} cell count out of range"
                )));
            }
            let mut covered = 0u32;
            for run in &track.runs {
                if run.start_cell != covered
                    || run.count == 0
                    || run.numerator == 0
                    || run.denominator == 0
                {
                    return Err(invalid(format!(
                        "track {track_idx} has noncanonical ratio coverage"
                    )));
                }
                covered = covered
                    .checked_add(run.count)
                    .ok_or_else(|| invalid("cell overflow"))?;
            }
            if covered != track.cell_count {
                return Err(invalid(format!(
                    "track {track_idx} has incomplete ratio coverage"
                )));
            }
            if track
                .stops
                .iter()
                .any(|stop| stop.cell >= track.cell_count || stop.string >= 8)
                || track
                    .controls
                    .iter()
                    .any(|event| event.space >= track.cell_count)
            {
                return Err(invalid(format!(
                    "track {track_idx} event outside local ruler"
                )));
            }
            let actual = self.time_at(track_idx, track.cell_count)?;
            let expected = ExactTick::new(u128::from(end) * TICKS_PER_SPACE, 1)?;
            if actual != expected {
                return Err(invalid(format!(
                    "track {track_idx} ends at {}/{} ticks, expected {}",
                    actual.numerator, actual.denominator, expected.numerator
                )));
            }
            actual.rounded_tick()?;
        }
        Ok(())
    }

    pub fn time_at(&self, track_idx: usize, cell: u32) -> Result<ExactTick, RuxError> {
        let track = self
            .tracks
            .get(track_idx)
            .ok_or_else(|| invalid("track out of range"))?;
        if cell > track.cell_count {
            return Err(invalid("cell out of range"));
        }
        let mut time = ExactTick::zero();
        for run in &track.runs {
            if cell <= run.start_cell {
                break;
            }
            let count = cell.min(run.start_cell + run.count) - run.start_cell;
            let numerator = u128::from(count) * TICKS_PER_SPACE * u128::from(run.denominator);
            time = time.add(ExactTick::new(numerator, u128::from(run.numerator))?)?;
        }
        Ok(time)
    }

    /// Return the shared bar containing an exact tick; the end sentinel has
    /// no bar and returns `None`.
    pub fn bar_at(&self, tick: ExactTick) -> Result<Option<usize>, RuxError> {
        if tick.denominator == 0 {
            return Err(invalid("zero time denominator"));
        }
        let whole = tick.numerator / tick.denominator;
        let end = u128::from(self.boundaries.last().unwrap().space) * TICKS_PER_SPACE;
        if whole > end || (whole == end && tick.numerator % tick.denominator != 0) {
            return Err(invalid("time after score end"));
        }
        if tick == ExactTick::new(end, 1)? {
            return Ok(None);
        }
        let mut index = 0;
        for boundary in &self.boundaries {
            let boundary_tick = u128::from(boundary.space) * TICKS_PER_SPACE;
            let scaled = boundary_tick
                .checked_mul(tick.denominator)
                .ok_or_else(|| invalid("boundary comparison overflow"))?;
            if scaled > tick.numerator {
                break;
            }
            index += 1;
        }
        Ok(Some(index - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(runs: Vec<TbtRatioRun>, count: u32, end_space: u32) -> TbtTimeline {
        TbtTimeline {
            boundaries: vec![
                TbtBarLine::default(),
                TbtBarLine {
                    space: end_space,
                    ..Default::default()
                },
            ],
            tracks: vec![TbtTrackTimeline {
                cell_count: count,
                runs,
                stops: vec![],
                controls: vec![],
            }],
        }
    }

    #[test]
    fn exact_ratios_and_single_boundary_rounding() {
        for (numerator, denominator, count, end_space, positions) in [
            (1, 1, 4, 4, vec![0, 240, 480, 720, 960]),
            (2, 1, 8, 4, vec![0, 120, 240, 360, 480, 600, 720, 840, 960]),
            (3, 2, 3, 2, vec![0, 160, 320, 480]),
            (7, 4, 7, 4, vec![0, 137, 274, 411, 549, 686, 823, 960]),
        ] {
            let timeline = sample(
                vec![TbtRatioRun {
                    start_cell: 0,
                    count,
                    numerator,
                    denominator,
                }],
                count,
                end_space,
            );
            timeline.validate().unwrap();
            for (cell, expected) in positions.iter().enumerate() {
                assert_eq!(
                    timeline
                        .time_at(0, cell as u32)
                        .unwrap()
                        .rounded_tick()
                        .unwrap(),
                    *expected
                );
            }
            assert_eq!(
                timeline
                    .bar_at(timeline.time_at(0, count).unwrap())
                    .unwrap(),
                None
            );
        }
        let timeline = sample(
            vec![TbtRatioRun {
                start_cell: 0,
                count: 7,
                numerator: 7,
                denominator: 4,
            }],
            7,
            4,
        );
        assert_eq!(
            timeline.time_at(0, 1).unwrap(),
            ExactTick::new(960, 7).unwrap()
        );
    }

    #[test]
    fn different_local_counts_meet_shared_boundary() {
        let mut timeline = sample(
            vec![TbtRatioRun {
                start_cell: 0,
                count: 3,
                numerator: 3,
                denominator: 2,
            }],
            3,
            2,
        );
        timeline.tracks.push(TbtTrackTimeline {
            cell_count: 2,
            runs: vec![TbtRatioRun {
                start_cell: 0,
                count: 2,
                numerator: 1,
                denominator: 1,
            }],
            stops: vec![],
            controls: vec![],
        });
        timeline.validate().unwrap();
        assert_eq!(
            timeline.time_at(0, 3).unwrap(),
            timeline.time_at(1, 2).unwrap()
        );
        assert!(timeline.time_at(0, 4).is_err());
        timeline.tracks[1].runs[0].count = 1;
        assert!(timeline.validate().is_err());
    }

    #[test]
    fn empty_bars_silence_and_source_free_roundtrip() {
        let mut timeline = sample(
            vec![TbtRatioRun {
                start_cell: 0,
                count: 8,
                numerator: 1,
                denominator: 1,
            }],
            8,
            8,
        );
        timeline.boundaries.insert(
            1,
            TbtBarLine {
                space: 2,
                ..Default::default()
            },
        );
        timeline.boundaries.insert(
            2,
            TbtBarLine {
                space: 6,
                ..Default::default()
            },
        );
        timeline.tracks[0]
            .stops
            .push(TbtStop { cell: 5, string: 0 });
        timeline.validate().unwrap();
        assert_eq!(
            timeline.bar_at(timeline.time_at(0, 2).unwrap()).unwrap(),
            Some(1)
        );
        assert_eq!(
            timeline.bar_at(timeline.time_at(0, 7).unwrap()).unwrap(),
            Some(2)
        );
        assert_eq!(
            timeline.time_at(0, 5).unwrap().rounded_tick().unwrap(),
            1_200
        );
        let restored: TbtTimeline =
            serde_json::from_str(&serde_json::to_string(&timeline).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, timeline);
        assert!(restored.time_at(0, 9).is_err());
    }
}
