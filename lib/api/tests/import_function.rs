use wasm_bindgen_test::*;

use wasmer::*;

#[wasm_bindgen_test]
#[cfg(feature = "wat")]
async fn pass_i64_between_host_and_plugin() {
    let mut store = Store::default();

    let wat = r#"(module
        (func $add_one_i64 (import "host" "add_one_i64") (param i64) (result i64))
        (func (export "add_three_i64") (param i64) (result i64)
            (i64.add (call $add_one_i64 (i64.add (local.get 0) (i64.const 1))) (i64.const 1))
        )
    )"#;
    let module = Module::from_wat(wat)
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();

    let imports = {
        imports! {
            "host" => {
                "add_one_i64" => Function::new_typed(&mut store, |value: i64| value.wrapping_add(1)),
            },
        }
    };

    let instance = Instance::new(&mut store, &module, &imports, Default::default())
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let add_three_i64 = instance
        .exports
        .get_typed_function::<i64, i64>(&store, "add_three_i64")
        .map_err(|e| format!("{e:?}"))
        .unwrap();

    let mut numbers = Vec::<i64>::new();
    numbers.extend(-4..=4);
    numbers.extend((i64::MAX - 4)..=i64::MAX);
    numbers.extend((i64::MIN)..=(i64::MIN + 4));

    for number in numbers {
        let wasm_result = add_three_i64
            .call(&mut store, number)
            .map_err(|e| format!("{e:?}"))
            .unwrap();
        let compare_result = number.wrapping_add(3);

        assert_eq!(wasm_result, compare_result)
    }
}

#[wasm_bindgen_test]
#[cfg(feature = "wat")]
async fn pass_u64_between_host_and_plugin() {
    let mut store = Store::default();

    let wat = r#"(module
        (func $add_one_u64 (import "host" "add_one_u64") (param i64) (result i64))
        (func (export "add_three_u64") (param i64) (result i64)
            (i64.add (call $add_one_u64 (i64.add (local.get 0) (i64.const 1))) (i64.const 1))
        )
    )"#;
    let module = Module::from_wat(wat)
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();

    let imports = {
        imports! {
            "host" => {
                "add_one_u64" => Function::new_typed(&mut store, |value: u64| value.wrapping_add(1)),
            },
        }
    };

    let instance = Instance::new(&mut store, &module, &imports, Default::default())
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let add_three_u64 = instance
        .exports
        .get_typed_function::<u64, u64>(&store, "add_three_u64")
        .map_err(|e| format!("{e:?}"))
        .unwrap();

    let mut numbers = Vec::<u64>::new();
    numbers.extend(0..=4);
    numbers.extend((u64::MAX / 2 - 4)..=(u64::MAX / 2 + 4));
    numbers.extend((u64::MAX - 4)..=u64::MAX);

    for number in numbers {
        let wasm_result = add_three_u64
            .call(&mut store, number)
            .map_err(|e| format!("{e:?}"))
            .unwrap();
        let compare_result = number.wrapping_add(3);

        assert_eq!(wasm_result, compare_result)
    }
}

#[wasm_bindgen_test]
#[cfg(feature = "wat")]
async fn calling_function_exports() {
    let mut store = Store::default();
    let wat = r#"(module
    (func (export "add") (param $lhs i32) (param $rhs i32) (result i32)
        local.get $lhs
        local.get $rhs
        i32.add)
)"#;
    let module = Module::from_wat(wat).await.unwrap();
    let imports = imports! {
        // "host" => {
        //     "host_func1" => Function::new_typed(&mut store, |p: u64| {
        //         println!("host_func1: Found number {}", p);
        //         // assert_eq!(p, u64::max_value());
        //     }),
        // }
    };
    let instance = Instance::new(&mut store, &module, &imports, Default::default())
        .await
        .unwrap();

    let add: TypedFunction<(i32, i32), i32> =
        instance.exports.get_typed_function(&store, "add").unwrap();

    let result = add.call(&mut store, 10, 20).unwrap();
    assert_eq!(result, 30);
}

#[wasm_bindgen_test]
#[cfg(feature = "wat")]
async fn back_and_forth_with_imports() {
    let mut store = Store::default();
    // We can use the WAT syntax as well!
    let module = Module::from_wat(
        br#"(module
            (func $sum (import "env" "sum") (param i32 i32) (result i32))
            (func (export "add_one") (param i32) (result i32)
                (call $sum (local.get 0) (i32.const 1))
            )
        )"#,
    )
    .await
    .unwrap();

    fn sum(a: i32, b: i32) -> i32 {
        println!("Summing: {}+{}", a, b);
        a + b
    }

    let import_object = imports! {
        "env" => {
            "sum" => Function::new_typed(&mut store, sum),
        }
    };
    let instance = Instance::new(&mut store, &module, &import_object, Default::default())
        .await
        .unwrap();

    let add_one: TypedFunction<i32, i32> = instance
        .exports
        .get_typed_function(&store, "add_one")
        .unwrap();
    add_one.call(&mut store, 1).unwrap();
}
