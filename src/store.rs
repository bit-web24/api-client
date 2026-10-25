//! Keeps the [`Workspace`] in `services.json` under the user's config directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::service::Workspace;

/// File name of the saved workspace inside [`config_dir`].
const FILE_NAME: &str = "services.json";

/// Returns `$XDG_CONFIG_HOME/api-client`, falling back to `~/.config/api-client`.
///
/// Returns `None` when neither variable is set, e.g. in a stripped-down
/// environment; the app then runs without saving.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("api-client"))
}

/// What [`load`] found.
#[derive(Debug)]
pub enum Loaded {
    /// The saved workspace, or a fresh one on first start.
    Workspace(Workspace),
    /// The file existed but could not be used; it was moved to `kept_at` and a
    /// fresh workspace started, so nothing the user had is overwritten.
    Damaged {
        workspace: Workspace,
        kept_at: PathBuf,
        reason: String,
    },
}

/// Loads the workspace saved in `dir`.
///
/// # Errors
///
/// Returns the IO error when the file exists but can't be read, or a damaged
/// file can't be moved aside.
pub fn load(dir: &Path) -> io::Result<Loaded> {
    let path = dir.join(FILE_NAME);
    let json = match fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded::Workspace(Workspace::default()));
        }
        Err(error) => return Err(error),
    };
    match Workspace::from_json(&json) {
        Ok(workspace) => Ok(Loaded::Workspace(workspace)),
        Err(error) => {
            let kept_at = path.with_extension("json.damaged");
            fs::rename(&path, &kept_at)?;
            Ok(Loaded::Damaged {
                workspace: Workspace::default(),
                kept_at,
                reason: error.to_string(),
            })
        }
    }
}

/// Saves `workspace` to `dir`, creating the directory if needed.
///
/// Writes a temporary file and renames it over the old one, so a crash mid-save
/// leaves the previous version intact. The file may hold typed bearer tokens,
/// so on Unix it is readable by the owner only.
///
/// # Errors
///
/// Returns the IO error if the directory or file can't be written.
pub fn save(dir: &Path, workspace: &Workspace) -> io::Result<()> {
    let json = workspace.to_json().map_err(io::Error::other)?;
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, json)?;
    restrict_to_owner(&temp)?;
    fs::rename(&temp, &path)
}

/// Makes `path` readable and writable by its owner only.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Leaves permissions to the platform default where Unix modes don't exist.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("api-client-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // reason: leftovers from an aborted earlier run
        dir
    }

    #[test]
    fn first_start_gives_a_fresh_workspace() {
        let dir = temp_dir("first-start");
        let loaded = load(&dir).unwrap();
        assert!(
            matches!(loaded, Loaded::Workspace(workspace) if workspace == Workspace::default())
        );
    }

    #[test]
    fn saved_workspace_loads_back() {
        let dir = temp_dir("round-trip");
        let mut workspace = Workspace::default();
        workspace.create("Billing").unwrap();
        save(&dir, &workspace).unwrap();
        let loaded = load(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(loaded, Loaded::Workspace(found) if found == workspace));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("permissions");
        save(&dir, &Workspace::default()).unwrap();
        let mode = fs::metadata(dir.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn damaged_file_is_kept_aside_not_overwritten() {
        let dir = temp_dir("damaged");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        let loaded = load(&dir).unwrap();
        let kept = fs::read_to_string(dir.join("services.json.damaged")).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(
            matches!(loaded, Loaded::Damaged { workspace, .. } if workspace == Workspace::default())
        );
        assert_eq!(kept, "{ not json");
    }
}
