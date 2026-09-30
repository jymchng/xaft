//! `xaft_cli::error` had no tests at all (0/29), so every exit code the
//! binary can produce from a top-level failure was unverified.
//!
//! The load-bearing property is that the exit code is chosen by the error
//! VARIANT, not by its message, and that a `Runtime` error delegates to
//! `RuntimeError::exit_code` rather than flattening every runtime failure
//! into a single code. These tests pin both, plus the `Display` prefixes
//! that the shell sees.

use std::io;

use xaft_cli::error::XaftError;
use xaft_config::ConfigError;
use xaft_runtime::{ExitCode, RuntimeError};

#[test]
fn a_config_error_exits_with_the_config_code_not_the_generic_failure_code() {
    let err = XaftError::Config(ConfigError::UnknownPreset {
        name: "nope".into(),
    });
    assert_eq!(err.exit_code(), ExitCode::CONFIG_ERROR);
    assert_eq!(err.exit_code().code(), 3);
    // A config error is NOT a usage error: the user gave valid arguments,
    // it is their config that is wrong.
    assert_ne!(err.exit_code(), ExitCode::USAGE_ERROR);
    assert_ne!(err.exit_code(), ExitCode::TASK_FAILED);
}

#[test]
fn a_usage_error_exits_two_so_the_shell_can_distinguish_it_from_a_failure() {
    let err = XaftError::Usage("unknown flag --nope".into());
    assert_eq!(err.exit_code(), ExitCode::USAGE_ERROR);
    assert_eq!(err.exit_code().code(), 2);
}

#[test]
fn an_io_error_is_a_task_failure_not_a_config_error() {
    // A missing config file surfaces as ConfigError::Io, but a bare
    // std::io::Error wrapped by the CLI (e.g. a failed write) is a
    // TASK_FAILED. Pinning this keeps the two from being conflated.
    let err = XaftError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    assert_eq!(err.exit_code(), ExitCode::TASK_FAILED);
}

#[test]
fn not_implemented_is_a_task_failure() {
    let err = XaftError::NotImplemented("browser automation".into());
    assert_eq!(err.exit_code(), ExitCode::TASK_FAILED);
}

#[test]
fn a_runtime_error_delegates_to_the_runtime_exit_code_rather_than_flattening_it() {
    // The Runtime arm is `e.exit_code()`, not a constant. If it ever
    // collapses to a single code, a Ctrl-C (130) or a budget exhaustion (4)
    // would be reported to the shell as an ordinary failure (1).
    let cancelled = XaftError::Runtime(RuntimeError::Cancelled("user".into()));
    assert_eq!(cancelled.exit_code(), ExitCode::CANCELLED);
    assert_eq!(cancelled.exit_code().code(), 130);

    let budget = XaftError::Runtime(RuntimeError::BudgetExceeded("tokens".into()));
    assert_eq!(budget.exit_code(), ExitCode::BUDGET_EXCEEDED);
    assert_eq!(budget.exit_code().code(), 4);

    let config = XaftError::Runtime(RuntimeError::Config("bad".into()));
    assert_eq!(config.exit_code(), ExitCode::CONFIG_ERROR);

    // A runtime error with no more specific mapping falls back to 1.
    let other = XaftError::Runtime(RuntimeError::Provider("no such provider".into()));
    assert_eq!(other.exit_code(), ExitCode::TASK_FAILED);
}

#[test]
fn each_variant_renders_its_own_display_prefix() {
    // These prefixes are what the user actually reads. `Usage` and
    // `NotImplemented` are the two that a copy-paste regression would
    // silently change.
    assert!(
        XaftError::Usage("bad flag".into())
            .to_string()
            .starts_with("usage error: ")
    );
    assert!(
        XaftError::NotImplemented("thing".into())
            .to_string()
            .starts_with("not yet implemented: ")
    );
    assert!(
        XaftError::Io(io::Error::other("boom"))
            .to_string()
            .starts_with("I/O error: ")
    );
    assert!(
        XaftError::Config(ConfigError::UnknownPreset { name: "x".into() })
            .to_string()
            .starts_with("configuration error: ")
    );
    // Runtime is transparent: its Display is the inner error's, with no
    // extra prefix, because the runtime errors are already phrased for
    // end users.
    assert_eq!(
        XaftError::Runtime(RuntimeError::Provider("no such provider".into())).to_string(),
        "provider error: no such provider"
    );
}

#[test]
fn the_error_arms_convert_from_their_source_types() {
    // The `#[from]` impls are how the `?` operator is used throughout the
    // CLI; if one is removed every call site stops compiling.
    let from_io: XaftError = io::Error::other("x").into();
    assert!(matches!(from_io, XaftError::Io(_)));

    let from_runtime: XaftError = RuntimeError::Agent("boom".into()).into();
    assert!(matches!(from_runtime, XaftError::Runtime(_)));

    let from_config: XaftError = ConfigError::UnknownProvider { name: "p".into() }.into();
    assert!(matches!(from_config, XaftError::Config(_)));
}

#[test]
fn print_diagnostic_runs_for_every_variant_without_panicking() {
    // `print_diagnostic` is the stderr path. It has three hint branches
    // plus a catch-all; every arm must be reachable without a panic, and
    // the two hint-bearing config variants must be the ones that produce
    // a hint. Output goes to stderr, which the test harness captures.
    for err in [
        XaftError::Config(ConfigError::MissingApiKey {
            provider: "anthropic".into(),
            env_var: "ANTHROPIC_API_KEY".into(),
        }),
        XaftError::Config(ConfigError::Validation {
            section: "model".into(),
            message: "unknown".into(),
        }),
        XaftError::Usage("bad flag".into()),
        XaftError::NotImplemented("thing".into()),
        XaftError::Io(io::Error::other("x")),
        XaftError::Runtime(RuntimeError::Agent("boom".into())),
    ] {
        err.print_diagnostic();
    }
}
