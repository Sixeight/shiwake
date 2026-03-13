use std::{collections::HashSet, path::Path};

use crate::{AnalysisContext, ChangedFile, Confidence, PluginAnalysis, PluginFinding};

use super::helper_process::RevisionHelperInputs;

pub trait PackageSnapshotView {
    fn exports(&self) -> &std::collections::HashMap<String, String>;
    fn implementations(&self) -> &[String];
}

pub trait RevisionSnapshotView {
    type Package: PackageSnapshotView;
    type File;

    fn package_snapshot(&self, dir: &str) -> Option<&Self::Package>;
    fn file_snapshot(&self, path: &str) -> Option<&Self::File>;
}

pub fn analyze_revision_plugin<S, RunHelper, Fallback, AnalyzePackage, AnalyzeFile, AnalyzeTest>(
    ctx: &AnalysisContext,
    helper_inputs: RevisionHelperInputs,
    run_helper: RunHelper,
    fallback_findings: Fallback,
    analyze_package: AnalyzePackage,
    analyze_file: AnalyzeFile,
    analyze_test: AnalyzeTest,
) -> PluginAnalysis
where
    S: RevisionSnapshotView,
    RunHelper: Fn(&Path, &[String]) -> Result<S, String>,
    Fallback: Fn(&AnalysisContext, &str) -> PluginAnalysis,
    AnalyzePackage: Fn(&str, Option<&S::Package>, Option<&S::Package>, &mut Vec<PluginFinding>),
    AnalyzeFile: Fn(&str, Option<&S::File>, Option<&S::File>, &mut Vec<PluginFinding>),
    AnalyzeTest: Fn(&ChangedFile, &mut Vec<PluginFinding>),
{
    if helper_inputs.changed_files.is_empty() {
        return PluginAnalysis::new(Confidence::High, Vec::new());
    }

    let before = match run_helper(
        &helper_inputs.before_workspace,
        &helper_inputs.changed_files,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => return fallback_findings(ctx, &error),
    };
    let after = match run_helper(&helper_inputs.after_workspace, &helper_inputs.changed_files) {
        Ok(snapshot) => snapshot,
        Err(error) => return fallback_findings(ctx, &error),
    };

    let mut findings = Vec::new();

    for (dir, path) in representative_paths_by_dir(&helper_inputs.changed_files) {
        analyze_package(
            &path,
            before.package_snapshot(&dir),
            after.package_snapshot(&dir),
            &mut findings,
        );
    }

    for path in &helper_inputs.changed_files {
        analyze_file(
            path,
            before.file_snapshot(path),
            after.file_snapshot(path),
            &mut findings,
        );
    }

    for file in &ctx.files {
        analyze_test(file, &mut findings);
    }

    PluginAnalysis::new(Confidence::High, findings)
}

