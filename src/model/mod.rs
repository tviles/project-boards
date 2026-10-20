//! Plain domain types. Nothing here knows about GraphQL or the terminal.

pub mod caps;
pub mod detail;
pub mod field;
pub mod filter;
pub mod ids;
pub mod input;
pub mod item;
pub mod project;
pub mod provider;
pub mod refs;
pub mod sort;
pub mod teams;
pub mod view;

pub use caps::*;
pub use detail::*;
pub use field::*;
pub use filter::*;
pub use ids::*;
pub use input::*;
pub use item::*;
pub use project::*;
pub use provider::*;
pub use refs::*;
pub use sort::*;
pub use view::*;
