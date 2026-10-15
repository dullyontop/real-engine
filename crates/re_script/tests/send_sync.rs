//! Thread-safety facts. mlua's `send` feature is off (ADR-0011), so a VM and the
//! runtime stay on the thread that made them; the values they report are `Send`.

fn is_send<T: Send>() {}
fn is_sync<T: Sync>() {}

#[test]
fn reported_values_are_send_and_sync() {
    is_send::<re_script::ScriptError>();
    is_sync::<re_script::ScriptError>();
    is_send::<re_script::OutputLine>();
    is_send::<re_script::ReloadReport>();
    is_send::<re_script::RuntimeConfig>();
}

#[test]
fn compile_check_works_from_any_thread() {
    let n = std::thread::spawn(|| re_script::compile_check("return 1", "t.luau").len())
        .join()
        .unwrap();
    assert_eq!(n, 0);
}
