//! The dispatch-mode contract: each `Code.mode` selects its own table, and
//! the two production modes share every handler except the declared
//! mode-sensitive opcodes.
//!
//! The end-to-end proof that a *frame switch* consults `Code.mode` lives in
//! `tests/strict.rs`: the same bytecode in a strict function throws on a
//! refused `delete`/store while the sloppy one does not, and the strict
//! caller's mode survives a return through a sloppy callee.

use bytecode::Opcode;
use interpreter_become::{TABLE_SLOPPY, TABLE_STRICT, table_for};
use vm_core::{MODE_SLOPPY, MODE_STRICT};

/// Opcodes whose handlers are intentionally duplicated per mode. Keep this
/// in sync with the `modes` section of `handler_table!`.
const MODE_SENSITIVE: &[Opcode] = &[
    Opcode::Delete,
    Opcode::StoreNamedProperty,
    Opcode::StoreKeyedProperty,
    Opcode::StoreNamedPropertyNoShadow,
    Opcode::StoreKeyedPropertyNoShadow,
    Opcode::StoreGlobal,
];

#[test]
fn mode_ids_resolve_to_their_tables() {
    assert!(core::ptr::eq(table_for(MODE_SLOPPY), &TABLE_SLOPPY));
    assert!(core::ptr::eq(table_for(MODE_STRICT), &TABLE_STRICT));
}

#[test]
fn modes_share_every_handler_except_the_declared_ones() {
    for byte in 0..=u8::MAX {
        let Some(opcode) = Opcode::from_byte(byte) else {
            continue;
        };
        let same_narrow = TABLE_STRICT.narrow[byte as usize] as usize
            == TABLE_SLOPPY.narrow[byte as usize] as usize;
        let same_wide =
            TABLE_STRICT.wide[byte as usize] as usize == TABLE_SLOPPY.wide[byte as usize] as usize;
        let expected_same = !MODE_SENSITIVE.contains(&opcode);
        assert_eq!(same_narrow, expected_same, "narrow {opcode:?}");
        assert_eq!(same_wide, expected_same, "wide {opcode:?}");
    }

    // A profile that forgets an override would alias the other mode's
    // handler and fail the loop above; make the intended divergence
    // explicit too, so an accidentally identical pair is caught if LLVM
    // ever merges the bodies.
    for &opcode in MODE_SENSITIVE {
        let byte = opcode as usize;
        assert_ne!(
            TABLE_STRICT.narrow[byte] as usize, TABLE_SLOPPY.narrow[byte] as usize,
            "narrow {opcode:?}"
        );
        assert_ne!(
            TABLE_STRICT.wide[byte] as usize, TABLE_SLOPPY.wide[byte] as usize,
            "wide {opcode:?}"
        );
    }
}
