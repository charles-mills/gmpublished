/// Dropping a sink with sources still queued is normal when stopping a
/// preview. Disable rodio's error log for that case.
pub fn open_default_sink() -> Result<rodio::MixerDeviceSink, rodio::DeviceSinkError> {
    let mut output = rodio::DeviceSinkBuilder::from_default_device()
        .and_then(rodio::DeviceSinkBuilder::open_stream)?;
    output.log_on_drop(false);
    Ok(output)
}
