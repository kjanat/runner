//! PowerShell, which runs `.ps1` files: `pwsh` when it is installed, else
//! the `powershell` every Windows ships.

use runner_core::{
    Capabilities, Declared, Ecosystem, Evidence, Hooks, Kind, Provider, ProviderId, RunFileCap,
    Signal, SignalId, Weight, t,
};

/// The executable every host is assumed to have.
pub const PROGRAM: &str = if cfg!(windows) { "powershell" } else { "pwsh" };

#[cfg(windows)]
const SIGNALS: &[Signal] = &[Signal::Probe("pwsh"), Signal::Probe("powershell")];
#[cfg(not(windows))]
const SIGNALS: &[Signal] = &[Signal::Probe("pwsh")];

const RUN_FILE: RunFileCap = RunFileCap {
    unsupported: &[],
    program: None,
    extensions: &["ps1"],
    argv: t![
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        File,
        Args
    ],
};

const INTERPRETERS: &[&str] = &["pwsh", "powershell"];

/// PowerShell 7 on the host.
const PWSH: Capabilities = Capabilities {
    file_fallback: true,
    file_interpreters: INTERPRETERS,
    run_file: Some(RunFileCap {
        program: Some("pwsh"),
        ..RUN_FILE
    }),
    ..Capabilities::NONE
};

/// PowerShell.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::PowerShell,
    label: "powershell",
    aliases: &["pwsh"],
    ecosystem: Ecosystem::Any,
    kind: Kind::RUNTIME,
    program: Some(PROGRAM),
    signals: SIGNALS,
    writes: &[],
    caps: Capabilities {
        file_fallback: true,
        file_interpreters: INTERPRETERS,
        variants: &[("pwsh", PWSH)],
        run_file: Some(RUN_FILE),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks {
        before_plan: None,
        after_observe: Some(|_, evidence| Ok(pwsh_variant(evidence))),
    },
};

/// A `pwsh` on the host names the `pwsh` variant.
fn pwsh_variant(evidence: &[Evidence]) -> Vec<Evidence> {
    let mut derived: Vec<Evidence> = Vec::new();
    for item in evidence
        .iter()
        .filter(|e| e.provider == Some(ProviderId::PowerShell) && is_pwsh(&e.at))
    {
        if derived.iter().any(|seen| seen.scope == item.scope) {
            continue;
        }
        derived.push(Evidence {
            provider: Some(ProviderId::PowerShell),
            signal: Some(SignalId(0)),
            at: item.at.clone(),
            scope: item.scope.clone(),
            weight: Weight::Probed,
            declared: Some(Declared::Variant("pwsh".into())),
        });
    }
    derived
}

/// Whether `at` names a `pwsh` executable, with either path separator.
fn is_pwsh(at: &std::path::Path) -> bool {
    at.to_str()
        .and_then(|text| text.rsplit(['/', '\\']).next())
        .map(str::to_ascii_lowercase)
        .is_some_and(|name| name == "pwsh" || name == "pwsh.exe")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use runner_core::{Declared, Evidence, Present, ProviderId, Scope, SignalId, Weight};

    use super::{PROVIDER, pwsh_variant};

    fn probed(at: &str) -> Evidence {
        Evidence {
            provider: Some(ProviderId::PowerShell),
            signal: Some(SignalId(0)),
            at: PathBuf::from(at),
            scope: Scope::Root,
            weight: Weight::Probed,
            declared: None,
        }
    }

    #[test]
    fn pwsh_on_the_host_selects_the_pwsh_variant() {
        let evidence = vec![
            probed("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
            probed("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
        ];
        let derived = pwsh_variant(&evidence);
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].declared, Some(Declared::Variant("pwsh".into())));
        let present = Present {
            provider: ProviderId::PowerShell,
            scope: Scope::Root,
            version: None,
            bin_dirs: Vec::new(),
            because: [evidence, derived].concat(),
        };
        let run_file = PROVIDER.for_present(&present).caps.run_file.unwrap();
        assert_eq!(run_file.program, Some("pwsh"));
    }

    #[test]
    fn windows_powershell_alone_keeps_the_base_program() {
        let evidence = vec![probed(
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        )];
        assert_eq!(pwsh_variant(&evidence), []);
        let present = Present {
            provider: ProviderId::PowerShell,
            scope: Scope::Root,
            version: None,
            bin_dirs: Vec::new(),
            because: evidence,
        };
        let run_file = PROVIDER.for_present(&present).caps.run_file.unwrap();
        assert_eq!(run_file.program, None);
        assert_eq!(PROVIDER.program, Some(super::PROGRAM));
    }
}
