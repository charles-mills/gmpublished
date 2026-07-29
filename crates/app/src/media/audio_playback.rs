//! Audio playback for the file previewer: a rodio mixer device, door-loop
//! pooling with gain-based eviction, and a per-reference round-robin wave
//! cursor. Sibling of [`crate::media::sounds`], which plays the UI one-shots.

use std::{collections::HashMap, fmt, io::Cursor, sync::Arc, time::Duration};

use rodio::Source as _;

use crate::media::preview_model;

const MAX_DOOR_PLAYERS: usize = 16;
const MIN_DOOR_GAIN: f32 = 0.001;

#[derive(Clone)]
pub struct SharedAudioBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedAudioBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

pub type AudioDecoder = rodio::Decoder<Cursor<SharedAudioBytes>>;

pub fn decoder_from_audio_bytes(
    bytes: Arc<Vec<u8>>,
) -> Result<AudioDecoder, rodio::decoder::DecoderError> {
    rodio::Decoder::try_from(Cursor::new(SharedAudioBytes(bytes)))
}

pub struct AudioPlayback {
    output: rodio::MixerDeviceSink,
    player: rodio::Player,
    door_loops: HashMap<DoorLoopKey, DoorPlayer>,
    door_one_shots: Vec<DoorPlayer>,
    sound_cursors: HashMap<String, usize>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DoorLoopKey {
    content_id: u64,
    door_index: usize,
}

struct DoorPlayer {
    player: rodio::Player,
    gain: f32,
}

/// A door player eligible for eviction when the pool is full, carrying the
/// gain it's compared on so the two player kinds don't need co-varying
/// `Option` slots.
enum EvictionCandidate {
    Loop(DoorLoopKey, f32),
    OneShot(usize, f32),
}

impl EvictionCandidate {
    const fn gain(&self) -> f32 {
        match self {
            Self::Loop(_, gain) | Self::OneShot(_, gain) => *gain,
        }
    }
}

impl AudioPlayback {
    pub fn new() -> Result<Self, rodio::DeviceSinkError> {
        let output = crate::media::audio_output::open_default_sink()?;
        let player = rodio::Player::connect_new(output.mixer());
        Ok(Self {
            output,
            player,
            door_loops: HashMap::new(),
            door_one_shots: Vec::new(),
            sound_cursors: HashMap::new(),
        })
    }

