//! Opening the process's audio output device.

/// Opens the default device's sink.
///
/// `log_on_drop(false)` is the non-obvious part: rodio logs at error level
/// when a sink is dropped while still holding sources, which is the normal
/// end of every UI blip and every stopped preview, not a fault.
pub fn open_default_sink() -> Result<rodio::MixerDeviceSink, rodio::DeviceSinkError> {
    let mut output = rodio::DeviceSinkBuilder::from_default_device()
        .and_then(rodio::DeviceSinkBuilder::open_stream)?;
    output.log_on_drop(false);
    Ok(output)
}
