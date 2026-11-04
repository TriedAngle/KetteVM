use vm::{DefaultInterpreter, HeapBackend, JSRuntime, VM, VmCore};

pub fn vm<B: HeapBackend>(config: B::Config) -> VM {
    VM::new::<B, DefaultInterpreter>(config)
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("vm core")
}

/// The VM plus the JS runtime's runtime-function table (interpreting JS
/// bytecode needs it), without GC stress.
pub fn vm_with_js<B: HeapBackend>(config: B::Config) -> VM {
    VM::new::<B, DefaultInterpreter>(config)
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .expect("failed to create heap")
}

pub fn vm_with_builtins<B: HeapBackend>(config: B::Config) -> VM {
    let vm = VM::new::<B, DefaultInterpreter>(config)
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .expect("failed to create heap");
    vm.arm_gc_stress();
    vm
}

/// Instantiates each generic test function once per backend.
#[macro_export]
macro_rules! for_each_backend {
    ($($name:ident),* $(,)?) => {
        mod backend_dummy_heap {
            use super::*;
            $(
                #[test]
                fn $name() {
                    super::$name::<::dummy_heap::DummyHeap>();
                }
            )*
        }
        mod backend_mark_sweep {
            use super::*;
            $(
                #[test]
                fn $name() {
                    super::$name::<::mark_sweep::MarkSweep>();
                }
            )*
        }
    };
}
