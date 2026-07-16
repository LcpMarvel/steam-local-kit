use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use keyvalues_parser::{parse, Vdf};

use crate::SteamError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibrarySource {
    Registry { index: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryRoot {
    pub path: PathBuf,
    pub source: LibrarySource,
}

/// Returns platform-default Steam client roots in deterministic priority order.
///
/// This is a convenience adapter over process environment and OS registration.
/// Parsing functions accept explicit roots and remain deterministic in tests.
pub fn platform_default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("ProgramFiles(x86)") {
            roots.push(PathBuf::from(path).join("Steam"));
        }
        if let Some(path) = std::env::var_os("ProgramFiles") {
            roots.push(PathBuf::from(path).join("Steam"));
        }
        roots.extend(windows_registry_roots());
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Library/Application Support/Steam"));
    }

    #[cfg(target_os = "linux")]
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".local/share/Steam"));
    }

    deduplicate_paths(roots)
}

/// Reads additional library roots from `<steam_root>/steamapps/libraryfolders.vdf`.
///
/// Registry entry `0` is the client root itself and is intentionally omitted;
/// callers normally add `steam_root` before these additional libraries.
pub fn additional_library_roots(steam_root: &Path) -> Result<Vec<LibraryRoot>, SteamError> {
    let registry_path = steam_root.join("steamapps/libraryfolders.vdf");
    let contents = fs::read_to_string(&registry_path)
        .map_err(|source| SteamError::read(registry_path.clone(), source))?;
    let parsed = Vdf::from(
        parse(&contents)
            .map_err(|_| SteamError::invalid_vdf(registry_path.clone(), "parse_failed"))?,
    );
    let root = parsed.value.get_obj().ok_or_else(|| {
        SteamError::invalid_vdf(
            registry_path.clone(),
            "libraryfolders root is not an object",
        )
    })?;

    let mut indexed = root
        .iter()
        .filter_map(|(key, values)| {
            let index = key.parse::<u32>().ok()?;
            if index == 0 {
                return None;
            }
            let object = values.first()?.get_obj()?;
            let path = object.get("path")?.first()?.get_str()?.trim();
            if path.is_empty() {
                return None;
            }
            Some((index, PathBuf::from(path)))
        })
        .collect::<Vec<_>>();
    indexed.sort_by_key(|(index, _)| *index);

    let mut seen = BTreeSet::new();
    Ok(indexed
        .into_iter()
        .filter(|(_, path)| seen.insert(path_identity(path)))
        .map(|(index, path)| LibraryRoot {
            path,
            source: LibrarySource::Registry { index },
        })
        .collect())
}

fn deduplicate_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path_identity(path)))
        .collect()
}

pub(crate) fn path_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(windows)]
fn windows_registry_roots() -> Vec<PathBuf> {
    use winreg::{enums::*, RegKey};

    let locations = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];

    locations
        .into_iter()
        .filter_map(|(hive, key, value)| {
            RegKey::predef(hive)
                .open_subkey(key)
                .ok()?
                .get_value::<String, _>(value)
                .ok()
                .map(PathBuf::from)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::IoOperation;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "steam-local-kit-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn parses_additional_libraries_in_numeric_registry_order() {
        let root = temp_root("ordered");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            r#""libraryfolders"
{
    "contentstatsid" "123456"
    "0" { "path" "/default/Steam" }
    "2" { "path" "/Volumes/Second/SteamLibrary" }
    "1"
    {
        "path" "/Volumes/First/SteamLibrary"
        "apps" { "570" "1" }
    }
}"#,
        )
        .unwrap();

        let libraries = additional_library_roots(&root).unwrap();

        assert_eq!(
            libraries,
            vec![
                LibraryRoot {
                    path: PathBuf::from("/Volumes/First/SteamLibrary"),
                    source: LibrarySource::Registry { index: 1 },
                },
                LibraryRoot {
                    path: PathBuf::from("/Volumes/Second/SteamLibrary"),
                    source: LibrarySource::Registry { index: 2 },
                },
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn decodes_and_deduplicates_windows_library_paths() {
        let root = temp_root("windows");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            r#""libraryfolders"
{
    "1" { "path" "D:\\SteamLibrary" }
    "2" { "path" "D:\\SteamLibrary" }
}"#,
        )
        .unwrap();

        assert_eq!(
            additional_library_roots(&root).unwrap(),
            vec![LibraryRoot {
                path: PathBuf::from(r"D:\SteamLibrary"),
                source: LibrarySource::Registry { index: 1 },
            }]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn deduplicates_existing_library_paths_reached_through_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlinked-library");
        let actual = root.join("actual-library");
        let alias = root.join("alias-library");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::create_dir_all(&actual).unwrap();
        symlink(&actual, &alias).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"1\" {{ \"path\" \"{}\" }} \"2\" {{ \"path\" \"{}\" }} }}",
                actual.display(),
                alias.display()
            ),
        )
        .unwrap();

        let libraries = additional_library_roots(&root).unwrap();

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].path, actual);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_registry_is_a_typed_read_error_without_file_contents() {
        let root = temp_root("missing");

        let error = additional_library_roots(&root).unwrap_err();

        assert!(matches!(
            error,
            SteamError::Io {
                operation: IoOperation::Read,
                ..
            }
        ));
        assert!(error.to_string().contains("libraryfolders.vdf"));
        assert!(!error.to_string().contains("localconfig"));
    }

    #[test]
    fn malformed_registry_is_a_typed_vdf_error() {
        let root = temp_root("malformed");
        fs::create_dir_all(root.join("steamapps")).unwrap();
        fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            "\"libraryfolders\" { \"1\" { \"path\" ",
        )
        .unwrap();

        let error = additional_library_roots(&root).unwrap_err();

        assert!(matches!(error, SteamError::InvalidVdf { .. }));
        fs::remove_dir_all(root).unwrap();
    }
}
