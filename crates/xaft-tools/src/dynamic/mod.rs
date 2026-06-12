//! Dynamic tool creation — [`ScriptedTool`] and [`DynamicToolFactory`].
//!
//! These types extend the `agtrs-runtime` dynamic tool system with xaft-specific
//! behaviour: workspace sandboxing via `agtrs_shell::CommandExecutor`, approval
//! gate support, and a signal callback hook.

pub mod factory;
pub mod scripted;

pub use factory::DynamicToolFactory;
pub use scripted::ScriptedTool;