    pub fn play(
        &mut self,
        bytes: Arc<Vec<u8>>,
        resume_at: f32,
    ) -> Result<(), rodio::decoder::DecoderError> {
        let decoder = decoder_from_audio_bytes(bytes)?;
        self.player.stop();
        self.player = rodio::Player::connect_new(self.output.mixer());
        self.player.append(decoder);
        if resume_at.is_finite()
            && resume_at > 0.0
            && let Err(error) = self
                .player
                .try_seek(Duration::from_secs_f32(resume_at.max(0.0)))
        {
            log::debug!("file preview audio seek to {resume_at}s failed: {error}");
        }
        self.player.play();
        Ok(())
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn position_secs(&self) -> f32 {
        self.player.get_pos().as_secs_f32()
    }

    pub fn empty(&self) -> bool {
        self.player.empty()
    }

    pub fn handle_door_audio_event(
        &mut self,
        event: preview_model::DoorAudioEvent,
        door: &preview_model::DoorInstance,
    ) {
        let key = DoorLoopKey {
            content_id: event.content_id,
            door_index: event.door_index,
        };
        match event.kind {
            preview_model::DoorAudioEventKind::MoveStarted => {
                self.stop_door_loop(key);
                if door.class == gmpublished_backend::scene::map::MapDoorClass::PropDoorRotating {
                    if let Some(sound) = door.sounds.move_sound.as_ref() {
                        self.play_door_one_shot(sound, event.gain);
                    }
                } else if let Some(sound) = door.sounds.move_sound.as_ref() {
                    self.start_door_loop(key, sound, event.gain);
                }
            }
            preview_model::DoorAudioEventKind::MoveLoopVolumeChanged => {
                if let Some(looping) = self.door_loops.get_mut(&key) {
                    let gain = event.gain.max(0.0);
                    looping.gain = gain;
                    looping.player.set_volume(gain);
                }
            }
            preview_model::DoorAudioEventKind::MotionEnded { open } => {
                self.stop_door_loop(key);
                let sound = if door.class
                    == gmpublished_backend::scene::map::MapDoorClass::PropDoorRotating
                {
                    if open {
                        door.sounds.open_sound.as_ref()
                    } else {
                        door.sounds.close_sound.as_ref()
                    }
                } else {
                    door.sounds.stop_sound.as_ref()
                };
                if let Some(sound) = sound {
                    self.play_door_one_shot(sound, event.gain);
                }
            }
            preview_model::DoorAudioEventKind::Parked => {
                self.stop_door_loop(key);
            }
        }
    }

    pub fn stop_door_audio(&mut self) {
        for (_, looping) in self.door_loops.drain() {
            looping.player.stop();
        }
        for one_shot in self.door_one_shots.drain(..) {
            one_shot.player.stop();
        }
    }

    fn stop_door_loop(&mut self, key: DoorLoopKey) {
        if let Some(looping) = self.door_loops.remove(&key) {
            looping.player.stop();
        }
    }

    fn start_door_loop(&mut self, key: DoorLoopKey, sound: &preview_model::DoorSound, gain: f32) {
        let gain = gain.max(0.0);
        if gain < MIN_DOOR_GAIN {
            return;
        }
        self.prune_finished_door_players();
        if !self.reserve_door_player_slot(gain) {
            return;
        }
        let Some(bytes) = self.next_door_sound_wave(sound) else {
            return;
        };
        let decoder = match decoder_from_audio_bytes(bytes) {
            Ok(decoder) => decoder,
            Err(error) => {
                log::debug!(
                    "door move loop decode failed for {}: {error}",
                    sound.reference
                );
                return;
            }
        };
        let player = rodio::Player::connect_new(self.output.mixer());
        player.set_volume(gain);
        // Source move WAVs in this content are authored as loops with cue
        // points. Rodio does not consume Source cue metadata here, so playback
        // loops the whole decoded file while the door is moving.
        player.append(decoder.repeat_infinite());
        player.play();
        self.door_loops.insert(key, DoorPlayer { player, gain });
    }

    fn play_door_one_shot(&mut self, sound: &preview_model::DoorSound, gain: f32) {
        let gain = gain.max(0.0);
        if gain < MIN_DOOR_GAIN {
            return;
        }
        self.prune_finished_door_players();
        if !self.reserve_door_player_slot(gain) {
            return;
        }
        let Some(bytes) = self.next_door_sound_wave(sound) else {
            return;
        };
        let decoder = match decoder_from_audio_bytes(bytes) {
            Ok(decoder) => decoder,
            Err(error) => {
                log::debug!(
                    "door one-shot decode failed for {}: {error}",
                    sound.reference
                );
                return;
            }
        };
        let player = rodio::Player::connect_new(self.output.mixer());
        player.set_volume(gain);
        player.append(decoder);
        player.play();
        self.door_one_shots.push(DoorPlayer { player, gain });
    }

    fn next_door_sound_wave(&mut self, sound: &preview_model::DoorSound) -> Option<Arc<Vec<u8>>> {
        if sound.waves.is_empty() {
            return None;
        }
        let key = sound.reference.to_ascii_lowercase();
        let cursor = self.sound_cursors.entry(key).or_default();
        let index = *cursor % sound.waves.len();
        *cursor = cursor.saturating_add(1);
        Some(Arc::clone(&sound.waves[index].bytes))
    }

    fn prune_finished_door_players(&mut self) {
        self.door_one_shots
            .retain(|one_shot| !one_shot.player.empty());
        self.door_loops.retain(|_, looping| !looping.player.empty());
    }

    fn reserve_door_player_slot(&mut self, new_gain: f32) -> bool {
        let active = self.door_loops.len() + self.door_one_shots.len();
        if active < MAX_DOOR_PLAYERS {
            return true;
        }
        let quietest_loop = self
            .door_loops
            .iter()
            .map(|(key, player)| EvictionCandidate::Loop(*key, player.gain));
        let quietest_one_shot = self
            .door_one_shots
            .iter()
            .enumerate()
            .map(|(index, player)| EvictionCandidate::OneShot(index, player.gain));
        let Some(quietest) = quietest_loop.chain(quietest_one_shot).min_by(|a, b| {
            a.gain()
                .partial_cmp(&b.gain())
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            return true;
        };
        if new_gain <= quietest.gain() {
            return false;
        }
        match quietest {
            EvictionCandidate::Loop(key, _) => self.stop_door_loop(key),
            EvictionCandidate::OneShot(index, _) => {
                let one_shot = self.door_one_shots.swap_remove(index);
                one_shot.player.stop();
            }
        }
        true
    }
}

impl fmt::Debug for AudioPlayback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioPlayback")
            .field("output", &self.output)
            .field("empty", &self.player.empty())
            .field("door_loops", &self.door_loops.len())
            .field("door_one_shots", &self.door_one_shots.len())
            .finish()
    }
}
