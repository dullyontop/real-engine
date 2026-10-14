//! `init_logging` installs a process-wide subscriber, so it has its own test binary.

use re_core::{LogConfig, init_logging};

#[test]
fn second_init_is_re0002() {
    let config = LogConfig {
        env_override: false,
        ..LogConfig::default()
    };
    init_logging(config.clone()).expect("first call installs the subscriber");
    tracing::info!("logging works");
    let err = init_logging(config).expect_err("second call fails");
    assert_eq!(err.code.to_string(), "RE0002");
}
