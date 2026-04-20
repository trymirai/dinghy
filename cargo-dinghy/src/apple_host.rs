use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use cargo_metadata::{Dependency, DependencyKind, Package, Target};
use dinghy_lib::device::make_remote_bundle_with_name;
use dinghy_lib::dinghy_config::DinghyWorkspaceConfig;
use dinghy_lib::errors::*;
use dinghy_lib::project::{Project, rec_copy_excl};
use dinghy_lib::utils::LogCommandExt;
use dinghy_lib::{Build, BuildBundle, Platform};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunnerKind {
    Test,
    Bench,
    Binary,
    Example,
}

#[derive(Debug)]
struct ResolvedTarget {
    kind: RunnerKind,
    package: Package,
    source_relative_path: PathBuf,
}

const RUSTFLAGS_ENCODED_SEPARATOR: char = '\x1f';

fn dinghy_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cargo-dinghy should live under the dinghy workspace root")
        .to_path_buf()
}

pub fn prepare_generated_apple_host(
    project: &Project,
    platform: &Arc<Box<dyn Platform>>,
    build: &Build,
    runner_args: &[String],
) -> Result<Option<BuildBundle>> {
    if !platform.rustc_triple().contains("-apple-") {
        return Ok(None);
    }

    let resolved_target = resolve_target(project, build, runner_args)?;
    let dinghy_config = DinghyWorkspaceConfig::from_workspace_metadata(
        &project.metadata.workspace_metadata,
    )?;
    let mut bundle = make_remote_bundle_with_name(project, build, Some("Dinghy.app"))?;
    bundle.bundle_exe = bundle.bundle_dir.join("Dinghy");

    let generated_root = bundle.root_dir.join("apple-host");
    let source_root = generated_root.join("source");
    let workspace_root = generated_root.join("workspace");
    let runner_crate_root = workspace_root.join("runner");
    let host_crate_root = workspace_root.join("host");

    let _ = fs::remove_dir_all(&generated_root);
    fs::create_dir_all(&generated_root)?;
    fs::create_dir_all(&workspace_root)?;
    fs::create_dir_all(runner_crate_root.join("src"))?;
    fs::create_dir_all(host_crate_root.join("src"))?;

    copy_package_sources(
        resolved_target.package.manifest_path.parent().unwrap().as_std_path(),
        &source_root,
    )?;
    rewrite_rust_sources(&source_root, &dinghy_config)?;

    write_workspace_manifest(&workspace_root)?;
    write_runner_manifest(&runner_crate_root, &resolved_target, &dinghy_config)?;
    write_runner_source(&runner_crate_root, &source_root, &resolved_target)?;
    write_host_manifest(&host_crate_root, &dinghy_config)?;
    write_host_main(&host_crate_root)?;
    build_host_app_binary(
        &workspace_root,
        platform.rustc_triple(),
        &bundle.bundle_exe,
        build.runnable.exe.to_string_lossy().contains("/release/"),
        &dinghy_config,
    )?;

    Ok(Some(bundle))
}

fn resolve_target(project: &Project, build: &Build, runner_args: &[String]) -> Result<ResolvedTarget> {
    let package_root = build.runnable.source.clone();
    let current_package = project
        .metadata
        .packages
        .iter()
        .find(|package| {
            package
                .manifest_path
                .parent()
                .map(|path| path.as_std_path() == package_root)
                .unwrap_or(false)
        })
        .cloned()
        .ok_or_else(|| anyhow!("Could not resolve the current Cargo package for {:?}", package_root))?;

    let target_name = logical_target_name(&build.runnable.exe)?;
    let target_kind = infer_runner_kind(&build.runnable.exe, runner_args, &current_package, &target_name)?;
    let target = current_package
        .targets
        .iter()
        .find(|target| {
            target_names_match(&target.name, &target_name) && target_matches_kind(target, target_kind)
        })
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "Could not resolve target '{}' of kind {:?} in package {}",
                target_name,
                target_kind,
                current_package.name
            )
        })?;

    let package_root = current_package.manifest_path.parent().unwrap().as_std_path().to_path_buf();
    let source_relative_path = target
        .src_path
        .strip_prefix(&package_root)
        .map_err(|_| anyhow!("Target source {:?} is not under package root {:?}", target.src_path, package_root))?
        .to_path_buf();

    Ok(ResolvedTarget {
        kind: target_kind,
        package: current_package,
        source_relative_path: source_relative_path.into(),
    })
}