fn representative_paths_by_dir(paths: &[String]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut by_dir = Vec::new();

    for path in paths {
        let mut dir = Path::new(path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .to_string();
        if dir.is_empty() {
            dir = String::from(".");
        }

        if seen.insert(dir.clone()) {
            by_dir.push((dir, path.clone()));
        }
    }

    by_dir
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use crate::{
        AnalysisContext, ChangedFile, Confidence, InputKind, PluginAnalysis, PluginFinding,
        ReasonKind,
    };

    use super::{PackageSnapshotView, RevisionSnapshotView, analyze_revision_plugin};
    use crate::plugins::helper_process::RevisionHelperInputs;
    use crate::plugins::support::base_finding;

    #[derive(Clone)]
    struct PackageSnapshot {
        exports: HashMap<String, String>,
        implementations: Vec<String>,
    }

    impl PackageSnapshotView for PackageSnapshot {
        fn exports(&self) -> &HashMap<String, String> {
            &self.exports
        }

        fn implementations(&self) -> &[String] {
            &self.implementations
        }
    }

    #[derive(Clone)]
    struct FileSnapshot {
        counter: u32,
    }

    struct Snapshot {
        packages: HashMap<String, PackageSnapshot>,
        files: HashMap<String, FileSnapshot>,
    }

    impl RevisionSnapshotView for Snapshot {
        type Package = PackageSnapshot;
        type File = FileSnapshot;

        fn package_snapshot(&self, dir: &str) -> Option<&Self::Package> {
            self.packages.get(dir)
        }

        fn file_snapshot(&self, path: &str) -> Option<&Self::File> {
            self.files.get(path)
        }
    }

    #[test]
    fn revision_plugin_runner_combines_package_file_and_test_findings() {
        let ctx = AnalysisContext {
            input_kind: InputKind::GitRevisionRange,
            repo_root: None,
            base_rev: None,
            head_rev: None,
            before_workspace: None,
            after_workspace: None,
            files: vec![
                ChangedFile {
                    path: String::from("pkg/api.go"),
                    old_path: None,
                    new_path: None,
                    added: vec![String::from("func After() {}")],
                    removed: vec![String::from("func Before() {}")],
                    before_source: None,
                    after_source: None,
                    history: None,
                },
                ChangedFile {
                    path: String::from("pkg/api_test.go"),
                    old_path: None,
                    new_path: None,
                    added: vec![String::from("assert.Equal(t, 2, actual)")],
                    removed: vec![String::from("assert.Equal(t, 1, actual)")],
                    before_source: None,
                    after_source: None,
                    history: None,
                },
            ],
        };
        let helper_inputs = RevisionHelperInputs {
            changed_files: vec![String::from("pkg/api.go")],
            before_workspace: Path::new("before").to_path_buf(),
            after_workspace: Path::new("after").to_path_buf(),
        };

        let report = analyze_revision_plugin(
            &ctx,
            helper_inputs,
            |workspace, changed_files| {
                let is_before = workspace == Path::new("before");
                let mut packages = HashMap::new();
                let mut files = HashMap::new();
                packages.insert(
                    String::from("pkg"),
                    PackageSnapshot {
                        exports: if is_before {
                            HashMap::from([(String::from("Before"), String::from("func()"))])
                        } else {
                            HashMap::from([(String::from("After"), String::from("func()"))])
                        },
                        implementations: if is_before {
                            vec![String::from("OldImpl")]
                        } else {
                            vec![]
                        },
                    },
                );
                files.insert(
                    changed_files[0].clone(),
                    FileSnapshot {
                        counter: if is_before { 1 } else { 2 },
                    },
                );
                Ok(Snapshot { packages, files })
            },
            |_, reason| {
                PluginAnalysis::new(
                    Confidence::Medium,
                    vec![base_finding(
                        String::from("pkg/api.go"),
                        ReasonKind::PluginSignal,
                        reason,
                    )],
                )
            },
            |path, before, after, findings| {
                let before_exports = before
                    .map(|snapshot| snapshot.exports().clone())
                    .unwrap_or_default();
                let after_exports = after
                    .map(|snapshot| snapshot.exports().clone())
                    .unwrap_or_default();
                if before_exports != after_exports {
                    findings.push(base_finding(
                        path.to_string(),
                        ReasonKind::GoExportedApiChange,
                        "package changed",
                    ));
                }

                let before_impls = before
                    .map(|snapshot| snapshot.implementations().to_vec())
                    .unwrap_or_default();
                let after_impls = after
                    .map(|snapshot| snapshot.implementations().to_vec())
                    .unwrap_or_default();
                if before_impls != after_impls {
                    findings.push(base_finding(
                        path.to_string(),
                        ReasonKind::GoInterfaceBreak,
                        "implementation changed",
                    ));
                }
            },
            |path, before, after, findings| {
                let before_counter = before.map(|snapshot| snapshot.counter).unwrap_or_default();
                let after_counter = after.map(|snapshot| snapshot.counter).unwrap_or_default();
                if before_counter != after_counter {
                    findings.push(base_finding(
                        path.to_string(),
                        ReasonKind::GoConcurrencyChange,
                        "file changed",
                    ));
                }
            },
            |file, findings| {
                if file.path.ends_with("_test.go") {
                    findings.push(base_finding(
                        file.path.clone(),
                        ReasonKind::GoTestOracleChange,
                        "test changed",
                    ));
                }
            },
        );

        assert_eq!(report.confidence, Confidence::High);
        assert_eq!(report.findings.len(), 4);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == ReasonKind::GoExportedApiChange)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == ReasonKind::GoConcurrencyChange)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.kind == ReasonKind::GoTestOracleChange)
        );
    }

    #[test]
    fn revision_plugin_runner_returns_fallback_when_helper_fails() {
        let ctx = AnalysisContext {
            input_kind: InputKind::GitRevisionRange,
            repo_root: None,
            base_rev: None,
            head_rev: None,
            before_workspace: None,
            after_workspace: None,
            files: vec![ChangedFile {
                path: String::from("pkg/api.go"),
                old_path: None,
                new_path: None,
                added: vec![],
                removed: vec![],
                before_source: None,
                after_source: None,
                history: None,
            }],
        };
        let helper_inputs = RevisionHelperInputs {
            changed_files: vec![String::from("pkg/api.go")],
            before_workspace: Path::new("before").to_path_buf(),
            after_workspace: Path::new("after").to_path_buf(),
        };

        let report = analyze_revision_plugin::<Snapshot, _, _, _, _, _>(
            &ctx,
            helper_inputs,
            |_, _| Err(String::from("boom")),
            |_, reason| {
                PluginAnalysis::new(
                    Confidence::Medium,
                    vec![PluginFinding {
                        path: String::from("pkg/api.go"),
                        kind: ReasonKind::GoAnalysisFallback,
                        message: reason.to_string(),
                        weight_override: None,
                        score_mode: crate::PluginScoreMode::Additive,
                    }],
                )
            },
            |_, _, _, _| {},
            |_, _, _, _| {},
            |_, _| {},
        );

        assert_eq!(report.confidence, Confidence::Medium);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, ReasonKind::GoAnalysisFallback);
        assert!(report.findings[0].message.contains("boom"));
    }
}
