//! Which account is signed in to the running Steam client right now. Steam
//! keeps it as `ActiveProcess\ActiveUser`: in the Windows registry, and in
//! `registry.vdf` elsewhere. It is `0` while nobody is signed in. Unlike
//! `MostRecent` in `loginusers.vdf`, it does not outlive a sign-out, but it
//! can outlive a crashed Steam, so read it only while Steam runs.

#[cfg(not(windows))]
use std::{fs, io, path::Path};

#[cfg(not(windows))]
use keyvalues_parser::{parse, Vdf};

use crate::SteamError;

/// The account ID (the `userdata` directory name) signed in to Steam, or
/// `None` when nobody is or Steam never recorded one.
#[cfg(windows)]
pub fn active_account_id() -> Result<Option<u32>, SteamError> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let key = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Valve\Steam\ActiveProcess")
    {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SteamError::read(
                r"HKCU\Software\Valve\Steam\ActiveProcess".into(),
                error,
            ))
        }
    };
    match key.get_value::<u32, _>("ActiveUser") {
        Ok(0) => Ok(None),
        Ok(account_id) => Ok(Some(account_id)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SteamError::read(
            r"HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser".into(),
            error,
        )),
    }
}

#[cfg(not(windows))]
pub fn active_account_id() -> Result<Option<u32>, SteamError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    let path = home.join("Library/Application Support/Steam/registry.vdf");
    #[cfg(not(target_os = "macos"))]
    let path = home.join(".steam/registry.vdf");
    active_account_id_in(&path)
}

/// Reads `ActiveUser` from a `registry.vdf`; a missing file reads as nobody.
#[cfg(not(windows))]
pub fn active_account_id_in(path: &Path) -> Result<Option<u32>, SteamError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SteamError::read(path.into(), error)),
    };
    let vdf = Vdf::from(
        parse(&source).map_err(|_| SteamError::invalid_vdf(path.into(), "parse_failed"))?,
    );
    let mut object = vdf
        .value
        .get_obj()
        .ok_or_else(|| SteamError::invalid_vdf(path.into(), "unexpected_registry_root"))?;
    for key in ["HKCU", "Software", "Valve", "Steam", "ActiveProcess"] {
        // Steam writes these keys in varying case.
        let Some(next) = object
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, values)| values.first())
            .and_then(|value| value.get_obj())
        else {
            return Ok(None);
        };
        object = next;
    }
    let Some(value) = object
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("ActiveUser"))
        .and_then(|(_, values)| values.first())
        .and_then(|value| value.get_str())
    else {
        return Ok(None);
    };
    match value.parse::<u32>() {
        Ok(0) => Ok(None),
        Ok(account_id) => Ok(Some(account_id)),
        Err(_) => Err(SteamError::invalid_vdf(
            path.into(),
            "active_user_not_a_number",
        )),
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::{fs, path::PathBuf};

    use super::active_account_id_in;

    fn registry(name: &str, source: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "steam-local-kit-registry-{name}-{}.vdf",
            std::process::id()
        ));
        fs::write(&path, source).unwrap();
        path
    }

    #[test]
    fn reads_the_signed_in_account() {
        let path = registry(
            "signed-in",
            r#""Registry" { "HKCU" { "Software" { "Valve" { "Steam" {
                "ActiveProcess" { "pid" "4242" "ActiveUser" "107331" } } } } } }"#,
        );
        assert_eq!(active_account_id_in(&path).unwrap(), Some(107_331));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn nobody_signed_in_reads_as_none() {
        let path = registry(
            "signed-out",
            r#""Registry" { "HKCU" { "Software" { "valve" { "Steam" {
                "ActiveProcess" { "ActiveUser" "0" } } } } } }"#,
        );
        assert_eq!(active_account_id_in(&path).unwrap(), None);
        fs::remove_file(path).unwrap();
        assert_eq!(
            active_account_id_in(&std::env::temp_dir().join("steam-local-kit-no-registry.vdf"))
                .unwrap(),
            None
        );
    }
}