fn logical_target_name(executable: &Path) -> Result<String> {
    let file_name = executable
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .ok_or_else(|| anyhow!("invalid executable name {:?}", executable))?;
    let logical_name = if executable
        .parent()
        .and_then(Path::file_name)
        .and_then(|component| component.to_str())
        == Some("deps")
    {
        file_name
            .rsplit_once('-')
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| file_name.to_string())
    } else {
        file_name.to_string()
    };
    Ok(logical_name)
}

fn infer_runner_kind(
    executable: &Path,
    runner_args: &[String],
    package: &Package,
    target_name: &str,
) -> Result<RunnerKind> {
    if runner_args.iter().any(|arg| arg == "--bench") {
        return Ok(RunnerKind::Bench);
    }

    if executable
        .ancestors()
        .find(|ancestor| ancestor.file_name().and_then(|value| value.to_str()) == Some("examples"))
        .is_some()
    {
        return Ok(RunnerKind::Example);
    }

    if package
        .targets
        .iter()
        .any(|target| target_names_match(&target.name, target_name) && target.kind.iter().any(|kind| kind == "test"))
    {
        return Ok(RunnerKind::Test);
    }

    if package
        .targets
        .iter()
        .any(|target| target_names_match(&target.name, target_name) && target.kind.iter().any(|kind| kind == "example"))
    {
        return Ok(RunnerKind::Example);
    }

    if package
        .targets
        .iter()
        .any(|target| target_names_match(&target.name, target_name) && target.kind.iter().any(|kind| kind == "bin"))
    {
        return Ok(RunnerKind::Binary);
    }

    if package
        .targets
        .iter()
        .any(|target| target_names_match(&target.name, target_name) && is_library_target(target))
    {
        return Ok(RunnerKind::Test);
    }

    bail!("Could not infer runner kind for target '{}'", target_name)
}

fn target_matches_kind(target: &Target, expected_kind: RunnerKind) -> bool {
    match expected_kind {
        RunnerKind::Bench => target.kind.iter().any(|kind| kind == "bench"),
        RunnerKind::Test => target.kind.iter().any(|kind| kind == "test") || is_library_target(target),
        RunnerKind::Binary => target.kind.iter().any(|kind| kind == "bin"),
        RunnerKind::Example => target.kind.iter().any(|kind| kind == "example"),
    }
}

fn is_library_target(target: &Target) -> bool {
    target
        .kind
        .iter()
        .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "dylib" | "staticlib" | "cdylib"))
}

fn target_names_match(candidate: &str, wanted: &str) -> bool {
    candidate == wanted || candidate.replace('-', "_") == wanted.replace('-', "_")
}

fn copy_package_sources(package_root: &Path, destination: &Path) -> Result<()> {
    let mut excludes = vec![package_root.join("target")];
    if package_root.join(".git").exists() {
        excludes.push(package_root.join(".git"));
    }
    rec_copy_excl(package_root, destination, false, &excludes)?;
    Ok(())
}

fn rewrite_rust_sources(root: &Path, config: &DinghyWorkspaceConfig) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rewrite_rust_sources(&path, config)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        let mut source = fs::read_to_string(&path)?;
        source = source.replace("#![feature(custom_test_frameworks)]", "");
        source = source.replace("#![test_runner(crate::bench_runner)]", "");
        source = source.replace("#![reexport_test_harness_main = \"test_main\"]", "");
        source = source.replace("cfg(test)", "cfg(any(test, dinghy_force_test))");
        source = source.replace("#[test]", "#[dinghy_apple_runner_macros::test_case]");
        source = source.replace("#[ignore]", "#[dinghy_apple_runner_macros::ignored]");
        source = source.replace("#[::criterion_macro::criterion]", "#[dinghy_apple_runner_macros::bench_case]");
        source = source.replace("#[criterion_macro::criterion]", "#[dinghy_apple_runner_macros::bench_case]");
        source = source.replace("#[criterion]", "#[dinghy_apple_runner_macros::bench_case]");
        for (from, to) in &config.test_attribute_aliases {
            let pattern = format!("#[{from}]");
            let replacement = format!("#[{to}]");
            source = source.replace(&pattern, &replacement);
        }
        fs::write(path, source)?;
    }
    Ok(())
}

