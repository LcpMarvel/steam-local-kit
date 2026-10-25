use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use keyvalues_parser::{parse, Obj, Vdf};

use crate::{AppId, SteamError};

pub struct LocalConfigDocument<'source> {
    pub(crate) source: &'source str,
    pub(crate) vdf: Vdf<'source>,
    pub(crate) path: Option<PathBuf>,
}

impl fmt::Debug for LocalConfigDocument<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalConfigDocument")
            .field("path", &self.path)
            .field("source_len", &self.source.len())
            .finish_non_exhaustive()
    }
}

impl<'source> LocalConfigDocument<'source> {
    pub fn parse(source: &'source str) -> Result<Self, SteamError> {
        Self::parse_internal(source, None)
    }

    pub fn parse_at(path: &Path, source: &'source str) -> Result<Self, SteamError> {
        Self::parse_internal(source, Some(path.to_path_buf()))
    }

    fn parse_internal(source: &'source str, path: Option<PathBuf>) -> Result<Self, SteamError> {
        let parsed = parse(source).map_err(|_| match &path {
            Some(path) => SteamError::invalid_vdf(path.clone(), "parse_failed"),
            None => SteamError::invalid_vdf_source("parse_failed"),
        })?;
        let vdf = Vdf::from(parsed);
        if vdf.key.as_ref() != "UserLocalConfigStore" || vdf.value.get_obj().is_none() {
            return Err(match &path {
                Some(path) => SteamError::invalid_vdf(path.clone(), "unexpected_localconfig_root"),
                None => SteamError::invalid_vdf_source("unexpected_localconfig_root"),
            });
        }
        Ok(Self { source, vdf, path })
    }

    pub fn launch_options(&self, app_id: AppId) -> Result<Option<&str>, SteamError> {
        let root =
            self.vdf.value.get_obj().ok_or_else(|| {
                SteamError::missing_node(self.path.clone(), "UserLocalConfigStore")
            })?;
        let app_id = app_id.get().to_string();
        // A user who never ran the app has no node for it (or no `apps` yet).
        let Some(app) = unique_object(
            root,
            &["Software", "Valve", "Steam", "apps", app_id.as_str()],
            self.path.as_ref(),
        )?
        else {
            return Ok(None);
        };
        let Some(values) = app.get("LaunchOptions") else {
            return Ok(None);
        };
        let [value] = values.as_slice() else {
            return Err(SteamError::ambiguous_node(
                self.path.clone(),
                format!("Software/Valve/Steam/apps/{app_id}/LaunchOptions"),
            ));
        };
        value.get_str().map(Some).ok_or_else(|| {
            SteamError::invalid_vdf_at(
                self.path.clone(),
                format!("Software/Valve/Steam/apps/{app_id}/LaunchOptions is not a string"),
            )
        })
    }
}

pub fn read_launch_options(path: &Path, app_id: AppId) -> Result<Option<String>, SteamError> {
    let source =
        fs::read_to_string(path).map_err(|error| SteamError::read(path.to_path_buf(), error))?;
    LocalConfigDocument::parse_at(path, &source)?
        .launch_options(app_id)
        .map(|value| value.map(str::to_owned))
}

/// The object at `keys`, or `None` when a node on the way is absent.
fn unique_object<'value>(
    mut object: &'value Obj<'value>,
    keys: &[&str],
    source_path: Option<&PathBuf>,
) -> Result<Option<&'value Obj<'value>>, SteamError> {
    let mut traversed = Vec::new();
    for key in keys {
        traversed.push(*key);
        let node = traversed.join("/");
        let Some(values) = object.get(*key) else {
            return Ok(None);
        };
        let [value] = values.as_slice() else {
            return Err(SteamError::ambiguous_node(source_path.cloned(), node));
        };
        object = value.get_obj().ok_or_else(|| {
            SteamError::invalid_vdf_at(source_path.cloned(), format!("{node} is not an object"))
        })?;
    }
    Ok(Some(object))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const SAMPLE: &str = r#""UserLocalConfigStore"
{
    "Software" { "Valve" { "Steam" { "apps"
    {
        "570" { "LaunchOptions" "-language 简体中文 -gamestateintegration" }
        "730" { "LaunchOptions" "-console" }
    } } } }
}"#;

    #[test]
    fn reads_launch_options_for_only_the_requested_app() {
        let document = LocalConfigDocument::parse(SAMPLE).unwrap();

        assert_eq!(
            document.launch_options(AppId::new(570)).unwrap(),
            Some("-language 简体中文 -gamestateintegration")
        );
        assert_eq!(
            document.launch_options(AppId::new(730)).unwrap(),
            Some("-console")
        );
    }

    #[test]
    fn missing_launch_options_or_app_node_reads_as_none() {
        let source = SAMPLE.replace("\"LaunchOptions\" \"-console\"", "\"LastPlayed\" \"1\"");
        let document = LocalConfigDocument::parse(&source).unwrap();

        assert_eq!(document.launch_options(AppId::new(730)).unwrap(), None);
        assert_eq!(document.launch_options(AppId::new(999)).unwrap(), None);
        let no_apps = LocalConfigDocument::parse(
            "\"UserLocalConfigStore\" { \"Software\" { \"Valve\" { \"Steam\" { } } } }",
        )
        .unwrap();
        assert_eq!(no_apps.launch_options(AppId::new(570)).unwrap(), None);
    }

    #[test]
    fn duplicate_launch_options_fail_closed() {
        let source = SAMPLE.replace(
            "\"LaunchOptions\" \"-console\"",
            "\"LaunchOptions\" \"-console\" \"LaunchOptions\" \"-novid\"",
        );
        let document = LocalConfigDocument::parse(&source).unwrap();

        assert!(matches!(
            document.launch_options(AppId::new(730)).unwrap_err(),
            SteamError::AmbiguousNode { node, .. } if node.ends_with("/LaunchOptions")
        ));
    }

    #[test]
    fn invalid_source_error_does_not_contain_source_text() {
        let secret = "secret-token {";

        let error = LocalConfigDocument::parse(secret).unwrap_err();

        assert!(matches!(error, SteamError::InvalidVdf { .. }));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn debug_does_not_expose_source_or_parsed_values() {
        let source = SAMPLE.replace("-console", "do-not-log");
        let document = LocalConfigDocument::parse(&source).unwrap();

        let debug = format!("{document:?}");

        assert!(!debug.contains("do-not-log"));
        assert!(!debug.contains("UserLocalConfigStore"));
        assert!(debug.contains("source_len"));
    }

    #[test]
    fn rejects_a_valid_vdf_with_the_wrong_root() {
        let error = LocalConfigDocument::parse("\"OtherRoot\" { \"Software\" {} }").unwrap_err();

        assert!(matches!(error, SteamError::InvalidVdf { .. }));
    }

    #[test]
    fn file_api_attaches_the_source_path() {
        let root = std::env::temp_dir().join(format!(
            "steam-local-kit-local-config-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("localconfig.vdf");
        fs::write(&path, SAMPLE).unwrap();

        assert_eq!(
            read_launch_options(&path, AppId::new(570)).unwrap(),
            Some("-language 简体中文 -gamestateintegration".to_owned())
        );
        fs::remove_dir_all(root).unwrap();
    }
}
