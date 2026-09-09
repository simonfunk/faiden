//! Hermes ACP integration.
//!
//! * [`protocol`] is the wire and nothing else: pure, IO-free framing.
//! * [`locate`] finds an installed Hermes executable and builds the child
//!   environment.
//! * [`runtime`] owns one subprocess per agent session and turns protocol
//!   traffic into observable state.
//!
//! Faiden owns these children the same way it owns terminals: they do not
//! survive the application's exit, and nothing here ever claims otherwise.

pub mod locate;
pub mod protocol;
pub mod runtime;