fn write_workspace_manifest(workspace_root: &Path) -> Result<()> {
    fs::write(
        workspace_root.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"runner\", \"host\"]\n",
    )?;
    Ok(())
}

fn custom_cfgs(config: &DinghyWorkspaceConfig) -> BTreeSet<String> {
    let mut cfgs = BTreeSet::from([String::from("dinghy_force_test")]);
    cfgs.extend(config.forward_cfgs.iter().cloned());
    cfgs
}

fn render_check_cfg_line(config: &DinghyWorkspaceConfig) -> String {
    let cfgs = custom_cfgs(config)
        .into_iter()
        .map(|cfg| format!("'cfg({cfg})'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("unexpected_cfgs = {{ level = \"allow\", check-cfg = [{cfgs}] }}")
}

fn appended_cfg_flags(config: &DinghyWorkspaceConfig) -> Vec<String> {
    custom_cfgs(config)
        .into_iter()
        .flat_map(|cfg| {
            let check_cfg = format!("cfg({cfg})");
            ["--cfg".to_string(), cfg, "--check-cfg".to_string(), check_cfg]
        })
        .collect()
}

fn configure_rustflags_env(command: &mut Command, config: &DinghyWorkspaceConfig) {
    let flags = appended_cfg_flags(config);
    if let Ok(mut encoded_rustflags) = env::var("CARGO_ENCODED_RUSTFLAGS") {
        if !encoded_rustflags.is_empty() {
            encoded_rustflags.push(RUSTFLAGS_ENCODED_SEPARATOR);
        }
        encoded_rustflags.push_str(&flags.join(&RUSTFLAGS_ENCODED_SEPARATOR.to_string()));
        command.env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags);
        return;
    }

    let mut rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str(&flags.join(" "));
    command.env("RUSTFLAGS", rustflags);
}

fn write_runner_manifest(
    runner_crate_root: &Path,
    resolved_target: &ResolvedTarget,
    config: &DinghyWorkspaceConfig,
) -> Result<()> {
    let package_root = resolved_target.package.manifest_path.parent().unwrap().as_std_path();
    let dinghy_root = dinghy_workspace_root();
    let mut manifest = String::new();
    manifest.push_str("[package]\n");
    manifest.push_str("name = \"dinghy-generated-apple-runner\"\n");
    manifest.push_str("version = \"0.1.0\"\n");
    manifest.push_str("edition = \"2021\"\n\n");
    manifest.push_str("[lib]\npath = \"src/lib.rs\"\n\n");
    manifest.push_str("[lints.rust]\n");
    manifest.push_str(&render_check_cfg_line(config));
    manifest.push_str("\n\n");
    manifest.push_str("[dependencies]\n");
    manifest.push_str(&format!(
        "dinghy-apple-runner-support = {{ path = {:?} }}\n",
        dinghy_root.join("dinghy-apple-runner-support").canonicalize()?
    ));
    manifest.push_str(&format!(
        "dinghy-apple-runner-macros = {{ path = {:?} }}\n",
        dinghy_root.join("dinghy-apple-runner-macros").canonicalize()?
    ));
    manifest.push_str("inventory = \"0.3\"\n");

    if resolved_target.package.targets.iter().any(is_library_target) {
        manifest.push_str(&format!(
            "{} = {{ path = {:?} }}\n",
            resolved_target.package.name,
            package_root
        ));
    }

    let mut global_deps = Vec::new();
    let mut target_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen_keys = BTreeSet::new();
    for dependency in &resolved_target.package.dependencies {
        if dependency.kind == DependencyKind::Build {
            continue;
        }
        let key = dependency.rename.clone().unwrap_or_else(|| dependency.name.clone());
        if !seen_keys.insert((dependency.target.as_ref().map(|target| target.to_string()), key.clone())) {
            continue;
        }

        let rendered = render_dependency(key, dependency)?;
        if let Some(target) = &dependency.target {
            target_deps
                .entry(target.to_string())
                .or_default()
                .push(rendered);
        } else {
            global_deps.push(rendered);
        }
    }

    for dependency in global_deps {
        manifest.push_str(&dependency);
        manifest.push('\n');
    }

    for (target, dependencies) in target_deps {
        manifest.push('\n');
        manifest.push_str(&format!("[target.{:?}.dependencies]\n", target));
        for dependency in dependencies {
            manifest.push_str(&dependency);
            manifest.push('\n');
        }
    }

    fs::write(runner_crate_root.join("Cargo.toml"), manifest)?;
    Ok(())
}

fn render_dependency(key: String, dependency: &Dependency) -> Result<String> {
    let mut fields = Vec::new();
    if let Some(path) = &dependency.path {
        fields.push(format!("path = {:?}", path.as_std_path()));
    } else {
        fields.push(format!("version = {:?}", dependency.req.to_string()));
    }
    if let Some(rename) = &dependency.rename {
        if rename != &dependency.name {
            fields.push(format!("package = {:?}", dependency.name));
        }
    }
    if !dependency.uses_default_features {
        fields.push("default-features = false".to_string());
    }
    if !dependency.features.is_empty() {
        let features = dependency
            .features
            .iter()
            .map(|feature| format!("{feature:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("features = [{features}]"));
    }
    Ok(format!("{key} = {{ {} }}", fields.join(", ")))
}

fn write_runner_source(
    runner_crate_root: &Path,
    source_root: &Path,
    resolved_target: &ResolvedTarget,
) -> Result<()> {
    let entry_path = source_root.join(&resolved_target.source_relative_path);
    let entry_path = entry_path.canonicalize()?;
    let runner_source = match resolved_target.kind {
        RunnerKind::Bench => format!(
            r#"#![allow(dead_code, unused_imports)]

use std::panic::{{self, AssertUnwindSafe}};

include!({entry_path:?});

fn normalize_name(raw_name: &str) -> &str {{
    raw_name.split_once("::").map(|(_, name)| name).unwrap_or(raw_name)
}}

fn panic_message(payload: Box<dyn ::std::any::Any + Send>) -> String {{
    if let Some(message) = payload.downcast_ref::<String>() {{
        message.clone()
    }} else if let Some(message) = payload.downcast_ref::<&'static str>() {{
        (*message).to_string()
    }} else {{
        "non-string panic payload".to_string()
    }}
}}

pub fn run() -> i32 {{
    let bench_cases = ::inventory::iter::<::dinghy_apple_runner_support::BenchCase>;
    let ignored = ::inventory::iter::<::dinghy_apple_runner_support::IgnoredCase>
        .into_iter()
        .map(|case| case.name)
        .collect::<::std::collections::BTreeSet<_>>();
    let ignored_only = ::std::env::args().any(|arg| arg == "--ignored");

    for bench_case in bench_cases {{
        let case_name = bench_case.name;
        let is_ignored = ignored.contains(case_name);
        if ignored_only != is_ignored {{
            continue;
        }}

        let mut criterion = ::criterion::Criterion::default().configure_from_args();
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| (bench_case.run)(&mut criterion))) {{
            eprintln!(
                "benchmark {{}} ... FAILED: {{}}",
                normalize_name(case_name),
                panic_message(payload)
            );
            return 1;
        }}
    }}

    0
}}
"#
        ),
        RunnerKind::Test => format!(
            r#"#![allow(dead_code, unused_imports)]

use std::collections::BTreeSet;
use std::panic::{{self, AssertUnwindSafe}};

include!({entry_path:?});

fn normalize_name(raw_name: &str) -> &str {{
    raw_name.split_once("::").map(|(_, name)| name).unwrap_or(raw_name)
}}

fn panic_message(payload: Box<dyn ::std::any::Any + Send>) -> String {{
    if let Some(message) = payload.downcast_ref::<String>() {{
        message.clone()
    }} else if let Some(message) = payload.downcast_ref::<&'static str>() {{
        (*message).to_string()
    }} else {{
        "non-string panic payload".to_string()
    }}
}}

fn matches_filter(case_name: &str, filters: &[String], exact: bool) -> bool {{
    let display_name = normalize_name(case_name);
    if filters.is_empty() {{
        return true;
    }}

    filters.iter().any(|filter| {{
        if exact {{
            display_name == filter
        }} else {{
            display_name.contains(filter)
        }}
    }})
}}

pub fn run() -> i32 {{
    let mut filters = Vec::new();
    let mut exact = false;
    let mut list_only = false;
    let mut ignored_only = false;
    for argument in ::std::env::args().skip(1) {{
        match argument.as_str() {{
            "--exact" => exact = true,
            "--list" => list_only = true,
            "--ignored" => ignored_only = true,
            value if !value.starts_with('-') => filters.push(value.to_string()),
            _ => {{}}
        }}
    }}

    let ignored = ::inventory::iter::<::dinghy_apple_runner_support::IgnoredCase>
        .into_iter()
        .map(|case| case.name)
        .collect::<BTreeSet<_>>();
    let mut executed = 0usize;
    let mut failed = 0usize;

    for test_case in ::inventory::iter::<::dinghy_apple_runner_support::TestCase> {{
        let case_name = test_case.name;
        let is_ignored = ignored.contains(case_name);
        if ignored_only != is_ignored {{
            continue;
        }}
        if !matches_filter(case_name, &filters, exact) {{
            continue;
        }}

        let display_name = normalize_name(case_name);
        if list_only {{
            println!("{{display_name}}: test");
            continue;
        }}

        executed += 1;
        match panic::catch_unwind(AssertUnwindSafe(|| (test_case.run)())) {{
            Ok(()) => println!("test {{display_name}} ... ok"),
            Err(payload) => {{
                failed += 1;
                println!(
                    "test {{display_name}} ... FAILED: {{}}",
                    panic_message(payload)
                );
            }}
        }}
    }}

    if list_only {{
        return 0;
    }}

    println!();
    println!(
        "test result: {{}}. {{}} passed; {{}} failed; finished in 0.00s",
        if failed == 0 {{ "ok" }} else {{ "FAILED" }},
        executed.saturating_sub(failed),
        failed
    );

    if failed == 0 {{ 0 }} else {{ 1 }}
}}
"#
        ),
        RunnerKind::Binary | RunnerKind::Example => format!(
            r#"#![allow(dead_code, unused_imports)]

#[path = {entry_path:?}]
mod target_entry;

pub fn run() -> i32 {{
    target_entry::main();
    0
}}
"#
        ),
    };

    fs::write(runner_crate_root.join("src/lib.rs"), runner_source)?;
    Ok(())
}

