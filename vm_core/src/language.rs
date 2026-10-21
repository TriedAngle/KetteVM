//! Language modes and dispatch-mode ids.
//!
//! A function's language mode is fixed at compile time. The interpreter
//! does not branch on it per instruction: every [`crate::Code`] carries a
//! `mode` byte, and the frame switch selects the handler table for that
//! mode (see `interpreter_become::table_for`). Handlers that *are*
//! mode-sensitive are instantiated once per mode, so their bodies see the
//! mode as a constant.
//!
//! The stored id is a byte: at most 256 dispatch modes, plenty for
//! strict/sloppy plus non-JS frontends (Kette, ...) without letting the
//! table dimension grow unbounded.

/// ECMAScript strict vs sloppy semantics, as seen by runtime helpers
/// (`[[Set]]`, `delete`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageMode {
    Sloppy,
    Strict,
}

impl LanguageMode {
    /// The mode for the dispatch id stored on [`crate::Code`]. Unknown ids
    /// are sloppy: only registered modes may differ.
    #[inline(always)]
    pub const fn from_mode(mode: u8) -> LanguageMode {
        if mode == MODE_STRICT {
            LanguageMode::Strict
        } else {
            LanguageMode::Sloppy
        }
    }

    #[inline(always)]
    pub const fn from_strict(strict: bool) -> LanguageMode {
        if strict {
            LanguageMode::Strict
        } else {
            LanguageMode::Sloppy
        }
    }

    #[inline(always)]
    pub const fn is_strict(self) -> bool {
        matches!(self, LanguageMode::Strict)
    }
}

/// Dispatch-mode ids stored in [`crate::Code`].
pub const MODE_SLOPPY: u8 = 0;
pub const MODE_STRICT: u8 = 1;

#[inline(always)]
pub const fn mode_from_strict(strict: bool) -> u8 {
    if strict { MODE_STRICT } else { MODE_SLOPPY }
}
