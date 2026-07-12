//! Embedded UI sound effects.

use std::io::Cursor;

const SUCCESS_BYTES: &[u8] = include_bytes!("../../ui/sound/success.ogg");
const ERROR_BYTES: &[u8] = include_bytes!("../../ui/sound/error.ogg");
const BTN_ON_BYTES: &[u8] = include_bytes!("../../ui/sound/btn_on.ogg");
const BTN_OFF_BYTES: &[u8] = include_bytes!("../../ui/sound/btn_off.ogg");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sound {
    Success,
    Error,
    BtnOn,
    BtnOff,
}

impl Sound {
    pub(crate) const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Success => SUCCESS_BYTES,
            Self::Error => ERROR_BYTES,
            Self::BtnOn => BTN_ON_BYTES,
            Self::BtnOff => BTN_OFF_BYTES,
        }
    }
}

/// Plays a bundled sound effect if the sounds setting allows it.
///
/// Each play opens the audio output on a detached thread and drops it when
/// playback finishes, so no audio stream or thread outlives the blip and the
/// idle process stays at 0% CPU. Output failures are silent by design.
pub fn play(sound: Sound, enabled: bool) {
    if !enabled {
        return;
    }

    let spawned = std::thread::Builder::new()
        .name("ui-sound".to_owned())
        .spawn(move || play_blocking(sound));
    if let Err(error) = spawned {
        log::debug!("UI sound thread failed to spawn: {error}");
    }
}

fn play_blocking(sound: Sound) {
    let mut output = match rodio::DeviceSinkBuilder::from_default_device()
        .and_then(rodio::DeviceSinkBuilder::open_stream)
    {
        Ok(output) => output,
        Err(error) => {
            log::debug!("UI sound output unavailable: {error}");
            return;
        }
    };
    output.log_on_drop(false);

    match rodio::play(output.mixer(), Cursor::new(sound.bytes())) {
        Ok(player) => player.sleep_until_end(),
        Err(error) => log::debug!("UI sound failed to play: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::Sound;

    #[test]
    fn all_bundled_sounds_decode() {
        for sound in [Sound::Success, Sound::Error, Sound::BtnOn, Sound::BtnOff] {
            let decoder = rodio::Decoder::new(std::io::Cursor::new(sound.bytes()));
            assert!(decoder.is_ok(), "{sound:?} must decode");
        }
    }
}