fn write_host_manifest(host_crate_root: &Path, config: &DinghyWorkspaceConfig) -> Result<()> {
    let package_root = dinghy_workspace_root();
    let manifest = format!(
        r#"[package]
name = "dinghy-generated-apple-host"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "dinghy-generated-apple-host"
path = "src/main.rs"

[dependencies]
dinghy-apple-host-runtime = {{ path = {:?} }}
dinghy-generated-apple-runner = {{ path = "../runner" }}

[lints.rust]
{}
"#,
        package_root.join("dinghy-apple-host-runtime"),
        render_check_cfg_line(config)
    );
    fs::write(host_crate_root.join("Cargo.toml"), manifest)?;
    Ok(())
}

fn write_host_main(host_crate_root: &Path) -> Result<()> {
    fs::write(
        host_crate_root.join("src/main.rs"),
        "fn main() {\n    dinghy_apple_host_runtime::run_application(dinghy_generated_apple_runner::run)\n}\n",
    )?;
    Ok(())
}

fn build_host_app_binary(
    workspace_root: &Path,
    rustc_triple: &str,
    bundle_executable: &Path,
    release: bool,
    config: &DinghyWorkspaceConfig,
) -> Result<()> {
    let cargo = env::var("CARGO")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| PathBuf::from("cargo"));
    let mut command = Command::new(cargo);
    command.arg("build");
    command.arg("--manifest-path");
    command.arg(workspace_root.join("Cargo.toml"));
    command.arg("-p");
    command.arg("dinghy-generated-apple-host");
    command.arg("--bin");
    command.arg("dinghy-generated-apple-host");
    command.arg("--target");
    command.arg(rustc_triple);
    if release {
        command.arg("--release");
    }

    configure_rustflags_env(&mut command, config);

    let status = command.log_invocation(1).status()?;
    if !status.success() {
        bail!("Failed to build generated Apple host app")
    }

    let profile = if release { "release" } else { "debug" };
    let built_binary = workspace_root
        .join("target")
        .join(rustc_triple)
        .join(profile)
        .join("dinghy-generated-apple-host");
    fs::copy(&built_binary, bundle_executable)?;
    Ok(())
}
