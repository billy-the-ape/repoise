use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(args)
        .output()
        .expect("CLI should start")
}

#[test]
fn greeting_is_honest_about_readiness() {
    let output = cli(&[]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Hello from Repoise!"));
    assert!(text.contains("not implemented yet"));
}

#[test]
fn help_and_version_are_successful() {
    for flag in ["--help", "-h"] {
        let output = cli(&[flag]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("Usage: repoise")
        );
    }
    for flag in ["--version", "-V"] {
        let output = cli(&[flag]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("repoise {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn unsupported_input_fails_without_success_output() {
    for args in [&["index"][..], &["--unknown"], &["--help", "extra"]] {
        let output = cli(args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains("--help"));
    }
}
