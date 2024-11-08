use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use wasmer::{imports, Instance, Memory, MemoryLocation, MemoryType, Module, Store};

#[test]
fn test_shared_memory_atomics_notify_send() {
    let mut store = Store::default();
    let wat = r#"(module
(import "host" "memory" (memory 10 65536 shared))
)"#;
    let module = Module::new(&store, wat)
        .map_err(|e| format!("{e:?}"))
        .unwrap();

    let mem = Memory::new(&mut store, MemoryType::new(10, Some(65536), true)).unwrap();

    let imports = imports! {
        "host" => {
            "memory" => mem.clone(),
        },
    };

    let _inst = Instance::new(&mut store, &module, &imports).unwrap();

    let mem = if let Some(m) = mem.as_shared(&store) {
        m
    } else {
        return;
    };

    // Test basic notify.
    let mem2 = mem.clone();
    std::thread::spawn(move || loop {
        if mem2.notify(MemoryLocation::new_32(10), 1).unwrap() > 0 {
            break;
        }
    });

    mem.wait(MemoryLocation::new_32(10), None).unwrap();

    // Test wake_all

    let done = Arc::new(AtomicBool::new(false));

    std::thread::spawn({
        let mem = mem.clone();
        let done = done.clone();
        move || {
            while !done.load(Ordering::SeqCst) {
                mem.wake_all_atomic_waiters().ok();
            }
        }
    });

    mem.wait(MemoryLocation::new_32(10), None).unwrap();
    done.store(true, Ordering::SeqCst);
}
