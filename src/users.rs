use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use keyvalues_parser::{parse, Vdf};

use crate::discovery::path_identity;

const STEAM_ID64_ACCOUNT_BASE: u64 = 76_561_197_960_265_728;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    UserdataMissing,
    UserdataUnreadable,
    LoginUsersMissing,
    LoginUsersUnreadable,
    LoginUsersInvalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserEvidence {
    LocalConfig,
    LoginUsersEntry,
    LoginUsersMostRecent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamUserCandidate {
    pub account_id: u32,
    pub steam_id64: Option<u64>,
    pub local_config_path: PathBuf,
    pub most_recent: bool,
    pub evidence: Vec<UserEvidence>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UserDiscovery {
    pub candidates: Vec<SteamUserCandidate>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution<T> {
    Missing {
        diagnostics: Vec<Diagnostic>,
    },
    Unique(T),
    Ambiguous {
        candidates: Vec<T>,
        diagnostics: Vec<Diagnostic>,
    },
}

pub struct UserResolver;

impl UserResolver {
    /// Discovers local Steam users across explicit client roots.
    ///
    /// Missing or malformed `loginusers.vdf` is diagnostic-only: valid
    /// `localconfig.vdf` candidates remain available but lose recent-user
    /// evidence. An unreadable `userdata` directory is also reported without
    /// exposing file contents.
    pub fn discover<I, P>(steam_roots: I) -> UserDiscovery
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut discovery = UserDiscovery::default();
        let mut seen_roots = BTreeSet::new();
        let mut seen_configs = BTreeSet::new();

        for root in steam_roots {
            let root = root.as_ref();
            if !seen_roots.insert(path_identity(root)) {
                continue;
            }

            let login_users = login_users(root, &mut discovery.diagnostics);
            let userdata = root.join("userdata");
            let entries = match fs::read_dir(&userdata) {
                Ok(entries) => entries,
                Err(error) => {
                    discovery.diagnostics.push(Diagnostic {
                        code: if error.kind() == io::ErrorKind::NotFound {
                            DiagnosticCode::UserdataMissing
                        } else {
                            DiagnosticCode::UserdataUnreadable
                        },
                        path: Some(userdata),
                    });
                    continue;
                }
            };

            let mut root_candidates = entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let account_id = entry.file_name().to_str()?.parse::<u32>().ok()?;
                    let local_config_path = entry.path().join("config/localconfig.vdf");
                    if !local_config_path.is_file()
                        || !seen_configs.insert(path_identity(&local_config_path))
                    {
                        return None;
                    }
                    let login = login_users.get(&account_id);
                    let most_recent = login.is_some_and(|record| record.most_recent);
                    let mut evidence = vec![UserEvidence::LocalConfig];
                    if login.is_some() {
                        evidence.push(UserEvidence::LoginUsersEntry);
                    }
                    if most_recent {
                        evidence.push(UserEvidence::LoginUsersMostRecent);
                    }
                    Some(SteamUserCandidate {
                        account_id,
                        steam_id64: login.map(|record| record.steam_id64),
                        local_config_path,
                        most_recent,
                        evidence,
                    })
                })
                .collect::<Vec<_>>();
            root_candidates.sort_by(|left, right| {
                left.account_id
                    .cmp(&right.account_id)
                    .then_with(|| left.local_config_path.cmp(&right.local_config_path))
            });
            discovery.candidates.extend(root_candidates);
        }

        discovery
    }

    /// Resolves only a unique candidate or a uniquely evidenced recent user.
    pub fn resolve(discovery: &UserDiscovery) -> Resolution<SteamUserCandidate> {
        match discovery.candidates.as_slice() {
            [] => Resolution::Missing {
                diagnostics: discovery.diagnostics.clone(),
            },
            [candidate] => Resolution::Unique(candidate.clone()),
            candidates => {
                let recent = candidates
                    .iter()
                    .filter(|candidate| candidate.most_recent)
                    .collect::<Vec<_>>();
                if let [candidate] = recent.as_slice() {
                    Resolution::Unique((*candidate).clone())
                } else {
                    Resolution::Ambiguous {
                        candidates: candidates.to_vec(),
                        diagnostics: discovery.diagnostics.clone(),
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct LoginUserRecord {
    steam_id64: u64,
    most_recent: bool,
}

fn login_users(
    steam_root: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<u32, LoginUserRecord> {
    let path = steam_root.join("config/loginusers.vdf");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            diagnostics.push(Diagnostic {
                code: if error.kind() == io::ErrorKind::NotFound {
                    DiagnosticCode::LoginUsersMissing
                } else {
                    DiagnosticCode::LoginUsersUnreadable
                },
                path: Some(path),
            });
            return BTreeMap::new();
        }
    };
    let parsed = match parse(&contents) {
        Ok(parsed) => Vdf::from(parsed),
        Err(_) => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::LoginUsersInvalid,
                path: Some(path),
            });
            return BTreeMap::new();
        }
    };
    let Some(users) = parsed.value.get_obj() else {
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::LoginUsersInvalid,
            path: Some(path),
        });
        return BTreeMap::new();
    };

    users
        .iter()
        .filter_map(|(steam_id, values)| {
            let steam_id64 = steam_id.parse::<u64>().ok()?;
            let account_id = steam_id64.checked_sub(STEAM_ID64_ACCOUNT_BASE)?;
            let account_id = u32::try_from(account_id).ok()?;
            let user = values.first()?.get_obj()?;
            let most_recent = user
                .get("MostRecent")
                .and_then(|values| values.first())
                .and_then(|value| value.get_str())
                == Some("1");
            Some((
                account_id,
                LoginUserRecord {
                    steam_id64,
                    most_recent,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "steam-local-kit-users-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn add_user(root: &Path, account_id: u32) -> PathBuf {
        let path = root
            .join("userdata")
            .join(account_id.to_string())
            .join("config/localconfig.vdf");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "\"UserLocalConfigStore\" {}").unwrap();
        path
    }

    fn write_login_users(root: &Path, body: &str) {
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(
            root.join("config/loginusers.vdf"),
            format!("\"users\" {{ {body} }}"),
        )
        .unwrap();
    }

    #[test]
    fn one_local_config_resolves_without_loginusers() {
        let root = temp_root("unique");
        let path = add_user(&root, 42);

        let discovery = UserResolver::discover([&root]);

        assert_eq!(discovery.candidates.len(), 1);
        assert!(discovery
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::LoginUsersMissing));
        assert_eq!(
            UserResolver::resolve(&discovery),
            Resolution::Unique(SteamUserCandidate {
                account_id: 42,
                steam_id64: None,
                local_config_path: path,
                most_recent: false,
                evidence: vec![UserEvidence::LocalConfig],
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exactly_one_recent_user_resolves_multiple_candidates() {
        let root = temp_root("recent");
        add_user(&root, 41);
        let selected_path = add_user(&root, 42);
        write_login_users(
            &root,
            "\"76561197960265769\" { \"MostRecent\" \"0\" } \
             \"76561197960265770\" { \"MostRecent\" \"1\" }",
        );

        let discovery = UserResolver::discover([&root]);
        let resolution = UserResolver::resolve(&discovery);

        let Resolution::Unique(selected) = resolution else {
            panic!("expected a unique recent user");
        };
        assert_eq!(selected.account_id, 42);
        assert_eq!(selected.steam_id64, Some(76_561_197_960_265_770));
        assert_eq!(selected.local_config_path, selected_path);
        assert!(selected.most_recent);
        assert!(selected
            .evidence
            .contains(&UserEvidence::LoginUsersMostRecent));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multiple_recent_users_remain_ambiguous() {
        let root = temp_root("ambiguous");
        add_user(&root, 41);
        add_user(&root, 42);
        write_login_users(
            &root,
            "\"76561197960265769\" { \"MostRecent\" \"1\" } \
             \"76561197960265770\" { \"MostRecent\" \"1\" }",
        );

        let discovery = UserResolver::discover([&root]);

        assert!(matches!(
            UserResolver::resolve(&discovery),
            Resolution::Ambiguous { candidates, .. } if candidates.len() == 2
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_loginusers_is_diagnostic_and_does_not_hide_candidates() {
        let root = temp_root("malformed");
        add_user(&root, 42);
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/loginusers.vdf"), "secret-token {").unwrap();

        let discovery = UserResolver::discover([&root]);

        assert_eq!(discovery.candidates.len(), 1);
        assert!(discovery
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::LoginUsersInvalid));
        assert!(!format!("{:?}", discovery.diagnostics).contains("secret-token"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_roots_and_non_account_directories_do_not_duplicate_candidates() {
        let root = temp_root("deduplicate");
        add_user(&root, 42);
        fs::create_dir_all(root.join("userdata/not-an-account/config")).unwrap();
        fs::write(
            root.join("userdata/not-an-account/config/localconfig.vdf"),
            "ignored",
        )
        .unwrap();

        let discovery = UserResolver::discover([&root, &root]);

        assert_eq!(discovery.candidates.len(), 1);
        assert_eq!(discovery.candidates[0].account_id, 42);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_aliases_do_not_create_ambiguous_duplicate_users() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-deduplicate");
        let actual = root.join("actual");
        let alias = root.join("alias");
        add_user(&actual, 42);
        symlink(&actual, &alias).unwrap();

        let discovery = UserResolver::discover([&actual, &alias]);

        assert_eq!(discovery.candidates.len(), 1);
        assert!(matches!(
            UserResolver::resolve(&discovery),
            Resolution::Unique(candidate) if candidate.account_id == 42
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
