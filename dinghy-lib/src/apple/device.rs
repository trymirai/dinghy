use super::{xcode, AppleSimulatorType};
use crate::apple::AppleDevicePlatform;
use crate::device::make_remote_app_with_name;
use crate::errors::*;
use crate::project::Project;
use crate::utils::LogCommandExt;
use crate::utils::{get_current_verbosity, user_facing_log};
use crate::Build;
use crate::BuildBundle;
use crate::Device;
use crate::DeviceCompatibility;
use crate::Runnable;
use crate::SyncDirSpec;
use colored::Colorize;
use fs_err as fs;
use log::debug;
use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::path::Path;
use std::process::{self, Stdio};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct IosDevice {
    pub id: String,
    pub name: String,
    pub arch_cpu: &'static str,
    rustc_triple: String,
    pub os: String,
    pub coredevice_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppleSimDevice {
    pub id: String,
    pub name: String,
    pub os: String,
    pub sim_type: AppleSimulatorType,
}

unsafe impl Send for IosDevice {}

impl IosDevice {
    pub fn new(
        name: String,
        id: String,
        arch_cpu: &str,
        os: String,
        coredevice_id: Option<String>,
    ) -> Result<IosDevice> {
        let cpu = match &*arch_cpu {
            "arm64" | "arm64e" => "aarch64",
            _ => "armv7",
        };
        Ok(IosDevice {
            name,
            id,
            os,
            arch_cpu: cpu.into(),
            rustc_triple: format!("{}-apple-ios", cpu),
            coredevice_id,
        })
    }

    /// Returns the identifier to use with `xcrun devicectl` commands.
    fn devicectl_id(&self) -> &str {
        self.coredevice_id.as_deref().unwrap_or(&self.id)
    }

    fn is_locked(&self) -> Result<bool> {
        let result = process::Command::new("xcrun")
            .args(
                "devicectl device info lockState --quiet --json-output /dev/stdout --device"
                    .split_whitespace(),
            )
            .arg(self.devicectl_id())
            .log_invocation(1)
            .output()
            .context("Failed to run devicectl device info lockState")?;
        if !result.status.success() {
            bail!("Device lock query failed\n",)
        }
        Ok(
            json::parse(std::str::from_utf8(&result.stdout)?)?["result"]["passcodeRequired"]
                .as_bool()
                .unwrap(),
        )
    }

    fn make_app(
        &self,
        project: &Project,
        build: &Build,
        runnable: &Runnable,
    ) -> Result<BuildBundle> {
        let signing = xcode::look_for_signature_settings(&self.id, &build.apple_config)?
            .pop()
            .ok_or_else(|| anyhow!("no signing identity found"))?;
        let app_id = signing
            .name
            .split(" ")
            .last()
            .ok_or_else(|| anyhow!("no app id ?"))?;

        let mut build_bundle = if let Some(build_bundle) = build.prebuilt_bundle.clone() {
            build_bundle
        } else {
            make_apple_app(project, build, runnable, &app_id, None)?
        };
        if build.prebuilt_bundle.is_some() {
            let target = binary_arch(&build_bundle.bundle_exe)?;
            xcode::add_plist_to_app(&build_bundle, &target, &app_id, None, &build.apple_config)?;
        }
        build_bundle.app_id = Some(app_id.to_owned());

        super::xcode::sign_app(&build_bundle, &signing, &build.apple_config)?;
        Ok(build_bundle)
    }

    fn install_app(
        &self,
        project: &Project,
        build: &Build,
        runnable: &Runnable,
    ) -> Result<BuildBundle> {
        user_facing_log(
            "Installing",
            &format!("{} to {} ({})", build.runnable.id, self.id, self.name),
            0,
        );
        let build_bundle = self.make_app(project, build, runnable)?;
        let bundle = build_bundle.bundle_dir.to_string_lossy();
        let result = process::Command::new("xcrun")
            .args("devicectl device install app --device".split_whitespace())
            .arg(self.devicectl_id())
            .arg(&*bundle)
            .log_invocation(1)
            .status()
            .context("Failed to run devicectl device install app")?;
        if !result.success() {
            bail!("Installation on device failed\n",)
        }
        Ok(build_bundle)
    }

    fn app_id<'a>(
        &self,
        build_bundle: &'a BuildBundle,
    ) -> Result<&'a str> {
        build_bundle
            .app_id
            .as_deref()
            .ok_or_else(|| anyhow!("No app_id in build bundle"))
    }

    fn launch_app_with_devicectl(
        &self,
        app_id: &str,
        args: &[&str],
        envs: &[&str],
    ) -> Result<process::ExitStatus> {
        let mut cmd = process::Command::new("xcrun");
        cmd.args(
            "devicectl device process launch --console --terminate-existing --device"
                .split_whitespace(),
        );
        cmd.arg(self.devicectl_id());

        if !envs.is_empty() {
            let env_json = format!(
                "{{{}}}",
                envs.iter()
                    .filter_map(|e| {
                        let mut parts = e.splitn(2, '=');
                        let key = parts.next()?;
                        let val = parts.next().unwrap_or("");
                        Some(format!("\"{}\": \"{}\"", key, val))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            cmd.arg("--environment-variables").arg(&env_json);
        }

        cmd.arg(app_id);
        cmd.args(args);
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        cmd.stdin(Stdio::inherit());

        cmd.log_invocation(1)
            .status()
            .context("Failed to run devicectl device process launch")
    }

    fn probe_device_path_exists(
        &self,
        build_bundle: &BuildBundle,
        device_path: &str,
    ) -> Result<bool> {
        let app_id = self.app_id(build_bundle)?;
        let status = process::Command::new("xcrun")
            .args(
                "devicectl device info files --domain-type appDataContainer --device"
                    .split_whitespace(),
            )
            .arg(self.devicectl_id())
            .arg("--domain-identifier")
            .arg(app_id)
            .arg("--subdirectory")
            .arg(device_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .log_invocation(2)
            .status()
            .context("Failed to run devicectl device info files")?;
        Ok(status.success())
    }

    fn sync_dirs_to_device(
        &self,
        build_bundle: &BuildBundle,
        sync_dirs: &[SyncDirSpec],
    ) -> Result<()> {
        for spec in sync_dirs {
            if !spec.host_path.exists() {
                debug!(
                    "Host path {} missing; creating empty directory before sync to device",
                    spec.host_path.display()
                );
                fs::create_dir_all(&spec.host_path)?;
            }
            self.copy_to_device(build_bundle, &spec.host_path, &spec.device_path)?;
        }
        Ok(())
    }

    fn sync_dirs_from_device(
        &self,
        build_bundle: &BuildBundle,
        sync_dirs: &[SyncDirSpec],
    ) -> Result<()> {
        for spec in sync_dirs {
            if !self.probe_device_path_exists(build_bundle, &spec.device_path)? {
                debug!(
                    "Device path {} missing; leaving host path {} untouched",
                    spec.device_path,
                    spec.host_path.display()
                );
                continue;
            }
            if spec.host_path.exists() {
                fs::remove_dir_all(&spec.host_path)?;
            }
            fs::create_dir_all(&spec.host_path)?;
            self.copy_from_device(build_bundle, &spec.device_path, &spec.host_path)?;
        }
        Ok(())
    }

    fn run_remote(
        &self,
        build: &Build,
        build_bundle: &BuildBundle,
        args: &[&str],
        envs: &[&str],
    ) -> Result<()> {
        if self.is_locked()? {
            eprint!(
                "{}",
                format!("\n\n      Please unlock {}! ", &self.name).bright_yellow()
            );
            loop {
                std::thread::sleep(Duration::from_millis(300));
                if !self.is_locked()? {
                    eprintln!("{}", "   All good, yay!\n".bright_green());
                    break;
                }
            }
        }

        let app_id = self.app_id(build_bundle)?;
        self.sync_dirs_to_device(build_bundle, &build.setup_args.sync_dirs)?;
        let status = self.launch_app_with_devicectl(app_id, args, envs)?;
        let post_sync_result = self.sync_dirs_from_device(build_bundle, &build.setup_args.sync_dirs);

        if !status.success() {
            if let Err(sync_error) = post_sync_result {
                log::warn!(
                    "Failed to sync directories back from device after unsuccessful run: {}",
                    sync_error
                );
            }
            bail!("Run on device failed (exit code: {:?})", status.code());
        }

        post_sync_result?;
        Ok(())
    }
}

impl Device for IosDevice {
    fn clean_app(&self, _build_bundle: &BuildBundle) -> Result<()> {
        unimplemented!()
    }

    fn debug_app(
        &self,
        project: &Project,
        build: &Build,
        args: &[&str],
        envs: &[&str],
    ) -> Result<BuildBundle> {
        let build_bundle = self.install_app(project, build, &build.runnable)?;
        if get_current_verbosity() < 1 {
            // we log the full command for verbosity > 1, just log a short message when the user
            // didn't ask for verbose output
            user_facing_log(
                "Debugging",
                &format!("{} on {}", build.runnable.id, self.id),
                0,
            );
        }
        self.run_remote(build, &build_bundle, args, envs)?;
        Ok(build_bundle)
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn run_app(
        &self,
        project: &Project,
        build: &Build,
        args: &[&str],
        envs: &[&str],
    ) -> Result<BuildBundle> {
        let build_bundle = self.install_app(project, build, &build.runnable)?;
        if get_current_verbosity() < 1 {
            // we log the full command for verbosity > 1, just log a short message when the user
            // didn't ask for verbose output
            user_facing_log(
                "Running",
                &format!("{} on {}", build.runnable.id, self.id),
                0,
            );
        }
        self.run_remote(build, &build_bundle, args, envs)?;
        Ok(build_bundle)
    }

    fn copy_to_device(
        &self,
        bundle: &BuildBundle,
        host_source: &Path,
        device_destination: &str,
    ) -> Result<()> {
        let app_id = self.app_id(bundle)?;
        user_facing_log(
            "Copying",
            &format!(
                "{} from host to {}",
                host_source.display(),
                device_destination
            ),
            0,
        );
        let status = process::Command::new("xcrun")
            .args(
                "devicectl device copy to --domain-type appDataContainer --device"
                    .split_whitespace(),
            )
            .arg(self.devicectl_id())
            .arg("--domain-identifier")
            .arg(app_id)
            .arg("--source")
            .arg(host_source)
            .arg("--destination")
            .arg(device_destination)
            .log_invocation(1)
            .status()
            .context("Failed to run devicectl device copy to")?;
        if !status.success() {
            bail!(
                "devicectl device copy to failed (exit code: {:?})",
                status.code()
            );
        }
        Ok(())
    }

    fn copy_from_device(
        &self,
        bundle: &BuildBundle,
        device_source: &str,
        host_destination: &Path,
    ) -> Result<()> {
        let app_id = self.app_id(bundle)?;
        user_facing_log(
            "Copying",
            &format!(
                "{} from device to {}",
                device_source,
                host_destination.display()
            ),
            0,
        );
        if let Some(parent) = host_destination.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let status = process::Command::new("xcrun")
            .args(
                "devicectl device copy from --domain-type appDataContainer --device"
                    .split_whitespace(),
            )
            .arg(self.devicectl_id())
            .arg("--domain-identifier")
            .arg(app_id)
            .arg("--source")
            .arg(device_source)
            .arg("--destination")
            .arg(host_destination)
            .log_invocation(1)
            .status()
            .context("Failed to run devicectl device copy from")?;
        if !status.success() {
            bail!(
                "devicectl device copy from failed (exit code: {:?})",
                status.code()
            );
        }
        Ok(())
    }
}

impl AppleSimDevice {
    fn install_app(
        &self,
        project: &Project,
        build: &Build,
        runnable: &Runnable,
    ) -> Result<BuildBundle> {
        user_facing_log(
            "Installing",
            &format!("{} to {}", build.runnable.id, self.id),
            0,
        );
        let build_bundle = self.make_app(project, build, runnable)?;
        let _ = process::Command::new("xcrun")
            .args(&["simctl", "uninstall", &self.id, "Dinghy"])
            .log_invocation(2)
            .status()?;
        let stat = process::Command::new("xcrun")
            .args(&[
                "simctl",
                "install",
                &self.id,
                build_bundle
                    .bundle_dir
                    .to_str()
                    .ok_or_else(|| anyhow!("conversion to string"))?,
            ])
            .log_invocation(1)
            .status()?;
        if stat.success() {
            Ok(build_bundle)
        } else {
            bail!(
                "Failed to install {} for {}",
                runnable.exe.display(),
                self.id
            )
        }
    }

    fn make_app(
        &self,
        project: &Project,
        build: &Build,
        runnable: &Runnable,
    ) -> Result<BuildBundle> {
        if let Some(mut build_bundle) = build.prebuilt_bundle.clone() {
            let target = binary_arch(&build_bundle.bundle_exe)?;
            xcode::add_plist_to_app(&build_bundle, &target, "Dinghy", Some(&self.sim_type), &build.apple_config)?;
            build_bundle.app_id = Some("Dinghy".to_string());
            Ok(build_bundle)
        } else {
            make_apple_app(project, build, runnable, "Dinghy", Some(&self.sim_type))
        }
    }
}

impl Device for AppleSimDevice {
    fn clean_app(&self, _build_bundle: &BuildBundle) -> Result<()> {
        unimplemented!()
    }

    fn debug_app(
        &self,
        project: &Project,
        build: &Build,
        args: &[&str],
        envs: &[&str],
    ) -> Result<BuildBundle> {
        let runnable = &build.runnable;
        let build_bundle = self.install_app(project, build, runnable)?;
        let install_path = String::from_utf8(
            process::Command::new("xcrun")
                .args(&["simctl", "get_app_container", &self.id, "Dinghy"])
                .log_invocation(2)
                .output()?
                .stdout,
        )?;
        if get_current_verbosity() < 1 {
            // we log the full command for verbosity > 1, just log a short message when the user
            // didn't ask for verbose output
            user_facing_log(
                "Debugging",
                &format!("{} on {}", build.runnable.id, self.id),
                0,
            );
        }
        launch_lldb_simulator(&self, &install_path, args, envs, true)?;
        Ok(build_bundle)
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn run_app(
        &self,
        project: &Project,
        build: &Build,
        args: &[&str],
        envs: &[&str],
    ) -> Result<BuildBundle> {
        let build_bundle = self.install_app(&project, &build, &build.runnable)?;
        if get_current_verbosity() < 1 {
            // we log the full command for verbosity > 1, just log a short message when the user
            // didn't ask for verbose output
            user_facing_log(
                "Running",
                &format!("{} on {}", build.runnable.id, self.id),
                0,
            );
        }
        launch_app(&self, args, envs)?;
        Ok(build_bundle)
    }
}

impl Display for IosDevice {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        write!(
            fmt,
            "{} ({} {} {})",
            self.name, self.id, self.arch_cpu, self.os
        )
    }
}

impl Display for AppleSimDevice {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        write!(fmt, "{} ({} sim {})", self.name, self.id, self.os)
    }
}

impl DeviceCompatibility for IosDevice {
    fn is_compatible_with_simulator_platform(&self, platform: &AppleDevicePlatform) -> bool {
        if platform.sim.is_some() {
            return false;
        }

        if platform.toolchain.rustc_triple == self.rustc_triple.as_str() {
            return true;
        }
        return false;
    }
}

impl DeviceCompatibility for AppleSimDevice {
    fn is_compatible_with_simulator_platform(&self, platform: &AppleDevicePlatform) -> bool {
        if let Some(sim) = &platform.sim {
            self.sim_type == *sim
        } else {
            false
        }
    }
}

fn binary_arch(executable: &Path) -> Result<String> {
    let magic = process::Command::new("file")
        .arg(
            executable
                .to_str()
                .ok_or_else(|| anyhow!("path conversion to string: {:?}", executable))?,
        )
        .log_invocation(3)
        .output()?;
    Ok(String::from_utf8(magic.stdout)?
        .split(' ')
        .last()
        .ok_or_else(|| anyhow!("empty magic"))?
        .trim()
        .to_string())
}

fn make_apple_app(
    project: &Project,
    build: &Build,
    runnable: &Runnable,
    app_id: &str,
    sim_type: Option<&AppleSimulatorType>,
) -> Result<BuildBundle> {
    use crate::project;
    let build_bundle = make_remote_app_with_name(project, build, Some("Dinghy.app"))?;
    project::rec_copy(&runnable.exe, build_bundle.bundle_dir.join("Dinghy"), false)?;
    let target = binary_arch(&runnable.exe)?;
    xcode::add_plist_to_app(&build_bundle, &target, app_id, sim_type, &build.apple_config)?;
    Ok(build_bundle)
}

fn launch_app(dev: &AppleSimDevice, app_args: &[&str], _envs: &[&str]) -> Result<()> {
    use std::io::Write;
    let dir = tempfile::TempDir::with_prefix("mobiledevice-rs-lldb")?;
    let tmppath = dir.path();
    let mut install_path = String::from_utf8(
        process::Command::new("xcrun")
            .args(&["simctl", "get_app_container", &dev.id, "Dinghy"])
            .log_invocation(2)
            .output()?
            .stdout,
    )?;
    install_path.pop();
    let stdout = Path::new(&install_path)
        .join("stdout")
        .to_string_lossy()
        .into_owned();
    let stdout_param = &format!("--stdout={}", stdout);
    let mut xcrun_args: Vec<&str> = vec![
        "simctl",
        "launch",
        "--wait-for-debugger",
        stdout_param,
        &dev.id,
        "Dinghy",
    ];
    xcrun_args.extend(app_args);
    debug!("Launching app via xcrun using args: {:?}", xcrun_args);
    let launch_output = process::Command::new("xcrun")
        .args(&xcrun_args)
        .log_invocation(1)
        .output()?;
    let launch_output = String::from_utf8_lossy(&launch_output.stdout);
    debug!("xcrun simctl launch output: {:?}", launch_output);

    // Output from the launch command should be "Dinghy: $PID" which is after the 8th character.
    let dinghy_pid = launch_output.split_at(8).1;

    // Attaching to the processes needs to be done in a script, not a commandline parameter or
    // lldb will say "no simulators found".
    let lldb_script_filename = tmppath.join("lldb-script");
    let mut script = fs::File::create(&lldb_script_filename)?;
    write!(script, "attach {}\n", dinghy_pid)?;
    write!(script, "continue\n")?;
    write!(script, "quit\n")?;
    let output = process::Command::new("lldb")
        .arg("")
        .arg("-s")
        .arg(lldb_script_filename)
        .output()?;
    let test_contents = std::fs::read_to_string(&stdout)
        .with_context(|| format!("Reading llvm stdout from {stdout}"))?;
    println!("{}", test_contents);

    let output: String = String::from_utf8_lossy(&output.stdout).to_string();
    debug!("lldb script: \n{}", output);
    // The stdout from lldb is something like:
    //
    // (lldb) attach 34163
    // Process 34163 stopped
    // * thread #1, stop reason = signal SIGSTOP
    //     frame #0: 0x00000001019cd000 dyld`_dyld_start
    // dyld`_dyld_start:
    // ->  0x1019cd000 <+0>: popq   %rdi
    //     0x1019cd001 <+1>: pushq  $0x0
    //     0x1019cd003 <+3>: movq   %rsp, %rbp
    //     0x1019cd006 <+6>: andq   $-0x10, %rsp
    // Target 0: (Dinghy) stopped.
    // Executable module set to .....
    // Architecture set to: x86_64h-apple-ios-.
    // (lldb) continue
    // Process 34163 resuming
    // Process 34163 exited with status = 101 (0x00000065)
    // (lldb) quit
    //
    // We need the "exit with status" line which is the 3rd from the last
    let exit_status_line = output
        .lines()
        .rev()
        .find(|line| line.contains("exited with status"));
    if let Some(exit_status_line) = exit_status_line {
        let words: Vec<&str> = exit_status_line.split_whitespace().rev().collect();
        if let Some(exit_status) = words.get(1) {
            let exit_status = exit_status.parse::<u32>()?;
            if exit_status == 0 {
                Ok(())
            } else {
                bail!("Test failure, exit code: {}", exit_status)
            }
        } else {
            panic!(
                "Failed to parse lldb exit line for an exit status. {:?}",
                words
            );
        }
    } else {
        panic!("Failed to get the exit status line from lldb: {}", output);
    }
}

fn launch_lldb_simulator(
    dev: &AppleSimDevice,
    installed: &str,
    args: &[&str],
    envs: &[&str],
    debugger: bool,
) -> Result<()> {
    use std::io::Write;
    use std::process::Command;
    let dir = tempfile::TempDir::with_prefix("mobiledevice-rs-lldb")?;
    let tmppath = dir.path();
    let lldb_script_filename = tmppath.join("lldb-script");
    {
        let python_lldb_support = tmppath.join("helpers.py");
        let helper_py = include_str!("helpers.py");
        let helper_py = helper_py.replace("ENV_VAR_PLACEHOLDER", &envs.join("\", \""));
        fs::File::create(&python_lldb_support)?.write_fmt(format_args!("{}", &helper_py))?;
        let mut script = fs::File::create(&lldb_script_filename)?;
        writeln!(script, "platform select ios-simulator")?;
        writeln!(script, "target create {}", installed)?;
        writeln!(script, "script pass")?;
        writeln!(script, "command script import {:?}", python_lldb_support)?;
        writeln!(
            script,
            "command script add -s synchronous -f helpers.start start"
        )?;
        writeln!(
            script,
            "command script add -f helpers.connect_command connect"
        )?;
        writeln!(script, "connect connect://{}", dev.id)?;
        if !debugger {
            writeln!(script, "start {}", args.join(" "))?;
            writeln!(script, "quit")?;
        }
    }

    let stat = Command::new("xcrun")
        .arg("lldb")
        .arg("-Q")
        .arg("-s")
        .arg(lldb_script_filename)
        .log_invocation(1)
        .status()?;
    if stat.success() {
        Ok(())
    } else {
        bail!("LLDB returned error code {:?}", stat.code())
    }
}
