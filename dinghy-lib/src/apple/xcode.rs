use super::{AppleSimulatorType, SignatureSettings, SigningIdentity};
use crate::errors::*;
use fs_err as fs;
use log::{debug, trace};
use std::io::Write;
use std::{io, process};

use crate::config::AppleConfiguration;
use crate::utils::LogCommandExt;
use crate::BuildBundle;

pub fn add_plist_to_app(
    bundle: &BuildBundle,
    arch: &str,
    app_bundle_id: &str,
    sim_type: Option<&AppleSimulatorType>,
    apple_config: &AppleConfiguration,
) -> Result<()> {
    let bundle_name = apple_config
        .bundle_name
        .as_deref()
        .unwrap_or("Dinghy");

    let mut plist = fs::File::create(bundle.bundle_dir.join("Info.plist"))?;
    writeln!(plist, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        plist,
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#
    )?;
    writeln!(plist, r#"<plist version="1.0"><dict>"#)?;
    writeln!(
        plist,
        "<key>CFBundleName</key><string>{}</string>",
        bundle_name,
    )?;
    writeln!(
        plist,
        "<key>CFBundleExecutable</key><string>Dinghy</string>",
    )?;
    writeln!(
        plist,
        "<key>CFBundleIdentifier</key><string>{}</string>",
        app_bundle_id
    )?;
    writeln!(plist, "<key>CFBundleVersion</key>")?;
    writeln!(plist, "<string>{}</string>", arch)?;
    writeln!(plist, "<key>CFBundleShortVersionString</key>")?;
    writeln!(plist, "<string>{}</string>", arch)?;
    match sim_type {
        Some(AppleSimulatorType::Ios) | Some(AppleSimulatorType::Tvos) | None => {
            writeln!(plist, "<key>UIRequiredDeviceCapabilities</key>")?;
            writeln!(plist, "<array><string>{}</string></array>", arch)?;
            writeln!(plist, "<key>UILaunchStoryboardName</key>")?;
            writeln!(plist, "<string></string>")?;
            if let Some(target) = &apple_config.deployment_target {
                writeln!(
                    plist,
                    "<key>MinimumOSVersion</key><string>{}</string>",
                    target
                )?;
            }
        }
        Some(AppleSimulatorType::Watchos) => {
            let min_os = apple_config
                .deployment_target
                .as_deref()
                .unwrap_or("8.0");
            writeln!(
                plist,
                "<key>MinimumOSVersion</key><string>{}</string>",
                min_os,
            )?;
            writeln!(plist, "<key>WKApplication</key><true/>",)?;
            writeln!(plist, "<key>WKWatchOnly</key><true/>")?;
        }
    }
    if let Some(extra) = &apple_config.plist_extra {
        for (key, value) in extra {
            write!(plist, "<key>{}</key>", key)?;
            write_toml_value_as_plist(&mut plist, value)?;
            writeln!(plist)?;
        }
    }
    writeln!(plist, r#"</dict></plist>"#)?;
    Ok(())
}

fn write_toml_value_as_plist(writer: &mut impl Write, value: &toml::Value) -> Result<()> {
    match value {
        toml::Value::Boolean(true) => write!(writer, "<true/>")?,
        toml::Value::Boolean(false) => write!(writer, "<false/>")?,
        toml::Value::Integer(n) => write!(writer, "<integer>{}</integer>", n)?,
        toml::Value::Float(f) => write!(writer, "<real>{}</real>", f)?,
        toml::Value::String(s) => write!(writer, "<string>{}</string>", s)?,
        toml::Value::Array(arr) => {
            write!(writer, "<array>")?;
            for item in arr {
                write_toml_value_as_plist(writer, item)?;
            }
            write!(writer, "</array>")?;
        }
        toml::Value::Table(map) => {
            write!(writer, "<dict>")?;
            for (k, v) in map {
                write!(writer, "<key>{}</key>", k)?;
                write_toml_value_as_plist(writer, v)?;
            }
            write!(writer, "</dict>")?;
        }
        toml::Value::Datetime(dt) => write!(writer, "<string>{}</string>", dt)?,
    }
    Ok(())
}

pub fn sign_app(
    bundle: &BuildBundle,
    settings: &SignatureSettings,
    apple_config: &AppleConfiguration,
) -> Result<()> {
    debug!(
        "Will sign {:?} with team: {} using key: {} and profile: {}",
        bundle.bundle_dir, settings.identity.team, settings.identity.name, settings.file
    );

    let entitlements = bundle.root_dir.join("entitlements.xcent");
    debug!("entitlements file: {}", entitlements.to_str().unwrap_or(""));
    let mut plist = fs::File::create(&entitlements)?;
    writeln!(plist, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        plist,
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#
    )?;
    writeln!(plist, r#"<plist version="1.0"><dict>"#)?;
    writeln!(plist, "{}", settings.entitlements)?;
    if let Some(extra) = &apple_config.entitlements_extra {
        for line in extra {
            writeln!(plist, "{}", line)?;
        }
    }
    writeln!(plist, r#"</dict></plist>"#)?;

    let result = process::Command::new("codesign")
        .args(&["-s", &*settings.identity.name, "--entitlements"])
        .arg(entitlements)
        .arg(&bundle.bundle_dir)
        .log_invocation(2)
        .status()?;
    if !result.success() {
        bail!("Failure to sign application: codesign utility returned non-zero");
    }
    Ok(())
}

pub fn look_for_signature_settings(
    device_id: &str,
    apple_config: &AppleConfiguration,
) -> Result<Vec<SignatureSettings>> {
    let identity_regex = ::regex::Regex::new(r#"^ *[0-9]+\) ([A-Z0-9]{40}) "(.+)"$"#)?;
    let subject_regex = ::regex::Regex::new(r#"OU *= *([^,]+)"#)?;
    let mut identities: Vec<SigningIdentity> = vec![];
    let find_identities = process::Command::new("security")
        .args(&["find-identity", "-v", "-p", "codesigning"])
        .log_invocation(3)
        .output()?;
    for line in String::from_utf8(find_identities.stdout)?.split("\n") {
        if let Some(caps) = identity_regex.captures(&line) {
            let name: String = caps[2].into();
            if !name.starts_with("iPhone Developer: ") && !name.starts_with("Apple Development:") {
                continue;
            }
            let subject = process::Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "security find-certificate -a -c \"{}\" -p | openssl x509 -text | \
                     grep Subject:",
                    name
                ))
                .log_invocation(3)
                .output()?;
            let subject = String::from_utf8(subject.stdout)?;
            if let Some(ou) = subject_regex.captures(&subject) {
                identities.push(SigningIdentity {
                    id: caps[1].into(),
                    name: caps[2].into(),
                    team: ou[1].into(),
                })
            }
        }
    }
    if let Some(team) = &apple_config.team_id {
        identities.retain(|id| &id.team == team);
    }
    debug!("Possible signing identities: {:?}", identities);
    let mut settings = vec![];
    let home = dirs::home_dir().expect("can't get HOME dir");
    let profiles_dir = [
        "Library/MobileDevice/Provisioning Profiles",
        "Library/Developer/Xcode/UserData/Provisioning Profiles", // xcode 16 and above
    ];
    for file in profiles_dir
        .iter()
        .filter_map(|path| fs::read_dir(home.join(path)).ok())
        .flatten()
    {
        let file = file?;
        if file.path().starts_with(".")
            || file
                .path()
                .extension()
                .map(|ext| ext.to_string_lossy() != "mobileprovision")
                .unwrap_or(true)
        {
            trace!(
                " - skipping {:?} (not a mobileprovision profile)",
                file.path()
            );
            continue;
        }
        let decoded = process::Command::new("security")
            .arg("cms")
            .arg("-D")
            .arg("-i")
            .arg(file.path())
            .log_invocation(3)
            .output()?;
        let plist = plist::Value::from_reader(io::Cursor::new(&decoded.stdout))
            .with_context(|| format!("While reading profile {:?}", file.path()))?;
        let dict = plist
            .as_dictionary()
            .ok_or_else(|| anyhow!("plist root should be a dictionary"))?;
        let devices = if let Some(d) = dict.get("ProvisionedDevices") {
            d
        } else {
            trace!(" - skipping {:?} (no devices)", file.path());
            continue;
        };
        let devices = if let Some(ds) = devices.as_array() {
            ds
        } else {
            bail!("ProvisionedDevices expected to be array")
        };
        if !devices.contains(&plist::Value::String(device_id.into())) {
            trace!(" - skipping {:?} (not matching target device)", file.path());
            continue;
        }
        let name = dict
            .get("Name")
            .ok_or_else(|| anyhow!(format!("No name in profile {:?}", file.path())))?;
        let name = name
            .as_string()
            .ok_or_else(|| anyhow!("Name should have been a string in {:?}", file.path()))?;
        if !name.ends_with("Dinghy") && !name.ends_with(" *") {
            trace!(" - skipping {:?} (wrong app)", file.path());
            continue;
        }
        // TODO: check date in future
        let team = dict
            .get("TeamIdentifier")
            .ok_or_else(|| anyhow!("no TeamIdentifier"))?;
        let team = team
            .as_array()
            .ok_or_else(|| anyhow!("TeamIdentifier should be an array"))?;
        let team = team
            .first()
            .ok_or_else(|| anyhow!("empty TeamIdentifier"))?
            .as_string()
            .ok_or_else(|| anyhow!("TeamIdentifier should be a String"))?
            .to_string();
        let identity = identities.iter().find(|i| i.team == team);
        if identity.is_none() {
            trace!(
                " - skipping {:?} (no identity in profile for team)",
                file.path()
            );
            continue;
        }
        let identity = identity.unwrap();
        trace!(" - accepting {:?}", file.path());
        let entitlements = String::from_utf8(decoded.stdout)?
            .split("\n")
            .skip_while(|line| !line.contains("<key>Entitlements</key>"))
            .skip(2)
            .take_while(|line| !line.contains("</dict>"))
            .collect::<Vec<&str>>()
            .join("\n");
        settings.push(SignatureSettings {
            entitlements: entitlements,
            file: file
                .path()
                .to_str()
                .ok_or_else(|| anyhow!("filename should be utf8"))?
                .into(),
            name: if let Some(bundle_id) = &apple_config.bundle_id {
                bundle_id.clone()
            } else if name.ends_with(" *") {
                "org.zoy.kali.Dinghy".into()
            } else {
                name.into()
            },
            identity: identity.clone(),
            profile: file.path().to_str().unwrap().into(),
        });
    }
    Ok(settings)
}
