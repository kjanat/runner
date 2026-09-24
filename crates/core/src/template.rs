//! Argv templates and their rendering.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::capability::{Frozen, ScriptMechanism, ScriptSupport};

/// One position in an argv template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// Concatenate rendered pieces into one argument.
    Concat(&'static [Self]),
    /// An explicitly selected package.
    Package,
    /// A fixed word.
    Lit(&'static str),
    /// The task name or target.
    Task,
    /// The name being executed.
    Name,
    /// Forwarded arguments.
    Args,
    /// A separator, dropped when there are no arguments.
    Sep(&'static str),
    /// The quiet flag for the requested level.
    Quiet,
    /// The frozen flag, when frozen was requested.
    Frozen,
    /// The script flag for the requested policy.
    Scripts,
    /// The file being run.
    File,
    /// The tool-manager operation.
    Op,
    /// The files a test runner is handed.
    Files,
}

/// An argv after the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template(pub &'static [Piece]);

/// Build a template from string literals and bare piece names: `t!["run", Task, Sep("--"), Args]`.
#[macro_export]
macro_rules! t {
    (@acc [$($acc:expr),*]) => {
        $crate::template::Template(&[$($acc),*])
    };
    (@acc [$($acc:expr),*] $lit:literal $(, $($rest:tt)*)?) => {
        $crate::t!(@acc [$($acc,)* $crate::template::Piece::Lit($lit)] $($($rest)*)?)
    };
    (@acc [$($acc:expr),*] $piece:ident ( $arg:literal ) $(, $($rest:tt)*)?) => {
        $crate::t!(@acc [$($acc,)* $crate::template::Piece::$piece($arg)] $($($rest)*)?)
    };
    (@acc [$($acc:expr),*] $piece:ident $(, $($rest:tt)*)?) => {
        $crate::t!(@acc [$($acc,)* $crate::template::Piece::$piece] $($($rest)*)?)
    };
    ($($tokens:tt)*) => {
        $crate::t!(@acc [] $($tokens)*)
    };
}

/// What script policy a render asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptRequest {
    /// The provider's default.
    #[default]
    Default,
    /// Skip scripts.
    Deny,
    /// Run scripts.
    Allow,
}

/// The values a render fills the pieces with.
#[derive(Debug, Clone, Copy, Default)]
pub struct Request<'a> {
    /// The explicitly selected package.
    pub package: Option<&'a str>,
    /// For [`Piece::Task`].
    pub task: Option<&'a str>,
    /// For [`Piece::Name`].
    pub name: Option<&'a str>,
    /// For [`Piece::File`].
    pub file: Option<&'a Path>,
    /// For [`Piece::Op`].
    pub op: Option<&'a str>,
    /// For [`Piece::Args`] and [`Piece::Sep`].
    pub args: &'a [String],
    /// For [`Piece::Files`].
    pub files: &'a [PathBuf],
    /// The quiet template for the requested level, when the provider has one.
    pub quiet: Option<Template>,
    /// The stream switch, rendered after `quiet` at the same position.
    pub stream: Option<Template>,
    /// The provider's frozen mechanism, when frozen was requested.
    pub frozen: Option<Frozen>,
    /// The provider's script mechanisms and the policy requested.
    pub scripts: Option<(ScriptSupport, ScriptRequest)>,
}

/// A rendered argv and the environment it needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// Arguments after the program.
    pub args: Vec<OsString>,
    /// Variables the mechanisms set.
    pub env: Vec<(OsString, OsString)>,
}

impl Template {
    /// Render the template for `request`.
    #[must_use]
    pub fn render(self, request: &Request<'_>) -> Rendered {
        let mut out = Rendered::default();
        if let Some(Frozen::Argv(alternative)) = request.frozen {
            let frozen = Request {
                frozen: None,
                ..*request
            };
            return alternative.render(&frozen);
        }
        for piece in self.0 {
            piece.render(request, &mut out);
        }
        out
    }
}

