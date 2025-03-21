use wasm_bindgen_test::*;

use wasmer::*;

#[wasm_bindgen_test]
#[cfg(feature = "wat")]
async fn exports_work_after_multiple_instances_have_been_freed() {
    let mut store = Store::default();
    let module = Module::from_wat(
        "
(module
  (type $sum_t (func (param i32 i32) (result i32)))
  (func $sum_f (type $sum_t) (param $x i32) (param $y i32) (result i32)
    local.get $x
    local.get $y
    i32.add)
  (export \"sum\" (func $sum_f)))
",
    )
    .await
    .map_err(|e| format!("{e:?}"))
    .unwrap();

    let imports = Imports::new();
    let instance = Instance::new(&mut store, &module, &imports, Default::default())
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let instance2 = instance.clone();
    let instance3 = instance.clone();

    // The function is cloned to “break” the connection with `instance`.
    let sum = instance
        .exports
        .get_function("sum")
        .map_err(|e| format!("{e:?}"))
        .unwrap()
        .clone();

    drop(instance);
    drop(instance2);
    drop(instance3);

    // All instances have been dropped, but `sum` continues to work!
    assert_eq!(
        sum.call(&mut store, &[Value::I32(1), Value::I32(2)])
            .map_err(|e| format!("{e:?}"))
            .unwrap()
            .into_vec(),
        vec![Value::I32(3)],
    );
}

#[wasm_bindgen_test]
fn unit_native_function_env() {
    let mut store = Store::default();

    #[derive(Clone)]
    struct Env {
        multiplier: u32,
    }

    fn imported_fn(env: FunctionEnvMut<Env>, args: &[Value]) -> Result<Vec<Value>, RuntimeError> {
        let value = env.data().multiplier * args[0].unwrap_i32() as u32;
        Ok(vec![Value::I32(value as _)])
    }

    // We create the environment
    let env = Env { multiplier: 3 };
    // We move the environment to the store, so it can be used by the `Function`
    let env = FunctionEnv::new(&mut store, env);

    let imported_signature = FunctionType::new(vec![Type::I32], vec![Type::I32]);
    let imported = Function::new_with_env(&mut store, &env, imported_signature, imported_fn);

    let expected = vec![Value::I32(12)].into_boxed_slice();
    let result = imported
        .call(&mut store, &[Value::I32(4)])
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    assert_eq!(result, expected);
}
