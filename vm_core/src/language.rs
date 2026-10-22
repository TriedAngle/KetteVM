/// ECMAScript strict vs sloppy semantics, as seen by runtime helpers
/// (`[[Set]]`, `delete`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageMode {
    Sloppy,
    Strict,
}

impl LanguageMode {
    /// The mode for the id stored on [`crate::CallableInfoObject`].
    /// Unknown ids are sloppy: only registered modes may differ.
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

/// Language-mode ids stored in [`crate::CallableInfoObject`].
pub const MODE_SLOPPY: u8 = 0;
pub const MODE_STRICT: u8 = 1;

#[inline(always)]
pub const fn mode_from_strict(strict: bool) -> u8 {
    if strict { MODE_STRICT } else { MODE_SLOPPY }
}