impl Piece {
    fn render(self, request: &Request<'_>, out: &mut Rendered) {
        match self {
            Self::Package => out.args.extend(request.package.map(OsString::from)),
            Self::Concat(pieces) => {
                let rendered = Template(pieces).render(request);
                let mut word = OsString::new();
                for part in rendered.args {
                    word.push(part);
                }
                out.args.push(word);
                out.env.extend(rendered.env);
            }
            Self::Lit(word) => out.args.push(word.into()),
            Self::Task => out.args.extend(request.task.map(OsString::from)),
            Self::Name => out.args.extend(request.name.map(OsString::from)),
            Self::File => out.args.extend(request.file.map(OsString::from)),
            Self::Op => out.args.extend(request.op.map(OsString::from)),
            Self::Args => out.args.extend(request.args.iter().map(OsString::from)),
            Self::Files => out.args.extend(request.files.iter().map(OsString::from)),
            Self::Sep(sep) => {
                if !request.args.is_empty() {
                    out.args.push(sep.into());
                }
            }
            Self::Quiet => {
                for flags in [request.quiet, request.stream].into_iter().flatten() {
                    out.args
                        .extend(flags.0.iter().filter_map(|piece| match piece {
                            Self::Lit(word) => Some(OsString::from(word)),
                            _ => None,
                        }));
                }
            }
            Self::Frozen => match request.frozen {
                Some(Frozen::Flag(flag)) => out.args.push(flag.into()),
                Some(Frozen::Env(key, value)) => out.env.push((key.into(), value.into())),
                Some(Frozen::Argv(_) | Frozen::Unsupported) | None => {}
            },
            Self::Scripts => {
                let Some((support, wanted)) = request.scripts else {
                    return;
                };
                let mechanism = match wanted {
                    ScriptRequest::Default => return,
                    ScriptRequest::Deny => support.deny,
                    ScriptRequest::Allow => support.allow,
                };
                match mechanism {
                    ScriptMechanism::Flag(flag) => out.args.push(flag.into()),
                    ScriptMechanism::Env(key, value) => {
                        out.env.push((key.into(), value.into()));
                    }
                    ScriptMechanism::Default
                    | ScriptMechanism::Unsupported
                    | ScriptMechanism::Warn(_) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Request, ScriptRequest};
    use crate::capability::{Frozen, ScriptMechanism, ScriptSupport};

    fn words(rendered: &super::Rendered) -> Vec<String> {
        rendered
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn separator_is_dropped_without_args() {
        let template = t!["run", Task, Sep("--"), Args];
        let plain = template.render(&Request {
            task: Some("build"),
            ..Request::default()
        });
        assert_eq!(words(&plain), ["run", "build"]);
        let args = ["--flag".to_owned()];
        let with = template.render(&Request {
            task: Some("build"),
            args: &args,
            ..Request::default()
        });
        assert_eq!(words(&with), ["run", "build", "--", "--flag"]);
    }

    #[test]
    fn quiet_renders_at_its_position() {
        let template = t!["run", Quiet, Task];
        let rendered = template.render(&Request {
            task: Some("build"),
            quiet: Some(t!["--silent"]),
            ..Request::default()
        });
        assert_eq!(words(&rendered), ["run", "--silent", "build"]);
    }

    #[test]
    fn stream_follows_quiet_at_the_same_position() {
        let template = t![Quiet, "run", Task];
        let rendered = template.render(&Request {
            task: Some("build"),
            quiet: Some(t!["--silent"]),
            stream: Some(t!["--use-stderr"]),
            ..Request::default()
        });
        assert_eq!(
            words(&rendered),
            ["--silent", "--use-stderr", "run", "build"]
        );
        let only_stream = template.render(&Request {
            task: Some("build"),
            stream: Some(t!["--use-stderr"]),
            ..Request::default()
        });
        assert_eq!(words(&only_stream), ["--use-stderr", "run", "build"]);
    }

    #[test]
    fn frozen_argv_replaces_the_template() {
        let template = t!["install", Frozen, Scripts];
        let rendered = template.render(&Request {
            frozen: Some(Frozen::Argv(t!["ci", Scripts])),
            scripts: Some((
                ScriptSupport {
                    deny: ScriptMechanism::Flag("--ignore-scripts"),
                    allow: ScriptMechanism::Flag("--no-ignore-scripts"),
                },
                ScriptRequest::Deny,
            )),
            ..Request::default()
        });
        assert_eq!(words(&rendered), ["ci", "--ignore-scripts"]);
    }

    #[test]
    fn env_mechanisms_set_variables_instead_of_flags() {
        let template = t!["install", Frozen, Scripts];
        let rendered = template.render(&Request {
            frozen: Some(Frozen::Flag("--immutable")),
            scripts: Some((
                ScriptSupport {
                    deny: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "false"),
                    allow: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "true"),
                },
                ScriptRequest::Allow,
            )),
            ..Request::default()
        });
        assert_eq!(words(&rendered), ["install", "--immutable"]);
        assert_eq!(
            rendered.env,
            vec![(
                OsString::from("YARN_ENABLE_SCRIPTS"),
                OsString::from("true")
            )]
        );
    }
}
