//! Static checks of the Windows installer script (the installer itself is compiled and installed /
//! uninstalled on windows-latest by the CI job `test`, step "Installer round trip").

use std::path::Path;

fn installer() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows/installer.iss");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

#[test]
fn installer_removes_its_path_entry_on_uninstall() {
    let iss = installer();
    // The PATH entry is added under HKCU\Environment by the addtopath task ...
    assert!(iss.contains(
        r#"Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath"#
    ));
    // ... and removed again by the uninstaller (not with uninsdeletevalue, which would delete the whole PATH).
    assert!(!iss.contains("uninsdeletevalue"));
    assert!(iss.contains("procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);"));
    assert!(iss.contains("if CurUninstallStep = usUninstall then"));
    assert!(iss.contains("RemoveFromPath(ExpandConstant('{app}'));"));
    assert!(
        iss.contains("RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths);")
    );
    assert!(iss.contains("ChangesEnvironment=yes"));
}

/// The Pascal routine in installer.iss, transcribed: removes every `;`-separated entry equal to `dir`
/// (case-insensitive) and keeps the others in order. Guards the string arithmetic of the script.
fn remove_from_path(paths: &str, dir: &str) -> Option<String> {
    let mut padded = format!(";{};", paths);
    let needle = format!(";{};", dir.to_uppercase());
    let mut p = padded.to_uppercase().find(&needle)?;
    loop {
        padded.replace_range(p..p + dir.len() + 1, "");
        match padded.to_uppercase().find(&needle) {
            Some(n) => p = n,
            None => break,
        }
    }
    if padded.len() <= 2 {
        Some(String::new())
    } else {
        Some(padded[1..padded.len() - 1].to_string())
    }
}

#[test]
fn path_removal_keeps_the_other_entries() {
    let app = r"C:\Users\ana\AppData\Local\Programs\Rakvor\RV PDFAuth";
    assert_eq!(
        remove_from_path(&format!(r"C:\bin;{};D:\tools", app), app).as_deref(),
        Some(r"C:\bin;D:\tools")
    );
    assert_eq!(remove_from_path(app, app).as_deref(), Some(""));
    assert_eq!(
        remove_from_path(&format!("{};C:\\x;{}", app.to_lowercase(), app), app).as_deref(),
        Some(r"C:\x")
    );
    assert_eq!(
        remove_from_path(&format!(r"C:\bin;{};", app), app).as_deref(),
        Some(r"C:\bin;")
    );
    assert_eq!(remove_from_path(r"C:\bin;C:\other", app), None);
    // A longer folder that only starts with the install path is not touched.
    assert_eq!(remove_from_path(&format!(r"{}-old;C:\bin", app), app), None);
}

/// The installer identity of every Rakvor tool. Two installers with the same AppId upgrade or uninstall each
/// other (rv-imgauth 0.2.0 shipped with rv-pdfauth's). A new tool adds its AppId here, in every tool repo.
const THIS_TOOL: &str = "rv-pdfauth";
const RAKVOR_APP_IDS: [(&str, &str); 4] = [
    ("rv-hashinv", "6D3F3B0E-5B7A-4B2C-9E61-2A1C7D4E9F10"),
    ("rv-pdfauth", "B2454915-B966-44CD-9C5B-5413F38185AE"),
    ("rv-rsmf", "7E0DA535-8638-4E9D-8680-ED046DCA9F45"),
    ("rv-imgauth", "04A0C3C3-4929-42ED-81F7-93DB361940C8"),
];

#[test]
fn installer_app_id_is_this_tools_own() {
    let iss = installer();
    let line = iss
        .lines()
        .find(|l| l.starts_with("AppId="))
        .expect("installer.iss has an AppId line");
    // Inno Setup escapes the opening brace: AppId={{GUID}
    let id = line
        .strip_prefix("AppId={{")
        .and_then(|l| l.strip_suffix('}'))
        .unwrap_or_else(|| panic!("AppId must be written {{{{GUID}}: {}", line));
    let (own_tool, own_id) = RAKVOR_APP_IDS
        .iter()
        .find(|(t, _)| *t == THIS_TOOL)
        .copied()
        .unwrap_or_else(|| panic!("{} is not in RAKVOR_APP_IDS", THIS_TOOL));
    assert_eq!(id, own_id, "{} installer AppId", own_tool);
    for (other, other_id) in RAKVOR_APP_IDS {
        if other != own_tool {
            assert_ne!(
                id, other_id,
                "{} shares its installer AppId with {}",
                own_tool, other
            );
        }
    }
    let mut ids: Vec<&str> = RAKVOR_APP_IDS.iter().map(|(_, i)| *i).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), RAKVOR_APP_IDS.len(), "AppIds are unique");
}
