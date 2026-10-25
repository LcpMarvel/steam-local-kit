use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use keyvalues_parser::{parse, Obj, Vdf};

use crate::{AppId, SteamError};

/// An app a library says it holds, from its `appmanifest_<id>.acf`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppInstall {
    pub app_id: AppId,
    pub library: PathBuf,
    pub manifest_path: PathBuf,
    /// `<library>/steamapps/common/<installdir>`, as Steam names it. It is
    /// not resolved: the directory may be a link to another disk, and may
    /// not exist at all when the files were removed by hand.
    pub install_path: PathBuf,
}

/// Reads the app's manifest in one library. A library without the manifest
/// does not hold the app and reads as `None`; a manifest that is there but
/// unreadable, malformed, names another app, or names an `installdir` that
/// is not one plain directory name is an error.
pub fn find_app_install(library: &Path, app_id: AppId) -> Result<Option<AppInstall>, SteamError> {
    let manifest_path = library
        .join("steamapps")
        .join(format!("appmanifest_{}.acf", app_id.get()));
    let source = match fs::read_to_string(&manifest_path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SteamError::read(manifest_path, error)),
    };
    let install_dir = parse_install_dir(&manifest_path, &source, app_id)?;
    Ok(Some(AppInstall {
        app_id,
        library: library.to_path_buf(),
        install_path: library.join("steamapps").join("common").join(install_dir),
        manifest_path,
    }))
}

fn parse_install_dir(path: &Path, source: &str, app_id: AppId) -> Result<String, SteamError> {
    let parsed = parse(source).map_err(|_| SteamError::invalid_vdf(path.into(), "parse_failed"))?;
    let vdf = Vdf::from(parsed);
    let root = match (vdf.key.as_ref(), vdf.value.get_obj()) {
        ("AppState", Some(root)) => root,
        _ => {
            return Err(SteamError::invalid_vdf(
                path.into(),
                "unexpected_appmanifest_root",
            ))
        }
    };
    let found = one_string(root, "appid", path)?;
    if found != app_id.get().to_string() {
        return Err(SteamError::AppMismatch {
            path: path.into(),
            expected: app_id,
        });
    }
    let install_dir = one_string(root, "installdir", path)?;
    if !is_single_normal_component(install_dir) {
        return Err(SteamError::UnsafeInstallDir { path: path.into() });
    }
    Ok(install_dir.to_owned())
}

fn one_string<'a>(object: &'a Obj<'a>, key: &str, path: &Path) -> Result<&'a str, SteamError> {
    match object.get(key).map(Vec::as_slice) {
        None | Some([]) => Err(SteamError::missing_node(Some(path.into()), key)),
        Some([value]) => value
            .get_str()
            .ok_or_else(|| SteamError::invalid_vdf(path.into(), "expected_string")),
        Some(_) => Err(SteamError::ambiguous_node(Some(path.into()), key)),
    }
}

fn is_single_normal_component(value: &str) -> bool {
    if value.contains(['/', '\\']) {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::find_app_install;
    use crate::{AppId, SteamError};

    fn library(name: &str, manifest: Option<&str>) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "steam-local-kit-manifest-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("steamapps")).unwrap();
        if let Some(manifest) = manifest {
            fs::write(root.join("steamapps/appmanifest_123.acf"), manifest).unwrap();
        }
        root
    }

    #[test]
    fn finds_the_install_the_manifest_names() {
        let root = library(
            "found",
            Some("\"AppState\"\n{\n\t\"appid\"\t\t\"123\"\n\t\"installdir\"\t\t\"some game\"\n}\n"),
        );

        let install = find_app_install(&root, AppId::new(123)).unwrap().unwrap();

        assert_eq!(
            install.install_path,
            root.join("steamapps/common/some game")
        );
        assert_eq!(
            install.manifest_path,
            root.join("steamapps/appmanifest_123.acf")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_library_without_the_manifest_does_not_hold_the_app() {
        let root = library("absent", None);

        assert_eq!(find_app_install(&root, AppId::new(123)).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_manifest_for_another_app_is_a_mismatch() {
        let root = library(
            "mismatch",
            Some(r#""AppState" { "appid" "456" "installdir" "other" }"#),
        );

        let error = find_app_install(&root, AppId::new(123)).unwrap_err();

        assert!(matches!(error, SteamError::AppMismatch { expected, .. } if expected.get() == 123));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_installdir_that_leaves_common_is_refused() {
        for install_dir in ["..", ".", "", r"..\\escape", "a/b"] {
            let root = library(
                "unsafe",
                Some(&format!(
                    r#""AppState" {{ "appid" "123" "installdir" "{install_dir}" }}"#
                )),
            );

            let error = find_app_install(&root, AppId::new(123)).unwrap_err();

            assert!(
                matches!(error, SteamError::UnsafeInstallDir { .. }),
                "{install_dir}: {error}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_broken_manifest_is_a_typed_vdf_error() {
        let root = library("broken", Some(r#""AppState" { "appid" "#));
        assert!(matches!(
            find_app_install(&root, AppId::new(123)).unwrap_err(),
            SteamError::InvalidVdf { .. }
        ));
        fs::remove_dir_all(root).unwrap();

        let root = library("no-installdir", Some(r#""AppState" { "appid" "123" }"#));
        assert!(matches!(
            find_app_install(&root, AppId::new(123)).unwrap_err(),
            SteamError::MissingNode { node, .. } if node == "installdir"
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
