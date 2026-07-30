//! Size Analyzer route surface for the Iced application.

mod effect;
mod message;
mod state;
mod update;
mod view;

pub use effect::Effect;
pub use message::Message;
pub use state::{ContextMenuRequest, PreviewTarget, State};
pub use update::update;
pub use view::view;
