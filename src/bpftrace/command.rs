//! Pure argv builders for every bpftrace invocation (spec §6.4, §6.5, D-013).
//!
//! bpftrace parses its options with GNU `getopt_long`, which *permutes*: an argument
//! that looks like an option is an option wherever it appears, unless it comes after
//! `--`. After option parsing, bpftrace takes the first remaining argument as the script
//! and classifies the rest itself (`--name[=v]` → named param, anything else →
//! positional). So everything user-controlled goes after `--`; that way a parameter value
//! like `--unsafe` or `-o /etc/passwd` can never turn into a bpftrace option.

use std::ffi::OsString;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedArg {
    pub name: String,
    pub value: NamedValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedValue {
    /// `true` → `--name`, `false` → omitted (bpftrace's default for flags).
    Flag(bool),
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs<'a> {
    pub script: &'a Path,
    pub positional: &'a [String],
    pub named: &'a [NamedArg],
    /// Only ever set from the confirmation dialog's explicit toggle (D-009).
    pub allow_unsafe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// bpftrace would read it as a named parameter; there is no way to escape it.
    #[error("positional parameter ${index} cannot start with \"--\": {value:?}")]
    PositionalLooksNamed { index: usize, value: String },
    #[error("invalid named parameter name {0:?}")]
    InvalidName(String),
}

/// `bpftrace -f json -B line [--unsafe] -- <script> [positional…] [--name[=value]…]`
pub fn run_argv(bpftrace: &Path, args: &RunArgs<'_>) -> Result<Vec<OsString>, CommandError> {
    let mut argv: Vec<OsString> = vec![
        bpftrace.into(),
        "-f".into(),
        "json".into(),
        "-B".into(),
        "line".into(),
    ];
    if args.allow_unsafe {
        argv.push("--unsafe".into());
    }
    argv.push("--".into());
    argv.push(args.script.into());
    for (i, value) in args.positional.iter().enumerate() {
        if value.starts_with("--") {
            return Err(CommandError::PositionalLooksNamed {
                index: i + 1,
                value: value.clone(),
            });
        }
        argv.push(value.into());
    }
    for arg in args.named {
        if !is_valid_name(&arg.name) {
            return Err(CommandError::InvalidName(arg.name.clone()));
        }
        match &arg.value {
            NamedValue::Flag(false) => {}
            NamedValue::Flag(true) => argv.push(format!("--{}", arg.name).into()),
            NamedValue::Value(v) => argv.push(format!("--{}={v}", arg.name).into()),
        }
    }
    Ok(argv)
}

/// `bpftrace --dry-run -q -f json -- <script> [0…]`: parse, load and attach, then exit.
/// Positional params the script reads get `0` so the dry run doesn't fail on missing
/// args; named params fall back to the script's defaults. Never `--unsafe` (D-009).
pub fn dry_run_argv(bpftrace: &Path, script: &Path, positional_count: u32) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        bpftrace.into(),
        "--dry-run".into(),
        "-q".into(),
        "-f".into(),
        "json".into(),
        "--".into(),
        script.into(),
    ];
    argv.extend((0..positional_count).map(|_| OsString::from("0")));
    argv
}

/// `bpftrace -l -- <probe>`: lists matching probes, one per line.
pub fn probe_list_argv(bpftrace: &Path, probe: &str) -> Vec<OsString> {
    vec![bpftrace.into(), "-l".into(), "--".into(), probe.into()]
}

pub fn version_argv(bpftrace: &Path) -> Vec<OsString> {
    vec![bpftrace.into(), "--version".into()]
}

pub fn help_argv(bpftrace: &Path) -> Vec<OsString> {
    vec![bpftrace.into(), "--help".into()]
}

