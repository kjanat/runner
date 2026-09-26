//! Provider-owned turbo support.

pub(crate) use runner_providers::extract::turbo::*;
#[cfg(test)]
mod scope_tests {
    use crate::tool::test_support::TempDir;
    use crate::types::TaskSource;
    use std::fs;
    #[test]
    fn a_workspace_scoped_entry_is_the_task_in_that_member_scope() {
        let dir = TempDir::new("turbo-mixed-keys");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        let member = dir.path().join("apps").join("web");
        fs::create_dir_all(&member).expect("member dir should be created");
        fs::write(member.join("package.json"), r#"{ "name": "web" }"#)
            .expect("member package.json should be written");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"build":{},"//#lint":{},"//#format":{"cache":false},"web#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let ctx =
            crate::detect::detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        let mut tasks: Vec<(&str, &str)> = ctx
            .tasks
            .iter()
            .filter(|task| task.source == TaskSource::TurboJson)
            .map(|task| (task.name.as_str(), task.scope()))
            .collect();
        tasks.sort_unstable();

        assert_eq!(
            tasks,
            [
                ("build", "root"),
                ("build", "web"),
                ("format", "root"),
                ("lint", "root"),
            ]
        );
    }
}
