//! Parameters form (spec §5.3): one field per positional `$N` and per `getopt()`.

use crate::bpftrace::command::{NamedArg, NamedValue};
use crate::discovery::metadata::Metadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Positional(u32),
    /// Named value; `default` is the literal default when it is a plain literal.
    Named {
        name: String,
        default: Option<String>,
    },
    /// Boolean getopt: rendered as a checkbox.
    Flag {
        name: String,
        default: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: FieldKind,
    pub value: String,
    pub checked: bool,
    pub help: Option<String>,
}

impl Field {
    pub fn label(&self) -> String {
        match &self.kind {
            FieldKind::Positional(n) => format!("${n}"),
            FieldKind::Named { name, .. } | FieldKind::Flag { name, .. } => format!("--{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamForm {
    pub fields: Vec<Field>,
    pub focus: usize,
    pub uses_argc: bool,
    /// Header USAGE lines, shown as help above the form.
    pub usage: Vec<String>,
    /// Set when the values cannot be turned into a command line.
    pub error: Option<String>,
}

impl ParamForm {
    /// `None` when the script takes no parameters.
    pub fn new(meta: &Metadata) -> Option<Self> {
        let params = &meta.params;
        // bpftrace parameters are positional: `$3` needs `$1` and `$2` to exist too.
        let max = params.positional.last().copied().unwrap_or(0);
        let mut fields: Vec<Field> = (1..=max)
            .map(|n| Field {
                kind: FieldKind::Positional(n),
                value: String::new(),
                checked: false,
                help: (!params.positional.contains(&n)).then(|| "not read by the script".to_string()),
            })
            .collect();
        for p in &params.named {
            let field = if p.is_bool {
                let default = p.default.as_deref() == Some("true");
                Field {
                    kind: FieldKind::Flag {
                        name: p.name.clone(),
                        default,
                    },
                    value: String::new(),
                    checked: default,
                    help: p.description.clone(),
                }
            } else {
                let default = p.default.as_deref().and_then(literal);
                Field {
                    value: default.clone().unwrap_or_default(),
                    kind: FieldKind::Named {
                        name: p.name.clone(),
                        default,
                    },
                    checked: false,
                    help: p.description.clone(),
                }
            };
            fields.push(field);
        }
        if fields.is_empty() {
            return None;
        }
        Some(Self {
            fields,
            focus: 0,
            uses_argc: params.uses_argc,
            usage: meta.usage.clone(),
            error: None,
        })
    }

    pub fn next(&mut self) {
        self.focus = (self.focus + 1) % self.fields.len();
    }

    pub fn prev(&mut self) {
        self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
    }

    /// Type into the focused text field; a space on a checkbox toggles it.
    pub fn input(&mut self, c: char) {
        self.error = None;
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        match field.kind {
            FieldKind::Flag { .. } if c == ' ' => field.checked = !field.checked,
            FieldKind::Flag { .. } => {}
            _ => field.value.push(c),
        }
    }

    pub fn backspace(&mut self) {
        self.error = None;
        if let Some(field) = self.fields.get_mut(self.focus) {
            field.value.pop();
        }
    }

    /// Positional values (up to the last non-empty one; gaps become empty strings) and
    /// the named params that differ from the script's defaults.
    pub fn args(&self) -> (Vec<String>, Vec<NamedArg>) {
        let positional: Vec<&Field> = self
            .fields
            .iter()
            .filter(|f| matches!(f.kind, FieldKind::Positional(_)))
            .collect();
        let last = positional
            .iter()
            .rposition(|f| !f.value.is_empty())
            .map_or(0, |i| i + 1);
        let positional = positional[..last].iter().map(|f| f.value.clone()).collect();

        let named = self
            .fields
            .iter()
            .filter_map(|f| match &f.kind {
                FieldKind::Named { name, default } => {
                    let unchanged = f.value.is_empty() || default.as_deref() == Some(f.value.as_str());
                    (!unchanged).then(|| NamedArg {
                        name: name.clone(),
                        value: NamedValue::Value(f.value.clone()),
                    })
                }
                FieldKind::Flag { name, default } => (f.checked != *default).then(|| NamedArg {
                    name: name.clone(),
                    value: if f.checked {
                        NamedValue::Flag(true)
                    } else {
                        NamedValue::Value("false".into())
                    },
                }),
                FieldKind::Positional(_) => None,
            })
            .collect();
        (positional, named)
    }
}

/// A default we can prefill: a number or a string literal (quotes removed). Expressions
/// like `$a` are left to bpftrace.
fn literal(default: &str) -> Option<String> {
    let d = default.trim();
    if let Some(s) = d.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Some(s.to_string());
    }
    d.parse::<i64>().is_ok().then(|| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::metadata::extract;
    use pretty_assertions::assert_eq;

    fn form(src: &str) -> ParamForm {
        ParamForm::new(&extract(src)).expect("form")
    }

    fn type_str(form: &mut ParamForm, s: &str) {
        for c in s.chars() {
            form.input(c);
        }
    }

    #[test]
    fn no_params_no_form() {
        assert!(ParamForm::new(&extract("BEGIN { printf(\"hi\"); }")).is_none());
    }

    #[test]
    fn fields_from_metadata() {
        let f = form(
            r#"BEGIN { $x = $3 + $1 + $#; getopt("depth", 35, "Max depth"); getopt("name", "abc");
               getopt("verbose"); getopt("color", true); getopt("expr", $x); }"#,
        );
        let labels: Vec<_> = f.fields.iter().map(Field::label).collect();
        assert_eq!(
            labels,
            vec![
                "$1",
                "$2",
                "$3",
                "--depth",
                "--name",
                "--verbose",
                "--color",
                "--expr"
            ]
        );
        assert_eq!(f.fields[1].help.as_deref(), Some("not read by the script"));
        assert_eq!(f.fields[3].value, "35");
        assert_eq!(f.fields[3].help.as_deref(), Some("Max depth"));
        assert_eq!(f.fields[4].value, "abc");
        assert!(!f.fields[5].checked);
        assert!(f.fields[6].checked);
        assert_eq!(f.fields[7].value, "", "non-literal default is not prefilled");
        assert!(f.uses_argc);
    }

    #[test]
    fn untouched_form_passes_nothing() {
        let f = form(r#"BEGIN { $1; getopt("depth", 35); getopt("verbose"); getopt("color", true); }"#);
        assert_eq!(f.args(), (vec![], vec![]));
    }

    #[test]
    fn edits_become_args() {
        let mut f = form(r#"BEGIN { $3; getopt("depth", 35); getopt("verbose"); getopt("color", true); }"#);
        type_str(&mut f, "1234"); // $1
        f.next();
        f.next();
        type_str(&mut f, "x y"); // $3, spaces are text in text fields
        f.next();
        f.backspace();
        f.backspace();
        type_str(&mut f, "10"); // --depth
        f.next();
        f.input(' '); // --verbose on
        f.input('z'); // ignored on a checkbox
        f.next();
        f.input(' '); // --color off (default true)
        let (positional, named) = f.args();
        assert_eq!(positional, vec!["1234", "", "x y"]);
        assert_eq!(
            named,
            vec![
                NamedArg {
                    name: "depth".into(),
                    value: NamedValue::Value("10".into())
                },
                NamedArg {
                    name: "verbose".into(),
                    value: NamedValue::Flag(true)
                },
                NamedArg {
                    name: "color".into(),
                    value: NamedValue::Value("false".into())
                },
            ]
        );
    }

    #[test]
    fn focus_wraps() {
        let mut f = form(r#"BEGIN { $2; }"#);
        assert_eq!(f.focus, 0);
        f.prev();
        assert_eq!(f.focus, 1);
        f.next();
        assert_eq!(f.focus, 0);
    }
}
