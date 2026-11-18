//! gitmeta: núcleo de issues e PRs locais para Git.
//! Eventos imutáveis, um arquivo por evento, estado calculado por replay.
#![forbid(unsafe_code)]

pub mod event;
pub mod gitstore;
pub mod state;
pub mod store;
pub mod sync;
pub mod time;

pub use event::{Author, Event, Kind};
pub use gitstore::{GitStore, MergeOutcome};
pub use state::{Item, ItemKind, State};
pub use store::{EventStore, Store, StoreError};