/// getopt names as bpftrace scripts write them; `=` or whitespace would change meaning.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const BT: &str = "/usr/bin/bpftrace";

    fn strs(argv: &[OsString]) -> Vec<String> {
        argv.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn named(name: &str, value: NamedValue) -> NamedArg {
        NamedArg {
            name: name.into(),
            value,
        }
    }

    fn run(positional: &[&str], named: &[NamedArg], allow_unsafe: bool) -> Result<Vec<String>, CommandError> {
        let positional: Vec<String> = positional.iter().map(|s| s.to_string()).collect();
        let args = RunArgs {
            script: Path::new("/cache/r/tools/biolatency.bt"),
            positional: &positional,
            named,
            allow_unsafe,
        };
        run_argv(Path::new(BT), &args).map(|a| strs(&a))
    }

    #[test]
    fn plain_run() {
        assert_eq!(
            run(&[], &[], false).expect("argv"),
            vec![
                BT,
                "-f",
                "json",
                "-B",
                "line",
                "--",
                "/cache/r/tools/biolatency.bt"
            ]
        );
    }

    #[test]
    fn positional_and_named() {
        let named = [
            named("interval", NamedValue::Value("5".into())),
            named("verbose", NamedValue::Flag(true)),
            named("quiet", NamedValue::Flag(false)),
            named("sep", NamedValue::Value("a b=c".into())),
        ];
        assert_eq!(
            run(&["1234", "-1", "two words"], &named, false).expect("argv"),
            vec![
                BT,
                "-f",
                "json",
                "-B",
                "line",
                "--",
                "/cache/r/tools/biolatency.bt",
                "1234",
                "-1",
                "two words",
                "--interval=5",
                "--verbose",
                "--sep=a b=c",
            ]
        );
    }

    #[test]
    fn unsafe_only_when_asked_and_always_before_separator() {
        let safe = run(&["--x"], &[], false);
        assert!(safe.is_err());
        let argv = run(&["-e", "--unsafe-looking"], &[], false);
        assert!(matches!(
            argv,
            Err(CommandError::PositionalLooksNamed { index: 2, .. })
        ));

        let argv = run(&["-o", "/etc/passwd", "--unsafe"], &[], false);
        assert!(argv.is_err(), "`--unsafe` as a value must never reach bpftrace");

        let argv = run(&["-o", "/etc/passwd"], &[], false).expect("argv");
        assert!(!argv.contains(&"--unsafe".to_string()));
        let sep = argv.iter().position(|a| a == "--").expect("separator");
        assert_eq!(
            &argv[sep + 2..],
            ["-o", "/etc/passwd"],
            "option-like values stay after --"
        );

        let argv = run(&[], &[], true).expect("argv");
        let unsafe_at = argv.iter().position(|a| a == "--unsafe").expect("--unsafe");
        assert!(unsafe_at < argv.iter().position(|a| a == "--").expect("--"));
    }

    #[test]
    fn named_names_are_validated() {
        for bad in ["", "-x", "a=b", "a b", "x\n"] {
            let err = run(&[], &[named(bad, NamedValue::Flag(true))], false);
            assert!(matches!(err, Err(CommandError::InvalidName(_))), "{bad:?}");
        }
        assert!(run(&[], &[named("max-depth_2", NamedValue::Flag(true))], false).is_ok());
    }

    #[test]
    fn dry_run() {
        let script = Path::new("/s/params_demo.bt");
        assert_eq!(
            strs(&dry_run_argv(Path::new(BT), script, 0)),
            vec![BT, "--dry-run", "-q", "-f", "json", "--", "/s/params_demo.bt"]
        );
        assert_eq!(
            strs(&dry_run_argv(Path::new(BT), script, 2)),
            vec![
                BT,
                "--dry-run",
                "-q",
                "-f",
                "json",
                "--",
                "/s/params_demo.bt",
                "0",
                "0"
            ]
        );
    }

    #[test]
    fn probe_list_and_introspection() {
        assert_eq!(
            strs(&probe_list_argv(Path::new(BT), "kprobe:vfs_*")),
            vec![BT, "-l", "--", "kprobe:vfs_*"]
        );
        assert_eq!(strs(&version_argv(Path::new(BT))), vec![BT, "--version"]);
        assert_eq!(strs(&help_argv(Path::new(BT))), vec![BT, "--help"]);
    }
}
