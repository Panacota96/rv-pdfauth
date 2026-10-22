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
