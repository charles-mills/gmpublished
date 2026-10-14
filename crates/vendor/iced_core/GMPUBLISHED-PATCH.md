Based on the crates.io release of Iced 0.14.0 (MIT). See `LICENSE`.

These three patches work together to provide native map-viewer mouse capture:

- `iced_core`: adds `mouse::Event::Motion` carrying unscaled relative device deltas.
- `iced_runtime`: exposes `window::capture_cursor`, including a fallible result.
- `iced_winit`: forwards Winit mouse device events only to the captured, focused
  window. Cursor capture tries Locked then Confined, hides the pointer, ignores
  absolute cursor motion during capture, and releases automatically on focus loss.
  Widget hover changes cannot make a captured cursor visible again.

The app releases capture on Esc, preview close/replacement, and layout changes,
clears held movement keys, and ignores stale capture failure callbacks.
